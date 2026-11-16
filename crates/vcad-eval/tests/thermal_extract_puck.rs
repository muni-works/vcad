//! Document → ThermalSpec on the every-physics puck.
//!
//! The PoC (`docs/videos/every-physics/poc`, PR #1190) hand-built a
//! `ThermalModel` for the amp-on-board: board `[15, 15, 0.4]` W/m·K, die
//! k = 120, 0.8 W, a 50 × 50 × 13 grid over board + die, convection on the
//! ±z domain faces and every exposed face, ±x/±y adiabatic. It printed
//! T_max 80.6 °C at h = 5 and 64.3 °C at h = 10. Here the same problem comes
//! out of the document: the board and die are voxelized from their
//! tessellations, the source is the die part, its power the document's own
//! `amp_loss_w` parameter.

use std::collections::BTreeMap;

use vcad_eval::thermal_extract::{
    extract_thermal_spec, extract_thermal_spec_from_scene, find_thermal_study, MaterialSource,
    ThermalExtractError,
};
use vcad_eval::{evaluate_document, EvalOptions, EvaluatedScene};
use vcad_ir::{
    AnalysisStudy, AnalysisStudyKind, Document, StudyRegion, StudyValue, ThermalLaminate,
    ThermalStudy, ThermalStudyBoundary, ThermalStudyMaterial, ThermalStudySource,
};
use vcad_kernel_thermal::model::{Boundary, MaterialRegion, PowerSource, Shape, ThermalModel};
use vcad_kernel_thermal::solve::{solve_steady, SolveOptions};

const PUCK: &str = include_str!("fixtures/puck_thermal.loon");

fn puck() -> Document {
    vcad_loon::eval_vcad(PUCK, None).expect("puck loon evaluates")
}

/// Evaluate once per test (the enclosure booleans dominate the runtime);
/// every extraction below reuses the scene.
fn scene(doc: &Document) -> EvaluatedScene {
    let opts = EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
    };
    evaluate_document(doc, &opts).expect("evaluate")
}

fn conv(h: f64) -> ThermalStudyBoundary {
    ThermalStudyBoundary::Convection {
        h_w_m2k: h.into(),
        ambient_c: 25.0.into(),
    }
}

/// The PoC's problem, authored as a document study.
fn poc_study(h: f64) -> ThermalStudy {
    let ad = ThermalStudyBoundary::Adiabatic;
    ThermalStudy {
        parts: vec!["board".into(), "die".into()],
        divisions: Some([50, 50, 13]),
        pitch_mm: None,
        padding_mm: None,
        min_voxels_across: None,
        materials: vec![
            ThermalStudyMaterial {
                part: "board".into(),
                k_w_mk: None,
                // 2-layer FR4 + copper pour (the PoC's assumption).
                laminate: Some(ThermalLaminate {
                    in_plane_w_mk: 15.0.into(),
                    through_plane_w_mk: 0.4.into(),
                }),
                heat_capacity_j_m3k: None,
            },
            ThermalStudyMaterial {
                part: "die".into(),
                k_w_mk: Some(vec![120.0.into()]),
                laminate: None,
                heat_capacity_j_m3k: None,
            },
        ],
        sources: vec![ThermalStudySource {
            name: "amp".into(),
            part: "die".into(),
            power_w: StudyValue::Named("amp_loss_w".into()),
            region: None,
        }],
        sinks: vec![],
        exposed: conv(h),
        domain_faces: Some(vec![
            ad.clone(),
            ad.clone(),
            ad.clone(),
            ad,
            conv(h),
            conv(h),
        ]),
        reference_c: None,
    }
}

/// The PoC's hand-built model (board-local frame), on any grid.
fn hand_model(h: f64, divisions: [usize; 3]) -> ThermalModel {
    let (bw, bl, q) = (50.0, 50.0, 0.8);
    let mut m = ThermalModel::new([0.0, 0.0, 0.0], [bw, bl, 2.6], divisions);
    m.materials.push(MaterialRegion::anisotropic(
        Shape::Box {
            min_mm: [0.0, 0.0, 0.0],
            size_mm: [bw, bl, 1.6],
        },
        [15.0, 15.0, 0.4],
    ));
    let die = Shape::Box {
        min_mm: [bw / 2.0 - 2.5, bl / 2.0 - 2.5, 1.6],
        size_mm: [5.0, 5.0, 1.0],
    };
    m.materials
        .push(MaterialRegion::isotropic(die.clone(), 120.0));
    m.sources.push(PowerSource {
        name: "amp".into(),
        shape: die,
        power_w: q,
    });
    let c = Boundary::Convection {
        h_w_m2k: h,
        ambient_c: 25.0,
    };
    m.domain_faces[4] = c;
    m.domain_faces[5] = c;
    m.exposed = c;
    m
}

fn solve_doc(doc: &Document, sc: &EvaluatedScene, study: &ThermalStudy) -> (f64, f64, f64) {
    let ex = extract_thermal_spec_from_scene(doc, sc, study).expect("extract");
    let model = ex.spec.resolve(&ex.parameters).expect("resolve");
    let sol = solve_steady(&model, &SolveOptions::default()).expect("solve");
    (
        sol.t_max_c,
        sol.sources[0].theta_c_per_w.unwrap(),
        sol.energy.residual_rel,
    )
}

#[test]
fn grid_aligned_puck_matches_the_hand_model_exactly() {
    // At 0.5 mm in-plane the die edges (±2.5 mm) fall on voxel faces, so
    // no voxel centre sits on a boundary: the voxelizer and the hand
    // painting must produce the same field, and the same answer.
    let doc = puck();
    let sc = scene(&doc);
    for h in [5.0, 10.0] {
        let mut s = poc_study(h);
        s.divisions = Some([100, 100, 13]);
        let ex = extract_thermal_spec_from_scene(&doc, &sc, &s).expect("extract");
        let (board, die) = (&ex.parts[0], &ex.parts[1]);
        assert_eq!(board.material_source, MaterialSource::Override);
        assert_eq!(board.laminate_axis, Some(2), "board is thinnest in z");
        assert_eq!(board.k_w_mk, [15.0, 15.0, 0.4]);
        assert_eq!(board.voxels, 100 * 100 * 8);
        assert_eq!(die.voxels, 10 * 10 * 5);
        assert!((board.fill_volume_mm3 - 4000.0).abs() < 0.01);
        assert!((die.fill_volume_mm3 - 25.0).abs() < 0.01);
        assert!((ex.parameters["amp_loss_w"] - 0.8).abs() < 1e-12);

        let model = ex.spec.resolve(&ex.parameters).expect("resolve");
        let sol = solve_steady(&model, &SolveOptions::default()).expect("solve");
        let hand =
            solve_steady(&hand_model(h, [100, 100, 13]), &SolveOptions::default()).expect("hand");
        println!(
            "aligned 0.5 mm grid, h = {h}: document T_max {:.3} °C, hand-built {:.3} °C",
            sol.t_max_c, hand.t_max_c
        );
        assert!(
            (sol.t_max_c - hand.t_max_c).abs() < 1e-3,
            "document {} vs hand {}",
            sol.t_max_c,
            hand.t_max_c
        );
        assert!(sol.energy.residual_rel < 1e-6);
    }
}

#[test]
fn puck_on_the_poc_grid_explains_the_difference() {
    // On the PoC's own 1 mm grid the die edges sit exactly on voxel
    // centres. The PoC's closed Box painting takes every such voxel — a
    // 6 × 6 footprint, 36 mm² for a 25 mm² die. The voxelizer's ½-fill rule
    // keeps the edge voxels (exactly half full) and drops the four corners
    // (a quarter full): 32 mm². A smaller footprint concentrates the same
    // 0.8 W, so the document reads hotter than the PoC by a bounded margin.
    let doc = puck();
    let sc = scene(&doc);
    for (h, poc_printed) in [(5.0, 80.6), (10.0, 64.3)] {
        let ex = extract_thermal_spec_from_scene(&doc, &sc, &poc_study(h)).expect("extract");
        assert_eq!(ex.grid.divisions, [50, 50, 13]);
        for a in 0..3 {
            assert!((ex.grid.pitch_mm[a] - [1.0, 1.0, 0.2][a]).abs() < 1e-5);
        }
        assert_eq!(ex.parts[0].voxels, 50 * 50 * 8);
        assert_eq!(ex.parts[1].voxels, (36 - 4) * 5);

        let model = ex.spec.resolve(&ex.parameters).expect("resolve");
        let sol = solve_steady(&model, &SolveOptions::default()).expect("solve");
        let hand =
            solve_steady(&hand_model(h, [50, 50, 13]), &SolveOptions::default()).expect("hand");
        let mut fine = poc_study(h);
        fine.divisions = Some([100, 100, 13]);
        let (t_fine, _, _) = solve_doc(&doc, &sc, &fine);
        println!(
            "PoC 1 mm grid, h = {h}: PoC printed {poc_printed}, hand-built {:.2} °C (36 mm² die), \
             document {:.2} °C (32 mm²), aligned 0.5 mm grid {:.2} °C (25 mm², exact footprint)",
            hand.t_max_c, sol.t_max_c, t_fine
        );
        // The PoC reproduces.
        assert!((hand.t_max_c - poc_printed).abs() < 0.05);
        // Footprint ordering: 36 mm² < 32 mm² < 25 mm² in temperature.
        assert!(hand.t_max_c < sol.t_max_c && sol.t_max_c < t_fine);
        // The grid band: the footprint artefact moves the rise by ~4-5 %.
        let rise = |t: f64| t - 25.0;
        assert!((rise(sol.t_max_c) / rise(hand.t_max_c) - 1.0).abs() < 0.07);
        assert!(sol.energy.residual_rel < 1e-6);
    }
}

#[test]
fn study_persists_on_the_document() {
    let mut doc = puck();
    doc.analysis_studies.push(AnalysisStudy {
        id: "amp-thermal".into(),
        name: "amp on board".into(),
        study: AnalysisStudyKind::Thermal(poc_study(5.0)),
        baseline: None,
    });
    // Round-trips through .vcad JSON.
    let json = serde_json::to_string(&doc).unwrap();
    let back: Document = serde_json::from_str(&json).unwrap();
    let study = find_thermal_study(&back, None).expect("one thermal study");
    assert_eq!(study, &poc_study(5.0));
    assert!(find_thermal_study(&back, Some("nope")).is_err());

    // Scrubbing the document parameter re-prices the run: 2× power → 2×
    // temperature rise (conduction is linear).
    // amp_loss_w is not geometric, so one scene serves both documents.
    let sc = scene(&back);
    let (t1, _, _) = solve_doc(&back, &sc, study);
    let mut hot = back.clone();
    hot.parameters.get_mut("amp_loss_w").unwrap().value = vcad_ir::parameters::Expr::Number(1.6);
    let (t2, _, _) = solve_doc(&hot, &sc, study);
    assert!(
        ((t2 - 25.0) / (t1 - 25.0) - 2.0).abs() < 1e-6,
        "{t1} → {t2}"
    );
}

#[test]
fn default_domain_faces_take_the_exposed_boundary() {
    // With no `domainFaces`, the board's edges convect too (physically
    // right for a free board): slightly cooler than the PoC's adiabatic
    // edges.
    let doc = puck();
    let sc = scene(&doc);
    let mut s = poc_study(5.0);
    s.domain_faces = None;
    let (t_all, _, res) = solve_doc(&doc, &sc, &s);
    let (t_poc, _, _) = solve_doc(&doc, &sc, &poc_study(5.0));
    println!(
        "h = 5, board edges convecting: T_max {t_all:.2} °C (PoC edges adiabatic: {t_poc:.2})"
    );
    assert!(t_all < t_poc && t_all > 25.0);
    assert!(res < 1e-6);
}

#[test]
fn pitch_and_table_materials() {
    // No overrides: board from the table (bare FR4 laminate, far worse
    // in-plane) and die from silicon — hotter than the poured board.
    let doc = puck();
    let sc = scene(&doc);
    let mut s = poc_study(10.0);
    s.materials.clear();
    s.divisions = None;
    s.pitch_mm = Some([1.0, 1.0, 0.2]);
    let ex = extract_thermal_spec_from_scene(&doc, &sc, &s).expect("extract");
    assert_eq!(ex.grid.divisions, [50, 50, 13]);
    assert_eq!(ex.parts[0].material_source, MaterialSource::Table);
    assert_eq!(ex.parts[0].k_w_mk, [0.81, 0.81, 0.29]);
    assert_eq!(ex.parts[1].k_w_mk, [148.0; 3]);
    assert!(ex.parts[1].heat_capacity_j_m3k.unwrap() > 1e6);
    let (t, _, _) = solve_doc(&doc, &sc, &s);
    println!("h = 10, bare-FR4 board (table): T_max {t:.1} °C");
    assert!(t > 64.3);
}

#[test]
fn thin_wall_is_refused_naming_the_part() {
    let doc = puck();
    let sc = scene(&doc);
    let mut s = poc_study(5.0);
    s.divisions = Some([50, 50, 3]); // 0.87 mm in z: board 1.6 mm < 2 voxels
    match extract_thermal_spec_from_scene(&doc, &sc, &s) {
        Err(ThermalExtractError::ThinWall { part, axis, .. }) => {
            assert_eq!(part, "board");
            assert_eq!(axis, 'z');
        }
        other => panic!("expected ThinWall, got {other:?}"),
    }
}

#[test]
fn overlapping_parts_are_refused() {
    // The cavity air contains the board.
    let doc = puck();
    let sc = scene(&doc);
    let mut s = poc_study(5.0);
    s.parts = vec!["air".into(), "board".into()];
    s.materials.retain(|m| m.part == "board");
    s.sources.clear();
    s.divisions = None;
    s.pitch_mm = Some([2.0, 2.0, 0.5]);
    match extract_thermal_spec_from_scene(&doc, &sc, &s) {
        Err(ThermalExtractError::PartsOverlap { a, b, overlap_mm3 }) => {
            assert_eq!((a.as_str(), b.as_str()), ("air", "board"));
            assert!(overlap_mm3 > 1000.0, "{overlap_mm3}");
        }
        other => panic!("expected PartsOverlap, got {other:?}"),
    }
}

#[test]
fn unknown_material_and_unselected_part_fail_closed() {
    let doc = vcad_loon::eval_vcad(
        "[let blob [cube 10.0 10.0 10.0]]\n[root blob \"unobtainium\"]\n",
        None,
    )
    .unwrap();
    let s = ThermalStudy {
        parts: vec!["blob".into()],
        divisions: Some([10, 10, 10]),
        ..poc_study(5.0)
    };
    let mut s = s;
    s.materials.clear();
    s.sources.clear();
    s.domain_faces = None;
    match extract_thermal_spec(&doc, &s) {
        Err(ThermalExtractError::UnknownMaterial { part, material, .. }) => {
            assert_eq!((part.as_str(), material.as_str()), ("blob", "unobtainium"));
        }
        other => panic!("expected UnknownMaterial, got {other:?}"),
    }
    s.sources = poc_study(5.0).sources; // names "die", not selected
    s.materials = vec![ThermalStudyMaterial {
        part: "blob".into(),
        k_w_mk: Some(vec![1.0.into()]),
        laminate: None,
        heat_capacity_j_m3k: None,
    }];
    assert!(matches!(
        extract_thermal_spec(&doc, &s),
        Err(ThermalExtractError::NotSelected { .. })
    ));
    // An undeclared parameter name fails closed too.
    s.sources[0].part = "blob".into();
    s.sources[0].power_w = StudyValue::Named("no_such_param".into());
    assert!(matches!(
        extract_thermal_spec(&doc, &s),
        Err(ThermalExtractError::UnknownParameter { .. })
    ));
}

#[test]
fn source_box_may_not_swallow_another_part() {
    // A chip sitting in a pocket of a plate: the plate's voxel bounding
    // box holds the chip, so a source "on the plate" would heat the chip.
    let doc = vcad_loon::eval_vcad(
        "[let pocket [translate 3.0 3.0 1.0 [cube 4.0 4.0 1.0]]]\n\
         [let plate [difference pocket [cube 10.0 10.0 2.0]]]\n\
         [root plate \"copper\"]\n\
         [let chip [translate 3.0 3.0 1.0 [cube 4.0 4.0 1.0]]]\n\
         [root chip \"silicon\"]\n",
        None,
    )
    .unwrap();
    let mut s = ThermalStudy {
        parts: vec!["plate".into(), "chip".into()],
        divisions: Some([20, 20, 8]),
        ..poc_study(5.0)
    };
    s.materials.clear();
    s.domain_faces = None;
    s.sources = vec![ThermalStudySource {
        name: "heater".into(),
        part: "plate".into(),
        power_w: 1.0.into(),
        region: None,
    }];
    match extract_thermal_spec(&doc, &s) {
        Err(ThermalExtractError::SharedRegion { part, other, .. }) => {
            assert_eq!((part.as_str(), other.as_str()), ("plate", "chip"));
        }
        other => panic!("expected SharedRegion, got {other:?}"),
    }
    // A region below the pocket holds only plate voxels.
    s.sources[0].region = Some(StudyRegion {
        min: [-1.0, -1.0, -1.0],
        max: [11.0, 11.0, 0.9],
    });
    let ex = extract_thermal_spec(&doc, &s).expect("region source");
    let model = ex.spec.resolve(&BTreeMap::new()).expect("resolve");
    let sol = solve_steady(&model, &SolveOptions::default()).expect("solve");
    assert!((sol.sources[0].power_w - 1.0).abs() < 1e-12);
    assert!(sol.energy.residual_rel < 1e-6);
}

#[test]
fn documented_study_json_parses_into_a_document() {
    // The shape docs/thermal-voxelizer.md shows, with integer literals —
    // the materializer drops every study on a parse failure, so pin it.
    let json = r#"{
      "id": "t1", "name": "amp on board",
      "study": { "type": "thermal",
        "parts": ["board", "die"],
        "divisions": [50, 50, 13],
        "materials": [
          { "part": "board", "laminate": { "inPlaneWMk": 15, "throughPlaneWMk": 0.4 } },
          { "part": "die", "kWMk": [120] } ],
        "sources": [ { "name": "amp", "part": "die", "powerW": "amp_loss_w" } ],
        "exposed": { "type": "convection", "hWM2k": 5, "ambientC": 25 },
        "domainFaces": [ {"type": "adiabatic"}, {"type": "adiabatic"},
          {"type": "adiabatic"}, {"type": "adiabatic"},
          { "type": "convection", "hWM2k": 5, "ambientC": 25 },
          { "type": "fixedTemperature", "temperatureC": 30 } ] } }"#;
    let study: AnalysisStudy = serde_json::from_str(json).expect("parses");
    let AnalysisStudyKind::Thermal(t) = study.study else {
        panic!("not a thermal study")
    };
    assert_eq!(t.parts, vec!["board", "die"]);
    assert_eq!(t.sources[0].power_w, StudyValue::Named("amp_loss_w".into()));
    assert_eq!(
        t.domain_faces.as_ref().unwrap()[5],
        ThermalStudyBoundary::FixedTemperature {
            temperature_c: 30.0.into()
        }
    );
}

/// Footprint convergence on the puck (not run by default): how far the
/// 0.5 mm answer is from converged. `cargo test -p vcad-eval --test
/// thermal_extract_puck -- --ignored --nocapture`.
#[test]
#[ignore]
fn puck_in_plane_convergence() {
    let doc = puck();
    let sc = scene(&doc);
    for h in [5.0, 10.0] {
        for n in [50u32, 100, 200] {
            let mut s = poc_study(h);
            s.divisions = Some([n, n, 13]);
            let (t, _, _) = solve_doc(&doc, &sc, &s);
            println!(
                "h = {h}, {n} x {n} x 13 ({:.2} mm): T_max {t:.2} °C",
                50.0 / n as f64
            );
        }
    }
}

#[test]
fn radiating_exposed_faces_cool_the_puck_board() {
    // Same board + die, but the exposed faces also radiate (ε from a
    // document parameter) in parallel with the h = 5 film: cooler than
    // convection alone, and the energy audit still closes.
    let mut doc = puck();
    doc.parameters
        .insert("board_eps".into(), vcad_ir::Parameter::literal(0.9));
    let sc = scene(&doc);
    let rad = ThermalStudyBoundary::Radiation {
        emissivity: StudyValue::Named("board_eps".into()),
        ambient_c: 25.0.into(),
        h_w_m2k: Some(5.0.into()),
        area_scale: None,
        enclosure: None,
    };
    let mut s = poc_study(5.0);
    s.exposed = rad.clone();
    s.domain_faces = None; // every face radiates + convects
    let ex = extract_thermal_spec_from_scene(&doc, &sc, &s).expect("extract");
    let model = ex.spec.resolve(&ex.parameters).expect("resolve");
    let sol = solve_steady(&model, &SolveOptions::default()).expect("solve");
    let mut c = poc_study(5.0);
    c.domain_faces = None;
    let (t_conv, _, _) = solve_doc(&doc, &sc, &c);
    println!(
        "h = 5 + ε = 0.9 radiation: T_max {:.2} °C (convection only {t_conv:.2}), \
         radiated {:.3} W of 0.8, Newton {}",
        sol.t_max_c, sol.energy.radiation_out_w, sol.newton_iterations
    );
    assert!(
        sol.t_max_c < t_conv - 5.0,
        "radiation should matter at ~80 °C"
    );
    assert!(sol.energy.radiation_out_w > 0.1 && sol.energy.radiation_out_w < 0.8);
    assert!(sol.energy.residual_rel < 1e-6);
}
