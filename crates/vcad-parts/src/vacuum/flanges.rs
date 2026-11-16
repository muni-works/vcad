//! Flanges and their consumables: CF flanges, copper gaskets, bolt sets,
//! zero-length reducers; KF flanges, centering rings, clamps.

use super::*;
use crate::types::{Param, Params, PartEntry, PartInfo, PartMetadata, PortInfo};
use serde_json::json;
use vcad_ir::Document;

const ORIGIN_UP: [f64; 3] = [1.0, 0.0, 0.0];
const PLUS_Z: [f64; 3] = [0.0, 0.0, 1.0];

fn seal_frame() -> Frame {
    Frame::new([0.0; 3], PLUS_Z, ORIGIN_UP)
}

/// Standard metadata block for one CF row.
pub(crate) fn cf_meta(d: &CfDims) -> serde_json::Value {
    json!({
        "family": "CF",
        "size": d.dn,
        "size_inch": d.inch,
        "standard": "ISO 3669",
        "od_mm": d.od,
        "thickness_mm": d.thickness,
        "thickness_rotatable_mm": d.thickness_rotatable,
        "bolt_circle_mm": d.bolt_circle,
        "bolt_count": d.bolt_count,
        "thread": d.thread,
        "hole_clearance_mm": d.hole_clearance,
        "hole_tap_drill_mm": d.hole_tapped(),
        "gasket": { "od_mm": d.gasket_od, "id_mm": d.gasket_id, "thickness_mm": d.gasket_thickness },
        "tube_mm": { "od": d.tube_od, "wall": d.tube_wall, "bore": d.bore() },
        "knife_edge_dia_mm": d.knife_edge_dia(),
        "source": "Metallicflex CF Flanschbauteile catalog (CF-01…CF-024); bolt circles cross-checked against vacgen",
    })
}

/// Standard metadata block for one KF row.
pub(crate) fn kf_meta(d: &KfDims) -> serde_json::Value {
    json!({
        "family": "KF",
        "size": d.name,
        "standard": "ISO 2861-1 / DIN 28403",
        "od_mm": d.od,
        "bore_mm": d.bore,
        "flange_height_mm": kf_flange_height(d),
        "taper_deg": 15.0,
        "seal_gap_mm": KF_SEAL_GAP,
        "source": "MKS Series 31 ISO-KF catalog (Fig. 1, clamp and tee tables); centering ring O-ring per Marco Rubber",
        "provenance": "OD, bore, taper, clamp envelope: catalog. Rim thickness and assembled seal gap: envelope approximation (not fixed by ISO 2861).",
    })
}

/// BOM line JSON.
pub(crate) fn bom(spec: impl Into<String>, qty: u32) -> serde_json::Value {
    json!({ "spec": spec.into(), "qty": qty })
}

/// Hardware consumed by one flange joint of `spec` (both sides through-hole
/// unless `tapped_side`): gasket + bolts/nuts for CF; ring + clamp for KF.
pub fn joint_hardware(spec: FlangeSpec, tapped_side: bool) -> Vec<(String, u32)> {
    match spec {
        FlangeSpec::Cf(d) => {
            let len = bolt_length(d, tapped_side);
            let mut v = vec![
                (format!("{} CF copper gasket (OFHC)", d.dn), 1),
                (
                    format!("{}x{} 12-point / hex bolt, silver-plated", d.thread, len),
                    d.bolt_count,
                ),
            ];
            if !tapped_side {
                v.push((format!("{} hex nut, plate", d.thread), d.bolt_count));
            }
            v.push((format!("{} flat washer", d.thread), d.bolt_count * 2));
            v
        }
        FlangeSpec::Kf(d) => vec![
            (format!("{} centering ring, SS + FKM O-ring", d.name), 1),
            (format!("{} wing-nut clamp", d.name), 1),
        ],
    }
}

/// Recommended bolt length for a CF joint: two flange thicknesses (faces
/// meet), two washers, a nut and two thread pitches — rounded up to the
/// next stocked length. A tapped mate needs only one flange + 1.5 d of
/// engagement.
pub fn bolt_length(d: &CfDims, tapped_side: bool) -> u32 {
    let dia = crate::fasteners::metric_major_dia(d.thread);
    let washer = 0.18 * dia; // ISO 7089 h ≈ 0.16–0.2 d
    let nut = crate::fasteners::nut_thickness(d.thread);
    let need = if tapped_side {
        d.thickness + washer + 1.5 * dia
    } else {
        2.0 * d.thickness + 2.0 * washer + nut + 2.0 * 0.15 * dia
    };
    const STOCK: &[u32] = &[
        10, 12, 16, 20, 25, 30, 35, 40, 45, 50, 55, 60, 65, 70, 80, 90, 100,
    ];
    STOCK
        .iter()
        .copied()
        .find(|&l| l as f64 >= need)
        .unwrap_or(100)
}

// ---------------------------------------------------------------------------
// CF flange
// ---------------------------------------------------------------------------

/// `vacuum.cf.flange`.
pub const CF_FLANGE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.cf.flange",
        name: "CF flange (ConFlat)",
        category: "Vacuum",
        description: Some(
            "ISO 3669 knife-edge flange, DN16–DN160: blank or bored, through or tapped holes, \
             fixed or rotatable. Sealing face at z = 0, port `seal` faces +Z.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: CF_SIZES,
                default: "DN40",
            },
            Param::Enum {
                name: "style",
                values: &["blank", "bored"],
                default: "blank",
            },
            Param::Enum {
                name: "holes",
                values: &["through", "tapped"],
                default: "through",
            },
            Param::Boolean {
                name: "rotatable",
                default: false,
            },
            Param::Boolean {
                name: "knife_edge",
                default: true,
            },
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "conflat",
            "cf flange",
            "blank flange",
            "knife edge flange",
            "1.33 cf",
            "2.75 cf",
            "4.5 cf",
            "6 cf",
            "8 cf",
            "uhv flange",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_cf_flange,
    info: Some(info_cf_flange),
};

fn cf_opts(p: &Params, d: &CfDims) -> CfOpts {
    CfOpts {
        bore: (p.str("style") == "bored").then(|| d.bore()),
        tapped: p.str("holes") == "tapped",
        rotatable: p.bool("rotatable"),
        knife_edge: p.bool("knife_edge"),
    }
}

fn build_cf_flange(p: &Params) -> Result<Document, String> {
    let d = cf(&p.str("size"))?;
    let mut b = Builder::new();
    let mut parts = Parts::default();
    flange_at(
        &mut b,
        &mut parts,
        FlangeSpec::Cf(d),
        &seal_frame(),
        Some(cf_opts(p, d)),
    );
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_cf_flange(p: &Params) -> Result<PartInfo, String> {
    let d = cf(&p.str("size"))?;
    let o = cf_opts(p, d);
    let mut m = cf_meta(d);
    m["style"] = json!(p.str("style"));
    m["holes"] = json!(p.str("holes"));
    m["rotatable"] = json!(o.rotatable);
    m["bakeable_c_typical"] = json!(450);
    m["bom"] = json!([bom(
        format!(
            "{} CF {} flange, {}{}",
            d.dn,
            p.str("style"),
            if o.tapped { "tapped" } else { "through-hole" },
            if o.rotatable { ", rotatable" } else { "" }
        ),
        1
    )]);
    Ok(PartInfo {
        ports: vec![PortInfo {
            tapped: o.tapped,
            ..seal_frame().port("seal", FlangeSpec::Cf(d))
        }],
        metadata: m,
    })
}

// ---------------------------------------------------------------------------
// Copper gasket
// ---------------------------------------------------------------------------

/// `vacuum.cf.gasket`.
pub const CF_GASKET: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.cf.gasket",
        name: "CF gasket (copper)",
        category: "Vacuum",
        description: Some(
            "OFHC copper (or annealed / FKM) CF gasket, centred on z = 0 so it lands in the recesses \
             of two mated flanges.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: CF_SIZES,
                default: "DN40",
            },
            Param::Enum {
                name: "material",
                values: &["copper-ofhc", "copper-annealed", "fkm"],
                default: "copper-ofhc",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &["copper gasket", "cf gasket", "conflat gasket", "ofhc gasket"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_cf_gasket,
    info: Some(info_cf_gasket),
};

fn build_cf_gasket(p: &Params) -> Result<Document, String> {
    let d = cf(&p.str("size"))?;
    let mut b = Builder::new();
    let h = d.gasket_thickness;
    let g = tube_z(
        &mut b,
        d.gasket_od / 2.0,
        d.gasket_id / 2.0,
        -h / 2.0,
        h / 2.0,
    );
    let mat = if p.str("material") == "fkm" {
        "rubber"
    } else {
        "copper"
    };
    Ok(b.finish(g, mat))
}

fn info_cf_gasket(p: &Params) -> Result<PartInfo, String> {
    let d = cf(&p.str("size"))?;
    let mat = p.str("material");
    Ok(PartInfo {
        ports: vec![seal_frame().port("seal", FlangeSpec::Cf(d))],
        metadata: json!({
            "family": "CF",
            "size": d.dn,
            "od_mm": d.gasket_od,
            "id_mm": d.gasket_id,
            "thickness_mm": d.gasket_thickness,
            "material": mat,
            "single_use": mat != "fkm",
            "source": "Metallicflex CF-023 copper gasket table",
            "bom": [bom(format!("{} CF gasket, {mat}", d.dn), 1)],
        }),
    })
}

// ---------------------------------------------------------------------------
// Bolt set
// ---------------------------------------------------------------------------

/// `vacuum.cf.bolt-set`.
pub const CF_BOLT_SET: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.cf.bolt-set",
        name: "CF bolt set",
        category: "Vacuum",
        description: Some(
            "Bolts (and nuts) for one CF joint, positioned on the bolt circle of two mated flanges \
             whose shared face is z = 0; length is chosen from the stack and reported.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: CF_SIZES,
                default: "DN40",
            },
            Param::Enum {
                name: "mate",
                values: &["through", "tapped"],
                default: "through",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &["cf bolts", "conflat hardware", "flange bolt kit"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_cf_bolt_set,
    info: Some(info_cf_bolt_set),
};

fn build_cf_bolt_set(p: &Params) -> Result<Document, String> {
    let d = cf(&p.str("size"))?;
    let tapped = p.str("mate") == "tapped";
    let len = bolt_length(d, tapped) as f64;
    let dia = crate::fasteners::metric_major_dia(d.thread);
    let head_h = 0.7 * dia;
    let head_r = 0.85 * dia;
    let nut_t = crate::fasteners::nut_thickness(d.thread);
    let nut_r = crate::fasteners::nut_across_flats(d.thread) / 2.0 / 0.866_025_403_784_438_7;
    let t = d.thickness;
    let mut b = Builder::new();
    let mut ids = Vec::new();
    for [x, y] in cf_bolt_positions(d) {
        // Head bears on the +Z flange's back face (z = t); shank runs to −Z.
        let head = cyl_z(&mut b, head_r, t, t + head_h);
        let shank = cyl_z(&mut b, dia / 2.0, t - len, t + 0.01);
        let mut bolt = b.union(head, shank);
        if !tapped {
            let n = b.cylinder_segments(nut_r, nut_t, 6);
            let n = b.translate(n, 0.0, 0.0, -t - nut_t);
            bolt = b.union(bolt, n);
        }
        ids.push(b.translate(bolt, x, y, 0.0));
    }
    let root = union_all(&mut b, &ids).ok_or("no bolts")?;
    Ok(b.finish(root, SS))
}

fn info_cf_bolt_set(p: &Params) -> Result<PartInfo, String> {
    let d = cf(&p.str("size"))?;
    let tapped = p.str("mate") == "tapped";
    let lines: Vec<_> = joint_hardware(FlangeSpec::Cf(d), tapped)
        .into_iter()
        .filter(|(s, _)| !s.contains("gasket"))
        .map(|(s, q)| bom(s, q))
        .collect();
    Ok(PartInfo {
        ports: vec![seal_frame().port("seal", FlangeSpec::Cf(d))],
        metadata: json!({
            "family": "CF",
            "size": d.dn,
            "thread": d.thread,
            "count": d.bolt_count,
            "length_mm": bolt_length(d, tapped),
            "mate": p.str("mate"),
            "practice": "tighten in a star pattern in several passes until the flange faces meet",
            "bom": lines,
        }),
    })
}

// ---------------------------------------------------------------------------
// Zero-length reducer
// ---------------------------------------------------------------------------

/// `vacuum.cf.reducer`.
pub const CF_REDUCER: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.cf.reducer",
        name: "CF zero-length reducer",
        category: "Vacuum",
        description: Some(
            "Zero-length reducer flange: a large CF flange (port `large`, faces −Z) carrying a \
             tapped knife edge for a smaller CF size on its back (port `small`, faces +Z).",
        ),
        params: &[
            Param::Enum {
                name: "large",
                values: CF_SIZES,
                default: "DN100",
            },
            Param::Enum {
                name: "small",
                values: CF_SIZES,
                default: "DN40",
            },
        ],
        xrefs: CF_XREF,
        synonyms: &[
            "zero length reducer",
            "reducing flange",
            "cf adapter flange",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_cf_reducer,
    info: Some(info_cf_reducer),
};

fn reducer_dims(p: &Params) -> Result<(&'static CfDims, &'static CfDims), String> {
    let l = cf(&p.str("large"))?;
    let s = cf(&p.str("small"))?;
    if s.od >= l.od {
        return Err(format!(
            "reducer: {} is not smaller than {} — swap `large` and `small`",
            s.dn, l.dn
        ));
    }
    Ok((l, s))
}

/// Thickness of a zero-length reducer: the large flange, deep enough for the
/// small side's tapped holes (1.5 d engagement) behind its own recess.
fn reducer_thickness(l: &CfDims, s: &CfDims) -> f64 {
    let dia = crate::fasteners::metric_major_dia(s.thread);
    l.thickness
        .max(l.recess_depth() + s.recess_depth() + 1.5 * dia + 3.0)
}

fn build_cf_reducer(p: &Params) -> Result<Document, String> {
    let (l, s) = reducer_dims(p)?;
    let t = reducer_thickness(l, s);
    let mut b = Builder::new();
    // Large side: an ordinary flange (through bolt holes, bored to the small
    // size) facing −Z, its body occupying z ∈ [0, T_large].
    let large = Frame::new([0.0; 3], [0.0, 0.0, -1.0], ORIGIN_UP);
    let lo = CfOpts {
        bore: Some(s.bore()),
        tapped: false,
        rotatable: false,
        knife_edge: true,
    };
    let fl = cf_flange_local(&mut b, l, &lo);
    let mut body = place(&mut b, fl, &large);
    if t > l.thickness + 1e-9 {
        // Raised boss inside the large bolt circle for deep tapped holes.
        let r = l.bolt_circle / 2.0 - l.hole_clearance / 2.0 - 1.0;
        let boss = tube_z(&mut b, r, s.bore() / 2.0, l.thickness - 0.5, t);
        body = b.union(body, boss);
    }
    // Small side at z = t facing +Z: recess, blind tapped holes, knife edge.
    let depth = s.recess_depth();
    let dia = crate::fasteners::metric_major_dia(s.thread);
    let hole_depth = 1.5 * dia + 1.0;
    let mut cuts = vec![tube_z(
        &mut b,
        s.gasket_od / 2.0 + 0.1,
        s.bore() / 2.0 - 0.01,
        t - depth,
        t + 1.0,
    )];
    for [x, y] in cf_bolt_positions(s) {
        let h = cyl_z(&mut b, s.hole_tapped() / 2.0, t - hole_depth, t + 1.0);
        cuts.push(b.translate(h, x, y, 0.0));
    }
    let cut = union_all(&mut b, &cuts).ok_or("reducer: no cuts")?;
    body = b.difference(body, cut);
    let r = s.knife_edge_dia() / 2.0;
    let z0 = t - depth - 0.05;
    let disc = cyl_z(&mut b, r, z0, t - depth + 0.3);
    let flank = b.cone(r - 0.6, r + 0.001, 0.35);
    let flank = b.translate(flank, 0.0, 0.0, z0);
    let ring = b.difference(disc, flank);
    let root = b.union(body, ring);
    Ok(b.finish(root, SS))
}

fn info_cf_reducer(p: &Params) -> Result<PartInfo, String> {
    let (l, s) = reducer_dims(p)?;
    let t = reducer_thickness(l, s);
    let large = Frame::new([0.0; 3], [0.0, 0.0, -1.0], ORIGIN_UP);
    let small = Frame::new([0.0, 0.0, t], PLUS_Z, ORIGIN_UP);
    Ok(PartInfo {
        ports: vec![
            large.port("large", FlangeSpec::Cf(l)),
            PortInfo {
                tapped: true,
                ..small.port("small", FlangeSpec::Cf(s))
            },
        ],
        metadata: json!({
            "family": "CF",
            "large": cf_meta(l),
            "small": cf_meta(s),
            "small_holes": "tapped (blind)",
            "thickness_mm": t,
            "bom": [bom(format!("{} to {} CF zero-length reducer, tapped", l.dn, s.dn), 1)],
        }),
    })
}

// ---------------------------------------------------------------------------
// KF flange
// ---------------------------------------------------------------------------

/// `vacuum.kf.flange`.
pub const KF_FLANGE: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.kf.flange",
        name: "KF flange (ISO-KF)",
        category: "Vacuum",
        description: Some(
            "ISO 2861 quick-flange, KF16–KF50: blank-off or weld stub. Face at z = −2 \
             (half the centering-ring gap), port `seal` at z = 0 facing +Z.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: KF_SIZES,
                default: "KF25",
            },
            Param::Enum {
                name: "style",
                values: &["blank", "weld-stub"],
                default: "blank",
            },
            Param::Length {
                name: "stub_length",
                min: 5.0,
                max: 200.0,
                default: 13.0,
                unit: "mm",
            },
        ],
        xrefs: KF_XREF,
        synonyms: &[
            "nw flange",
            "qf flange",
            "iso-kf",
            "kf blank",
            "quick flange",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_kf_flange,
    info: Some(info_kf_flange),
};

fn build_kf_flange(p: &Params) -> Result<Document, String> {
    let d = kf(&p.str("size"))?;
    let stub = p.str("style") == "weld-stub";
    let mut b = Builder::new();
    let back = -KF_SEAL_GAP / 2.0;
    let h = kf_flange_height(d);
    let (solid, cutter) = kf_flange_local(&mut b, d, stub.then(|| FlangeSpec::Kf(d).bore()));
    let mut parts = Parts::default();
    parts.solids.push(b.translate(solid, 0.0, 0.0, back));
    if let Some(c) = cutter {
        parts.cutters.push(b.translate(c, 0.0, 0.0, back));
    }
    if stub {
        let l = p.f64("stub_length");
        let z1 = back - h + 0.5;
        let r_in = FlangeSpec::Kf(d).bore() / 2.0;
        parts
            .solids
            .push(tube_z(&mut b, d.tube_od / 2.0, r_in, z1 - l, z1));
        parts.cutters.push(cyl_z(&mut b, r_in, z1 - l - 1.0, 1.0));
    }
    let root = parts.assemble(&mut b)?;
    Ok(b.finish(root, SS))
}

fn info_kf_flange(p: &Params) -> Result<PartInfo, String> {
    let d = kf(&p.str("size"))?;
    let mut m = kf_meta(d);
    m["style"] = json!(p.str("style"));
    m["bom"] = json!([bom(format!("{} {} flange", d.name, p.str("style")), 1)]);
    Ok(PartInfo {
        ports: vec![seal_frame().port("seal", FlangeSpec::Kf(d))],
        metadata: m,
    })
}

// ---------------------------------------------------------------------------
// KF centering ring
// ---------------------------------------------------------------------------

/// `vacuum.kf.centering-ring`.
pub const KF_CENTERING_RING: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.kf.centering-ring",
        name: "KF centering ring",
        category: "Vacuum",
        description: Some(
            "Metal centering ring + O-ring for a KF joint, centred on the seal mid-plane z = 0.",
        ),
        params: &[
            Param::Enum {
                name: "size",
                values: KF_SIZES,
                default: "KF25",
            },
            Param::Enum {
                name: "oring",
                values: &["fkm", "nbr", "silicone"],
                default: "fkm",
            },
        ],
        xrefs: KF_XREF,
        synonyms: &[
            "centering ring",
            "centring ring",
            "kf o-ring",
            "nw centering ring",
        ],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_kf_ring,
    info: Some(info_kf_ring),
};

fn build_kf_ring(p: &Params) -> Result<Document, String> {
    let d = kf(&p.str("size"))?;
    let mut b = Builder::new();
    let g = KF_SEAL_GAP;
    let r_o = d.oring_id / 2.0;
    // Metal carrier inside the O-ring, O-ring outside it (envelope).
    let carrier = tube_z(&mut b, r_o, d.bore / 2.0, -g / 2.0, g / 2.0);
    let oring = tube_z(&mut b, r_o + KF_ORING_CS, r_o - 0.01, -g / 2.0, g / 2.0);
    let root = b.union(carrier, oring);
    Ok(b.finish(root, SS))
}

fn info_kf_ring(p: &Params) -> Result<PartInfo, String> {
    let d = kf(&p.str("size"))?;
    Ok(PartInfo {
        ports: vec![seal_frame().port("seal", FlangeSpec::Kf(d))],
        metadata: json!({
            "family": "KF",
            "size": d.name,
            "oring_id_mm": d.oring_id,
            "oring_cs_mm": KF_ORING_CS,
            "oring": p.str("oring"),
            "assembled_gap_mm": KF_SEAL_GAP,
            "provenance": "O-ring ID and cross-section: Marco Rubber table; carrier thickness: envelope",
            "bom": [bom(format!("{} centering ring, {} O-ring", d.name, p.str("oring")), 1)],
        }),
    })
}

// ---------------------------------------------------------------------------
// KF clamp
// ---------------------------------------------------------------------------

/// `vacuum.kf.clamp`.
pub const KF_CLAMP: PartEntry = PartEntry {
    meta: PartMetadata {
        id: "vacuum.kf.clamp",
        name: "KF clamp (wing nut)",
        category: "Vacuum",
        description: Some(
            "Hinged wing-nut clamp envelope for a KF joint, centred on the seal mid-plane; the wing \
             nut points along +X. Use it as a clearance keep-out, not a machined part.",
        ),
        params: &[Param::Enum {
            name: "size",
            values: KF_SIZES,
            default: "KF25",
        }],
        xrefs: KF_XREF,
        synonyms: &["wing nut clamp", "kf clamp", "nw clamp", "hinged clamp"],
        version: "1.0",
        thumb: THUMB,
    },
    build: build_kf_clamp,
    info: Some(info_kf_clamp),
};

fn build_kf_clamp(p: &Params) -> Result<Document, String> {
    let d = kf(&p.str("size"))?;
    let mut b = Builder::new();
    let w = d.clamp_width;
    let ring = tube_z(
        &mut b,
        d.clamp_od / 2.0,
        d.od / 2.0 + 0.5,
        -w / 2.0,
        w / 2.0,
    );
    // Wing-nut boss out to the published overall span along +X.
    let reach = d.clamp_span - d.clamp_od / 2.0 - d.od / 2.0;
    let boss_w = 0.6 * w;
    let boss = b.cube(reach.max(1.0), boss_w, w);
    let boss = b.translate(boss, d.od / 2.0 + 1.0, -boss_w / 2.0, -w / 2.0);
    let root = b.union(ring, boss);
    Ok(b.finish(root, "aluminum"))
}

fn info_kf_clamp(p: &Params) -> Result<PartInfo, String> {
    let d = kf(&p.str("size"))?;
    Ok(PartInfo {
        ports: vec![seal_frame().port("seal", FlangeSpec::Kf(d))],
        metadata: json!({
            "family": "KF",
            "size": d.name,
            "envelope_mm": { "span": d.clamp_span, "ring_od": d.clamp_od, "width": d.clamp_width },
            "max_torque_in_lb": 70,
            "envelope": true,
            "source": "MKS Series 31 wing-nut clamp table (A × B × C)",
            "bom": [bom(format!("{} wing-nut clamp", d.name), 1)],
        }),
    })
}
