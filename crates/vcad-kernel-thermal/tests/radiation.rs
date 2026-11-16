//! The M7 radiation validation ladder. Each rung is closed-form, and each
//! states what it proves.
//!
//! 1. Slab conducting into a radiating face → the scalar surface balance
//!    (T₀ − T_s)·k/L = εσ(T_s⁴ − T_∞⁴) [+ h(T_s − T_∞)]. With no source the
//!    profile is linear, so the finite-volume field is *exact* at voxel
//!    centers: this proves the half-cell-in-series surface solve and the
//!    Newton convergence to machine precision.
//! 2. Radiating sphere with uniform generation → T_s = (P/(εσA) + T_∞⁴)^¼
//!    and the parabolic interior T(0) − T_s = q‴R²/(6k). A voxel sphere is
//!    a staircase whose area is ~1.5× the true area at every resolution;
//!    with `area_scale` = true/voxel area the surface temperature lands on
//!    the closed form; without it the part reads cold (unsafe direction),
//!    which the test also pins.
//! 3. Lumped radiative cooling (Bi ≪ 1) → the implicit closed form
//!    t = C/(εσA)·[F(T₀) − F(T)], F(T) = (ln((T−a)/(T+a)) − 2·atan(T/a))/(4a³).
//!    Proves the transient Newton path, first order in Δt, with the energy
//!    audit still an identity.
//! 4. Two-surface enclosure → ε_eff = 1/(1/ε + (A_s/A_w)(1/ε_w − 1)): the
//!    slab of rung 1 inside an enclosure must equal the open slab at ε_eff.
//! 5. Fail-closed range checks and the serde seam.

use std::collections::BTreeMap;

use vcad_kernel_thermal::model::{
    Boundary, Enclosure, MaterialRegion, ModelError, PowerSource, Shape, ThermalModel,
    KELVIN_OFFSET_C, STEFAN_BOLTZMANN,
};
use vcad_kernel_thermal::solve::{solve_steady, SolveError, SolveOptions};
use vcad_kernel_thermal::spec::{ParamRole, ThermalSpec};
use vcad_kernel_thermal::transient::{solve_transient, TransientOptions};

fn tight() -> SolveOptions {
    SolveOptions {
        tol: 1e-12,
        max_iters: 200_000,
    }
}

fn rad(emissivity: f64, ambient_c: f64, h_w_m2k: f64) -> Boundary {
    Boundary::Radiation {
        emissivity,
        ambient_c,
        h_w_m2k,
        area_scale: 1.0,
        enclosure: None,
    }
}

/// Bisection on a monotone increasing scalar function.
fn bisect(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if f(mid) > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.5 * (lo + hi)
}

const K: f64 = 50.0;
const L_MM: f64 = 20.0;

fn slab(nx: usize, far: Boundary) -> ThermalModel {
    let mut m = ThermalModel::new([0.0, 0.0, 0.0], [L_MM, 5.0, 5.0], [nx, 1, 1]);
    m.materials.push(MaterialRegion::isotropic(
        Shape::Box {
            min_mm: [0.0, 0.0, 0.0],
            size_mm: [L_MM, 5.0, 5.0],
        },
        K,
    ));
    m.domain_faces[0] = Boundary::FixedTemperature {
        temperature_c: 500.0,
    };
    m.domain_faces[1] = far;
    m
}

/// Exact surface temperature of the rung-1 slab.
fn slab_surface_c(eps: f64, ta_c: f64, h: f64) -> f64 {
    let l = L_MM * 1e-3;
    let ta_k = ta_c + KELVIN_OFFSET_C;
    bisect(
        |ts_c| {
            let ts_k = ts_c + KELVIN_OFFSET_C;
            eps * STEFAN_BOLTZMANN * (ts_k.powi(4) - ta_k.powi(4)) + h * (ts_c - ta_c)
                - (500.0 - ts_c) * K / l
        },
        ta_c,
        500.0,
    )
}

#[test]
fn slab_into_a_radiating_face_is_exact() {
    for (eps, h) in [(0.8, 0.0), (0.3, 15.0), (1.0, 0.0)] {
        let m = slab(8, rad(eps, 20.0, h));
        let sol = solve_steady(&m, &tight()).expect("solve");
        let ts = slab_surface_c(eps, 20.0, h);
        let l = L_MM * 1e-3;
        let q = (500.0 - ts) * K / l;
        let dx = l / 8.0;
        for i in 0..8 {
            let x = (i as f64 + 0.5) * dx;
            let exact = 500.0 - q * x / K;
            let got = sol.temperature_c(i, 0, 0);
            assert!(
                (got - exact).abs() < 1e-8,
                "eps={eps} h={h} voxel {i}: computed {got:.10}, exact {exact:.10}"
            );
        }
        let r = &sol.radiation[0];
        assert!(
            (r.max_surface_c - ts).abs() < 1e-8,
            "surface {} vs {ts}",
            r.max_surface_c
        );
        assert!(sol.newton_iterations > 0);
        assert!(sol.energy.residual_rel < 1e-10, "{:?}", sol.energy);
        // Radiation alone accounts for the radiated part of the outflow.
        let ta_k = 20.0 + KELVIN_OFFSET_C;
        let area = 25e-6;
        let rad_exact =
            eps * STEFAN_BOLTZMANN * ((ts + KELVIN_OFFSET_C).powi(4) - ta_k.powi(4)) * area;
        assert!((r.radiated_w - rad_exact).abs() < 1e-9 * rad_exact.max(1.0));
    }
}

/// Uniformly heated copper-ish sphere, R = 10 mm, P = 50 W, ε = 0.9.
fn sphere(div: usize, area_scale: f64) -> ThermalModel {
    let r = 10.0;
    let box_mm = 24.0;
    let c = box_mm / 2.0;
    let mut m = ThermalModel::new([0.0, 0.0, 0.0], [box_mm; 3], [div; 3]);
    // Paint the ball with voxel materials (no sphere primitive): index 0
    // inside, void outside.
    m.materials.push(MaterialRegion::isotropic(
        Shape::Box {
            min_mm: [0.0; 3],
            size_mm: [box_mm; 3],
        },
        200.0,
    ));
    let d = box_mm / div as f64;
    let mut idx = Vec::with_capacity(div * div * div);
    for k in 0..div {
        for j in 0..div {
            for i in 0..div {
                let p = [
                    (i as f64 + 0.5) * d,
                    (j as f64 + 0.5) * d,
                    (k as f64 + 0.5) * d,
                ];
                let r2 = (p[0] - c).powi(2) + (p[1] - c).powi(2) + (p[2] - c).powi(2);
                idx.push(if r2 <= r * r { 0 } else { -1 });
            }
        }
    }
    m.voxel_materials = Some(vcad_kernel_thermal::model::VoxelMaterials {
        material_index: idx,
    });
    m.sources.push(PowerSource {
        name: "core".into(),
        shape: Shape::Box {
            min_mm: [0.0; 3],
            size_mm: [box_mm; 3],
        },
        power_w: 50.0,
    });
    m.exposed = Boundary::Radiation {
        emissivity: 0.9,
        ambient_c: 25.0,
        h_w_m2k: 0.0,
        area_scale,
        enclosure: None,
    };
    m
}

#[test]
fn radiating_sphere_matches_the_closed_form_with_area_correction() {
    let r_m = 0.010;
    let area = 4.0 * std::f64::consts::PI * r_m * r_m;
    let ta_k = 25.0 + KELVIN_OFFSET_C;
    let ts_exact_c =
        (50.0 / (0.9 * STEFAN_BOLTZMANN * area) + ta_k.powi(4)).powf(0.25) - KELVIN_OFFSET_C;

    let opts = SolveOptions::default();
    // Pass 1: the voxel staircase area (geometry only).
    let raw = solve_steady(&sphere(40, 1.0), &opts).expect("raw solve");
    let voxel_area = raw.radiation[0].voxel_area_m2;
    let staircase = voxel_area / area;
    assert!(
        (1.35..1.65).contains(&staircase),
        "a voxel sphere's staircase area should be ~1.5x the true area, got {staircase:.3}"
    );
    // Uncorrected, the over-counted area over-cools: unsafe direction.
    assert!(raw.radiation[0].mean_surface_c < ts_exact_c - 50.0);

    // Pass 2: area_scale = true/voxel area.
    let sol = solve_steady(&sphere(40, area / voxel_area), &opts).expect("solve");
    let rr = &sol.radiation[0];
    assert!((rr.area_m2 - area).abs() < 1e-12);
    let err = rr.mean_surface_c - ts_exact_c;
    println!(
        "sphere: staircase x{staircase:.3}; uncorrected surface {:.2} C; corrected {:.2} C vs \
         closed form {ts_exact_c:.2} C (err {err:+.3} K); Newton {}",
        raw.radiation[0].mean_surface_c, rr.mean_surface_c, sol.newton_iterations
    );
    assert!(
        err.abs() < 0.01 * (ts_exact_c - 25.0),
        "mean surface {:.2} C vs closed form {ts_exact_c:.2} C",
        rr.mean_surface_c
    );
    // Interior: the parabolic rise q'''R²/(6k), on a staircase ball the
    // voxel volume differs from 4/3πR³, so allow 30% on a ~1 K rise.
    let q3 = 50.0 / (4.0 / 3.0 * std::f64::consts::PI * r_m.powi(3));
    let rise = q3 * r_m * r_m / (6.0 * 200.0);
    let got_rise = sol.t_max_c - rr.mean_surface_c;
    assert!(
        got_rise > 0.4 * rise && got_rise < 1.6 * rise,
        "interior rise {got_rise:.3} K vs {rise:.3} K"
    );
    assert!(sol.energy.residual_rel < 1e-7, "{:?}", sol.energy);
    assert!((sol.energy.radiation_out_w - 50.0).abs() < 1e-5);
}

#[test]
fn lumped_radiative_cooling_matches_the_implicit_closed_form() {
    // 4 mm copper cube radiating from all six domain faces, ε = 0.8,
    // 800 °C into 20 °C. h_rad ≈ 4εσT³ ≲ 230 W/m²K → Bi ≈ 4e-4.
    let (eps, rc, side): (f64, f64, f64) = (0.8, 3.45e6, 0.004);
    let mut m = ThermalModel::new([0.0; 3], [4.0; 3], [4, 4, 4]);
    m.materials.push(
        MaterialRegion::isotropic(
            Shape::Box {
                min_mm: [0.0; 3],
                size_mm: [4.0; 3],
            },
            400.0,
        )
        .with_heat_capacity(rc),
    );
    m.domain_faces = [rad(eps, 20.0, 0.0); 6];
    let (dt, steps) = (0.02, 2500); // 50 s
    let tsol = solve_transient(
        &m,
        &tight(),
        &TransientOptions {
            dt_s: dt,
            steps,
            initial_c: 800.0,
            snapshot_every: 0,
        },
    )
    .expect("transient");

    let a = 20.0 + KELVIN_OFFSET_C;
    let f = |t_k: f64| (((t_k - a) / (t_k + a)).ln() - 2.0 * (t_k / a).atan()) / (4.0 * a.powi(3));
    let cap = rc * side.powi(3);
    let area = 6.0 * side * side;
    let k = eps * STEFAN_BOLTZMANN * area / cap;
    let t0 = 800.0 + KELVIN_OFFSET_C;
    for (step, &time) in tsol.times_s.iter().enumerate().step_by(500) {
        let target = f(t0) - k * time;
        let exact_k = bisect(|t| f(t) - target, a + 1e-9, t0);
        let got = tsol.t_max_c[step] + KELVIN_OFFSET_C;
        assert!(
            (got - exact_k).abs() < 1.0,
            "t = {time:.1} s: computed {got:.3} K, closed form {exact_k:.3} K"
        );
    }
    let mut worst = 0.0_f64;
    for (step, &time) in tsol.times_s.iter().enumerate() {
        let target = f(t0) - k * time;
        let exact_k = bisect(|t| f(t) - target, a + 1e-9, t0);
        worst = worst.max((tsol.t_max_c[step] + KELVIN_OFFSET_C - exact_k).abs());
    }
    println!(
        "lumped: worst |T - closed form| over 50 s = {worst:.4} K (dt {dt} s); audit {:.2e}; \
         Newton {} over {steps} steps",
        tsol.energy_audit_residual_rel, tsol.newton_iterations_total
    );
    let end = tsol.t_max_c.last().unwrap() + KELVIN_OFFSET_C;
    assert!(end < t0 - 400.0, "should have cooled hard: {end} K");
    assert!(tsol.newton_iterations_total >= steps);
    assert!(
        tsol.energy_audit_residual_rel < 1e-7,
        "audit {}",
        tsol.energy_audit_residual_rel
    );
}

#[test]
fn transient_relaxes_to_the_radiating_steady_state() {
    let mut m = ThermalModel::new([0.0; 3], [20.0, 20.0, 2.0], [10, 10, 2]);
    m.materials.push(
        MaterialRegion::isotropic(
            Shape::Box {
                min_mm: [0.0; 3],
                size_mm: [20.0, 20.0, 2.0],
            },
            20.0,
        )
        .with_heat_capacity(2.0e6),
    );
    m.sources.push(PowerSource {
        name: "heater".into(),
        shape: Shape::Box {
            min_mm: [8.0, 8.0, 0.0],
            size_mm: [4.0, 4.0, 2.0],
        },
        power_w: 2.0,
    });
    m.domain_faces[4] = Boundary::Convection {
        h_w_m2k: 8.0,
        ambient_c: 25.0,
    };
    m.domain_faces[5] = rad(0.9, 25.0, 8.0);
    let steady = solve_steady(&m, &tight()).expect("steady");
    let tsol = solve_transient(
        &m,
        &tight(),
        &TransientOptions {
            dt_s: 20.0,
            steps: 400,
            initial_c: 25.0,
            snapshot_every: 0,
        },
    )
    .expect("transient");
    assert!(
        (tsol.final_state.t_max_c - steady.t_max_c).abs() < 1e-4,
        "transient end {} vs steady {}",
        tsol.final_state.t_max_c,
        steady.t_max_c
    );
    assert!(
        tsol.energy_audit_residual_rel < 1e-7,
        "audit {} (newton {}, cg {})",
        tsol.energy_audit_residual_rel,
        tsol.newton_iterations_total,
        tsol.cg_iterations_total
    );
}

#[test]
fn enclosure_is_the_open_slab_at_effective_emissivity() {
    let area_s = 25e-6; // the slab's +x face
    let enc = Enclosure {
        emissivity: 0.5,
        area_m2: 2.0 * area_s,
    };
    let eps = 0.5;
    let eps_eff = 1.0 / (1.0 / eps + 0.5 * (1.0 / 0.5 - 1.0)); // 0.4
    let boxed = slab(
        8,
        Boundary::Radiation {
            emissivity: eps,
            ambient_c: 20.0,
            h_w_m2k: 0.0,
            area_scale: 1.0,
            enclosure: Some(enc),
        },
    );
    let open = slab(8, rad(eps_eff, 20.0, 0.0));
    let a = solve_steady(&boxed, &tight()).unwrap();
    let b = solve_steady(&open, &tight()).unwrap();
    assert!((a.radiation[0].effective_emissivity - 0.4).abs() < 1e-14);
    for (x, y) in a.t_c.iter().zip(&b.t_c) {
        assert!((x - y).abs() < 1e-9);
    }
    // A black wall or a huge enclosure is the open limit.
    let big = slab(
        8,
        Boundary::Radiation {
            emissivity: eps,
            ambient_c: 20.0,
            h_w_m2k: 0.0,
            area_scale: 1.0,
            enclosure: Some(Enclosure {
                emissivity: 1.0,
                area_m2: 1.0,
            }),
        },
    );
    let c = solve_steady(&big, &tight()).unwrap();
    assert!((c.radiation[0].effective_emissivity - eps).abs() < 1e-14);
}

#[test]
fn radiation_ranges_fail_closed() {
    let bad = |bc: Boundary| match solve_steady(&slab(4, bc), &SolveOptions::default()) {
        Err(SolveError::Model(ModelError::InvalidRadiation(_))) => {}
        other => panic!("expected InvalidRadiation, got {other:?}"),
    };
    bad(rad(0.0, 20.0, 0.0));
    bad(rad(1.2, 20.0, 0.0));
    bad(rad(f64::NAN, 20.0, 0.0));
    bad(rad(0.5, -300.0, 0.0));
    bad(rad(0.5, 20.0, -1.0));
    bad(Boundary::Radiation {
        emissivity: 0.5,
        ambient_c: 20.0,
        h_w_m2k: 0.0,
        area_scale: 0.0,
        enclosure: None,
    });
    // Enclosure no bigger than what it encloses.
    bad(Boundary::Radiation {
        emissivity: 0.5,
        ambient_c: 20.0,
        h_w_m2k: 0.0,
        area_scale: 1.0,
        enclosure: Some(Enclosure {
            emissivity: 0.5,
            area_m2: 25e-6,
        }),
    });
    bad(Boundary::Radiation {
        emissivity: 0.5,
        ambient_c: 20.0,
        h_w_m2k: 0.0,
        area_scale: 1.0,
        enclosure: Some(Enclosure {
            emissivity: 0.0,
            area_m2: 1.0,
        }),
    });
    // The linear-only graph paths refuse rather than drop the loss.
    let m = slab(4, rad(0.5, 20.0, 0.0));
    assert!(matches!(
        vcad_kernel_thermal::graph::device_assembly(&m),
        Err(SolveError::RadiationUnsupported(_))
    ));
}

#[test]
fn a_radiation_only_part_in_vacuum_is_grounded() {
    // No conduction path to anything: radiation alone must ground it.
    let mut m = ThermalModel::new([0.0; 3], [10.0; 3], [5, 5, 5]);
    m.materials.push(MaterialRegion::isotropic(
        Shape::Box {
            min_mm: [0.0; 3],
            size_mm: [10.0; 3],
        },
        100.0,
    ));
    m.sources.push(PowerSource {
        name: "p".into(),
        shape: Shape::Box {
            min_mm: [0.0; 3],
            size_mm: [10.0; 3],
        },
        power_w: 1.0,
    });
    m.domain_faces = [rad(0.6, 25.0, 0.0); 6];
    let sol = solve_steady(&m, &tight()).expect("radiation-only solve");
    let area = 6.0 * 1e-4;
    let ta = 25.0 + KELVIN_OFFSET_C;
    let ts = (1.0 / (0.6 * STEFAN_BOLTZMANN * area) + ta.powi(4)).powf(0.25) - KELVIN_OFFSET_C;
    assert!((sol.radiation[0].mean_surface_c - ts).abs() < 0.05);
    assert_eq!(sol.reference_c, Some(25.0));
}

#[test]
fn spec_round_trips_radiation_and_classifies_roles() {
    let json = r#"{
        "origin_mm": [0.0, 0.0, 0.0],
        "size_mm": [20.0, 5.0, 5.0],
        "divisions": [8, 1, 1],
        "materials": [{"shape": {"type": "Box", "min_mm": [0.0, 0.0, 0.0],
                       "size_mm": [20.0, 5.0, 5.0]}, "k_w_mk": [50.0, 50.0, 50.0]}],
        "domain_faces": [
            {"type": "FixedTemperature", "temperature_c": 500.0},
            {"type": "Radiation", "emissivity": "eps_grid", "ambient_c": "t_wall",
             "enclosure": {"emissivity": "eps_wall", "area_m2": 0.25}},
            {"type": "Adiabatic"}, {"type": "Adiabatic"},
            {"type": "Adiabatic"}, {"type": "Adiabatic"}
        ]
    }"#;
    let spec: ThermalSpec = serde_json::from_str(json).expect("parse");
    let params: BTreeMap<String, f64> = [
        ("eps_grid".to_string(), 0.8),
        ("t_wall".to_string(), 20.0),
        ("eps_wall".to_string(), 0.3),
    ]
    .into_iter()
    .collect();
    let model = spec.resolve(&params).expect("resolve");
    match model.domain_faces[1] {
        Boundary::Radiation {
            emissivity,
            ambient_c,
            h_w_m2k,
            area_scale,
            enclosure,
        } => {
            assert_eq!(
                (emissivity, ambient_c, h_w_m2k, area_scale),
                (0.8, 20.0, 0.0, 1.0)
            );
            assert_eq!(enclosure.unwrap().emissivity, 0.3);
        }
        other => panic!("expected radiation, got {other:?}"),
    }
    let roles = spec.parameter_roles();
    assert_eq!(roles["eps_grid"], ParamRole::Adjoint);
    assert_eq!(roles["t_wall"], ParamRole::FiniteDifference);
    assert_eq!(roles["eps_wall"], ParamRole::FiniteDifference);
    // Unbound emissivity fails closed.
    assert!(spec.resolve(&BTreeMap::new()).is_err());
    // Literal round trip.
    let lit = ThermalSpec::from_model(&model);
    let back: ThermalSpec = serde_json::from_str(&serde_json::to_string(&lit).unwrap()).unwrap();
    assert_eq!(back.resolve(&BTreeMap::new()).unwrap(), model);
    // And the receipt names the model.
    let sol = solve_steady(&model, &SolveOptions::default()).unwrap();
    let set =
        vcad_kernel_thermal::receipt::predicted_claims(&model, &sol, &SolveOptions::default());
    assert!(set
        .provenance
        .radiation_model
        .as_deref()
        .unwrap()
        .contains("gray-diffuse"));
    assert!(set.claims.iter().any(|c| c.name == "radiated_w"));
    assert!(set.claims[0].note.contains("radiation"));
    assert!(!set.claims[0].note.contains("radiation not modeled"));
}
