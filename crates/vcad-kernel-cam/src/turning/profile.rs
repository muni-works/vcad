//! The half-section of a turned part: exact lines and arcs in (z, r).

use super::geom::{self, P2};
use super::TurnError;
use serde::{Deserialize, Serialize};

/// One exact piece of a half-section boundary, in `[z, r]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProfileSeg {
    /// Straight segment.
    Line {
        /// Start point.
        a: P2,
        /// End point.
        b: P2,
    },
    /// Circular arc from `a` to `b` about `center`.
    Arc {
        /// Start point.
        a: P2,
        /// End point.
        b: P2,
        /// Arc centre.
        center: P2,
        /// Counter-clockwise in the (z right, r up) plane.
        ccw: bool,
    },
}

impl ProfileSeg {
    /// Start point.
    pub fn start(&self) -> P2 {
        match *self {
            ProfileSeg::Line { a, .. } | ProfileSeg::Arc { a, .. } => a,
        }
    }

    /// End point.
    pub fn end(&self) -> P2 {
        match *self {
            ProfileSeg::Line { b, .. } | ProfileSeg::Arc { b, .. } => b,
        }
    }

    fn sweep(&self) -> f64 {
        match *self {
            ProfileSeg::Line { .. } => 0.0,
            ProfileSeg::Arc { a, b, center, ccw } => {
                let a0 = (a[1] - center[1]).atan2(a[0] - center[0]);
                let a1 = (b[1] - center[1]).atan2(b[0] - center[0]);
                let mut s = a1 - a0;
                if ccw {
                    while s <= 1e-12 {
                        s += std::f64::consts::TAU;
                    }
                } else {
                    while s >= -1e-12 {
                        s -= std::f64::consts::TAU;
                    }
                }
                s
            }
        }
    }

    /// Point at parameter `t ∈ [0, 1]`.
    pub fn point_at(&self, t: f64) -> P2 {
        match *self {
            ProfileSeg::Line { a, b } => geom::add(a, geom::scale(geom::sub(b, a), t)),
            ProfileSeg::Arc { a, center, .. } => {
                let r = geom::dist(a, center);
                let a0 = (a[1] - center[1]).atan2(a[0] - center[0]);
                let ang = a0 + self.sweep() * t;
                [center[0] + r * ang.cos(), center[1] + r * ang.sin()]
            }
        }
    }

    /// Polyline approximation with chord sagitta ≤ `tol` (start included,
    /// end included).
    pub fn polyline(&self, tol: f64) -> Vec<P2> {
        match *self {
            ProfileSeg::Line { a, b } => vec![a, b],
            ProfileSeg::Arc { a, center, .. } => {
                let r = geom::dist(a, center);
                let sweep = self.sweep().abs();
                let tol = tol.max(1e-9).min(r);
                let max_step = 2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).acos();
                let n = ((sweep / max_step.max(1e-6)).ceil() as usize).max(1);
                (0..=n)
                    .map(|i| self.point_at(i as f64 / n as f64))
                    .collect()
            }
        }
    }

    fn translated(&self, dz: f64) -> Self {
        let t = |p: P2| [p[0] + dz, p[1]];
        match *self {
            ProfileSeg::Line { a, b } => ProfileSeg::Line { a: t(a), b: t(b) },
            ProfileSeg::Arc { a, b, center, ccw } => ProfileSeg::Arc {
                a: t(a),
                b: t(b),
                center: t(center),
                ccw,
            },
        }
    }

    fn reversed(&self) -> Self {
        match *self {
            ProfileSeg::Line { a, b } => ProfileSeg::Line { a: b, b: a },
            ProfileSeg::Arc { a, b, center, ccw } => ProfileSeg::Arc {
                a: b,
                b: a,
                center,
                ccw: !ccw,
            },
        }
    }
}

/// Which part of the turned part a boundary piece belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegRole {
    /// Outside diameter, including shoulders between diameters.
    Outer,
    /// Bore wall, including bore shoulders and a blind bore's bottom.
    Inner,
    /// The finished face at the tailstock end (`z = z_max`).
    RightFace,
    /// The parted face at the chuck end (`z = z_min`).
    LeftFace,
    /// Lies on the spindle axis (not a real surface).
    Axis,
}

/// A closed half-section of a solid of revolution, CCW, every point at
/// `r ≥ 0`. `z` runs along the spindle axis, positive toward the tailstock;
/// the finished face is `z_max`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnProfile {
    segs: Vec<ProfileSeg>,
}

/// Chord tolerance used internally for analysis (scanlines, roles).
const ANALYSIS_TOL: f64 = 1e-4;
/// Geometric epsilon for role classification (mm).
const EPS: f64 = 1e-5;

impl TurnProfile {
    /// Build from exact segments forming one closed loop.
    pub fn new(segs: Vec<ProfileSeg>) -> Result<Self, TurnError> {
        if segs.len() < 3 {
            return Err(TurnError::BadProfile("fewer than three segments".into()));
        }
        for (i, s) in segs.iter().enumerate() {
            let next = segs[(i + 1) % segs.len()].start();
            let gap = geom::dist(s.end(), next);
            if gap > 1e-6 {
                return Err(TurnError::BadProfile(format!(
                    "loop not closed after segment {i}: gap {gap:.6} mm"
                )));
            }
            for p in s.polyline(ANALYSIS_TOL) {
                if p[1] < -1e-9 {
                    return Err(TurnError::BadProfile(format!(
                        "segment {i} crosses the axis (r = {:.6})",
                        p[1]
                    )));
                }
            }
        }
        let mut prof = TurnProfile { segs };
        if prof.signed_area() < 0.0 {
            prof.segs = prof.segs.iter().rev().map(|s| s.reversed()).collect();
        }
        if prof.signed_area().abs() < 1e-9 {
            return Err(TurnError::BadProfile("zero-area half-section".into()));
        }
        prof.check_single_interval()?;
        Ok(prof)
    }

    /// Build from a closed polyline of `[z, r]` points (last ≠ first).
    pub fn from_points(points: &[P2]) -> Result<Self, TurnError> {
        let n = points.len();
        let segs = (0..n)
            .map(|i| ProfileSeg::Line {
                a: points[i],
                b: points[(i + 1) % n],
            })
            .filter(|s| geom::dist(s.start(), s.end()) > 1e-12)
            .collect();
        Self::new(segs)
    }

    /// The exact boundary segments (CCW).
    pub fn segments(&self) -> &[ProfileSeg] {
        &self.segs
    }

    /// Polygon approximation (CCW) with chord sagitta ≤ `tol`.
    pub fn polygon(&self, tol: f64) -> Vec<P2> {
        let mut out = Vec::new();
        for s in &self.segs {
            let pl = s.polyline(tol);
            out.extend_from_slice(&pl[..pl.len() - 1]);
        }
        out
    }

    fn signed_area(&self) -> f64 {
        let p = self.polygon(ANALYSIS_TOL);
        let n = p.len();
        (0..n)
            .map(|i| geom::cross(p[i], p[(i + 1) % n]))
            .sum::<f64>()
            / 2.0
    }

    /// Smallest `z` (chuck end).
    pub fn z_min(&self) -> f64 {
        geom::bbox(&self.polygon(ANALYSIS_TOL))[0]
    }

    /// Largest `z` (finished face).
    pub fn z_max(&self) -> f64 {
        geom::bbox(&self.polygon(ANALYSIS_TOL))[2]
    }

    /// Largest radius.
    pub fn max_radius(&self) -> f64 {
        geom::bbox(&self.polygon(ANALYSIS_TOL))[3]
    }

    /// The same profile shifted so the finished face sits at `z = 0`.
    pub fn face_at_zero(&self) -> Self {
        let dz = -self.z_max();
        TurnProfile {
            segs: self.segs.iter().map(|s| s.translated(dz)).collect(),
        }
    }

    /// Material intervals `[r0, r1]` along the radial line at `z`.
    pub fn intervals_at(&self, z: f64) -> Vec<(f64, f64)> {
        let p = self.polygon(ANALYSIS_TOL);
        let n = p.len();
        let mut rs: Vec<f64> = Vec::new();
        for i in 0..n {
            let (a, b) = (p[i], p[(i + 1) % n]);
            if (a[0] > z) != (b[0] > z) {
                rs.push(a[1] + (z - a[0]) / (b[0] - a[0]) * (b[1] - a[1]));
            }
        }
        rs.sort_by(f64::total_cmp);
        rs.chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0], c[1]))
            .collect()
    }

    /// Material radius bounds `(r_inner, r_outer)` at `z`, if the radial
    /// line at `z` crosses material.
    pub fn radii_at(&self, z: f64) -> Option<(f64, f64)> {
        let iv = self.intervals_at(z);
        if iv.is_empty() {
            return None;
        }
        Some((iv[0].0, iv[iv.len() - 1].1))
    }

    fn check_single_interval(&self) -> Result<(), TurnError> {
        let mut zs: Vec<f64> = self.polygon(ANALYSIS_TOL).iter().map(|p| p[0]).collect();
        zs.sort_by(f64::total_cmp);
        zs.dedup_by(|a, b| (*a - *b).abs() < 1e-7);
        for w in zs.windows(2) {
            let z = 0.5 * (w[0] + w[1]);
            let iv = self.intervals_at(z);
            if iv.len() > 1 {
                return Err(TurnError::NotTurnable(format!(
                    "{} separate material bands at z = {z:.3} (an internal cavity or a face \
                     groove): not reachable from the OD or a bore in one setup",
                    iv.len()
                )));
            }
        }
        Ok(())
    }

    /// Each exact segment with its role.
    pub fn classified(&self) -> Vec<(ProfileSeg, SegRole)> {
        let (zmin, zmax) = (self.z_min(), self.z_max());
        self.segs
            .iter()
            .map(|s| (*s, self.role_of(s, zmin, zmax)))
            .collect()
    }

    fn role_of(&self, s: &ProfileSeg, zmin: f64, zmax: f64) -> SegRole {
        let (a, b) = (s.start(), s.end());
        let m = s.point_at(0.5);
        if a[1] < EPS && b[1] < EPS && m[1] < EPS {
            return SegRole::Axis;
        }
        let vertical = matches!(s, ProfileSeg::Line { .. }) && (a[0] - b[0]).abs() < EPS;
        if vertical {
            let z0 = a[0];
            if (z0 - zmax).abs() < EPS {
                return SegRole::RightFace;
            }
            if (z0 - zmin).abs() < EPS {
                return SegRole::LeftFace;
            }
            let d = 1e-3;
            let l = self.radii_at(z0 - d);
            let r = self.radii_at(z0 + d);
            let ro = match (l, r) {
                (Some(l), Some(r)) => l.1.min(r.1),
                (Some(l), None) => l.1,
                (None, Some(r)) => r.1,
                (None, None) => return SegRole::Outer,
            };
            return if m[1] >= ro - 1e-3 {
                SegRole::Outer
            } else {
                SegRole::Inner
            };
        }
        match self.radii_at(m[0]) {
            Some((ri, ro)) => {
                if (m[1] - ro).abs() <= (m[1] - ri).abs() {
                    SegRole::Outer
                } else if ri > EPS {
                    SegRole::Inner
                } else {
                    SegRole::Axis
                }
            }
            None => SegRole::Outer,
        }
    }

    /// Does the part have a bore (any `Inner` boundary)?
    pub fn has_bore(&self) -> bool {
        self.classified().iter().any(|(_, r)| *r == SegRole::Inner)
    }

    /// Boundary of the full axial section (both halves), axis segments
    /// dropped: a closed edge soup for region tests.
    pub fn full_section_edges(&self, tol: f64) -> Vec<(P2, P2)> {
        let mut out = Vec::new();
        for (s, role) in self.classified() {
            if role == SegRole::Axis {
                continue;
            }
            let pl = s.polyline(tol);
            for w in pl.windows(2) {
                out.push((w[0], w[1]));
                out.push(([w[0][0], -w[0][1]], [w[1][0], -w[1][1]]));
            }
        }
        out
    }
}

/// Incremental builder for profiles: start at a point, add lines and arcs.
#[derive(Debug, Clone)]
pub struct ProfileBuilder {
    segs: Vec<ProfileSeg>,
    cur: P2,
    start: P2,
}

impl ProfileBuilder {
    /// Start at `[z, r]`.
    pub fn start(p: P2) -> Self {
        Self {
            segs: Vec::new(),
            cur: p,
            start: p,
        }
    }

    /// Straight line to `p`.
    pub fn line_to(mut self, p: P2) -> Self {
        if geom::dist(self.cur, p) > 1e-12 {
            self.segs.push(ProfileSeg::Line { a: self.cur, b: p });
        }
        self.cur = p;
        self
    }

    /// Arc to `p` about `center`.
    pub fn arc_to(mut self, p: P2, center: P2, ccw: bool) -> Self {
        self.segs.push(ProfileSeg::Arc {
            a: self.cur,
            b: p,
            center,
            ccw,
        });
        self.cur = p;
        self
    }

    /// Close back to the start and validate.
    pub fn close(self) -> Result<TurnProfile, TurnError> {
        let start = self.start;
        self.line_to(start).finish()
    }

    fn finish(self) -> Result<TurnProfile, TurnError> {
        TurnProfile::new(self.segs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stepped() -> TurnProfile {
        // Ø20 × 10 head, Ø12 × 15 body, Ø6 through bore.
        TurnProfile::from_points(&[
            [0.0, 3.0],
            [0.0, 6.0],
            [-15.0, 6.0],
            [-15.0, 10.0],
            [-25.0, 10.0],
            [-25.0, 3.0],
        ])
        .unwrap()
    }

    #[test]
    fn roles() {
        let p = stepped();
        let roles: Vec<SegRole> = p.classified().iter().map(|x| x.1).collect();
        assert!(roles.contains(&SegRole::RightFace));
        assert!(roles.contains(&SegRole::LeftFace));
        assert_eq!(roles.iter().filter(|r| **r == SegRole::Outer).count(), 3);
        assert_eq!(roles.iter().filter(|r| **r == SegRole::Inner).count(), 1);
        assert!(p.has_bore());
    }

    #[test]
    fn refuses_cavity() {
        // A ring with a hidden internal slot reached from nowhere.
        let r = TurnProfile::from_points(&[
            [0.0, 0.0],
            [0.0, 10.0],
            [-20.0, 10.0],
            [-20.0, 0.0],
            [-12.0, 0.0],
            [-12.0, 5.0],
            [-8.0, 5.0],
            [-8.0, 0.0],
        ]);
        assert!(r.is_ok(), "a groove open to the axis is one band per z");
        // An annular groove cut axially into the right face: two bands at z = -2.
        let cavity = TurnProfile::from_points(&[
            [0.0, 0.0],
            [0.0, 4.0],
            [-5.0, 4.0],
            [-5.0, 6.0],
            [0.0, 6.0],
            [0.0, 10.0],
            [-20.0, 10.0],
            [-20.0, 0.0],
        ]);
        assert!(matches!(cavity, Err(TurnError::NotTurnable(_))));
    }

    #[test]
    fn arc_polyline_sagitta() {
        let s = ProfileSeg::Arc {
            a: [1.0, 0.0],
            b: [0.0, 1.0],
            center: [0.0, 0.0],
            ccw: true,
        };
        let pl = s.polyline(1e-3);
        for w in pl.windows(2) {
            let mid = geom::scale(geom::add(w[0], w[1]), 0.5);
            assert!(1.0 - geom::norm(mid) <= 1e-3 + 1e-12);
        }
    }
}
