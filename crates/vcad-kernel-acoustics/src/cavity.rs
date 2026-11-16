//! Axisymmetric acoustic cavities as a coaxial stack of cylinders.
//!
//! Geometry is in **millimeters** (vcad convention); the medium carries SI
//! sound speed and density. A [`Cavity`] is a body of revolution about the
//! z axis built from contiguous coaxial [`Segment`]s stacked along +z. That
//! one primitive spans the M0 catalogue:
//!
//! - a **closed cylinder** — one segment, all walls rigid (axial-mode
//!   oracle `fₙ = n·c/2L`);
//! - a **Helmholtz resonator** — a wide cavity segment plus a narrow neck
//!   segment, neck mouth open (pressure-release);
//! - a **ported box** (bass-reflex loudspeaker enclosure) — a box segment
//!   plus a port segment, port mouth open, a driver piston on the far face.
//!
//! Rigid annular **obstacles** ([`Obstacle`]) can be carved out of the
//! segment union: the wall of a port tube that hangs *inside* the box
//! (an internal port — the common real-world layout), a baffle ring, or a
//! central post. Fluid on either side of an obstacle only communicates
//! around its ends, which is exactly how an internal port's air column
//! couples to the annular box air around it.
//!
//! Walls are rigid (Neumann) by default. Two faces can be reassigned: the
//! **top** (+z, far end of the last segment) and **bottom** (−z, near end of
//! the first segment) each carry an [`EndCondition`] — rigid, an open
//! pressure-release mouth, or a driven piston over a disk.

use crate::medium::Medium;

/// One coaxial cylinder segment. `z1 > z0`; the segment occupies
/// `r ∈ [0, radius]`, `z ∈ [z0, z1]` (millimeters).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Lower z bound, mm.
    pub z0_mm: f64,
    /// Upper z bound, mm.
    pub z1_mm: f64,
    /// Segment radius, mm.
    pub radius_mm: f64,
}

impl Segment {
    /// Length `z1 − z0`, mm.
    #[inline]
    pub fn length_mm(&self) -> f64 {
        self.z1_mm - self.z0_mm
    }

    /// Cross-sectional area `π r²`, mm².
    #[inline]
    pub fn area_mm2(&self) -> f64 {
        std::f64::consts::PI * self.radius_mm * self.radius_mm
    }

    /// Enclosed volume `π r² L`, mm³.
    #[inline]
    pub fn volume_mm3(&self) -> f64 {
        self.area_mm2() * self.length_mm()
    }
}

/// A rigid annular solid carved out of the segment union: occupies
/// `r ∈ (r_in, r_out)`, `z ∈ [z0, z1]` (millimeters). `r_in = 0` is a solid
/// central post. The canonical use is the wall of an **internal port** — a
/// tube of bore `r_in` and outer radius `r_out` hanging inside the box.
///
/// Discretisation (vertex-centred grid): a node is solid when `r_in < r <
/// r_out` (strict; the axis node counts for `r_in = 0`) and `z0 ≤ z ≤ z1`
/// (inclusive). The strict radial test gives the obstacle the same `±dr/2`
/// face convention a segment edge already has; the inclusive axial test
/// closes the wall flush against a box end wall it touches. The grid must
/// put at least one node strictly inside `(r_in, r_out)` or the wall would
/// leak — [`crate::helmholtz::solve_driven`] refuses such a grid
/// ([`crate::helmholtz::SolveError::ObstacleUnresolved`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Obstacle {
    /// Inner radius, mm (`0` = solid post on the axis).
    pub r_in_mm: f64,
    /// Outer radius, mm.
    pub r_out_mm: f64,
    /// Lower z bound, mm.
    pub z0_mm: f64,
    /// Upper z bound, mm.
    pub z1_mm: f64,
}

impl Obstacle {
    /// True when the point `(r, z)` (mm) is inside the solid, under the
    /// discretisation rule in the type docs.
    pub fn contains(&self, r_mm: f64, z_mm: f64) -> bool {
        let eps = 1e-9;
        let radial_in = if self.r_in_mm <= eps {
            r_mm < self.r_out_mm - eps
        } else {
            r_mm > self.r_in_mm + eps && r_mm < self.r_out_mm - eps
        };
        radial_in && z_mm >= self.z0_mm - eps && z_mm <= self.z1_mm + eps
    }

    /// Solid volume `π(r_out² − r_in²)(z1 − z0)`, mm³.
    pub fn volume_mm3(&self) -> f64 {
        std::f64::consts::PI
            * (self.r_out_mm * self.r_out_mm - self.r_in_mm * self.r_in_mm)
            * (self.z1_mm - self.z0_mm)
    }
}

/// What happens at an end face (±z) of the stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EndCondition {
    /// Rigid wall — zero normal velocity (Neumann). The default.
    Rigid,
    /// Open mouth — pressure-release (`p = 0`) over the end-segment disk.
    /// The crude free-space termination: it omits the exterior radiation
    /// mass (the end correction), so a field-solved resonance lands slightly
    /// **above** the fully end-corrected lumped value — see the M0 doc.
    Open,
    /// A rigid piston of radius `radius_mm` driven at unit normal velocity
    /// over the end-segment disk (the loudspeaker cone). The drive amplitude
    /// and phase are applied at solve time.
    Piston {
        /// Piston (driver) radius, mm.
        radius_mm: f64,
    },
    /// A locally-reacting impedance termination over the end-segment disk:
    /// `∂p/∂n = −jk·β·p` with `β` the **normalized admittance** `ρc/Z`. The
    /// span from rigid to release: `β = 0` is rigid, `β = 1` is a matched
    /// (anechoic) termination that absorbs a normally-incident wave with no
    /// reflection, larger `β` tends toward pressure-release. The imaginary
    /// diagonal term it adds regularizes the otherwise-singular resonance —
    /// this is how M1's absorptive materials and radiating mouth will enter.
    Impedance {
        /// Normalized admittance `β = ρc/Z` (dimensionless).
        admittance: f64,
    },
}

/// A complete axisymmetric acoustic cavity.
#[derive(Debug, Clone, PartialEq)]
pub struct Cavity {
    /// Coaxial segments, stacked along +z (contiguous: each `z1` equals the
    /// next `z0`), first segment based at `z0 = 0`.
    pub segments: Vec<Segment>,
    /// Rigid annular solids carved out of the segments (internal port walls,
    /// baffle rings). Empty for the M0 catalogue shapes. Each must lie
    /// within the segment union and not overlap another.
    pub obstacles: Vec<Obstacle>,
    /// Condition at the near (−z) end of the first segment.
    pub bottom: EndCondition,
    /// Condition at the far (+z) end of the last segment.
    pub top: EndCondition,
    /// The acoustic medium.
    pub medium: Medium,
}

impl Cavity {
    /// A rigid closed cylinder of radius `radius_mm`, height `height_mm`.
    /// The clean axial-mode oracle: `fₙ = n·c/2L`.
    pub fn closed_cylinder(radius_mm: f64, height_mm: f64, medium: Medium) -> Self {
        Self {
            segments: vec![Segment {
                z0_mm: 0.0,
                z1_mm: height_mm,
                radius_mm,
            }],
            obstacles: Vec::new(),
            bottom: EndCondition::Rigid,
            top: EndCondition::Rigid,
            medium,
        }
    }

    /// A Helmholtz resonator: a rigid cavity (`cavity_radius_mm` ×
    /// `cavity_height_mm`) with a coaxial neck (`neck_radius_mm` ×
    /// `neck_length_mm`) whose mouth is open to the atmosphere.
    pub fn helmholtz_resonator(
        cavity_radius_mm: f64,
        cavity_height_mm: f64,
        neck_radius_mm: f64,
        neck_length_mm: f64,
        medium: Medium,
    ) -> Self {
        Self {
            segments: vec![
                Segment {
                    z0_mm: 0.0,
                    z1_mm: cavity_height_mm,
                    radius_mm: cavity_radius_mm,
                },
                Segment {
                    z0_mm: cavity_height_mm,
                    z1_mm: cavity_height_mm + neck_length_mm,
                    radius_mm: neck_radius_mm,
                },
            ],
            obstacles: Vec::new(),
            bottom: EndCondition::Rigid,
            top: EndCondition::Open,
            medium,
        }
    }

    /// A ported (bass-reflex) loudspeaker enclosure: a rigid box
    /// (`box_radius_mm` × `box_height_mm`) with a coaxial port
    /// (`port_radius_mm` × `port_length_mm`) venting out the top, driven by
    /// a piston of radius `driver_radius_mm` on the bottom face.
    pub fn ported_box(
        box_radius_mm: f64,
        box_height_mm: f64,
        port_radius_mm: f64,
        port_length_mm: f64,
        driver_radius_mm: f64,
        medium: Medium,
    ) -> Self {
        Self {
            segments: vec![
                Segment {
                    z0_mm: 0.0,
                    z1_mm: box_height_mm,
                    radius_mm: box_radius_mm,
                },
                Segment {
                    z0_mm: box_height_mm,
                    z1_mm: box_height_mm + port_length_mm,
                    radius_mm: port_radius_mm,
                },
            ],
            obstacles: Vec::new(),
            bottom: EndCondition::Piston {
                radius_mm: driver_radius_mm,
            },
            top: EndCondition::Open,
            medium,
        }
    }

    /// Largest segment radius, mm (the grid's radial extent).
    pub fn r_max_mm(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| s.radius_mm)
            .fold(0.0, f64::max)
    }

    /// Total axial extent `[z_min, z_max]`, mm.
    pub fn z_span_mm(&self) -> (f64, f64) {
        let zmin = self
            .segments
            .iter()
            .map(|s| s.z0_mm)
            .fold(f64::INFINITY, f64::min);
        let zmax = self
            .segments
            .iter()
            .map(|s| s.z1_mm)
            .fold(f64::NEG_INFINITY, f64::max);
        (zmin, zmax)
    }

    /// Total fluid volume, mm³: the segments (disjoint in z, so their sum)
    /// minus the obstacles carved out of them. For the M0 catalogue shapes
    /// (no obstacles) this is the compliance volume `V` in the lumped
    /// resonator law; with an internal port the port's own air column is
    /// inside it too, so the lumped `V_box` is this minus the port bore.
    pub fn volume_mm3(&self) -> f64 {
        self.segments.iter().map(|s| s.volume_mm3()).sum::<f64>()
            - self.obstacles.iter().map(|o| o.volume_mm3()).sum::<f64>()
    }

    /// True when the point `(r, z)` (mm) lies inside the fluid: inside some
    /// segment and not inside any obstacle.
    pub fn contains(&self, r_mm: f64, z_mm: f64) -> bool {
        let eps = 1e-9;
        self.segments
            .iter()
            .any(|s| r_mm <= s.radius_mm + eps && z_mm >= s.z0_mm - eps && z_mm <= s.z1_mm + eps)
            && !self.obstacles.iter().any(|o| o.contains(r_mm, z_mm))
    }

    /// A ported box whose port **hangs inside** the box (an internal port):
    /// a rigid box (`box_radius_mm` × `box_height_mm`), a top wall of
    /// `top_wall_mm`, and a port tube of bore `port_radius_mm`, wall
    /// `port_wall_mm` and total length `port_length_mm` measured from its
    /// inner (hanging) end to the open mouth flush with the outside of the
    /// top wall. Driven by a piston of radius `driver_radius_mm` on the
    /// bottom face.
    ///
    /// The tube wall is an [`Obstacle`]; the bore is an air column that
    /// couples to the box air only at its hanging end. The top wall is a
    /// short port-radius segment above the box.
    #[allow(clippy::too_many_arguments)]
    pub fn ported_box_internal(
        box_radius_mm: f64,
        box_height_mm: f64,
        top_wall_mm: f64,
        port_radius_mm: f64,
        port_wall_mm: f64,
        port_length_mm: f64,
        driver_radius_mm: f64,
        medium: Medium,
    ) -> Self {
        let top = box_height_mm + top_wall_mm;
        Self {
            segments: vec![
                Segment {
                    z0_mm: 0.0,
                    z1_mm: box_height_mm,
                    radius_mm: box_radius_mm,
                },
                Segment {
                    z0_mm: box_height_mm,
                    z1_mm: top,
                    radius_mm: port_radius_mm,
                },
            ],
            obstacles: vec![Obstacle {
                r_in_mm: port_radius_mm,
                r_out_mm: port_radius_mm + port_wall_mm,
                z0_mm: top - port_length_mm,
                z1_mm: box_height_mm,
            }],
            bottom: EndCondition::Piston {
                radius_mm: driver_radius_mm,
            },
            top: EndCondition::Open,
            medium,
        }
    }

    /// The last (outermost, +z) segment — the neck/port for resonators and
    /// ported boxes.
    pub fn port_segment(&self) -> &Segment {
        self.segments
            .last()
            .expect("cavity has at least one segment")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_cylinder_volume_and_span() {
        let cav = Cavity::closed_cylinder(50.0, 200.0, Medium::air(20.0));
        assert_eq!(cav.z_span_mm(), (0.0, 200.0));
        assert!((cav.volume_mm3() - std::f64::consts::PI * 2500.0 * 200.0).abs() < 1e-6);
        assert_eq!(cav.top, EndCondition::Rigid);
    }

    #[test]
    fn resonator_stacks_cavity_then_neck() {
        let cav = Cavity::helmholtz_resonator(60.0, 80.0, 10.0, 20.0, Medium::air(20.0));
        assert_eq!(cav.segments.len(), 2);
        assert_eq!(cav.r_max_mm(), 60.0);
        assert_eq!(cav.z_span_mm(), (0.0, 100.0));
        assert_eq!(cav.port_segment().radius_mm, 10.0);
        assert_eq!(cav.top, EndCondition::Open);
        // A point in the neck but outside the cavity radius is fluid;
        // the annular shoulder above the cavity (outside the neck) is not.
        assert!(cav.contains(5.0, 90.0));
        assert!(!cav.contains(30.0, 90.0));
        assert!(cav.contains(30.0, 40.0));
    }

    #[test]
    fn internal_port_wall_separates_bore_from_box_air() {
        // 52 mm box, 114 mm tall, 3 mm top wall, 12 mm bore, 2 mm wall,
        // 60 mm port (the puck).
        let cav =
            Cavity::ported_box_internal(52.0, 114.0, 3.0, 12.0, 2.0, 60.0, 32.0, Medium::air(20.0));
        // Bore and annular box air are fluid, the tube wall is not.
        assert!(cav.contains(6.0, 90.0));
        assert!(cav.contains(30.0, 90.0));
        assert!(!cav.contains(13.0, 90.0));
        // The wall closes flush against the top wall (inclusive z) …
        assert!(!cav.contains(13.0, 114.0));
        // … and is open below its hanging end.
        assert!(cav.contains(13.0, 56.0));
        // The top wall passes only the bore.
        assert!(cav.contains(6.0, 116.0));
        assert!(!cav.contains(20.0, 116.0));
        let v =
            std::f64::consts::PI * (52.0f64.powi(2) * 114.0 + 144.0 * 3.0 - (196.0 - 144.0) * 57.0);
        assert!((cav.volume_mm3() - v).abs() < 1e-6);
    }

    #[test]
    fn central_post_obstacle_includes_the_axis() {
        let o = Obstacle {
            r_in_mm: 0.0,
            r_out_mm: 5.0,
            z0_mm: 10.0,
            z1_mm: 20.0,
        };
        assert!(o.contains(0.0, 15.0));
        assert!(o.contains(4.0, 15.0));
        assert!(!o.contains(5.0, 15.0));
        assert!(!o.contains(1.0, 25.0));
    }

    #[test]
    fn ported_box_has_driver_and_open_port() {
        let cav = Cavity::ported_box(90.0, 250.0, 25.0, 60.0, 70.0, Medium::air(20.0));
        assert_eq!(cav.bottom, EndCondition::Piston { radius_mm: 70.0 });
        assert_eq!(cav.top, EndCondition::Open);
        assert!((cav.z_span_mm().1 - 310.0).abs() < 1e-9);
    }
}
