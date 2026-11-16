//! One JSON tool layer over vcad's physics solver crates, shared by the
//! native MCP server (`vcad-mcp`, headless and inside vcad.app) and the
//! app's Analysis panel (`vcad-ffi`). The wasm bindings in
//! `vcad-kernel-wasm` predate this crate; outputs here are a strict
//! superset of their keys so a client of the node server sees no change.
//!
//! | tool | crate | what |
//! |---|---|---|
//! | `simulate_charged_particles` | `vcad-kernel-particle` | fields + deuteron ensemble, FoMs, predicted claims, optional section (equipotentials, tracks) and voltage sweep |
//! | `optimize_electrodes` | `vcad-kernel-particle` | multi-start FD ascent on named spec parameters, every evaluation logged |
//! | `simulate_neutron_shield` | `vcad-kernel-neutronics` | MC dose at detector shells, optional bare-source twin and dose budget |
//! | `compare_measurements` | both | bind bench readings to a returned `claim_set` → Holds / Violated / Unmeasured + receipt claims |
//!
//! Every handler is pure (JSON in, JSON out), deterministic, and reports
//! progress / honours cancellation through [`Hooks`].

#![warn(missing_docs)]

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

pub mod compare;
pub mod neutronics;
pub mod particle;
pub mod section;

/// Progress + cancellation for a running tool.
pub struct Hooks<'a> {
    /// `(fraction in [0,1], stage message)`.
    pub progress: &'a (dyn Fn(f64, &str) + Sync),
    /// Cooperative cancel flag; handlers stop at the next particle /
    /// evaluation and return [`ToolError::Cancelled`].
    pub cancel: &'a AtomicBool,
}

impl Hooks<'_> {
    pub(crate) fn report(&self, fraction: f64, msg: &str) {
        (self.progress)(fraction.clamp(0.0, 1.0), msg);
    }
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Runs `f` with no-op hooks (tests, one-shot callers).
pub fn with_quiet_hooks<T>(f: impl FnOnce(&Hooks) -> T) -> T {
    let cancel = AtomicBool::new(false);
    let progress = |_: f64, _: &str| {};
    f(&Hooks {
        progress: &progress,
        cancel: &cancel,
    })
}

/// Why a tool did not produce a result.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolError {
    /// Bad or inconsistent arguments (spec parse, unbound parameter, …).
    Invalid(String),
    /// The solver refused (did not converge, truncated histories, …).
    Solver(String),
    /// Cancelled through [`Hooks::cancel`].
    Cancelled,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Invalid(m) => write!(f, "invalid arguments: {m}"),
            ToolError::Solver(m) => write!(f, "{m}"),
            ToolError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

/// Tool names this crate serves.
pub const TOOL_NAMES: &[&str] = &[
    "simulate_charged_particles",
    "optimize_electrodes",
    "simulate_neutron_shield",
    "compare_measurements",
];

/// The tools that run long enough to earn an MCP Task handle.
pub const LONG_RUNNING: &[&str] = &[
    "simulate_charged_particles",
    "optimize_electrodes",
    "simulate_neutron_shield",
];

/// Run one tool by name.
pub fn run(name: &str, args: &Value, hooks: &Hooks) -> Result<Value, ToolError> {
    match name {
        "simulate_charged_particles" => particle::simulate(args, hooks),
        "optimize_electrodes" => particle::optimize(args, hooks),
        "simulate_neutron_shield" => neutronics::simulate(args, hooks),
        "compare_measurements" => compare::compare(args),
        _ => Err(ToolError::Invalid(format!("unknown solver tool {name}"))),
    }
}

/// Drop the bulky section / per-evaluation arrays from a result before it
/// goes into an agent's context, keeping counts so the agent knows they
/// exist. vcad.app keeps the full result for the panel.
pub fn compact_for_agent(name: &str, mut v: Value) -> Value {
    if let Some(sec) = v.get_mut("section").and_then(Value::as_object_mut) {
        let tracks = sec
            .get("tracks")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let contours = sec
            .get("equipotentials")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        sec.remove("potential");
        sec.remove("tracks");
        sec.remove("equipotentials");
        sec.insert(
            "omitted".into(),
            json!(format!(
                "{tracks} tracks, {contours} equipotential levels and the potential grid are drawn in vcad.app's Analysis → Fields panel, not returned to the agent"
            )),
        );
    }
    if name == "optimize_electrodes" {
        if let Some(evals) = v.get("evaluations").and_then(Value::as_array) {
            let n = evals.len();
            if n > 12 {
                let tail: Vec<Value> = evals[n - 12..].to_vec();
                v["evaluations"] = Value::Array(tail);
                v["evaluations_note"] = json!(format!("last 12 of {n} evaluations"));
            }
        }
    }
    v
}

fn param_value_schema() -> Value {
    json!({
        "description": "A number, or the name of a named parameter bound via `parameters`.",
        "anyOf": [{"type": "number"}, {"type": "string"}]
    })
}

fn device_spec_schema() -> Value {
    let pv = param_value_schema();
    json!({
        "type": "object",
        "description": "Axisymmetric device: grounded cylindrical chamber + biased (optionally current-carrying) wire rings, coaxial with Z. Units mm / volts / ampere-turns. Any numeric field may instead be a string naming a parameter supplied in `parameters` (fail-closed: unbound names error). Shorthand: {\"classic_fusor\": {chamber_radius_mm, cathode_radius_mm, rings, wire_radius_mm, cathode_v}} — a spherical cathode as `rings` latitude rings, chamber half height = radius; its fields may name parameters, so `cathode_radius_mm: \"grid_r\"` lets optimize_electrodes move the whole grid.",
        "properties": {
            "classic_fusor": {"type": "object", "properties": {
                "chamber_radius_mm": pv, "cathode_radius_mm": pv, "rings": {"type": "integer"},
                "wire_radius_mm": pv, "cathode_v": pv
            }},
            "chamber_radius_mm": pv, "chamber_half_height_mm": pv,
            "wall_potential_v": pv,
            "rings": {"type": "array", "minItems": 1, "items": {
                "type": "object",
                "required": ["ring_radius_mm", "z_mm", "wire_radius_mm", "potential_v"],
                "properties": {
                    "ring_radius_mm": pv, "z_mm": pv, "wire_radius_mm": pv, "potential_v": pv,
                    "ampere_turns": pv
                }
            }}
        }
    })
}

fn params_schema() -> Value {
    json!({"type": "object", "additionalProperties": {"type": "number"},
           "description": "Bindings for named spec parameters, `{name: value}`."})
}

fn op_point_props() -> Value {
    json!({
        "ion_current_a": {"type": "number", "description": "Operating-point ion current, A. Default 0.010."},
        "d2_pressure_mtorr": {"type": "number", "description": "Operating-point D₂ pressure, mTorr. Default 2.0."},
        "temperature_k": {"type": "number", "description": "Gas temperature, K. Default 300."}
    })
}

/// `tools/list` descriptors: `{name, description, inputSchema}`.
pub fn descriptors() -> Vec<Value> {
    let mut sim_opts = json!({
        "nr": {"type": "number", "description": "Radial grid nodes. Default 101."},
        "nz": {"type": "number", "description": "Axial grid nodes. Default 201."},
        "particles": {"type": "number", "description": "Deuteron ensemble size. Default 64."},
        "max_passes": {"type": "number", "description": "Core-pass cap (censoring boundary). Default 25."},
        "cx_sigma_m2": {"type": "number", "description": "Enable single-generation charge exchange with this constant cross section, m² (order 1e-19 for D⁺ on D₂ in the keV band). With CX on, the result's `prediction` reports a floor (CX on) … ceiling (CX off) bracket from the same traces."}
    });
    if let (Some(o), Some(p)) = (sim_opts.as_object_mut(), op_point_props().as_object()) {
        o.extend(p.clone());
    }
    let mut opt_opts = json!({
        "nr": {"type": "number", "description": "Default 81."},
        "nz": {"type": "number", "description": "Default 161."},
        "particles": {"type": "number", "description": "Default 48."},
        "max_passes": {"type": "number", "description": "Default 20."},
        "max_iters": {"type": "number", "description": "Ascent iterations per start. Default 8."},
        "multi_start": {"type": "boolean", "description": "Three seeds across the box instead of one (the landscape is multimodal). Default true."},
        "gradient": {"type": "string", "enum": ["fd", "hybrid"], "description": "fd (default): central differences, every evaluation streamed. hybrid: adjoint for potentials / ampere-turns, FD for geometry, FD fallback — measured to end 2–11% below FD on the shielded two-ring, so FD stays the default. Needs a ring-list spec."},
        "adjoint_particles": {"type": "number", "description": "Adjoint ensemble size (hybrid)."},
        "adjoint_crossings": {"type": "number", "description": "Adjoint flight-time budget in ideal chamber crossings (hybrid)."}
    });
    if let (Some(o), Some(p)) = (opt_opts.as_object_mut(), op_point_props().as_object()) {
        o.extend(p.clone());
    }
    let pv = param_value_schema();
    vec![
        json!({
            "name": "simulate_charged_particles",
            "description": "Charged-particle optics for axisymmetric electrode devices (fusors, magnetically shielded-grid IEC, ring traps): solve the electrostatic field + ring-coil magnetics, trace a deuteron ensemble (Boris integrator, Bosch-Hale D-D yield weighting), and report interception fraction (the cathode-ammeter observable), recirculation, predicted neutron rate, fusion power, Q and distance-to-Lawson — as data plus `vcad.particle-claims/1` and unified-receipt claims (basis \"predicted\": Provisional until bench-measured; bind readings with compare_measurements). With `options.cx_sigma_m2` the result's `prediction` is a floor (CX on) … ceiling (CX off) bracket from one ensemble. `sweep.kv` adds a voltage sweep (the same bracket per point). `section: true` adds equipotential contours and deuteron tracks for drawing; in vcad.app they appear in Analysis → Fields over the device section. Vacuum single-particle optics: no space charge, no CX chain; neutron rates are floors. Deterministic. Cost ∝ grid × particles × passes; defaults run in seconds.",
            "inputSchema": {
                "type": "object", "required": ["spec"],
                "properties": {
                    "spec": device_spec_schema(),
                    "parameters": params_schema(),
                    "options": {"type": "object", "properties": sim_opts},
                    "section": {"description": "true, or {tracks, contours, max_points, spacing_mm}: also return the section drawing data (potential grid, equipotentials, wires, tracks in the y = 0 plane, x = signed r). Off by default: it is large.", "anyOf": [{"type": "boolean"}, {"type": "object"}]},
                    "sweep": {"type": "object", "description": "Voltage sweep: every biased ring rescaled so the deepest drop equals each value. {kv: [20, 25, 30], nr?, nz?, particles?, max_passes?} (grid defaults to the main run's).",
                        "properties": {"kv": {"type": "array", "items": {"type": "number"}}}}
                }
            }
        }),
        json!({
            "name": "optimize_electrodes",
            "description": "Design electrodes by gradient ascent: optimize named device-spec parameters (grid radius, ring spacing, shield ampere-turns, voltage) against predicted D-D yield per ion, with multi-start FD search (the yield landscape is multimodal — single starts stall on the recirculation hill at ~3x lower yield). Every objective evaluation is logged (`evaluations`, streamed as progress) and priced in n/s at the operating point. Returns the best parameter set, its yield, per-start results and ascent histories. Candidates that fail to resolve or converge score 0 rather than aborting. Follow with simulate_charged_particles at the winning parameters for full claims. Cost ≈ evals × one simulation.",
            "inputSchema": {
                "type": "object", "required": ["spec", "variables"],
                "properties": {
                    "spec": device_spec_schema(),
                    "parameters": params_schema(),
                    "variables": {"type": "array", "minItems": 1, "items": {
                        "type": "object", "required": ["name", "lo", "hi"],
                        "properties": {"name": {"type": "string"}, "lo": {"type": "number"}, "hi": {"type": "number"},
                                       "start": {"type": "number", "description": "Optional explicit start for the first seed."}}
                    }},
                    "options": {"type": "object", "properties": opt_opts}
                }
            }
        }),
        json!({
            "name": "simulate_neutron_shield",
            "description": "Monte Carlo neutron shielding run (analog transport, 5-group library, exact elastic kinematics): spherical layer stack around a point source (D-D band), H*(10) dose rate at each detector shell WITH 1-sigma batch error bars, attenuation vs bare source, thermal flux, absorbed/leakage balance — plus `vcad.neutronics-claims/1` and unified-receipt claims (Provisional until surveyed; bind readings with compare_measurements). `budget_usv_per_h` adds a pass/fail against a dose budget; `compare_bare: true` re-runs with every layer as air for the unshielded curve. Fail-closed: truncated histories or an unscored tally refuse claims. Neutron dose only: no capture gammas, no bremsstrahlung x-rays. Spherical geometry only. Deterministic per seed; histories capped at 5M.",
            "inputSchema": {
                "type": "object", "required": ["spec"],
                "properties": {
                    "spec": {
                        "type": "object", "required": ["layers", "source", "detectors"],
                        "description": "Spherical shield: concentric layers from r=0 outward around a point source, detector shells each strictly inside one layer (put them in air). Units mm.",
                        "properties": {
                            "layers": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["material", "thickness_mm"],
                                "properties": {"material": {"type": "string", "description": "hdpe, paraffin, borated-hdpe-5, water, lead, concrete, air, void."}, "thickness_mm": pv}}},
                            "source": {"type": "object", "required": ["rate_n_per_s", "energy_ev"], "properties": {"rate_n_per_s": pv, "energy_ev": pv}},
                            "detectors": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["label", "radius_mm"],
                                "properties": {"label": {"type": "string"}, "radius_mm": pv, "half_width_mm": pv}}},
                            "run": {"type": "object", "properties": {"histories_per_batch": {"type": "number"}, "batches": {"type": "number"}, "seed": {"type": "number"}}}
                        }
                    },
                    "parameters": params_schema(),
                    "budget_usv_per_h": {"type": "number", "description": "Dose budget; every detector is checked against it (including its 1σ)."},
                    "compare_bare": {"type": "boolean", "description": "Also run the same geometry with every layer as air (the unshielded curve)."}
                }
            }
        }),
        json!({
            "name": "compare_measurements",
            "description": "Bind bench measurements to a predicted claim set and return the receipt verdict per claim — Holds, Violated, or Unmeasured (fail-closed: an unmeasured claim never passes). Pass the `claim_set` object from a simulate_charged_particles or simulate_neutron_shield result unchanged, plus readings: e.g. {name: \"ddn_neutron_rate\", value: 3.1e4, uncertainty: 4e3, instrument: \"He-3 tube …\", band_factor: 3} or {name: \"dose_rate:operator\", value: 1.2, …} or {name: \"interception_fraction\", …}. A claim holds when measured ∈ [predicted/band − σ, predicted·band + σ] (neutronics also widens by the MC error). Returns the comparison rows with each acceptance window, measured receipt claims (Pass/Fail), and both rollups: the measured claims alone, and the whole receipt (Provisional while any claim is unmeasured).",
            "inputSchema": {
                "type": "object", "required": ["claim_set", "measurements"],
                "properties": {
                    "claim_set": {"type": "object", "description": "The `claim_set` from a simulation result (schema vcad.particle-claims/1 or vcad.neutronics-claims/1)."},
                    "measurements": {"type": "array", "minItems": 1, "items": {
                        "type": "object", "required": ["name", "value"],
                        "properties": {
                            "name": {"type": "string", "description": "Claim name (must match a claim in the set)."},
                            "value": {"type": "number", "description": "Measured value in the claim's unit."},
                            "uncertainty": {"type": "number", "description": "1σ absolute uncertainty, same unit. Default 0."},
                            "instrument": {"type": "string", "description": "Instrument provenance."},
                            "band_factor": {"type": "number", "description": "Multiplicative acceptance band (≥ 1). Default 2."}
                        }
                    }}
                }
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_cover_every_tool() {
        let names: Vec<String> = descriptors()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        for d in descriptors() {
            assert_eq!(d["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn unknown_tool_is_invalid() {
        let r = with_quiet_hooks(|h| run("nope", &json!({}), h));
        assert!(matches!(r, Err(ToolError::Invalid(_))));
    }
}
