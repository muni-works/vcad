//! Document → acoustic cavity on the "every physics" puck: a cylindrical
//! bass-reflex speaker whose port hangs INSIDE the box. Extract the field
//! geometry and lumped quantities from the document's own BRep, check them
//! against the hand-computed values (and the PoC's hand mapping), run the
//! solver, then the fail-closed cases.

use std::f64::consts::PI;

use vcad_eval::cavity_extract::{extract_cavity, CavityExtractError, CavityExtraction};
use vcad_ir::Document;
use vcad_kernel_acoustics::cavity::Cavity;
use vcad_kernel_acoustics::fom;
use vcad_kernel_acoustics::lumped::{self, PortMount};
use vcad_kernel_acoustics::medium::Medium;
use vcad_kernel_acoustics::ported::{analyze, AnalysisOptions};
use vcad_kernel_acoustics::spec::{EndSpec, ParamValue};

const PUCK: &str = include_str!("fixtures/puck.loon");

fn doc(src: &str) -> Document {
    vcad_loon::eval_vcad(src, None).expect("loon fixture evaluates")
}

fn sel() -> CavityExtraction {
    CavityExtraction {
        enclosure: "enclosure".into(),
        // The puck's air root is unnamed; select it by its material.
        air: "air".into(),
        driver_radius: Some(ParamValue::Named("driver_r".into())),
        temp_c: None,
        tolerance: Default::default(),
    }
}

/// π·(52²·114 − 14²·57): cavity minus the port tube's outer cylinder.
fn puck_v_box() -> f64 {
    PI * (52.0f64.powi(2) * 114.0 - 14.0f64.powi(2) * 57.0)
}

fn lit(v: &ParamValue) -> f64 {
    match v {
        ParamValue::Literal(x) => *x,
        ParamValue::Named(n) => panic!("expected a literal, got {n:?}"),
    }
}

#[test]
fn puck_extracts_internal_port_geometry() {
    let ex = extract_cavity(&doc(PUCK), &sel()).expect("puck extracts");
    let m = &ex.model;
    // V_box is the air solid's volume, exactly (PoC: 0.933 L).
    assert!(
        (ex.air_volume_mm3 - puck_v_box()).abs() < 1e-6 * puck_v_box(),
        "V_box {} vs {}",
        ex.air_volume_mm3,
        puck_v_box()
    );
    assert_eq!(m.box_volume_mm3, ex.air_volume_mm3);
    assert!((ex.air_volume_mm3 * 1e-6 - 0.933).abs() < 5e-4);
    // Tessellated mesh agrees to within the polygonal approximation.
    assert!((ex.air_volume_mesh_mm3 - ex.air_volume_mm3).abs() < 0.01 * ex.air_volume_mm3);
    // Port: 12 mm bore, 60 mm from its hanging end to the mouth, internal.
    assert!((m.port_radius_mm - 12.0).abs() < 1e-9);
    assert!((m.port_length_mm - 60.0).abs() < 1e-9);
    assert_eq!(m.mount, PortMount::Internal);
    assert_eq!(ex.driver_cutout_radius_mm, Some(35.0));
    // Field stack: box (r 52, 114 tall) + top-wall bore (r 12, 3 mm), with
    // the tube wall (12 < r < 14) hanging 57 mm into the box.
    let segs: Vec<[f64; 3]> = m
        .spec
        .segments
        .iter()
        .map(|s| [lit(&s.z0_mm), lit(&s.z1_mm), lit(&s.radius_mm)])
        .collect();
    let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9);
    assert_eq!(segs.len(), 2, "{segs:?}");
    assert!(close(&segs[0], &[0.0, 114.0, 52.0]), "{segs:?}");
    assert!(close(&segs[1], &[114.0, 117.0, 12.0]), "{segs:?}");
    assert_eq!(m.spec.obstacles.len(), 1);
    let o = &m.spec.obstacles[0];
    assert!(close(
        &[
            lit(&o.r_in_mm),
            lit(&o.r_out_mm),
            lit(&o.z0_mm),
            lit(&o.z1_mm)
        ],
        &[12.0, 14.0, 57.0, 114.0]
    ));
    // Piston radius stays bound to the document parameter.
    assert_eq!(
        m.spec.bottom,
        EndSpec::Piston {
            radius_mm: ParamValue::Named("driver_r".into())
        }
    );
    assert_eq!(ex.parameters["driver_r"], 32.0);
    // Axis is world +Z (port vents up), spec z = 0 at the cavity floor.
    assert!(close(&ex.axis, &[0.0, 0.0, 1.0]));
    assert!(close(&ex.origin, &[0.0, 0.0, 3.0]));
    // Probe is in the box air below the port.
    assert!(m.probe_mm[1] < 57.0);
    assert!((m.grid_mm - 1.0).abs() < 1e-9);
    // The resolved field cavity's fluid = box air + port bore.
    let cav = m.spec.resolve(&ex.parameters).unwrap();
    let bore = PI * 144.0 * 60.0;
    assert!((cav.volume_mm3() - (puck_v_box() + bore)).abs() < 1e-6 * cav.volume_mm3());
}

#[test]
fn puck_lumped_band_matches_the_poc_hand_mapping() {
    let ex = extract_cavity(&doc(PUCK), &sel()).unwrap();
    let air = Medium::air(20.0);
    // PoC: lumped::ported_box_tuning_mm(V_air, port_r, port_len) → 136.6 Hz.
    let poc = lumped::ported_box_tuning_mm(&air, puck_v_box(), 12.0, 60.0);
    let band = lumped::ported_box_tuning_mount_mm(
        &air,
        ex.model.box_volume_mm3,
        ex.model.port_radius_mm,
        ex.model.port_length_mm,
        ex.model.mount,
    );
    assert!((band.f_nominal_hz - 136.6).abs() < 0.05, "{band:?}");
    assert!((band.f_nominal_hz - poc.f_nominal_hz).abs() < 1e-9);
    assert!((band.f_min_hz - poc.f_min_hz).abs() < 1e-9);
    // Internal mount: the interior end is unflanged, so the band top rises.
    assert!(band.f_max_hz > poc.f_max_hz);
}

#[test]
fn puck_field_tuning_runs_from_the_document() {
    let ex = extract_cavity(&doc(PUCK), &sel()).unwrap();
    let rep = analyze(
        &ex.model,
        &ex.parameters,
        &AnalysisOptions {
            sweep_points: 28,
            curve_points: 8,
            ..Default::default()
        },
    )
    .expect("analyze puck");
    let f = rep.field_tuning_hz.expect("tuning peak");
    // The PoC's hand mapping: equivalent-volume cylinder of the cavity
    // radius with the port moved outside the box.
    let air = Medium::air(20.0);
    let h_eq = puck_v_box() / (PI * 52.0 * 52.0);
    let poc_cav = Cavity::ported_box(52.0, h_eq, 12.0, 60.0, 32.0, air);
    let (z0, z1) = poc_cav.z_span_mm();
    let nz = (((z1 - z0) / 2.0).round() as usize).max(60) + 1;
    let poc_f = fom::driven_tuning_hz(
        &poc_cav,
        27,
        nz,
        (rep.lumped.f_min_hz * 0.6).max(10.0),
        rep.lumped.f_max_hz * 1.8,
        40,
    );
    // Same mapping on the same 1 mm grid, to separate the port topology
    // from the PoC's coarser discretisation.
    let poc_f_fine = fom::driven_tuning_hz(
        &poc_cav,
        53,
        ((z1 - z0).ceil() as usize) + 1,
        (rep.lumped.f_min_hz * 0.6).max(10.0),
        rep.lumped.f_max_hz * 1.8,
        40,
    );
    println!("puck: PoC mapping on the 1 mm grid {poc_f_fine:?} Hz");
    println!(
        "puck: lumped [{:.1}, {:.1}, {:.1}] Hz; field f_b (internal port, doc) {f:.1} Hz \
         (field/nominal {:.3}); PoC equivalent-volume external-port mapping {poc_f:?} Hz; \
         grid {:?}",
        rep.lumped.f_min_hz,
        rep.lumped.f_nominal_hz,
        rep.lumped.f_max_hz,
        rep.field_over_nominal.unwrap(),
        rep.grid
    );
    assert!(
        f > rep.lumped.f_min_hz && f < 1.25 * rep.lumped.f_max_hz,
        "field {f} vs {:?}",
        rep.lumped
    );
    assert!(rep.claims.claims.iter().any(|c| c.name == "tuning_hz"));
    assert!(rep
        .claims
        .claims
        .iter()
        .any(|c| c.name == "tuning_hz_lumped" && c.note.contains("internal port")));
    // The port output peaks near the tuning.
    let peak = rep
        .port_velocity
        .iter()
        .max_by(|a, b| a.port_to_cone.total_cmp(&b.port_to_cone))
        .unwrap();
    assert!(peak.port_to_cone > 1.0, "{peak:?}");
}

#[test]
fn scrubbing_a_parameter_reprices_the_extraction() {
    let src = PUCK.replace("[defparam port_len 60.0", "[defparam port_len 80.0");
    let ex = extract_cavity(&doc(&src), &sel()).unwrap();
    assert!((ex.model.port_length_mm - 80.0).abs() < 1e-9);
    let v = PI * (52.0f64.powi(2) * 114.0 - 14.0f64.powi(2) * 77.0);
    assert!((ex.model.box_volume_mm3 - v).abs() < 1e-6 * v);
}

#[test]
fn default_driver_radius_is_the_cutout_and_is_stated() {
    let mut s = sel();
    s.driver_radius = None;
    let ex = extract_cavity(&doc(PUCK), &s).unwrap();
    assert_eq!(
        ex.model.spec.bottom,
        EndSpec::Piston {
            radius_mm: ParamValue::Literal(35.0)
        }
    );
    assert!(ex.assumptions.iter().any(|a| a.contains("cutout radius")));
}

// ---------------------------------------------------------------------------
// Fail-closed
// ---------------------------------------------------------------------------

fn err(src: &str, s: &CavityExtraction) -> CavityExtractError {
    extract_cavity(&doc(src), s).expect_err("must fail closed")
}

#[test]
fn a_box_enclosure_is_refused_naming_the_face() {
    let src = PUCK.replace(
        "[let outer [cylinder body_r body_h]]",
        "[let outer [translate [- 0.0 body_r] [- 0.0 body_r] 0.0 [cube [* 2.0 body_r] [* 2.0 body_r] body_h]]]",
    );
    let e = err(&src, &sel());
    let msg = e.to_string();
    assert!(
        matches!(e, CavityExtractError::NonAxisymmetricFace { ref part, .. } if part == "enclosure"),
        "{msg}"
    );
    assert!(
        msg.contains("face #") && msg.contains("not perpendicular"),
        "{msg}"
    );
}

#[test]
fn an_off_axis_port_is_refused() {
    let src = PUCK
        .replace(
            "[let port-tube [translate 0.0 0.0",
            "[let port-tube [translate 5.0 0.0",
        )
        .replace(
            "[let port-bore [translate 0.0 0.0",
            "[let port-bore [translate 5.0 0.0",
        );
    let e = err(&src, &sel());
    assert!(
        matches!(e, CavityExtractError::NonAxisymmetricFace { .. }),
        "{e}"
    );
    // Whichever face the walk meets first: the shifted tube's cylinder, or
    // the top wall whose hole is no longer a circle about the axis.
    let msg = e.to_string();
    assert!(
        msg.contains("not coaxial") || msg.contains("not a circle about the axis"),
        "{msg}"
    );
}

#[test]
fn air_that_does_not_fill_the_cavity_is_refused() {
    // Air 1 mm short of the wall: a sideways gap.
    let src = PUCK.replace(
        "[root [difference port-tube cavity] \"air\"]",
        "[root [difference port-tube [translate 0.0 0.0 wall [cylinder [- ri 1.0] hi]]] \"air\"]",
    );
    let e = err(&src, &sel());
    assert!(e.to_string().contains("sideways"), "{e}");
}

#[test]
fn unknown_parameter_and_oversized_piston_are_refused() {
    let mut s = sel();
    s.driver_radius = Some(ParamValue::Named("cone_r".into()));
    assert!(matches!(
        err(PUCK, &s),
        CavityExtractError::UnknownParameter { ref name, .. } if name == "cone_r"
    ));
    s.driver_radius = Some(ParamValue::Literal(40.0));
    assert!(err(PUCK, &s)
        .to_string()
        .contains("exceeds the driver cutout"));
}

#[test]
fn a_sealed_box_is_not_mistaken_for_a_ported_one() {
    // No port: the driver cutout is the only opening.
    let src = PUCK
        .replace(
            "[let s3 [union port-tube s2]]\n[let enclosure [difference port-bore s3]]",
            "[let enclosure s2]",
        )
        .replace(
            "[root [difference port-tube cavity] \"air\"]",
            "[root cavity \"air\"]",
        );
    assert!(src.contains("[let enclosure s2]"), "fixture text drifted");
    let msg = err(&src, &sel()).to_string();
    assert!(msg.contains("sealed box"), "{msg}");
}

#[test]
fn unknown_part_lists_what_is_there() {
    let mut s = sel();
    s.enclosure = "shell".into();
    let msg = err(PUCK, &s).to_string();
    assert!(msg.contains("enclosure"), "{msg}");
}
