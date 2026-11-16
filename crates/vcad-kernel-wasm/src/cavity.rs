//! WASM binding for document → acoustic cavity → bass-reflex analysis
//! (`vcad_eval::cavity_extract` + `vcad_kernel_acoustics::ported`).
//!
//! One JSON-over-the-boundary entry point: the MCP `simulate_cavity` tool
//! sends the session document, the part selection, solver options and
//! parameter overrides, and gets back `{extracted, report}`.

use std::collections::BTreeMap;

use serde::Serialize;
use vcad_eval::cavity_extract::{extract_cavity, CavityExtraction, ExtractedCavity};
use vcad_kernel::vcad_kernel_acoustics::ported::{analyze, AnalysisOptions, PortedBoxReport};
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct CavityRun {
    extracted: ExtractedCavity,
    report: PortedBoxReport,
}

/// Extract a ported box from a document and analyze it.
///
/// `doc_json` is a `vcad_ir::Document`; `selection_json` a
/// `vcad_eval::cavity_extract::CavityExtraction`; `options_json` a
/// `vcad_kernel_acoustics::ported::AnalysisOptions` (`{}` for defaults);
/// `params_json` overrides for the document's parameter values when
/// resolving named spec fields. Fail-closed: unrepresentable geometry errors
/// naming the part and face.
#[wasm_bindgen(js_name = simulateCavity)]
pub fn simulate_cavity(
    doc_json: &str,
    selection_json: &str,
    options_json: &str,
    params_json: &str,
) -> Result<String, JsValue> {
    let doc: vcad_ir::Document = serde_json::from_str(doc_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse document: {e}")))?;
    let sel: CavityExtraction = serde_json::from_str(selection_json)
        .map_err(|e| JsValue::from_str(&format!("bad cavity selection: {e}")))?;
    let opts: AnalysisOptions = serde_json::from_str(options_json)
        .map_err(|e| JsValue::from_str(&format!("bad cavity options: {e}")))?;
    let overrides: BTreeMap<String, f64> = serde_json::from_str(params_json)
        .map_err(|e| JsValue::from_str(&format!("bad parameters: {e}")))?;
    let mut extracted =
        extract_cavity(&doc, &sel).map_err(|e| JsValue::from_str(&e.to_string()))?;
    extracted.parameters.extend(overrides);
    let report = analyze(&extracted.model, &extracted.parameters, &opts)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    serde_json::to_string(&CavityRun { extracted, report })
        .map_err(|e| JsValue::from_str(&e.to_string()))
}
