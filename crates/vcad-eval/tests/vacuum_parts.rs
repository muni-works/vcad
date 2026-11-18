//! The vacuum parts library through the real kernel: every part evaluates to
//! a non-empty solid, the worst-case flange matches its closed-form volume,
//! `std:` PartInstance nodes expand on the Rust path, and a mated joint has
//! no interference.

use std::collections::HashMap;

use serde_json::{json, Value};
use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::{CsgOp, Document, Node, SceneEntry};
use vcad_parts::vacuum::{cf, mate_transform};
use vcad_parts::{all_parts, build_part, part_info, splice};

fn params(v: Value) -> HashMap<String, Value> {
    serde_json::from_value(v).unwrap()
}

/// Signed volume of a triangle mesh (divergence theorem).
fn mesh_volume(pos: &[f32], idx: &[u32]) -> f64 {
    let p = |i: u32| {
        let k = 3 * i as usize;
        [pos[k] as f64, pos[k + 1] as f64, pos[k + 2] as f64]
    };
    idx.chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// Evaluate a document; return (mesh volume, bbox) of its single root.
fn eval(doc: &Document) -> (f64, [f64; 3], [f64; 3]) {
    let scene = evaluate_document(
        doc,
        &EvalOptions {
            skip_clash_detection: true,
            ..Default::default()
        },
    )
    .expect("evaluate");
    assert!(scene.failures.is_empty(), "failures: {:?}", scene.failures);
    assert_eq!(scene.parts.len(), 1, "expected one part");
    let m = &scene.parts[0].mesh;
    assert!(!m.indices.is_empty(), "empty mesh");
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for v in m.positions.chunks(3) {
        for i in 0..3 {
            lo[i] = lo[i].min(v[i] as f64);
            hi[i] = hi[i].max(v[i] as f64);
        }
    }
    (mesh_volume(&m.positions, &m.indices), lo, hi)
}

fn eval_part(path: &str, p: Value) -> (f64, [f64; 3], [f64; 3]) {
    let doc = build_part(path, &params(p)).unwrap_or_else(|e| panic!("{path}: {e}"));
    eval(&doc)
}

/// Parts whose evaluation takes minutes in the current boolean kernel
/// (crossing-cylinder intersections are sampled densely: a 6-way cross is
/// ~136k triangles even at `detail = "envelope"`). Covered by the ignored
/// `heavy_vacuum_parts_evaluate`; run it with `--ignored`.
const HEAVY: &[&str] = &[
    "vacuum.cf.reducer",
    "vacuum.cross",
    "vacuum.chamber.spherical-octagon",
];

fn check_every(heavy: bool) {
    for e in all_parts()
        .iter()
        .filter(|e| e.meta.id.starts_with("vacuum.") && HEAVY.contains(&e.meta.id) == heavy)
    {
        let t = std::time::Instant::now();
        let (vol, lo, hi) = eval_part(e.meta.id, json!({}));
        eprintln!(
            "{:<36} {:>8.2}s  {vol:.0} mm³",
            e.meta.id,
            t.elapsed().as_secs_f64()
        );
        assert!(vol > 0.0, "{}: volume {vol}", e.meta.id);
        let bbox_vol: f64 = (0..3).map(|i| hi[i] - lo[i]).product();
        assert!(
            vol < bbox_vol,
            "{}: volume {vol} exceeds bbox {bbox_vol}",
            e.meta.id
        );
    }
}

#[test]
fn every_vacuum_part_evaluates_with_defaults() {
    check_every(false);
}

#[test]
#[ignore = "minutes of boolean time; run with --ignored before touching chambers/crosses"]
fn heavy_vacuum_parts_evaluate() {
    check_every(true);
}

#[test]
fn dn160_blank_flange_matches_closed_form() {
    let d = cf("DN160").unwrap();
    let (vol, lo, hi) = eval_part(
        "vacuum.cf.flange",
        json!({"size": "DN160", "knife_edge": false}),
    );
    let pi = std::f64::consts::PI;
    let disc = pi * (d.od / 2.0).powi(2) * d.thickness;
    let holes = d.bolt_count as f64 * pi * (d.hole_clearance / 2.0).powi(2) * d.thickness;
    let recess = pi * (d.gasket_od / 2.0 + 0.1).powi(2) * d.recess_depth();
    let expect = disc - holes - recess;
    assert!(
        (vol - expect).abs() / expect < 0.01,
        "DN160 flange volume {vol:.0} vs closed form {expect:.0}"
    );
    assert!((hi[2] - 0.0).abs() < 1e-3 && (lo[2] + d.thickness).abs() < 1e-3);
    assert!((hi[0] - lo[0] - d.od).abs() < 0.5);
}

#[test]
fn knife_edge_adds_material_inside_the_recess() {
    let (plain, ..) = eval_part(
        "vacuum.cf.flange",
        json!({"size": "DN40", "knife_edge": false}),
    );
    let (edged, _, hi) = eval_part("vacuum.cf.flange", json!({"size": "DN40"}));
    // The edge is a small ring (~0.3 mm tall) plus the inner land kept at
    // face height: more material, nothing proud of the face.
    assert!(
        edged > plain,
        "knife edge removed material: {edged} vs {plain}"
    );
    assert!(hi[2] < 1e-3, "nothing may stand proud of the sealing face");
}

/// (boundary, non-manifold, folded) edges of a mesh welded at 0.1 µm.
/// Folded = the two triangles on an edge wind it the same way, or lie back
/// to back.
fn edge_census(pos: &[f32], idx: &[u32]) -> (usize, usize, usize) {
    let p = |i: u32| {
        let k = 3 * i as usize;
        [pos[k] as f64, pos[k + 1] as f64, pos[k + 2] as f64]
    };
    let key = |i: u32| p(i).map(|c| (c * 1e4).round() as i64);
    let normal = |t: usize| {
        let (a, b, c) = (p(idx[3 * t]), p(idx[3 * t + 1]), p(idx[3 * t + 2]));
        let (u, w) = (
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-300);
        n.map(|c| c / l)
    };
    type Key = [i64; 3];
    let mut edges: HashMap<(Key, Key), Vec<(usize, bool)>> = HashMap::new();
    for (t, tri) in idx.chunks(3).enumerate() {
        let k = [key(tri[0]), key(tri[1]), key(tri[2])];
        for i in 0..3 {
            let (a, b) = (k[i], k[(i + 1) % 3]);
            if a != b {
                edges
                    .entry((a.min(b), a.max(b)))
                    .or_default()
                    .push((t, a < b));
            }
        }
    }
    let (mut boundary, mut non_manifold, mut folded) = (0, 0, 0);
    for uses in edges.values() {
        match uses[..] {
            [_] => boundary += 1,
            [(t0, d0), (t1, d1)] => {
                let (n0, n1) = (normal(t0), normal(t1));
                let dot: f64 = (0..3).map(|i| n0[i] * n1[i]).sum();
                folded += usize::from(d0 == d1 || dot < -0.9);
            }
            _ => non_manifold += 1,
        }
    }
    (boundary, non_manifold, folded)
}

/// The spherical cube at `detail = "envelope"`: one closed analytic solid at
/// its closed-form volume.
///
/// It used to come back `Analytic` and wrong: the body was seven overlapping
/// shells (each flange disc concatenated onto the box-trimmed sphere, not
/// fused), the bores then left two membranes per port (4,848 folded edges on
/// DN63) and the volume read +4.6%.
#[test]
fn spherical_cube_is_one_clean_solid_at_closed_form_volume() {
    use vcad_parts::vacuum::chambers::chamber_geom;
    let pi = std::f64::consts::PI;
    for size in ["DN63", "DN100"] {
        let d = cf(size).unwrap();
        let g = chamber_geom(d);
        let doc = build_part(
            "vacuum.chamber.spherical-cube",
            &params(json!({"size": size})),
        )
        .unwrap();
        let scene = evaluate_document(
            &doc,
            &EvalOptions {
                skip_clash_detection: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(scene.failures.is_empty(), "{size}: {:?}", scene.failures);
        let part = &scene.parts[0];
        let solid = part.solid.as_ref().expect("solid");
        assert_eq!(
            solid.fidelity().as_str(),
            "analytic",
            "{size}: {:?}",
            solid
                .degradations()
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
        let m = &part.mesh;
        assert_eq!(
            edge_census(&m.positions, &m.indices),
            (0, 0, 0),
            "{size}: boundary / non-manifold / folded edges"
        );

        // Sphere cap beyond a plane at distance `at`, and sphere slab
        // between two planes.
        let cap = |r: f64, at: f64| pi * (r - at).powi(2) * (2.0 * r + at) / 3.0;
        let slab = |r: f64, a: f64, b: f64| pi * (r * r * (b - a) - (b.powi(3) - a.powi(3)) / 3.0);
        let (r_fl, r_b, e) = (d.od / 2.0, d.bore() / 2.0, g.h + 1.0);
        // Body: outer sphere less six caps, plus each flange disc less the
        // 1 mm slab it shares with the sphere.
        let body = 4.0 / 3.0 * pi * g.r_out.powi(3) - 6.0 * cap(g.r_out, e)
            + 6.0 * (pi * r_fl * r_fl * d.thickness - slab(g.r_out, g.h, e));
        // Removed: the cavity sphere plus each bore from 0.6·r_i to the
        // face, less what bore and cavity share.
        let (z0, zc) = (0.6 * g.r_i, (g.r_i * g.r_i - r_b * r_b).sqrt());
        let shared = pi * r_b * r_b * (zc - z0) + cap(g.r_i, zc);
        let removed =
            4.0 / 3.0 * pi * g.r_i.powi(3) + 6.0 * (pi * r_b * r_b * (g.c2f - z0) - shared);
        let exact = body - removed;
        let vol = mesh_volume(&m.positions, &m.indices);
        // Measured +0.035% (DN63) and +0.059% (DN100); the un-fused body
        // alone is +2.9%.
        assert!(
            (vol - exact).abs() / exact < 0.003,
            "{size}: volume {vol:.0} vs closed form {exact:.0}"
        );
    }
}

#[test]
#[ignore = "minutes of boolean time; run with --ignored"]
fn spherical_cube_with_corner_ports_evaluates_and_matches_ports() {
    let p = json!({"size": "DN100", "corner_ports": true});
    let (vol, lo, hi) = eval_part("vacuum.chamber.spherical-cube", p.clone());
    let info = part_info("vacuum.chamber.spherical-cube", &params(p)).unwrap();
    assert_eq!(info.ports.len(), 14);
    let c2f = info.metadata["center_to_face_mm"].as_f64().unwrap();
    for i in 0..3 {
        assert!(
            (hi[i] - c2f).abs() < 1.0 || hi[i] > c2f,
            "axis {i}: {} vs {c2f}",
            hi[i]
        );
        assert!((lo[i] + c2f).abs() < 1.0 || lo[i] < -c2f);
    }
    assert!(vol > 1.0e6, "cube volume {vol}");
}

#[test]
fn part_instance_expands_on_the_rust_path() {
    let mut doc = Document::new();
    doc.nodes.insert(
        1,
        Node {
            id: 1,
            name: Some("flange".into()),
            op: CsgOp::PartInstance {
                path: "std:vacuum.cf.flange".into(),
                version: "1.0".into(),
                params: params(json!({"size": "DN40"})),
            },
        },
    );
    doc.roots.push(SceneEntry {
        root: 1,
        material: "steel".into(),
        visible: None,
    });
    let (vol, ..) = eval(&doc);
    assert!(vol > 30_000.0, "expanded flange volume {vol}");
}

#[test]
fn unknown_part_instance_is_reported_not_swallowed() {
    let mut doc = Document::new();
    doc.nodes.insert(
        1,
        Node {
            id: 1,
            name: None,
            op: CsgOp::PartInstance {
                path: "std:vacuum.nope".into(),
                version: "1.0".into(),
                params: HashMap::new(),
            },
        },
    );
    doc.roots.push(SceneEntry {
        root: 1,
        material: "steel".into(),
        visible: None,
    });
    let scene = evaluate_document(&doc, &EvalOptions::default()).unwrap();
    assert!(scene
        .failures
        .iter()
        .any(|f| f.error.contains("unknown part")));
}

/// Place `child` (path, params, port) onto `parent`'s `parent_port`, both as
/// spliced sub-graphs in one document, and return (doc, parent id, child id).
fn mated_pair(parent: (&str, Value, &str), child: (&str, Value, &str)) -> (Document, u64, u64) {
    let pp = params(parent.1);
    let cp = params(child.1);
    let pinfo = part_info(parent.0, &pp).unwrap();
    let cinfo = part_info(child.0, &cp).unwrap();
    let ppt = pinfo.ports.iter().find(|p| p.name == parent.2).unwrap();
    let cpt = cinfo.ports.iter().find(|p| p.name == child.2).unwrap();
    let m = mate_transform(cpt, ppt);

    let mut doc = Document::new();
    let mut next = 1u64;
    let mut alloc = || {
        let n = next;
        next += 1;
        n
    };
    let pid = splice(
        &mut doc.nodes,
        &build_part(parent.0, &pp).unwrap(),
        &mut alloc,
    )
    .unwrap();
    let cid = splice(
        &mut doc.nodes,
        &build_part(child.0, &cp).unwrap(),
        &mut alloc,
    )
    .unwrap();
    let e = m.euler_xyz_deg();
    let rid = alloc();
    doc.nodes.insert(
        rid,
        Node {
            id: rid,
            name: None,
            op: CsgOp::Rotate {
                child: cid,
                angles: vcad_ir::Vec3::new(e[0], e[1], e[2]),
            },
        },
    );
    let tid = alloc();
    doc.nodes.insert(
        tid,
        Node {
            id: tid,
            name: None,
            op: CsgOp::Translate {
                child: rid,
                offset: vcad_ir::Vec3::new(m.t[0], m.t[1], m.t[2]),
            },
        },
    );
    (doc, pid, tid)
}

#[test]
fn mated_cf_joint_stays_on_its_own_side_of_the_seal_plane() {
    // A mated joint's two solids touch on the seal plane and nowhere else.
    // An Intersection boolean of face-touching solids is the kernel's
    // degenerate coplanar-contact case (it reports a sliver), so check the
    // geometric claim directly: every nipple vertex lies on or below its +z
    // seal plane, every blank-flange vertex on or above it.
    let parent = (
        "vacuum.nipple",
        json!({"size": "DN40", "length": 80.0}),
        "+z",
    );
    let child = ("vacuum.cf.flange", json!({"size": "DN40"}), "seal");
    let (doc, pid, cid) = mated_pair(parent, child);
    let zs = |root: u64| {
        let mut d = doc.clone();
        d.roots.push(SceneEntry {
            root,
            material: "steel".into(),
            visible: None,
        });
        let s = evaluate_document(
            &d,
            &EvalOptions {
                skip_clash_detection: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.failures.is_empty(), "{:?}", s.failures);
        let z: Vec<f64> = s.parts[0]
            .mesh
            .positions
            .chunks(3)
            .map(|v| v[2] as f64)
            .collect();
        (
            z.iter().cloned().fold(f64::MAX, f64::min),
            z.iter().cloned().fold(f64::MIN, f64::max),
        )
    };
    let (_, nip_max) = zs(pid);
    let (fl_min, fl_max) = zs(cid);
    let t = cf("DN40").unwrap().thickness;
    assert!(
        nip_max <= 80.0 + 1e-3,
        "nipple crosses the seal plane: {nip_max}"
    );
    assert!(
        fl_min >= 80.0 - 1e-3,
        "flange crosses the seal plane: {fl_min}"
    );
    assert!(
        (fl_max - (80.0 + t)).abs() < 1e-3,
        "blank's back face at {fl_max}"
    );
}

#[test]
fn kf25_blank_flange_matches_closed_form() {
    use vcad_parts::vacuum::{kf, kf_flange_height, KF_RIM};
    let d = kf("KF25").unwrap();
    let (vol, ..) = eval_part("vacuum.kf.flange", json!({"size": "KF25"}));
    let pi = std::f64::consts::PI;
    let (ro, rt) = (d.od / 2.0, d.tube_od / 2.0 + 0.5);
    let h = kf_flange_height(d) - KF_RIM;
    let expect = pi * ro * ro * KF_RIM + pi * h / 3.0 * (ro * ro + ro * rt + rt * rt);
    assert!(
        (vol - expect).abs() / expect < 0.02,
        "KF25 flange volume {vol:.0} vs closed form {expect:.0}"
    );
}

/// Every port of a multi-port fitting lies on the evaluated part's bounding
/// box, at the face its axis points through. Catches a frame placed with the
/// wrong Euler convention: `CsgOp::Rotate` builds Rx·Ry·Rz (Z acts first),
/// and decomposing frames as Rz·Ry·Rx put the cross's x tube along −y and its
/// +x flange 13 mm beyond the port.
fn ports_on_bbox(path: &str, p: Value) {
    let (_, lo, hi) = eval_part(path, p.clone());
    let info = part_info(path, &params(p)).unwrap();
    for port in &info.ports {
        let k = (0..3)
            .max_by(|&a, &b| port.axis[a].abs().total_cmp(&port.axis[b].abs()))
            .unwrap();
        let face = if port.axis[k] > 0.0 { hi[k] } else { lo[k] };
        assert!(
            (port.origin[k] - face).abs() < 1.0,
            "{path}: port {} at {:?} is off the bbox face {face:.2} (bbox {lo:?}..{hi:?})",
            port.name,
            port.origin
        );
    }
}

#[test]
#[ignore = "kernel leaves a 1 mm bore lip past the +z flange face (a crossing-cylinder saddle strip the flange plane fails to split); un-ignore once the kernel splits it"]
fn tee_ports_lie_on_its_bbox() {
    ports_on_bbox("vacuum.tee", json!({}));
}

#[test]
#[ignore = "minutes of boolean time; run with --ignored"]
fn cross_ports_lie_on_its_bbox() {
    ports_on_bbox("vacuum.cross", json!({}));
}
