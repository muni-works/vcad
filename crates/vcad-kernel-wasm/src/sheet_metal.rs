//! WASM bindings for sheet-metal evaluation.
//!
//! Thin `#[wasm_bindgen]` wrappers over [`vcad_kernel::sheet_json`], which
//! holds the JSON-over-the-boundary implementation (shared with the native
//! C ABI in `vcad-ffi`). See that module for the chain JSON shape.

use vcad_kernel::sheet_json as sj;
use wasm_bindgen::prelude::*;

/// Evaluate a sheet-metal op chain → mesh, flat pattern, summary, DXF.
#[wasm_bindgen(js_name = evaluateSheetMetalChain)]
pub fn evaluate_sheet_metal_chain(chain_json: &str) -> String {
    sj::evaluate_sheet_metal_chain(chain_json)
}

/// Manufacturability check against a shop profile.
#[wasm_bindgen(js_name = checkSheetMetal)]
pub fn check_sheet_metal(chain_json: &str, shop_json: &str) -> String {
    sj::check_sheet_metal(chain_json, shop_json)
}

/// Cost estimate for a chain.
#[wasm_bindgen(js_name = costSheetMetal)]
pub fn cost_sheet_metal(chain_json: &str, rates_json: &str, quantity: u32) -> String {
    sj::cost_sheet_metal(chain_json, rates_json, quantity)
}

/// Rectangular nesting of part footprints on stock sheets.
#[wasm_bindgen(js_name = nestSheetMetalParts)]
pub fn nest_sheet_metal_parts(parts_json: &str, params_json: &str) -> String {
    sj::nest_sheet_metal_parts(parts_json, params_json)
}

/// One layered DXF per nested stock sheet.
#[wasm_bindgen(js_name = nestedSheetMetalDxf)]
pub fn nested_sheet_metal_dxf(placements_json: &str) -> String {
    sj::nested_sheet_metal_dxf(placements_json)
}

/// Feasible bend sequence for a chain.
#[wasm_bindgen(js_name = sheetMetalSequence)]
pub fn sheet_metal_sequence(chain_json: &str) -> String {
    sj::sheet_metal_sequence(chain_json)
}

/// Built-in sheet-metal materials registry.
#[wasm_bindgen(js_name = getSheetMetalMaterials)]
pub fn get_sheet_metal_materials() -> String {
    sj::get_sheet_metal_materials()
}

/// Built-in shop bending catalog by id.
#[wasm_bindgen(js_name = getSheetMetalShopCatalog)]
pub fn get_sheet_metal_shop_catalog(shop_id: &str) -> String {
    sj::get_sheet_metal_shop_catalog(shop_id)
}

/// Built-in bend-table rows.
#[wasm_bindgen(js_name = getSheetMetalBendTable)]
pub fn get_sheet_metal_bend_table() -> String {
    sj::get_sheet_metal_bend_table()
}

/// Folded solid as a STEP file with true cylindrical bend faces.
#[wasm_bindgen(js_name = sheetMetalFoldedStep)]
pub fn sheet_metal_folded_step(chain_json: &str) -> String {
    sj::sheet_metal_folded_step(chain_json)
}

/// Recover a flat pattern from an arbitrary constant-thickness solid mesh.
#[wasm_bindgen(js_name = flattenSolidToSheetMetal)]
pub fn flatten_solid_to_sheet_metal(request_json: &str) -> String {
    sj::flatten_solid_to_sheet_metal(request_json)
}
