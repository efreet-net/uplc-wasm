use wasm_bindgen::prelude::*;

/// JSON strings preserve arbitrary-size UPLC integers across the JavaScript ABI.
#[wasm_bindgen]
pub fn evaluate_json(request: &str) -> String {
    uplc_core::evaluate_json(request)
}
