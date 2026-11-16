//! `simulate_charged_particles` for a 3D wire device (`chamber` +
//! `electrodes`, a `vcad_kernel_particle::spec3d::DeviceSpec3d`): real globe
//! grids, solved by the thin-wire BEM and traced with a full-sphere
//! Fibonacci ensemble. Same numbers and output keys as the wasm
//! `particleSimulate3d` (#1193) plus a `prediction` block shaped like the 2D
//! one; no receipt claims (the claim set is bound to the 2D grid basis).

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde_json::{json, Value};
use vcad_kernel_particle as pk;

use crate::{Hooks, ToolError};

/// True when `spec` is a 3D wire device rather than a ring list.
pub fn is_3d(spec: &Value) -> bool {
    spec.get("electrodes").is_some()
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options3d {
    particles: usize,
    max_passes: u32,
    ion_current_a: f64,
    d2_pressure_mtorr: f64,
    temperature_k: f64,
    cx_sigma_m2: Option<f64>,
    step_mm: Option<f64>,
}

impl Default for Options3d {
    fn default() -> Self {
        Self {
            particles: 64,
            max_passes: 25,
            ion_current_a: 0.010,
            d2_pressure_mtorr: 2.0,
            temperature_k: 300.0,
            cx_sigma_m2: None,
            step_mm: None,
        }
    }
}

/// Run a 3D device. `args` carries `spec`, `parameters`, `options`;
/// `section` / `sweep` are 2D-only and refused rather than ignored.
pub fn simulate(args: &Value, hooks: &Hooks) -> Result<Value, ToolError> {
    let invalid = |m: String| ToolError::Invalid(m);
    if args.get("sweep").is_some() {
        return Err(invalid("`sweep` is 2D-only (ring specs); 3D sweeps are not wired yet".into()));
    }
    let spec: pk::spec3d::DeviceSpec3d = serde_json::from_value(args["spec"].clone())
        .map_err(|e| invalid(format!("bad 3D spec: {e}")))?;
    let params: BTreeMap<String, f64> = match args.get("parameters") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(p) => serde_json::from_value(p.clone())
            .map_err(|e| invalid(format!("bad parameters: {e}")))?,
    };
    let o: Options3d = match args.get("options") {
        None | Some(Value::Null) => Options3d::default(),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| invalid(format!("bad options (3D specs take particles, max_passes, ion_current_a, d2_pressure_mtorr, temperature_k, cx_sigma_m2, step_mm): {e}")))?,
    };
    if o.particles == 0 || o.particles > 4096 {
        return Err(invalid("options.particles must be in 1..=4096".into()));
    }
    let device = spec.resolve(&params).map_err(|e| invalid(e.to_string()))?;
    let cancelled = || hooks.cancel.load(Ordering::Relaxed);

    (hooks.progress)(0.02, "3D thin-wire BEM solve");
    // Same unknown cap as the wasm tool, so node and native agree.
    let bem_opts = pk::bem::BemOptions {
        max_unknowns: 1500,
        ..Default::default()
    };
    let field =
        pk::bem::solve(&device, &bem_opts).map_err(|e| ToolError::Solver(e.to_string()))?;
    if cancelled() {
        return Err(ToolError::Cancelled);
    }
    let n_d = pk::xsection::d2_deuteron_density_m3(o.d2_pressure_mtorr, o.temperature_k);
    let topts = pk::trace::TraceOptions {
        max_passes: o.max_passes,
        cx: o.cx_sigma_m2.map(|sigma| pk::trace::CxModel {
            sigma_cx_m2: sigma,
            background_deuteron_density_m3: n_d,
        }),
        ..Default::default()
    };
    let step = match o.step_mm {
        Some(mm) if mm.is_finite() && mm > 0.0 => mm * 1e-3,
        Some(_) => return Err(invalid("step_mm must be finite and positive".into())),
        None => field.default_step_length_m(),
    };
    let tracer = field.tracer(step, topts);
    // pk::bem::launch_fibonacci, one particle at a time so the run streams
    // progress and honours cancel.
    let r = tracer.shell_radius_m();
    let dirs = pk::wire3d::fibonacci_sphere(o.particles);
    let n = dirs.len();
    let mut outcomes = Vec::with_capacity(n);
    for (i, d) in dirs.into_iter().enumerate() {
        if cancelled() {
            return Err(ToolError::Cancelled);
        }
        outcomes.push(tracer.trace_from(
            pk::trace::DEUTERON,
            [d[0] * r, d[1] * r, d[2] * r],
            [0.0; 3],
        ));
        (hooks.progress)(
            0.1 + 0.9 * (i + 1) as f64 / n as f64,
            &format!("traced {}/{n} deuterons (3D)", i + 1),
        );
    }
    let stats = pk::fom::stats(&outcomes);
    let first =
        outcomes.iter().filter(|o| o.core_passes >= 1).count() as f64 / outcomes.len().max(1) as f64;
    let ceiling = pk::fom::neutron_rate_per_s(stats.mean_ddn_sigma_v_m3, o.ion_current_a, n_d);
    let floor = o.cx_sigma_m2.map(|_| {
        pk::fom::neutron_rate_from_counts(
            stats.mean_neutrons_ion_channel + stats.mean_neutrons_cx_channel,
            o.ion_current_a,
        )
    });
    let note = "3D thin-wire BEM (vcad-kernel-particle::bem). Receipt claims are not issued \
                for 3D devices yet: vcad.particle-claims/1 is bound to the 2D grid basis.";
    Ok(json!({
        "device_kind": "3d",
        "stats": stats,
        "first_pass_transparency": first,
        "geometric_transparency": device.geometric_transparency(4000),
        "center_potential_v": field.potential([0.0; 3]),
        "neutron_rate_ceiling_per_s": ceiling,
        "neutron_rate_floor_per_s": floor,
        "unknowns": field.unknowns(),
        "residual": field.residual_report(),
        "step_mm": step * 1e3,
        "claims": Value::Null,
        "note": note,
        "prediction": {
            "neutron_rate_n_per_s": floor.unwrap_or(ceiling),
            "floor_n_per_s": floor,
            "ceiling_n_per_s": ceiling,
            "cx_included": floor.is_some(),
            "note": note,
        },
        "operating_point": {"ion_current_a": o.ion_current_a, "d2_pressure_mtorr": o.d2_pressure_mtorr, "temperature_k": o.temperature_k},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::with_quiet_hooks;

    /// A 3-great-circle globe runs natively, keys match the wasm tool, and
    /// 2D-only knobs are refused instead of silently ignored.
    #[test]
    fn globe_runs_and_two_d_knobs_are_refused() {
        let spec = json!({
            "chamber": {"kind": "sphere", "radius_mm": 60.0},
            "electrodes": [{"kind": "great_circle_globe", "grid_radius_mm": 20.0,
                            "layout": "orthogonal3", "wire_radius_mm": 1.0, "potential_v": "v"}]
        });
        let out = with_quiet_hooks(|h| {
            simulate(
                &json!({"spec": spec, "parameters": {"v": -20000.0},
                        "options": {"particles": 6, "max_passes": 2, "cx_sigma_m2": 1e-19}}),
                h,
            )
        })
        .unwrap();
        assert_eq!(out["device_kind"], "3d");
        assert!(out["unknowns"].as_u64().unwrap() > 0);
        assert!(out["center_potential_v"].as_f64().unwrap() < -1000.0);
        let p = &out["prediction"];
        assert!(p["floor_n_per_s"].as_f64().unwrap() <= p["ceiling_n_per_s"].as_f64().unwrap());
        assert!(out["residual"].is_object());

        let swept = with_quiet_hooks(|h| {
            simulate(&json!({"spec": spec, "parameters": {"v": -2e4}, "sweep": {"kv": [10]}}), h)
        });
        assert!(matches!(swept, Err(ToolError::Invalid(_))));
        let nr = with_quiet_hooks(|h| {
            simulate(&json!({"spec": spec, "parameters": {"v": -2e4}, "options": {"nr": 51}}), h)
        });
        assert!(matches!(nr, Err(ToolError::Invalid(_))));
    }
}
