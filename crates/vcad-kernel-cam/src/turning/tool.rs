//! Lathe tools: indexable inserts, grooving/parting blades, drills.
//!
//! Every tool is modelled in the turning plane as a convex **core** polygon
//! (relative to the nose-centre `C`) grown by the nose radius `rn`:
//! `T(C) = (C + core) ⊕ disk(rn)`. For an insert the core is the wedge
//! between its two flanks, so the flank and relief geometry — not just the
//! nose circle — takes part in every gouge check.
//!
//! **Programmed point.** Grbl has no G41/G42 and this crate does not rely on
//! LinuxCNC's either, so tool-nose-radius compensation happens here: the
//! program carries the *imaginary tool tip* — the point you touch off on a
//! face (Z) and a diameter (X) — which is `C + tip_sign · rn`. This is the
//! one place that convention lives; [`LatheTool::programmed_offset`] and
//! [`LatheTool::nose_center`] are its only readers.

use super::geom::{self, P2};
use super::TurnError;
use serde::{Deserialize, Serialize};

/// What a lathe tool is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatheToolKind {
    /// External turning / facing insert.
    Turning,
    /// Internal boring bar.
    Boring,
    /// Grooving blade.
    Grooving,
    /// Parting (cut-off) blade.
    Parting,
    /// Twist drill on the spindle axis (tailstock or turret).
    Drill,
}

/// A lathe tool in the turning plane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatheTool {
    /// Tool number (T word).
    pub number: u32,
    /// Human-readable description.
    pub name: String,
    /// What the tool is for.
    pub kind: LatheToolKind,
    /// Nose (corner) radius, mm.
    pub nose_radius: f64,
    /// Convex core polygon relative to the nose centre (CCW).
    pub core: Vec<P2>,
    /// Imaginary-tip direction: programmed point = `C + tip_sign · rn`.
    pub tip_sign: [f64; 2],
    /// Non-cutting body (boring bar, holder) relative to the nose centre;
    /// any contact with material is a crash.
    pub holder: Option<Vec<P2>>,
    /// Blade width for grooving/parting tools, mm.
    pub width: Option<f64>,
    /// Drill diameter, mm.
    pub drill_diameter: Option<f64>,
    /// Smallest bore this tool enters, mm (boring bars).
    pub min_bore_diameter: Option<f64>,
}

fn dir(deg: f64) -> P2 {
    let r = deg.to_radians();
    [r.cos(), r.sin()]
}

impl LatheTool {
    /// Right-hand external turning insert (cuts toward the chuck, −Z).
    ///
    /// `lead_deg` is the approach angle κr between the major edge and the
    /// feed direction (95° for a CNMG, 93° for DNMG/VBMT); `included_deg` is
    /// the insert's point angle ε (80° C, 55° D, 35° V). The end-cutting edge
    /// then trails the finished surface by `180 − κr − ε` — the steepest
    /// descending wall this insert can follow toward the chuck.
    pub fn turning_rh(
        number: u32,
        name: impl Into<String>,
        nose_radius: f64,
        lead_deg: f64,
        included_deg: f64,
        edge_len: f64,
    ) -> Self {
        let major = 180.0 - lead_deg;
        let minor = major - included_deg;
        Self {
            number,
            name: name.into(),
            kind: LatheToolKind::Turning,
            nose_radius,
            core: geom::convex_hull(&[
                [0.0, 0.0],
                geom::scale(dir(major), edge_len),
                geom::scale(dir(minor), edge_len),
            ]),
            tip_sign: [-1.0, -1.0],
            holder: None,
            width: None,
            drill_diameter: None,
            min_bore_diameter: None,
        }
    }

    /// CNMG 80° insert in a 95° holder (roughing / general turning).
    pub fn cnmg_95(number: u32, nose_radius: f64) -> Self {
        Self::turning_rh(
            number,
            format!("CNMG 80° κ95 r{nose_radius}"),
            nose_radius,
            95.0,
            80.0,
            6.0,
        )
    }

    /// DNMG 55° insert in a 93° holder (profiling / finishing).
    pub fn dnmg_93(number: u32, nose_radius: f64) -> Self {
        Self::turning_rh(
            number,
            format!("DNMG 55° κ93 r{nose_radius}"),
            nose_radius,
            93.0,
            55.0,
            6.0,
        )
    }

    /// VBMT 35° insert in a 93° holder (copying into steep valleys).
    pub fn vbmt_93(number: u32, nose_radius: f64) -> Self {
        Self::turning_rh(
            number,
            format!("VBMT 35° κ93 r{nose_radius}"),
            nose_radius,
            93.0,
            35.0,
            6.0,
        )
    }

    /// Right-hand boring bar: the turning insert mirrored into the bore,
    /// on a round bar of `bar_diameter` that enters bores ≥ `min_bore`.
    pub fn boring_rh(
        number: u32,
        nose_radius: f64,
        lead_deg: f64,
        included_deg: f64,
        min_bore: f64,
        reach: f64,
    ) -> Self {
        let mut t = Self::turning_rh(number, "", nose_radius, lead_deg, included_deg, 3.0);
        t.name = format!("boring bar κ{lead_deg} ε{included_deg} r{nose_radius} min Ø{min_bore}");
        t.kind = LatheToolKind::Boring;
        t.core = geom::convex_hull(&t.core.iter().map(|p| [p[0], -p[1]]).collect::<Vec<_>>());
        t.tip_sign = [-1.0, 1.0];
        // The bar: from just behind the insert out of the bore (+Z), spanning
        // radially from the far side of a min-bore hole up to under the tip.
        let x_lo = nose_radius - min_bore;
        let x_hi = -nose_radius - 0.3;
        t.holder = Some(geom::convex_hull(&[
            [3.0, x_lo],
            [reach, x_lo],
            [reach, x_hi],
            [3.0, x_hi],
        ]));
        t.min_bore_diameter = Some(min_bore);
        t
    }

    /// External grooving blade of `width` with corner radius `corner`,
    /// plunging up to `depth` deep. The programmed point is the chuck-side
    /// (−Z) corner's imaginary tip.
    pub fn grooving(number: u32, width: f64, corner: f64, depth: f64) -> Self {
        let w = (width - 2.0 * corner).max(0.0);
        Self {
            number,
            name: format!("groove {width} mm r{corner}"),
            kind: LatheToolKind::Grooving,
            nose_radius: corner,
            core: geom::convex_hull(&[[0.0, 0.0], [w, 0.0], [w, depth], [0.0, depth]]),
            tip_sign: [-1.0, -1.0],
            holder: None,
            width: Some(width),
            drill_diameter: None,
            min_bore_diameter: None,
        }
    }

    /// Parting blade (same geometry as a grooving blade).
    pub fn parting(number: u32, width: f64, corner: f64, depth: f64) -> Self {
        let mut t = Self::grooving(number, width, corner, depth);
        t.kind = LatheToolKind::Parting;
        t.name = format!("parting {width} mm r{corner}");
        t
    }

    /// Twist drill on the axis; programmed point = the drill point.
    pub fn drill(number: u32, diameter: f64, point_angle_deg: f64, flute_len: f64) -> Self {
        let r = diameter / 2.0;
        let h = r / (point_angle_deg.to_radians() / 2.0).tan();
        Self {
            number,
            name: format!("drill Ø{diameter} {point_angle_deg}°"),
            kind: LatheToolKind::Drill,
            nose_radius: 0.0,
            core: geom::convex_hull(&[
                [0.0, 0.0],
                [h, r],
                [flute_len, r],
                [flute_len, -r],
                [h, -r],
            ]),
            tip_sign: [0.0, 0.0],
            holder: None,
            width: None,
            drill_diameter: Some(diameter),
            min_bore_diameter: None,
        }
    }

    /// Axial length of a drill's conical point, mm.
    pub fn drill_point_length(&self) -> f64 {
        self.core
            .iter()
            .filter(|p| p[1] > 0.0)
            .map(|p| p[0])
            .fold(f64::INFINITY, f64::min)
            .max(0.0)
    }

    /// Offset from nose centre to programmed point.
    pub fn programmed_offset(&self) -> P2 {
        geom::scale(self.tip_sign, self.nose_radius)
    }

    /// Nose centre for a programmed point.
    pub fn nose_center(&self, programmed: P2) -> P2 {
        geom::sub(programmed, self.programmed_offset())
    }

    /// Programmed point for a nose centre.
    pub fn programmed(&self, center: P2) -> P2 {
        geom::add(center, self.programmed_offset())
    }

    /// Core polygon placed at nose centre `c`.
    pub fn core_at(&self, c: P2) -> Vec<P2> {
        self.core.iter().map(|p| geom::add(*p, c)).collect()
    }

    /// Holder polygon placed at nose centre `c`.
    pub fn holder_at(&self, c: P2) -> Option<Vec<P2>> {
        self.holder
            .as_ref()
            .map(|h| h.iter().map(|p| geom::add(*p, c)).collect())
    }

    /// Reject physically meaningless tools.
    pub fn validate(&self) -> Result<(), TurnError> {
        if (self.nose_radius.is_nan() || self.nose_radius < 0.0) || self.core.is_empty() {
            return Err(TurnError::BadTool(format!(
                "T{}: nose radius {} / empty core",
                self.number, self.nose_radius
            )));
        }
        if self.kind != LatheToolKind::Drill && self.nose_radius < 0.01 {
            return Err(TurnError::BadTool(format!(
                "T{}: nose radius {} < 0.01 mm (model a sharp corner as r0.01)",
                self.number, self.nose_radius
            )));
        }
        if !geom::in_convex(&self.core, [0.0, 0.0]) {
            return Err(TurnError::BadTool(format!(
                "T{}: nose centre outside the core polygon",
                self.number
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cnmg_geometry() {
        let t = LatheTool::cnmg_95(1, 0.4);
        t.validate().unwrap();
        // Programmed point sits at the nose's min-Z, min-X corner.
        assert_eq!(t.programmed_offset(), [-0.4, -0.4]);
        let c = t.nose_center([10.0, 5.0]);
        assert_eq!(c, [10.4, 5.4]);
        // Body extends to +Z and +X only.
        assert!(t.core.iter().all(|p| p[0] >= -1e-9 && p[1] >= -1e-9));
    }

    #[test]
    fn boring_bar_points_up_into_the_wall() {
        let t = LatheTool::boring_rh(3, 0.2, 95.0, 80.0, 8.0, 40.0);
        t.validate().unwrap();
        assert!(t.core.iter().all(|p| p[1] <= 1e-9));
        let h = t.holder.as_ref().unwrap();
        let span = geom::bbox(h);
        // Far side of the bar reaches across a min bore.
        assert!((0.2 - span[1] - 8.0).abs() < 1e-9);
    }

    #[test]
    fn drill_point() {
        let t = LatheTool::drill(4, 6.0, 118.0, 40.0);
        assert!((t.drill_point_length() - 3.0 / (59f64).to_radians().tan()).abs() < 1e-9);
    }
}
