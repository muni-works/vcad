//! Document → acoustic cavity on the v2 "every physics" puck, whose speaker
//! motor (yoke, magnet, top plate, voice coil) sits inside the box and is
//! carved out of the air as a coaxial r = 30 mm, 18 mm tall envelope. The
//! motor parts are listed as rigid obstacles; the extractor must take the
//! air's own volume as V_box, emit the envelope as one solid-core obstacle,
//! and still fail closed on everything it can't represent.

use std::f64::consts::PI;

use vcad_eval::cavity_extract::{extract_cavity, CavityExtractError, CavityExtraction};
use vcad_ir::Document;
use vcad_kernel_acoustics::lumped::PortMount;
use vcad_kernel_acoustics::ported::{analyze, AnalysisOptions};
use vcad_kernel_acoustics::spec::ParamValue;

const PUCK: &str = include_str!("fixtures/puck_v2.loon");
const MOTOR: [&str; 4] = ["yoke", "magnet", "top_plate", "voice_coil"];

fn doc(src: &str) -> Document {
    vcad_loon::eval_vcad(src, None).expect("loon fixture evaluates")
}

fn sel(obstacles: &[&str]) -> CavityExtraction {
    CavityExtraction {
        enclosure: "enclosure".into(),
        air: "air".into(),
        obstacles: obstacles.iter().map(|s| s.to_string()).collect(),
        driver_radius: Some(ParamValue::Named("driver_r".into())),
        temp_c: None,
        tolerance: Default::default(),
    }
}

/// π·(52²·114 − 14²·57 − 30²·18): cavity minus the port tube's outer
/// cylinder minus the motor envelope.
fn v_box() -> f64 {
    PI * (52.0f64.powi(2) * 114.0 - 14.0f64.powi(2) * 57.0 - 30.0f64.powi(2) * 18.0)
}

fn lit(v: &ParamValue) -> f64 {
    match v {
        ParamValue::Literal(x) => *x,
        ParamValue::Named(n) => panic!("expected a literal, got {n:?}"),
    }
}

fn close(a: &[f64], b: &[f64]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

#[test]
fn v2_puck_extracts_with_the_motor_as_an_obstacle() {
    let ex = extract_cavity(&doc(PUCK), &sel(&MOTOR)).expect("v2 puck extracts");
    let m = &ex.model;
    assert!(
        (ex.air_volume_mm3 - v_box()).abs() < 1e-6 * v_box(),
        "V_box {} vs {}",
        ex.air_volume_mm3,
        v_box()
    );
    assert_eq!(m.box_volume_mm3, ex.air_volume_mm3);
    assert!((ex.air_volume_mesh_mm3 - ex.air_volume_mm3).abs() < 0.01 * ex.air_volume_mm3);
    // The motor parts plus the void they don't fill = the carved envelope.
    let envelope = PI * 30.0 * 30.0 * 18.0;
    assert!(
        (ex.obstacle_solid_mm3 + ex.obstacle_void_mm3 - envelope).abs() < 1e-6 * envelope,
        "solid {} + void {} vs envelope {envelope}",
        ex.obstacle_solid_mm3,
        ex.obstacle_void_mm3
    );
    assert!(ex.obstacle_solid_mm3 > 0.0 && ex.obstacle_void_mm3 > 0.0);
    assert_eq!(ex.obstacle_parts, MOTOR.map(String::from).to_vec());
    assert!(ex.assumptions.iter().any(|a| a.contains("Rigid obstacles")));

    // Port and stack are the v1 ones.
    assert!((m.port_radius_mm - 12.0).abs() < 1e-9);
    assert!((m.port_length_mm - 60.0).abs() < 1e-9);
    assert_eq!(m.mount, PortMount::Internal);
    let segs: Vec<[f64; 3]> = m
        .spec
        .segments
        .iter()
        .map(|s| [lit(&s.z0_mm), lit(&s.z1_mm), lit(&s.radius_mm)])
        .collect();
    assert_eq!(segs.len(), 2, "{segs:?}");
    assert!(close(&segs[0], &[0.0, 114.0, 52.0]), "{segs:?}");
    assert!(close(&segs[1], &[114.0, 117.0, 12.0]), "{segs:?}");
    // Obstacles: the motor envelope (solid core, r_in = 0) 15 mm above the
    // cavity floor, and the port tube wall.
    let mut obs: Vec<[f64; 4]> = m
        .spec
        .obstacles
        .iter()
        .map(|o| {
            [
                lit(&o.r_in_mm),
                lit(&o.r_out_mm),
                lit(&o.z0_mm),
                lit(&o.z1_mm),
            ]
        })
        .collect();
    obs.sort_by(|a, b| a[2].total_cmp(&b[2]));
    assert_eq!(obs.len(), 2, "{obs:?}");
    assert!(close(&obs[0], &[0.0, 30.0, 15.0, 33.0]), "{obs:?}");
    assert!(close(&obs[1], &[12.0, 14.0, 57.0, 114.0]), "{obs:?}");
    // The motor's internal levels don't shrink the grid.
    assert!((m.grid_mm - 1.0).abs() < 1e-9, "grid {}", m.grid_mm);
    // Probe in the air between the driver face and the motor, on the axis.
    assert!(
        m.probe_mm[0] == 0.0 && m.probe_mm[1] < 15.0,
        "{:?}",
        m.probe_mm
    );
    // Field fluid = V_box + port bore.
    let cav = m.spec.resolve(&ex.parameters).unwrap();
    let bore = PI * 144.0 * 60.0;
    assert!((cav.volume_mm3() - (v_box() + bore)).abs() < 1e-6 * cav.volume_mm3());
}

#[test]
fn v2_puck_field_tuning_runs() {
    let ex = extract_cavity(&doc(PUCK), &sel(&MOTOR)).unwrap();
    let rep = analyze(
        &ex.model,
        &ex.parameters,
        &AnalysisOptions {
            sweep_points: 28,
            curve_points: 8,
            ..Default::default()
        },
    )
    .expect("analyze v2 puck");
    let f = rep.field_tuning_hz.expect("tuning peak");
    eprintln!("v2 puck: lumped {:?}, field f_b {f:.2} Hz", rep.lumped);
    // Smaller box than v1 (0.882 L vs 0.933 L) → tuned higher than the
    // lumped nominal, and in the same neighbourhood as v1's 151 Hz.
    assert!(f > rep.lumped.f_nominal_hz, "{f} vs {:?}", rep.lumped);
    assert!((140.0..175.0).contains(&f), "field f_b {f}");
}

#[test]
fn v2_puck_without_obstacles_refuses_and_says_how_to_fix_it() {
    let e = extract_cavity(&doc(PUCK), &sel(&[])).unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("sideways") && msg.contains("obstacles"),
        "{msg}"
    );
}

#[test]
fn non_axisymmetric_obstacle_is_refused_by_name() {
    let e = extract_cavity(&doc(PUCK), &sel(&["yoke", "board"])).unwrap_err();
    match e {
        CavityExtractError::NonAxisymmetricFace { part, .. } => assert_eq!(part, "board"),
        other => panic!("expected a non-axisymmetric face on board, got {other}"),
    }
}

#[test]
fn off_axis_obstacle_is_refused_by_name() {
    let marker = "[root-named [translate 0.0 0.0 motor_z";
    assert!(PUCK.contains(marker), "fixture text drifted");
    let src = PUCK.replace(marker, "[root-named [translate 1.0 0.0 motor_z");
    let e = extract_cavity(&doc(&src), &sel(&MOTOR)).unwrap_err();
    match e {
        CavityExtractError::NonAxisymmetricFace { part, detail, .. } => {
            assert_eq!(part, "yoke");
            assert!(
                detail.contains("not coaxial") || detail.contains("not a circle about the axis"),
                "{detail}"
            );
        }
        other => panic!("expected an off-axis yoke, got {other}"),
    }
}

#[test]
fn obstacle_overlapping_the_enclosure_is_refused() {
    // The driver frame is press-fit into the wall: it overlaps the enclosure.
    let e = extract_cavity(&doc(PUCK), &sel(&["driver_frame"])).unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("\"driver_frame\"") && msg.contains("overlaps"),
        "{msg}"
    );
}

#[test]
fn obstacle_that_is_the_air_is_refused() {
    let e = extract_cavity(&doc(PUCK), &sel(&["air"])).unwrap_err();
    assert!(e.to_string().contains("already the enclosure"), "{e}");
}
