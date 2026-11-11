//! `create_cad_loon` replacing a document that holds electronics: loading the
//! program replaces the mechanical parts, not the board or schematic. Port of
//! the app's `AgentEdits.carryElectronics` (and node's `carry-electronics.ts`),
//! on the IR as JSON so unknown fields ride along untouched.

use serde_json::{json, Map, Value};

fn is_board(n: &Value) -> bool {
    n.pointer("/op/type").and_then(Value::as_str) == Some("PcbBoard")
}

/// Carry `session`'s board (and the transform chain above it), `pcb` and
/// `schematic` into `doc`. `Ok(None)`: nothing to carry. `Err`: both hold a
/// board or schematic — they can't be merged, nothing should change.
pub fn carry_electronics(session: &Value, doc: &mut Value) -> Result<Option<String>, String> {
    let empty = Map::new();
    let nodes = session["nodes"].as_object().unwrap_or(&empty);
    let node = |id: i64| nodes.get(&id.to_string());
    let board_ids: Vec<i64> = nodes
        .values()
        .filter(|n| is_board(n))
        .filter_map(|n| n["id"].as_i64())
        .collect();
    let has_pcb = !session["pcb"].is_null();
    let schematic = session.get("schematic").filter(|s| !s.is_null()).cloned();
    if board_ids.is_empty() && !has_pcb && schematic.is_none() {
        return Ok(None);
    }
    let mut target = doc["nodes"].as_object().cloned().unwrap_or_default();
    let program_has_board = target.values().any(is_board) || !doc["pcb"].is_null();
    let program_has_schematic = doc.get("schematic").is_some_and(|s| !s.is_null());
    if ((!board_ids.is_empty() || has_pcb) && program_has_board)
        || (schematic.is_some() && program_has_schematic)
    {
        return Err("create_cad_loon: this document already holds a board or schematic and the program defines its own, so they can't be merged; nothing was changed. Remove them from the source.".into());
    }
    // The scene root is the Translate/Rotate/Scale chain above the board.
    let chain = |root: i64| -> Option<Vec<i64>> {
        let mut out = Vec::new();
        let mut id = Some(root);
        while let Some(at) = id {
            let n = node(at)?;
            if out.contains(&at) {
                return None;
            }
            out.push(at);
            let ty = n.pointer("/op/type").and_then(Value::as_str).unwrap_or("");
            if ty == "PcbBoard" {
                return Some(out);
            }
            id = matches!(ty, "Translate" | "Rotate" | "Scale")
                .then(|| n.pointer("/op/child").and_then(Value::as_i64))
                .flatten();
        }
        None
    };
    let rooted: Vec<(Value, Vec<i64>)> = session["roots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| Some((e.clone(), chain(e["root"].as_i64()?)?)))
        .collect();
    let mut carried: Vec<i64> = Vec::new();
    for id in rooted.iter().flat_map(|(_, c)| c.iter().copied()).chain(board_ids.iter().copied()) {
        if !carried.contains(&id) {
            carried.push(id);
        }
    }
    let mut next = target.keys().filter_map(|k| k.parse::<i64>().ok()).max().unwrap_or(-1) + 1;
    let mut remap = std::collections::HashMap::new();
    for id in &carried {
        remap.insert(*id, next);
        next += 1;
    }
    let mut roots = doc["roots"].as_array().cloned().unwrap_or_default();
    let mut materials = doc["materials"].as_object().cloned().unwrap_or_default();
    let session_materials = session["materials"].as_object().cloned().unwrap_or_default();
    let mut keep_material = |key: Option<&str>, materials: &mut Map<String, Value>| {
        if let Some(k) = key {
            if !materials.contains_key(k) {
                if let Some(m) = session_materials.get(k) {
                    materials.insert(k.into(), m.clone());
                }
            }
        }
    };
    for (entry, c) in &rooted {
        let mut e = entry.clone();
        e["root"] = json!(remap[&c[0]]);
        roots.push(e);
        keep_material(entry["material"].as_str(), &mut materials);
    }
    let rooted_boards: Vec<i64> = rooted.iter().filter_map(|(_, c)| c.last().copied()).collect();
    for id in &board_ids {
        if !rooted_boards.contains(id) {
            roots.push(json!({"root": remap[id], "material": "__pcb_fr4__"}));
            keep_material(Some("__pcb_fr4__"), &mut materials);
        }
    }
    let mut part_ids = doc["partIds"].as_object().cloned().unwrap_or_default();
    let session_part_ids = session["partIds"].as_object().cloned().unwrap_or_default();
    for old in &carried {
        let Some(mut n) = node(*old).cloned() else { continue };
        let id = remap[old];
        n["id"] = json!(id);
        if let Some(child) = n.pointer("/op/child").and_then(Value::as_i64) {
            if let Some(c) = remap.get(&child) {
                n["op"]["child"] = json!(c);
            }
        }
        target.insert(id.to_string(), n);
        if let Some(stable) = session_part_ids.get(&old.to_string()) {
            part_ids.insert(id.to_string(), stable.clone());
        }
    }
    doc["nodes"] = Value::Object(target);
    doc["roots"] = Value::Array(roots);
    doc["materials"] = Value::Object(materials);
    if !part_ids.is_empty() {
        doc["partIds"] = Value::Object(part_ids);
    }
    if has_pcb {
        doc["pcb"] = session["pcb"].clone();
    }
    if let Some(s) = schematic.clone() {
        doc["schematic"] = s;
    }
    let what: Vec<&str> = [
        (!board_ids.is_empty() || has_pcb).then_some("PCB board"),
        schematic.is_some().then_some("schematic"),
    ]
    .into_iter()
    .flatten()
    .collect();
    Ok(Some(format!("kept the document's {}", what.join(" and "))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_board_and_refuses_two() {
        let session = json!({
            "nodes": {
                "0": {"id": 0, "op": {"type": "PcbBoard"}},
                "1": {"id": 1, "op": {"type": "Translate", "child": 0, "offset": {"x": 0, "y": 0, "z": 0}}},
                "2": {"id": 2, "op": {"type": "Cube"}}
            },
            "roots": [{"root": 1, "material": "fr4"}, {"root": 2}],
            "materials": {"fr4": {"name": "FR4"}},
            "pcb": {"layers": 2}
        });
        let mut doc = json!({"nodes": {"0": {"id": 0, "op": {"type": "Cube"}}}, "roots": [{"root": 0}]});
        let note = carry_electronics(&session, &mut doc).unwrap().unwrap();
        assert_eq!(note, "kept the document's PCB board");
        assert_eq!(doc["roots"].as_array().unwrap().len(), 2);
        // Translate (old 1) → 1, board (old 0) → 2; the chain points at the new id.
        assert_eq!(doc["nodes"]["1"]["op"]["child"], 2, "chain remapped");
        assert_eq!(doc["nodes"]["2"]["op"]["type"], "PcbBoard");
        assert_eq!(doc["pcb"]["layers"], 2);
        assert!(doc["materials"]["fr4"].is_object());
        let mut again = doc.clone();
        assert!(carry_electronics(&session, &mut again).is_err());
        let mut plain = json!({"nodes": {}, "roots": []});
        assert!(carry_electronics(&json!({"nodes": {}, "roots": []}), &mut plain).unwrap().is_none());
    }
}
