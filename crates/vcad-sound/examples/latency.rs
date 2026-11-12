//! Where does click-to-hear time go? `latency <doc.vcad>`
fn main() {
    let path = std::env::args().nth(1).expect("doc");
    let doc: vcad_ir::Document =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let t = std::time::Instant::now();
    let scene = vcad_sound::evaluate(&doc).unwrap();
    println!(
        "evaluate: {:.0} ms ({} parts)",
        t.elapsed().as_secs_f64() * 1e3,
        scene.parts.len()
    );
    for part in [0usize, 7, 8] {
        let t = std::time::Instant::now();
        let pm = vcad_sound::part_modes(
            &doc,
            &scene,
            &vcad_sound::ModesRequest {
                part,
                modes: 6,
                quality: vcad_sound::Quality::Preview,
                material: None,
                shapes: true,
            },
        )
        .unwrap();
        println!(
            "part {part} {}: {:.0} ms total, {} dofs, {} anim verts, f1 {:.1} Hz {}",
            pm.name,
            t.elapsed().as_secs_f64() * 1e3,
            pm.dofs,
            pm.vertices,
            pm.modes[0].hz,
            pm.modes[0].note
        );
    }
}
