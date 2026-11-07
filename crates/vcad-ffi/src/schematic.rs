//! Schematic checks and circuit simulation for the native app
//! (`vcad_schematic_*`): ERC and the `simulate_circuit` twin.
//!
//! The sheet is the IR `SchematicSheet` the document stores under
//! `schematic`. Simulation maps it through the same fail-closed converter the
//! MCP tool uses (`circuit_from_schematic`: R/C/L/V/I/D by refdes prefix) —
//! an unmappable component is reported, never skipped. Connectors, test
//! points and mounting holes are stubbed as opens (they anchor nets but carry
//! no device), and every stub is listed in the result.

use std::ffi::c_char;
use std::ptr;

use serde::Serialize;
use serde_json::{json, Value};
use vcad_ecad_sim::circuit::dc::operating_point;
use vcad_ecad_sim::circuit::netlist::{circuit_from_schematic, is_ground_net, ConvertOptions};
use vcad_ecad_sim::circuit::CircuitEnv;
use vcad_ir::ecad::SchematicSheet;

use crate::analysis::parse_request;
use crate::err;
use crate::json::{borrow_str, guard_ptr, json_out};

fn run<T: Serialize>(what: &str, f: impl FnOnce() -> Result<T, String>) -> *mut c_char {
    guard_ptr(what, || match f() {
        Ok(v) => json_out(&v),
        Err(e) => {
            err::set_error(format!("{what}: {e}"));
            ptr::null_mut()
        }
    })
}

fn parse_sheet(json: *const c_char) -> Result<SchematicSheet, String> {
    let text = borrow_str("schematic", json).ok_or("schematic: bad string")?;
    serde_json::from_str(text).map_err(|e| format!("schematic: parse: {e}"))
}

/// Refdes prefixes that carry no electrical device.
const STUB_PREFIXES: &[&str] = &["J", "P", "TP", "H", "MH", "CN"];

fn prefix(r: &str) -> String {
    r.chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Simulate a schematic: DC operating point, plus an optional transient.
///
/// request `{"ground"?: net, "tstop"?: s, "dt"?: s, "probes"?: [net]}`.
/// `ground` renames that net to `GND` first (so a sheet whose return net is
/// called e.g. `N2` can still be solved). Returns
/// `{dc: {net: V}, currents: {ref: A}, transient?: {time:[..], nets:{net:[..]}}, stubbed, ground_nets}`.
pub fn simulate(mut sheet: SchematicSheet, req: &Value) -> Result<Value, String> {
    if let Some(g) = req
        .get("ground")
        .and_then(Value::as_str)
        .filter(|g| !g.is_empty())
    {
        if let Some(nets) = sheet.nets.as_mut() {
            if let Some(pins) = nets.remove(g) {
                nets.entry("GND".into()).or_default().extend(pins);
            }
        }
    }
    let stub: Vec<String> = sheet
        .components
        .iter()
        .map(|c| c.reference.clone())
        .filter(|r| STUB_PREFIXES.contains(&prefix(r).as_str()))
        .collect();
    let mapped = circuit_from_schematic(&sheet, &ConvertOptions { stub_as_open: stub })
        .map_err(|e| e.to_string())?;
    if mapped.ground_nets.is_empty() && !mapped.node_of_net.keys().any(|n| is_ground_net(n)) {
        return Err(
            "no ground net — name a net GND (or pass a ground net) so voltages have a reference"
                .into(),
        );
    }
    let dc = operating_point(&mapped.circuit).map_err(|e| format!("DC operating point: {e}"))?;
    let volts = |v: &[f64]| -> serde_json::Map<String, Value> {
        mapped
            .node_of_net
            .iter()
            .map(|(n, &id)| (n.clone(), json!(v.get(id).copied().unwrap_or(0.0))))
            .collect()
    };
    let currents: serde_json::Map<String, Value> = mapped
        .device_of_ref
        .iter()
        .map(|(r, &id)| {
            (
                r.clone(),
                json!(dc.device_currents.get(id).copied().unwrap_or(0.0)),
            )
        })
        .collect();
    let mut out = json!({
        "dc": volts(&dc.node_voltages),
        "currents": currents,
        "stubbed": mapped.stubbed,
        "ground_nets": mapped.ground_nets,
        "newton_iterations": dc.newton_iterations,
    });
    if let Some(tstop) = req
        .get("tstop")
        .and_then(Value::as_f64)
        .filter(|t| *t > 0.0)
    {
        let dt = req
            .get("dt")
            .and_then(Value::as_f64)
            .unwrap_or(tstop / 500.0);
        if dt.is_nan() || dt <= 0.0 || tstop / dt > 200_000.0 {
            return Err(format!(
                "transient: dt {dt} too small for tstop {tstop} (max 200k steps)"
            ));
        }
        let probes: Vec<String> = req
            .get("probes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| mapped.node_of_net.keys().cloned().collect());
        let mut env = CircuitEnv::new(mapped.circuit.clone(), dt);
        env.reset();
        let stride = ((tstop / dt) / 400.0).ceil().max(1.0) as usize;
        let mut time = vec![];
        let mut series: Vec<Vec<f64>> = vec![vec![]; probes.len()];
        let mut push = |o: &vcad_ecad_sim::circuit::Observation, time: &mut Vec<f64>| {
            time.push(o.time);
            for (i, p) in probes.iter().enumerate() {
                let id = mapped.node_of_net.get(p).copied().unwrap_or(0);
                series[i].push(o.node_voltages.get(id).copied().unwrap_or(0.0));
            }
        };
        push(&env.observe(), &mut time);
        let mut k = 0usize;
        while env.observe().time < tstop - dt * 0.5 {
            let o = env.step();
            k += 1;
            if k.is_multiple_of(stride) {
                push(&o, &mut time);
            }
        }
        let nets: serde_json::Map<String, Value> = probes
            .iter()
            .cloned()
            .zip(series.into_iter().map(|s| json!(s)))
            .collect();
        out["transient"] = json!({ "time": time, "nets": nets });
    }
    Ok(out)
}

/// `char *vcad_schematic_simulate(const char *schematic_json, const char *request_json)`.
#[no_mangle]
pub extern "C" fn vcad_schematic_simulate(
    sheet_json: *const c_char,
    request_json: *const c_char,
) -> *mut c_char {
    run("vcad_schematic_simulate", || {
        let sheet = parse_sheet(sheet_json)?;
        let req = parse_request(request_json)?;
        simulate(sheet, &req)
    })
}

/// `char *vcad_schematic_erc(const char *schematic_json)` →
/// `{violations: [...], count}`.
#[no_mangle]
pub extern "C" fn vcad_schematic_erc(sheet_json: *const c_char) -> *mut c_char {
    run("vcad_schematic_erc", || {
        let sheet = parse_sheet(sheet_json)?;
        let v = vcad_ecad_schematic::erc::check_erc(&sheet);
        Ok(json!({ "count": v.len(), "violations": v }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(r: &str, v: &str) -> Value {
        // Distinct positions: coincident pins are joined geometrically.
        let x = r.bytes().map(|b| b as f64).sum::<f64>() * 10.0;
        json!({"ref": r, "value": v, "footprintId": "x", "position": {"x":x,"y":0},
               "pins": [{"number":"1","name":"1","pin_type":"Passive","position":{"x":-2,"y":0}},
                        {"number":"2","name":"2","pin_type":"Passive","position":{"x":2,"y":0}}]})
    }

    /// 5 V across a 1k/1k divider: OUT = 2.5 V; RC transient settles there.
    #[test]
    fn divider_dc_and_transient() {
        let sheet: SchematicSheet = serde_json::from_value(json!({
            "components": [comp("V1","5"), comp("R1","1k"), comp("R2","1k"), comp("C1","1u"), comp("J1","conn")],
            "wires": [], "junctions": [], "labels": [],
            "nets": {"VIN": ["V1.1","R1.1","J1.1"], "OUT": ["R1.2","R2.1","C1.1"], "N3": ["V1.2","R2.2","C1.2","J1.2"]}
        })).unwrap();
        let r = simulate(sheet, &json!({"ground": "N3", "tstop": 0.01})).unwrap();
        let out = r["dc"]["OUT"].as_f64().unwrap();
        assert!((out - 2.5).abs() < 1e-6, "OUT = {out}");
        assert_eq!(r["stubbed"], json!(["J1"]));
        let tr = r["transient"]["nets"]["OUT"].as_array().unwrap();
        assert!((tr.last().unwrap().as_f64().unwrap() - 2.5).abs() < 0.01);
    }

    #[test]
    fn no_ground_fails_closed() {
        let sheet: SchematicSheet = serde_json::from_value(json!({
            "components": [comp("V1","5"), comp("R1","1k")],
            "nets": {"A": ["V1.1","R1.1"], "B": ["V1.2","R1.2"]}
        }))
        .unwrap();
        assert!(simulate(sheet, &json!({})).is_err());
    }
}
