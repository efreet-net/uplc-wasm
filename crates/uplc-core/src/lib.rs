//! Portable implementation boundary for the new evaluator.
//!
//! Implement Flat decoding, the CEK machine, builtins, and costing here. The core
//! deliberately depends on neither reference implementation. Until implemented,
//! requests return Unsupported, which is never a conformance pass.

use uplc_conformance::{Outcome, Request};

pub fn evaluate(request: &Request) -> Outcome {
    if let Err(error) = request.validate() {
        return Outcome::infrastructure(format!("invalid request: {error}"));
    }
    Outcome::unsupported(
        "candidate Flat decoder, CEK machine, builtins, and costing are not implemented",
    )
}

pub fn evaluate_json(request: &str) -> String {
    uplc_conformance::dispatch(request, "uplc-core", env!("CARGO_PKG_VERSION"), evaluate)
}
