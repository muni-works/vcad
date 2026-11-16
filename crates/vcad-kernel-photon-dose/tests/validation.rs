//! Analytic and published-value validation for the photon transport.

use std::collections::BTreeMap;

use vcad_kernel_photon_dose::dose::{h10_psv_cm2, PSV_PER_S_TO_USV_PER_H};
use vcad_kernel_photon_dose::materials::{self, Material};
use vcad_kernel_photon_dose::receipt;
use vcad_kernel_photon_dose::source::{Spectrum, ThickTarget};
use vcad_kernel_photon_dose::spec::{evaluate_chamber, evaluate_path, ChamberSpec, PathSpec};
use vcad_kernel_photon_dose::transport::{self, hvl_mm, run, PathConfig, Slab};

const PI4: f64 = 4.0 * std::f64::consts::PI;

fn narrow(slabs: Vec<Slab>, d: Vec<f64>) -> PathConfig {
    PathConfig {
        scatter: false,
        ..PathConfig::new(slabs, d)
    }
}

#[test]
fn uncollided_is_exact_exponential_times_inverse_square() {
    let s = Spectrum::line(662.0, 1.0e6);
    let pb = materials::lead();
    let res = run(&s, &narrow(vec![Slab::new(pb.clone(), 10.0)], vec![1000.0])).unwrap();
    let mu = pb.linear(662.0).unwrap().total;
    let mu_air = materials::air().linear(662.0).unwrap().total;
    let phi = 1.0e6 * (-mu * 1.0 - mu_air * 99.0).exp() / (PI4 * 100.0 * 100.0);
    let want = phi * h10_psv_cm2(662.0) * PSV_PER_S_TO_USV_PER_H;
    let got = res.detectors[0].h10_usv_h;
    assert!((got.mean / want - 1.0).abs() < 1e-12, "{} vs {want}", got.mean);
    assert_eq!(got.rse, 0.0, "uncollided-only has no statistical error");
}

#[test]
fn exponential_attenuation_composes() {
    // T(2t) = T(t)² for a line source, uncollided.
    let s = Spectrum::line(100.0, 1.0);
    let fe = materials::ss304();
    let one = run(&s, &narrow(vec![Slab::new(fe.clone(), 2.0)], vec![500.0])).unwrap();
    let two = run(&s, &narrow(vec![Slab::new(fe.clone(), 2.0), Slab::new(fe, 2.0)], vec![500.0])).unwrap();
    let bare = materials::air();
    // Air-only reference with the same distance (thin air slab as a stand-in for "nothing").
    let zero = run(&s, &narrow(vec![Slab::new(bare, 1.0e-9)], vec![500.0])).unwrap();
    let t1 = one.detectors[0].h10_usv_h.mean / zero.detectors[0].h10_usv_h.mean;
    let t2 = two.detectors[0].h10_usv_h.mean / zero.detectors[0].h10_usv_h.mean;
    assert!((t2 / (t1 * t1) - 1.0).abs() < 1e-9, "t1 {t1} t2 {t2}");
}

#[test]
fn inverse_square_in_free_field() {
    let s = Spectrum::line(1000.0, 1.0);
    let al = materials::aluminum();
    let res = run(&s, &narrow(vec![Slab::new(al, 1.0)], vec![500.0, 1000.0, 2000.0])).unwrap();
    let mu_air = materials::air().linear(1000.0).unwrap().total;
    let d = &res.detectors;
    let r1 = d[0].h10_usv_h.mean / d[1].h10_usv_h.mean;
    let r2 = d[1].h10_usv_h.mean / d[2].h10_usv_h.mean;
    assert!((r1 / (4.0 * (mu_air * 50.0).exp()) - 1.0).abs() < 1e-9);
    assert!((r2 / (4.0 * (mu_air * 100.0).exp()) - 1.0).abs() < 1e-9);
}

/// First broad-beam HVL (mm) from the full transport: thickness where
/// the on-axis H*(10) at 1 m falls to half its thin-slab value.
fn broad_hvl(m: &Material, e_kev: f64, grid: &[f64]) -> f64 {
    let s = Spectrum::line(e_kev, 1.0e9);
    let dose = |t: f64| {
        let mut c = PathConfig::new(vec![Slab::new(m.clone(), t)], vec![1000.0]);
        c.histories_per_batch = 20_000;
        c.batches = 10;
        run(&s, &c).unwrap().detectors[0].h10_usv_h.mean
    };
    let d0 = dose(1.0e-6);
    let mut prev = (1.0e-6, d0);
    for &t in grid {
        let d = dose(t);
        if d < 0.5 * d0 {
            // log-linear interpolation
            let (t0, y0) = prev;
            let f = ((0.5 * d0).ln() - y0.ln()) / (d.ln() - y0.ln());
            return t0 + f * (t - t0);
        }
        prev = (t, d);
    }
    panic!("HVL beyond grid");
}

#[test]
fn published_gamma_hvls_lead_and_steel() {
    // Narrow-beam from the data: ln2/μ.
    let pb = materials::lead();
    let fe = materials::ss304();
    let narrow_pb = std::f64::consts::LN_2 / pb.linear(662.0).unwrap().total * 10.0;
    assert!((narrow_pb - 5.5).abs() < 0.3, "narrow Pb HVL Cs-137 {narrow_pb} mm");
    // First broad-beam HVL (with buildup). Handbook shielding tables
    // (Radiological Health Handbook / NCRP-49 class) give Pb 6.5 mm and
    // steel ~16 mm for Cs-137. For Co-60 the handbook's 12 mm Pb is the
    // deep-penetration convention (TVL 40 mm / 3.32); the *first* HVL
    // with ANS-6.4.3 lead buildup B(1 mfp, 1.25 MeV) ≈ 1.36 solves
    // B·e^{−x} = ½ at x ≈ 1.0 mfp ≈ 15 mm — that is what the MC must hit.
    let pb_cs = broad_hvl(&pb, 662.0, &[4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    eprintln!("Pb Cs-137: narrow {narrow_pb:.2} mm, MC broad {pb_cs:.2} mm (handbook 6.5)");
    assert!(pb_cs > narrow_pb && (5.6..7.6).contains(&pb_cs), "Pb Cs-137 broad HVL {pb_cs}");
    let pb_co = broad_hvl(&pb, 1250.0, &[8.0, 10.0, 12.0, 14.0, 16.0, 18.0]);
    let narrow_co = std::f64::consts::LN_2 / pb.linear(1250.0).unwrap().total * 10.0;
    eprintln!("Pb Co-60: narrow {narrow_co:.2} mm, MC broad {pb_co:.2} mm (ANS-buildup first HVL ≈ 15)");
    assert!(pb_co > narrow_co && (13.0..17.0).contains(&pb_co), "Pb Co-60 broad HVL {pb_co}");
    let fe_cs = broad_hvl(&fe, 662.0, &[10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0]);
    let narrow_fe = std::f64::consts::LN_2 / fe.linear(662.0).unwrap().total * 10.0;
    eprintln!("steel Cs-137: narrow {narrow_fe:.2} mm, MC broad {fe_cs:.2} mm (handbook ~16)");
    assert!(fe_cs > narrow_fe && (12.5..19.0).contains(&fe_cs), "steel Cs-137 broad HVL {fe_cs}");
}

/// Additional lead thickness halving the narrow-beam kerma transmission
/// once the beam is already attenuated to `depth` (the hardened-beam
/// HVL that barrier-design tables quote).
fn deep_lead_hvl(spectrum: &Spectrum, prefilter: &[Slab], depth: f64) -> f64 {
    let pb = materials::lead();
    let base = transport::kerma_transmission(prefilter, spectrum);
    let t_at = |x: f64| {
        let mut s = prefilter.to_vec();
        s.push(Slab::new(pb.clone(), x));
        transport::kerma_transmission(&s, spectrum) / base
    };
    let solve = |target: f64| {
        let (mut lo, mut hi) = (1e-6f64, 100.0f64);
        for _ in 0..200 {
            let m = (lo * hi).sqrt();
            if t_at(m) > target {
                lo = m
            } else {
                hi = m
            }
        }
        lo
    };
    solve(0.5 * depth) - solve(depth)
}

#[test]
fn published_xray_hvls() {
    // Kramers spectra (shape is Z-independent) with 2.5 mm Al inherent
    // filtration, the diagnostic-tube convention.
    let spec = |kv: f64| {
        ThickTarget::new(kv, 1.0, materials::lead())
            .unwrap()
            .spectrum(10.0, 400)
            .unwrap()
    };
    let al = materials::aluminum();
    let pre = vec![Slab::new(al.clone(), 2.5)];
    // First Al HVL of a 100 kVp beam with 2.5 mm Al: 2.5–4 mm Al (IEC /
    // NCRP-102 tube data).
    let al_100 = hvl_mm(&al, &pre, &spec(100.0));
    let pb_first = hvl_mm(&materials::lead(), &pre, &spec(100.0));
    // Barrier tables (NCRP-49/147) quote the *hardened* lead HVL:
    // ≈ 0.25–0.27 mm at 100 kVp, ≈ 0.06 mm at 50 kVp. The transmission
    // window just below the Pb K edge (70–88 keV, HVL ≈ 0.25 mm) is
    // what sets it — a sharp test of the edge data and Kramers shape.
    let pb_100 = deep_lead_hvl(&spec(100.0), &pre, 1.0e-3);
    let pb_50 = deep_lead_hvl(&spec(50.0), &pre, 1.0e-3);
    eprintln!(
        "x-ray HVLs: Al@100kVp {al_100:.2} mm; Pb@100kVp first {pb_first:.3} mm, hardened {pb_100:.3} mm; Pb@50kVp hardened {pb_50:.3} mm"
    );
    assert!((2.0..4.5).contains(&al_100), "Al HVL @100 kVp {al_100} mm");
    assert!(pb_first < pb_100, "beam hardening must lengthen the HVL");
    assert!((0.18..0.34).contains(&pb_100), "hardened Pb HVL @100 kVp {pb_100} mm (tables 0.25-0.27)");
    assert!((0.035..0.09).contains(&pb_50), "hardened Pb HVL @50 kVp {pb_50} mm (tables ~0.06)");
}

#[test]
fn buildup_behaves() {
    // Water, 1 MeV, 2 mfp: scatter adds dose, but a finite slab with
    // void beyond stays below the infinite-medium point-isotropic
    // buildup (3.71, ANS-6.4.3).
    let w = materials::water();
    let mfp = 1.0 / w.linear(1000.0).unwrap().total * 10.0;
    let mut c = PathConfig::new(vec![Slab::new(w, 2.0 * mfp)], vec![2.0 * mfp + 500.0]);
    c.histories_per_batch = 20_000;
    c.batches = 10;
    let r = run(&Spectrum::line(1000.0, 1.0), &c).unwrap();
    let b = r.detectors[0].buildup_h10;
    eprintln!("water 1 MeV 2 mfp slab buildup {b:.3} (ANS-6.4.3 infinite medium 3.71)");
    assert!(b > 1.3 && b < 3.71, "water 1 MeV 2 mfp buildup {b}");
    // Lead, 1 MeV, 1 mfp: ANS-6.4.3 infinite-medium point-isotropic
    // exposure buildup 1.37; a finite slab with void behind sits a bit
    // below it.
    let pb = materials::lead();
    let mfp = 1.0 / pb.linear(1000.0).unwrap().total * 10.0;
    let mut c = PathConfig::new(vec![Slab::new(pb, mfp)], vec![mfp + 500.0]);
    c.histories_per_batch = 20_000;
    c.batches = 10;
    let b = run(&Spectrum::line(1000.0, 1.0), &c).unwrap().detectors[0].buildup_h10;
    eprintln!("lead 1 MeV 1 mfp slab buildup {b:.3} (ANS-6.4.3 infinite medium 1.37)");
    assert!(b > 1.1 && b < 1.45, "lead 1 MeV 1 mfp buildup {b}");
    // Lead at 30 keV is photoabsorption-dominated: buildup ≈ 1.
    let mut c = PathConfig::new(vec![Slab::new(materials::lead(), 0.1)], vec![500.0]);
    c.histories_per_batch = 5_000;
    c.batches = 5;
    let r = run(&Spectrum::line(30.0, 1.0), &c).unwrap();
    assert!(r.detectors[0].buildup_h10 < 1.05, "{}", r.detectors[0].buildup_h10);
}

#[test]
fn deterministic_and_seed_consistent() {
    let s = Spectrum::line(662.0, 1.0);
    let mk = |seed| {
        let mut c = PathConfig::new(vec![Slab::new(materials::ss304(), 20.0)], vec![1000.0]);
        c.histories_per_batch = 5_000;
        c.batches = 10;
        c.seed = seed;
        run(&s, &c).unwrap().detectors[0].h10_usv_h
    };
    let a = mk(1);
    assert_eq!(a, mk(1));
    let b = mk(2);
    let sig = (a.abs_sigma().powi(2) + b.abs_sigma().powi(2)).sqrt();
    assert!((a.mean - b.mean).abs() < 4.0 * sig, "{a} vs {b}");
}

#[test]
fn spectrum_binning_converged() {
    let t = ThickTarget::new(40.0, 10.0, materials::ss304()).unwrap();
    let slabs = vec![Slab::new(materials::borosilicate(), 5.0)];
    let d = |n| {
        run(&t.spectrum(10.0, n).unwrap(), &narrow(slabs.clone(), vec![500.0])).unwrap().detectors[0]
            .h10_usv_h
            .mean
    };
    let (a, b) = (d(120), d(480));
    assert!((a / b - 1.0).abs() < 0.01, "{a} vs {b}");
}

#[test]
fn soft_leak_fails_closed() {
    let t = ThickTarget::new(40.0, 1.0, materials::ss304()).unwrap();
    let thin = vec![Slab::new(materials::hdpe(), 1.0)];
    let e = run(&t.spectrum(10.0, 30).unwrap(), &narrow(thin, vec![500.0]));
    assert!(matches!(e, Err(transport::TransportError::SoftLeak { .. })));
}

#[test]
fn lead_glass_equivalence_is_plausible() {
    // 8 mm of 65%-PbO glass ≈ 1.5–2.5 mm Pb at 40 kV (vendor sheets for
    // ~4.8 g/cc grades quote ~0.22–0.28 mm Pb per mm at 100–150 kV,
    // more at low kV where Pb's L edges are not yet reached).
    let t = ThickTarget::new(40.0, 1.0, materials::ss304()).unwrap();
    let s = t.spectrum(10.0, 120).unwrap();
    let eq = transport::lead_equivalent_mm(&Slab::new(materials::lead_glass(), 8.0), &s);
    assert!((1.0..3.0).contains(&eq), "8 mm lead glass ≈ {eq} mm Pb at 40 kV");
}

#[test]
fn path_spec_receipt_is_provisional_and_fails_closed() {
    let spec: PathSpec = serde_json::from_str(
        r#"{
          "source": {"kind": "line", "energy_kev": 662, "rate_per_s": "S"},
          "layers": [{"material": "lead", "thickness_mm": 5}],
          "detectors": [{"label": "op", "distance_mm": 1000}],
          "run": {"histories_per_batch": 2000, "batches": 4, "seed": 9}
        }"#,
    )
    .unwrap();
    let p = BTreeMap::from([("S".to_string(), 3.7e7)]);
    let ev = evaluate_path(&spec, &p).unwrap();
    let set = receipt::predicted_claims(&ev).unwrap();
    assert_eq!(set.schema, "vcad.photon-claims/1");
    let h = set.claims.iter().find(|c| c.name == "h10_rate:op").unwrap();
    assert!(h.low < h.value && h.value < h.high && h.rse.is_finite());
    let rc = receipt::design_claims(&set);
    let r = vcad_receipt::DesignReceipt::with_claims(rc);
    assert_eq!(r.verdict(), vcad_receipt::ReceiptVerdict::Provisional);

    // Truncation fails closed.
    let mut bad = ev.clone();
    bad.truncated_histories = 1;
    assert!(receipt::predicted_claims(&bad).is_err());
    let mut bad = ev.clone();
    bad.scatter_scored = false;
    assert!(receipt::predicted_claims(&bad).is_err());

    // Survey binding.
    let m = |v| receipt::Measurement {
        name: "h10_rate:op".into(),
        value: v,
        uncertainty: 0.0,
        instrument: "test".into(),
    };
    let ok = receipt::compare(&set, &[m(h.value)]).unwrap();
    assert!(ok.iter().any(|e| e.verdict == receipt::Verdict::Holds));
    let bad = receipt::compare(&set, &[m(h.high * 10.0)]).unwrap();
    assert!(bad.iter().any(|e| e.verdict == receipt::Verdict::Violated));
    assert!(receipt::compare(&set, &[receipt::Measurement { name: "nope".into(), ..m(1.0) }]).is_err());
}

#[test]
fn chamber_dose_scales_linearly_with_current_and_steeply_with_voltage() {
    let mk = |kv: f64, ma: f64| -> f64 {
        let spec: ChamberSpec = serde_json::from_str(&format!(
            r#"{{
              "voltage_kv": {kv}, "current_ma": {ma}, "electron_fraction": 1.0,
              "chamber_radius_mm": 76, "wall": {{"material": "ss304", "thickness_mm": 1.65}},
              "viewport": {{"clear_radius_mm": 19, "layers": [{{"material": "borosilicate", "thickness_mm": 4.8}}]}},
              "detectors": [{{"label": "cam", "path": "viewport", "distance_mm": 1000}}],
              "run": {{"histories_per_batch": 4000, "batches": 4, "seed": 5}}
            }}"#
        ))
        .unwrap();
        evaluate_chamber(&spec, &BTreeMap::new()).unwrap().reports[0].h10_usv_h.mean
    };
    let a = mk(30.0, 10.0);
    let b = mk(30.0, 20.0);
    assert!((b / a - 2.0).abs() < 1e-9, "current scaling {}", b / a);
    let c = mk(40.0, 10.0);
    assert!(c > 2.0 * a, "40 kV {c} vs 30 kV {a}");
}
