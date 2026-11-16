//! One machine, one receipt, five solver domains.
//!
//! Prices the shielded-grid IEC experiment (docs/shielded-grid-experiment.md)
//! across five kernel crates and assembles every prediction into a single
//! unified [`vcad_receipt::DesignReceipt`]:
//!
//! - **particle** — yield, interception, Q, distance-to-Lawson
//! - **em** — shield-coil inductance, stored energy, inter-coil force
//! - **thermal** — cathode temperature under interception heating
//! - **neutronics** — operator dose behind the HDPE shield (with MC error)
//! - **tolerance** — does the cusp geometry survive real machining scatter?
//!
//! The domains are wired, not stapled: particle's interception fraction
//! sets thermal's heat load, particle's neutron rate is neutronics' source
//! term, and em's inter-coil force is the load the tolerance-checked mounts
//! must carry. Every claim is `basis: predicted`, so the receipt rolls up
//! **Provisional** — the bench signs it, nothing else does.
//!
//! Rev-b closes the loop: rev-a's three co-design vetoes (melting
//! cathode, unpriced coil repulsion, deterministically-violable ring gap)
//! come back as **priced resolutions** on the same receipt — a pulsed
//! duty-cycle operating point, an explicit mount-load requirement, and a
//! physics-justified wider gap requirement. The receipt drove the
//! revision; the revision is on the receipt.
//!
//! Rev-c re-prices the thermal leg with **radiation**: the grid radiates
//! to the chamber wall (gray two-surface enclosure, staircase-corrected
//! area) instead of shedding heat only down its stalk, which is how a
//! star-mode grid actually sheds most of its power. The conduction-only
//! number is kept on the console as the floor rev-a vetoed on.
//!
//! Run: `cargo run --release -p vcad-kernel-particle --example fusor_codesign`

use std::collections::BTreeMap;

use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::field::FieldMap;
use vcad_kernel_particle::fom::stats;
use vcad_kernel_particle::poisson::{solve, SolveOptions};
use vcad_kernel_particle::receipt as particle_receipt;
use vcad_kernel_particle::trace::{TraceOptions, Tracer, DEUTERON};
use vcad_receipt::{ClaimBasis, ClaimQuantity, DesignReceipt, OracleRef, ReceiptClaim};

// ── The experiment configuration (phase B of the experiment doc) ──────
const CHAMBER_R_MM: f64 = 150.0;
const RING_R_MM: f64 = 45.0;
const RING_Z_MM: f64 = 25.0;
const WIRE_A_MM: f64 = 3.0;
const CATHODE_V: f64 = -30_000.0;
const SHIELD_AT: f64 = 160_000.0; // ampere-turns per ring, opposed
const COIL_TURNS: f64 = 400.0; // realization: 400 t × 400 A pulsed
const ION_CURRENT_A: f64 = 0.010;
const PRESSURE_MTORR: f64 = 2.0;
const SHIELD_HDPE_MM: f64 = 100.0;
const OPERATOR_MM: f64 = 2_000.0;
// Rev-b operating envelope: pulsed commissioning duty (thermal resolution)
// and the physics-justified ring-gap requirement (M0 sweep: yield is
// gentle in ring spacing near the optimum — ±1.5 mm costs <5% yield).
const DUTY_CYCLE: f64 = 0.02;
const GAP_REQ_MM: (f64, f64) = (48.5, 51.5);
const COPPER_MELT_C: f64 = 1_085.0;
// Rev-c radiation: grid surface emissivity (lightly oxidized copper; the
// band is ~0.05 sputter-clean to ~0.6 oxidized), chamber wall
// (stainless, ~0.3) at room temperature.
const GRID_EMISSIVITY: f64 = 0.1;
const CLEAN_COPPER_EMISSIVITY: f64 = 0.05;
const WALL_EMISSIVITY: f64 = 0.3;
const CHAMBER_WALL_C: f64 = 25.0;

/// Fold a domain crate's `{name, value, unit, note}` claim into the
/// unified receipt (basis `predicted`; the crates' own adapters will
/// supersede this once MCP wave 2 lands them).
fn unified(
    domain: &str,
    oracle: &OracleRef,
    name: &str,
    value: f64,
    unit: &str,
    note: &str,
) -> ReceiptClaim {
    let q = if unit == "1" || unit.is_empty() {
        ClaimQuantity::bare(value)
    } else {
        ClaimQuantity::new(value, unit)
    };
    ReceiptClaim::pass(format!("{domain}.{name}"), domain, note, oracle.clone())
        .with_basis(ClaimBasis::Predicted)
        .with_measured(q)
}

fn main() {
    let mut all: Vec<ReceiptClaim> = Vec::new();

    // ── 1. particle: the machine itself ───────────────────────────────
    let device = Device::shielded_two_ring(
        CHAMBER_R_MM,
        RING_R_MM,
        RING_Z_MM,
        WIRE_A_MM,
        CATHODE_V,
        SHIELD_AT,
    );
    let sopts = SolveOptions::default();
    let sol = solve(&device, 121, 241, &sopts).expect("particle poisson");
    let fields = FieldMap::new(&device, &sol);
    let topts = TraceOptions {
        max_passes: 30,
        ..TraceOptions::default()
    };
    let tracer = Tracer::new(&device, &fields, &sol, topts);
    let pstats = stats(&tracer.launch_ensemble(DEUTERON, 64));
    let op = particle_receipt::OperatingPoint {
        ion_current_a: ION_CURRENT_A,
        d2_pressure_mtorr: PRESSURE_MTORR,
        temperature_k: 300.0,
    };
    let pset = particle_receipt::predicted_claims(
        &pstats,
        &sol,
        &topts,
        sopts.tol,
        device.max_potential_drop_v(),
        &op,
    );
    let neutron_rate = pset
        .claims
        .iter()
        .find(|c| c.name == "ddn_neutron_rate")
        .map(|c| c.value)
        .expect("neutron rate claim");
    all.extend(particle_receipt::design_claims(&pset));
    println!(
        "particle   ✓ interception {:.2}, {:.2e} n/s predicted",
        pstats.interception_fraction, neutron_rate
    );

    // ── 2. em: the shield coils as electromagnets ─────────────────────
    use vcad_kernel_em::axisym::{Annulus, AxisymMagnetostatics, Coil};
    let coil_region = |z0: f64| Annulus {
        r_inner_mm: RING_R_MM - WIRE_A_MM,
        r_outer_mm: RING_R_MM + WIRE_A_MM,
        z_min_mm: z0 - WIRE_A_MM,
        z_max_mm: z0 + WIRE_A_MM,
    };
    let mut em_dev = AxisymMagnetostatics::new(CHAMBER_R_MM, -CHAMBER_R_MM, CHAMBER_R_MM);
    em_dev.coils.push(Coil {
        region: coil_region(RING_Z_MM),
        turns: COIL_TURNS,
        current_a: SHIELD_AT / COIL_TURNS,
    });
    em_dev.coils.push(Coil {
        region: coil_region(-RING_Z_MM),
        turns: COIL_TURNS,
        current_a: -SHIELD_AT / COIL_TURNS,
    });
    let em_opts = vcad_kernel_em::grid::SolveOptions::default();
    let em_sol = em_dev.solve(129, 257, &em_opts).expect("em solve");
    let em_oracle = OracleRef::new("vcad-kernel-em/axisym", env!("CARGO_PKG_VERSION"));
    let l_set = vcad_kernel_em::receipt::axisym_inductance_claims(&em_sol, 0, em_opts.tol, None);
    let f_set = vcad_kernel_em::receipt::axisym_force_claims(
        &em_sol,
        0,
        em_opts.tol,
        Some((70.0, 5.0, 45.0, 96)),
    );
    let coil_force_n = f_set
        .claims
        .iter()
        .find(|c| c.name == "force_n")
        .map(|c| c.value)
        .unwrap_or(0.0);
    for set in [&l_set, &f_set] {
        for c in &set.claims {
            all.push(unified(
                "em", &em_oracle, &c.name, c.value, &c.unit, &c.note,
            ));
        }
    }
    // Rev-b resolution: the repulsion becomes an explicit, priced
    // requirement instead of an unexamined assumption.
    all.push(unified(
        "em",
        &em_oracle,
        "coil_mount_load_n",
        coil_force_n.abs(),
        "N",
        "axial repulsion between the opposed shield coils at full current;          the mount stack must be rated to >= 1.5x this value — a structural          member, not clips",
    ));
    println!(
        "em         ✓ per-coil L {:.2e} H, inter-coil force {:.1} kN (mount requirement on receipt)",
        l_set.claims[0].value,
        coil_force_n.abs() / 1e3
    );

    // ── 3. thermal: interception heating of the cathode rings ─────────
    use vcad_kernel_thermal::model::{
        Axis, Boundary, Enclosure, FixedTemperature, MaterialRegion, PowerSource, Shape,
        ThermalModel,
    };
    // The cross-domain wire: the ions particle says still hit the wires
    // arrive as heat. P = interception × beam power.
    let intercept_power_w = pstats.interception_fraction * ION_CURRENT_A * CATHODE_V.abs();
    let ring_tube = |z0: f64| Shape::Tube {
        axis: Axis::Z,
        center_mm: [60.0, 60.0],
        inner_radius_mm: RING_R_MM - WIRE_A_MM,
        outer_radius_mm: RING_R_MM + WIRE_A_MM,
        span_mm: [60.0 + z0 - WIRE_A_MM, 60.0 + z0 + WIRE_A_MM],
    };
    // One support post at the ring radius, spanning both rings up to the
    // cooled feedthrough flange — the only conduction path out.
    let stalk_z0 = 60.0 - RING_Z_MM - WIRE_A_MM;
    let stalk = Shape::Box {
        min_mm: [57.0, 100.0, stalk_z0],
        size_mm: [6.0, 6.0, 120.0 - stalk_z0],
    };
    // Rev-c: the grid sits in vacuum inside the chamber and *radiates*
    // to the wall (two-surface gray enclosure, F = 1) as well as
    // conducting down the stalk. Rev-a/b were conduction-only (a floor
    // that vetoed a cathode radiation saves).
    let chamber_area_m2 = 4.0 * std::f64::consts::PI * (CHAMBER_R_MM * 1e-3).powi(2);
    let thermal_model = |power_w: f64, emissivity: f64, area_scale: f64| {
        let mut tm = ThermalModel::new([0.0, 0.0, 0.0], [120.0, 120.0, 120.0], [96, 96, 96]);
        tm.materials
            .push(MaterialRegion::isotropic(ring_tube(RING_Z_MM), 390.0)); // copper
        tm.materials
            .push(MaterialRegion::isotropic(ring_tube(-RING_Z_MM), 390.0));
        tm.materials
            .push(MaterialRegion::isotropic(stalk.clone(), 16.0)); // stainless stalk
        tm.sources.push(PowerSource {
            name: "ion-interception-upper".into(),
            shape: ring_tube(RING_Z_MM),
            power_w: 0.5 * power_w,
        });
        tm.sources.push(PowerSource {
            name: "ion-interception-lower".into(),
            shape: ring_tube(-RING_Z_MM),
            power_w: 0.5 * power_w,
        });
        tm.fixed.push(FixedTemperature {
            shape: Shape::Box {
                min_mm: [54.0, 97.0, 114.0],
                size_mm: [12.0, 12.0, 6.0],
            },
            temperature_c: 25.0,
        });
        tm.exposed = Boundary::Radiation {
            emissivity,
            ambient_c: CHAMBER_WALL_C,
            h_w_m2k: 0.0, // vacuum
            area_scale,
            enclosure: Some(Enclosure {
                emissivity: WALL_EMISSIVITY,
                area_m2: chamber_area_m2,
            }),
        };
        tm.reference_c = Some(25.0);
        tm
    };
    // Staircase correction: the voxelized rings' curved walls over-count
    // area (×4/π), which would over-cool. True radiating area, analytic:
    // two rectangular-section rings plus the stalk's free length (minus
    // the two ring crossings and the pinned flange), ±5%.
    let (ri, ro, h) = (
        RING_R_MM - WIRE_A_MM,
        RING_R_MM + WIRE_A_MM,
        2.0 * WIRE_A_MM,
    );
    let ring_area_mm2 = 2.0 * std::f64::consts::PI * (ro * ro - ri * ri)
        + 2.0 * std::f64::consts::PI * h * (ro + ri);
    let stalk_free_mm = 114.0 - stalk_z0 - 2.0 * h;
    let true_area_m2 = (2.0 * ring_area_mm2 + 4.0 * 6.0 * stalk_free_mm + 36.0) * 1e-6;
    let voxel_area_m2 =
        vcad_kernel_thermal::solve::radiation_areas(&thermal_model(1.0, GRID_EMISSIVITY, 1.0))
            .expect("thermal geometry")[0]
            .1;
    let area_scale = true_area_m2 / voxel_area_m2;

    let th_opts = vcad_kernel_thermal::solve::SolveOptions::default();
    let solve_at = |power_w: f64, emissivity: f64| {
        vcad_kernel_thermal::solve::solve_steady(
            &thermal_model(power_w, emissivity, area_scale),
            &th_opts,
        )
        .expect("thermal")
    };
    let tm = thermal_model(intercept_power_w, GRID_EMISSIVITY, area_scale);
    let th_sol = solve_at(intercept_power_w, GRID_EMISSIVITY);
    let th_set = vcad_kernel_thermal::receipt::predicted_claims(&tm, &th_sol, &th_opts);
    let th_oracle = OracleRef::new("vcad-kernel-thermal/steady", env!("CARGO_PKG_VERSION"));
    for c in &th_set.claims {
        all.push(unified(
            "thermal", &th_oracle, &c.name, c.value, &c.unit, &c.note,
        ));
    }
    // Rev-a/b's conduction-only number, kept for the record: the same
    // geometry with the radiating surface switched off.
    let conduction_only_c = {
        let mut m = thermal_model(intercept_power_w, GRID_EMISSIVITY, area_scale);
        m.exposed = Boundary::Adiabatic;
        vcad_kernel_thermal::solve::solve_steady(&m, &th_opts)
            .expect("thermal (conduction only)")
            .t_max_c
    };
    // Emissivity is the dominant uncertainty: sputter-cleaned copper sits
    // near 0.05, oxidized near 0.6. Price the clean (hot) bound as its
    // own claim.
    let t_clean_c = solve_at(intercept_power_w, CLEAN_COPPER_EMISSIVITY).t_max_c;
    all.push(unified(
        "thermal",
        &th_oracle,
        "t_max_c_clean_copper",
        t_clean_c,
        "C",
        &format!(
            "T_max at eps = {CLEAN_COPPER_EMISSIVITY} (sputter-cleaned copper, the hot bound \
             of the emissivity band); same radiating model and area as t_max_c"
        ),
    ));
    // Pulsed operation, priced at average power. Radiation is nonlinear,
    // so this is a fresh solve, not a linear rescale (rev-b's shortcut no
    // longer holds); valid when the pulse period is short against the
    // ring's thermal time constant.
    let t_duty_c = solve_at(intercept_power_w * DUTY_CYCLE, GRID_EMISSIVITY).t_max_c;
    all.push(unified(
        "thermal",
        &th_oracle,
        "t_max_c_at_duty",
        t_duty_c,
        "C",
        &format!(
            "T_max at {:.0}% duty, solved at average power (radiating, nonlinear: no linear \
             rescale); melt margin {:.1}x on the rise",
            DUTY_CYCLE * 100.0,
            (COPPER_MELT_C - 25.0) / (t_duty_c - 25.0)
        ),
    ));
    let rad_frac = th_sol.energy.radiation_out_w / th_sol.energy.source_w;
    println!(
        "thermal    ✓ {:.0} W steady, radiating (eps {GRID_EMISSIVITY}, area x{area_scale:.3}) \
         → T_max {:.0} °C ({:.0}% radiated; conduction-only floor said {:.0} °C); \
         clean-copper bound {:.0} °C; at {:.0}% duty → {:.0} °C",
        intercept_power_w,
        th_sol.t_max_c,
        rad_frac * 100.0,
        conduction_only_c,
        t_clean_c,
        DUTY_CYCLE * 100.0,
        t_duty_c
    );
    println!("thermal    emissivity sweep (steady, full power):");
    for eps in [0.05, 0.1, 0.2, 0.3, 0.6] {
        let s = solve_at(intercept_power_w, eps);
        println!(
            "             eps {eps:>4}: T_max {:>6.0} °C, radiated {:>4.0}%, Newton {}",
            s.t_max_c,
            100.0 * s.energy.radiation_out_w / s.energy.source_w,
            s.newton_iterations
        );
    }

    // ── 4. neutronics: the operator's dose behind the shield ──────────
    use vcad_kernel_neutronics::spec::{
        DetectorSpec, LayerSpec, ParamValue, RunSpec, ShieldSpec, SourceSpec,
    };
    // Cross-domain wire #2: the source term is particle's predicted rate.
    let shield = ShieldSpec {
        layers: vec![
            LayerSpec {
                material: "air".into(),
                thickness_mm: ParamValue::Literal(1_000.0),
            },
            LayerSpec {
                material: "hdpe".into(),
                thickness_mm: ParamValue::Literal(SHIELD_HDPE_MM),
            },
            LayerSpec {
                material: "air".into(),
                thickness_mm: ParamValue::Literal(OPERATOR_MM - 1_000.0 - SHIELD_HDPE_MM + 200.0),
            },
        ],
        source: SourceSpec {
            rate_n_per_s: ParamValue::Literal(neutron_rate),
            energy_ev: ParamValue::Literal(2.45e6),
        },
        detectors: vec![DetectorSpec {
            label: "operator".into(),
            radius_mm: ParamValue::Literal(OPERATOR_MM),
            half_width_mm: ParamValue::Literal(20.0),
        }],
        run: RunSpec::default(),
    };
    let n_set = vcad_kernel_neutronics::receipt::predicted_claims(&shield, &BTreeMap::new())
        .expect("neutronics");
    let n_oracle = OracleRef::new("vcad-kernel-neutronics/mc", env!("CARGO_PKG_VERSION"));
    for c in &n_set.claims {
        let note = format!("{} (rse {:.1}%)", c.note, c.rse * 100.0);
        all.push(unified(
            "neutronics",
            &n_oracle,
            &c.name,
            c.value,
            &c.unit,
            &note,
        ));
    }
    if let Some(d) = n_set
        .claims
        .iter()
        .find(|c| c.name.starts_with("dose_rate"))
    {
        println!(
            "neutronics ✓ operator dose {:.3} µSv/h ± {:.0}% behind {} mm HDPE",
            d.value,
            d.rse * 100.0,
            SHIELD_HDPE_MM
        );
    }

    // ── 5. tolerance: does the cusp survive real parts? ───────────────
    use vcad_kernel_tolerance::analysis::{monte_carlo, rss, worst_case, McOptions};
    use vcad_kernel_tolerance::dist::SigmaConvention;
    use vcad_kernel_tolerance::stackup::{Contributor, Requirement, Stackup};
    // Ring-to-ring gap chain, symmetric mounts: flange datum → feedthrough
    // → mount → ring seat, both sides. Cusp physics wants the 50 mm gap
    // held to ±1 mm (the M0 sweep's ring-spacing sensitivity).
    let side = |tag: &str, sign: f64| {
        vec![
            Contributor::normal(
                &format!("feedthrough-length-{tag}"),
                sign,
                60.0,
                0.30,
                SigmaConvention::ThreeSigma,
            ),
            Contributor::normal(
                &format!("mount-spacer-{tag}"),
                -sign,
                47.5,
                0.20,
                SigmaConvention::ThreeSigma,
            ),
            Contributor::normal(
                &format!("ring-seat-{tag}"),
                sign,
                12.5,
                0.15,
                SigmaConvention::ThreeSigma,
            ),
        ]
    };
    let mut contributors = side("upper", 1.0);
    contributors.extend(side("lower", 1.0));
    // Rev-a asked for ±1.0 mm and was deterministically violable; rev-b
    // widens to the physics-justified requirement and re-analyzes both.
    let stack_rev_a = Stackup {
        name: "cusp-ring-gap-rev-a".into(),
        contributors: contributors.clone(),
        requirement: Requirement::between("ring-gap", 49.0, 51.0),
    };
    let wc_rev_a = worst_case(&stack_rev_a).expect("wc rev-a");
    let stack = Stackup {
        name: "cusp-ring-gap".into(),
        contributors,
        requirement: Requirement::between("ring-gap", GAP_REQ_MM.0, GAP_REQ_MM.1),
    };
    let wc = worst_case(&stack).expect("wc");
    let rs = rss(&stack).expect("rss");
    let mc = monte_carlo(&stack, &McOptions::default()).expect("mc");
    let t_set =
        vcad_kernel_tolerance::receipt::predicted_claims(&stack, &wc, &rs, &mc).expect("claims");
    let t_oracle = OracleRef::new("vcad-kernel-tolerance/stackup", env!("CARGO_PKG_VERSION"));
    for c in &t_set.claims {
        let note = match c.uncertainty {
            Some(u) => format!("{} (± {u:.2e})", c.note),
            None => c.note.clone(),
        };
        all.push(unified(
            "tolerance",
            &t_oracle,
            &c.name,
            c.value,
            &c.unit,
            &note,
        ));
    }
    println!(
        "tolerance  ✓ gap {}–{} mm: RSS yield {:.4}, WC margin {:.2} mm (rev-a ±1.0: {:.2} mm)",
        GAP_REQ_MM.0,
        GAP_REQ_MM.1,
        rs.yield_estimate,
        wc.worst_margin(),
        wc_rev_a.worst_margin()
    );

    // ── The receipt ───────────────────────────────────────────────────
    let mut receipt = DesignReceipt::with_claims(all);
    receipt.document_id = Some("shielded-grid-experiment/rev-c".into());
    let domains: std::collections::BTreeSet<&str> =
        receipt.claims.iter().map(|c| c.domain.as_str()).collect();
    println!(
        "\n════ DesignReceipt: {} claims across {} domains ({}) ════",
        receipt.claims.len(),
        domains.len(),
        domains.into_iter().collect::<Vec<_>>().join(", ")
    );
    println!(
        "verdict: {:?} — every number is a prediction; the bench signs the receipt.",
        receipt.verdict()
    );

    // Co-design findings — the contradictions only a multi-domain receipt
    // can surface (each is invisible from inside its own domain):
    println!("\n──── co-design loop: rev-a vetoes → rev-b/c resolutions ────");
    println!(
        "· thermal:    conduction-only floor {:.0} °C (rev-a veto)  →  rev-c radiating \
         steady {:.0} °C (clean-copper bound {:.0} °C, melt {:.0} °C)  →  {:.0}% duty \
         {:.0} °C ({:.1}× melt margin)",
        conduction_only_c,
        th_sol.t_max_c,
        t_clean_c,
        COPPER_MELT_C,
        DUTY_CYCLE * 100.0,
        t_duty_c,
        (COPPER_MELT_C - 25.0) / (t_duty_c - 25.0)
    );
    println!(
        "· mechanical: {:.1} kN coil repulsion  →  explicit mount-load requirement \
         claim (rate the stack ≥ 1.5×)",
        coil_force_n.abs() / 1e3
    );
    println!(
        "· tolerance:  gap ±1.0 mm WC margin {:.2} mm  →  requirement {}–{} mm \
         (M0: <5% yield cost)  →  WC margin {:+.2} mm",
        wc_rev_a.worst_margin(),
        GAP_REQ_MM.0,
        GAP_REQ_MM.1,
        wc.worst_margin()
    );
    println!(
        "\nrev-c verdict stays {:?}: resolutions are still predictions — the bench \
         signs the receipt.",
        receipt.verdict()
    );
    println!(
        "\n{}",
        serde_json::to_string_pretty(&receipt).expect("json")
    );
}
