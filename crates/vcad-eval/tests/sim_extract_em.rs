//! CAD → EM, one document: a loon-authored 3" loudspeaker motor (Y30
//! ferrite ring, T-yoke, top plate, 0.5 mm radial gap, voice coil)
//! extracted to `vcad_kernel_em::spec::AxisymSpec`, checked annulus-for-
//! annulus against the hand-built reference (`examples/speaker_motor.rs`
//! in vcad-kernel-em), solved, and bracketed by a magnetic-circuit
//! estimate. Then the fail-closed cases, each refused by part name.

use std::collections::BTreeMap;

use vcad_eval::sim_extract::em::{
    extract_axisym_mag_spec, AxisymMagExtraction, DomainSelection, MagPartSelection, MagRole, North,
};
use vcad_eval::sim_extract::SimExtractError;
use vcad_ir::Document;
use vcad_kernel_em::axisym::{Annulus, AxisymMagnetostatics};
use vcad_kernel_em::grid::SolveOptions;
use vcad_kernel_em::spec::ParamValue;

/// The motor. Pole is translated up 1 mm so the union has no coplanar
/// bottom faces; the magnet's bore tool overshoots both caps.
fn motor_source(extra: &str, pole: &str, plate_extra: &str) -> String {
    format!(
        "[defparam vc_turns 60.0]\n\
         [defparam vc_current 0.0]\n\
         [root-named [union [cylinder 30.0 4.0] {pole}] \"steel-1010\" \"yoke\"]\n\
         [root-named [translate 0.0 0.0 4.0 [difference [translate 0.0 0.0 -1.0 [cylinder 12.0 10.0]] \
           [cylinder 30.0 8.0]]] \"ferrite-y30\" \"magnet\"]\n\
         [root-named [translate 0.0 0.0 12.0 [difference [translate 0.0 0.0 -1.0 [cylinder 10.25 6.0]] \
           {plate}]] \"steel-1010\" \"top_plate\"]\n\
         [root-named [translate 0.0 0.0 10.0 [difference [translate 0.0 0.0 -1.0 [cylinder 9.85 10.0]] \
           [cylinder 10.15 8.0]]] \"copper\" \"voice_coil\"]\n\
         {extra}",
        plate = if plate_extra.is_empty() {
            "[cylinder 30.0 4.0]".to_string()
        } else {
            plate_extra.to_string()
        },
    )
}

const POLE: &str = "[translate 0.0 0.0 1.0 [cylinder 9.75 15.0]]";

fn doc_with(extra: &str, pole: &str, plate: &str) -> Document {
    vcad_loon::eval_vcad(&motor_source(extra, pole, plate), None).expect("loon fixture evaluates")
}

fn doc() -> Document {
    doc_with("", POLE, "")
}

fn part(name: &str) -> MagPartSelection {
    MagPartSelection {
        part: name.into(),
        material: None,
        role: None,
        mu_r: None,
        js_t: None,
        linear: false,
        br_t: None,
        north: North::PlusAxis,
        turns: None,
        current_a: None,
    }
}

fn selection() -> AxisymMagExtraction {
    let mut coil = part("voice_coil");
    coil.role = Some(MagRole::Coil);
    coil.turns = Some(ParamValue::Named("vc_turns".into()));
    coil.current_a = Some(ParamValue::Named("vc_current".into()));
    AxisymMagExtraction {
        parts: vec![part("yoke"), part("magnet"), part("top_plate"), coil],
        domain: DomainSelection {
            pad_factor: 0.0,
            r_max_mm: Some(60.0),
            z_min_mm: Some(-30.0),
            z_max_mm: Some(46.0),
            cells_per_feature: 2.0,
        },
        tolerance: Default::default(),
    }
}

fn ann(r0: f64, r1: f64, z0: f64, z1: f64) -> Annulus {
    Annulus {
        r_inner_mm: r0,
        r_outer_mm: r1,
        z_min_mm: z0,
        z_max_mm: z1,
    }
}

fn close(a: &Annulus, b: &Annulus) -> bool {
    (a.r_inner_mm - b.r_inner_mm).abs() < 1e-9
        && (a.r_outer_mm - b.r_outer_mm).abs() < 1e-9
        && (a.z_min_mm - b.z_min_mm).abs() < 1e-9
        && (a.z_max_mm - b.z_max_mm).abs() < 1e-9
}

#[test]
fn speaker_motor_extracts_to_the_hand_built_device() {
    let d = doc();
    let ex = extract_axisym_mag_spec(&d, &selection()).expect("extraction");
    assert!((ex.axis.direction[2] - 1.0).abs() < 1e-12);
    let dev = ex.spec.resolve(&ex.parameters).expect("spec resolves");

    // Hand-built reference (same as examples/speaker_motor.rs).
    let steel_ref = [
        ann(0.0, 30.0, 0.0, 4.0),
        ann(0.0, 9.75, 4.0, 16.0),
        ann(10.25, 30.0, 12.0, 16.0),
    ];
    assert_eq!(dev.materials.len(), 3, "{:?}", ex.parts);
    for r in &steel_ref {
        let m = dev
            .materials
            .iter()
            .find(|m| close(&m.region, r))
            .unwrap_or_else(|| panic!("no steel annulus {r:?} in {:?}", dev.materials));
        assert_eq!(m.mu_r, 1000.0);
        assert_eq!(m.sat.map(|s| s.js_t), Some(2.0));
    }
    assert_eq!(dev.magnets.len(), 1);
    assert!(close(&dev.magnets[0].region, &ann(12.0, 30.0, 4.0, 12.0)));
    assert_eq!(dev.magnets[0].br_z_t, 0.39);
    assert_eq!(dev.magnets[0].mu_r, 1.05);
    assert_eq!(dev.coils.len(), 1);
    assert!(close(&dev.coils[0].region, &ann(9.85, 10.15, 10.0, 18.0)));
    assert_eq!(dev.coils[0].turns, 60.0);
    // Named drive is carried as a name, not baked in.
    assert_eq!(ex.spec.coils[0].turns, ParamValue::Named("vc_turns".into()));
    // Yoke is a T: two annuli from one part, volume-checked.
    let yoke = ex.parts.iter().find(|p| p.part == "yoke").unwrap();
    assert_eq!(yoke.annuli.len(), 2);
    assert!((yoke.volume_mm3[0] - yoke.volume_mm3[1]).abs() < 0.01 * yoke.volume_mm3[1]);
    // Thinnest material feature is the 0.5 mm gap.
    assert!(
        (ex.grid.thinnest_feature_mm - 0.5).abs() < 1e-9,
        "{:?}",
        ex.grid
    );
    assert!((ex.grid.dx_mm - 0.25).abs() < 1e-9);
}

/// Gap flux and Bl from the extracted device, bracketed by a
/// magnetic-circuit estimate. The circuit ignores leakage — large in
/// ferrite motors (the stray field around the ring) — so it is an upper
/// bound; real motors read 1/1.5–1/3 of it.
#[test]
fn extracted_motor_gap_flux_sits_in_the_magnetic_circuit_band() {
    let d = doc();
    let mut sel = selection();
    for p in sel
        .parts
        .iter_mut()
        .filter(|p| p.part != "voice_coil" && p.part != "magnet")
    {
        p.linear = true; // one linear solve keeps the test fast
    }
    let ex = extract_axisym_mag_spec(&d, &sel).expect("extraction");
    let dev: AxisymMagnetostatics = ex.spec.resolve(&ex.parameters).unwrap();
    let g = ex.grid;
    let sol = dev
        .solve(g.nx, g.ny, &SolveOptions::default())
        .expect("solve");
    let (br_gap, _) = sol.mean_field_in(&ann(9.75, 10.25, 12.0, 16.0), 16);

    let mu0 = 4e-7 * std::f64::consts::PI;
    let mmf = 0.39 / (mu0 * 1.05) * 8e-3;
    let a_g = 2.0 * std::f64::consts::PI * 10e-3 * 4e-3;
    let a_m = std::f64::consts::PI * (30e-3f64.powi(2) - 12e-3f64.powi(2));
    let b_circ = mmf / (0.5e-3 / (mu0 * a_g) + 8e-3 / (mu0 * 1.05 * a_m)) / a_g;
    assert!(
        br_gap < b_circ && br_gap > b_circ / 3.0,
        "gap |B_r| {br_gap:.3} T outside the circuit band ({:.3}, {b_circ:.3}) T",
        b_circ / 3.0
    );
    // Bl by two routes (ψ-difference functional vs sampled field).
    let bl = sol.force_factor(0);
    let bl2 = sol.force_factor_sampled(0, 32);
    assert!(bl.abs() > 1.0, "Bl {bl}");
    assert!(
        (bl - bl2).abs() < 0.05 * bl.abs(),
        "Bl routes {bl} vs {bl2}"
    );
    // North up → field in the gap points inward (pole is south side? the
    // flux leaves the top plate's bore toward the pole): B_r < 0.
    let (br_mid, _) = sol.b_at(10.0e-3, 14.0e-3);
    assert!(br_mid < 0.0);
}

fn expect_not_repr(res: Result<impl std::fmt::Debug, SimExtractError>, part: &str, needle: &str) {
    match res {
        Err(SimExtractError::NotRepresentable { part: p, detail }) => {
            assert_eq!(p, part, "{detail}");
            assert!(detail.contains(needle), "{detail:?} lacks {needle:?}");
        }
        other => panic!("expected NotRepresentable({part}), got {other:?}"),
    }
}

#[test]
fn chamfered_pole_is_refused_by_name() {
    let pole = "[union [translate 0.0 0.0 1.0 [cylinder 9.75 14.0]] \
                [translate 0.0 0.0 15.0 [cone 9.75 9.25 1.0]]]";
    let d = doc_with("", pole, "");
    expect_not_repr(extract_axisym_mag_spec(&d, &selection()), "yoke", "cone");
}

#[test]
fn off_axis_vent_hole_is_refused_by_name() {
    // plate = outer − (bore ∪ vent): the vent is a hole through the plate.
    let src = motor_source("", POLE, "").replace(
        "[translate 0.0 0.0 -1.0 [cylinder 10.25 6.0]]",
        "[union [translate 0.0 0.0 -1.0 [cylinder 10.25 6.0]] [translate 20.0 0.0 -1.0 [cylinder 1.0 6.0]]]",
    );
    let d = vcad_loon::eval_vcad(&src, None).unwrap();
    expect_not_repr(
        extract_axisym_mag_spec(&d, &selection()),
        "top_plate",
        "not coaxial",
    );
}

#[test]
fn unknown_material_and_missing_turns_fail_closed() {
    let d = doc();
    let mut sel = selection();
    sel.parts[0].material = Some("pla".into());
    expect_not_repr(
        extract_axisym_mag_spec(&d, &sel),
        "yoke",
        "not in the magnetic material table",
    );

    let mut sel = selection();
    sel.parts[3].turns = None;
    expect_not_repr(
        extract_axisym_mag_spec(&d, &sel),
        "voice_coil",
        "needs `turns`",
    );

    let mut sel = selection();
    sel.parts[3].current_a = Some(ParamValue::Named("nope".into()));
    assert!(matches!(
        extract_axisym_mag_spec(&d, &sel),
        Err(SimExtractError::UnknownParameter { .. })
    ));
}

#[test]
fn a_grid_that_cannot_resolve_the_gap_is_refused() {
    let d = doc();
    let mut sel = selection();
    sel.domain.cells_per_feature = 40.0;
    match extract_axisym_mag_spec(&d, &sel) {
        Err(SimExtractError::Invalid(msg)) => assert!(msg.contains("close the gap"), "{msg}"),
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[test]
fn south_up_magnet_flips_the_remanence() {
    let d = doc();
    let mut sel = selection();
    sel.parts[1].north = North::MinusAxis;
    let ex = extract_axisym_mag_spec(&d, &sel).unwrap();
    let dev = ex
        .spec
        .resolve(&BTreeMap::from([
            ("vc_turns".to_string(), 60.0),
            ("vc_current".to_string(), 0.0),
        ]))
        .unwrap();
    assert_eq!(dev.magnets[0].br_z_t, -0.39);
}
