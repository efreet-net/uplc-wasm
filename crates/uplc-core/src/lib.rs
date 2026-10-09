//! Independent primitive UPLC evaluator shared by native and WebAssembly APIs.
//!
//! The first milestone supports bounded raw Flat programs, primitive constants,
//! variables, lambdas/application, delay/force, and explicit errors under the
//! supplied PlutusV3/protocol-11 restricting model. Other features and resource
//! limits remain explicit Unsupported outcomes, never conformance passes.

pub mod cost;

use cost::{BudgetError, ExecutionBudget, MachineCosts, ModelError};
use error::{DecodeError, RuntimeError};
use machine::MachineError;
use uplc_conformance::{Budget, FailureKind, Language, Mode, Outcome, Program as Input, Request};

pub mod ast;
pub mod builtin;
pub mod error;
pub mod flat;
pub mod limits;
pub mod machine;

pub fn evaluate(request: &Request) -> Outcome {
    let parameters = match request.validate() {
        Ok(parameters) => parameters,
        Err(error) => return Outcome::infrastructure(format!("invalid request: {error}")),
    };
    if !matches!(request.profile.language, Language::PlutusV3)
        || request.profile.protocol_major != 11
    {
        return Outcome::unsupported("initial evaluator profile: PlutusV3 / protocol 11");
    }
    let Mode::Restricting { budget } = &request.mode else {
        return Outcome::unsupported("genuine counting mode is not implemented");
    };
    let costs = match MachineCosts::from_parameters(&parameters) {
        Ok(costs) => costs,
        Err(error @ ModelError::WrongParameterCount { .. }) => {
            return Outcome::infrastructure(error.to_string());
        }
        Err(error @ ModelError::NegativeMachineCost { .. }) => {
            return Outcome::unsupported(error.to_string());
        }
    };
    let Input::Flat { hex } = &request.program else {
        return Outcome::unsupported("textual UPLC parsing is not implemented; supply raw Flat");
    };
    let bytes = hex::decode(hex).expect("validated Flat hexadecimal transport");
    let program = match flat::decode(&bytes) {
        Ok(program) => program,
        Err(error) => return decode_outcome(error),
    };
    let (cpu, mem) = budget.limits().expect("validated restricting limits");
    let evaluation = machine::evaluate(&program, &costs, ExecutionBudget { cpu, mem });
    let consumed = evaluation
        .consumed
        .map(|used| Budget::new(used.cpu, used.mem));
    match evaluation.result {
        Ok(program) => match program.normalize() {
            Ok(term) => match consumed {
                Some(budget) => Outcome::Success {
                    term,
                    budget,
                    traces: vec![],
                },
                None => {
                    Outcome::infrastructure("successful evaluation has no representable budget")
                }
            },
            Err(error) => runtime_outcome(error, consumed),
        },
        Err(MachineError::InvalidProgram(error)) => decode_outcome(error),
        Err(MachineError::Runtime(error)) => runtime_outcome(error, consumed),
        Err(MachineError::Budget(error @ (BudgetError::Exhausted | BudgetError::Overflow))) => {
            Outcome::Failure {
                kind: FailureKind::BudgetExhausted,
                budget: consumed,
                traces: vec![],
                diagnostic: error.to_string(),
            }
        }
        Err(MachineError::Budget(error)) => Outcome::infrastructure(error.to_string()),
    }
}

fn decode_outcome(error: DecodeError) -> Outcome {
    match error {
        DecodeError::Malformed(reason) => Outcome::failure(FailureKind::Decode, reason),
        DecodeError::Unsupported(reason) => Outcome::unsupported(reason),
    }
}

fn runtime_outcome(error: RuntimeError, budget: Option<Budget>) -> Outcome {
    match error {
        RuntimeError::Unsupported(reason) => Outcome::unsupported(reason),
        error => Outcome::Failure {
            kind: FailureKind::Evaluation,
            budget,
            traces: vec![],
            diagnostic: error.to_string(),
        },
    }
}

pub fn evaluate_json(request: &str) -> String {
    uplc_conformance::dispatch(request, "uplc-core", env!("CARGO_PKG_VERSION"), evaluate)
}
