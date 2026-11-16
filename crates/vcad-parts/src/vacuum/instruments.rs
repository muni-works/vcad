//! Flange-mounted instruments: viewports, feedthroughs, valves, gauges.
//!
//! Every part here has one mounting port at the origin facing +Z — the
//! vacuum side, toward whatever it bolts onto. Anything that lives in the
//! chamber (a feedthrough's conductor) extends along +Z; the air-side body
//! extends along −Z.
//!
//! Ratings (kV, A, V) are **catalog inputs**: the user enters the rating of
//! the part they are buying and the metadata repeats it. Geometry-derived
//! numbers (creepage along the modeled insulator) are labelled
//! `*_model` — they describe this envelope, not a vendor certification.

use super::fittings::ALL_SIZES;
use super::flanges::{bom, cf_meta, kf_meta};
use super::*;
use crate::types::{Param, Params, PartEntry, PartInfo, PartMetadata};
use serde_json::json;
use vcad_ir::Document;

fn mount() -> Frame {
    Frame::new([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0])
}

fn meta_of(spec: FlangeSpec) -> serde_json::Value {
    match spec {
        FlangeSpec::Cf(d) => cf_meta(d),
        FlangeSpec::Kf(d) => kf_meta(d),
    }
}

/// Mounting flange for an instrument with a central opening of `bore`.
fn mount_flange(b: &mut Builder, parts: &mut Parts, spec: FlangeSpec, bore: Option<f64>) {
    let opts = CfOpts {
        bore,
        tapped: false,
        rotatable: false,
        knife_edge: true,
    };
    flange_at(b, parts, spec, &mount(), Some(opts));
}

/// Back face (−Z) of the mounting flange.
fn back_z(spec: FlangeSpec) -> f64 {
    -(spec.face_setback() + spec.thickness())
}

// ---------------------------------------------------------------------------
// Viewport
// ---------------------------------------------------------------------------

/// `vacuum.viewport`.
pub const VIEWPORT: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.viewport",
        name: "Viewport (CF)",
        category: "Vacuum",
        description: Some(
            "CF viewport: flange with a sealed window on an air-side sleeve. Clear aperture from \
             the catalog. Port `seal` at the origin facing +Z.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: &["DN16", "DN40", "DN63", "DN100", "DN160"],
                default: "DN40",
            },
            Param::Enum {
                name: "glass",
                values: &["kodial", "fused-silica", "sapphire", "leaded-glass"],
                default: "kodial",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &["window", "view port", "cf window", "kodial viewport"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_viewport,
    info: Some(info_viewport),
};

fn window(d: &CfDims) -> Result<f64, String> {
    d.viewport_window
        .ok_or_else(|| format!("no catalogued viewport for {}", d.dn))
}

fn glass_t(w: f64) -> f64 {
    (0.08 * w).max(3.0)
}

fn build_viewport(p: &Params) -> Result<Document, String> {
    let d = cf(&p.str("size"))?;
    let w = window(d)?;
    let mut b = Builder::new();
    let mut parts = Parts::default();
    mount_flange(&mut b, &mut parts, FlangeSpec::Cf(d), Some(w));
    // Air-side sleeve carrying the window.
    let z0 = -d.thickness;
    let sleeve_l = 0.25 * w + 6.0;
    let r_o = (w / 2.0 + 3.0).min(d.bolt_circle / 2.0 - d.hole_clearance);
    parts
        .solids
        .push(tube_z(&mut b, r_o, w / 2.0, z0 - sleeve_l, z0 + 0.5));
    let gt = glass_t(w);
    parts.after.push(cyl_z(
        &mut b,
        w / 2.0 + 0.01,
        z0 - sleeve_l + 1.0,
        z0 - sleeve_l + 1.0 + gt,
    ));
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_viewport(p: &Params) -> Result<PartInfo, String> {
    let d = cf(&p.str("size"))?;
    let w = window(d)?;
    let mut m = cf_meta(d);
    m["clear_aperture_mm"] = json!(w);
    m["glass"] = json!(p.str("glass"));
    m["glass_thickness_mm_model"] = json!(glass_t(w));
    m["note"] =
        json!("glass blocks no neutrons and little hard x-ray; add a shutter for fusor work");
    m["bom"] = json!([bom(format!("{} CF viewport, {}", d.dn, p.str("glass")), 1)]);
    Ok(PartInfo {
        ports: vec![mount().port("seal", FlangeSpec::Cf(d))],
        metadata: m,
    })
}

// ---------------------------------------------------------------------------
// HV feedthrough
// ---------------------------------------------------------------------------

/// Creepage design rule used to size the modeled air-side insulator: 3 mm
/// per kV of surface path on clean, dry, indoor ceramic. A sizing heuristic
/// for the envelope, not a rating.
pub const HV_CREEPAGE_MM_PER_KV: f64 = 3.0;

/// Suggested air clearance from the HV terminal to grounded parts: 1 mm/kV
/// (≈ 3× margin on ~3 kV/mm breakdown of air), at least 25 mm.
pub const HV_KEEPOUT_MM_PER_KV: f64 = 1.0;

/// `vacuum.feedthrough.hv`.
pub const HV_FEEDTHROUGH: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.feedthrough.hv",
        name: "HV feedthrough (CF)",
        category: "Vacuum",
        description: Some(
            "High-voltage single-pin feedthrough on a CF flange: ceramic air-side insulator sized \
             for `rated_kv` (catalog rating you enter), terminal cap, and a conductor reaching \
             `vacuum_length` into the chamber along +Z. Metadata carries creepage and a suggested \
             HV keep-out for check_clearance.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: &["DN16", "DN40", "DN63"],
                default: "DN40",
            },
            Param::Number {
                name: "rated_kv",
                min: 1.0,
                max: 100.0,
                default: 40.0,
            },
            Param::Number {
                name: "rated_a",
                min: 0.01,
                max: 200.0,
                default: 5.0,
            },
            Param::Length {
                name: "vacuum_length",
                min: 5.0,
                max: 500.0,
                default: 100.0,
                unit: "mm",
            },
            Param::Length {
                name: "conductor_dia",
                min: 1.0,
                max: 20.0,
                default: 3.2,
                unit: "mm",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "high voltage feedthrough",
            "hv feedthrough",
            "40 kv feedthrough",
            "fusor feedthrough",
            "ceramic feedthrough",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_hv,
    info: Some(info_hv),
};

struct HvGeom {
    d: &'static CfDims,
    ins_r: f64,
    air_len: f64,
    vac_sleeve: f64,
    cap_r: f64,
    cap_h: f64,
    cond_r: f64,
    vac_len: f64,
}

fn hv_geom(p: &Params) -> Result<HvGeom, String> {
    let d = cf(&p.str("size"))?;
    let kv = p.f64("rated_kv");
    let ins_r = (d.gasket_id / 2.0 - 2.0).max(4.0);
    let cond_r = p.f64("conductor_dia") / 2.0;
    if cond_r >= ins_r - 1.0 {
        return Err(format!(
            "hv feedthrough: {} mm conductor does not fit a {} insulator",
            2.0 * cond_r,
            d.dn
        ));
    }
    Ok(HvGeom {
        d,
        ins_r,
        air_len: (kv * HV_CREEPAGE_MM_PER_KV).max(20.0),
        vac_sleeve: (0.25 * kv).clamp(8.0, 30.0),
        cap_r: (2.0 * cond_r + 3.0).min(ins_r),
        cap_h: 8.0,
        cond_r,
        vac_len: p.f64("vacuum_length"),
    })
}

fn build_hv(p: &Params) -> Result<Document, String> {
    let g = hv_geom(p)?;
    let d = g.d;
    let mut b = Builder::new();
    let mut parts = Parts::default();
    mount_flange(
        &mut b,
        &mut parts,
        FlangeSpec::Cf(d),
        Some(2.0 * g.ins_r - 0.2),
    );
    let z_back = -d.thickness;
    // Air-side insulator (ceramic), through the flange, plus vacuum sleeve.
    let ins = cyl_z(
        &mut b,
        g.ins_r,
        z_back - g.air_len,
        g.vac_sleeve.min(g.vac_len),
    );
    parts.after.push(ins);
    // Terminal cap on the air end, conductor through the middle.
    let cap = cyl_z(
        &mut b,
        g.cap_r,
        z_back - g.air_len - g.cap_h,
        z_back - g.air_len + 0.01,
    );
    parts.after.push(cap);
    let rod = cyl_z(&mut b, g.cond_r, z_back - g.air_len - 0.5, g.vac_len);
    parts.after.push(rod);
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_hv(p: &Params) -> Result<PartInfo, String> {
    let g = hv_geom(p)?;
    let d = g.d;
    let kv = p.f64("rated_kv");
    // Surface path from flange to cap along the insulator's air side, plus
    // its end face out to the cap.
    let creep_air = g.air_len + (g.ins_r - g.cap_r).max(0.0);
    let creep_vac = g.vac_sleeve.min(g.vac_len) + (g.ins_r - g.cond_r);
    let keepout = (kv * HV_KEEPOUT_MM_PER_KV).max(25.0);
    let terminal_z = -d.thickness - g.air_len - g.cap_h;
    Ok(PartInfo {
        ports: vec![mount().port("seal", FlangeSpec::Cf(d))],
        metadata: json!({
            "flange": cf_meta(d),
            "rated_kv": kv,
            "rated_a": p.f64("rated_a"),
            "rating_provenance": "catalog input entered by the user — not derived from geometry",
            "creepage_air_mm_model": creep_air,
            "creepage_vacuum_mm_model": creep_vac,
            "creepage_rule_mm_per_kv": HV_CREEPAGE_MM_PER_KV,
            "creepage_rule": "air-side insulator sized at 3 mm/kV (clean, dry, indoor ceramic heuristic)",
            "insulator_od_mm": 2.0 * g.ins_r,
            "air_side_length_mm": g.air_len + g.cap_h,
            "terminal_position_mm": [0.0, 0.0, terminal_z],
            "conductor_tip_mm": [0.0, 0.0, g.vac_len],
            "conductor_dia_mm": 2.0 * g.cond_r,
            "suggested_clearance": [{
                "label": "hv-terminal-air",
                "min_mm": keepout,
                "between": "HV terminal / air-side insulator and any grounded or operator-reachable part",
                "rule": "1 mm/kV, min 25 mm (≈3× margin on air breakdown)",
            }],
            "bom": [bom(format!("{} CF HV feedthrough, {kv} kV / {} A", d.dn, p.f64("rated_a")), 1)],
        }),
    })
}

// ---------------------------------------------------------------------------
// Electrical / thermocouple feedthrough
// ---------------------------------------------------------------------------

/// `vacuum.feedthrough.electrical`.
pub const ELECTRICAL_FEEDTHROUGH: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.feedthrough.electrical",
        name: "Electrical feedthrough (CF)",
        category: "Vacuum",
        description: Some(
            "Multi-pin power or thermocouple feedthrough on a CF flange: pins on a circle through a \
             ceramic disc, protruding both sides. Ratings are catalog inputs.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: &["DN16", "DN40", "DN63"],
                default: "DN40",
            },
            Param::Enum {
                name: "kind",
                values: &["power", "thermocouple-k", "thermocouple-j", "instrumentation"],
                default: "power",
            },
            Param::Integer {
                name: "pins",
                min: 1,
                max: 32,
                default: 4,
            },
            Param::Number {
                name: "pin_rated_a",
                min: 0.01,
                max: 200.0,
                default: 5.0,
            },
            Param::Number {
                name: "rated_v",
                min: 1.0,
                max: 20000.0,
                default: 500.0,
            },
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "power feedthrough",
            "thermocouple feedthrough",
            "tc feedthrough",
            "multipin feedthrough",
            "instrumentation feedthrough",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_elec,
    info: Some(info_elec),
};

fn elec_geom(p: &Params) -> Result<(&'static CfDims, u32, f64, f64, f64), String> {
    let d = cf(&p.str("size"))?;
    let mut pins = p.i64("pins").max(1) as u32;
    if p.str("kind").starts_with("thermocouple") && pins % 2 == 1 {
        pins += 1; // thermocouples come in pairs
    }
    let disc_r = d.gasket_id / 2.0 - 1.5;
    let pin_r = if p.str("kind") == "power" { 1.2 } else { 0.8 };
    let ring = if pins == 1 { 0.0 } else { 0.55 * disc_r };
    let spacing = if pins == 1 {
        f64::INFINITY
    } else {
        std::f64::consts::TAU * ring / pins as f64
    };
    if spacing < 4.0 * pin_r {
        return Err(format!(
            "{pins} pins do not fit a {} feedthrough (pitch {spacing:.1} mm)",
            d.dn
        ));
    }
    Ok((d, pins, disc_r, pin_r, ring))
}

fn build_elec(p: &Params) -> Result<Document, String> {
    let (d, pins, disc_r, pin_r, ring) = elec_geom(p)?;
    let mut b = Builder::new();
    let mut parts = Parts::default();
    mount_flange(&mut b, &mut parts, FlangeSpec::Cf(d), Some(2.0 * disc_r));
    let z_back = -d.thickness;
    parts.after.push(cyl_z(&mut b, disc_r, z_back - 6.0, 2.0));
    for k in 0..pins {
        let th = std::f64::consts::TAU * k as f64 / pins as f64;
        let pin = cyl_z(&mut b, pin_r, z_back - 26.0, 30.0);
        parts
            .after
            .push(b.translate(pin, ring * th.cos(), ring * th.sin(), 0.0));
    }
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_elec(p: &Params) -> Result<PartInfo, String> {
    let (d, pins, _, pin_r, _) = elec_geom(p)?;
    let kind = p.str("kind");
    let tc = match kind.as_str() {
        "thermocouple-k" => {
            Some(json!({ "type": "K", "conductors": ["chromel", "alumel"], "pairs": pins / 2 }))
        }
        "thermocouple-j" => {
            Some(json!({ "type": "J", "conductors": ["iron", "constantan"], "pairs": pins / 2 }))
        }
        _ => None,
    };
    Ok(PartInfo {
        ports: vec![mount().port("seal", FlangeSpec::Cf(d))],
        metadata: json!({
            "flange": cf_meta(d),
            "kind": kind,
            "pins": pins,
            "pin_dia_mm": 2.0 * pin_r,
            "pin_rated_a": p.f64("pin_rated_a"),
            "rated_v": p.f64("rated_v"),
            "rating_provenance": "catalog input entered by the user — not derived from geometry",
            "thermocouple": tc,
            "bom": [bom(format!("{} CF {kind} feedthrough, {pins} pin", d.dn), 1)],
        }),
    })
}

// ---------------------------------------------------------------------------
// Needle / leak valve
// ---------------------------------------------------------------------------

/// `vacuum.valve.needle`.
pub const NEEDLE_VALVE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.valve.needle",
        name: "Needle / leak valve",
        category: "Vacuum",
        description: Some(
            "Envelope of a needle (metering) or UHV leak valve: mounting flange (port `outlet`, \
             facing +Z), body, adjustment knob on −Z and a 1/4\" gas inlet stub (port-free) on +X. \
             For clearance and layout — not a machined part.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: ALL_SIZES,
                default: "DN16",
            },
            Param::Enum {
                name: "kind",
                values: &["needle", "leak"],
                default: "leak",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "leak valve",
            "needle valve",
            "variable leak valve",
            "metering valve",
            "gas inlet valve",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_valve,
    info: Some(info_valve),
};

struct ValveGeom {
    w: f64,
    h: f64,
    knob_r: f64,
    knob_h: f64,
    stub: f64,
}

fn valve_geom(spec: FlangeSpec, kind: &str) -> ValveGeom {
    let scale_k = if kind == "leak" { 1.4 } else { 1.0 };
    let w = (spec.od() * 1.1).max(40.0) * scale_k;
    ValveGeom {
        w,
        h: 1.2 * w,
        knob_r: 0.4 * w,
        knob_h: 0.5 * w,
        stub: 0.5 * w,
    }
}

fn build_valve(p: &Params) -> Result<Document, String> {
    let spec = FlangeSpec::parse("", &p.str("size"))?;
    let g = valve_geom(spec, &p.str("kind"));
    let mut b = Builder::new();
    let mut parts = Parts::default();
    mount_flange(&mut b, &mut parts, spec, Some(spec.bore()));
    let zb = back_z(spec);
    let body = b.cube(g.w, g.w, g.h);
    parts
        .solids
        .push(b.translate(body, -g.w / 2.0, -g.w / 2.0, zb - g.h + 0.5));
    parts
        .solids
        .push(cyl_z(&mut b, g.knob_r, zb - g.h - g.knob_h, zb - g.h + 0.6));
    let stub = rod_between(
        &mut b,
        3.175,
        [g.w / 2.0 - 1.0, 0.0, zb - g.h / 2.0],
        [g.w / 2.0 + g.stub, 0.0, zb - g.h / 2.0],
    );
    parts.solids.push(stub);
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_valve(p: &Params) -> Result<PartInfo, String> {
    let spec = FlangeSpec::parse("", &p.str("size"))?;
    let kind = p.str("kind");
    let g = valve_geom(spec, &kind);
    let zb = back_z(spec);
    Ok(PartInfo {
        ports: vec![mount().port("outlet", spec)],
        metadata: json!({
            "flange": meta_of(spec),
            "kind": kind,
            "envelope": true,
            "envelope_mm": {
                "body": [g.w, g.w, g.h],
                "knob_dia": 2.0 * g.knob_r,
                "overall_length": -zb + g.h + g.knob_h,
            },
            "gas_inlet": { "type": "1/4\" tube stub", "position_mm": [g.w / 2.0 + g.stub, 0.0, zb - g.h / 2.0] },
            "provenance": "envelope approximation scaled from the flange; confirm against the vendor drawing",
            "bom": [bom(format!("{} {kind} valve, {} {}", spec.size(), spec.family(), "mount"), 1)],
        }),
    })
}

// ---------------------------------------------------------------------------
// Gauge
// ---------------------------------------------------------------------------

/// `vacuum.gauge`.
pub const GAUGE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.gauge",
        name: "Vacuum gauge",
        category: "Vacuum",
        description: Some(
            "Envelope of a vacuum gauge head (Pirani, cold-cathode, capacitance manometer, ion) on a \
             KF or CF port: port `inlet` facing +Z, body on −Z. Ranges are typical and labelled so.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: ALL_SIZES,
                default: "KF25",
            },
            Param::Enum {
                name: "kind",
                values: &["pirani", "cold-cathode", "capacitance-manometer", "ion"],
                default: "pirani",
            },
        ],
        xrefs: KF_XREF,
        synonyms: &["pirani gauge", "cold cathode gauge", "baratron", "capacitance manometer", "ion gauge", "vacuum gauge"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_gauge,
    info: Some(info_gauge),
};

/// (body dia, body length, typical range Torr) per kind — envelopes.
fn gauge_env(kind: &str) -> (f64, f64, [f64; 2]) {
    match kind {
        "cold-cathode" => (62.0, 110.0, [1e-9, 1e-2]),
        "capacitance-manometer" => (70.0, 90.0, [1e-4, 1e1]),
        "ion" => (55.0, 120.0, [1e-10, 1e-3]),
        _ => (40.0, 100.0, [1e-4, 760.0]),
    }
}

fn build_gauge(p: &Params) -> Result<Document, String> {
    let spec = FlangeSpec::parse("", &p.str("size"))?;
    let (dia, len, _) = gauge_env(&p.str("kind"));
    let mut b = Builder::new();
    let mut parts = Parts::default();
    mount_flange(&mut b, &mut parts, spec, Some(spec.bore()));
    let zb = back_z(spec);
    let neck_l = 15.0;
    parts
        .solids
        .push(cyl_z(&mut b, spec.tube_od() / 2.0, zb - neck_l, zb + 0.5));
    parts.solids.push(cyl_z(
        &mut b,
        dia / 2.0,
        zb - neck_l - len,
        zb - neck_l + 0.5,
    ));
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_gauge(p: &Params) -> Result<PartInfo, String> {
    let spec = FlangeSpec::parse("", &p.str("size"))?;
    let kind = p.str("kind");
    let (dia, len, range) = gauge_env(&kind);
    Ok(PartInfo {
        ports: vec![mount().port("inlet", spec)],
        metadata: json!({
            "flange": meta_of(spec),
            "kind": kind,
            "envelope": true,
            "envelope_mm": { "body_dia": dia, "body_length": len, "overall_length": -back_z(spec) + 15.0 + len },
            "range_torr_typical": range,
            "provenance": "envelope + typical range for the gauge type; confirm against the chosen head",
            "bom": [bom(format!("{kind} gauge head, {} {}", spec.family(), spec.size()), 1)],
        }),
    })
}
