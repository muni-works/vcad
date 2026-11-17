//! The M7 headline: a 6" D-D fusor in a 4 × 5 × 2.5 m concrete room,
//! with and without a 15 cm HDPE castle — dose map, and the expected
//! readings of the instruments the open-fusor build will actually carry
//! (moderated ³He counter at 0.5 m, bubble dosimeter, CR-39).
//!
//! Scene (`examples/fusor_room.json`, also valid `simulate_neutron_shield`
//! `scene` input): 6" (152 mm OD, 3 mm wall) steel chamber at 1 m height
//! in the middle of the room; HDPE castle 600 mm cube with a 300 mm
//! cavity (15 cm walls, closed on all sides); 200 mm concrete walls and
//! ceiling, 300 mm floor slab, a 1 × 2.1 m door opening in the y = 5 m
//! wall. Source rates: 5×10⁶ n/s (upper bound) and 2.5×10⁴ n/s (the
//! predicted floor) — one transport run per geometry; the response is
//! per source neutron, so rates are an exact rescale.
//!
//! Run: `cargo run --release -p vcad-kernel-neutronics --example fusor_room [-- OUT_DIR]`
//! With OUT_DIR, writes `dose_map_{castle,bare}.json` (µSv/h at 5e6 n/s,
//! per-voxel RSE) and `dose_map_{castle,bare}.svg`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use vcad_kernel_neutronics::receipt::scene_claims_from_run;
use vcad_kernel_neutronics::response::ResponseModel;
use vcad_kernel_neutronics::scene_spec::{report, SceneReport, SceneSpec};

const RATES: [(f64, &str); 2] = [(5.0e6, "upper bound"), (2.5e4, "predicted floor")];

fn main() {
    let out_dir = std::env::args().nth(1);
    let base: SceneSpec =
        serde_json::from_str(include_str!("fusor_room.json")).expect("bundled spec parses");

    for (label, castle) in [("castle", true), ("bare", false)] {
        let mut spec = base.clone();
        if !castle {
            for c in spec.cells.iter_mut() {
                if c.name == "castle" {
                    c.material = "air".into();
                }
            }
        }
        let mut params = BTreeMap::new();
        params.insert("castle_half_mm".to_string(), 300.0);
        params.insert("rate".to_string(), RATES[0].0);

        let t0 = std::time::Instant::now();
        let (resolved, result) = spec.evaluate(&params).expect("scene runs");
        let secs = t0.elapsed().as_secs_f64();
        let claims = scene_claims_from_run(&resolved, &result).expect("claims price");
        let rep = report(&resolved, &result);
        if castle {
            let r = &resolved.config.detectors[0].responses[0];
            if let ResponseModel::Tabulated(pts) = &r.model {
                println!(
                    "\nmoderated He-3 counter fluence response R(E) (table rse {:.1}%):",
                    r.table_rse * 100.0
                );
                for (e, v) in pts {
                    println!("  {e:>10.3e} eV  {v:.3e} counts per n/cm2");
                }
            }
        }

        println!(
            "\n=== {label}: {} histories in {secs:.1} s, truncated {}, balance dev {:.1e}, leaked {:.3} ===",
            result.total_histories, result.truncated_histories, result.balance_max_dev, result.leaked.mean
        );
        for (rate, what) in RATES {
            let k = rate / RATES[0].0;
            println!("\n  source {rate:.1e} n/s ({what})");
            println!(
                "  {:<15} {:>16} {:>22} {:>18} {:>18}",
                "detector", "H*(10) uSv/h", "He-3 counts/min", "bubbles/h", "CR-39 tr/cm2/h"
            );
            for d in &rep.detectors {
                let inst = |n: &str, per_min: bool| {
                    d.instruments
                        .iter()
                        .find(|i| i.name == n)
                        .map(|i| {
                            let v = if per_min { i.per_minute } else { i.per_hour } * k;
                            format!("{v:.3e} ±{:.1}%", i.rse.unwrap_or(f64::INFINITY) * 100.0)
                        })
                        .unwrap_or_else(|| "-".into())
                };
                println!(
                    "  {:<15} {:>9.3e} ±{:>4.1}% {:>22} {:>18} {:>18}",
                    d.label,
                    d.dose_usv_per_h.mean * k,
                    d.dose_usv_per_h.rse.unwrap_or(f64::INFINITY) * 100.0,
                    inst("he3_counter", true),
                    inst("bubble", false),
                    inst("cr39", false),
                );
            }
        }
        for c in &claims.claims {
            if c.name.starts_with("dose_map") || c.name.starts_with("capture_rate") {
                println!(
                    "  claim {:<34} {:.4e} {} (rse {:.3}) — {}",
                    c.name, c.value, c.unit, c.rse, c.note
                );
            }
        }
        if let Some(dir) = &out_dir {
            std::fs::create_dir_all(dir).expect("out dir");
            let js = serde_json::to_string(&rep.mesh).expect("serialize");
            std::fs::write(format!("{dir}/dose_map_{label}.json"), js).expect("write json");
            std::fs::write(format!("{dir}/dose_map_{label}.svg"), svg(&rep, label)).expect("svg");
        }
    }
}

/// Log-scale heat map of the mesh (z slice 0), 1 decade per color step.
fn svg(rep: &SceneReport, label: &str) -> String {
    let m = rep.mesh.as_ref().expect("mesh requested");
    let (nx, ny) = (m.mesh.shape[0], m.mesh.shape[1]);
    let px = 12.0;
    let vals: Vec<f64> = m
        .dose_usv_per_h
        .iter()
        .zip(&m.rse)
        .filter(|(_, r)| r.is_some_and(|r| r <= 0.2))
        .map(|(v, _)| *v)
        .filter(|v| *v > 0.0)
        .collect();
    let hi = vals.iter().cloned().fold(0.0, f64::max).log10().ceil();
    let lo = hi - 5.0;
    let mut s = String::new();
    let w = nx as f64 * px;
    let h = ny as f64 * px;
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" font-family="monospace" font-size="11">"#,
        w + 160.0,
        h + 30.0
    );
    for j in 0..ny {
        for i in 0..nx {
            let idx = i + nx * j;
            let (v, r) = (m.dose_usv_per_h[idx], m.rse[idx]);
            let fill = match r {
                Some(r) if r <= 0.2 && v > 0.0 => {
                    let t = ((v.log10() - lo) / (hi - lo)).clamp(0.0, 1.0);
                    // dark blue → yellow
                    let (r8, g8, b8) = (
                        (40.0 + 215.0 * t) as u8,
                        (30.0 + 200.0 * t) as u8,
                        (110.0 * (1.0 - t) + 20.0) as u8,
                    );
                    format!("rgb({r8},{g8},{b8})")
                }
                _ => "rgb(200,200,200)".to_string(), // unresolved: grey, not zero
            };
            // y up: row 0 at the bottom.
            let _ = write!(
                s,
                r#"<rect x="{:.1}" y="{:.1}" width="{px}" height="{px}" fill="{fill}"/>"#,
                i as f64 * px,
                (ny - 1 - j) as f64 * px
            );
        }
    }
    let _ = write!(
        s,
        r#"<text x="{:.0}" y="16">{label}</text><text x="{:.0}" y="32">H*(10) uSv/h</text><text x="{:.0}" y="48">@ {:.1e} n/s</text><text x="{:.0}" y="70">max 1e{hi}</text><text x="{:.0}" y="86">min 1e{lo}</text><text x="{:.0}" y="102">grey: RSE&gt;20%</text><text x="4" y="{:.0}">4 m x 5 m, z = 0.9-1.1 m</text></svg>"#,
        w + 8.0,
        w + 8.0,
        w + 8.0,
        rep.source_rate_n_per_s,
        w + 8.0,
        w + 8.0,
        w + 8.0,
        h + 20.0
    );
    s
}
