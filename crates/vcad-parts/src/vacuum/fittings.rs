//! Welded fittings: nipples, tees, 4/6-way crosses, CF↔KF adapters.
//!
//! The flange family follows the size name: `DN…` is ConFlat, `KF…` is
//! ISO-KF. Ports are named by their outward axis (`"+z"`, `"-x"`, …); run
//! ports (`±z`) clock to +X, side ports to +Z.

use super::flanges::{bom, cf_meta, kf_meta};
use super::*;
use crate::types::{Param, Params, PartEntry, PartInfo, PartMetadata};
use serde_json::json;
use vcad_ir::Document;

/// Every flange size a fitting accepts (CF then KF).
pub const ALL_SIZES: &[&str] = &[
    "DN16", "DN25", "DN40", "DN63", "DN100", "DN160", "KF16", "KF25", "KF40", "KF50",
];

/// Port name, direction and clocking for the six axis directions.
pub(crate) const AXES: &[(&str, [f64; 3], [f64; 3])] = &[
    ("+z", [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
    ("-z", [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
    ("+x", [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ("-x", [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ("+y", [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ("-y", [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
];

fn axis(name: &str) -> ([f64; 3], [f64; 3]) {
    AXES.iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, u)| (*a, *u))
        .unwrap_or(([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]))
}

fn spec_of(p: &Params) -> Result<FlangeSpec, String> {
    FlangeSpec::parse("", &p.str("size"))
}

fn meta_of(spec: FlangeSpec) -> serde_json::Value {
    match spec {
        FlangeSpec::Cf(d) => cf_meta(d),
        FlangeSpec::Kf(d) => kf_meta(d),
    }
}

/// Port frame for a flange whose *face* is `face_dist` from the origin along
/// the named axis (the port sits `face_setback` further out).
fn port_frame(spec: FlangeSpec, name: &str, face_dist: f64) -> Frame {
    let (a, u) = axis(name);
    Frame::new(scale(a, face_dist + spec.face_setback()), a, u)
}

/// Build a multi-port fitting from `(port name, face distance)` arms: one
/// flange per arm, and per axis one tube + one bore — a single through tube
/// when both `+` and `−` arms exist (two coaxial tubes meeting at the origin
/// would be a coplanar-face boolean), a tube from the origin otherwise.
///
/// Perpendicular axes get radii 0.05 mm apart (z, then x, then y) so no two
/// crossing cylinders are equal-radius — the tangent-intersection case the
/// kernel's surface intersector handles worst. The difference is far below
/// any catalog tolerance.
fn arms(
    b: &mut Builder,
    spec: FlangeSpec,
    detail: Detail,
    arms: &[(&str, f64)],
) -> Result<vcad_ir::NodeId, String> {
    let mut parts = Parts::with_detail(detail);
    for &(name, face) in arms {
        let f = port_frame(spec, name, face);
        flange_at(b, &mut parts, spec, &f, None);
    }
    let face_of = |n: &str| arms.iter().find(|(m, _)| *m == n).map(|(_, f)| *f);
    for (i, (plus, minus)) in [("+z", "-z"), ("+x", "-x"), ("+y", "-y")]
        .iter()
        .enumerate()
    {
        let (p, m) = (face_of(plus), face_of(minus));
        if p.is_none() && m.is_none() {
            continue;
        }
        let shrink = 0.05 * i as f64;
        let r_out = spec.tube_od() / 2.0 - shrink;
        let r_in = spec.bore() / 2.0 - shrink;
        let (a, _) = axis(plus);
        let tube_end = |f: f64| f - spec.thickness() * 0.5;
        let bore_end = |f: f64| f + spec.face_setback() + 1.0;
        let (t0, t1) = (m.map_or(0.0, |f| -tube_end(f)), p.map_or(0.0, tube_end));
        let (b0, b1) = (m.map_or(-0.5, |f| -bore_end(f)), p.map_or(0.5, bore_end));
        parts
            .solids
            .push(rod_between(b, r_out, scale(a, t0), scale(a, t1)));
        parts
            .cutters
            .push(rod_between(b, r_in, scale(a, b0), scale(a, b1)));
    }
    parts.assemble(b)
}

// ---------------------------------------------------------------------------
// Nipple
// ---------------------------------------------------------------------------

/// `vacuum.nipple`.
pub const NIPPLE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.nipple",
        name: "Vacuum nipple",
        category: "Vacuum",
        description: Some(
            "Full nipple: a tube with a flange on each end. `length` is face-to-face; ports `-z` \
             (at the origin) and `+z`. CF (DN…) or KF (KF…) by size.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: ALL_SIZES,
                default: "DN40",
            },
            Param::Length {
                name: "length",
                min: 10.0,
                max: 1000.0,
                default: 100.0,
                unit: "mm",
            },
            DETAIL_PARAM,
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "full nipple",
            "spool",
            "cf nipple",
            "kf nipple",
            "extension tube",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_nipple,
    info: Some(info_nipple),
};

fn nipple_len(p: &Params, spec: FlangeSpec) -> Result<f64, String> {
    let l = p.f64("length");
    let min = 2.0 * spec.thickness() + 2.0;
    if l < min {
        return Err(format!(
            "nipple: length {l} mm is shorter than two {} {} flanges ({min:.1} mm)",
            spec.family(),
            spec.size()
        ));
    }
    Ok(l)
}

fn build_nipple(p: &Params) -> Result<Document, String> {
    let spec = spec_of(p)?;
    let l = nipple_len(p, spec)?;
    let mut b = Builder::new();
    // Built centred, then shifted so the -z face sits at z = 0.
    let root = arms(
        &mut b,
        spec,
        Detail::parse(&p.str("detail")),
        &[("+z", l / 2.0), ("-z", l / 2.0)],
    )?;
    let root = b.translate(root, 0.0, 0.0, l / 2.0);
    Ok(b.finish(root, SS))
}

fn info_nipple(p: &Params) -> Result<PartInfo, String> {
    let spec = spec_of(p)?;
    let l = nipple_len(p, spec)?;
    let shift = Rigid {
        t: [0.0, 0.0, l / 2.0],
        ..Rigid::IDENTITY
    };
    let ports = ["-z", "+z"]
        .iter()
        .map(|n| shift.port(&port_frame(spec, n, l / 2.0).port(n, spec)))
        .collect();
    let mut m = meta_of(spec);
    m["face_to_face_mm"] = json!(l);
    m["bom"] = json!([bom(
        format!("{} {} full nipple, {l} mm", spec.family(), spec.size()),
        1
    )]);
    Ok(PartInfo { ports, metadata: m })
}

// ---------------------------------------------------------------------------
// Tee & cross
// ---------------------------------------------------------------------------

fn c2f(p: &Params, spec: FlangeSpec) -> Result<f64, String> {
    let given = p.f64("center_to_face");
    let c = if given > 0.0 {
        given
    } else {
        spec.center_to_face().ok_or_else(|| {
            format!(
                "no catalogued center-to-face for {} {} fittings — pass `center_to_face`",
                spec.family(),
                spec.size()
            )
        })?
    };
    let min = spec.od() / 2.0 + spec.thickness() + 1.0;
    if c < min {
        return Err(format!(
            "center_to_face {c} mm is too short for {} {} (flanges collide below {min:.1} mm)",
            spec.family(),
            spec.size()
        ));
    }
    Ok(c)
}

fn multiway(p: &Params, names: &[&str]) -> Result<Document, String> {
    let spec = spec_of(p)?;
    let c = c2f(p, spec)?;
    let mut b = Builder::new();
    let list: Vec<(&str, f64)> = names.iter().map(|n| (*n, c)).collect();
    let root = arms(&mut b, spec, Detail::parse(&p.str("detail")), &list)?;
    Ok(b.finish(root, SS))
}

fn multiway_info(p: &Params, names: &[&str], what: &str) -> Result<PartInfo, String> {
    let spec = spec_of(p)?;
    let c = c2f(p, spec)?;
    let ports = names
        .iter()
        .map(|n| port_frame(spec, n, c).port(n, spec))
        .collect();
    let mut m = meta_of(spec);
    m["center_to_face_mm"] = json!(c);
    m["catalog_center_to_face"] = json!(p.f64("center_to_face") <= 0.0);
    m["bom"] = json!([bom(
        format!(
            "{} {} {what}, {c} mm center-to-face",
            spec.family(),
            spec.size()
        ),
        1
    )]);
    Ok(PartInfo { ports, metadata: m })
}

const TEE_PORTS: &[&str] = &["-z", "+z", "+x"];

/// `vacuum.tee`.
pub const TEE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.tee",
        name: "Vacuum tee",
        category: "Vacuum",
        description: Some(
            "Equal tee centred on the origin: run along Z (ports `-z`, `+z`), branch `+x`. \
             `center_to_face` 0 = catalog value.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: ALL_SIZES,
                default: "DN40",
            },
            Param::Length {
                name: "center_to_face",
                min: 0.0,
                max: 500.0,
                default: 0.0,
                unit: "mm",
            },
            DETAIL_PARAM,
        ],
        xrefs: CF_XREF,
        synonyms: &["tee", "3-way cross", "cf tee", "kf tee"],
        version: "1.0",
        thumb: THUMB,
    },
    build: |p| multiway(p, TEE_PORTS),
    info: Some(|p| multiway_info(p, TEE_PORTS, "tee")),
};

/// `vacuum.cross`.
pub const CROSS: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.cross",
        name: "Vacuum cross (4/6-way)",
        category: "Vacuum",
        description: Some(
            "Equal 4-way (ports ±z, ±x) or 6-way (adds ±y) cross centred on the origin. \
             `center_to_face` 0 = catalog value.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: ALL_SIZES,
                default: "DN40",
            },
            Param::Enum {
                name: "ways",
                values: &["4", "6"],
                default: "6",
            },
            Param::Length {
                name: "center_to_face",
                min: 0.0,
                max: 500.0,
                default: 0.0,
                unit: "mm",
            },
            DETAIL_PARAM,
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "6-way cross",
            "4-way cross",
            "six way cross",
            "cf cross",
            "kf cross",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: |p| multiway(p, cross_ports(p)),
    info: Some(|p| multiway_info(p, cross_ports(p), &format!("{}-way cross", p.str("ways")))),
};

fn cross_ports(p: &Params) -> &'static [&'static str] {
    if p.str("ways") == "4" {
        &["-z", "+z", "+x", "-x"]
    } else {
        &["-z", "+z", "+x", "-x", "+y", "-y"]
    }
}

// ---------------------------------------------------------------------------
// CF ↔ KF adapter
// ---------------------------------------------------------------------------

/// `vacuum.adapter.cf-kf`.
pub const ADAPTER_CF_KF: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.adapter.cf-kf",
        name: "CF to KF adapter",
        category: "Vacuum",
        description: Some(
            "Half nipple: CF flange (port `cf`, at the origin facing −Z) to KF flange (port `kf`, \
             facing +Z). `length` is face-to-face.",
        ),
        params: &[
            Param::Enum {
                name: "cf",
                values: CF_SIZES,
                default: "DN40",
            },
            Param::Enum {
                name: "kf",
                values: KF_SIZES,
                default: "KF40",
            },
            Param::Length {
                name: "length",
                min: 20.0,
                max: 500.0,
                default: 50.0,
                unit: "mm",
            },
            DETAIL_PARAM,
        ],
        xrefs: CF_XREF,
        synonyms: &["conflat to kf", "cf-kf adapter", "cf to nw adapter"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_adapter,
    info: Some(info_adapter),
};

fn adapter_specs(p: &Params) -> Result<(FlangeSpec, FlangeSpec, f64), String> {
    let c = FlangeSpec::Cf(cf(&p.str("cf"))?);
    let k = FlangeSpec::Kf(kf(&p.str("kf"))?);
    let l = p.f64("length");
    if l < c.thickness() + k.thickness() + 2.0 {
        return Err(format!("adapter: length {l} mm too short for both flanges"));
    }
    Ok((c, k, l))
}

fn adapter_frames(c: FlangeSpec, k: FlangeSpec, l: f64) -> (Frame, Frame) {
    (
        Frame::new([0.0; 3], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
        Frame::new(
            [0.0, 0.0, l + k.face_setback() + c.face_setback()],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
        ),
    )
}

fn build_adapter(p: &Params) -> Result<Document, String> {
    let (c, k, l) = adapter_specs(p)?;
    let (fc, fk) = adapter_frames(c, k, l);
    let mut b = Builder::new();
    let mut parts = Parts::with_detail(Detail::parse(&p.str("detail")));
    flange_at(&mut b, &mut parts, c, &fc, None);
    flange_at(&mut b, &mut parts, k, &fk, None);
    let r_out = c.tube_od().min(k.tube_od()) / 2.0;
    let r_in = c.bore().min(k.bore()) / 2.0;
    parts.solids.push(cyl_z(
        &mut b,
        r_out,
        c.thickness() * 0.5,
        l - k.thickness() * 0.5,
    ));
    parts.cutters.push(cyl_z(&mut b, r_in, -1.0, l + 5.0));
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_adapter(p: &Params) -> Result<PartInfo, String> {
    let (c, k, l) = adapter_specs(p)?;
    let (fc, fk) = adapter_frames(c, k, l);
    Ok(PartInfo {
        ports: vec![fc.port("cf", c), fk.port("kf", k)],
        metadata: json!({
            "cf": meta_of(c),
            "kf": meta_of(k),
            "face_to_face_mm": l,
            "bom": [bom(format!("{} CF to {} adapter, {l} mm", c.size(), k.size()), 1)],
        }),
    })
}
