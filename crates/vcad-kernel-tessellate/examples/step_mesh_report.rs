//! Import a STEP file and report what tessellation does with it, stage by
//! stage: read time, per-surface-kind face and triangle counts, the welded
//! mesh's volume and edge census (what the export repair's gate sees), and
//! the repair's own cost.
//!
//! ```text
//! cargo run --release -p vcad-kernel-tessellate --example step_mesh_report -- part.step [segments]
//! ```

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;
use vcad_kernel_tessellate::{
    manifold::signed_volume, repair_watertightness, tessellate_brep, tessellate_brep_by_face,
    TessellationParams, TriangleMesh,
};

/// Position-quantized edge census: (overused, open) edge counts.
fn census(mesh: &TriangleMesh) -> (usize, usize) {
    let q = |i: u32| {
        let k = i as usize * 3;
        [0, 1, 2].map(|c| (mesh.vertices[k + c] as f64 * 1e4).round() as i64)
    };
    let mut counts: HashMap<([i64; 3], [i64; 3]), (u32, i32)> = HashMap::new();
    for t in mesh.indices.chunks(3) {
        for k in 0..3 {
            let (a, b) = (q(t[k]), q(t[(k + 1) % 3]));
            if a == b {
                continue;
            }
            let (key, s) = if a < b { ((a, b), 1) } else { ((b, a), -1) };
            let e = counts.entry(key).or_default();
            e.0 += 1;
            e.1 += s;
        }
    }
    let over = counts.values().filter(|(n, _)| *n > 2).count();
    let open = counts
        .values()
        .filter(|(n, net)| *n <= 2 && (*n != 2 || *net != 0))
        .count();
    (over, open)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: step_mesh_report <file.step> [segments]");
    let segments: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(32);

    let t = Instant::now();
    let read = vcad_kernel_step::read_step_with_report(&path).expect("read STEP");
    println!("read: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    if let Some(s) = read.report.summary() {
        println!("{s}");
    }
    for solid in &read.report.solids {
        for note in &solid.notes {
            println!("note: {note}");
        }
    }
    for brep in &read.solids {
        let t = Instant::now();
        let faces = tessellate_brep_by_face(brep, &TessellationParams::from_segments(segments));
        let mut kinds: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for (_, kind, m) in &faces {
            let e = kinds.entry(format!("{kind:?}")).or_default();
            e.0 += 1;
            e.1 += m.indices.len() / 3;
        }
        println!(
            "faces: {:.1} ms, (faces, triangles) by kind {kinds:?}",
            t.elapsed().as_secs_f64() * 1e3
        );

        let t = Instant::now();
        let mut mesh = tessellate_brep(brep, segments);
        let (over, open) = census(&mesh);
        println!(
            "tessellate_brep: {:.1} ms, {} tris, volume {:.3}, overused edges {over}, open edges {open}",
            t.elapsed().as_secs_f64() * 1e3,
            mesh.indices.len() / 3,
            signed_volume(&mesh),
        );

        let t = Instant::now();
        repair_watertightness(&mut mesh);
        println!(
            "repair: {:.1} ms, {} tris, volume {:.3}",
            t.elapsed().as_secs_f64() * 1e3,
            mesh.indices.len() / 3,
            signed_volume(&mesh),
        );
    }
}
