//! One STEP file → one JSON line of interop metrics, for
//! `scripts/step-corpus/scorecard.py` (which runs this per file under a
//! timeout, so a hang shows up as a timeout instead of stalling the run).
//!
//! ```text
//! cargo run --release -p vcad-kernel-tessellate --example step_scorecard -- part.step [roundtrip.step]
//! ```
//!
//! Measures what the Mac app does with the file: the flat read
//! (`read_step_with_report`, what `vcad_import_step` uses), the assembly read
//! (part names, instance placements), tessellation at the app's 32 segments
//! (triangles, open/overused edges, signed volume), styled colours, and a
//! round trip (write every solid back out, re-read, compare faces/volume).
//! When a second path is given, the round-tripped STEP is left there so
//! another tool (OCCT) can open it.

use std::collections::HashMap;
use std::time::Instant;
use vcad_kernel_tessellate::{manifold::signed_volume, tessellate_brep, TriangleMesh};

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

fn stage(s: &str) {
    eprintln!("stage: {s}");
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 1e3).round()
}

#[derive(Default)]
struct MeshStats {
    tris: usize,
    open: usize,
    over: usize,
    closed_solids: usize,
    volume: f64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: step_scorecard <file.step> [roundtrip-out.step]");
    let rt_out = args.next();
    let mut out = serde_json::Map::new();
    let mut put = |k: &str, v: serde_json::Value| {
        out.insert(k.into(), v);
    };
    let data = std::fs::read(&path).expect("read file");
    let text = String::from_utf8_lossy(&data);
    let count = |needle: &str| text.matches(needle).count();
    put("src_advanced_faces", count("ADVANCED_FACE(").into());
    put("src_styled_items", (count("STYLED_ITEM(")).into());
    put("src_nauo", count("NEXT_ASSEMBLY_USAGE_OCCURRENCE(").into());

    stage("read");
    let t = Instant::now();
    let read = match vcad_kernel_step::read_step_from_buffer_with_report(&data) {
        Ok(r) => r,
        Err(e) => {
            put("read_ok", false.into());
            put("read_error", e.to_string().into());
            put("read_ms", ms(t).into());
            println!("{}", serde_json::Value::Object(out));
            return;
        }
    };
    put("read_ok", true.into());
    put("read_ms", ms(t).into());
    put("solids", read.solids.len().into());
    put(
        "faces",
        read.solids
            .iter()
            .map(|s| s.topology.faces.len())
            .sum::<usize>()
            .into(),
    );
    put("skipped_faces", read.report.total_skipped_faces().into());
    put(
        "colored_solids",
        read.colors.iter().filter(|c| c.is_some()).count().into(),
    );
    put(
        "named_solids",
        read.names.iter().filter(|n| n.is_some()).count().into(),
    );
    // Faces once every assembly occurrence is placed: what OCCT counts.
    let faces_of: HashMap<u64, usize> = read
        .report
        .solids
        .iter()
        .zip(&read.solids)
        .map(|(r, s)| (r.solid_id, s.topology.faces.len()))
        .collect();
    put("occurrences", read.occurrences.len().into());
    if !read.occurrences.is_empty() {
        put(
            "placed_faces",
            read.occurrences
                .iter()
                .filter_map(|o| faces_of.get(&o.solid_id))
                .sum::<usize>()
                .into(),
        );
    }
    let t = Instant::now();
    let mut st = MeshStats::default();
    let mut vol_of: HashMap<u64, f64> = HashMap::new();
    for (i, s) in read.solids.iter().enumerate() {
        stage(&format!(
            "tessellate solid {i} ({} faces)",
            s.topology.faces.len()
        ));
        let mesh = tessellate_brep(s, 32);
        let (over, open) = census(&mesh);
        st.tris += mesh.indices.len() / 3;
        st.open += open;
        st.over += over;
        if open == 0 && over == 0 && !mesh.indices.is_empty() {
            st.closed_solids += 1;
        }
        let v = signed_volume(&mesh);
        st.volume += v;
        vol_of.insert(read.report.solids[i].solid_id, v);
    }
    put("tess_ms", ms(t).into());
    put("tris", st.tris.into());
    put("open_edges", st.open.into());
    put("overused_edges", st.over.into());
    put("closed_solids", st.closed_solids.into());
    // Volume with every assembly occurrence placed — what OCCT reports.
    let placed: f64 = if read.occurrences.is_empty() {
        st.volume
    } else {
        read.occurrences
            .iter()
            .filter_map(|o| vol_of.get(&o.solid_id))
            .sum()
    };
    put("volume", ((placed * 1e3).round() / 1e3).into());
    put("unplaced_volume", ((st.volume * 1e3).round() / 1e3).into());

    let t = Instant::now();
    stage("assembly");
    match vcad_kernel_step::read_step_assembly_from_buffer(&data) {
        Ok(a) => {
            put("asm_parts", a.parts.len().into());
            put("asm_instances", a.instances.len().into());
            put(
                "asm_nested",
                a.instances
                    .iter()
                    .filter(|i| i.parent_id.is_some())
                    .count()
                    .into(),
            );
        }
        Err(e) => put("asm_error", e.to_string().into()),
    }
    put("asm_ms", ms(t).into());

    stage("roundtrip");
    // Round trip: write every solid, read back.
    let t = Instant::now();
    let named: Vec<String> = (0..read.solids.len())
        .map(|i| format!("solid{i}"))
        .collect();
    let pairs: Vec<_> = read
        .solids
        .iter()
        .zip(named.iter().map(String::as_str))
        .collect();
    match vcad_kernel_step::write_step_solids_to_buffer(&pairs) {
        Ok(buf) => {
            if let Some(p) = &rt_out {
                std::fs::write(p, &buf).expect("write roundtrip");
            }
            match vcad_kernel_step::read_step_from_buffer_with_report(&buf) {
                Ok(rt) => {
                    put("rt_solids", rt.solids.len().into());
                    put(
                        "rt_faces",
                        rt.solids
                            .iter()
                            .map(|s| s.topology.faces.len())
                            .sum::<usize>()
                            .into(),
                    );
                    let v: f64 = rt
                        .solids
                        .iter()
                        .map(|s| signed_volume(&tessellate_brep(s, 32)))
                        .sum();
                    put("rt_volume", ((v * 1e3).round() / 1e3).into());
                }
                Err(e) => put("rt_error", format!("reread: {e}").into()),
            }
        }
        Err(e) => put("rt_error", format!("write: {e}").into()),
    }
    put("rt_ms", ms(t).into());
    println!("{}", serde_json::Value::Object(out));
}
