//! Independent, iterative CEK evaluation for primitive UPLC terms.
//!
//! The transition rules follow the pinned Plutus specification's
//! `doc/plutus-core-spec/untyped-cek-machine.tex` (revision
//! `5f785edeac0d1d89622d44344fdda07ef48e8c73`). Values and environments contain
//! arena indices rather than recursive owners. Constants borrow their source
//! term until discharge, so evaluating a constant does not copy its payload.
//!
//! Startup is charged immediately. Compute events accumulate in batches of 200
//! (the official default slippage), charged before the 200th event's action and
//! on successful termination, in constant/variable/lambda/apply/delay/force order.
//! Semantic errors do not flush an unfinished batch. Return, closure application,
//! discharge, and explicit error have no execution-unit charge of their own.
//! A failed charge retains the full attempted charge; see [`BudgetMeter`].
//! Independent resource bounds cover compute/return/discharge transitions and
//! environment lookups, and at most one million aggregate runtime arena/frame
//! entries. Discharge bounds expanded nodes, depth, and copied constant payload
//! before allocation. JSON normalization additionally checks serialized size.
//! Resource exhaustion is unsupported, never a semantic evaluation failure.

use std::fmt;

use crate::{
    ast::{Constant, Program, Term, TermId},
    cost::{BudgetError, BudgetMeter, ExecutionBudget, MachineCosts, Step},
    error::{DecodeError, RuntimeError},
    limits::{MAX_MACHINE_STEPS, MAX_OUTPUT_BYTES, MAX_OUTPUT_DEPTH, MAX_OUTPUT_NODES},
};

/// Values, environment cells, and pending frames together may contain this many
/// entries. Vec capacity growth is bounded by a constant factor of this limit.
pub const MAX_RUNTIME_ENTRIES: usize = 1_000_000;

/// Pinned official CEK `defaultSlippage`, also used by both reference adapters.
const SLIPPAGE: u32 = 200;

const CHARGE_ORDER: [Step; 6] = [
    Step::Constant,
    Step::Var,
    Step::Lambda,
    Step::Apply,
    Step::Delay,
    Step::Force,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineError {
    InvalidProgram(DecodeError),
    Runtime(RuntimeError),
    Budget(BudgetError),
}

impl fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProgram(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::Budget(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MachineError {}

impl From<RuntimeError> for MachineError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<BudgetError> for MachineError {
    fn from(error: BudgetError) -> Self {
        Self::Budget(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub result: Result<Program, MachineError>,
    /// Includes a failed attempted charge. None means its exact total exceeds
    /// the wire's i64 range; it is never saturated or wrapped.
    pub consumed: Option<ExecutionBudget>,
}

/// Evaluate a raw AST with the supplied costs and restricting limit. Invalid
/// arenas and unsupported versions are rejected before the startup charge.
pub fn evaluate(program: &Program, costs: &MachineCosts, limit: ExecutionBudget) -> Evaluation {
    evaluate_with_limits(program, costs, limit, ResourceLimits::default())
}

#[derive(Clone, Copy)]
struct ResourceLimits {
    work: usize,
    entries: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            work: MAX_MACHINE_STEPS,
            entries: MAX_RUNTIME_ENTRIES,
        }
    }
}

fn evaluate_with_limits(
    program: &Program,
    costs: &MachineCosts,
    limit: ExecutionBudget,
    resources: ResourceLimits,
) -> Evaluation {
    let mut meter = match BudgetMeter::new(limit) {
        Ok(meter) => meter,
        Err(error) => {
            return Evaluation {
                result: Err(MachineError::Budget(error)),
                consumed: Some(ExecutionBudget { cpu: 0, mem: 0 }),
            };
        }
    };
    let result = (|| {
        program.validate().map_err(MachineError::InvalidProgram)?;
        if !matches!(program.version, [1, 0, 0] | [1, 1, 0]) {
            return Err(MachineError::InvalidProgram(DecodeError::Unsupported(
                "the machine supports UPLC versions 1.0.0 and 1.1.0".into(),
            )));
        }
        meter.charge(costs.cost(Step::Startup))?;
        let mut machine = Machine {
            program,
            costs,
            meter: &mut meter,
            values: Vec::new(),
            environments: Vec::new(),
            frames: Vec::new(),
            work_left: resources.work,
            entry_limit: resources.entries,
            event_counts: [0; 6],
            pending_events: 0,
        };
        let value = machine.run()?;
        machine.discharge(value).map_err(MachineError::Runtime)
    })();
    Evaluation {
        result,
        consumed: meter.consumed(),
    }
}

type ValueId = usize;
type EnvId = usize;
type Environment = Option<EnvId>;

#[derive(Clone, Copy)]
enum Value {
    Constant(TermId),
    Lambda { body: TermId, env: Environment },
    Delay { body: TermId, env: Environment },
}

struct EnvEntry {
    value: ValueId,
    parent: Environment,
    length: usize,
}

enum Frame {
    Argument { term: TermId, env: Environment },
    Function(ValueId),
    Force,
}

enum State {
    Compute(TermId, Environment),
    Return(ValueId),
}

struct Machine<'a> {
    program: &'a Program,
    costs: &'a MachineCosts,
    meter: &'a mut BudgetMeter,
    values: Vec<Value>,
    environments: Vec<EnvEntry>,
    frames: Vec<Frame>,
    work_left: usize,
    entry_limit: usize,
    event_counts: [u32; 6],
    pending_events: u32,
}

impl Machine<'_> {
    fn run(&mut self) -> Result<ValueId, MachineError> {
        let mut state = State::Compute(self.program.root, None);
        loop {
            self.work()?;
            state = match state {
                State::Compute(id, env) => match &self.program.terms[id] {
                    Term::Var(index) => {
                        self.step(Step::Var)?;
                        let index = *index;
                        let value = self.lookup(env, index)?.ok_or(RuntimeError::OpenTerm {
                            index,
                            environment_size: self.environment_length(env),
                        })?;
                        State::Return(value)
                    }
                    Term::Constant(_) => {
                        self.step(Step::Constant)?;
                        State::Return(self.value(Value::Constant(id))?)
                    }
                    Term::Lambda(body) => {
                        self.step(Step::Lambda)?;
                        State::Return(self.value(Value::Lambda { body: *body, env })?)
                    }
                    Term::Delay(body) => {
                        self.step(Step::Delay)?;
                        State::Return(self.value(Value::Delay { body: *body, env })?)
                    }
                    Term::Force(body) => {
                        self.step(Step::Force)?;
                        let body = *body;
                        self.frame(Frame::Force)?;
                        State::Compute(body, env)
                    }
                    Term::Apply { function, argument } => {
                        self.step(Step::Apply)?;
                        let function = *function;
                        let argument = *argument;
                        self.frame(Frame::Argument {
                            term: argument,
                            env,
                        })?;
                        State::Compute(function, env)
                    }
                    Term::Error => return Err(RuntimeError::ExplicitError.into()),
                    Term::Builtin(_) => {
                        return Err(RuntimeError::Unsupported(
                            "builtin runtime is not implemented yet".into(),
                        )
                        .into());
                    }
                },
                State::Return(value) => match self.frames.pop() {
                    None => {
                        self.flush()?;
                        return Ok(value);
                    }
                    Some(Frame::Argument { term, env }) => {
                        // The caller's environment belongs to the argument.
                        // Even a non-function must evaluate its argument first.
                        self.frame(Frame::Function(value))?;
                        State::Compute(term, env)
                    }
                    Some(Frame::Function(function)) => match self.values[function] {
                        Value::Lambda { body, env } => {
                            let env = self.extend(env, value)?;
                            State::Compute(body, Some(env))
                        }
                        _ => return Err(RuntimeError::NonFunctionApplication.into()),
                    },
                    Some(Frame::Force) => match self.values[value] {
                        Value::Delay { body, env } => State::Compute(body, env),
                        _ => return Err(RuntimeError::NonDelayForce.into()),
                    },
                },
            };
        }
    }

    fn step(&mut self, step: Step) -> Result<(), BudgetError> {
        let index = match step {
            Step::Constant => 0,
            Step::Var => 1,
            Step::Lambda => 2,
            Step::Apply => 3,
            Step::Delay => 4,
            Step::Force => 5,
            Step::Startup => unreachable!("startup is charged before machine construction"),
        };
        self.event_counts[index] += 1;
        self.pending_events += 1;
        if self.pending_events == SLIPPAGE {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BudgetError> {
        let counts = std::mem::take(&mut self.event_counts);
        self.pending_events = 0;
        for (step, count) in CHARGE_ORDER.into_iter().zip(counts) {
            self.meter.charge_repeated(self.costs.cost(step), count)?;
        }
        Ok(())
    }

    fn work(&mut self) -> Result<(), RuntimeError> {
        self.work_left = self
            .work_left
            .checked_sub(1)
            .ok_or_else(|| RuntimeError::Unsupported("machine work bound exceeded".into()))?;
        Ok(())
    }

    fn reserve_entry(&self) -> Result<(), RuntimeError> {
        if self.values.len() + self.environments.len() + self.frames.len() >= self.entry_limit {
            return Err(RuntimeError::Unsupported(
                "runtime value/environment/frame arena bound exceeded".into(),
            ));
        }
        Ok(())
    }

    fn value(&mut self, value: Value) -> Result<ValueId, RuntimeError> {
        self.reserve_entry()?;
        let id = self.values.len();
        self.values.push(value);
        Ok(id)
    }

    fn frame(&mut self, frame: Frame) -> Result<(), RuntimeError> {
        self.reserve_entry()?;
        self.frames.push(frame);
        Ok(())
    }

    fn extend(&mut self, env: Environment, value: ValueId) -> Result<EnvId, RuntimeError> {
        self.reserve_entry()?;
        let id = self.environments.len();
        self.environments.push(EnvEntry {
            value,
            parent: env,
            length: self.environment_length(env) + 1,
        });
        Ok(id)
    }

    fn environment_length(&self, env: Environment) -> usize {
        env.map_or(0, |id| self.environments[id].length)
    }

    fn lookup(
        &mut self,
        mut env: Environment,
        index: u64,
    ) -> Result<Option<ValueId>, RuntimeError> {
        if index == 0 || index > self.environment_length(env) as u64 {
            return Ok(None);
        }
        // The length comparison also makes conversion independent of host width.
        for _ in 1..index {
            self.work()?;
            env = self.environments[env.expect("validated environment index")].parent;
        }
        Ok(Some(
            self.environments[env.expect("nonzero in-range environment index")].value,
        ))
    }

    fn discharge(&mut self, value: ValueId) -> Result<Program, RuntimeError> {
        let mut output = DischargeOutput::default();
        let mut pending = vec![Discharge::Value { value, depth: 0 }];
        while let Some(task) = pending.pop() {
            self.work()?;
            match task {
                Discharge::Value { value, depth } => match self.values[value] {
                    Value::Constant(term) => pending.push(Discharge::Term {
                        term,
                        env: None,
                        binders: 0,
                        depth,
                    }),
                    Value::Lambda { body, env } => {
                        output.visit(depth)?;
                        pending.push(Discharge::Finish(Parent::Lambda));
                        pending.push(Discharge::Term {
                            term: body,
                            env,
                            binders: 1,
                            depth: depth + 1,
                        });
                    }
                    Value::Delay { body, env } => {
                        output.visit(depth)?;
                        pending.push(Discharge::Finish(Parent::Delay));
                        pending.push(Discharge::Term {
                            term: body,
                            env,
                            binders: 0,
                            depth: depth + 1,
                        });
                    }
                },
                Discharge::Term {
                    term,
                    env,
                    binders,
                    depth,
                } => {
                    let term = &self.program.terms[term];
                    if let Term::Var(index) = term
                        && *index > binders
                        && let Some(value) = self.lookup(env, *index - binders)?
                    {
                        // Begin the substituted value with its own environment
                        // and binder depth. Unbound source indices stay intact,
                        // as in the official De Bruijn result discharge.
                        pending.push(Discharge::Value { value, depth });
                        continue;
                    }
                    output.visit(depth)?;
                    match term {
                        Term::Var(index) => output.push(Term::Var(*index)),
                        Term::Constant(constant) => {
                            output.copy_constant(constant)?;
                        }
                        Term::Error => output.push(Term::Error),
                        Term::Builtin(builtin) => output.push(Term::Builtin(*builtin)),
                        Term::Lambda(body) | Term::Delay(body) | Term::Force(body) => {
                            let (parent, binders) = match term {
                                Term::Lambda(_) => (Parent::Lambda, binders + 1),
                                Term::Delay(_) => (Parent::Delay, binders),
                                Term::Force(_) => (Parent::Force, binders),
                                _ => unreachable!(),
                            };
                            pending.push(Discharge::Finish(parent));
                            pending.push(Discharge::Term {
                                term: *body,
                                env,
                                binders,
                                depth: depth + 1,
                            });
                        }
                        Term::Apply { function, argument } => {
                            pending.push(Discharge::Finish(Parent::Apply));
                            pending.push(Discharge::Term {
                                term: *argument,
                                env,
                                binders,
                                depth: depth + 1,
                            });
                            pending.push(Discharge::Term {
                                term: *function,
                                env,
                                binders,
                                depth: depth + 1,
                            });
                        }
                    }
                }
                Discharge::Finish(parent) => output.finish(parent),
            }
        }
        Ok(Program {
            version: self.program.version,
            root: output.stack.pop().expect("discharged result"),
            terms: output.terms,
        })
    }
}

enum Parent {
    Lambda,
    Delay,
    Force,
    Apply,
}

enum Discharge {
    Value {
        value: ValueId,
        depth: usize,
    },
    Term {
        term: TermId,
        env: Environment,
        binders: u64,
        depth: usize,
    },
    Finish(Parent),
}

#[derive(Default)]
struct DischargeOutput {
    terms: Vec<Term>,
    stack: Vec<TermId>,
    nodes: usize,
    payload_bytes: usize,
}

impl DischargeOutput {
    fn visit(&mut self, depth: usize) -> Result<(), RuntimeError> {
        self.nodes += 1;
        if self.nodes > MAX_OUTPUT_NODES {
            return Err(RuntimeError::Unsupported(format!(
                "discharged result exceeds {MAX_OUTPUT_NODES} nodes"
            )));
        }
        if depth > MAX_OUTPUT_DEPTH {
            return Err(RuntimeError::Unsupported(format!(
                "discharged result depth exceeds {MAX_OUTPUT_DEPTH}"
            )));
        }
        Ok(())
    }

    fn push(&mut self, term: Term) {
        self.stack.push(self.terms.len());
        self.terms.push(term);
    }

    fn copy_constant(&mut self, constant: &Constant) -> Result<(), RuntimeError> {
        let bytes = match constant {
            Constant::Integer(integer) => integer.bits().div_ceil(8) as usize,
            Constant::String(string) => string.len(),
            Constant::ByteString(bytes) => bytes.len(),
            Constant::Bool(_) | Constant::Unit => 0,
        };
        // Bounds are checked before copying. The sum cannot overflow because
        // each source payload and the prior sum have already been bounded.
        self.payload_bytes += bytes;
        if self.payload_bytes > MAX_OUTPUT_BYTES {
            return Err(RuntimeError::Unsupported(format!(
                "discharged constant payload exceeds {MAX_OUTPUT_BYTES} bytes"
            )));
        }
        self.push(Term::Constant(constant.clone()));
        Ok(())
    }

    fn finish(&mut self, parent: Parent) {
        let last = self.stack.pop().expect("discharged child");
        let term = match parent {
            Parent::Lambda => Term::Lambda(last),
            Parent::Delay => Term::Delay(last),
            Parent::Force => Term::Force(last),
            Parent::Apply => Term::Apply {
                function: self.stack.pop().expect("discharged function"),
                argument: last,
            },
        };
        self.push(term);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cost::PARAMETER_COUNT, limits::MAX_CONSTANT_BYTES};
    use serde_json::{Value as Json, json};

    const UNLIMITED: ExecutionBudget = ExecutionBudget {
        cpu: i64::MAX,
        mem: i64::MAX,
    };

    fn program(terms: impl Into<Vec<Term>>, root: TermId) -> Program {
        Program::new([1, 0, 0], terms.into(), root).unwrap()
    }

    fn integer(value: i64) -> Term {
        Term::Constant(Constant::Integer(value.into()))
    }

    fn apply(function: TermId, argument: TermId) -> Term {
        Term::Apply { function, argument }
    }

    fn model() -> MachineCosts {
        let profile: Json =
            serde_json::from_str(include_str!("../../../profiles/plutus-v3-pv11.json")).unwrap();
        let parameters = profile["cost_model"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().parse().unwrap())
            .collect::<Vec<i64>>();
        MachineCosts::from_parameters(&parameters).unwrap()
    }

    fn run(program: &Program) -> Evaluation {
        evaluate(program, &model(), UNLIMITED)
    }

    fn result(evaluation: Evaluation) -> Json {
        evaluation.result.unwrap().normalize().unwrap()
    }

    fn consumed(events: i64) -> Option<ExecutionBudget> {
        Some(ExecutionBudget {
            cpu: 100 + 16_000 * events,
            mem: 100 + 100 * events,
        })
    }

    #[test]
    fn constants_preserve_arbitrary_precision_and_primitive_structure() {
        let decimal = "-123456789012345678901234567890123456789012345678901234567890";
        for (constant, expected) in [
            (
                Constant::Integer(decimal.parse().unwrap()),
                json!(["integer", decimal]),
            ),
            (Constant::ByteString(vec![0, 255]), json!(["bytes", "00ff"])),
            (
                Constant::String("\0\"\\\nλ🙂".into()),
                json!(["string", "\0\"\\\nλ🙂"]),
            ),
            (Constant::Unit, json!(["unit"])),
            (Constant::Bool(false), json!(["bool", false])),
            (Constant::Bool(true), json!(["bool", true])),
        ] {
            let evaluation = run(&program([Term::Constant(constant)], 0));
            assert_eq!(evaluation.consumed, consumed(1));
            assert_eq!(result(evaluation), json!(["constant", expected]));
        }
    }

    #[test]
    fn identity_and_shadowed_variables_are_one_based() {
        // ((lambda x. lambda x. x) 1) 2 = 2.
        let ast = program(
            [
                Term::Var(1),
                Term::Lambda(0),
                Term::Lambda(1),
                integer(1),
                apply(2, 3),
                integer(2),
                apply(4, 5),
            ],
            6,
        );
        let evaluation = run(&ast);
        assert_eq!(evaluation.consumed, consumed(7));
        assert_eq!(result(evaluation), json!(["constant", ["integer", "2"]]));
    }

    #[test]
    fn lexical_closure_uses_capture_instead_of_callers_shadow() {
        // (lambda x. (lambda f. (lambda x. f unit) 2) (lambda _. x)) 1.
        let ast = program(
            [
                Term::Var(2),
                Term::Lambda(0),
                Term::Constant(Constant::Unit),
                apply(0, 2),
                Term::Lambda(3),
                integer(2),
                apply(4, 5),
                Term::Lambda(6),
                apply(7, 1),
                Term::Lambda(8),
                integer(1),
                apply(9, 10),
            ],
            11,
        );
        assert_eq!(result(run(&ast)), json!(["constant", ["integer", "1"]]));
    }

    #[test]
    fn function_evaluation_does_not_replace_argument_environment() {
        // (lambda x. ((lambda x. lambda y. y) 10) x) 20.
        let ast = program(
            [
                Term::Var(1),
                Term::Lambda(0),
                Term::Lambda(1),
                integer(10),
                apply(2, 3),
                apply(4, 0),
                Term::Lambda(5),
                integer(20),
                apply(6, 7),
            ],
            8,
        );
        let evaluation = run(&ast);
        assert_eq!(evaluation.consumed, consumed(10));
        assert_eq!(result(evaluation), json!(["constant", ["integer", "20"]]));
    }

    #[test]
    fn force_uses_the_delays_captured_environment() {
        // (lambda x. (lambda d. (lambda x. force d) 20) (delay x)) 10.
        let ast = program(
            [
                Term::Var(2),
                Term::Force(0),
                Term::Lambda(1),
                integer(20),
                apply(2, 3),
                Term::Lambda(4),
                Term::Var(1),
                Term::Delay(6),
                apply(5, 7),
                Term::Lambda(8),
                integer(10),
                apply(9, 10),
            ],
            11,
        );
        assert_eq!(result(run(&ast)), json!(["constant", ["integer", "10"]]));
    }

    #[test]
    fn lambda_and_delay_bodies_are_not_evaluated_or_reduced() {
        for (ast, expected) in [
            (
                program([Term::Error, Term::Delay(0)], 1),
                json!(["delay", ["error"]]),
            ),
            (
                program([integer(42), Term::Force(0), Term::Lambda(1)], 2),
                json!(["lambda", ["force", ["constant", ["integer", "42"]]]]),
            ),
        ] {
            let evaluation = run(&ast);
            assert_eq!(evaluation.consumed, consumed(1));
            assert_eq!(result(evaluation), expected);
        }
    }

    #[test]
    fn discharge_substitutes_captures_below_several_binders() {
        // (lambda x. lambda y. lambda z. (x y) z) 42.
        let ast = program(
            [
                Term::Var(3),
                Term::Var(2),
                apply(0, 1),
                Term::Var(1),
                apply(2, 3),
                Term::Lambda(4),
                Term::Lambda(5),
                Term::Lambda(6),
                integer(42),
                apply(7, 8),
            ],
            9,
        );
        let evaluation = run(&ast);
        assert_eq!(evaluation.consumed, consumed(4));
        assert_eq!(
            result(evaluation),
            json!([
                "lambda",
                [
                    "lambda",
                    [
                        "apply",
                        ["apply", ["constant", ["integer", "42"]], ["var", "2"]],
                        ["var", "1"]
                    ]
                ]
            ])
        );
    }

    #[test]
    fn discharge_of_substituted_closure_uses_its_own_binder_depth() {
        // (lambda x. (lambda f. lambda z. f) (lambda y. x)) 42.
        let ast = program(
            [
                Term::Var(2),
                Term::Lambda(0),
                Term::Lambda(1),
                apply(2, 1),
                Term::Lambda(3),
                integer(42),
                apply(4, 5),
            ],
            6,
        );
        assert_eq!(
            result(run(&ast)),
            json!(["lambda", ["lambda", ["constant", ["integer", "42"]]]])
        );
    }

    #[test]
    fn discharge_of_substituted_delay_retains_unevaluated_body() {
        // (lambda x. (lambda d. lambda y. d) (delay (force x))) 42.
        let ast = program(
            [
                Term::Var(2),
                Term::Lambda(0),
                Term::Lambda(1),
                Term::Var(1),
                Term::Force(3),
                Term::Delay(4),
                apply(2, 5),
                Term::Lambda(6),
                integer(42),
                apply(7, 8),
            ],
            9,
        );
        assert_eq!(
            result(run(&ast)),
            json!([
                "lambda",
                ["delay", ["force", ["constant", ["integer", "42"]]]]
            ])
        );
    }

    #[test]
    fn discharge_leaves_genuinely_free_indices_unchanged() {
        // The official De Bruijn discharge does not rebase a free index when
        // its attempted lookup lies beyond the captured environment.
        for index in [3, u64::MAX] {
            let ast = program(
                [
                    Term::Var(index),
                    Term::Lambda(0),
                    Term::Lambda(1),
                    integer(42),
                    apply(2, 3),
                ],
                4,
            );
            assert_eq!(
                result(run(&ast)),
                json!(["lambda", ["var", index.to_string()]])
            );
        }
    }

    #[test]
    fn zero_and_out_of_range_indices_are_runtime_open_terms() {
        for index in [0, 1, u64::MAX] {
            let evaluation = run(&program([Term::Var(index)], 0));
            assert_eq!(evaluation.consumed, consumed(0));
            assert_eq!(
                evaluation.result,
                Err(MachineError::Runtime(RuntimeError::OpenTerm {
                    index,
                    environment_size: 0,
                }))
            );
        }
        let ast = program([Term::Var(2), Term::Lambda(0), integer(42), apply(1, 2)], 3);
        assert_eq!(
            run(&ast).result,
            Err(MachineError::Runtime(RuntimeError::OpenTerm {
                index: 2,
                environment_size: 1,
            }))
        );
    }

    #[test]
    fn application_computes_function_then_argument_before_checking_function_kind() {
        for (terms, expected) in [
            (
                [Term::Error, Term::Var(1), apply(0, 1)],
                RuntimeError::ExplicitError,
            ),
            (
                [integer(0), Term::Error, apply(0, 1)],
                RuntimeError::ExplicitError,
            ),
            (
                [integer(0), Term::Var(1), apply(0, 1)],
                RuntimeError::OpenTerm {
                    index: 1,
                    environment_size: 0,
                },
            ),
            (
                [integer(0), integer(1), apply(0, 1)],
                RuntimeError::NonFunctionApplication,
            ),
        ] {
            let evaluation = run(&program(terms, 2));
            assert_eq!(evaluation.result, Err(MachineError::Runtime(expected)));
            assert_eq!(evaluation.consumed, consumed(0));
        }
    }

    #[test]
    fn function_argument_is_evaluated_even_when_binder_is_unused() {
        let ast = program([integer(42), Term::Lambda(0), Term::Error, apply(1, 2)], 3);
        let evaluation = run(&ast);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(evaluation.consumed, consumed(0));
    }

    #[test]
    fn force_distinguishes_delays_from_other_values_and_evaluated_errors() {
        for (ast, expected) in [
            (
                program([Term::Error, Term::Delay(0), Term::Force(1)], 2),
                RuntimeError::ExplicitError,
            ),
            (
                program([Term::Error, Term::Force(0)], 1),
                RuntimeError::ExplicitError,
            ),
            (
                program([Term::Error, Term::Lambda(0), Term::Force(1)], 2),
                RuntimeError::NonDelayForce,
            ),
            (
                program([integer(42), Term::Force(0)], 1),
                RuntimeError::NonDelayForce,
            ),
        ] {
            let evaluation = run(&ast);
            assert_eq!(evaluation.result, Err(MachineError::Runtime(expected)));
            assert_eq!(evaluation.consumed, consumed(0));
        }
    }

    fn every_event_program() -> Program {
        // force ((lambda x. delay x) 42) visits each event exactly once.
        program(
            [
                Term::Var(1),
                Term::Delay(0),
                Term::Lambda(1),
                integer(42),
                apply(2, 3),
                Term::Force(4),
            ],
            5,
        )
    }

    #[test]
    fn successful_totals_include_exactly_startup_and_visited_compute_events() {
        let evaluation = run(&every_event_program());
        assert_eq!(evaluation.consumed, consumed(6));
        assert_eq!(result(evaluation), json!(["constant", ["integer", "42"]]));
        let mut parameters = vec![0; PARAMETER_COUNT];
        for (index, cpu, mem) in [
            (29, 1, 2),
            (31, 4, 8),
            (21, 16, 32),
            (27, 64, 128),
            (17, 256, 512),
            (23, 1024, 2048),
            (25, 4096, 8192),
        ] {
            parameters[index] = cpu;
            parameters[index + 1] = mem;
        }
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        let evaluation = evaluate(&every_event_program(), &costs, UNLIMITED);
        assert_eq!(
            evaluation.consumed,
            Some(ExecutionBudget {
                cpu: 5461,
                mem: 10922,
            })
        );
        assert_eq!(result(evaluation), json!(["constant", ["integer", "42"]]));
        // Prefix budgets independently verify the official batch charging
        // order, which differs from this program's compute traversal order.
        let mut prefix = ExecutionBudget { cpu: 0, mem: 0 };
        for (cpu, mem) in [
            (1, 2),
            (16, 32),
            (4, 8),
            (64, 128),
            (256, 512),
            (1024, 2048),
            (4096, 8192),
        ] {
            prefix.cpu += cpu;
            prefix.mem += mem;
            for budget in [
                ExecutionBudget {
                    cpu: prefix.cpu - 1,
                    mem: i64::MAX,
                },
                ExecutionBudget {
                    cpu: i64::MAX,
                    mem: prefix.mem - 1,
                },
            ] {
                let evaluation = evaluate(&every_event_program(), &costs, budget);
                assert_eq!(
                    evaluation.result,
                    Err(MachineError::Budget(BudgetError::Exhausted))
                );
                assert_eq!(evaluation.consumed, Some(prefix));
            }
        }
    }

    #[test]
    fn exact_budgets_succeed_and_each_dimension_one_below_fails() {
        let ast = every_event_program();
        let required = ExecutionBudget {
            cpu: 96100,
            mem: 700,
        };
        assert!(evaluate(&ast, &model(), required).result.is_ok());
        for limit in [
            ExecutionBudget {
                cpu: required.cpu - 1,
                ..required
            },
            ExecutionBudget {
                mem: required.mem - 1,
                ..required
            },
        ] {
            let evaluation = evaluate(&ast, &model(), limit);
            assert_eq!(
                evaluation.result,
                Err(MachineError::Budget(BudgetError::Exhausted))
            );
            assert_eq!(evaluation.consumed, Some(required));
        }
    }

    #[test]
    fn zero_budget_charges_startup_and_error_has_no_own_charge() {
        let ast = program([Term::Error], 0);
        let evaluation = evaluate(&ast, &model(), ExecutionBudget { cpu: 0, mem: 0 });
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
        assert_eq!(evaluation.consumed, consumed(0));
        let evaluation = evaluate(&ast, &model(), ExecutionBudget { cpu: 100, mem: 100 });
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(evaluation.consumed, consumed(0));
    }

    #[test]
    fn semantic_failure_does_not_flush_a_pending_batch() {
        // Official CEK slippage means this semantic failure happens before
        // the force charge is submitted, even with no budget left at startup.
        let ast = program([Term::Error, Term::Force(0)], 1);
        let evaluation = evaluate(&ast, &model(), ExecutionBudget { cpu: 100, mem: 100 });
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(evaluation.consumed, consumed(0));
    }

    #[test]
    fn two_hundredth_compute_event_flushes_before_its_action() {
        // 199 compute events followed by Error retain only startup. At event
        // 200 the pending batch is charged, before evaluation reaches Error.
        let mut below = vec![Term::Error, Term::Force(0)];
        let mut root = 1;
        for _ in 0..99 {
            below.push(Term::Delay(root));
            below.push(Term::Force(root + 1));
            root += 2;
        }
        let startup_only = ExecutionBudget { cpu: 100, mem: 100 };
        let evaluation = evaluate(&program(below, root), &model(), startup_only);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(evaluation.consumed, consumed(0));

        let mut at_limit = vec![Term::Error];
        let mut root = 0;
        for _ in 0..100 {
            at_limit.push(Term::Delay(root));
            at_limit.push(Term::Force(root + 1));
            root += 2;
        }
        let ast = program(at_limit, root);
        let evaluation = evaluate(&ast, &model(), startup_only);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
        // Delay is earlier than force in batch order, so its whole 100-event
        // charge is recorded and the force category is not attempted.
        assert_eq!(evaluation.consumed, consumed(100));
        let evaluation = run(&ast);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(evaluation.consumed, consumed(200));

        // An additional event after a full batch remains uncharged when Error
        // is reached. The prior batch's consumption is retained exactly.
        let mut after = vec![Term::Error, Term::Force(0)];
        let mut root = 1;
        for _ in 0..100 {
            after.push(Term::Delay(root));
            after.push(Term::Force(root + 1));
            root += 2;
        }
        let evaluation = run(&program(after, root));
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(evaluation.consumed, consumed(200));
    }

    #[test]
    fn successful_evaluation_flushes_multiple_batches_and_the_final_remainder() {
        // 400 delay/force events fill two batches; the final constant fills
        // neither and is charged by the successful-termination flush.
        let mut terms = vec![integer(42)];
        let mut root = 0;
        for _ in 0..200 {
            terms.push(Term::Delay(root));
            terms.push(Term::Force(root + 1));
            root += 2;
        }
        let ast = program(terms, root);
        let required = consumed(401).unwrap();
        let evaluation = evaluate(&ast, &model(), required);
        assert_eq!(evaluation.consumed, Some(required));
        assert_eq!(result(evaluation), json!(["constant", ["integer", "42"]]));
        for limit in [
            ExecutionBudget {
                cpu: required.cpu - 1,
                ..required
            },
            ExecutionBudget {
                mem: required.mem - 1,
                ..required
            },
        ] {
            let evaluation = evaluate(&ast, &model(), limit);
            assert_eq!(
                evaluation.result,
                Err(MachineError::Budget(BudgetError::Exhausted))
            );
            assert_eq!(evaluation.consumed, Some(required));
        }
    }

    #[test]
    fn overflow_exhausts_budget_without_wrapping_or_reporting_false_totals() {
        for (startup_cpu, startup_mem, constant_cpu, constant_mem) in
            [(i64::MAX, 0, 1, 0), (0, i64::MAX, 0, 1)]
        {
            let mut parameters = vec![0; PARAMETER_COUNT];
            parameters[29] = startup_cpu;
            parameters[30] = startup_mem;
            parameters[21] = constant_cpu;
            parameters[22] = constant_mem;
            let costs = MachineCosts::from_parameters(&parameters).unwrap();
            let evaluation = evaluate(&program([integer(1)], 0), &costs, UNLIMITED);
            assert_eq!(
                evaluation.result,
                Err(MachineError::Budget(BudgetError::Overflow))
            );
            assert_eq!(evaluation.consumed, None);
        }
    }

    #[test]
    fn versions_are_preserved_and_invalid_ast_is_rejected_without_charge() {
        for version in [[1, 0, 0], [1, 1, 0]] {
            let ast = Program::new(version, vec![integer(42)], 0).unwrap();
            assert_eq!(run(&ast).result.unwrap().version, version);
        }
        for ast in [
            Program {
                version: [1, 0, 0],
                terms: vec![Term::Delay(0)],
                root: 0,
            },
            Program {
                version: [1, 0, 0],
                terms: vec![integer(42)],
                root: 1,
            },
            Program {
                version: [1, 2, 0],
                terms: vec![integer(42)],
                root: 0,
            },
        ] {
            let evaluation = run(&ast);
            assert!(matches!(
                evaluation.result,
                Err(MachineError::InvalidProgram(_))
            ));
            assert_eq!(
                evaluation.consumed,
                Some(ExecutionBudget { cpu: 0, mem: 0 })
            );
        }
    }

    fn omega() -> Program {
        // (lambda x. x x) (lambda x. x x).
        program([Term::Var(1), apply(0, 0), Term::Lambda(1), apply(2, 2)], 3)
    }

    #[test]
    fn zero_cost_nontermination_has_explicit_work_and_allocation_bounds() {
        let costs = MachineCosts::from_parameters(&vec![0; PARAMETER_COUNT]).unwrap();
        let budget = ExecutionBudget { cpu: 0, mem: 0 };
        for (resources, expected) in [
            (
                ResourceLimits {
                    work: 30,
                    entries: MAX_RUNTIME_ENTRIES,
                },
                "work bound",
            ),
            (
                ResourceLimits {
                    work: MAX_MACHINE_STEPS,
                    entries: 30,
                },
                "arena bound",
            ),
        ] {
            let evaluation = evaluate_with_limits(&omega(), &costs, budget, resources);
            assert!(matches!(
                evaluation.result,
                Err(MachineError::Runtime(RuntimeError::Unsupported(reason)))
                    if reason.contains(expected)
            ));
            assert_eq!(evaluation.consumed, Some(budget));
        }
        // A normal nonzero model stops at its restricting budget instead.
        let evaluation = evaluate(
            &omega(),
            &model(),
            ExecutionBudget {
                cpu: 100_000,
                mem: 1_000,
            },
        );
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
    }

    #[test]
    fn resource_limits_apply_to_values_frames_and_environment_cells() {
        for (ast, limit) in [
            (program([integer(42)], 0), 0),
            (program([integer(42), Term::Force(0), Term::Force(1)], 2), 1),
            (
                program([Term::Var(1), Term::Lambda(0), integer(42), apply(1, 2)], 3),
                2,
            ),
        ] {
            let evaluation = evaluate_with_limits(
                &ast,
                &model(),
                UNLIMITED,
                ResourceLimits {
                    work: MAX_MACHINE_STEPS,
                    entries: limit,
                },
            );
            assert!(matches!(
                evaluation.result,
                Err(MachineError::Runtime(RuntimeError::Unsupported(reason)))
                    if reason.contains("arena bound")
            ));
        }
    }

    #[test]
    fn discharge_bounds_expansion_and_payload_before_unbounded_allocation() {
        let mut terms = vec![Term::Error];
        for child in 0..17 {
            terms.push(apply(child, child));
        }
        terms.push(Term::Delay(17));
        let evaluation = run(&program(terms, 18));
        assert!(matches!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::Unsupported(reason)))
                if reason.contains("nodes")
        ));
        assert_eq!(evaluation.consumed, consumed(1));

        let mut terms = vec![Term::Constant(Constant::String(
            "x".repeat(MAX_CONSTANT_BYTES),
        ))];
        for child in 0..4 {
            terms.push(apply(child, child));
        }
        terms.push(Term::Delay(4));
        assert!(matches!(
            run(&program(terms, 5)).result,
            Err(MachineError::Runtime(RuntimeError::Unsupported(reason)))
                if reason.contains("payload")
        ));
    }

    #[test]
    fn discharge_depth_limit_is_iterative_and_exact() {
        let mut terms = vec![Term::Error];
        for child in 0..MAX_OUTPUT_DEPTH {
            terms.push(Term::Lambda(child));
        }
        let evaluation = run(&program(terms.clone(), MAX_OUTPUT_DEPTH));
        assert_eq!(evaluation.consumed, consumed(1));
        assert!(evaluation.result.unwrap().normalize().is_ok());
        terms.push(Term::Lambda(MAX_OUTPUT_DEPTH));
        assert!(matches!(
            run(&program(terms, MAX_OUTPUT_DEPTH + 1)).result,
            Err(MachineError::Runtime(RuntimeError::Unsupported(reason)))
                if reason.contains("depth")
        ));
    }
}
