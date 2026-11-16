//! Turning operations: facing, OD/ID roughing and finishing, grooving,
//! parting and on-axis drilling.
//!
//! Every path is generated for the tool's **nose centre** against the
//! configuration-space envelope of the full insert (core + nose radius), then
//! converted to the programmed point. That conversion *is* the tool-nose
//! radius compensation; nothing downstream needs G41/G42.

use super::envelope::Envelope;
use super::geom::{self, P2};
use super::profile::{SegRole, TurnProfile};
use super::program::{CutParams, SpindleSpeed, TurnMove, TurnOp, TurnProgram, TurnStock};
use super::tool::{LatheTool, LatheToolKind};
use super::TurnError;

/// Chord tolerance for generated paths (mm). Arcs are chorded on the
/// outside, so this is material left, never a gouge.
const GEN_TOL: f64 = 0.0005;

#[derive(Debug, Clone, Copy, PartialEq)]
enum K {
    Rapid,
    Feed(f64),
}

/// Plans a turning program against one profile and one stock.
#[derive(Debug, Clone)]
pub struct TurnPlanner {
    profile: TurnProfile,
    stock: TurnStock,
    cutoff_allowance: f64,
    tools: Vec<LatheTool>,
    ops: Vec<TurnOp>,
    hole_radius: f64,
    hole_depth: f64,
    od_allowance: Option<f64>,
}

impl TurnPlanner {
    /// Start planning. The profile's finished face must be at `z = 0` (see
    /// [`TurnProfile::face_at_zero`]) and the part must fit in the stock.
    pub fn new(profile: TurnProfile, stock: TurnStock) -> Result<Self, TurnError> {
        if profile.z_max().abs() > 1e-6 {
            return Err(TurnError::BadProfile(format!(
                "finished face must be at z = 0 (is {:.4}); use face_at_zero()",
                profile.z_max()
            )));
        }
        if profile.max_radius() > stock.radius() + 1e-9 {
            return Err(TurnError::BadStock(format!(
                "part Ø{:.3} larger than stock Ø{:.3}",
                2.0 * profile.max_radius(),
                stock.diameter
            )));
        }
        if stock.face_allowance.is_nan() || stock.face_allowance < 0.0 {
            return Err(TurnError::BadStock("negative face allowance".into()));
        }
        if profile.z_min() < stock.z_chuck() + 1.0 {
            return Err(TurnError::BadStock(format!(
                "part (to z = {:.3}) runs into the jaws at z = {:.3}",
                profile.z_min(),
                stock.z_chuck()
            )));
        }
        let stock_hole = stock.bore_diameter / 2.0;
        if stock_hole > 0.0 {
            for (s, role) in profile.classified() {
                if role == SegRole::Axis {
                    continue;
                }
                for p in s.polyline(1e-3) {
                    if p[1] < stock_hole - 1e-9 {
                        return Err(TurnError::BadStock(format!(
                            "tube bore Ø{:.3} is larger than the part at z = {:.3}",
                            stock.bore_diameter, p[0]
                        )));
                    }
                }
            }
        }
        Ok(Self {
            profile,
            stock,
            cutoff_allowance: 2.0,
            tools: Vec::new(),
            ops: Vec::new(),
            hole_radius: stock_hole,
            hole_depth: if stock_hole > 0.0 { f64::INFINITY } else { 0.0 },
            od_allowance: None,
        })
    }

    /// How far past the part's chuck end OD/ID passes run, so the parting
    /// blade enters turned diameter (default 2 mm).
    pub fn with_cutoff_allowance(mut self, mm: f64) -> Self {
        self.cutoff_allowance = mm.max(0.0);
        self
    }

    /// The profile being machined.
    pub fn profile(&self) -> &TurnProfile {
        &self.profile
    }

    /// The stock.
    pub fn stock(&self) -> &TurnStock {
        &self.stock
    }

    fn register(&mut self, tool: &LatheTool) -> Result<(), TurnError> {
        tool.validate()?;
        match self.tools.iter().find(|t| t.number == tool.number) {
            Some(t) if t != tool => Err(TurnError::BadTool(format!(
                "tool number T{} reused for a different tool",
                tool.number
            ))),
            Some(_) => Ok(()),
            None => {
                self.tools.push(tool.clone());
                Ok(())
            }
        }
    }

    fn want(tool: &LatheTool, kinds: &[LatheToolKind], op: &str) -> Result<(), TurnError> {
        if kinds.contains(&tool.kind) {
            Ok(())
        } else {
            Err(TurnError::BadTool(format!(
                "{op} needs a {kinds:?} tool, T{} is {:?}",
                tool.number, tool.kind
            )))
        }
    }

    fn z_start(&self, p: &CutParams) -> f64 {
        self.stock.z_face() + p.clearance
    }

    fn safe_x(&self, p: &CutParams) -> f64 {
        self.stock.radius() + p.clearance
    }

    fn push_op(
        &mut self,
        name: &str,
        tool: &LatheTool,
        p: &CutParams,
        nose: Vec<(P2, K)>,
    ) -> Result<(), TurnError> {
        let moves = nose
            .into_iter()
            .map(|(c, k)| {
                let to = tool.programmed(c);
                match k {
                    K::Rapid => TurnMove::Rapid { to },
                    K::Feed(s) => TurnMove::Feed { to, feed_scale: s },
                }
            })
            .collect();
        self.ops.push(TurnOp {
            name: name.to_string(),
            tool: tool.number,
            speed: p.speed,
            feed_per_rev: p.feed_per_rev,
            moves,
        });
        Ok(())
    }

    /// Edges of the OD chain (outer + right face) plus the cutoff extension.
    fn od_edges(&self) -> Vec<(P2, P2)> {
        let mut e = Vec::new();
        for (s, role) in self.profile.classified() {
            if matches!(role, SegRole::Outer | SegRole::RightFace) {
                for w in s.polyline(GEN_TOL).windows(2) {
                    e.push((w[0], w[1]));
                }
            }
        }
        let zmin = self.profile.z_min();
        if let Some((_, ro)) = self.profile.radii_at(zmin + 1e-6) {
            e.push(([zmin, ro], [zmin - self.cutoff_allowance - 50.0, ro]));
        }
        e
    }

    /// Edges of the bore chain (inner + right face), plus the extension
    /// for a through bore. Returns (edges, deepest z the bore reaches).
    fn id_edges(&self) -> (Vec<(P2, P2)>, f64, bool) {
        let mut e = Vec::new();
        let mut zdeep = 0.0f64;
        for (s, role) in self.profile.classified() {
            if matches!(role, SegRole::Inner | SegRole::RightFace) {
                for w in s.polyline(GEN_TOL).windows(2) {
                    e.push((w[0], w[1]));
                    if role == SegRole::Inner {
                        zdeep = zdeep.min(w[0][0]).min(w[1][0]);
                    }
                }
            }
        }
        let zmin = self.profile.z_min();
        let through = self
            .profile
            .radii_at(zmin + 1e-6)
            .is_some_and(|(ri, _)| ri > 1e-6);
        if through {
            let (ri, _) = self.profile.radii_at(zmin + 1e-6).unwrap_or((0.0, 0.0));
            e.push(([zmin, ri], [zmin - self.cutoff_allowance - 50.0, ri]));
        }
        (e, zdeep, through)
    }

    /// Face the free end down to `z = 0`, in passes of `depth_of_cut`.
    pub fn face(&mut self, tool: &LatheTool, p: &CutParams) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Turning], "facing")?;
        if tool.tip_sign[0] > 0.0 {
            return Err(TurnError::BadTool(
                "facing toward the centre needs a right-hand tool (cuts −Z side)".into(),
            ));
        }
        self.register(tool)?;
        let rn = tool.nose_radius;
        let safe = self.safe_x(p);
        let zs = self.z_start(p);
        let x_end = if self.hole_radius > 0.0 {
            (self.hole_radius - 0.5 + rn).max(0.0)
        } else {
            0.0
        };
        let n = (self.stock.face_allowance / p.depth_of_cut).ceil().max(1.0) as usize;
        let mut nose = vec![([zs, safe], K::Rapid)];
        for k in (0..n).rev() {
            let zf = self.stock.face_allowance * k as f64 / n as f64;
            let cz = zf + rn;
            nose.push(([cz, safe], K::Rapid));
            nose.push(([cz, x_end], K::Feed(1.0)));
            nose.push(([cz + 0.3, x_end], K::Feed(1.0)));
            nose.push(([cz + 0.3, safe], K::Rapid));
        }
        nose.push(([zs, safe], K::Rapid));
        self.push_op("face", tool, p, nose)?;
        Ok(self)
    }

    /// Level roughing + one semi-finish contour in the "up" frame (material
    /// below the envelope, stock surface at `top`). Returns nose centres.
    #[allow(clippy::too_many_arguments)]
    fn level_rough(
        env: &Envelope,
        top: f64,
        bottom_domain: (f64, f64),
        z_start: f64,
        z_end: f64,
        doc: f64,
        safe: f64,
        floor: f64,
        contour: bool,
    ) -> Result<Vec<(P2, K)>, TurnError> {
        let x_bottom = env.min_over(bottom_domain.0, bottom_domain.1, f64::NEG_INFINITY);
        if !x_bottom.is_finite() {
            return Err(TurnError::NotTurnable(
                "no reachable profile in the roughing domain".into(),
            ));
        }
        let mut out: Vec<(P2, K)> = vec![([z_start, safe], K::Rapid)];
        if top > x_bottom + 1e-9 {
            let n = ((top - x_bottom) / doc).ceil().max(1.0) as usize;
            let step = (top - x_bottom) / n as f64;
            for k in 1..=n {
                let x = top - step * k as f64;
                let prev = top - step * (k - 1) as f64;
                let mut iv = env.below(x + 1e-9, z_end, z_start);
                iv.reverse();
                for (lo, hi) in iv {
                    if hi - lo < 0.01 {
                        continue;
                    }
                    out.push(([hi, safe], K::Rapid));
                    if hi >= z_start - 1e-9 {
                        out.push(([hi, x], K::Rapid));
                    } else {
                        out.push(([hi, (prev + 0.1).min(safe)], K::Rapid));
                        out.push(([hi, x], K::Feed(1.0)));
                    }
                    out.push(([lo, x], K::Feed(1.0)));
                    out.push(([lo, prev], K::Feed(1.0)));
                    out.push(([lo, safe], K::Rapid));
                }
            }
        }
        if contour {
            let path = env.path(z_start, z_end, floor, GEN_TOL);
            if let Some(first) = path.first() {
                out.push(([z_start, safe], K::Rapid));
                out.push(([first[0], first[1].max(floor)], K::Rapid));
                for q in &path[1..] {
                    out.push((*q, K::Feed(1.0)));
                }
                let last = *path.last().unwrap_or(first);
                out.push(([last[0], safe], K::Feed(1.0)));
            }
        }
        out.push(([z_start, safe], K::Rapid));
        Ok(out)
    }

    fn od_floor(&self) -> f64 {
        let ro = self.profile.radii_at(-1e-6).map_or(0.0, |r| r.1);
        (ro - 1.0).max(0.0)
    }

    /// OD roughing: horizontal passes toward the chuck, then one contour
    /// pass, leaving `allowance` (radial, uniform) for the finish.
    pub fn rough_od(
        &mut self,
        tool: &LatheTool,
        p: &CutParams,
        allowance: f64,
    ) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Turning], "OD roughing")?;
        self.register(tool)?;
        let zs = self.z_start(p);
        let ze = self.profile.z_min() - self.cutoff_allowance;
        let env = Envelope::new(
            &self.od_edges(),
            &tool.core,
            tool.nose_radius + allowance,
            ze,
            zs,
        );
        let nose = Self::level_rough(
            &env,
            self.stock.radius(),
            (ze, 0.0),
            zs,
            ze,
            p.depth_of_cut,
            self.safe_x(p),
            self.od_floor(),
            true,
        )?;
        self.push_op("rough OD", tool, p, nose)?;
        self.od_allowance = Some(allowance);
        Ok(self)
    }

    /// OD finishing: one pass along the exact envelope (nose-radius
    /// compensated), up the face corner and down the whole OD.
    pub fn finish_od(&mut self, tool: &LatheTool, p: &CutParams) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Turning], "OD finishing")?;
        self.register(tool)?;
        let zs = self.z_start(p);
        let ze = self.profile.z_min() - self.cutoff_allowance;
        let env = Envelope::new(&self.od_edges(), &tool.core, tool.nose_radius, ze, zs);
        let nose = Self::level_rough(
            &env,
            f64::NEG_INFINITY,
            (ze, 0.0),
            zs,
            ze,
            p.depth_of_cut,
            self.safe_x(p),
            self.od_floor(),
            true,
        )?;
        self.push_op("finish OD", tool, p, nose)?;
        self.od_allowance = Some(0.0);
        Ok(self)
    }

    /// Drill on the axis to `depth` (full diameter, from the face), pecking
    /// every `peck` mm. Needs a fixed-RPM spindle.
    pub fn drill(
        &mut self,
        tool: &LatheTool,
        p: &CutParams,
        depth: f64,
        peck: f64,
    ) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Drill], "drilling")?;
        if !matches!(p.speed, SpindleSpeed::Rpm { .. }) {
            return Err(TurnError::BadParams(
                "drilling on the axis needs G97 RPM: CSS at r = 0 is the rpm ceiling".into(),
            ));
        }
        if !(depth > 0.0 && peck > 0.0) {
            return Err(TurnError::BadParams("depth and peck must be > 0".into()));
        }
        self.register(tool)?;
        let zs = self.z_start(p);
        let tip = -depth - tool.drill_point_length();
        let mut nose = vec![([zs, 0.0], K::Rapid)];
        let mut z = 0.0f64;
        let mut first = true;
        while z > tip + 1e-9 {
            let next = (z - peck).max(tip);
            if !first {
                nose.push(([z + 0.3, 0.0], K::Rapid));
            }
            nose.push(([next, 0.0], K::Feed(1.0)));
            nose.push(([zs, 0.0], K::Rapid));
            z = next;
            first = false;
        }
        self.push_op("drill", tool, p, nose)?;
        let r = tool.drill_diameter.unwrap_or(0.0) / 2.0;
        if r >= self.hole_radius {
            self.hole_radius = r;
            self.hole_depth = depth;
        }
        Ok(self)
    }

    fn bore(
        &mut self,
        tool: &LatheTool,
        p: &CutParams,
        allowance: f64,
        finish: bool,
    ) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Boring], "boring")?;
        if !self.profile.has_bore() {
            return Err(TurnError::NotTurnable("the part has no bore".into()));
        }
        if self.hole_radius <= 0.0 {
            return Err(TurnError::NotTurnable(
                "boring needs a pre-drilled hole or tube stock: drill first".into(),
            ));
        }
        if let Some(min) = tool.min_bore_diameter {
            if min > 2.0 * self.hole_radius + 1e-9 {
                return Err(TurnError::NotTurnable(format!(
                    "T{} needs a Ø{min} bore, the hole is Ø{:.3}",
                    tool.number,
                    2.0 * self.hole_radius
                )));
            }
        }
        self.register(tool)?;
        let (edges, zdeep, through) = self.id_edges();
        let zs = self.z_start(p);
        let ze = if through {
            self.profile.z_min() - self.cutoff_allowance
        } else {
            zdeep - 1.0
        };
        let need = -ze;
        if through && self.hole_depth + 1e-6 < need {
            return Err(TurnError::NotTurnable(format!(
                "the pre-drilled hole is {:.2} deep, the through bore needs {need:.2}",
                self.hole_depth
            )));
        }
        // Mirror x → −x: the bore wall becomes an "OD" seen from below.
        let m = |q: P2| [q[0], -q[1]];
        let medges: Vec<(P2, P2)> = edges.iter().map(|(a, b)| (m(*a), m(*b))).collect();
        let mcore: Vec<P2> =
            geom::convex_hull(&tool.core.iter().map(|q| m(*q)).collect::<Vec<_>>());
        let r = tool.nose_radius + if finish { 0.0 } else { allowance };
        let env = Envelope::new(&medges, &mcore, r, ze, zs);
        // Park inside the hole with both the tip and the bar's far side
        // clear of the wall (the bar spans the hole's diameter).
        let reach_up = geom::bbox(&tool.core)[3] + tool.nose_radius;
        let reach_dn = match &tool.holder {
            Some(h) => geom::bbox(h)[1].min(geom::bbox(&tool.core)[1] - tool.nose_radius),
            None => geom::bbox(&tool.core)[1] - tool.nose_radius,
        };
        let (lo, hi) = (
            -self.hole_radius + 0.05 - reach_dn,
            self.hole_radius - 0.05 - reach_up,
        );
        if lo > hi {
            return Err(TurnError::NotTurnable(format!(
                "T{} does not fit in the Ø{:.3} hole with clearance",
                tool.number,
                2.0 * self.hole_radius
            )));
        }
        let safe_m = -0.5 * (lo + hi);
        let ri_face = self.profile.radii_at(-1e-6).map_or(0.0, |v| v.0);
        let floor_m = -(ri_face + 1.0).min(self.stock.radius());
        let top_m = if finish {
            f64::NEG_INFINITY
        } else {
            -self.hole_radius
        };
        let nose_m = Self::level_rough(
            &env,
            top_m,
            (ze.max(zdeep), 0.0),
            zs,
            ze,
            p.depth_of_cut,
            safe_m,
            floor_m,
            true,
        )?;
        let nose: Vec<(P2, K)> = nose_m.into_iter().map(|(q, k)| (m(q), k)).collect();
        self.push_op(
            if finish { "finish bore" } else { "rough bore" },
            tool,
            p,
            nose,
        )?;
        Ok(self)
    }

    /// Rough the bore from the drilled hole, leaving `allowance`.
    pub fn rough_bore(
        &mut self,
        tool: &LatheTool,
        p: &CutParams,
        allowance: f64,
    ) -> Result<&mut Self, TurnError> {
        self.bore(tool, p, allowance, false)
    }

    /// Finish the bore along the exact envelope.
    pub fn finish_bore(&mut self, tool: &LatheTool, p: &CutParams) -> Result<&mut Self, TurnError> {
        self.bore(tool, p, 0.0, true)
    }

    /// Plunge-groove the OD between `z_lo` and `z_hi` (the groove's opening),
    /// then finish the floor and corners in one pass.
    pub fn groove(
        &mut self,
        tool: &LatheTool,
        p: &CutParams,
        z_lo: f64,
        z_hi: f64,
    ) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Grooving], "grooving")?;
        self.register(tool)?;
        let w = tool.width.unwrap_or(0.0);
        let rc = tool.nose_radius;
        let eps = 1e-5;
        let cz_hi = z_hi - w + rc - eps;
        let cz_lo = z_lo + rc + eps;
        if cz_hi < cz_lo - 2.0 * eps {
            return Err(TurnError::NotTurnable(format!(
                "groove {:.3} wide is narrower than the {w} mm blade",
                z_hi - z_lo
            )));
        }
        let cz_hi = cz_hi.max(cz_lo);
        let zs = self.z_start(p);
        let safe = self.safe_x(p);
        let core_h = geom::bbox(&tool.core)[3];
        let env = Envelope::new(&self.od_edges(), &tool.core, rc, cz_lo - w, cz_hi + w);
        let x_top = match self.od_allowance {
            Some(u) => {
                let mut m = 0.0f64;
                let n = 50;
                for i in 0..=n {
                    let z = (z_lo - w) + (z_hi - z_lo + 2.0 * w) * i as f64 / n as f64;
                    if let Some((_, ro)) = self.profile.radii_at(z.min(-1e-6)) {
                        m = m.max(ro);
                    }
                }
                (m + u + p.clearance).min(safe)
            }
            None => safe,
        };
        let n = ((cz_hi - cz_lo) / (0.8 * w)).ceil().max(0.0) as usize;
        let mut nose = vec![([zs, safe], K::Rapid)];
        for i in 0..=n {
            let cz = if n == 0 {
                cz_hi
            } else {
                cz_hi - (cz_hi - cz_lo) * i as f64 / n as f64
            };
            let bottom = env.value(cz);
            if x_top - (bottom - rc) > core_h + 1e-9 {
                return Err(TurnError::NotTurnable(format!(
                    "groove at z = {cz:.3} is {:.3} deep, the blade reaches {core_h:.3}",
                    x_top - bottom + rc
                )));
            }
            nose.push(([cz, safe], K::Rapid));
            nose.push(([cz, x_top], K::Rapid));
            nose.push(([cz, bottom + 0.05], K::Feed(1.0)));
            nose.push(([cz, x_top], K::Feed(1.0)));
        }
        // Finish: down the right side, along the floor, up the left side.
        let path = env.path(cz_hi, cz_lo, f64::NEG_INFINITY, GEN_TOL);
        nose.push(([cz_hi, x_top], K::Rapid));
        for q in &path {
            nose.push((*q, K::Feed(0.5)));
        }
        nose.push(([cz_lo, x_top], K::Feed(1.0)));
        nose.push(([cz_lo, safe], K::Rapid));
        nose.push(([zs, safe], K::Rapid));
        self.push_op("groove", tool, p, nose)?;
        Ok(self)
    }

    /// Part off at the chuck end of the part, slowing to half feed for the
    /// last 2 mm. `overcut` drives the blade past the centre (or bore).
    pub fn part_off(
        &mut self,
        tool: &LatheTool,
        p: &CutParams,
        overcut: f64,
    ) -> Result<&mut Self, TurnError> {
        p.validate()?;
        Self::want(tool, &[LatheToolKind::Parting], "parting")?;
        self.register(tool)?;
        let w = tool.width.unwrap_or(0.0);
        let rc = tool.nose_radius;
        let zmin = self.profile.z_min();
        let cz = zmin - w + rc - 1e-5;
        let inner = self.profile.radii_at(zmin + 1e-6).map_or(0.0, |v| v.0);
        let target = if inner > 1e-6 { inner } else { 0.0 } - overcut.max(0.0);
        let x_end = target + rc;
        let safe = self.safe_x(p);
        let zs = self.z_start(p);
        let core_h = geom::bbox(&tool.core)[3];
        let top = self.stock.radius();
        if top - target > core_h + 1e-9 {
            return Err(TurnError::NotTurnable(format!(
                "parting {:.3} deep, the blade reaches {core_h:.3}",
                top - target
            )));
        }
        let slow = (x_end + 2.0).min(safe);
        let nose = vec![
            ([zs, safe], K::Rapid),
            ([cz, safe], K::Rapid),
            ([cz, slow], K::Feed(1.0)),
            ([cz, x_end], K::Feed(0.5)),
            ([cz, safe], K::Rapid),
            ([zs, safe], K::Rapid),
        ];
        self.push_op("part off", tool, p, nose)?;
        Ok(self)
    }

    /// Finish planning.
    pub fn program(&self, name: impl Into<String>) -> TurnProgram {
        TurnProgram {
            name: name.into(),
            tools: self.tools.clone(),
            ops: self.ops.clone(),
        }
    }
}
