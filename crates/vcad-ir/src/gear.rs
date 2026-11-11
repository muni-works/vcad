//! Involute spur-gear profiles.
//!
//! # Why a true involute and not the box approximation
//!
//! Every rana gear to date is "tangential cubes on the root circle": a tooth
//! count of boxes unioned onto a pitch cylinder. That recipe exists because
//! the shapes were built by a mesh-append helper with no 2D profile step, so a
//! curved flank had nowhere to live. vcad has one: [`crate::CsgOp::Sketch2D`]
//! takes a closed profile and [`crate::CsgOp::Extrude`] turns it into a solid
//! wall-by-wall, with no boolean pass at all. A gear is therefore *cheaper* as
//! an exact involute polygon than as N unioned boxes — N booleans is what the
//! box recipe actually costs, and booleans are where the cracks came from.
//!
//! So: exact involute flanks, sampled at [`FLANK_SAMPLES`] points per flank.
//! The two documented approximations, both the same ones `rana/GEARS.md`
//! already accepts for its wire-EDM profiles:
//!
//! 1. **The root fillet is a circular arc, not the true trochoid** left by a
//!    hob. Below the base circle the flank continues as a radial line down to
//!    the root circle. For printed and EDM'd prototypes at these loads this is
//!    the normal simplification; a hobbed production gear gets the real
//!    fillet from the tool regardless of what the CAD said.
//! 2. **Tip and root arcs are polyline-sampled**, like every other curved
//!    surface in the tessellated pipeline.
//!
//! No profile shift, no tip relief, no crowning: `x = 0` full-depth ISO teeth
//! with a 20° pressure angle by default ([`GearSpec::pressure_angle_deg`]
//! overrides it — 14.5° and 25° are the other common standards). The solid is extruded from `z = 0` up the face
//! width, matching what `cylinder` and `prism` actually build.
//!
//! # Internal gears
//!
//! `internal = true` returns the *bore* profile — the material a ring gear
//! removes, not the ring itself. Subtract it from a tower or cover and what
//! is left has internal teeth. This is deliberate: the field failure being
//! fixed here is a brief that specified ring teeth tangent to the planet root
//! circle (0.0 clearance), which happens when the ring is modelled as its own
//! positive solid and then eyeballed against the planet. As a subtraction the
//! clearances fall out of the same standard proportions as the external case.

use std::f64::consts::PI;

use crate::{SketchSegment2D, Vec2};

/// Default pressure angle, radians (20°, ISO full-depth standard).
pub const PRESSURE_ANGLE: f64 = 20.0 * PI / 180.0;
/// Smallest pressure angle accepted, degrees.
pub const MIN_PRESSURE_ANGLE_DEG: f64 = 10.0;
/// Largest pressure angle accepted, degrees.
pub const MAX_PRESSURE_ANGLE_DEG: f64 = 35.0;
/// Addendum, in modules. Tip sits one module off the pitch circle.
pub const ADDENDUM: f64 = 1.0;
/// Dedendum, in modules. The extra 0.25 is the standard bottom clearance
/// that keeps a mating tip off this root — see [`GearDims::root_diameter`].
pub const DEDENDUM: f64 = 1.25;
/// Points sampled along each involute flank, by default.
///
/// This is the facet-density knob's default, not a fixed property of the
/// primitive: [`GearSpec::flank_samples`] overrides it, and
/// [`GearSpec::tip_samples`] and [`GearSpec::root_samples`] scale with it so
/// one number controls the whole profile. A gear shop that wants the flank
/// inside a tighter chordal tolerance raises it; a preview that wants a fast
/// tessellation lowers it.
pub const FLANK_SAMPLES: usize = 14;
/// Points sampled along each tip arc at [`FLANK_SAMPLES`] density.
pub const TIP_SAMPLES: usize = 5;
/// Points sampled along each root arc (the gap between two teeth) at
/// [`FLANK_SAMPLES`] density.
pub const ROOT_SAMPLES: usize = 7;
/// Fewest points that can describe a flank: its two ends.
pub const MIN_FLANK_SAMPLES: usize = 2;
/// Most points allowed on a flank.
///
/// A gear is `teeth * (2 * flank + tip + root)` points, and the sketch
/// extruder builds one wall quad per point. At 200 teeth this cap is already
/// a six-figure face count; past it the author wants a different
/// representation, not a denser polyline.
pub const MAX_FLANK_SAMPLES: usize = 512;

/// Involute function: `inv(a) = tan(a) - a`.
fn inv(a: f64) -> f64 {
    a.tan() - a
}

/// The analytic diameters of a gear. Every one of these is exact — no
/// tessellation enters here, which is what makes them assertable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GearDims {
    /// Module (mm of pitch diameter per tooth).
    pub module: f64,
    /// Tooth count.
    pub teeth: u32,
    /// True for a ring gear (teeth point inward).
    pub internal: bool,
    /// Pressure angle, radians.
    pub pressure_angle: f64,
}

impl GearDims {
    /// Pitch diameter `d = m z`.
    pub fn pitch_diameter(&self) -> f64 {
        self.module * self.teeth as f64
    }

    /// Base diameter `db = d cos(alpha)` — the circle the involute unwinds from.
    pub fn base_diameter(&self) -> f64 {
        self.pitch_diameter() * self.pressure_angle.cos()
    }

    /// Tip diameter. External teeth grow outward (`d + 2m`); internal teeth
    /// point inward, so the ring's tip circle is the *smaller* one (`d - 2m`).
    pub fn tip_diameter(&self) -> f64 {
        let a = 2.0 * ADDENDUM * self.module;
        if self.internal {
            self.pitch_diameter() - a
        } else {
            self.pitch_diameter() + a
        }
    }

    /// Root diameter. Mirrors [`Self::tip_diameter`]: `d -/+ 2.5m`.
    pub fn root_diameter(&self) -> f64 {
        let b = 2.0 * DEDENDUM * self.module;
        if self.internal {
            self.pitch_diameter() + b
        } else {
            self.pitch_diameter() - b
        }
    }
}

/// A gear to generate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GearSpec {
    /// Module, mm.
    pub module: f64,
    /// Tooth count (>= 4).
    pub teeth: u32,
    /// Face width — the extrusion depth, mm.
    pub face_width: f64,
    /// Circumferential tooth thinning at the pitch circle, mm. Each member of
    /// a mesh is thinned by its own `backlash`, so a pair thinned 0.02 each
    /// runs ~0.04 of circumferential backlash per mesh.
    pub backlash: f64,
    /// True for a ring gear — see the module docs, this yields the bore.
    pub internal: bool,
    /// Points sampled along each involute flank.
    ///
    /// The documented facet-density knob. `None` means [`FLANK_SAMPLES`].
    /// The flanks are exact involutes evaluated at these points and chorded
    /// between them, so this trades face count against how far the chord
    /// departs from the true curve — halving the departure costs roughly
    /// sqrt(2) times the points.
    pub flank_samples: Option<usize>,
    /// Pressure angle, degrees. `None` means 20° ([`PRESSURE_ANGLE`]).
    pub pressure_angle_deg: Option<f64>,
}

impl GearSpec {
    /// The pressure angle, radians.
    pub fn pressure_angle(&self) -> f64 {
        self.pressure_angle_deg
            .map(|d| d.to_radians())
            .unwrap_or(PRESSURE_ANGLE)
    }

    /// Points on each involute flank: the knob, or [`FLANK_SAMPLES`].
    pub fn flank_samples(&self) -> usize {
        self.flank_samples.unwrap_or(FLANK_SAMPLES)
    }

    /// Points on each tip arc, scaled from [`Self::flank_samples`] so the
    /// whole profile densifies together.
    pub fn tip_samples(&self) -> usize {
        scale_samples(self.flank_samples(), TIP_SAMPLES)
    }

    /// Points on each root arc, scaled from [`Self::flank_samples`].
    pub fn root_samples(&self) -> usize {
        scale_samples(self.flank_samples(), ROOT_SAMPLES)
    }

    /// The analytic diameters for this spec.
    pub fn dims(&self) -> GearDims {
        GearDims {
            module: self.module,
            teeth: self.teeth,
            internal: self.internal,
            pressure_angle: self.pressure_angle(),
        }
    }

    /// Validate. Returns a human-readable reason when the spec cannot make a
    /// gear — these are hard errors rather than silent clamps because a
    /// silently-degenerate gear is exactly the failure this primitive exists
    /// to stop (a variant once shipped with blank pitch cylinders).
    pub fn validate(&self) -> Result<(), String> {
        if self.module.is_nan() || self.module <= 0.0 {
            return Err(format!("gear module must be > 0, got {}", self.module));
        }
        if self.teeth < 4 {
            return Err(format!("gear needs at least 4 teeth, got {}", self.teeth));
        }
        if let Some(n) = self.flank_samples {
            if !(MIN_FLANK_SAMPLES..=MAX_FLANK_SAMPLES).contains(&n) {
                return Err(format!(
                    "gear flank samples must be {MIN_FLANK_SAMPLES}..={MAX_FLANK_SAMPLES}, got {n}"
                ));
            }
        }
        if self.face_width.is_nan() || self.face_width <= 0.0 {
            return Err(format!(
                "gear face width must be > 0, got {}",
                self.face_width
            ));
        }
        if let Some(a) = self.pressure_angle_deg {
            if !(MIN_PRESSURE_ANGLE_DEG..=MAX_PRESSURE_ANGLE_DEG).contains(&a) {
                return Err(format!(
                    "gear pressure angle must be {MIN_PRESSURE_ANGLE_DEG}..={MAX_PRESSURE_ANGLE_DEG}°, got {a}"
                ));
            }
        }
        if self.backlash < 0.0 {
            return Err(format!("gear backlash must be >= 0, got {}", self.backlash));
        }
        // Half the circular tooth thickness at the pitch circle. If backlash
        // eats the whole tooth there is nothing to cut.
        if self.pitch_half_angle() <= 0.0 {
            return Err(format!(
                "gear backlash {} removes the entire tooth (circular thickness {:.4})",
                self.backlash,
                PI * self.module / 2.0
            ));
        }
        let d = self.dims();
        if d.tip_diameter() <= 0.0 || d.root_diameter() <= 0.0 {
            return Err("gear proportions produce a non-positive diameter".to_string());
        }
        Ok(())
    }

    /// Angular half-thickness of the tooth at the pitch circle, radians.
    fn pitch_half_angle(&self) -> f64 {
        let thickness = PI * self.module / 2.0 - self.backlash;
        thickness / self.dims().pitch_diameter()
    }

    /// Angular half-thickness of the tooth at radius `r`, radians.
    ///
    /// The involute identity: rolling out from the pitch circle to `r` the
    /// tooth narrows by `inv(phi(r)) - inv(alpha)`. Same relation for external
    /// and internal teeth — internal flanks are involutes of the same base
    /// circle, they simply run inward from the root.
    fn half_angle_at(&self, r: f64) -> f64 {
        let rb = self.dims().base_diameter() / 2.0;
        let cos_phi = (rb / r).clamp(-1.0, 1.0);
        let phi = cos_phi.acos();
        self.pitch_half_angle() + inv(self.pressure_angle()) - inv(phi)
    }

    /// The closed tooth-profile polygon, counter-clockwise, centred on the
    /// origin. First point is not repeated at the end.
    pub fn profile(&self) -> Result<Vec<[f64; 2]>, String> {
        self.validate()?;
        let d = self.dims();
        let flank_samples = self.flank_samples();
        let tip_samples = self.tip_samples();
        let root_samples = self.root_samples();
        let rb = d.base_diameter() / 2.0;
        let r_tip = d.tip_diameter() / 2.0;
        let r_root = d.root_diameter() / 2.0;

        // The flank runs between the root and tip radii, but the involute
        // itself only exists at or outside the base circle. Where the root
        // dives under the base circle (any external gear below ~17 teeth,
        // which is every sun in this train) the profile continues as a radial
        // line at the base-circle angle — the documented arc-root
        // approximation standing in for the hob's trochoid.
        let (r_near, r_far) = if self.internal {
            (r_tip, r_root) // internal: tip is the inner radius
        } else {
            (r_root, r_tip)
        };
        let r_lo = r_near.max(rb).min(r_far);

        // Guard a pointed tooth: if the flanks cross before the tip circle
        // the profile self-intersects, which is unbuildable and silently ugly.
        let far_half = self.half_angle_at(r_far.max(rb));
        if far_half <= 0.0 {
            return Err(format!(
                "gear m{} z{} is pointed: the flanks meet below the tip circle \
                 (half-angle {far_half:.5} rad). Reduce backlash or raise the tooth count.",
                self.module, self.teeth
            ));
        }

        let tau = 2.0 * PI / self.teeth as f64;
        let mut pts: Vec<[f64; 2]> = Vec::new();
        let polar = |r: f64, a: f64| [r * a.cos(), r * a.sin()];

        for k in 0..self.teeth {
            let theta = tau * k as f64;

            // The angle the flank holds while it is under the base circle.
            let a_lo = self.half_angle_at(r_lo);

            // Radial run-out below the base circle, trailing side.
            if r_near < r_lo {
                pts.push(polar(r_near, theta - a_lo));
            }
            // Trailing flank, near -> far.
            for i in 0..flank_samples {
                let t = i as f64 / (flank_samples - 1) as f64;
                let r = r_lo + (r_far - r_lo) * t;
                pts.push(polar(r, theta - self.half_angle_at(r)));
            }
            // Tip arc.
            let a_far = self.half_angle_at(r_far);
            for i in 1..tip_samples {
                let t = i as f64 / tip_samples as f64;
                pts.push(polar(r_far, theta - a_far + 2.0 * a_far * t));
            }
            // Leading flank, far -> near.
            for i in 0..flank_samples {
                let t = i as f64 / (flank_samples - 1) as f64;
                let r = r_far + (r_lo - r_far) * t;
                pts.push(polar(r, theta + self.half_angle_at(r)));
            }
            if r_near < r_lo {
                pts.push(polar(r_near, theta + a_lo));
            }
            // Root arc across to the next tooth.
            let start = theta + a_lo;
            let end = theta + tau - a_lo;
            for i in 1..root_samples {
                let t = i as f64 / root_samples as f64;
                pts.push(polar(r_near, start + (end - start) * t));
            }
        }

        // Normalise winding to counter-clockwise (positive signed area). An
        // internal profile traversed this way comes out clockwise.
        let area: f64 = pts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let q = pts[(i + 1) % pts.len()];
                p[0] * q[1] - q[0] * p[1]
            })
            .sum();
        if area < 0.0 {
            pts.reverse();
        }
        Ok(pts)
    }

    /// The profile as closed sketch segments, ready for [`crate::CsgOp::Sketch2D`].
    pub fn sketch_segments(&self) -> Result<Vec<SketchSegment2D>, String> {
        let pts = self.profile()?;
        Ok((0..pts.len())
            .map(|i| {
                let a = pts[i];
                let b = pts[(i + 1) % pts.len()];
                SketchSegment2D::Line {
                    start: Vec2 { x: a[0], y: a[1] },
                    end: Vec2 { x: b[0], y: b[1] },
                }
            })
            .collect())
    }
}

/// Centre distance of an external spur pair at standard (zero-shift)
/// proportions: the pitch circles touch, `a = m (z1 + z2) / 2`.
pub fn center_distance(module: f64, z1: u32, z2: u32) -> f64 {
    module * (z1 + z2) as f64 / 2.0
}

/// The rotation (degrees, about the gear's own Z axis) that meshes a second
/// external gear with a first one: tooth into gap.
///
/// Both gears use this module's profile convention — tooth 0 centred on the
/// gear's local +X axis. `rot1_deg` is the first gear's rotation,
/// `direction_deg` the direction from the first gear's centre to the second's
/// (0 = +X). The pitch circles roll on each other at the contact point, so a
/// tooth of gear 1 sitting a fraction `f` of a pitch clockwise of the contact
/// point needs a gap of gear 2 the same arc length counter-clockwise (in gear
/// 2's own sense) of it. Result is normalised to `[0, 360)`.
pub fn mesh_rotation_deg(z1: u32, rot1_deg: f64, z2: u32, direction_deg: f64) -> f64 {
    let tau1 = 360.0 / z1 as f64;
    let tau2 = 360.0 / z2 as f64;
    let f = ((direction_deg - rot1_deg) / tau1).rem_euclid(1.0);
    (direction_deg + 180.0 + (f - 0.5) * tau2).rem_euclid(360.0)
}

/// Chords on a bore circle. The extruder would polygonise an arc with its
/// coarse default (a 2.5% area loss on a small bore); a gear's bore is a fit
/// surface, so it gets a fine, fixed polygon instead (0.07% area loss).
pub const BORE_SEGMENTS: usize = 96;

/// A circular bore of diameter `d` centred on the origin, as a closed
/// polygon of [`BORE_SEGMENTS`] chords — ready to go in a
/// [`crate::CsgOp::Sketch2D`]'s `holes`.
pub fn bore_segments(d: f64) -> Vec<SketchSegment2D> {
    let r = d / 2.0;
    let at = |i: usize| {
        let t = 2.0 * PI * i as f64 / BORE_SEGMENTS as f64;
        Vec2 {
            x: r * t.cos(),
            y: r * t.sin(),
        }
    };
    (0..BORE_SEGMENTS)
        .map(|i| SketchSegment2D::Line {
            start: at(i),
            end: at((i + 1) % BORE_SEGMENTS),
        })
        .collect()
}

/// Check a bore against a gear: it has to leave a rim inside the root circle.
pub fn validate_bore(spec: &GearSpec, bore_diameter: f64) -> Result<(), String> {
    if bore_diameter.is_nan() || bore_diameter < 0.0 {
        return Err(format!("gear bore must be >= 0, got {bore_diameter}"));
    }
    let root = spec.dims().root_diameter();
    if bore_diameter >= root - spec.module {
        return Err(format!(
            "gear bore {bore_diameter} leaves no rim inside the {root:.3} mm root circle \
             (keep it under {:.3})",
            root - spec.module
        ));
    }
    Ok(())
}

/// The [`GearSpec`] and bore diameter behind a [`crate::CsgOp::SpurGear`],
/// validated. Errors on any other op.
pub fn spur_gear_spec(op: &crate::CsgOp) -> Result<(GearSpec, f64), String> {
    let crate::CsgOp::SpurGear {
        module,
        teeth,
        pressure_angle_deg,
        face_width,
        bore_diameter,
        backlash,
    } = op
    else {
        return Err("not a SpurGear".to_string());
    };
    let spec = GearSpec {
        module: *module,
        teeth: *teeth,
        face_width: *face_width,
        backlash: *backlash,
        internal: false,
        flank_samples: None,
        pressure_angle_deg: Some(*pressure_angle_deg),
    };
    spec.validate()?;
    validate_bore(&spec, *bore_diameter)?;
    Ok((spec, *bore_diameter))
}

/// The outer profile and hole loops (the bore, when there is one) of a
/// [`crate::CsgOp::SpurGear`], ready to extrude by `face_width` along +Z.
#[allow(clippy::type_complexity)]
pub fn spur_gear_sketch(
    op: &crate::CsgOp,
) -> Result<(Vec<SketchSegment2D>, Vec<Vec<SketchSegment2D>>), String> {
    let (spec, bore) = spur_gear_spec(op)?;
    let holes = if bore > 0.0 {
        vec![bore_segments(bore)]
    } else {
        Vec::new()
    };
    Ok((spec.sketch_segments()?, holes))
}

/// Scale an arc's sample count with the flank density.
///
/// `at_default` is that arc's count at [`FLANK_SAMPLES`]; the result holds
/// the same ratio at any density, never dropping below the two points an arc
/// needs to exist.
fn scale_samples(flank: usize, at_default: usize) -> usize {
    let scaled = (flank * at_default).div_ceil(FLANK_SAMPLES);
    scaled.max(MIN_FLANK_SAMPLES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(module: f64, teeth: u32, internal: bool) -> GearSpec {
        GearSpec {
            module,
            teeth,
            face_width: 6.0,
            backlash: 0.0,
            internal,
            flank_samples: None,
            pressure_angle_deg: None,
        }
    }

    /// The same spec with the facet-density knob turned to `n`.
    fn spec_dense(module: f64, teeth: u32, internal: bool, n: usize) -> GearSpec {
        GearSpec {
            flank_samples: Some(n),
            ..spec(module, teeth, internal)
        }
    }

    /// The knob changes the point count and nothing else: a denser profile
    /// still spans exactly the analytic tip and root diameters, because the
    /// extra points are evaluations of the same involute rather than a
    /// different curve.
    #[test]
    fn flank_samples_densify_without_moving_the_profile() {
        let sparse = spec_dense(0.5, 20, false, MIN_FLANK_SAMPLES);
        let dense = spec_dense(0.5, 20, false, 60);

        let n_sparse = sparse.profile().expect("sparse profile").len();
        let n_dense = dense.profile().expect("dense profile").len();
        assert!(
            n_dense > n_sparse * 4,
            "60 points per flank should be far denser than {MIN_FLANK_SAMPLES}: \
             {n_dense} vs {n_sparse}",
        );

        let d = dense.dims();
        let radius = |p: &[f64; 2]| p[0].hypot(p[1]);
        for spec in [sparse, dense] {
            let pts = spec.profile().expect("profile");
            let max = pts.iter().map(radius).fold(f64::MIN, f64::max);
            let min = pts.iter().map(radius).fold(f64::MAX, f64::min);
            assert!(
                (max - d.tip_diameter() / 2.0).abs() < 1e-9,
                "tip radius moved at {:?} samples",
                spec.flank_samples,
            );
            assert!(
                (min - d.root_diameter() / 2.0).abs() < 1e-9,
                "root radius moved at {:?} samples",
                spec.flank_samples,
            );
        }
    }

    /// The tip and root arcs densify with the flanks rather than staying at
    /// their default counts, so one knob controls the whole profile.
    #[test]
    fn arc_samples_scale_with_flank_samples() {
        let default = spec(0.5, 20, false);
        assert_eq!(default.tip_samples(), TIP_SAMPLES);
        assert_eq!(default.root_samples(), ROOT_SAMPLES);

        let doubled = spec_dense(0.5, 20, false, FLANK_SAMPLES * 2);
        assert_eq!(doubled.tip_samples(), TIP_SAMPLES * 2);
        assert_eq!(doubled.root_samples(), ROOT_SAMPLES * 2);

        // An arc never collapses below its two endpoints.
        let minimal = spec_dense(0.5, 20, false, MIN_FLANK_SAMPLES);
        assert_eq!(minimal.tip_samples(), MIN_FLANK_SAMPLES);
        assert_eq!(minimal.root_samples(), MIN_FLANK_SAMPLES);
    }

    /// Out-of-range densities are hard errors, like every other bad gear
    /// field — a silently clamped facet count is how a gear ships with a
    /// profile nobody chose.
    #[test]
    fn out_of_range_flank_samples_are_rejected() {
        assert!(spec_dense(0.5, 20, false, 1).validate().is_err());
        assert!(spec_dense(0.5, 20, false, MAX_FLANK_SAMPLES + 1)
            .validate()
            .is_err());
        assert!(spec_dense(0.5, 20, false, MIN_FLANK_SAMPLES)
            .validate()
            .is_ok());
        assert!(spec_dense(0.5, 20, false, MAX_FLANK_SAMPLES)
            .validate()
            .is_ok());
    }

    #[test]
    fn analytic_diameters_match_iso_full_depth() {
        // The rana-60c train: sun 10T, planet 20T, ring 50T, all m0.5.
        let sun = spec(0.5, 10, false).dims();
        assert_eq!(sun.pitch_diameter(), 5.0);
        assert_eq!(sun.tip_diameter(), 6.0);
        assert_eq!(sun.root_diameter(), 3.75);

        let planet = spec(0.5, 20, false).dims();
        assert_eq!(planet.pitch_diameter(), 10.0);
        assert_eq!(planet.tip_diameter(), 11.0);
        assert_eq!(planet.root_diameter(), 8.75);

        let ring = spec(0.5, 50, true).dims();
        assert_eq!(ring.pitch_diameter(), 25.0);
        assert_eq!(ring.tip_diameter(), 24.0);
        assert_eq!(ring.root_diameter(), 26.25);
    }

    /// The generated profile must actually reach the analytic tip and root
    /// radii — the check that would have caught the blank-gear ship, where
    /// the "gear" was a pitch cylinder and max == min == pitch radius.
    #[test]
    fn profile_extremes_match_analytic_diameters() {
        for (m, z, internal) in [(0.5, 10, false), (0.5, 20, false), (0.5, 50, true)] {
            let s = spec(m, z, internal);
            let d = s.dims();
            let pts = s.profile().unwrap();
            let rs: Vec<f64> = pts.iter().map(|p| p[0].hypot(p[1])).collect();
            let rmax = rs.iter().cloned().fold(f64::MIN, f64::max);
            let rmin = rs.iter().cloned().fold(f64::MAX, f64::min);
            let (r_tip, r_root) = (d.tip_diameter() / 2.0, d.root_diameter() / 2.0);
            let (want_max, want_min) = if internal {
                (r_root, r_tip)
            } else {
                (r_tip, r_root)
            };
            assert!(
                (rmax - want_max).abs() < 1e-9,
                "m{m} z{z} internal={internal}: rmax {rmax} != {want_max}"
            );
            assert!(
                (rmin - want_min).abs() < 1e-9,
                "m{m} z{z} internal={internal}: rmin {rmin} != {want_min}"
            );
            // Not a blank: the profile must have real radial relief.
            assert!(rmax - rmin > 2.0 * m, "m{m} z{z}: profile has no teeth");
        }
    }

    #[test]
    fn profile_is_simple_and_ccw() {
        let pts = spec(0.5, 20, false).profile().unwrap();
        let area: f64 = (0..pts.len())
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum::<f64>()
            / 2.0;
        assert!(area > 0.0, "expected CCW winding, area {area}");
        // Angles must advance monotonically around the centre — a sufficient
        // condition for a star-shaped (hence simple) polygon.
        let mut prev = pts[0][1].atan2(pts[0][0]);
        let mut wound = 0.0;
        // Walk every edge including the closing one back to pts[0], or the
        // total falls one root-arc step short of a full turn.
        for p in pts.iter().skip(1).chain(std::iter::once(&pts[0])) {
            let a = p[1].atan2(p[0]);
            let mut step = a - prev;
            while step > PI {
                step -= 2.0 * PI;
            }
            while step < -PI {
                step += 2.0 * PI;
            }
            assert!(step >= -1e-12, "profile doubles back: step {step}");
            wound += step;
            prev = a;
        }
        assert!((wound.abs() - 2.0 * PI).abs() < 1e-6, "wound {wound}");
    }

    /// Centre-distance clearance for the 60c train, measured off the
    /// generated profiles rather than the formula: at cd = 7.5 every tip must
    /// stay at least 0.1 mm off the mating root.
    #[test]
    fn mesh_clearance_at_center_distance_7p5() {
        const CD: f64 = 7.5;
        let r = |s: GearSpec| {
            let pts = s.profile().unwrap();
            let rs: Vec<f64> = pts.iter().map(|p| p[0].hypot(p[1])).collect();
            (
                rs.iter().cloned().fold(f64::MAX, f64::min),
                rs.iter().cloned().fold(f64::MIN, f64::max),
            )
        };
        let (sun_root, sun_tip) = r(spec(0.5, 10, false));
        let (planet_root, planet_tip) = r(spec(0.5, 20, false));
        // Internal ring: min radius is the tip (pointing inward), max the root.
        let (ring_tip, ring_root) = r(spec(0.5, 50, true));

        // sun-planet: external/external, centres CD apart.
        let c1 = CD - sun_tip - planet_root;
        let c2 = CD - planet_tip - sun_root;
        // planet-ring: an internal mesh, centres CD apart. Measured from the
        // ring's centre the planet's root circle reaches out to CD + r_root
        // and its tip circle to CD + r_tip, so both clearances are radial
        // differences in the same direction — the ring's own circles sit
        // outside the planet's.
        let c3 = ring_root - (CD + planet_tip);
        let c4 = ring_tip - (CD + planet_root);

        for (name, c) in [
            ("sun tip / planet root", c1),
            ("planet tip / sun root", c2),
            ("planet tip / ring root", c3),
            ("ring tip / planet root", c4),
        ] {
            assert!(c >= 0.1, "{name}: clearance {c:.4} < 0.1 at cd {CD}");
            // 0.25 module is the standard bottom clearance; anything much
            // larger means the proportions drifted.
            assert!(c < 0.2, "{name}: clearance {c:.4} unexpectedly large");
        }
    }

    #[test]
    fn backlash_thins_the_tooth() {
        let thick = spec(0.5, 20, false);
        let mut thin = thick;
        thin.backlash = 0.02;
        // Half-angle at the pitch circle drops by backlash / d.
        let delta = thick.pitch_half_angle() - thin.pitch_half_angle();
        assert!((delta - 0.02 / 10.0).abs() < 1e-12, "delta {delta}");
        // Tip and root are unaffected by backlash.
        assert_eq!(thick.dims().tip_diameter(), thin.dims().tip_diameter());
    }

    #[test]
    fn degenerate_specs_are_rejected() {
        assert!(spec(0.0, 20, false).validate().is_err());
        assert!(spec(0.5, 3, false).validate().is_err());
        let mut fw = spec(0.5, 20, false);
        fw.face_width = 0.0;
        assert!(fw.validate().is_err());
        let mut bl = spec(0.5, 20, false);
        bl.backlash = 5.0;
        assert!(bl.validate().is_err());
    }

    #[test]
    fn sketch_segments_form_a_closed_loop() {
        let segs = spec(0.5, 10, false).sketch_segments().unwrap();
        assert_eq!(segs.len(), spec(0.5, 10, false).profile().unwrap().len());
        let ends: Vec<(Vec2, Vec2)> = segs
            .iter()
            .map(|s| match s {
                SketchSegment2D::Line { start, end } => (*start, *end),
                _ => panic!("expected lines"),
            })
            .collect();
        for i in 0..ends.len() {
            let next = ends[(i + 1) % ends.len()];
            assert!((ends[i].1.x - next.0.x).abs() < 1e-12);
            assert!((ends[i].1.y - next.0.y).abs() < 1e-12);
        }
    }

    // ---- involute correctness and meshing (spur-gear tool) ----

    fn spec_pa(module: f64, teeth: u32, pa: f64) -> GearSpec {
        GearSpec {
            pressure_angle_deg: Some(pa),
            ..spec(module, teeth, false)
        }
    }

    /// Angular width (radians) of tooth 0 where the profile crosses radius
    /// `r`, measured off the polygon itself: the two crossings nearest +X.
    fn tooth_angle_at(pts: &[[f64; 2]], r: f64, tau: f64) -> f64 {
        let mut hits: Vec<f64> = Vec::new();
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            let (ra, rb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
            if (ra - r) * (rb - r) <= 0.0 && (ra - rb).abs() > 1e-15 {
                let t = (r - ra) / (rb - ra);
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                let ang = p[1].atan2(p[0]);
                if ang.abs() < tau / 2.0 {
                    hits.push(ang);
                }
            }
        }
        assert_eq!(
            hits.len(),
            2,
            "expected 2 flank crossings at r={r}, got {hits:?}"
        );
        (hits[0] - hits[1]).abs()
    }

    /// Tooth thickness on the pitch circle is half the circular pitch,
    /// `s = pi m / 2`, at every pressure angle — that is what makes two
    /// zero-backlash gears of the same module mesh.
    #[test]
    fn tooth_thickness_at_pitch_circle_is_half_the_circular_pitch() {
        for (m, z, pa) in [(2.0, 20, 20.0), (1.0, 31, 14.5), (1.5, 24, 25.0)] {
            let sp = GearSpec {
                flank_samples: Some(200),
                ..spec_pa(m, z, pa)
            };
            let pts = sp.profile().unwrap();
            let rp = sp.dims().pitch_diameter() / 2.0;
            let tau = 2.0 * PI / z as f64;
            let s = tooth_angle_at(&pts, rp, tau) * rp; // arc length
            let want = PI * m / 2.0;
            assert!(
                (s - want).abs() < 2e-3 * m,
                "m{m} z{z} {pa}°: pitch thickness {s:.5}, want {want:.5}"
            );
        }
    }

    /// The flank really is an involute of the base circle `rb = rp cos a`:
    /// every flank point between the base and tip circles sits at the polar
    /// angle `half(rp) + inv(a) - inv(acos(rb / r))` off the tooth axis.
    #[test]
    fn flank_points_lie_on_the_involute_of_the_base_circle() {
        for pa in [14.5f64, 20.0, 25.0] {
            let sp = spec_pa(2.0, 30, pa);
            let d = sp.dims();
            let rb = d.base_diameter() / 2.0;
            let rp = d.pitch_diameter() / 2.0;
            assert!((rb - rp * pa.to_radians().cos()).abs() < 1e-12);
            let half_p = (PI * sp.module / 2.0) / d.pitch_diameter();
            let tau = 2.0 * PI / sp.teeth as f64;
            let r_tip = d.tip_diameter() / 2.0;
            let r_lo = rb.max(d.root_diameter() / 2.0);
            let mut checked = 0;
            for p in sp.profile().unwrap() {
                let r = p[0].hypot(p[1]);
                if r < r_lo + 1e-9 || r > r_tip - 1e-9 {
                    continue;
                }
                let ang = p[1].atan2(p[0]);
                // Fold into tooth 0's frame.
                let local = (ang + tau / 2.0).rem_euclid(tau) - tau / 2.0;
                let want = half_p + inv(pa.to_radians()) - inv((rb / r).acos());
                assert!(
                    (local.abs() - want).abs() < 1e-9,
                    "{pa}°: r {r:.4} at {local:.6}, involute says ±{want:.6}"
                );
                checked += 1;
            }
            assert!(checked > 100, "only {checked} flank points checked");
        }
    }

    #[test]
    fn pressure_angle_out_of_range_is_rejected() {
        assert!(spec_pa(1.0, 20, 5.0).validate().is_err());
        assert!(spec_pa(1.0, 20, 40.0).validate().is_err());
        assert!(spec_pa(1.0, 20, 14.5).validate().is_ok());
    }

    #[test]
    fn bore_must_leave_a_rim() {
        let sp = spec(2.0, 20, false); // root 35
        assert!(validate_bore(&sp, 0.0).is_ok());
        assert!(validate_bore(&sp, 10.0).is_ok());
        assert!(validate_bore(&sp, 34.0).is_err());
        assert!(validate_bore(&sp, -1.0).is_err());
    }

    fn place(pts: &[[f64; 2]], rot: f64, dx: f64) -> Vec<[f64; 2]> {
        let (s, c) = rot.sin_cos();
        pts.iter()
            .map(|p| [c * p[0] - s * p[1] + dx, s * p[0] + c * p[1]])
            .collect()
    }

    fn inside(poly: &[[f64; 2]], q: [f64; 2]) -> bool {
        let mut w = false;
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            if (a[1] > q[1]) != (b[1] > q[1])
                && q[0] < a[0] + (q[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1])
            {
                w = !w;
            }
        }
        w
    }

    fn seg_dist(q: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
        let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
        let l2 = vx * vx + vy * vy;
        let t = if l2 > 0.0 {
            (((q[0] - a[0]) * vx + (q[1] - a[1]) * vy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (q[0] - a[0] - t * vx).hypot(q[1] - a[1] - t * vy)
    }

    /// Signed clearance between two placed profiles near the mesh point:
    /// negative when a vertex of one lies inside the other.
    fn clearance(a: &[[f64; 2]], b: &[[f64; 2]], band: (f64, f64)) -> f64 {
        let near = |p: &[f64; 2]| p[0] > band.0 && p[0] < band.1;
        let mut min = f64::MAX;
        for (p, q) in [(a, b), (b, a)] {
            let edges: Vec<usize> = (0..q.len()).filter(|&i| near(&q[i])).collect();
            for v in p.iter().filter(|v| near(v)) {
                let d = edges
                    .iter()
                    .map(|&i| seg_dist(*v, q[i], q[(i + 1) % q.len()]))
                    .fold(f64::MAX, f64::min);
                let d = if inside(q, *v) { -d } else { d };
                min = min.min(d);
            }
        }
        min
    }

    /// Two zero-backlash gears at `a = m (z1 + z2) / 2`, phased by
    /// [`mesh_rotation_deg`], clear each other through a full tooth pitch of
    /// rotation and run with only a small gap (chordal sag of the sampled
    /// flanks). Pulled 0.1 module closer they interfere; phased half a pitch
    /// wrong (tooth on tooth) they interfere too.
    #[test]
    fn meshing_pair_clears_at_center_distance_and_interferes_closer() {
        for (m, z1, z2, pa) in [
            (2.0, 20, 30, 20.0),
            (1.0, 24, 40, 25.0),
            (1.5, 18, 18, 20.0),
        ] {
            let g1 = spec_pa(m, z1, pa).profile().unwrap();
            let g2 = spec_pa(m, z2, pa).profile().unwrap();
            let a = center_distance(m, z1, z2);
            let r1 = spec_pa(m, z1, pa).dims().tip_diameter() / 2.0;
            let r2 = spec_pa(m, z2, pa).dims().tip_diameter() / 2.0;
            let band = (a - r2 - m, r1 + m);
            let tau1 = 360.0 / z1 as f64;
            let mut worst = f64::MAX;
            let mut loosest: f64 = 0.0;
            for k in 0..12 {
                let rot1 = tau1 * k as f64 / 12.0;
                let rot2 = mesh_rotation_deg(z1, rot1, z2, 0.0);
                let p1 = place(&g1, rot1.to_radians(), 0.0);
                let p2 = place(&g2, rot2.to_radians(), a);
                let c = clearance(&p1, &p2, band);
                worst = worst.min(c);
                loosest = loosest.max(c);

                if k == 0 {
                    let closer = place(&g2, rot2.to_radians(), a - 0.1 * m);
                    let cc = clearance(&p1, &closer, band);
                    assert!(
                        cc < 0.0,
                        "m{m} {z1}/{z2}: 0.1m closer still clears ({cc:.5})"
                    );
                    let wrong = place(&g2, (rot2 + 180.0 / z2 as f64).to_radians(), a);
                    let cw = clearance(&p1, &wrong, band);
                    assert!(cw < 0.0, "m{m} {z1}/{z2}: tooth-on-tooth clears ({cw:.5})");
                }
            }
            assert!(
                worst >= 0.0,
                "m{m} {z1}/{z2} {pa}°: interference {worst:.6}"
            );
            assert!(
                loosest < 0.02 * m,
                "m{m} {z1}/{z2} {pa}°: gap {loosest:.5} is not a mesh"
            );
        }
    }

    /// The phase rule in a direction other than +X, and with gear 1 already
    /// turned, still lands tooth into gap.
    #[test]
    fn mesh_rotation_works_in_any_direction() {
        let (m, z1, z2) = (1.0, 20u32, 33u32);
        let g1 = spec(m, z1, false).profile().unwrap();
        let g2 = spec(m, z2, false).profile().unwrap();
        let a = center_distance(m, z1, z2);
        for (rot1, dir) in [(7.0, 90.0), (3.3, 217.0), (0.0, -45.0)] {
            let rot2 = mesh_rotation_deg(z1, rot1, z2, dir);
            // Rotate the whole scene by -dir so the pair lies along +X.
            let p1 = place(&g1, (rot1 - dir).to_radians(), 0.0);
            let p2 = place(&g2, (rot2 - dir).to_radians(), a);
            let r1 = spec(m, z1, false).dims().tip_diameter() / 2.0;
            let r2 = spec(m, z2, false).dims().tip_diameter() / 2.0;
            let c = clearance(&p1, &p2, (a - r2 - m, r1 + m));
            assert!(
                (0.0..0.02).contains(&c),
                "rot1 {rot1} dir {dir}: clearance {c:.5}"
            );
        }
    }

    /// Profile area against the closed form: the full-depth tooth band is
    /// close to half material, so `A ~ pi r_root^2 + pi (r_tip^2 - r_root^2) / 2`.
    #[test]
    fn profile_area_matches_closed_form_estimate() {
        for (m, z) in [(1.0, 20), (2.0, 40), (0.5, 60)] {
            let sp = spec(m, z, false);
            let pts = sp.profile().unwrap();
            let area: f64 = (0..pts.len())
                .map(|i| {
                    let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
                    p[0] * q[1] - q[0] * p[1]
                })
                .sum::<f64>()
                / 2.0;
            let d = sp.dims();
            let (rt, rr) = (d.tip_diameter() / 2.0, d.root_diameter() / 2.0);
            let est = PI * rr * rr + PI * (rt * rt - rr * rr) / 2.0;
            let rel = (area - est).abs() / est;
            assert!(
                rel < 0.03,
                "m{m} z{z}: area {area:.3} vs est {est:.3} ({rel:.4})"
            );
        }
    }
}
