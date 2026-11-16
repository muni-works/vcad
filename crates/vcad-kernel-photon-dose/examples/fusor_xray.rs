//! The open-fusor x-ray question: dose at a bare glass viewport vs a
//! leaded-glass window vs a steel shutter, for a 6" stainless IEC
//! chamber at −20…−40 kV, 10 mA, at 0.5 / 1 / 2 m — plus the
//! capture-gamma dose from a 15 cm HDPE neutron shield, summed with the
//! neutron dose.
//!
//! Electron fraction 1.0: every milliamp of supply current is assumed to
//! reach the wall as electrons at full energy — the **bound**. Dose is
//! linear in it; real fusors put some of the current into ions, so scale
//! down only once measured.
//!
//! Run: `cargo run --release -p vcad-kernel-photon-dose --example fusor_xray`

use std::collections::BTreeMap;

use vcad_kernel_neutronics::spec::ShieldSpec;
use vcad_kernel_photon_dose::capture;
use vcad_kernel_photon_dose::materials;
use vcad_kernel_photon_dose::receipt;
use vcad_kernel_photon_dose::source::ThickTarget;
use vcad_kernel_photon_dose::spec::{evaluate_chamber, ChamberSpec, RunSpec};
use vcad_kernel_photon_dose::transport::{lead_equivalent_mm, Slab};

const WINDOWS: [(&str, &str); 3] = [
    (
        "bare glass (4.8 mm borosilicate)",
        r#"[{"material": "borosilicate", "thickness_mm": 4.8}]"#,
    ),
    (
        "glass + 8 mm lead glass",
        r#"[{"material": "borosilicate", "thickness_mm": 4.8}, {"material": "lead-glass", "thickness_mm": 8}]"#,
    ),
    (
        "glass + 3 mm SS304 shutter",
        r#"[{"material": "borosilicate", "thickness_mm": 4.8}, {"material": "ss304", "thickness_mm": 3}]"#,
    ),
];

fn chamber(kv: f64, window: &str) -> ChamberSpec {
    serde_json::from_str(&format!(
        r#"{{
          "voltage_kv": {kv}, "current_ma": 10, "electron_fraction": 1.0,
          "chamber_radius_mm": 76,
          "wall": {{"material": "ss304", "thickness_mm": 1.65}},
          "viewport": {{"clear_radius_mm": 19, "layers": {window}}},
          "detectors": [
            {{"label": "vp-0.5m", "path": "viewport", "distance_mm": 500}},
            {{"label": "vp-1m",   "path": "viewport", "distance_mm": 1000}},
            {{"label": "vp-2m",   "path": "viewport", "distance_mm": 2000}},
            {{"label": "wall-0.5m", "path": "wall", "distance_mm": 500}},
            {{"label": "wall-1m",   "path": "wall", "distance_mm": 1000}},
            {{"label": "wall-2m",   "path": "wall", "distance_mm": 2000}}
          ],
          "run": {{"histories_per_batch": 20000, "batches": 20, "seed": 20261116}}
        }}"#
    ))
    .expect("example spec parses")
}

fn main() {
    println!("6\" SS304 IEC chamber (r = 76 mm, 1.65 mm wall), 2.75\" CF viewport (19 mm clear radius)");
    println!("10 mA supply, electron fraction 1.0 (bound); H*(10) µSv/h = mean ± MC rse, [band low – high]\n");
    let p = BTreeMap::new();
    for kv in [20.0, 30.0, 40.0] {
        println!("=== −{kv} kV ===");
        for (name, w) in WINDOWS {
            let ev = evaluate_chamber(&chamber(kv, w), &p).expect("chamber evaluates");
            let set = receipt::predicted_claims(&ev).expect("claims");
            println!("  {name}");
            for r in ev.reports.iter().filter(|r| r.label.starts_with("vp")) {
                let c = set.claims.iter().find(|c| c.name == format!("h10_rate:{}", r.label)).unwrap();
                println!(
                    "    {:<10} {:>10.3e} ± {:>4.1}%  [{:.2e} – {:.2e}]  band ×/÷{:.2}  kerma {:.2e} µGy/h  buildup {:.2}",
                    r.label,
                    c.value,
                    c.rse * 100.0,
                    c.low,
                    c.high,
                    c.band_factor,
                    r.kerma_ugy_h.mean,
                    r.buildup_h10
                );
            }
        }
        let ev = evaluate_chamber(&chamber(kv, WINDOWS[0].1), &p).unwrap();
        println!("  through the 1.65 mm steel wall");
        for r in ev.reports.iter().filter(|r| r.label.starts_with("wall")) {
            println!(
                "    {:<10} {:>10.3e} ± {:>4.1}%  [{:.2e} – {:.2e}]",
                r.label,
                r.h10_usv_h.mean,
                r.h10_usv_h.rse * 100.0,
                r.h10_low(),
                r.h10_high()
            );
        }
        println!();
    }

    let t = ThickTarget::new(40.0, 1.0, materials::ss304()).unwrap();
    let s = t.spectrum(10.0, 120).unwrap();
    println!(
        "lead equivalence at 40 kV (narrow-beam air kerma): 8 mm lead glass ≈ {:.2} mm Pb; 3 mm SS304 ≈ {:.3} mm Pb; 4.8 mm borosilicate ≈ {:.3} mm Pb\n",
        lead_equivalent_mm(&Slab::new(materials::lead_glass(), 8.0), &s),
        lead_equivalent_mm(&Slab::new(materials::ss304(), 3.0), &s),
        lead_equivalent_mm(&Slab::new(materials::borosilicate(), 4.8), &s),
    );

    // Capture gammas from the neutron shield, summed with neutron dose.
    println!("=== neutron + capture gamma, 5e6 n/s D-D (safety-case upper bound), 15 cm HDPE ===");
    let shield: ShieldSpec = serde_json::from_str(
        r#"{
          "layers": [
            {"material": "air",  "thickness_mm": 300},
            {"material": "hdpe", "thickness_mm": 150},
            {"material": "air",  "thickness_mm": 1700}
          ],
          "source": {"rate_n_per_s": 5.0e6, "energy_ev": 2.45e6},
          "detectors": [
            {"label": "0.5m-from-center", "radius_mm": 500},
            {"label": "1m", "radius_mm": 1000},
            {"label": "2m", "radius_mm": 2000}
          ],
          "run": {"histories_per_batch": 50000, "batches": 20, "seed": 20260717}
        }"#,
    )
    .unwrap();
    let run = RunSpec {
        histories_per_batch: 20_000,
        batches: 20,
        seed: 20261116,
    };
    let (neutron, cap) = capture::from_shield_spec(&shield, &p, &run).expect("capture evaluates");
    let lines: Vec<String> = cap
        .lines
        .iter()
        .map(|l| format!("{} {:.0} keV {:.3e}/s ± {:.1}%", l.element, l.energy_kev, l.rate_per_s.mean, l.rate_per_s.rse * 100.0))
        .collect();
    println!("  capture lines: {}", lines.join("; "));
    let total = receipt::combined_claims(&neutron, &cap.evaluation.reports.iter().collect::<Vec<_>>(), cap.evaluation.provenance.clone()).unwrap();
    for r in &cap.evaluation.reports {
        let n = neutron.claims.iter().find(|c| c.name == format!("dose_rate:{}", r.label)).unwrap();
        let t = total.claims.iter().find(|c| c.name == format!("total_dose_rate:{}", r.label)).unwrap();
        println!(
            "  {:<18} neutron {:.3e} ± {:.1}%   capture-γ {:.3e} ± {:.1}% (×/÷{:.2})   total {:.3e} [{:.2e} – {:.2e}] µSv/h",
            r.label,
            n.value,
            n.rse * 100.0,
            r.h10_usv_h.mean,
            r.h10_usv_h.rse * 100.0,
            r.band_factor(),
            t.value,
            t.low,
            t.high
        );
    }
}
