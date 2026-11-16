//! Vacuum hardware: ConFlat (CF) and ISO-KF flanges, fittings, chambers,
//! viewports, feedthroughs, valves and gauges.
//!
//! # Sources
//!
//! Every dimension here comes from a published table; nothing is eyeballed.
//!
//! * **CF (ISO 3669 knife-edge flanges)** — Metallicflex *CF Flanschbauteile*
//!   catalog (`E_CF_Flanschbauteile.pdf`, sheets CF-01 … CF-024): flange OD ×
//!   thickness (fixed and rotatable), tube OD × wall, bolt circle, hole count
//!   and diameter (clearance / tapped thread), copper gasket OD / ID /
//!   thickness, viewport clear window, and tee/cross face-to-face lengths.
//!   Bolt circles agree with the vacgen CF table.
//! * **ISO-KF (ISO 2861-1 / DIN 28403)** — MKS *Series 31 ISO-KF Fittings*
//!   (`isokf.pdf`): flange OD and seal bore (Figure 1), 15° clamping taper,
//!   wing-nut clamp envelope (A × B × C), tee/cross center-to-face (A),
//!   weld-stub tube sizes; centering-ring O-ring ID and 5.33 mm cross-section
//!   from the Marco Rubber centering-ring table.
//! * **Spherical cube** — Kimball Physics MCF600-SphCube-F6 product page:
//!   six 6.00" CF faces, spherical ID 213.36 mm. Kimball does not publish a
//!   center-to-face distance; ours is *derived* (see
//!   [`chambers`]) and labelled as such in the part's metadata.
//!
//! Where a standard leaves something to the vendor (KF rim thickness, the
//! assembled KF seal gap, valve/gauge/feedthrough envelopes) the value is an
//! *envelope approximation* and the part's metadata says so under
//! `"provenance"`. Ratings (voltage, current, bake temperature) are catalog
//! inputs the user supplies — never derived from geometry.
//!
//! # Conventions
//!
//! Every part exposes named **ports** via [`PartInfo`]. A port sits on the
//! *seal mid-plane* of its flange: for CF that is the flange face (mated CF
//! faces meet, the gasket lives in the two recesses); for KF it is half the
//! centering-ring gap in front of the face. `axis` points out of the part,
//! toward whatever mates there; `up` is the clocking reference — CF bolt
//! holes straddle it, so two mated CF ports with matching `up` vectors have
//! aligned bolt holes. Mating = make the ports coincide with anti-parallel
//! axes and parallel `up` ([`mate_transform`]).

pub mod chambers;
pub mod fittings;
pub mod flanges;
pub mod instruments;

use crate::types::{PortInfo, Xref};
use crate::Builder;
use vcad_ir::NodeId;

// ---------------------------------------------------------------------------
// CF table
// ---------------------------------------------------------------------------

/// One row of the CF (ConFlat, ISO 3669) dimension table. Millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CfDims {
    /// ISO nominal size, e.g. `"DN40"`.
    pub dn: &'static str,
    /// US nominal flange OD designation, e.g. `"2.75\""`.
    pub inch: &'static str,
    /// Flange outside diameter.
    pub od: f64,
    /// Fixed flange thickness.
    pub thickness: f64,
    /// Rotatable flange thickness (two-piece, slightly thicker on large sizes).
    pub thickness_rotatable: f64,
    /// Standard tube outside diameter for welded fittings.
    pub tube_od: f64,
    /// Standard tube wall.
    pub tube_wall: f64,
    /// Bolt-circle diameter.
    pub bolt_circle: f64,
    /// Number of bolt holes.
    pub bolt_count: u32,
    /// Bolt thread (tapped flanges), e.g. `"M6"`.
    pub thread: &'static str,
    /// Clearance hole diameter (through-hole flanges).
    pub hole_clearance: f64,
    /// Copper gasket outside diameter (= recess diameter).
    pub gasket_od: f64,
    /// Copper gasket inside diameter.
    pub gasket_id: f64,
    /// Copper gasket thickness.
    pub gasket_thickness: f64,
    /// Viewport clear window diameter (Kodial), if catalogued.
    pub viewport_window: Option<f64>,
    /// Equal tee / cross center-to-face distance (half the face-to-face L).
    pub center_to_face: Option<f64>,
}

impl CfDims {
    /// Bore of the standard tube (tube OD − 2 × wall).
    pub fn bore(&self) -> f64 {
        self.tube_od - 2.0 * self.tube_wall
    }

    /// Tap-drill diameter for the bolt thread.
    pub fn hole_tapped(&self) -> f64 {
        tap_drill(self.thread)
    }

    /// Knife-edge diameter. ISO 3669 places the 20°/70° knife edge on the
    /// gasket land between gasket ID and OD; we put it 40 % of the way out,
    /// which lands it inside the land for every row of the table.
    pub fn knife_edge_dia(&self) -> f64 {
        self.gasket_id + 0.4 * (self.gasket_od - self.gasket_id)
    }

    /// Recess (gasket counterbore) depth: half the gasket, so two mated
    /// faces meet with the gasket filling both recesses.
    pub fn recess_depth(&self) -> f64 {
        self.gasket_thickness / 2.0
    }
}

/// CF dimension table. Metallicflex CF catalog sheets CF-01/CF-04/CF-023
/// (flanges, rotatable flanges, gaskets), CF-011 (viewports), CF-017/CF-020
/// (tees, crosses). DN25 is not an ISO 3669 size but is a common US 2-1/8"
/// flange; Metallicflex lists it with flanges and gaskets only.
pub const CF_TABLE: &[CfDims] = &[
    CfDims {
        dn: "DN16",
        inch: "1.33\"",
        od: 33.8,
        thickness: 7.3,
        thickness_rotatable: 7.3,
        tube_od: 18.0,
        tube_wall: 1.0,
        bolt_circle: 27.0,
        bolt_count: 6,
        thread: "M4",
        hole_clearance: 4.3,
        gasket_od: 21.3,
        gasket_id: 16.2,
        gasket_thickness: 2.0,
        viewport_window: Some(16.0),
        center_to_face: Some(38.0),
    },
    CfDims {
        dn: "DN25",
        inch: "2.12\"",
        od: 54.0,
        thickness: 12.0,
        thickness_rotatable: 12.0,
        tube_od: 26.9,
        tube_wall: 1.65,
        bolt_circle: 41.3,
        bolt_count: 4,
        thread: "M6",
        hole_clearance: 6.5,
        gasket_od: 32.9,
        gasket_id: 25.6,
        gasket_thickness: 2.0,
        viewport_window: None,
        center_to_face: None,
    },
    CfDims {
        dn: "DN40",
        inch: "2.75\"",
        od: 69.4,
        thickness: 13.0,
        thickness_rotatable: 13.0,
        tube_od: 40.0,
        tube_wall: 1.5,
        bolt_circle: 58.7,
        bolt_count: 6,
        thread: "M6",
        hole_clearance: 6.5,
        gasket_od: 48.1,
        gasket_id: 36.8,
        gasket_thickness: 2.0,
        viewport_window: Some(38.0),
        center_to_face: Some(63.0),
    },
    CfDims {
        dn: "DN63",
        inch: "4.5\"",
        od: 113.5,
        thickness: 17.5,
        thickness_rotatable: 19.0,
        tube_od: 70.0,
        tube_wall: 2.0,
        bolt_circle: 92.1,
        bolt_count: 8,
        thread: "M8",
        hole_clearance: 8.4,
        gasket_od: 82.4,
        gasket_id: 63.6,
        gasket_thickness: 2.0,
        viewport_window: Some(65.0),
        center_to_face: Some(105.0),
    },
    CfDims {
        dn: "DN100",
        inch: "6\"",
        od: 151.6,
        thickness: 19.8,
        thickness_rotatable: 21.5,
        tube_od: 104.0,
        tube_wall: 2.0,
        bolt_circle: 130.2,
        bolt_count: 16,
        thread: "M8",
        hole_clearance: 8.4,
        gasket_od: 120.5,
        gasket_id: 101.7,
        gasket_thickness: 2.0,
        viewport_window: Some(90.0),
        center_to_face: Some(135.0),
    },
    CfDims {
        dn: "DN160",
        inch: "8\"",
        od: 202.4,
        thickness: 22.2,
        thickness_rotatable: 24.0,
        tube_od: 154.0,
        tube_wall: 2.0,
        bolt_circle: 181.0,
        bolt_count: 20,
        thread: "M8",
        hole_clearance: 8.4,
        gasket_od: 171.2,
        gasket_id: 152.5,
        gasket_thickness: 2.0,
        viewport_window: Some(135.0),
        center_to_face: Some(167.0),
    },
];

/// CF size names accepted by every CF part's `size` enum.
pub const CF_SIZES: &[&str] = &["DN16", "DN25", "DN40", "DN63", "DN100", "DN160"];

/// Look up a CF row by `"DN40"` or by its inch designation (`"2.75"`,
/// `"2.75\""`, `"2.75in"`).
pub fn cf(size: &str) -> Result<&'static CfDims, String> {
    let s = size.trim();
    let inch = s.trim_end_matches("in").trim_end_matches('"');
    CF_TABLE
        .iter()
        .find(|d| d.dn.eq_ignore_ascii_case(s) || d.inch.trim_end_matches('"') == inch)
        .ok_or_else(|| {
            format!(
                "unknown CF size {size:?} — expected one of {}",
                CF_SIZES.join(", ")
            )
        })
}

/// Tap-drill diameter (ISO coarse) for an M-thread.
pub fn tap_drill(thread: &str) -> f64 {
    match thread {
        "M4" => 3.3,
        "M6" => 5.0,
        "M8" => 6.8,
        "M10" => 8.5,
        _ => 0.8 * crate::fasteners::metric_major_dia(thread),
    }
}

// ---------------------------------------------------------------------------
// KF table
// ---------------------------------------------------------------------------

/// One row of the ISO-KF (ISO 2861-1) table. Millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KfDims {
    /// Nominal size, e.g. `"KF25"`.
    pub name: &'static str,
    /// Flange outside diameter (MKS Fig. 1).
    pub od: f64,
    /// Seal bore (MKS Fig. 1).
    pub bore: f64,
    /// Weld-stub tube OD (MKS short weld stub).
    pub tube_od: f64,
    /// Wing-nut clamp envelope: overall span across the wing nut (MKS "A").
    pub clamp_span: f64,
    /// Wing-nut clamp ring OD (MKS "B").
    pub clamp_od: f64,
    /// Wing-nut clamp axial width (MKS "C").
    pub clamp_width: f64,
    /// Centering-ring O-ring ID (Marco Rubber, -3xx series).
    pub oring_id: f64,
    /// Equal tee / cross center-to-face (MKS "A").
    pub center_to_face: f64,
}

/// O-ring cross-section shared by the KF16–KF50 centering rings (5.33 mm,
/// AS568 -3xx series).
pub const KF_ORING_CS: f64 = 5.33;

/// Assembled KF face-to-face gap (centering-ring metal thickness). ISO 2861
/// does not fix it; 4.0 mm is an envelope value (the O-ring compresses from
/// 5.33 mm to metal contact). Ports sit at half this gap in front of a face.
pub const KF_SEAL_GAP: f64 = 4.0;

/// KF rim thickness at the OD before the 15° clamping taper (envelope).
pub const KF_RIM: f64 = 3.0;

/// KF weld-stub wall (MKS: 0.065" = 1.65 mm).
pub const KF_TUBE_WALL: f64 = 1.65;

/// KF dimension table (MKS Series 31 + Marco Rubber).
pub const KF_TABLE: &[KfDims] = &[
    KfDims {
        name: "KF16",
        od: 30.0,
        bore: 17.0,
        tube_od: 19.05,
        clamp_span: 57.0,
        clamp_od: 43.0,
        clamp_width: 16.0,
        oring_id: 18.42,
        center_to_face: 40.0,
    },
    KfDims {
        name: "KF25",
        od: 40.0,
        bore: 26.0,
        tube_od: 25.4,
        clamp_span: 70.0,
        clamp_od: 55.0,
        clamp_width: 16.0,
        oring_id: 27.94,
        center_to_face: 50.0,
    },
    KfDims {
        name: "KF40",
        od: 55.0,
        bore: 41.0,
        tube_od: 38.1,
        clamp_span: 86.0,
        clamp_od: 70.0,
        clamp_width: 16.0,
        oring_id: 40.64,
        center_to_face: 65.0,
    },
    KfDims {
        name: "KF50",
        od: 75.0,
        bore: 52.0,
        tube_od: 50.8,
        clamp_span: 110.0,
        clamp_od: 94.0,
        clamp_width: 20.0,
        oring_id: 53.34,
        center_to_face: 70.0,
    },
];

/// KF size names accepted by every KF part's `size` enum.
pub const KF_SIZES: &[&str] = &["KF16", "KF25", "KF40", "KF50"];

/// Look up a KF row by `"KF25"` / `"NW25"` / `"25"`.
pub fn kf(size: &str) -> Result<&'static KfDims, String> {
    let s = size.trim().to_ascii_uppercase();
    let n = s
        .trim_start_matches("KF")
        .trim_start_matches("NW")
        .trim_start_matches("ISO")
        .trim_start_matches('-');
    KF_TABLE.iter().find(|d| &d.name[2..] == n).ok_or_else(|| {
        format!(
            "unknown KF size {size:?} — expected one of {}",
            KF_SIZES.join(", ")
        )
    })
}

// ---------------------------------------------------------------------------
// Flange specs (family + size) used by fittings
// ---------------------------------------------------------------------------

/// A flange interface: family + size row.
#[derive(Debug, Clone, Copy)]
pub enum FlangeSpec {
    /// ConFlat.
    Cf(&'static CfDims),
    /// ISO-KF.
    Kf(&'static KfDims),
}

impl FlangeSpec {
    /// Resolve `family` (`"CF"`/`"KF"`) + size, accepting a size string that
    /// already names its family (`"KF25"`, `"DN40"`).
    pub fn parse(family: &str, size: &str) -> Result<Self, String> {
        let up = size.trim().to_ascii_uppercase();
        if up.starts_with("KF") || up.starts_with("NW") {
            return kf(size).map(FlangeSpec::Kf);
        }
        if up.starts_with("DN") {
            return cf(size).map(FlangeSpec::Cf);
        }
        match family.to_ascii_uppercase().as_str() {
            "KF" => kf(size).map(FlangeSpec::Kf),
            _ => cf(size).map(FlangeSpec::Cf),
        }
    }

    /// `"CF"` or `"KF"`.
    pub fn family(&self) -> &'static str {
        match self {
            FlangeSpec::Cf(_) => "CF",
            FlangeSpec::Kf(_) => "KF",
        }
    }

    /// Size designation (`"DN40"`, `"KF25"`).
    pub fn size(&self) -> &'static str {
        match self {
            FlangeSpec::Cf(d) => d.dn,
            FlangeSpec::Kf(d) => d.name,
        }
    }

    /// Flange outside diameter.
    pub fn od(&self) -> f64 {
        match self {
            FlangeSpec::Cf(d) => d.od,
            FlangeSpec::Kf(d) => d.od,
        }
    }

    /// Tube OD of a welded fitting on this flange.
    pub fn tube_od(&self) -> f64 {
        match self {
            FlangeSpec::Cf(d) => d.tube_od,
            FlangeSpec::Kf(d) => d.tube_od,
        }
    }

    /// Through bore of a welded fitting on this flange.
    pub fn bore(&self) -> f64 {
        match self {
            FlangeSpec::Cf(d) => d.bore(),
            FlangeSpec::Kf(d) => (d.tube_od - 2.0 * KF_TUBE_WALL).min(d.bore),
        }
    }

    /// Axial length the flange occupies behind its face.
    pub fn thickness(&self) -> f64 {
        match self {
            FlangeSpec::Cf(d) => d.thickness,
            FlangeSpec::Kf(d) => kf_flange_height(d),
        }
    }

    /// Distance from the port (seal mid-plane) back to the flange face.
    pub fn face_setback(&self) -> f64 {
        match self {
            FlangeSpec::Cf(_) => 0.0,
            FlangeSpec::Kf(_) => KF_SEAL_GAP / 2.0,
        }
    }

    /// Equal tee/cross center-to-face from the catalog, if listed.
    pub fn center_to_face(&self) -> Option<f64> {
        match self {
            FlangeSpec::Cf(d) => d.center_to_face,
            FlangeSpec::Kf(d) => Some(d.center_to_face),
        }
    }
}

/// Axial height of a KF flange: rim + the 15° taper from OD down to the tube.
pub fn kf_flange_height(d: &KfDims) -> f64 {
    KF_RIM + (d.od - d.tube_od) / 2.0 * 15f64.to_radians().tan() + 1.0
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// A right-handed frame: origin, `axis` (local +Z) and `up` (local +X).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Origin.
    pub origin: [f64; 3],
    /// Local +Z (port outward normal). Unit length.
    pub axis: [f64; 3],
    /// Local +X (clocking reference). Unit, ⟂ `axis`.
    pub up: [f64; 3],
}

impl Frame {
    /// Frame with orthonormalized `up`.
    pub fn new(origin: [f64; 3], axis: [f64; 3], up: [f64; 3]) -> Self {
        let a = normalize(axis);
        let d = dot(up, a);
        let u = normalize(sub(up, scale(a, d)));
        Self {
            origin,
            axis: a,
            up: u,
        }
    }

    /// Rotation matrix whose columns are (up, axis × up, axis): local → world.
    pub fn matrix(&self) -> [[f64; 3]; 3] {
        let x = self.up;
        let z = self.axis;
        let y = cross(z, x);
        [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]
    }

    /// Convert to a [`PortInfo`] with the given name and flange.
    pub fn port(&self, name: &str, spec: FlangeSpec) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            origin: self.origin,
            axis: self.axis,
            up: self.up,
            family: spec.family().to_string(),
            size: spec.size().to_string(),
            tapped: false,
        }
    }
}

/// A rigid transform `p ↦ R·p + t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rigid {
    /// Rotation (row-major).
    pub r: [[f64; 3]; 3],
    /// Translation.
    pub t: [f64; 3],
}

impl Rigid {
    /// Identity.
    pub const IDENTITY: Rigid = Rigid {
        r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0, 0.0, 0.0],
    };

    /// Apply to a point.
    pub fn point(&self, p: [f64; 3]) -> [f64; 3] {
        add(mat_vec(&self.r, p), self.t)
    }

    /// Apply to a direction.
    pub fn dir(&self, v: [f64; 3]) -> [f64; 3] {
        mat_vec(&self.r, v)
    }

    /// `self ∘ other` (apply `other` first).
    pub fn then_after(&self, other: &Rigid) -> Rigid {
        Rigid {
            r: mat_mul(&self.r, &other.r),
            t: self.point(other.t),
        }
    }

    /// Transform a port.
    pub fn port(&self, p: &PortInfo) -> PortInfo {
        PortInfo {
            origin: self.point(p.origin),
            axis: self.dir(p.axis),
            up: self.dir(p.up),
            ..p.clone()
        }
    }

    /// XYZ Euler angles in degrees for `CsgOp::Rotate` (R = Rz·Ry·Rx).
    pub fn euler_xyz_deg(&self) -> [f64; 3] {
        mat_to_euler_xyz_deg(&self.r)
    }
}

/// The transform that places a child so its port `child` mates with the
/// (world-space) port `parent`: origins coincide, axes anti-parallel, `up`
/// vectors parallel (so CF bolt holes line up).
pub fn mate_transform(child: &PortInfo, parent: &PortInfo) -> Rigid {
    // Child port frame (local→child coords) and target frame in world.
    let c = Frame::new(child.origin, child.axis, child.up);
    let target = Frame::new(parent.origin, scale(parent.axis, -1.0), parent.up);
    let rc = c.matrix();
    let rt = target.matrix();
    // R maps child frame onto target frame: R = Rt · Rcᵀ.
    let r = mat_mul(&rt, &transpose(&rc));
    let t = sub(target.origin, mat_vec(&r, c.origin));
    Rigid { r, t }
}

/// Check that two flange interfaces can mate (same family and size).
pub fn check_compatible(child: &PortInfo, parent: &PortInfo) -> Result<(), String> {
    if child.family != parent.family || child.size != parent.size {
        return Err(format!(
            "port mismatch: {} {} cannot mate with {} {} — insert a reducer or adapter",
            child.family, child.size, parent.family, parent.size
        ));
    }
    Ok(())
}

/// Decompose R = Rx(a)·Ry(b)·Rz(c) into degrees — the matrix `CsgOp::Rotate`
/// builds (`rx.then(&ry).then(&rz)`, where `then` is a right-multiply, so Z
/// acts on the part first and X last).
pub fn mat_to_euler_xyz_deg(m: &[[f64; 3]; 3]) -> [f64; 3] {
    let sb = m[0][2];
    let (ax, ay, az) = if sb.abs() > 1.0 - 1e-9 {
        // Gimbal lock: pitch is ±90°, fold roll into yaw.
        (0.0_f64, sb.clamp(-1.0, 1.0).asin(), m[1][0].atan2(m[1][1]))
    } else {
        (
            (-m[1][2]).atan2(m[2][2]),
            sb.clamp(-1.0, 1.0).asin(),
            (-m[0][1]).atan2(m[0][0]),
        )
    };
    [ax.to_degrees(), ay.to_degrees(), az.to_degrees()]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub(crate) fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub(crate) fn normalize(a: [f64; 3]) -> [f64; 3] {
    let n = dot(a, a).sqrt();
    if n < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        scale(a, 1.0 / n)
    }
}
fn mat_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
fn transpose(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}
fn mat_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Options for drawing a CF flange.
#[derive(Debug, Clone, Copy)]
pub struct CfOpts {
    /// Through bore diameter (`None` = blank).
    pub bore: Option<f64>,
    /// Tapped bolt holes (else clearance).
    pub tapped: bool,
    /// Rotatable (two-piece) flange.
    pub rotatable: bool,
    /// Model the knife edge in the gasket recess.
    pub knife_edge: bool,
}

impl CfOpts {
    /// Blank (the fitting cuts its own bore), through holes, fixed, with
    /// knife edge — the flange on a welded fitting.
    pub fn fitting(_d: &CfDims) -> Self {
        Self {
            bore: None,
            tapped: false,
            rotatable: false,
            knife_edge: true,
        }
    }

    /// Chamber face: blank, tapped blind holes, knife edge.
    pub fn chamber() -> Self {
        Self {
            bore: None,
            tapped: true,
            rotatable: false,
            knife_edge: true,
        }
    }
}

/// How much flange detail a multi-flange part models.
///
/// Every bolt hole and knife edge is a set of faces the next boolean has to
/// classify, and that cost compounds when a fitting or chamber unions
/// several flanges with its body. Ports, metadata and outer envelope are
/// identical at every level; only the face detail changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detail {
    /// Plain flange discs (bored where the part needs a bore): the right
    /// envelope for layout, clearance and rendering. Fast.
    Envelope,
    /// Bolt holes and gasket recesses, no knife edge.
    Holes,
    /// Bolt holes, recesses and knife edges.
    #[default]
    Full,
}

impl Detail {
    /// Parse the `detail` parameter (`envelope` / `holes` / `full`).
    pub fn parse(s: &str) -> Self {
        match s {
            "envelope" => Detail::Envelope,
            "holes" => Detail::Holes,
            _ => Detail::Full,
        }
    }
}

/// The `detail` parameter shared by multi-flange parts.
pub(crate) const DETAIL_PARAM: crate::types::Param = crate::types::Param::Enum {
    name: "detail",
    values: &["envelope", "holes", "full"],
    default: "envelope",
};

/// Union a list of nodes (left fold); `None` when empty.
pub(crate) fn union_all(b: &mut Builder, ids: &[NodeId]) -> Option<NodeId> {
    let mut it = ids.iter().copied();
    let first = it.next()?;
    Some(it.fold(first, |acc, id| b.union(acc, id)))
}

/// Cylinder from `z0` to `z1` along local Z.
pub(crate) fn cyl_z(b: &mut Builder, r: f64, z0: f64, z1: f64) -> NodeId {
    let c = b.cylinder(r, z1 - z0);
    b.translate(c, 0.0, 0.0, z0)
}

/// Annulus (tube) from `z0` to `z1`.
///
/// Deliberately a cylinder difference, not an extrude of a ring sketch: the
/// kernel's extrude-with-hole-loops currently over-removes hole area (a
/// DN160 bolt pattern measured 2.55 % light), while the boolean is exact.
pub(crate) fn tube_z(b: &mut Builder, r_out: f64, r_in: f64, z0: f64, z1: f64) -> NodeId {
    let o = cyl_z(b, r_out, z0, z1);
    let i = cyl_z(b, r_in, z0 - 0.5, z1 + 0.5);
    b.difference(o, i)
}

/// Bolt-hole centres (local XY) on a bolt circle, straddling +X.
pub(crate) fn bolt_circle_xy(count: u32, bc: f64) -> Vec<[f64; 2]> {
    let step = std::f64::consts::TAU / count as f64;
    (0..count)
        .map(|k| {
            let th = (k as f64 + 0.5) * step;
            [bc / 2.0 * th.cos(), bc / 2.0 * th.sin()]
        })
        .collect()
}

/// Bolt-hole centres (local XY) for a CF row, straddling +X.
pub fn cf_bolt_positions(d: &CfDims) -> Vec<[f64; 2]> {
    bolt_circle_xy(d.bolt_count, d.bolt_circle)
}

/// Finished CF flange in its local frame: sealing face at `z = 0`, body
/// `z ∈ [-T, 0]`.
///
/// One cylinder minus *one* union of cutters (bolt holes, gasket recess,
/// optional bore and rotatable split groove), then — optionally — a revolved
/// knife edge on the recess floor. Pinned to its closed-form volume by
/// `vcad-eval/tests/vacuum_parts.rs`.
pub(crate) fn cf_flange_local(b: &mut Builder, d: &CfDims, o: &CfOpts) -> NodeId {
    let t = if o.rotatable {
        d.thickness_rotatable
    } else {
        d.thickness
    };
    let hole = if o.tapped {
        d.hole_tapped()
    } else {
        d.hole_clearance
    };
    let disc = cyl_z(b, d.od / 2.0, -t, 0.0);
    let mut cuts: Vec<NodeId> = cf_bolt_positions(d)
        .into_iter()
        .map(|[x, y]| {
            let c = cyl_z(b, hole / 2.0, -t - 1.0, 1.0);
            b.translate(c, x, y, 0.0)
        })
        .collect();

    // Gasket recess (counterbore to the gasket OD, half a gasket deep).
    let depth = d.recess_depth();
    let r_recess = d.gasket_od / 2.0 + 0.1;
    cuts.push(cyl_z(b, r_recess, -depth, 1.0));
    if let Some(bore) = o.bore {
        cuts.push(cyl_z(b, bore / 2.0, -t - 1.0, 1.0));
    }
    if o.rotatable {
        // Split line between the knife-edge insert and the bolt ring: a
        // narrow groove from the back face, stopping short of the seal face
        // so the model stays one solid.
        let r = (r_recess + d.bolt_circle / 2.0 - hole / 2.0) / 2.0;
        cuts.push(tube_z(b, r + 0.25, r - 0.25, -t - 1.0, -depth - 1.5));
    }
    let cut = union_all(b, &cuts).expect("a CF flange always has cuts");
    let mut flange = b.difference(disc, cut);

    if o.knife_edge {
        // ISO 3669 knife edge on the recess floor: vertical outer wall, the
        // sloped flank facing the bore, 0.3 mm tall, sunk 0.05 mm into the
        // floor so the union is not coplanar. Built as a short cylinder
        // minus a cone (a revolved triangle unions far slower in the
        // current kernel).
        let r = d.knife_edge_dia() / 2.0;
        let h = 0.3;
        let z0 = -depth - 0.05;
        let disc = cyl_z(b, r, z0, -depth + h);
        let flank = b.cone(r - 0.6, r + 0.001, h + 0.05);
        let flank = b.translate(flank, 0.0, 0.0, z0);
        let ring = b.difference(disc, flank);
        flange = b.union(flange, ring);
    }
    flange
}

/// KF flange solid in local frame: face at `z = 0`, body behind — rim plus
/// the 15° clamping taper as one revolve. Returns (solid, bore cutter).
pub(crate) fn kf_flange_local(
    b: &mut Builder,
    d: &KfDims,
    bore: Option<f64>,
) -> (NodeId, Option<NodeId>) {
    let h = kf_flange_height(d);
    let ro = d.od / 2.0;
    let rt = d.tube_od / 2.0 + 0.5;
    let solid = b.revolve_z(&[[0.0, -h], [rt, -h], [ro, -KF_RIM], [ro, 0.0], [0.0, 0.0]]);
    let cutter = bore.map(|bore| cyl_z(b, bore / 2.0, -h - 1.0, 1.0));
    (solid, cutter)
}

/// Geometry being collected for one part: material, things to cut, and
/// details added back after the cut.
#[derive(Default)]
pub(crate) struct Parts {
    pub solids: Vec<NodeId>,
    pub cutters: Vec<NodeId>,
    pub after: Vec<NodeId>,
    /// Flange detail level for this part.
    pub detail: Detail,
    /// Local-frame flange nodes already built, keyed by size + options.
    /// The evaluator caches solids per node id, so every port that reuses
    /// one is evaluated once — a 6-way cross builds one flange, not six.
    flange_cache: std::collections::HashMap<String, NodeId>,
}

impl Parts {
    /// Empty collection at the given flange detail.
    pub fn with_detail(detail: Detail) -> Self {
        Self {
            detail,
            ..Default::default()
        }
    }

    /// Union solids, subtract all cutters at once, then union the details.
    pub fn assemble(self, b: &mut Builder) -> Result<NodeId, String> {
        let body = union_all(b, &self.solids).ok_or("part has no geometry")?;
        let body = match union_all(b, &self.cutters) {
            Some(c) => b.difference(body, c),
            None => body,
        };
        Ok(match union_all(b, &self.after) {
            Some(a) => b.union(body, a),
            None => body,
        })
    }
}

/// Draw a flange for `spec` at the world-space port frame `f` (seal
/// mid-plane) into `out`. CF flanges arrive finished (bolt holes, recess,
/// knife edge); a bore, if any, comes from `cf_opts` for a standalone part.
/// Fittings pass a blank flange and cut one bore through flange + tube.
pub(crate) fn flange_at(
    b: &mut Builder,
    out: &mut Parts,
    spec: FlangeSpec,
    f: &Frame,
    cf_opts: Option<CfOpts>,
) {
    match spec {
        FlangeSpec::Cf(d) => {
            let mut o = cf_opts.unwrap_or_else(|| CfOpts::fitting(d));
            if out.detail == Detail::Holes {
                o.knife_edge = false;
            }
            let key = format!("{}{o:?}{:?}", d.dn, out.detail);
            let fl = match out.flange_cache.get(&key) {
                Some(&id) => id,
                None => {
                    let id = if out.detail == Detail::Envelope {
                        let t = if o.rotatable {
                            d.thickness_rotatable
                        } else {
                            d.thickness
                        };
                        let disc = cyl_z(b, d.od / 2.0, -t, 0.0);
                        match o.bore {
                            Some(bore) => {
                                let c = cyl_z(b, bore / 2.0, -t - 1.0, 1.0);
                                b.difference(disc, c)
                            }
                            None => disc,
                        }
                    } else {
                        cf_flange_local(b, d, &o)
                    };
                    out.flange_cache.insert(key, id);
                    id
                }
            };
            out.solids.push(place(b, fl, f));
        }
        FlangeSpec::Kf(d) => {
            let bore = cf_opts.and_then(|o| o.bore);
            let (s, c) = kf_flange_local(b, d, bore);
            // Shift the KF face back from the port by half the seal gap.
            let back = -spec.face_setback();
            let s = b.translate(s, 0.0, 0.0, back);
            out.solids.push(place(b, s, f));
            if let Some(c) = c {
                let c = b.translate(c, 0.0, 0.0, back);
                out.cutters.push(place(b, c, f));
            }
        }
    }
}

/// Place a node built in local coordinates (Z = axis, X = up) at frame `f`.
pub(crate) fn place(b: &mut Builder, node: NodeId, f: &Frame) -> NodeId {
    let r = Rigid {
        r: f.matrix(),
        t: f.origin,
    };
    let e = r.euler_xyz_deg();
    let id = if e.iter().any(|a| a.abs() > 1e-9) {
        b.rotate(node, e[0], e[1], e[2])
    } else {
        node
    };
    if f.origin.iter().any(|v| v.abs() > 1e-12) {
        b.translate(id, f.origin[0], f.origin[1], f.origin[2])
    } else {
        id
    }
}

/// A tube segment along an arbitrary axis from `p0` to `p1` (solid, no bore).
pub(crate) fn rod_between(b: &mut Builder, r: f64, p0: [f64; 3], p1: [f64; 3]) -> NodeId {
    let d = sub(p1, p0);
    let len = dot(d, d).sqrt();
    let axis = normalize(d);
    let up = any_perp(axis);
    let c = b.cylinder(r, len);
    place(b, c, &Frame::new(p0, axis, up))
}

/// Some unit vector perpendicular to `a`.
pub(crate) fn any_perp(a: [f64; 3]) -> [f64; 3] {
    let t = if a[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    normalize(sub(t, scale(a, dot(t, a))))
}

/// Standard xrefs: CF parts cite ISO 3669, KF parts ISO 2861.
pub(crate) const CF_XREF: &[Xref] = &[Xref {
    params: &[],
    mcmaster: None,
    iso: Some("ISO 3669"),
    din: None,
}];

/// ISO-KF xrefs.
pub(crate) const KF_XREF: &[Xref] = &[Xref {
    params: &[],
    mcmaster: None,
    iso: Some("ISO 2861"),
    din: Some("DIN 28403"),
}];

/// Shared palette thumbnail for vacuum parts.
pub(crate) const THUMB: &[u8] = include_bytes!("../../thumbs/vacuum.svg");

/// Material key used for stainless vacuum hardware.
pub(crate) const SS: &str = "stainless";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_lookup_by_dn_and_inch() {
        assert_eq!(cf("DN40").unwrap().od, 69.4);
        assert_eq!(cf("2.75\"").unwrap().dn, "DN40");
        assert_eq!(cf("6").unwrap().dn, "DN100");
        assert!(cf("DN999").is_err());
    }

    #[test]
    fn kf_lookup_aliases() {
        assert_eq!(kf("KF25").unwrap().od, 40.0);
        assert_eq!(kf("NW40").unwrap().od, 55.0);
        assert_eq!(kf("16").unwrap().name, "KF16");
    }

    #[test]
    fn knife_edge_sits_on_gasket_land() {
        for d in CF_TABLE {
            let ke = d.knife_edge_dia();
            assert!(ke > d.gasket_id && ke < d.gasket_od, "{}", d.dn);
            // and clear of the standard bore
            assert!(
                ke / 2.0 - 0.6 > d.bore() / 2.0,
                "{} knife edge over bore",
                d.dn
            );
            // bolt holes clear the recess
            assert!(
                d.bolt_circle / 2.0 - d.hole_clearance / 2.0 > d.gasket_od / 2.0,
                "{}",
                d.dn
            );
        }
    }

    #[test]
    fn mate_transform_flips_axis_and_keeps_up() {
        let parent = PortInfo {
            name: "p".into(),
            origin: [10.0, 0.0, 0.0],
            axis: [1.0, 0.0, 0.0],
            up: [0.0, 0.0, 1.0],
            family: "CF".into(),
            size: "DN40".into(),
            tapped: false,
        };
        let child = PortInfo {
            name: "c".into(),
            origin: [0.0, 0.0, 5.0],
            axis: [0.0, 0.0, -1.0],
            up: [1.0, 0.0, 0.0],
            family: "CF".into(),
            size: "DN40".into(),
            tapped: false,
        };
        let m = mate_transform(&child, &parent);
        let placed = m.port(&child);
        for i in 0..3 {
            assert!((placed.origin[i] - parent.origin[i]).abs() < 1e-9);
            assert!((placed.axis[i] + parent.axis[i]).abs() < 1e-9);
            assert!((placed.up[i] - parent.up[i]).abs() < 1e-9);
        }
    }

    /// World-space bolt-hole centres of a CF port (holes straddle `up`).
    fn bolt_world(p: &PortInfo) -> Vec<[f64; 3]> {
        let d = cf(&p.size).unwrap();
        let m = Frame::new(p.origin, p.axis, p.up).matrix();
        cf_bolt_positions(d)
            .into_iter()
            .map(|[x, y]| {
                add(
                    p.origin,
                    [
                        m[0][0] * x + m[0][1] * y,
                        m[1][0] * x + m[1][1] * y,
                        m[2][0] * x + m[2][1] * y,
                    ],
                )
            })
            .collect()
    }

    #[test]
    fn mated_cf_bolt_holes_line_up() {
        use std::collections::HashMap;
        let none = HashMap::new();
        let cube = crate::part_info("vacuum.chamber.spherical-cube", &none).unwrap();
        let mut np = HashMap::new();
        np.insert("size".to_string(), serde_json::json!("DN100"));
        let nip = crate::part_info("vacuum.nipple", &np).unwrap();
        let cp = nip.ports.iter().find(|p| p.name == "-z").unwrap();
        for pp in &cube.ports {
            let placed = mate_transform(cp, pp).port(cp);
            let a = bolt_world(pp);
            let b = bolt_world(&placed);
            for h in &a {
                assert!(
                    b.iter().any(|g| (0..3).all(|i| (g[i] - h[i]).abs() < 1e-6)),
                    "port {}: hole {h:?} has no partner",
                    pp.name
                );
            }
        }
    }

    #[test]
    fn euler_roundtrip() {
        let f = Frame::new([0.0; 3], [1.0, 1.0, 1.0], [0.0, 0.0, 1.0]);
        let m = f.matrix();
        let e = mat_to_euler_xyz_deg(&m);
        let (a, bb, c) = (e[0].to_radians(), e[1].to_radians(), e[2].to_radians());
        let rx = [
            [1.0, 0.0, 0.0],
            [0.0, a.cos(), -a.sin()],
            [0.0, a.sin(), a.cos()],
        ];
        let ry = [
            [bb.cos(), 0.0, bb.sin()],
            [0.0, 1.0, 0.0],
            [-bb.sin(), 0.0, bb.cos()],
        ];
        let rz = [
            [c.cos(), -c.sin(), 0.0],
            [c.sin(), c.cos(), 0.0],
            [0.0, 0.0, 1.0],
        ];
        let r = mat_mul(&rx, &mat_mul(&ry, &rz));
        for i in 0..3 {
            for j in 0..3 {
                assert!((r[i][j] - m[i][j]).abs() < 1e-9);
            }
        }
    }
}
