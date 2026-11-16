//! Multi-port chambers: spherical cube (6 faces, optional 8 corner ports)
//! and spherical octagon (2 faces + 8 radial side ports).
//!
//! # Derived geometry
//!
//! Kimball Physics publishes the MCF600 (6.00" / DN100) spherical cube's
//! spherical ID — 213.36 mm — but not its center-to-face. We keep the
//! published cavity and derive the rest:
//!
//! * cavity radius `r_i` = 106.68 mm × (flange OD / 151.6 mm), i.e. the
//!   Kimball cavity scaled with the face flange for other sizes;
//! * the face plane sits at `h` = the larger of (OD/2 + 3 mm) — adjacent
//!   flanges must not touch — and the height where the cavity meets the
//!   plane inside the bore (`√(r_i² − r_bore²) + 1`), so the cavity never
//!   breaks out through a sealing face;
//! * center-to-face = `h` + flange thickness;
//! * outer body = sphere of radius `√(h² + (OD/2)²)` (each face flat just
//!   carries its flange) intersected with the face planes.
//!
//! The part's metadata labels center-to-face as `derived`, and reports the
//! cavity volume next to Kimball's published 5,512 cc for comparison.

use super::flanges::{bom, cf_meta};
use super::fittings::AXES;
use super::*;
use crate::types::{Param, Params, PartEntry, PartInfo, PartMetadata};
use serde_json::json;
use vcad_ir::Document;

const CHAMBER_SIZES: &[&str] = &["DN63", "DN100", "DN160"];
const PORT_SIZES: &[&str] = &["DN16", "DN40"];

/// Kimball MCF600 spherical ID / 2 (mm) and its face flange OD (DN100).
const KIMBALL_R_I: f64 = 213.36 / 2.0;
const KIMBALL_OD: f64 = 151.6;

/// Derived chamber geometry for a face flange row.
#[derive(Debug, Clone, Copy)]
pub struct ChamberGeom {
    /// Cavity radius.
    pub r_i: f64,
    /// Face plane distance (flange back).
    pub h: f64,
    /// Center-to-face.
    pub c2f: f64,
    /// Outer sphere radius.
    pub r_out: f64,
}

/// Derive the chamber body for face flange `d`.
pub fn chamber_geom(d: &CfDims) -> ChamberGeom {
    let r_i = KIMBALL_R_I * d.od / KIMBALL_OD;
    let r_bore = d.bore() / 2.0;
    let h = (d.od / 2.0 + 3.0).max((r_i * r_i - r_bore * r_bore).sqrt() + 1.0);
    ChamberGeom {
        r_i,
        h,
        c2f: h + d.thickness,
        r_out: (h * h + d.od * d.od / 4.0).sqrt(),
    }
}

/// Distance from the center to a satellite port's face along its direction,
/// and the angular half-width (rad) its flange subtends from the center.
fn satellite(g: &ChamberGeom, s: &CfDims) -> (f64, f64) {
    let face = g.r_out + s.thickness + 4.0;
    let half = (s.od / 2.0).atan2(face - s.thickness);
    (face, half)
}

/// Angular half-width of a main face flange seen from the center.
fn main_half(g: &ChamberGeom, d: &CfDims) -> f64 {
    (d.od / 2.0).atan2(g.h)
}

fn body(b: &mut Builder, g: &ChamberGeom, slab_only_z: bool) -> vcad_ir::NodeId {
    let s = b.sphere(g.r_out);
    let big = 2.0 * g.r_out + 2.0;
    let clip = if slab_only_z {
        let c = b.cube(big, big, 2.0 * g.h);
        b.translate(c, -big / 2.0, -big / 2.0, -g.h)
    } else {
        let c = b.cube(2.0 * g.h, 2.0 * g.h, 2.0 * g.h);
        b.translate(c, -g.h, -g.h, -g.h)
    };
    b.intersection(s, clip)
}

fn main_frame(g: &ChamberGeom, name: &str) -> Frame {
    let (_, a, u) = AXES.iter().find(|(n, _, _)| *n == name).copied().unwrap();
    Frame::new(scale(a, g.c2f), a, u)
}

// ---------------------------------------------------------------------------
// Spherical cube
// ---------------------------------------------------------------------------

/// `vacuum.chamber.spherical-cube`.
pub const SPHERICAL_CUBE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.chamber.spherical-cube",
        name: "Spherical cube chamber",
        category: "Vacuum",
        description: Some(
            "Kimball-style spherical cube centred on the origin: six CF faces (ports ±x, ±y, ±z), \
             optional eight corner ports (`c+++`, `c+-+`, …). DN100 = the 6\" cube.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: CHAMBER_SIZES,
                default: "DN100",
            },
            Param::Boolean {
                name: "corner_ports",
                default: false,
            },
            Param::Enum {
                name: "corner_size",
                values: PORT_SIZES,
                default: "DN40",
            },
        ],
        xrefs: &[Xref {
            params: &[("size", "DN100")],
            mcmaster: None,
            iso: Some("ISO 3669"),
            din: None,
        }],
        synonyms: &[
            "spherical cube",
            "6 inch cube",
            "cf cube",
            "vacuum chamber",
            "fusor chamber",
            "mcf600",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_cube,
    info: Some(info_cube),
};

const CORNERS: &[(&str, [f64; 3])] = &[
    ("c+++", [1.0, 1.0, 1.0]),
    ("c-++", [-1.0, 1.0, 1.0]),
    ("c+-+", [1.0, -1.0, 1.0]),
    ("c--+", [-1.0, -1.0, 1.0]),
    ("c++-", [1.0, 1.0, -1.0]),
    ("c-+-", [-1.0, 1.0, -1.0]),
    ("c+--", [1.0, -1.0, -1.0]),
    ("c---", [-1.0, -1.0, -1.0]),
];

struct CubeSpec {
    d: &'static CfDims,
    g: ChamberGeom,
    corner: Option<(&'static CfDims, f64)>,
}

fn cube_spec(p: &Params) -> Result<CubeSpec, String> {
    let d = cf(&p.str("size"))?;
    let g = chamber_geom(d);
    let corner = if p.bool("corner_ports") {
        let s = cf(&p.str("corner_size"))?;
        let (face, half) = satellite(&g, s);
        // Corner direction is 54.74° off each face axis.
        let gap = (1.0f64 / 3f64.sqrt()).acos() - main_half(&g, d) - half;
        if gap <= 0.0 {
            return Err(format!(
                "spherical cube: {} corner flanges would collide with the {} faces",
                s.dn, d.dn
            ));
        }
        Some((s, face))
    } else {
        None
    };
    Ok(CubeSpec { d, g, corner })
}

fn corner_frame(dir: [f64; 3], face: f64) -> Frame {
    let a = normalize(dir);
    Frame::new(scale(a, face), a, any_perp_z(a))
}

/// Clock satellite ports to the global Z where possible.
fn any_perp_z(a: [f64; 3]) -> [f64; 3] {
    let z = [0.0, 0.0, 1.0];
    let u = sub(z, scale(a, dot(z, a)));
    if dot(u, u) < 1e-9 {
        any_perp(a)
    } else {
        normalize(u)
    }
}

fn build_cube(p: &Params) -> Result<Document, String> {
    let c = cube_spec(p)?;
    let (d, g) = (c.d, c.g);
    let mut b = Builder::new();
    let mut parts = Parts::default();
    parts.solids.push(body(&mut b, &g, false));
    for (name, a, _) in AXES {
        let f = main_frame(&g, name);
        flange_at(&mut b, &mut parts, FlangeSpec::Cf(d), &f, Some(CfOpts::chamber()));
        parts
            .cutters
            .push(rod_between(&mut b, d.bore() / 2.0, [0.0; 3], scale(*a, g.c2f + 1.0)));
    }
    if let Some((s, face)) = c.corner {
        for (_, dir) in CORNERS {
            let f = corner_frame(*dir, face);
            flange_at(&mut b, &mut parts, FlangeSpec::Cf(s), &f, Some(CfOpts::chamber()));
            let a = f.axis;
            parts.solids.push(rod_between(
                &mut b,
                s.tube_od / 2.0,
                scale(a, g.r_i),
                scale(a, face - s.thickness * 0.5),
            ));
            parts.cutters.push(rod_between(
                &mut b,
                s.bore() / 2.0,
                [0.0; 3],
                scale(a, face + 1.0),
            ));
        }
    }
    parts.cutters.push(b.sphere(g.r_i));
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_cube(p: &Params) -> Result<PartInfo, String> {
    let c = cube_spec(p)?;
    let (d, g) = (c.d, c.g);
    let mut ports: Vec<_> = AXES
        .iter()
        .map(|(n, _, _)| main_frame(&g, n).port(n, FlangeSpec::Cf(d)))
        .collect();
    let mut lines = vec![bom(format!("{} CF spherical cube, 6 faces", d.dn), 1)];
    if let Some((s, face)) = c.corner {
        ports.extend(
            CORNERS
                .iter()
                .map(|(n, dir)| corner_frame(*dir, face).port(n, FlangeSpec::Cf(s))),
        );
        lines = vec![bom(
            format!("{} CF spherical cube, 6 faces + 8 {} corners", d.dn, s.dn),
            1,
        )];
    }
    Ok(PartInfo {
        ports,
        metadata: json!({
            "face": cf_meta(d),
            "corner_size": c.corner.map(|(s, _)| s.dn),
            "center_to_face_mm": g.c2f,
            "center_to_face_provenance": "derived (Kimball does not publish it); see vcad-parts vacuum::chambers",
            "cavity_id_mm": 2.0 * g.r_i,
            "cavity_volume_cc_sphere": 4.0 / 3.0 * std::f64::consts::PI * g.r_i.powi(3) / 1000.0,
            "reference": "Kimball Physics MCF600-SphCube-F6: spherical ID 213.36 mm, internal volume 5,512 cc",
            "bom": lines,
        }),
    })
}

// ---------------------------------------------------------------------------
// Spherical octagon
// ---------------------------------------------------------------------------

/// `vacuum.chamber.spherical-octagon`.
pub const SPHERICAL_OCTAGON: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.chamber.spherical-octagon",
        name: "Spherical octagon chamber",
        category: "Vacuum",
        description: Some(
            "Spherical octagon centred on the origin: two large CF faces (ports +z, −z) and eight \
             radial side ports in the XY plane (`s0` along +X, then every 45°).",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: CHAMBER_SIZES,
                default: "DN100",
            },
            Param::Enum {
                name: "side_size",
                values: PORT_SIZES,
                default: "DN40",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &["spherical octagon", "octagon chamber", "mcf600-sphoct"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_oct,
    info: Some(info_oct),
};

const SIDE_NAMES: &[&str] = &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"];

fn oct_spec(
    p: &Params,
) -> Result<(&'static CfDims, &'static CfDims, ChamberGeom, f64), String> {
    let d = cf(&p.str("size"))?;
    let s = cf(&p.str("side_size"))?;
    let g = chamber_geom(d);
    let (face, half) = satellite(&g, s);
    if half >= std::f64::consts::PI / 8.0 {
        return Err(format!(
            "spherical octagon: adjacent {} side flanges would collide",
            s.dn
        ));
    }
    if std::f64::consts::FRAC_PI_2 - main_half(&g, d) <= half {
        return Err(format!(
            "spherical octagon: {} side flanges would collide with the {} faces",
            s.dn, d.dn
        ));
    }
    Ok((d, s, g, face))
}

fn side_frame(k: usize, face: f64) -> Frame {
    let th = k as f64 * std::f64::consts::FRAC_PI_4;
    let a = [th.cos(), th.sin(), 0.0];
    Frame::new(scale(a, face), a, [0.0, 0.0, 1.0])
}

fn build_oct(p: &Params) -> Result<Document, String> {
    let (d, s, g, face) = oct_spec(p)?;
    let mut b = Builder::new();
    let mut parts = Parts::default();
    parts.solids.push(body(&mut b, &g, true));
    for name in ["+z", "-z"] {
        let f = main_frame(&g, name);
        flange_at(&mut b, &mut parts, FlangeSpec::Cf(d), &f, Some(CfOpts::chamber()));
        parts.cutters.push(rod_between(
            &mut b,
            d.bore() / 2.0,
            [0.0; 3],
            scale(f.axis, g.c2f + 1.0),
        ));
    }
    for k in 0..8 {
        let f = side_frame(k, face);
        flange_at(&mut b, &mut parts, FlangeSpec::Cf(s), &f, Some(CfOpts::chamber()));
        parts.solids.push(rod_between(
            &mut b,
            s.tube_od / 2.0,
            scale(f.axis, g.r_i),
            scale(f.axis, face - s.thickness * 0.5),
        ));
        parts.cutters.push(rod_between(
            &mut b,
            s.bore() / 2.0,
            [0.0; 3],
            scale(f.axis, face + 1.0),
        ));
    }
    parts.cutters.push(b.sphere(g.r_i));
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_oct(p: &Params) -> Result<PartInfo, String> {
    let (d, s, g, face) = oct_spec(p)?;
    let mut ports = vec![
        main_frame(&g, "+z").port("+z", FlangeSpec::Cf(d)),
        main_frame(&g, "-z").port("-z", FlangeSpec::Cf(d)),
    ];
    ports.extend((0..8).map(|k| side_frame(k, face).port(SIDE_NAMES[k], FlangeSpec::Cf(s))));
    Ok(PartInfo {
        ports,
        metadata: json!({
            "face": cf_meta(d),
            "side": cf_meta(s),
            "center_to_face_mm": g.c2f,
            "side_center_to_face_mm": face,
            "center_to_face_provenance": "derived; see vcad-parts vacuum::chambers",
            "cavity_id_mm": 2.0 * g.r_i,
            "bom": [bom(format!("{} CF spherical octagon, 2 faces + 8 {} sides", d.dn, s.dn), 1)],
        }),
    })
}
