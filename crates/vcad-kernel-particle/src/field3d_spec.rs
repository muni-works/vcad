//! The JSON seam for [`crate::field3d`]: posed sources in millimeters and
//! degrees, query points or a plane grid, and a report of B vectors.
//!
//! This is what the `magnetic_field` MCP tool speaks. Validation is
//! fail-closed: non-positive dimensions, a zero axis, contradictory pose
//! fields, an oversized grid, or a non-finite field at a query point (a
//! point exactly on a filament or a magnet edge) are errors, never silent
//! `null`s.

use serde::{Deserialize, Serialize};

use crate::field3d::{FieldScene, FieldSource, Pose};

/// Most query points one evaluation accepts.
pub const MAX_POINTS: usize = 20_000;

/// Central-difference step for `gradient: true`, mm.
pub const GRADIENT_STEP_MM: f64 = 0.01;

/// Where a source sits. Local `+z` is the coil axis / magnetization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoseSpec {
    /// Source center, mm. Default origin.
    #[serde(default)]
    pub position_mm: [f64; 3],
    /// Direction of the source's local `+z` (north pole / coil axis).
    #[serde(default)]
    pub axis: Option<[f64; 3]>,
    /// With `axis`: direction of the source's local `+x` (its orthogonal
    /// part is used). Only matters for block magnets.
    #[serde(default)]
    pub x_axis: Option<[f64; 3]>,
    /// Euler angles about X, Y, Z in degrees, `R = Rx·Ry·Rz` (the IR
    /// `Rotate` convention). Exclusive with `axis`.
    #[serde(default)]
    pub rotation_deg: Option<[f64; 3]>,
}

fn one() -> f64 {
    1.0
}

/// One posed source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceSpec {
    /// Circular current loop.
    Loop {
        /// Loop radius, mm.
        radius_mm: f64,
        /// Current, A (+ = CCW about the local +z axis).
        current_a: f64,
        /// Turns. Default 1.
        #[serde(default = "one")]
        turns: f64,
        /// Conductor radius, mm (regularizes the field inside the wire).
        /// Default 0 = filament.
        #[serde(default)]
        wire_radius_mm: f64,
        /// Placement.
        #[serde(default)]
        pose: PoseSpec,
    },
    /// Thin finite solenoid centered on its pose.
    Solenoid {
        /// Winding radius, mm.
        radius_mm: f64,
        /// Winding length, mm.
        length_mm: f64,
        /// Current, A.
        current_a: f64,
        /// Total turns.
        turns: f64,
        /// Discretize as this many loops instead of the exact closed form.
        #[serde(default)]
        n_loops: Option<usize>,
        /// Placement.
        #[serde(default)]
        pose: PoseSpec,
    },
    /// Axially magnetized disc / rod (`inner_radius_mm` 0) or ring magnet.
    CylinderMagnet {
        /// Outer radius, mm.
        radius_mm: f64,
        /// Bore radius, mm. Default 0 (solid).
        #[serde(default)]
        inner_radius_mm: f64,
        /// Length along the magnetization, mm.
        length_mm: f64,
        /// Remanence B_r, tesla (N52 ≈ 1.43, N42 ≈ 1.3, ferrite ≈ 0.4).
        remanence_t: f64,
        /// Discretize as this many loops instead of the exact closed form.
        #[serde(default)]
        n_loops: Option<usize>,
        /// Placement.
        #[serde(default)]
        pose: PoseSpec,
    },
    /// Rectangular block magnet magnetized along its local z edge.
    BlockMagnet {
        /// Full edge lengths along local x, y, z (z = magnetization), mm.
        size_mm: [f64; 3],
        /// Remanence B_r, tesla.
        remanence_t: f64,
        /// Placement.
        #[serde(default)]
        pose: PoseSpec,
    },
}

/// The sources to superpose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSpec {
    /// At least one source.
    pub sources: Vec<SourceSpec>,
}

/// A rectangular grid of points on a plane:
/// `origin + u·u_dir + v·v_dir` for `u ∈ u_range`, `v ∈ v_range`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridSpec {
    /// Plane origin, mm. Default world origin.
    #[serde(default)]
    pub origin_mm: [f64; 3],
    /// First in-plane direction. Default +X.
    #[serde(default)]
    pub u_dir: Option<[f64; 3]>,
    /// Second in-plane direction. Default +Y.
    #[serde(default)]
    pub v_dir: Option<[f64; 3]>,
    /// `[min, max]` along `u_dir`, mm.
    pub u_range_mm: [f64; 2],
    /// `[min, max]` along `v_dir`, mm.
    pub v_range_mm: [f64; 2],
    /// Samples along u (≥ 1; 1 = the range midpoint).
    pub nu: usize,
    /// Samples along v (≥ 1).
    pub nv: usize,
}

/// What to sample.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySpec {
    /// Explicit points, mm.
    #[serde(default)]
    pub points_mm: Option<Vec<[f64; 3]>>,
    /// A plane grid (appended after `points_mm`, row-major in v then u:
    /// index = iv·nu + iu).
    #[serde(default)]
    pub grid: Option<GridSpec>,
    /// Also return ∇|B| (T/m) at every point.
    #[serde(default)]
    pub gradient: bool,
}

/// Per-source summary in the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceSummary {
    /// Source kind.
    pub kind: &'static str,
    /// `"exact"` or `"loop_stack(n)"`.
    pub method: String,
    /// Dipole moment magnitude along `axis`, A·m².
    pub moment_a_m2: f64,
    /// The source's local +z in world coordinates.
    pub axis: [f64; 3],
}

/// Grid dimensions echoed back.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GridShape {
    /// Samples along u.
    pub nu: usize,
    /// Samples along v.
    pub nv: usize,
    /// Index of the first grid point in the point arrays.
    pub offset: usize,
}

/// Result of [`evaluate`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldReport {
    /// The sampled points, mm.
    pub points_mm: Vec<[f64; 3]>,
    /// B at each point, tesla.
    pub b_t: Vec<[f64; 3]>,
    /// |B| at each point, tesla.
    pub b_mag_t: Vec<f64>,
    /// ∇|B| at each point, T/m (only with `gradient: true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grad_b_mag_t_per_m: Option<Vec<[f64; 3]>>,
    /// Grid layout, when a grid was sampled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid: Option<GridShape>,
    /// Largest |B| over the points, tesla.
    pub max_b_mag_t: f64,
    /// Where it occurs, mm.
    pub max_at_mm: [f64; 3],
    /// Smallest |B| over the points, tesla.
    pub min_b_mag_t: f64,
    /// One entry per source, in input order.
    pub sources: Vec<SourceSummary>,
}

fn finite3(v: [f64; 3]) -> bool {
    v.iter().all(|x| x.is_finite())
}

fn mm(v: [f64; 3]) -> [f64; 3] {
    [v[0] * 1e-3, v[1] * 1e-3, v[2] * 1e-3]
}

fn positive(name: &str, v: f64) -> Result<f64, String> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(format!("{name} must be a positive finite number (got {v})"))
    }
}

fn finite(name: &str, v: f64) -> Result<f64, String> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(format!("{name} must be finite (got {v})"))
    }
}

fn nonzero_dir(name: &str, v: [f64; 3]) -> Result<[f64; 3], String> {
    if finite3(v) && v.iter().any(|x| *x != 0.0) {
        Ok(v)
    } else {
        Err(format!(
            "{name} must be a finite nonzero vector (got {v:?})"
        ))
    }
}

impl PoseSpec {
    /// Resolve to an SI [`Pose`].
    pub fn resolve(&self, ctx: &str) -> Result<Pose, String> {
        if !finite3(self.position_mm) {
            return Err(format!("{ctx}: position_mm must be finite"));
        }
        let pos = mm(self.position_mm);
        match (self.rotation_deg, self.axis, self.x_axis) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(format!(
                "{ctx}: give either rotation_deg or axis (+ optional x_axis), not both"
            )),
            (Some(r), None, None) => {
                if !finite3(r) {
                    return Err(format!("{ctx}: rotation_deg must be finite"));
                }
                Ok(Pose::from_euler_deg(pos, r))
            }
            (None, Some(a), None) => Ok(Pose::from_axis(
                pos,
                nonzero_dir(&format!("{ctx}: axis"), a)?,
            )),
            (None, Some(a), Some(x)) => {
                let a = nonzero_dir(&format!("{ctx}: axis"), a)?;
                let x = nonzero_dir(&format!("{ctx}: x_axis"), x)?;
                Pose::from_axis_and_x(pos, a, x)
                    .ok_or_else(|| format!("{ctx}: x_axis is parallel to axis"))
            }
            (None, None, Some(_)) => Err(format!("{ctx}: x_axis requires axis")),
            (None, None, None) => Ok(Pose::translation(pos)),
        }
    }
}

impl SourceSpec {
    /// Resolve to an SI source and pose.
    pub fn resolve(&self, ctx: &str) -> Result<(FieldSource, Pose), String> {
        let n_ok = |n: Option<usize>| match n {
            Some(0) => Err(format!("{ctx}: n_loops must be ≥ 1 (omit it for exact)")),
            _ => Ok(n),
        };
        match self {
            SourceSpec::Loop {
                radius_mm,
                current_a,
                turns,
                wire_radius_mm,
                pose,
            } => {
                let r = positive(&format!("{ctx}: radius_mm"), *radius_mm)?;
                let w = finite(&format!("{ctx}: wire_radius_mm"), *wire_radius_mm)?;
                if !(0.0..r).contains(&w) {
                    return Err(format!("{ctx}: wire_radius_mm must be in [0, radius_mm)"));
                }
                let at = finite(&format!("{ctx}: current_a"), *current_a)?
                    * finite(&format!("{ctx}: turns"), *turns)?;
                Ok((
                    FieldSource::CurrentLoop {
                        radius_m: r * 1e-3,
                        ampere_turns: at,
                        wire_radius_m: w * 1e-3,
                    },
                    pose.resolve(ctx)?,
                ))
            }
            SourceSpec::Solenoid {
                radius_mm,
                length_mm,
                current_a,
                turns,
                n_loops,
                pose,
            } => Ok((
                FieldSource::Solenoid {
                    radius_m: positive(&format!("{ctx}: radius_mm"), *radius_mm)? * 1e-3,
                    length_m: positive(&format!("{ctx}: length_mm"), *length_mm)? * 1e-3,
                    ampere_turns: finite(&format!("{ctx}: current_a"), *current_a)?
                        * finite(&format!("{ctx}: turns"), *turns)?,
                    n_loops: n_ok(*n_loops)?,
                },
                pose.resolve(ctx)?,
            )),
            SourceSpec::CylinderMagnet {
                radius_mm,
                inner_radius_mm,
                length_mm,
                remanence_t,
                n_loops,
                pose,
            } => {
                let r = positive(&format!("{ctx}: radius_mm"), *radius_mm)?;
                let ri = finite(&format!("{ctx}: inner_radius_mm"), *inner_radius_mm)?;
                if !(0.0..r).contains(&ri) {
                    return Err(format!("{ctx}: inner_radius_mm must be in [0, radius_mm)"));
                }
                Ok((
                    FieldSource::CylinderMagnet {
                        outer_radius_m: r * 1e-3,
                        inner_radius_m: ri * 1e-3,
                        length_m: positive(&format!("{ctx}: length_mm"), *length_mm)? * 1e-3,
                        remanence_t: finite(&format!("{ctx}: remanence_t"), *remanence_t)?,
                        n_loops: n_ok(*n_loops)?,
                    },
                    pose.resolve(ctx)?,
                ))
            }
            SourceSpec::BlockMagnet {
                size_mm,
                remanence_t,
                pose,
            } => {
                let mut s = [0.0; 3];
                for (i, v) in size_mm.iter().enumerate() {
                    s[i] = positive(&format!("{ctx}: size_mm[{i}]"), *v)? * 1e-3;
                }
                Ok((
                    FieldSource::BlockMagnet {
                        size_m: s,
                        remanence_t: finite(&format!("{ctx}: remanence_t"), *remanence_t)?,
                    },
                    pose.resolve(ctx)?,
                ))
            }
        }
    }
}

impl SceneSpec {
    /// Resolve every source into an SI [`FieldScene`].
    pub fn resolve(&self) -> Result<FieldScene, String> {
        if self.sources.is_empty() {
            return Err("scene has no sources".into());
        }
        let mut scene = FieldScene::new();
        for (i, s) in self.sources.iter().enumerate() {
            let (src, pose) = s.resolve(&format!("sources[{i}]"))?;
            scene.push(src, pose);
        }
        Ok(scene)
    }
}

impl QuerySpec {
    /// Expand to the list of query points (mm) and the grid layout.
    pub fn points(&self) -> Result<(Vec<[f64; 3]>, Option<GridShape>), String> {
        let mut pts: Vec<[f64; 3]> = self.points_mm.clone().unwrap_or_default();
        if let Some((i, _)) = pts.iter().enumerate().find(|(_, p)| !finite3(**p)) {
            return Err(format!("points_mm[{i}] must be finite"));
        }
        let mut shape = None;
        if let Some(g) = &self.grid {
            if g.nu == 0 || g.nv == 0 {
                return Err("grid.nu and grid.nv must be ≥ 1".into());
            }
            let n = g.nu.saturating_mul(g.nv);
            if n.saturating_add(pts.len()) > MAX_POINTS {
                return Err(format!(
                    "{} query points exceeds the limit of {MAX_POINTS}; coarsen the grid",
                    n.saturating_add(pts.len())
                ));
            }
            let u = nonzero_dir("grid.u_dir", g.u_dir.unwrap_or([1.0, 0.0, 0.0]))?;
            let v = nonzero_dir("grid.v_dir", g.v_dir.unwrap_or([0.0, 1.0, 0.0]))?;
            let (u, v) = (crate::field3d::normalize(u), crate::field3d::normalize(v));
            if crate::field3d::norm(crate::field3d::cross(u, v)) < 1e-9 {
                return Err("grid.u_dir and grid.v_dir are parallel".into());
            }
            if !finite3(g.origin_mm)
                || !g
                    .u_range_mm
                    .iter()
                    .chain(&g.v_range_mm)
                    .all(|x| x.is_finite())
            {
                return Err("grid origin and ranges must be finite".into());
            }
            let lin = |r: [f64; 2], n: usize, k: usize| {
                if n == 1 {
                    0.5 * (r[0] + r[1])
                } else {
                    r[0] + (r[1] - r[0]) * k as f64 / (n - 1) as f64
                }
            };
            shape = Some(GridShape {
                nu: g.nu,
                nv: g.nv,
                offset: pts.len(),
            });
            for iv in 0..g.nv {
                let b = lin(g.v_range_mm, g.nv, iv);
                for iu in 0..g.nu {
                    let a = lin(g.u_range_mm, g.nu, iu);
                    pts.push([
                        g.origin_mm[0] + a * u[0] + b * v[0],
                        g.origin_mm[1] + a * u[1] + b * v[1],
                        g.origin_mm[2] + a * u[2] + b * v[2],
                    ]);
                }
            }
        }
        if pts.is_empty() {
            return Err("query has no points: give points_mm and/or grid".into());
        }
        if pts.len() > MAX_POINTS {
            return Err(format!(
                "{} query points exceeds the limit of {MAX_POINTS}",
                pts.len()
            ));
        }
        Ok((pts, shape))
    }
}

/// Resolve the scene, sample the query, and report.
pub fn evaluate(scene: &SceneSpec, query: &QuerySpec) -> Result<FieldReport, String> {
    let field = scene.resolve()?;
    let (points_mm, grid) = query.points()?;
    let h = GRADIENT_STEP_MM * 1e-3;
    let mut b_t = Vec::with_capacity(points_mm.len());
    let mut b_mag_t = Vec::with_capacity(points_mm.len());
    let mut grads = query.gradient.then(|| Vec::with_capacity(points_mm.len()));
    let (mut max_b, mut max_at, mut min_b) = (f64::NEG_INFINITY, [0.0; 3], f64::INFINITY);
    for (i, &p) in points_mm.iter().enumerate() {
        let ps = mm(p);
        let b = field.b(ps);
        let singular = || {
            format!(
                "B is not finite at query point {i} ({p:?} mm): it lies on a source \
                 singularity (a filament loop with wire_radius_mm = 0, a magnet edge, \
                 or a sheet rim). Move the point or the source, or give the loop a \
                 wire_radius_mm."
            )
        };
        if !finite3(b) {
            return Err(singular());
        }
        let m = crate::field3d::norm(b);
        if m > max_b {
            max_b = m;
            max_at = p;
        }
        min_b = min_b.min(m);
        b_t.push(b);
        b_mag_t.push(m);
        if let Some(g) = grads.as_mut() {
            let gv = field.grad_magnitude(ps, h);
            if !finite3(gv) {
                return Err(singular());
            }
            g.push(gv);
        }
    }
    let sources = field
        .sources
        .iter()
        .map(|s| SourceSummary {
            kind: s.source.kind(),
            method: s.source.method(),
            moment_a_m2: s.source.moment(),
            axis: s.pose.axis(),
        })
        .collect();
    Ok(FieldReport {
        points_mm,
        b_t,
        b_mag_t,
        grad_b_mag_t_per_m: grads,
        grid,
        max_b_mag_t: max_b,
        max_at_mm: max_at,
        min_b_mag_t: min_b,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field3d::DiscMagnet;

    fn parse(scene: &str, query: &str) -> Result<FieldReport, String> {
        let s: SceneSpec = serde_json::from_str(scene).map_err(|e| e.to_string())?;
        let q: QuerySpec = serde_json::from_str(query).map_err(|e| e.to_string())?;
        evaluate(&s, &q)
    }

    #[test]
    fn json_disc_matches_kernel_in_mm() {
        let r = parse(
            r#"{"sources":[{"type":"cylinder_magnet","radius_mm":5,"length_mm":5,
                "remanence_t":1.43,"pose":{"position_mm":[0,0,0],"axis":[0,0,1]}}]}"#,
            r#"{"points_mm":[[0,0,10]]}"#,
        )
        .unwrap();
        let exact = DiscMagnet::n52_10x5().b_axis_exact(10e-3);
        assert!((r.b_t[0][2] - exact).abs() < 1e-12 * exact);
        assert_eq!(r.sources[0].method, "exact");
        assert!((r.sources[0].moment_a_m2 - DiscMagnet::n52_10x5().moment()).abs() < 1e-12);
    }

    #[test]
    fn grid_layout_gradient_and_summary() {
        let r = parse(
            r#"{"sources":[{"type":"block_magnet","size_mm":[10,10,5],"remanence_t":1.3,
                "pose":{"position_mm":[0,0,-2.5]}},
               {"type":"loop","radius_mm":30,"current_a":5,"turns":20,"wire_radius_mm":1,
                "pose":{"position_mm":[0,0,20],"rotation_deg":[10,0,0]}}]}"#,
            r#"{"points_mm":[[0,0,40]],"gradient":true,
                "grid":{"origin_mm":[0,0,3],"u_range_mm":[-20,20],"v_range_mm":[-10,10],
                        "nu":5,"nv":3}}"#,
        )
        .unwrap();
        assert_eq!(r.points_mm.len(), 1 + 15);
        let g = r.grid.unwrap();
        assert_eq!((g.nu, g.nv, g.offset), (5, 3, 1));
        // Row-major: index offset + iv·nu + iu.
        assert_eq!(r.points_mm[1 + 5 + 2], [0.0, 0.0, 3.0]);
        assert_eq!(r.grad_b_mag_t_per_m.as_ref().unwrap().len(), 16);
        // Peak |B| on this plane is above the block's pole.
        assert!(r.max_b_mag_t >= r.b_mag_t[1 + 5 + 2] - 1e-15);
        assert_eq!(r.sources.len(), 2);
        assert!(r.b_mag_t.iter().all(|m| m.is_finite()));
    }

    #[test]
    fn fails_closed() {
        let disc = r#"{"sources":[{"type":"cylinder_magnet","radius_mm":5,"length_mm":5,"remanence_t":1.4}]}"#;
        // No points.
        assert!(parse(disc, "{}").is_err());
        // Oversized grid.
        assert!(parse(
            disc,
            r#"{"grid":{"u_range_mm":[0,1],"v_range_mm":[0,1],"nu":1000,"nv":1000}}"#
        )
        .is_err());
        // Conflicting pose fields.
        assert!(parse(
            r#"{"sources":[{"type":"loop","radius_mm":5,"current_a":1,
                "pose":{"axis":[0,0,1],"rotation_deg":[0,0,0]}}]}"#,
            r#"{"points_mm":[[0,0,1]]}"#
        )
        .is_err());
        // Zero axis, negative size, bad bore.
        for bad in [
            r#"{"sources":[{"type":"loop","radius_mm":5,"current_a":1,"pose":{"axis":[0,0,0]}}]}"#,
            r#"{"sources":[{"type":"block_magnet","size_mm":[1,-1,1],"remanence_t":1}]}"#,
            r#"{"sources":[{"type":"cylinder_magnet","radius_mm":5,"inner_radius_mm":5,"length_mm":1,"remanence_t":1}]}"#,
            r#"{"sources":[]}"#,
        ] {
            assert!(parse(bad, r#"{"points_mm":[[0,0,9]]}"#).is_err(), "{bad}");
        }
        // A point exactly on a filament or a block edge: error, not null.
        assert!(parse(
            r#"{"sources":[{"type":"loop","radius_mm":10,"current_a":1}]}"#,
            r#"{"points_mm":[[10,0,0]]}"#
        )
        .unwrap_err()
        .contains("not finite"));
        assert!(parse(
            r#"{"sources":[{"type":"block_magnet","size_mm":[10,10,10],"remanence_t":1}]}"#,
            r#"{"points_mm":[[5,0,5]]}"#
        )
        .is_err());
        // Typos in pose fields are rejected, not ignored.
        assert!(parse(
            r#"{"sources":[{"type":"loop","radius_mm":5,"current_a":1,"pose":{"positon_mm":[0,0,0]}}]}"#,
            r#"{"points_mm":[[0,0,1]]}"#
        )
        .is_err());
    }
}
