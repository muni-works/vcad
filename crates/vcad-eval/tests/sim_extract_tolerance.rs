//! CAD → tolerance stack, one document: a lid pressed into a rim, both
//! FDM-printed (±0.1 mm on each diameter). The clearance assertion names
//! the fit; the chain (coefficients ±½ on the two diameters), the
//! requirement and the nominal interference are derived from the
//! document, then priced by worst case, RSS and Monte Carlo.

use vcad_eval::sim_extract::tolerance::{extract_stackup_spec, FitFeature, StackupExtraction};
use vcad_eval::sim_extract::SimExtractError;
use vcad_ir::{ClearanceSpec, Document};
use vcad_kernel_tolerance::analysis::{monte_carlo, rss, worst_case, McOptions};

/// Rim: a Ø70 cup, 20 tall, with a Ø`rim_id` × 6 mm socket at the top.
/// Lid: a Ø`lid_od` × 6 mm plug under a Ø70 × 2 mm flange, seated.
fn source(lid_tol: &str, extra: &str) -> String {
    format!(
        "[defparam rim_id 60.0 :tol 0.1 :unit \"mm\"]\n\
         [defparam lid_od 60.15 {lid_tol} :unit \"mm\"]\n\
         [defparam rim_h 20.0]\n\
         {extra}\
         [root-named [difference [translate 0.0 0.0 14.0 [cylinder [* 0.5 rim_id] 7.0]] \
           [cylinder 35.0 rim_h]] \"pla\" \"rim\"]\n\
         [root-named [union [translate 0.0 0.0 14.0 [cylinder [* 0.5 lid_od] 6.5]] \
           [translate 0.0 0.0 20.0 [cylinder 35.0 2.0]]] \"pla\" \"lid\"]\n"
    )
}

fn doc_from(src: &str) -> Document {
    let mut d = vcad_loon::eval_vcad(src, None).expect("loon fixture evaluates");
    // Press fit: crack above 0.15 mm radial interference.
    d.clearance_specs.push(ClearanceSpec {
        label: "lid-fit".into(),
        group_a: vec!["lid".into()],
        group_b: vec!["rim".into()],
        min_mm: -0.15,
        allow_contact: None,
        sweep: None,
        baseline_mm: None,
    });
    d
}

fn sel() -> StackupExtraction {
    StackupExtraction {
        clearance: "lid-fit".into(),
        feature: FitFeature::Radial,
        fit_window_mm: 1.0,
        // Rattles below 0.01 mm interference.
        upper_mm: Some(-0.01),
        lower_mm: None,
        process_tolerance_mm: None,
        parameters: None,
        tolerance: Default::default(),
    }
}

#[test]
fn press_fit_stack_is_derived_from_the_document() {
    let d = doc_from(&source(":tol 0.1", ""));
    let ex = extract_stackup_spec(&d, &sel()).expect("extraction");
    assert!((ex.gap_mm + 0.075).abs() < 1e-9, "gap {}", ex.gap_mm);
    assert_eq!(ex.fit_parts, ["lid".to_string(), "rim".to_string()]);
    let coeff = |n: &str| {
        ex.contributors
            .iter()
            .find(|c| c.parameter == n)
            .unwrap()
            .coeff
    };
    assert!((coeff("rim_id") - 0.5).abs() < 1e-9);
    assert!((coeff("lid_od") + 0.5).abs() < 1e-9);
    assert_eq!(ex.independent, vec!["rim_h".to_string()]);
    assert!(ex.fixed_geometry_mm.abs() < 1e-9);

    let stack = ex.spec.resolve(&ex.parameters).expect("resolves");
    assert!((stack.nominal_gap() - ex.gap_mm).abs() < 1e-12);
    assert_eq!(stack.requirement.name, "lid-fit");
    assert_eq!(stack.requirement.lower_mm, Some(-0.15));
    assert_eq!(stack.requirement.upper_mm, Some(-0.01));

    // Worst case: −0.075 ± 0.1 spans [−0.175, 0.025] — fails both ends.
    let wc = worst_case(&stack).unwrap();
    assert!((wc.min_gap + 0.175).abs() < 1e-9 && (wc.max_gap - 0.025).abs() < 1e-9);
    assert!(!wc.passes);
    // RSS: σ = √2·½·0.1/3.
    let r = rss(&stack).unwrap();
    let sigma = (2.0f64).sqrt() * 0.5 * 0.1 / 3.0;
    assert!((r.sigma_gap - sigma).abs() < 1e-12);
    // MC agrees with the normal yield within its error bars.
    let mc = monte_carlo(
        &stack,
        &McOptions {
            n: 200_000,
            seed: 7,
            batches: 20,
        },
    )
    .unwrap();
    assert!(
        (mc.fit.p - r.yield_estimate).abs() < 5.0 * mc.fit.standard_error.max(1e-4),
        "MC {} vs RSS {}",
        mc.fit.p,
        r.yield_estimate
    );
    assert!(r.yield_estimate > 0.99 && r.yield_estimate < 0.999);
}

#[test]
fn a_dimension_without_a_tolerance_fails_closed_unless_a_process_default_is_given() {
    let d = doc_from(&source("", ""));
    match extract_stackup_spec(&d, &sel()) {
        Err(SimExtractError::Invalid(m)) => {
            assert!(m.contains("lid_od") && m.contains("no tolerance"), "{m}")
        }
        other => panic!("expected refusal, got {other:?}"),
    }
    let mut s = sel();
    s.process_tolerance_mm = Some(0.1);
    let ex = extract_stackup_spec(&d, &s).expect("process default applies");
    let lid = ex
        .contributors
        .iter()
        .find(|c| c.parameter == "lid_od")
        .unwrap();
    assert_eq!(lid.tol_source, "process");
    assert_eq!(lid.tol, (0.1, 0.1));
}

#[test]
fn asymmetric_tolerance_centres_the_distribution() {
    let d = doc_from(&source(":tol-plus 0.0 :tol-minus 0.2", ""));
    let ex = extract_stackup_spec(&d, &sel()).unwrap();
    let stack = ex.spec.resolve(&ex.parameters).unwrap();
    // lid_od mean shifts −0.1 → gap mean shifts +0.05.
    assert!((stack.mean_gap() - (ex.gap_mm + 0.05)).abs() < 1e-12);
}

#[test]
fn missing_assertion_and_absent_fit_fail_closed() {
    let d = doc_from(&source(":tol 0.1", ""));
    let mut s = sel();
    s.clearance = "nope".into();
    assert!(
        matches!(extract_stackup_spec(&d, &s), Err(SimExtractError::Invalid(m)) if m.contains("lid-fit"))
    );
    let mut s = sel();
    s.fit_window_mm = 0.01; // the 0.075 interference is outside
    assert!(
        matches!(extract_stackup_spec(&d, &s), Err(SimExtractError::Invalid(m)) if m.contains("no Radial fit"))
    );
}
