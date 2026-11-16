//! Capture-gamma source from a neutronics run.

use std::collections::BTreeMap;

use vcad_kernel_neutronics::spec::ShieldSpec;
use vcad_kernel_photon_dose::capture::{self, CaptureError};
use vcad_kernel_photon_dose::receipt;
use vcad_kernel_photon_dose::spec::RunSpec;

fn shield(material: &str) -> ShieldSpec {
    serde_json::from_str(&format!(
        r#"{{
          "layers": [
            {{"material": "air",  "thickness_mm": 300}},
            {{"material": "{material}", "thickness_mm": 150}},
            {{"material": "air",  "thickness_mm": 1700}}
          ],
          "source": {{"rate_n_per_s": 5.0e6, "energy_ev": 2.45e6}},
          "detectors": [
            {{"label": "1m", "radius_mm": 1000}},
            {{"label": "2m", "radius_mm": 2000}}
          ],
          "run": {{"histories_per_batch": 4000, "batches": 8, "seed": 11}}
        }}"#
    ))
    .unwrap()
}

fn run_spec() -> RunSpec {
    RunSpec {
        histories_per_batch: 4000,
        batches: 8,
        seed: 3,
    }
}

#[test]
fn hdpe_capture_gamma_is_a_real_minority_of_neutron_dose() {
    let (neutron, cap) = capture::from_shield_spec(&shield("hdpe"), &BTreeMap::new(), &run_spec()).unwrap();
    // Captures can't exceed the source; H dominates the lines.
    let total: f64 = cap.lines.iter().filter(|l| l.element == "H").map(|l| l.rate_per_s.mean).sum();
    assert!(total > 0.0 && total < 5.0e6, "H captures {total}/s");
    // Books balance against the neutron run: captures ≤ absorbed fraction × S.
    let absorbed = neutron.claims.iter().find(|c| c.name == "absorbed_fraction").unwrap();
    let all: f64 = cap
        .lines
        .iter()
        .filter(|l| l.element != "C")
        .map(|l| l.rate_per_s.mean)
        .sum();
    assert!(
        all <= absorbed.value * 5.0e6 * (1.0 + 4.0 * absorbed.rse),
        "captures {all}/s exceed absorbed {}/s",
        absorbed.value * 5.0e6
    );
    let h = cap.lines.iter().find(|l| l.element == "H").unwrap();
    let c = cap.lines.iter().find(|l| l.element == "C").unwrap();
    assert!(h.rate_per_s.mean > 50.0 * c.rate_per_s.mean);
    let r1 = &cap.evaluation.reports[0];
    let n1 = neutron.claims.iter().find(|c| c.name == "dose_rate:1m").unwrap().value;
    let frac = r1.h10_usv_h.mean / n1;
    // Literature: capture gammas are ~5–30% of neutron dose behind
    // ~15 cm of polyethylene.
    assert!((0.01..0.5).contains(&frac), "gamma/neutron = {frac}");
    // Falls roughly as 1/r² beyond the shield.
    let r2 = &cap.evaluation.reports[1];
    let ratio = r1.h10_usv_h.mean / r2.h10_usv_h.mean;
    assert!(ratio > 3.0 && ratio < 6.0, "1m/2m = {ratio}");

    // Combined total claims.
    let set = receipt::combined_claims(
        &neutron,
        &cap.evaluation.reports.iter().collect::<Vec<_>>(),
        cap.evaluation.provenance.clone(),
    )
    .unwrap();
    let t = set.claims.iter().find(|c| c.name == "total_dose_rate:1m").unwrap();
    assert!((t.value - (n1 + r1.h10_usv_h.mean)).abs() < 1e-12 * t.value);
    assert!(t.high > t.value);
}

#[test]
fn borated_poly_adds_the_478_kev_line() {
    let (_, cap) = capture::from_shield_spec(&shield("borated-hdpe-5"), &BTreeMap::new(), &run_spec()).unwrap();
    let b = cap.lines.iter().find(|l| l.element == "B").unwrap();
    assert!((b.energy_kev - 477.6).abs() < 1e-9 && b.rate_per_s.mean > 0.0);
}

#[test]
fn unsupported_emitters_fail_closed() {
    let e = capture::from_shield_spec(&shield("concrete"), &BTreeMap::new(), &run_spec()).unwrap_err();
    assert!(matches!(e, CaptureError::UnsupportedEmitter(_)), "{e}");
}
