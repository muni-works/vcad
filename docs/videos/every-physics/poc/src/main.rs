//! every-physics PoC v2: ONE loon document (`../puck.loon`), every solver.
//!
//! v1 (PR #1190) hand-transcribed most solver specs from `doc.parameters`.
//! v2 runs on main after the gap PRs (#1200 #1202 #1204–#1207 #1209 #1210):
//! every solver below reads the document through a vcad extractor. Tags:
//!
//! - `[doc-geom]`     — read off the evaluated solid (mesh, BRep).
//! - `[doc-extract]`  — a vcad extractor built the solver spec from the
//!                      document (part names, BRep faces, parameters).
//! - `[doc-injected]` — in the document, but put there by THIS driver
//!                      (the PcbBoard node, the thermal study, the fit
//!                      assertion): loon can't author them yet.
//! - `[assumed]`      — not in the document at all (film coefficient, air
//!                      temperature, optimizer weights).
//!
//! Run (on a build box, never the Mac):
//!   cargo run --release -- ../puck.loon results/
//! Env: MODAL_QUALITY=fine|preview|skip (default fine), OPT=run|skip
//! (default run), OPT_ITERS=<outer rounds> (default 4).

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Map, Value};
use vcad_eval::antenna_extract::{extract_antenna_spec, AntennaExtraction, FeedPad, GroundModel};
use vcad_eval::cavity_extract::{extract_cavity, CavityExtraction};
use vcad_eval::optimize::{
    optimize_document, Constraint, ConstraintOp, ObjectiveTerm, OptimizeDocOptions,
    OptimizeDocRequest, OptimizeHooks, ParamSpec, QoiInput, QoiRef, QoiRegistry, ScalarQoi,
    TermShape, TrajectoryStep,
};
use vcad_eval::sim_extract::em::{
    extract_axisym_mag_spec_from_scene, AxisymMagExtraction, DomainSelection, MagPartSelection,
    MagRole, North,
};
use vcad_eval::sim_extract::tolerance::{
    extract_stackup_spec_from_scene, FitFeature, StackupExtraction,
};
use vcad_eval::thermal_extract::{extract_thermal_spec_from_scene, find_thermal_study};
use vcad_eval::{evaluate_document, EvalOptions, EvaluatedMesh, EvaluatedScene};
use vcad_ir::ecad::Pcb;
use vcad_ir::{
    AnalysisStudy, AnalysisStudyKind, ClearanceSpec, CsgOp, Document, Node, StudyValue,
    ThermalLaminate, ThermalStudy, ThermalStudyBoundary, ThermalStudyMaterial, ThermalStudySource,
};
use vcad_kernel_acoustics::lumped;
use vcad_kernel_acoustics::medium::Medium;
use vcad_kernel_acoustics::ported::{analyze, AnalysisOptions};
use vcad_kernel_acoustics::spec::ParamValue as AcParam;
use vcad_kernel_antenna::{find_resonance, sweep, SolveOptions as AntOpts};
use vcad_kernel_dfm::{Process, RulePack};
use vcad_kernel_em::axisym::{Annulus, AxisymMagnetostatics};
use vcad_kernel_em::grid::SolveOptions as EmOpts;
use vcad_kernel_em::spec::ParamValue as EmParam;
use vcad_kernel_tessellate::TessellationParams;
use vcad_kernel_thermal::solve::{solve_steady, SolveOptions as ThOpts};
use vcad_kernel_tolerance::analysis::{monte_carlo, rss, worst_case, McOptions};
use vcad_sound::optimize::ModalFrequencyQoi;
use vcad_sound::{part_modes_alone, ModesRequest, Quality};

fn opts() -> EvalOptions {
    EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
        on_root_ready: None,
    }
}

/// Signed volume of a closed triangle mesh, mm³.
fn mesh_volume(m: &EvaluatedMesh) -> f64 {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            m.positions[i] as f64,
            m.positions[i + 1] as f64,
            m.positions[i + 2] as f64,
        ]
    };
    m.indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum::<f64>()
}

/// Scene index of the root named `name`.
fn part_index(doc: &Document, name: &str) -> usize {
    doc.roots
        .iter()
        .position(|r| {
            doc.nodes
                .get(&r.root)
                .and_then(|n| n.name.as_deref())
                == Some(name)
        })
        .unwrap_or_else(|| panic!("no root named {name}"))
}

fn cavity_sel() -> CavityExtraction {
    CavityExtraction {
        enclosure: "enclosure".into(),
        air: "air".into(),
        // The motor sits inside the box: rigid coaxial obstacles (#1222).
        obstacles: ["yoke", "magnet", "top_plate", "voice_coil"]
            .map(String::from)
            .to_vec(),
        driver_radius: Some(AcParam::Named("driver_r".into())),
        temp_c: None,
        tolerance: Default::default(),
    }
}

// ── the PCB copper: one PcbBoard node, from the document's own parameters ──

fn v(x: f64, y: f64) -> Value {
    json!({"x": x, "y": y})
}

fn rect_zone(x0: f64, y0: f64, x1: f64, y1: f64, layer: &str, net: &str) -> Value {
    json!({
        "outline": [v(x0, y0), v(x1, y0), v(x1, y1), v(x0, y1)],
        "net": net, "layer": layer, "clearance": 0.3
    })
}

/// `board_w` × `board_l` FR-4, GND pour on both layers below the feed, a
/// straight `ant_len` printed monopole on net ANT fed by U.FL J1.1 on the
/// pour edge. Same `Pcb` shape the ECAD MCP tools write.
fn pcb_board(bw: f64, bl: f64, ant_len: f64) -> Pcb {
    let feed_y = bl - ant_len - 2.0; // 2 mm keepout to the board edge
    let fx = bw / 2.0;
    let b = json!({
        "outline": {
            "vertices": [v(0.0, 0.0), v(bw, 0.0), v(bw, bl), v(0.0, bl)],
            "thickness": 1.6
        },
        "stackup": {"layers": [
            {"layer": "FCu", "copperThickness": 0.035},
            {"layer": "BCu", "copperThickness": 0.035,
             "dielectricThickness": 1.53, "dielectricEr": 4.4, "material": "FR4"}
        ]},
        "nets": [{"id": "ANT", "name": "ANT"}, {"id": "GND", "name": "GND"}],
        "rules": {
            "defaultRules": {"name": "Default", "traceWidth": 0.25, "clearance": 0.2,
                             "viaDiameter": 0.6, "viaDrill": 0.3},
            "netClassAssignments": {},
            "edgeClearance": 0.3, "holeToHole": 0.25, "minAnnularRing": 0.13, "minDrill": 0.3
        },
        "footprints": [{
            "ref": "J1", "value": "U.FL", "footprintName": "U.FL",
            "position": v(fx, feed_y),
            "pads": [
                {"number": "1", "padType": "SMD", "shape": {"type": "Rect", "width": 1.0, "height": 1.0},
                 "position": v(0.0, 0.0), "net": "ANT", "layers": ["FCu"]},
                {"number": "2", "padType": "SMD", "shape": {"type": "Rect", "width": 1.0, "height": 2.0},
                 "position": v(-1.5, -1.5), "net": "GND", "layers": ["FCu"]}
            ]
        }],
        "traces": [{"start": v(fx, feed_y), "end": v(fx, feed_y + ant_len),
                    "width": 1.0, "layer": "FCu", "net": "ANT"}],
        "zones": [
            rect_zone(0.0, 0.0, bw, feed_y, "FCu", "GND"),
            rect_zone(0.0, 0.0, bw, feed_y, "BCu", "GND")
        ]
    });
    serde_json::from_value(b).expect("board JSON parses as Pcb")
}

fn conv(h: f64) -> ThermalStudyBoundary {
    ThermalStudyBoundary::Convection {
        h_w_m2k: h.into(),
        ambient_c: 25.0.into(),
    }
}

/// The amp-on-board study (v1's hand-built model, now a document study).
fn amp_study(h: f64) -> ThermalStudy {
    let ad = ThermalStudyBoundary::Adiabatic;
    ThermalStudy {
        parts: vec!["board".into(), "die".into()],
        divisions: Some([100, 100, 13]),
        pitch_mm: None,
        padding_mm: None,
        min_voxels_across: None,
        materials: vec![
            ThermalStudyMaterial {
                part: "board".into(),
                k_w_mk: None,
                // 2-layer FR4 + copper pour [assumed laminate]
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

fn mag_part(name: &str) -> MagPartSelection {
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

/// Lumped bass-reflex tuning (nominal), read off the document through the
/// cavity extractor — the acoustics coupling the optimizer differences.
struct TuningQoi;

impl ScalarQoi for TuningQoi {
    fn name(&self) -> &str {
        "tuning_hz"
    }
    fn evaluate(&self, input: &QoiInput, _part: Option<usize>) -> Result<f64, String> {
        let ex = extract_cavity(input.doc, &cavity_sel()).map_err(|e| e.to_string())?;
        let m = &ex.model;
        Ok(lumped::ported_box_tuning_mount_mm(
            &Medium::air(20.0),
            m.box_volume_mm3,
            m.port_radius_mm,
            m.port_length_mm,
            m.mount,
        )
        .f_nominal_hz)
    }
    fn needs_brep(&self) -> bool {
        true
    }
}

fn err(summary: &mut Map<String, Value>, key: &str, e: impl std::fmt::Display) {
    println!("  FAILED (fail-closed) — {e}");
    summary.insert(key.into(), json!({ "error": e.to_string() }));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src_path = args.get(1).map(String::as_str).unwrap_or("../puck.loon");
    let out = Path::new(args.get(2).map(String::as_str).unwrap_or("results"));
    fs::create_dir_all(out).expect("out dir");

    let source = fs::read_to_string(src_path).expect("read loon");
    let mut doc: Document = vcad_loon::eval_vcad(&source, None).expect("loon eval");
    let p = vcad_ir::resolve_parameters(&doc.parameters).expect("params");
    let g = |k: &str| *p.get(k).unwrap_or_else(|| panic!("doc has no parameter {k}"));

    // [doc-injected] the board's copper, the amp study, the driver-frame fit.
    doc.nodes.insert(
        9000,
        Node {
            id: 9000,
            name: Some("PCB Board".into()),
            op: CsgOp::PcbBoard {
                board: Box::new(pcb_board(g("board_w"), g("board_l"), g("ant_len"))),
            },
        },
    );
    doc.analysis_studies.push(AnalysisStudy {
        id: "amp-thermal".into(),
        name: "amp on board".into(),
        study: AnalysisStudyKind::Thermal(Box::new(amp_study(5.0))),
        baseline: None,
    });
    doc.clearance_specs.push(ClearanceSpec {
        label: "driver-fit".into(),
        group_a: vec!["driver_frame".into()],
        group_b: vec!["enclosure".into()],
        // Press fit: the PLA cutout cracks above 0.15 mm radial interference.
        min_mm: -0.15,
        allow_contact: None,
        sweep: None,
        baseline_mm: None,
        max_mm: Some(-0.01), // rattles below 0.01 mm interference
        fit: None,
    });
    fs::write(
        out.join("puck.vcad"),
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
    let mut summary = Map::new();
    let mut timing = Map::new();

    // ── 0. evaluate the one document ───────────────────────────────────────
    println!("# puck v2 — one document, every solver\n");
    let t = Instant::now();
    let scene: EvaluatedScene = evaluate_document(&doc, &opts()).expect("evaluate");
    timing.insert("evaluate_s".into(), t.elapsed().as_secs_f64().into());
    assert!(
        scene.failures.is_empty(),
        "root failures: {:?}",
        scene.failures
    );
    let i_enc = part_index(&doc, "enclosure");
    let i_air = part_index(&doc, "air");
    println!("## parts + mass [doc-geom × material library]");
    let mut parts = Map::new();
    for (i, r) in doc.roots.iter().enumerate() {
        let name = doc.nodes[&r.root].name.clone().unwrap_or_default();
        let vol = mesh_volume(&scene.parts[i].mesh);
        let rho = vcad_ir::materials::resolve_material(&doc, &r.material)
            .ok()
            .and_then(|m| m.density_kg_m3);
        let mass_g = rho.map(|r| vol * 1e-9 * r * 1e3);
        println!(
            "  {name:<13} {:<12} {vol:>10.0} mm³  {}",
            r.material,
            mass_g.map_or("density n/a".into(), |m| format!("{m:.1} g"))
        );
        parts.insert(name, json!({"material": r.material, "mm3": vol, "g": mass_g}));
    }
    summary.insert("parts".into(), parts.into());
    println!(
        "  enclosed air      {:.3} L",
        mesh_volume(&scene.parts[i_air].mesh) * 1e-6
    );

    // ── 1. diff seam ───────────────────────────────────────────────────────
    println!("\n## diff seam: document_parameter_gradient (enclosure) [doc-geom]");
    let tess = TessellationParams {
        circle_segments: 64,
        height_segments: 2,
        ..Default::default()
    };
    let t = Instant::now();
    for param in ["wall", "body_h", "port_len"] {
        match vcad_eval::diff::document_parameter_gradient(&doc, param, 1.24e-3, &tess, 1e-4) {
            Ok(gr) => {
                if let Some(pg) = gr.iter().find(|pg| pg.part_index == i_enc) {
                    println!(
                        "  dV/d{param} = {:+.1} mm³/mm, dm/d{param} = {:+.3} g/mm",
                        pg.derivative.volume, pg.derivative.mass
                    );
                    summary.insert(format!("dV_d{param}"), pg.derivative.volume.into());
                }
            }
            Err(e) => err(&mut summary, &format!("dV_d{param}"), e),
        }
    }
    timing.insert("diff_s".into(), t.elapsed().as_secs_f64().into());

    // ── 2. DFM ─────────────────────────────────────────────────────────────
    println!("\n## DFM, FDM pack (enclosure) [doc-geom]");
    if let Some(brep) = scene.parts[i_enc].solid.as_ref().and_then(|s| s.as_brep()) {
        let rep = vcad_kernel_dfm::run_dfm(
            brep,
            None,
            Process::Fdm,
            &RulePack::default_for(Process::Fdm),
        );
        println!("  {} issues ({})", rep.issues.len(), rep.rule_pack_name);
        for iss in rep.issues.iter().take(6) {
            println!("   - {:?}: {}", iss.severity, iss.message);
        }
        summary.insert("dfm_fdm_issues".into(), rep.issues.len().into());
    } else {
        println!("  enclosure has no BRep — DFM skipped");
    }

    // ── 3. structural modes ────────────────────────────────────────────────
    let mq = std::env::var("MODAL_QUALITY").unwrap_or_else(|_| "fine".into());
    println!("\n## structural modes, free-free, material from the IR library, quality={mq} [doc-geom]");
    if mq != "skip" {
        let req = ModesRequest {
            part: i_enc,
            modes: 6,
            quality: if mq == "preview" {
                Quality::Preview
            } else {
                Quality::Fine
            },
            material: None, // the root's own "pla", resolved through the library
            shapes: false,
        };
        let t = Instant::now();
        match part_modes_alone(&doc, &req) {
            Ok(m) => {
                for md in &m.modes {
                    println!("  {:8.1} Hz  ({} {:+.0}¢)", md.hz, md.note, md.cents);
                }
                summary.insert(
                    "modes_hz".into(),
                    m.modes.iter().map(|x| x.hz).collect::<Vec<_>>().into(),
                );
            }
            Err(e) => err(&mut summary, "modes_hz", format!("{e:?}")),
        }
        timing.insert("modal_s".into(), t.elapsed().as_secs_f64().into());
    }

    // ── 4. acoustics: document → axisym cavity ─────────────────────────────
    // The motor is carved out of the air and listed as cavity obstacles
    // (#1222 closed G11): V_box excludes it, the field solve sees it.
    println!("\n## acoustics: ported box from the document, motor as obstacles [doc-extract; 20 °C assumed]");
    let t = Instant::now();
    match extract_cavity(&doc, &cavity_sel()) {
        Ok(ex) => {
            let m = &ex.model;
            println!(
                "  V_box {:.3} L (BRep), port r {:.1} × {:.1} mm, mount {:?}, {} obstacle(s)",
                ex.air_volume_mm3 * 1e-6,
                m.port_radius_mm,
                m.port_length_mm,
                m.mount,
                m.spec.obstacles.len()
            );
            for a in &ex.assumptions {
                println!("   · {a}");
            }
            match analyze(
                m,
                &ex.parameters,
                &AnalysisOptions {
                    sweep_points: 77,
                    curve_points: 8,
                    ..Default::default()
                },
            ) {
                Ok(rep) => {
                    let b = &rep.lumped;
                    println!(
                        "  lumped f_b [{:.1}, {:.1}, {:.1}] Hz; field f_b {}",
                        b.f_min_hz,
                        b.f_nominal_hz,
                        b.f_max_hz,
                        rep.field_tuning_hz
                            .map_or("not found".into(), |f| format!("{f:.1} Hz"))
                    );
                    let mut csv = String::from("freq_hz,port_volume_velocity_m3s\n");
                    for pt in &rep.port_velocity {
                        csv.push_str(&format!("{:.2},{:.6e}\n", pt.f_hz, pt.volume_velocity_m3_s));
                    }
                    fs::write(out.join("port_velocity.csv"), csv).unwrap();
                    summary.insert(
                        "fb_lumped_hz".into(),
                        vec![b.f_min_hz, b.f_nominal_hz, b.f_max_hz].into(),
                    );
                    summary.insert("fb_field_hz".into(), rep.field_tuning_hz.into());
                    summary.insert("air_l".into(), (ex.air_volume_mm3 * 1e-6).into());
                    summary.insert("acoustics_claims".into(), rep.claims.claims.len().into());
                }
                Err(e) => err(&mut summary, "fb_lumped_hz", format!("{e:?}")),
            }
        }
        Err(e) => err(&mut summary, "fb_lumped_hz", e),
    }
    timing.insert("acoustics_s".into(), t.elapsed().as_secs_f64().into());

    // ── 5. thermal: document study → voxel model ───────────────────────────
    println!("\n## thermal: amp on board, voxelized from the document [doc-extract; h assumed]");
    let t = Instant::now();
    let mut th = Map::new();
    let study = find_thermal_study(&doc, Some("amp-thermal")).expect("study").clone();
    for h in [5.0, 10.0] {
        let mut s = study.clone();
        s.exposed = conv(h);
        if let Some(df) = s.domain_faces.as_mut() {
            df[4] = conv(h);
            df[5] = conv(h);
        }
        match extract_thermal_spec_from_scene(&doc, &scene, &s)
            .map_err(|e| e.to_string())
            .and_then(|ex| ex.spec.resolve(&ex.parameters).map_err(|e| format!("{e:?}")))
            .and_then(|model| solve_steady(&model, &ThOpts::default()).map_err(|e| format!("{e:?}")))
        {
            Ok(sol) => {
                println!(
                    "  h = {h:>4.0} W/m²K: T_max {:.1} °C, θ_ja {:.1} K/W, energy residual {:.1e}",
                    sol.t_max_c,
                    sol.sources[0].theta_c_per_w.unwrap_or(f64::NAN),
                    sol.energy.residual_rel
                );
                th.insert(format!("tmax_c_h{h}"), sol.t_max_c.into());
            }
            Err(e) => err(&mut th, &format!("tmax_c_h{h}"), e),
        }
    }
    summary.insert("thermal".into(), th.into());
    timing.insert("thermal_s".into(), t.elapsed().as_secs_f64().into());

    // ── 6. antenna: PcbBoard traces → wire grid ────────────────────────────
    println!("\n## antenna: printed monopole extracted from the PcbBoard [doc-injected board → doc-extract]");
    let t = Instant::now();
    let asel = AntennaExtraction {
        board_node: None,
        net: None,
        feed: FeedPad {
            footprint: "J1".into(),
            pad: "1".into(),
        },
        ground_net: "GND".into(),
        f_max_hz: 3.0e9,
        segments_per_wavelength: 60.0,
        max_feed_gap_mm: 2.0,
        ground: GroundModel::InfiniteImage,
    };
    match extract_antenna_spec(&doc, &asel) {
        Ok(ex) => {
            println!(
                "  net {}, {} segments, feed {:?}, ground {} ({:.2} λ along edge)",
                ex.net, ex.segments, ex.feed.treatment, ex.ground.model, ex.ground.pour_along_edge_wavelengths
            );
            for n in &ex.notes {
                println!("   · {n}");
            }
            let (mesh, feed) = ex.spec.resolve(&ex.parameters).expect("resolve");
            let ao = AntOpts::default();
            let l = g("ant_len");
            let f_qw = 299.792458e9 / (4.0 * l);
            let fres = find_resonance(&mesh, feed, 0.75 * f_qw, 1.05 * f_qw, &ao);
            let freqs: Vec<f64> = (0..61).map(|i| 1.8e9 + 1.2e9 * i as f64 / 60.0).collect();
            let pts = sweep(&mesh, feed, &freqs, 50.0, &ao).expect("sweep");
            let best = pts.iter().min_by(|a, b| a.s11_db.total_cmp(&b.s11_db)).unwrap();
            let at24 = pts
                .iter()
                .min_by(|a, b| (a.freq_hz - 2.44e9).abs().total_cmp(&(b.freq_hz - 2.44e9).abs()))
                .unwrap();
            match &fres {
                Ok(f) => println!("  resonance (Im Z = 0) {:.0} MHz (free space; FR4 pulls it lower)", f / 1e6),
                Err(e) => println!("  resonance not bracketed: {e:?}"),
            }
            println!(
                "  S11 min {:.1} dB at {:.0} MHz; at 2.44 GHz {:.1} dB",
                best.s11_db,
                best.freq_hz / 1e6,
                at24.s11_db
            );
            let mut s = String::from("freq_mhz,s11_db,r_ohm,x_ohm\n");
            for pt in &pts {
                s.push_str(&format!(
                    "{:.1},{:.3},{:.3},{:.3}\n",
                    pt.freq_hz / 1e6,
                    pt.s11_db,
                    pt.z_in.re,
                    pt.z_in.im
                ));
            }
            fs::write(out.join("antenna_s11.csv"), s).unwrap();
            summary.insert("ant_res_mhz".into(), fres.ok().map(|f| f / 1e6).into());
            summary.insert("ant_s11_min".into(), vec![best.freq_hz / 1e6, best.s11_db].into());
            summary.insert("ant_s11_2440".into(), at24.s11_db.into());
        }
        Err(e) => err(&mut summary, "ant_res_mhz", e),
    }
    timing.insert("antenna_s".into(), t.elapsed().as_secs_f64().into());

    // ── 7. EM: the speaker motor → axisym magnetostatics ───────────────────
    println!("\n## EM: speaker motor extracted from the document [doc-extract]");
    let t = Instant::now();
    let mz = g("motor_z");
    let mut coil = mag_part("voice_coil");
    coil.role = Some(MagRole::Coil);
    coil.turns = Some(EmParam::Named("vc_turns".into()));
    coil.current_a = Some(EmParam::Named("vc_current".into()));
    let esel = AxisymMagExtraction {
        parts: vec![mag_part("yoke"), mag_part("magnet"), mag_part("top_plate"), coil],
        domain: DomainSelection {
            pad_factor: 0.0,
            r_max_mm: Some(60.0),
            z_min_mm: Some(mz - 30.0),
            z_max_mm: Some(mz + 46.0),
            cells_per_feature: 2.0,
        },
        tolerance: Default::default(),
    };
    match extract_axisym_mag_spec_from_scene(&doc, &scene, &esel) {
        Ok(ex) => {
            println!(
                "  {} parts, grid {}×{} (dx {:.3} mm), air gap {:?}",
                ex.parts.len(),
                ex.grid.nx,
                ex.grid.ny,
                ex.grid.dx_mm,
                ex.air_gap
            );
            let dev: AxisymMagnetostatics = ex.spec.resolve(&ex.parameters).expect("resolve");
            match dev.solve(ex.grid.nx, ex.grid.ny, &EmOpts::default()) {
                Ok(sol) => {
                    let gap = Annulus {
                        r_inner_mm: 9.75,
                        r_outer_mm: 10.25,
                        z_min_mm: mz + 12.0,
                        z_max_mm: mz + 16.0,
                    };
                    let (br_gap, _) = sol.mean_field_in(&gap, 16);
                    let bl = sol.force_factor(0);
                    let bl2 = sol.force_factor_sampled(0, 32);
                    println!(
                        "  gap |B_r| {:.3} T; Bl {:.2} T·m (ψ route) / {:.2} (sampled)",
                        br_gap.abs(),
                        bl,
                        bl2
                    );
                    summary.insert(
                        "em".into(),
                        json!({"gap_br_t": br_gap.abs(), "bl_tm": bl, "bl_sampled_tm": bl2}),
                    );
                }
                Err(e) => err(&mut summary, "em", format!("{e:?}")),
            }
        }
        Err(e) => err(&mut summary, "em", e),
    }
    timing.insert("em_s".into(), t.elapsed().as_secs_f64().into());

    // ── 8. tolerance: driver-frame press fit ───────────────────────────────
    println!("\n## tolerance: driver-frame press fit, stack from the document [doc-extract; fit assertion doc-injected]");
    let t = Instant::now();
    let tsel = StackupExtraction {
        clearance: "driver-fit".into(),
        feature: FitFeature::Radial,
        fit_window_mm: 1.0,
        upper_mm: None,
        lower_mm: None,
        process_tolerance_mm: None,
        parameters: None,
        tolerance: Default::default(),
    };
    match extract_stackup_spec_from_scene(&doc, &scene, &tsel) {
        Ok(ex) => {
            let stack = ex.spec.resolve(&ex.parameters).expect("resolve");
            println!(
                "  nominal gap {:+.3} mm between {:?}; contributors {:?}",
                ex.gap_mm,
                ex.fit_parts,
                ex.contributors
                    .iter()
                    .map(|c| (c.parameter.clone(), c.coeff))
                    .collect::<Vec<_>>()
            );
            let wc = worst_case(&stack).expect("wc");
            let r = rss(&stack).expect("rss");
            let mc = monte_carlo(
                &stack,
                &McOptions {
                    n: 200_000,
                    seed: 7,
                    batches: 20,
                },
            )
            .expect("mc");
            println!(
                "  worst case [{:+.3}, {:+.3}] mm → {}; RSS yield {:.2}%; MC yield {:.2}% ± {:.2}",
                wc.min_gap,
                wc.max_gap,
                if wc.passes { "PASS" } else { "FAIL" },
                r.yield_estimate * 100.0,
                mc.fit.p * 100.0,
                mc.fit.standard_error * 100.0
            );
            summary.insert(
                "tolerance".into(),
                json!({
                    "gap_mm": ex.gap_mm,
                    "wc": [wc.min_gap, wc.max_gap, wc.passes],
                    "rss_yield": r.yield_estimate,
                    "mc_yield": mc.fit.p,
                }),
            );
        }
        Err(e) => err(&mut summary, "tolerance", e),
    }
    timing.insert("tolerance_s".into(), t.elapsed().as_secs_f64().into());

    // ── 9. joint optimization ──────────────────────────────────────────────
    let opt = std::env::var("OPT").unwrap_or_else(|_| "run".into());
    println!("\n## joint optimization: optimize_document over body_h, port_len [doc-extract QoIs; weights assumed]");
    if opt == "skip" {
        println!("  skipped");
    } else {
        let t = Instant::now();
        let start = vcad_sound::evaluate(&doc).expect("evaluate");
        let (res0, pitch0) = vcad_sound::modal_discretization(
            &start.parts[i_enc].mesh.positions,
            &start.parts[i_enc].mesh.indices,
            Quality::Preview,
        );
        let modal = || ModalFrequencyQoi::first().pinned(res0, pitch0);
        let f1_start = {
            let q = modal();
            q.evaluate(
                &QoiInput {
                    doc: &doc,
                    scene: &start,
                    params: &[],
                    theta: &[],
                },
                Some(i_enc),
            )
            .expect("modal f1")
        };
        println!("  start: modal f1 (preview lattice) {f1_start:.1} Hz");
        let outer: usize = std::env::var("OPT_ITERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4);
        let req = OptimizeDocRequest {
            parameters: vec![
                ParamSpec {
                    name: "body_h".into(),
                    lo: 80.0,
                    hi: 200.0,
                },
                ParamSpec {
                    name: "port_len".into(),
                    lo: 20.0,
                    hi: 100.0,
                },
            ],
            objective: vec![
                // w1·(f_b − 90)²  [assumed target + weight]
                ObjectiveTerm {
                    qoi: QoiRef::new("tuning_hz"),
                    weight: 1.0,
                    shape: TermShape::Target(90.0),
                },
                // w2·mass (kg)  [assumed weight]
                ObjectiveTerm {
                    qoi: QoiRef::part("mass", i_enc),
                    weight: 2000.0,
                    shape: TermShape::Linear,
                },
            ],
            constraints: vec![
                // Port mouth stays 10 mm above the motor (motor_z + 18 + 10).
                Constraint::Expr {
                    expr: "port_len <= body_h - motor_z - 28".into(),
                    scale: None,
                },
                // Don't let the shell get floppier than it is.
                Constraint::Qoi {
                    qoi: QoiRef::part("modal_f1", i_enc),
                    op: ConstraintOp::Ge,
                    value: f1_start,
                },
            ],
            options: OptimizeDocOptions {
                density: 1.24e-6, // PLA, kg/mm³
                max_outer: outer,
                max_inner: 6,
                grad_tol: 1e-3,
                feas_tol: 2e-3,
                circle_segments: 48,
                check_seam_fd: false,
                ..Default::default()
            },
        };
        let reg = QoiRegistry::new()
            .with(Box::new(TuningQoi))
            .with(Box::new(modal()));
        let traj: Mutex<Vec<Value>> = Mutex::new(Vec::new());
        let on_step = |s: &TrajectoryStep| {
            let q = |n: &str| s.qois.iter().find(|q| q.qoi.qoi == n).map(|q| q.value);
            println!(
                "  it {:>2} r{} body_h {:.1} port_len {:.1}  f_b {:.1} Hz  mass {:.1} g  f1 {:.1} Hz  viol {:.1e}  ({:.0} s)",
                s.iter,
                s.round,
                s.theta[0],
                s.theta[1],
                q("tuning_hz").unwrap_or(f64::NAN),
                q("mass").unwrap_or(f64::NAN) * 1e3,
                q("modal_f1").unwrap_or(f64::NAN),
                s.max_violation,
                s.cost.wall_ms / 1e3
            );
            traj.lock().unwrap().push(json!({
                "iter": s.iter, "round": s.round, "theta": s.theta,
                "fb_hz": q("tuning_hz"), "mass_kg": q("mass"), "f1_hz": q("modal_f1"),
                "objective": s.objective, "viol": s.max_violation,
            }));
        };
        match optimize_document(
            &doc,
            &req,
            &reg,
            &OptimizeHooks {
                cancel: None,
                on_step: Some(&on_step),
            },
        ) {
            Ok(res) => {
                println!(
                    "  → body_h {:.1}, port_len {:.1}; stop {:?}; KKT feasible {} satisfied {}; {} doc evals, {} plug-in evals, {:.0} s",
                    res.theta[0],
                    res.theta[1],
                    res.stop,
                    res.kkt.feasible,
                    res.kkt.satisfied,
                    res.document_evals,
                    res.plugin_evals,
                    res.wall_ms / 1e3
                );
                summary.insert(
                    "optimize".into(),
                    json!({
                        "theta": res.theta, "stop": format!("{:?}", res.stop),
                        "kkt_feasible": res.kkt.feasible, "kkt_satisfied": res.kkt.satisfied,
                        "f1_floor_hz": f1_start,
                        "trajectory": traj.lock().unwrap().clone(),
                    }),
                );
            }
            Err(e) => err(&mut summary, "optimize", e),
        }
        timing.insert("optimize_s".into(), t.elapsed().as_secs_f64().into());
    }

    summary.insert("timing".into(), timing.into());
    fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!(
        "\nwrote {}/{{puck.vcad,port_velocity.csv,antenna_s11.csv,summary.json}}",
        out.display()
    );
}
