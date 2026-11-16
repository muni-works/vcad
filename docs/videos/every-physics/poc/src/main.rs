//! every-physics PoC: ONE loon document (`../puck.loon`), many solvers.
//!
//! Every number printed is tagged with where its input came from:
//!
//! - `[doc-geom]`  — read off the evaluated solid (mesh volume, BRep, modal
//!                   lattice). This is the real document→solver seam.
//! - `[doc-param]` — read off `doc.parameters` and hand-transcribed into a
//!                   solver spec by THIS driver. This is the missing adapter,
//!                   prototyped; it is not a vcad feature yet.
//! - `[assumed]`   — not in the document at all (film coefficient, air temp).
//!
//! Run (on a build box, never the Mac):
//!   cargo run --release -- ../puck.loon out/

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use vcad_eval::{evaluate_document, EvalOptions, EvaluatedMesh};
use vcad_ir::Document;
use vcad_kernel_acoustics::cavity::Cavity;
use vcad_kernel_acoustics::complex::Cplx;
use vcad_kernel_acoustics::fom::{self, port_volume_velocity};
use vcad_kernel_acoustics::helmholtz::{solve_driven, Source};
use vcad_kernel_acoustics::lumped;
use vcad_kernel_acoustics::medium::Medium;
use vcad_kernel_antenna::ecad::add_trace_as_wire;
use vcad_kernel_antenna::{find_resonance, sweep, Mesh, SolveOptions as AntOpts, WireGrid};
use vcad_kernel_dfm::{Process, RulePack};
use vcad_kernel_tessellate::TessellationParams;
use vcad_kernel_thermal::model::{Boundary, MaterialRegion, PowerSource, Shape, ThermalModel};
use vcad_kernel_thermal::solve::{solve_steady, SolveOptions as ThOpts};
use vcad_sound::{part_modes_alone, ModesRequest, Quality};

fn opts() -> EvalOptions {
    EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src_path = args.get(1).map(String::as_str).unwrap_or("../puck.loon");
    let out = Path::new(args.get(2).map(String::as_str).unwrap_or("out"));
    fs::create_dir_all(out).expect("out dir");

    let source = fs::read_to_string(src_path).expect("read loon");
    let doc: Document = vcad_loon::eval_vcad(&source, None).expect("loon eval");
    fs::write(
        out.join("puck.vcad"),
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
    let p: HashMap<String, f64> = vcad_ir::resolve_parameters(&doc.parameters).expect("params");
    let g = |k: &str| {
        *p.get(k)
            .unwrap_or_else(|| panic!("doc has no parameter {k}"))
    };
    let mut summary = serde_json::Map::new();

    // ── 0. evaluate the one document ───────────────────────────────────────
    println!("# puck — one document, many solvers\n");
    let scene = evaluate_document(&doc, &opts()).expect("evaluate");
    assert!(
        scene.failures.is_empty(),
        "root failures: {:?}",
        scene.failures
    );
    let find = |mat: &str| {
        scene
            .parts
            .iter()
            .position(|pt| pt.material == mat)
            .unwrap_or_else(|| panic!("no part with material {mat}"))
    };
    let (i_enc, i_air) = (find("pla"), find("air"));
    let v_shell = mesh_volume(&scene.parts[i_enc].mesh);
    let v_air = mesh_volume(&scene.parts[i_air].mesh);
    let mass_g = v_shell * 1e-3 * 1.24; // PLA 1.24 g/cm³ (material density not in the IR)
    println!("## mass properties [doc-geom]");
    println!(
        "  enclosure volume  {:.0} mm³  → {:.1} g PLA (100% infill)",
        v_shell, mass_g
    );
    println!("  enclosed air      {:.3} L", v_air * 1e-6);
    summary.insert("shell_mm3".into(), v_shell.into());
    summary.insert("shell_mass_g_pla".into(), mass_g.into());
    summary.insert("air_l".into(), (v_air * 1e-6).into());

    // ── 1. diff seam: d(shell mass)/d(wall) through the real document ──────
    println!("\n## diff seam: document_parameter_gradient [doc-geom]");
    let tess = TessellationParams {
        circle_segments: 64,
        height_segments: 2,
        ..Default::default()
    };
    for param in ["wall", "body_h", "port_len"] {
        match vcad_eval::diff::document_parameter_gradient(&doc, param, 1.24e-3, &tess, 1e-4) {
            Ok(gr) => {
                for pg in &gr {
                    println!(
                        "  part {}: V = {:.0} mm³, dV/d{param} = {:+.1} mm³/mm, dm/d{param} = {:+.3} g/mm",
                        pg.part_index, pg.properties.volume, pg.derivative.volume, pg.derivative.mass
                    );
                }
                summary.insert(
                    format!("dV_d{param}"),
                    gr.iter()
                        .map(|pg| pg.derivative.volume)
                        .collect::<Vec<_>>()
                        .into(),
                );
            }
            Err(e) => {
                println!("  d/d{param}: FAILED — {e}");
                summary.insert(format!("dV_d{param}"), format!("error: {e}").into());
            }
        }
    }

    // ── 2. DFM (FDM rule pack) on the enclosure BRep ───────────────────────
    println!("\n## DFM, FDM pack [doc-geom]");
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
        println!("  enclosure has no BRep (mesh-only) — DFM skipped");
    }

    // ── 3. structural modal (shell ring/panel modes) on the real solid ─────
    // MODAL_QUALITY=fine|preview|skip (default fine). Fine is ≤~40k cells and
    // takes many minutes on this shell; preview is ≤~6k cells.
    let mq = std::env::var("MODAL_QUALITY").unwrap_or_else(|_| "fine".into());
    println!("\n## structural modes, free-free PLA, quality={mq} [doc-geom]");
    let req = ModesRequest {
        part: i_enc,
        modes: 6,
        quality: if mq == "preview" {
            Quality::Preview
        } else {
            Quality::Fine
        },
        material: Some("pla".into()),
        shapes: false,
    };
    let t0 = std::time::Instant::now();
    let modal = if mq == "skip" {
        println!("  skipped");
        None
    } else {
        Some(part_modes_alone(&doc, &req))
    };
    if let Some(modal) = modal {
        println!(
            "  ({:.0} s wall, shared box — not a benchmark)",
            t0.elapsed().as_secs_f64()
        );
        match modal {
            Ok(m) => {
                for md in &m.modes {
                    println!("  {:8.1} Hz  ({} {:+.0}¢)", md.hz, md.note, md.cents);
                }
                summary.insert(
                    "modes_hz".into(),
                    m.modes.iter().map(|x| x.hz).collect::<Vec<_>>().into(),
                );
            }
            Err(e) => {
                println!("  modal FAILED — {e:?}");
                summary.insert("modes_hz".into(), format!("error: {e:?}").into());
            }
        }
    }

    // ── 4. acoustics: port tuning ──────────────────────────────────────────
    // Box compliance from the document's measured air volume [doc-geom];
    // port radius/length and piston radius from parameters [doc-param].
    // The axisym solver puts the port OUTSIDE the box, the document hangs it
    // INSIDE: map to an equivalent-volume cylinder of the cavity radius.
    println!("\n## acoustics: bass-reflex tuning [doc-geom V_box + doc-param port]");
    let air = Medium::air(20.0); // [assumed] 20 °C
    let (port_r, port_len, ri) = (g("port_r"), g("port_len"), g("body_r") - g("wall"));
    let h_eq = v_air / (std::f64::consts::PI * ri * ri);
    let band = lumped::ported_box_tuning_mm(&air, v_air, port_r, port_len);
    println!(
        "  lumped f_b band [{:.1}, {:.1}, {:.1}] Hz (min/nominal/max), V_box {:.3} L",
        band.f_min_hz,
        band.f_nominal_hz,
        band.f_max_hz,
        v_air * 1e-6
    );
    let cav = Cavity::ported_box(ri, h_eq, port_r, port_len, g("driver_r"), air);
    let (zmin, zmax) = cav.z_span_mm();
    let nz = (((zmax - zmin) / 2.0).round() as usize).max(60) + 1;
    let nr = 27;
    let f_field = fom::driven_tuning_hz(
        &cav,
        nr,
        nz,
        (band.f_min_hz * 0.6).max(10.0),
        band.f_max_hz * 1.8,
        40,
    );
    match f_field {
        Some(f) => println!("  field  f_b {f:.1} Hz  (field/nominal {:.3}; docs say pressure-release mouth reads ~15% high)", f / band.f_nominal_hz),
        None => println!("  field tuning not found in bracket"),
    }
    let mut csv = String::from("freq_hz,port_volume_velocity_m3s\n");
    let (flo, fhi) = (20.0, 400.0);
    for i in 0..77 {
        let f = flo + (fhi - flo) * i as f64 / 76.0;
        if let Ok(fld) = solve_driven(
            &cav,
            nr,
            nz,
            f,
            Source::Piston {
                velocity: Cplx::ONE,
            },
        ) {
            csv.push_str(&format!(
                "{f:.2},{:.6e}\n",
                port_volume_velocity(&fld, &cav).abs()
            ));
        }
    }
    fs::write(out.join("port_velocity.csv"), csv).unwrap();
    summary.insert(
        "fb_lumped_hz".into(),
        vec![band.f_min_hz, band.f_nominal_hz, band.f_max_hz].into(),
    );
    summary.insert("fb_field_hz".into(), f_field.into());

    // ── 5. thermal: amp die on the board inside the box ────────────────────
    println!("\n## thermal: amp on board [doc-param geometry + power, assumed h]");
    let (bw, bl, q) = (g("board_w"), g("board_l"), g("amp_loss_w"));
    let mut th = serde_json::Map::new();
    for h in [5.0, 10.0] {
        let mut m = ThermalModel::new(
            [0.0, 0.0, 0.0],
            [bw, bl, 2.6],
            [bw as usize, bl as usize, 13],
        );
        m.materials.push(MaterialRegion::anisotropic(
            Shape::Box {
                min_mm: [0.0, 0.0, 0.0],
                size_mm: [bw, bl, 1.6],
            },
            [15.0, 15.0, 0.4], // 2-layer FR4 + pour, in-plane / through-plane [assumed]
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
        let conv = Boundary::Convection {
            h_w_m2k: h,
            ambient_c: 25.0,
        };
        m.domain_faces[4] = conv;
        m.domain_faces[5] = conv;
        m.exposed = conv;
        match solve_steady(&m, &ThOpts::default()) {
            Ok(sol) => {
                println!(
                    "  h = {h:>4.0} W/m²K: T_max {:.1} °C, θ_ja {:.1} K/W, energy residual {:.1e}",
                    sol.t_max_c,
                    sol.sources[0].theta_c_per_w.unwrap_or(f64::NAN),
                    sol.energy.residual_rel
                );
                th.insert(format!("tmax_c_h{h}"), sol.t_max_c.into());
            }
            Err(e) => println!("  h = {h}: FAILED {e:?}"),
        }
    }
    summary.insert("thermal".into(), th.into());

    // ── 6. antenna: 2.4 GHz monopole ───────────────────────────────────────
    println!("\n## antenna: printed monopole over ground [doc-param length]");
    let l = g("ant_len");
    let mut grid = WireGrid::new();
    grid.set_ground_plane(true); // infinite ground [assumed] — the board is 50 mm, ~0.4 λ
    add_trace_as_wire(&mut grid, &[[0.0, 0.0, 0.0], [0.0, 0.0, l]], 1.0, &[16]).unwrap();
    let mesh = Mesh::build(&grid).unwrap();
    let feed = mesh.nearest_basis([0.0, 0.0, 0.0]).unwrap();
    let ao = AntOpts::default();
    let f_qw = 299.792458e9 / (4.0 * l);
    let fres = find_resonance(&mesh, feed, 0.75 * f_qw, 1.05 * f_qw, &ao);
    let freqs: Vec<f64> = (0..61).map(|i| 1.8e9 + 1.2e9 * i as f64 / 60.0).collect();
    let pts = sweep(&mesh, feed, &freqs, 50.0, &ao).expect("sweep");
    let best = pts
        .iter()
        .min_by(|a, b| a.s11_db.total_cmp(&b.s11_db))
        .unwrap();
    match &fres {
        Ok(f) => println!("  resonance (Im Z = 0) {:.0} MHz", f / 1e6),
        Err(e) => println!("  resonance not bracketed: {e:?}"),
    }
    println!(
        "  S11 min {:.1} dB at {:.0} MHz (vs 50 Ω)",
        best.s11_db,
        best.freq_hz / 1e6
    );
    let at24 = pts
        .iter()
        .min_by(|a, b| {
            (a.freq_hz - 2.44e9)
                .abs()
                .total_cmp(&(b.freq_hz - 2.44e9).abs())
        })
        .unwrap();
    println!("  S11 at 2.44 GHz {:.1} dB", at24.s11_db);
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
    summary.insert(
        "ant_s11_min".into(),
        vec![best.freq_hz / 1e6, best.s11_db].into(),
    );
    summary.insert("ant_s11_2440".into(), at24.s11_db.into());

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
