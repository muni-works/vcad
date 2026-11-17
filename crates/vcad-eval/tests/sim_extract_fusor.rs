//! CAD → simulation, one document: a loon-authored hobby fusor (6" chamber,
//! 4-ring 1.5" grid, 15 cm HDPE shell) extracted to the particle crate's
//! `DeviceSpec` and the neutronics crate's `ShieldSpec`, round-tripped
//! against hand-written references, and run through both solvers. Then the
//! fail-closed cases: every unrepresentable shape errors by part name.

use std::collections::BTreeMap;

use vcad_eval::sim_extract::{
    extract_device_spec, extract_shield_spec, wire_features, DeviceExtraction, ElectrodeSelection,
    ExtractedDevice, ShieldExtraction, ShieldLayerSelection, SimExtractError, WireFeature,
};
use vcad_ir::{Document, Expr};
use vcad_kernel_neutronics::spec::{
    self as nspec, DetectorSpec, LayerSpec, ParamValue as NParam, RunSpec, ShieldSpec, SourceSpec,
};
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::spec::DeviceSpec;

/// 6" chamber: 76.2 mm cavity radius and half-height, 3 mm wall.
const CHAMBER_R: f64 = 76.2;
/// 1.5" grid diameter.
const GRID_R: f64 = 19.05;
const WIRE_R: f64 = 1.0;
const CATHODE_V: f64 = -30_000.0;
const N_RINGS: usize = 4;

/// The grid as latitude rings on a sphere of radius `grid_r`, parametric in
/// the `grid_r` / `wire_r` document parameters (same placement rule as
/// `Device::classic_fusor`).
fn grid_rings() -> Vec<String> {
    (0..N_RINGS)
        .map(|k| {
            let th = std::f64::consts::PI * (k as f64 + 1.0) / (N_RINGS as f64 + 1.0);
            format!(
                "[translate 0.0 0.0 [* grid_r {c:.17}] [torus [* grid_r {s:.17}] wire_r]]",
                c = th.cos(),
                s = th.sin()
            )
        })
        .collect()
}

fn chamber_source() -> String {
    let h = 2.0 * CHAMBER_R;
    let wall = 3.0;
    // loon's [difference tool body] = body − tool.
    format!(
        "[root-named [translate 0.0 0.0 {z0} [difference [translate 0.0 0.0 {wall} \
         [cylinder {CHAMBER_R} {h}]] [cylinder {ro} {ho}]]] \"steel\" \"chamber\"]\n",
        z0 = -(CHAMBER_R + wall),
        ro = CHAMBER_R + wall,
        ho = h + 2.0 * wall,
    )
}

fn fusor_source(extra: &str) -> String {
    format!(
        "[defparam cathode_v {CATHODE_V:.1}]\n\
         [defparam src_rate 25000.0]\n\
         [defparam grid_r {GRID_R}]\n\
         [defparam wire_r {WIRE_R}]\n\
         {chamber}\
         [root-named [union {rings}] \"tungsten\" \"grid\"]\n\
         [root-named [difference [sphere 150.0] [sphere 300.0]] \"hdpe\" \"shield\"]\n\
         {extra}",
        chamber = chamber_source(),
        rings = grid_rings().join(" "),
    )
}

fn doc(extra: &str) -> Document {
    vcad_loon::eval_vcad(&fusor_source(extra), None).expect("loon fixture evaluates")
}

fn device_sel(electrodes: &[&str]) -> DeviceExtraction {
    DeviceExtraction {
        chamber: "chamber".to_string(),
        wall_potential: None,
        electrodes: electrodes
            .iter()
            .map(|p| ElectrodeSelection {
                part: p.to_string(),
                potential: "cathode_v".to_string(),
                ampere_turns: None,
            })
            .collect(),
        tolerance: Default::default(),
    }
}

fn shield_sel(shells: &[&str]) -> ShieldExtraction {
    ShieldExtraction {
        shells: shells
            .iter()
            .map(|p| ShieldLayerSelection {
                part: p.to_string(),
                material: None,
            })
            .collect(),
        fill_material: "air".to_string(),
        source: SourceSpec {
            rate_n_per_s: NParam::Named("src_rate".to_string()),
            energy_ev: NParam::Literal(2.45e6),
        },
        detectors: vec![
            DetectorSpec {
                label: "near".to_string(),
                radius_mm: NParam::Literal(1000.0),
                half_width_mm: NParam::Literal(20.0),
            },
            DetectorSpec {
                label: "operator".to_string(),
                radius_mm: NParam::Literal(2000.0),
                half_width_mm: NParam::Literal(20.0),
            },
        ],
        world_radius_mm: Some(2400.0),
        center: None,
        run: RunSpec {
            histories_per_batch: 1000,
            batches: 4,
            seed: 11,
        },
        tolerance: Default::default(),
    }
}

fn resolved(ex: &ExtractedDevice) -> Device {
    ex.spec
        .resolve(&ex.parameters)
        .expect("extracted spec resolves")
}

fn assert_close(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() <= 1e-9 * (1.0 + b.abs()),
        "{what}: {a} vs {b}"
    );
}

fn assert_devices_close(got: &Device, want: &Device) {
    assert_close(got.chamber_radius_mm, want.chamber_radius_mm, "chamber r");
    assert_close(
        got.chamber_half_height_mm,
        want.chamber_half_height_mm,
        "chamber half-height",
    );
    assert_close(got.wall_potential_v, want.wall_potential_v, "wall V");
    assert_eq!(got.rings.len(), want.rings.len(), "ring count");
    let mut w = want.rings.clone();
    w.sort_by(|a, b| a.z_mm.total_cmp(&b.z_mm));
    for (i, (g, w)) in got.rings.iter().zip(&w).enumerate() {
        assert_close(g.ring_radius_mm, w.ring_radius_mm, &format!("ring {i} R"));
        assert_close(g.z_mm, w.z_mm, &format!("ring {i} z"));
        assert_close(g.wire_radius_mm, w.wire_radius_mm, &format!("ring {i} r"));
        assert_close(g.potential_v, w.potential_v, &format!("ring {i} V"));
        assert_close(g.ampere_turns, w.ampere_turns, &format!("ring {i} AT"));
    }
}

fn set_param(doc: &mut Document, name: &str, v: f64) {
    doc.parameters.get_mut(name).expect("param").value = Expr::Number(v);
}

#[test]
fn fusor_document_round_trips_to_device_spec_and_simulates() {
    let d = doc("");
    let ex = extract_device_spec(&d, &device_sel(&["grid"])).expect("extract");

    // Round trip: the document's solids == the reference device.
    let reference = Device::classic_fusor(CHAMBER_R, GRID_R, N_RINGS, WIRE_R, CATHODE_V);
    assert_devices_close(&resolved(&ex), &reference);
    assert_eq!(ex.ring_parts, vec!["grid"; N_RINGS]);
    assert_close(ex.cavity.origin[2], 0.0, "device origin z");
    assert_eq!(ex.cavity.axis, [0.0, 0.0, 1.0]);

    // The potential stays a named document parameter (adjoint-capable)…
    let roles = ex.spec.parameter_roles();
    assert_eq!(
        roles.get("cathode_v"),
        Some(&vcad_kernel_particle::spec::ParamRole::Adjoint)
    );
    // …and the spec survives JSON (the MCP wire format).
    let json = serde_json::to_string(&ex.spec).unwrap();
    let back: DeviceSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ex.spec);

    // Through the solver: field solve + a small deuteron ensemble.
    let device = resolved(&ex);
    let sol = vcad_kernel_particle::poisson::solve(
        &device,
        41,
        81,
        &vcad_kernel_particle::poisson::SolveOptions::default(),
    )
    .expect("poisson solve");
    assert!(
        sol.potential_at(0.0, 0.0) < -1_000.0,
        "well depth at the centre: {} V",
        sol.potential_at(0.0, 0.0)
    );
    let fields = vcad_kernel_particle::field::FieldMap::new(&device, &sol);
    let topts = vcad_kernel_particle::trace::TraceOptions {
        max_passes: 4,
        ..Default::default()
    };
    let tracer = vcad_kernel_particle::trace::Tracer::new(&device, &fields, &sol, topts);
    let stats = vcad_kernel_particle::fom::stats(
        &tracer.launch_ensemble(vcad_kernel_particle::trace::DEUTERON, 8),
    );
    assert_eq!(stats.n, 8);
    assert!(stats.mean_passes.is_finite() && stats.mean_passes > 0.0);
    let total = stats.interception_fraction + stats.wall_fraction + stats.survivor_fraction;
    assert!((total - 1.0).abs() < 1e-9, "fates sum to {total}");
}

#[test]
fn scrubbing_document_parameters_reprices_the_device() {
    let mut d = doc("");
    // Electrical parameter: flows through the named binding.
    set_param(&mut d, "cathode_v", -40_000.0);
    // Geometric parameter: re-evaluates the solids.
    set_param(&mut d, "grid_r", 25.4);
    let ex = extract_device_spec(&d, &device_sel(&["grid"])).expect("extract");
    let reference = Device::classic_fusor(CHAMBER_R, 25.4, N_RINGS, WIRE_R, -40_000.0);
    assert_devices_close(&resolved(&ex), &reference);
}

#[test]
fn opposed_current_rings_bind_per_part() {
    // Shielded two-ring cathode: separate parts, opposed ampere-turns via a
    // derived document parameter.
    let src = format!(
        "[defparam cathode_v -30000.0]\n[defparam shield_at 40000.0]\n\
         [defparam shield_at_neg \"0 - shield_at\"]\n{chamber}\
         [root-named [translate 0.0 0.0 25.0 [torus 45.0 3.0]] \"w\" \"upper\"]\n\
         [root-named [translate 0.0 0.0 -25.0 [torus 45.0 3.0]] \"w\" \"lower\"]\n",
        chamber = chamber_source()
    );
    let d = vcad_loon::eval_vcad(&src, None).unwrap();
    let mut sel = device_sel(&["upper", "lower"]);
    sel.electrodes[0].ampere_turns = Some("shield_at".into());
    sel.electrodes[1].ampere_turns = Some("shield_at_neg".into());
    let ex = extract_device_spec(&d, &sel).unwrap();
    let reference = Device::shielded_two_ring(CHAMBER_R, 45.0, 25.0, 3.0, -30_000.0, 40_000.0);
    assert_devices_close(&resolved(&ex), &reference);
    assert_eq!(ex.ring_parts, vec!["lower", "upper"]);
}

#[test]
fn fusor_document_round_trips_to_shield_spec_and_transports() {
    let d = doc("");
    let dev = extract_device_spec(&d, &device_sel(&["grid"])).unwrap();
    let mut sel = shield_sel(&["shield"]);
    // The source sits at the device centre; the shell must be concentric.
    sel.center = Some(dev.cavity.origin);
    let ex = extract_shield_spec(&d, &sel).expect("extract shield");
    assert_eq!(
        ex.layer_parts,
        vec![None, Some("shield".to_string()), None],
        "air core | HDPE shell | air world"
    );

    // Round trip: identical to the hand-written spec, field for field.
    let hand = ShieldSpec {
        layers: vec![
            LayerSpec {
                material: "air".into(),
                thickness_mm: NParam::Literal(150.0),
            },
            LayerSpec {
                material: "hdpe".into(),
                thickness_mm: NParam::Literal(150.0),
            },
            LayerSpec {
                material: "air".into(),
                thickness_mm: NParam::Literal(2100.0),
            },
        ],
        source: sel.source.clone(),
        detectors: sel.detectors.clone(),
        run: sel.run.clone(),
    };
    assert_eq!(ex.spec.layers.len(), 3);
    for (g, w) in ex.spec.layers.iter().zip(&hand.layers) {
        assert_eq!(g.material, w.material);
        let (NParam::Literal(gt), NParam::Literal(wt)) = (&g.thickness_mm, &w.thickness_mm) else {
            panic!("thicknesses are literals")
        };
        assert_close(*gt, *wt, "layer thickness");
    }

    // Through the transport: same seed, same doses as the hand spec.
    let params: BTreeMap<String, f64> = ex.parameters.clone();
    assert_eq!(params["src_rate"], 25_000.0);
    let (got, run) = nspec::evaluate(&ex.spec, &params).expect("transport");
    let (want, _) = nspec::evaluate(&hand, &params).expect("hand transport");
    assert_eq!(run.truncated_histories, 0);
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.label, w.label);
        assert!(g.dose_usv_per_h.mean > 0.0);
        assert_close(g.dose_usv_per_h.mean, w.dose_usv_per_h.mean, &g.label);
    }
}

// ---------------------------------------------------------------------------
// Fail-closed
// ---------------------------------------------------------------------------

fn part_of(e: &SimExtractError) -> Option<&str> {
    match e {
        SimExtractError::NotRepresentable { part, .. }
        | SimExtractError::NonCoaxialRing { part, .. }
        | SimExtractError::RingOutsideChamber { part, .. }
        | SimExtractError::NonConcentric { part, .. } => Some(part),
        SimExtractError::UnknownPart { name, .. } => Some(name),
        _ => None,
    }
}

#[test]
fn tilted_ring_is_rejected_by_name() {
    // A meridian ring (globe grid) — fine as a WireFeature, not coaxial.
    let d = doc("[root-named [rotate 90.0 0.0 0.0 [torus 19.05 1.0]] \"w\" \"meridian\"]\n");
    let err = extract_device_spec(&d, &device_sel(&["grid", "meridian"])).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NonCoaxialRing { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("meridian"));
    assert!(err.to_string().contains("3D electrode spec"), "{err}");
}

#[test]
fn off_axis_ring_is_rejected_by_name() {
    let d = doc("[root-named [translate 5.0 0.0 0.0 [torus 10.0 1.0]] \"w\" \"shifted\"]\n");
    let err = extract_device_spec(&d, &device_sel(&["shifted"])).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NonCoaxialRing { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("shifted"));
}

#[test]
fn ring_with_stalk_is_rejected_by_name() {
    let d = doc(
        "[root-named [union [torus 19.05 1.0] [translate 19.05 0.0 0.0 [cylinder 1.5 60.0]]] \
         \"w\" \"grid_with_stalk\"]\n",
    );
    let err = extract_device_spec(&d, &device_sel(&["grid_with_stalk"])).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NotRepresentable { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("grid_with_stalk"));
    assert!(err.to_string().contains("only toroidal"), "{err}");
}

/// Evaluate `src` and hand back the BRep of the part named `name`.
fn brep_of(d: &Document, name: &str) -> vcad_kernel::vcad_kernel_primitives::BRepSolid {
    let opts = vcad_eval::EvalOptions {
        skip_clash_detection: true,
        clock: None,
        root_cache: None,
        mesh_segments: 0,
        on_root: None,
        on_root_ready: None,
    };
    let scene = vcad_eval::evaluate_document(d, &opts).unwrap();
    let idx = d
        .roots
        .iter()
        .position(|r| d.nodes[&r.root].name.as_deref() == Some(name))
        .unwrap();
    scene.parts[idx]
        .solid
        .as_ref()
        .and_then(|s| s.as_brep())
        .cloned()
        .expect("brep")
}

#[test]
fn wire_feature_layer_is_orientation_agnostic_but_rejects_trimmed_rings() {
    let d = doc(
        "[root-named [rotate 90.0 0.0 0.0 [torus 19.05 1.0]] \"w\" \"meridian\"]\n\
         [root-named [union [torus 19.05 1.0] [rotate 90.0 0.0 0.0 [torus 19.05 1.0]]] \
         \"w\" \"crossed\"]\n",
    );
    let tol = Default::default();
    // A lone meridian ring is a valid feature (the 3D spec's input)…
    let rings = wire_features("meridian", &brep_of(&d, "meridian"), &tol).unwrap();
    assert_eq!(rings.len(), 1);
    let WireFeature::Ring {
        axis,
        ring_radius_mm,
        wire_radius_mm,
        ..
    } = &rings[0];
    assert!((axis[1].abs() - 1.0).abs() < 1e-12, "axis {axis:?}");
    assert_close(*ring_radius_mm, 19.05, "R");
    assert_close(*wire_radius_mm, 1.0, "r");
    // …but two crossing rings welded into one part are not two separate
    // complete rings — refuse rather than measure them as whole. (The kernel
    // may hand back both whole tori untrimmed, so this is the geometric
    // clearance check, not the adjacency one.)
    let err = wire_features("crossed", &brep_of(&d, "crossed"), &tol).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NotRepresentable { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("crossed"));
    assert!(err.to_string().contains("intersect"), "{err}");
}

#[test]
fn colliding_rings_across_parts_are_rejected() {
    // Same plane, centrelines 1.5 mm apart, 1 mm wires: 0.5 mm overlap. (In
    // separate parts: a loon union of these two tori currently evaluates to
    // the outer torus alone — a kernel boolean bug the extractor can't see
    // past — so the in-part case is covered by the crossing-ring test.)
    let d = doc("[root-named [torus 30.0 1.0] \"w\" \"inner\"]\n\
         [root-named [torus 31.5 1.0] \"w\" \"outer\"]\n");
    let err = extract_device_spec(&d, &device_sel(&["inner", "outer"])).unwrap_err();
    assert!(
        matches!(&err, SimExtractError::RingsOverlap { a, b, .. } if a == "inner" && b == "outer"),
        "{err}"
    );
}

#[test]
fn ring_outside_cavity_is_rejected() {
    let d = doc("[root-named [torus 75.0 2.0] \"w\" \"too_big\"]\n");
    let err = extract_device_spec(&d, &device_sel(&["too_big"])).unwrap_err();
    assert!(
        matches!(err, SimExtractError::RingOutsideChamber { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("too_big"));
}

#[test]
fn spherical_chamber_is_rejected() {
    let d =
        doc("[root-named [difference [sphere 76.2] [sphere 80.0]] \"steel\" \"sph_chamber\"]\n");
    let mut sel = device_sel(&["grid"]);
    sel.chamber = "sph_chamber".into();
    let err = extract_device_spec(&d, &sel).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NotRepresentable { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("sph_chamber"));
}

#[test]
fn unknown_names_fail_closed() {
    let d = doc("");
    let mut sel = device_sel(&["grid"]);
    sel.electrodes[0].potential = "anode_v".into();
    let err = extract_device_spec(&d, &sel).unwrap_err();
    assert!(
        matches!(&err, SimExtractError::UnknownParameter { name, .. } if name == "anode_v"),
        "{err}"
    );

    let err = extract_device_spec(&d, &device_sel(&["gridd"])).unwrap_err();
    assert!(matches!(err, SimExtractError::UnknownPart { .. }), "{err}");
    assert!(
        err.to_string().contains("grid"),
        "lists available parts: {err}"
    );

    let mut s = shield_sel(&["shield"]);
    s.source.rate_n_per_s = NParam::Named("nope".into());
    let err = extract_shield_spec(&d, &s).unwrap_err();
    assert!(
        matches!(&err, SimExtractError::UnknownParameter { name, .. } if name == "nope"),
        "{err}"
    );
}

#[test]
fn off_centre_shell_is_rejected_by_name() {
    let d = doc(
        "[root-named [translate 0.0 0.0 10.0 [difference [sphere 400.0] [sphere 450.0]]] \
         \"hdpe\" \"lopsided\"]\n",
    );
    let err = extract_shield_spec(&d, &shield_sel(&["shield", "lopsided"])).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NonConcentric { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("lopsided"));
}

#[test]
fn cylindrical_shell_is_rejected_by_name() {
    let d = doc(
        "[root-named [translate 0.0 0.0 -300.0 [difference [cylinder 400.0 600.0] \
         [cylinder 450.0 600.0]]] \"hdpe\" \"barrel\"]\n",
    );
    let err = extract_shield_spec(&d, &shield_sel(&["shield", "barrel"])).unwrap_err();
    assert!(
        matches!(err, SimExtractError::NotRepresentable { .. }),
        "{err}"
    );
    assert_eq!(part_of(&err), Some("barrel"));
}

#[test]
fn overlapping_shells_and_unknown_materials_are_rejected() {
    let d = doc(
        "[root-named [difference [sphere 250.0] [sphere 350.0]] \"hdpe\" \"overlap\"]\n\
         [root-named [difference [sphere 400.0] [sphere 450.0]] \"titanium\" \"ti_shell\"]\n",
    );
    let err = extract_shield_spec(&d, &shield_sel(&["shield", "overlap"])).unwrap_err();
    assert!(matches!(err, SimExtractError::ShellOverlap { .. }), "{err}");
    let err = extract_shield_spec(&d, &shield_sel(&["shield", "ti_shell"])).unwrap_err();
    assert!(
        matches!(&err, SimExtractError::UnknownMaterial { material, .. } if material == "titanium"),
        "{err}"
    );
    // An explicit library material overrides the document's render material.
    let mut s = shield_sel(&["shield", "ti_shell"]);
    s.shells[1].material = Some("lead".into());
    let ex = extract_shield_spec(&d, &s).expect("override");
    assert!(ex.spec.layers.iter().any(|l| l.material == "lead"));
}
