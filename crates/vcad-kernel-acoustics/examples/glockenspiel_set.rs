//! Print a tuned C-major glockenspiel set.
//!
//! `cargo run -p vcad-kernel-acoustics --release --example glockenspiel_set -- [tonic] [material]`

use vcad_kernel_acoustics::instrument::{design_instrument, BarMaterial, InstrumentSpec};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tonic = args.get(1).map(String::as_str).unwrap_or("C6");
    let material = BarMaterial::parse(args.get(2).map(String::as_str).unwrap_or("aluminium"))
        .expect("material");
    let t0 = std::time::Instant::now();
    let inst =
        design_instrument(&InstrumentSpec::new(tonic, material).expect("spec")).expect("design");
    println!(
        "{:<4} {:>9} {:>9} {:>7} {:>8} {:>7} {:>6} {:>5}  nodes (mm)",
        "note", "target", "f1", "cents", "L mm", "cut mm", "f2/f1", "iter"
    );
    for p in &inst.bars {
        let b = &p.bar;
        println!(
            "{:<4} {:>9.2} {:>9.2} {:>+7.3} {:>8.2} {:>7.2} {:>6.3} {:>5}  {:.1} / {:.1}{}",
            b.note,
            b.target_hz,
            b.hz[0],
            b.cents_error,
            b.geometry.length_mm,
            b.geometry.undercut_depth_mm,
            b.overtone_ratio,
            b.history.len(),
            b.nodes_mm[0],
            b.nodes_mm[1],
            if b.notes.is_empty() {
                String::new()
            } else {
                format!("  [{}]", b.notes.join("; "))
            }
        );
    }
    println!("designed in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    if args.iter().any(|a| a == "--loon") {
        println!("\n{}", inst.loon);
    }
}
