//! CEK probes derived from the pinned builtin signatures/transition rules and
//! hand-computed profile equations. These tests never query an evaluator oracle.

use super::*;
use crate::{
    builtin::{ArgumentType, CARDANO_INTEGER_MAXIMUM_BITS},
    cost::PARAMETER_COUNT,
};
use num_bigint::BigInt;
use serde_json::{Value as Json, json};

const UNLIMITED: ExecutionBudget = ExecutionBudget {
    cpu: i64::MAX,
    mem: i64::MAX,
};
const ZERO: ExecutionBudget = ExecutionBudget { cpu: 0, mem: 0 };
const DIVISION: [Builtin; 4] = [
    Builtin::DivideInteger,
    Builtin::QuotientInteger,
    Builtin::RemainderInteger,
    Builtin::ModInteger,
];

#[derive(Default)]
struct Ast(Vec<Term>);

impl Ast {
    fn term(&mut self, term: Term) -> TermId {
        let id = self.0.len();
        self.0.push(term);
        id
    }

    fn constant(&mut self, constant: Constant) -> TermId {
        self.term(Term::Constant(constant))
    }

    fn integer(&mut self, integer: impl Into<BigInt>) -> TermId {
        self.constant(Constant::Integer(integer.into()))
    }

    fn apply(&mut self, function: TermId, argument: TermId) -> TermId {
        self.term(Term::Apply { function, argument })
    }

    fn call(&mut self, builtin: Builtin, arguments: &[TermId]) -> TermId {
        let mut root = self.term(Term::Builtin(builtin));
        for _ in 0..builtin.force_count() {
            root = self.term(Term::Force(root));
        }
        for argument in arguments {
            root = self.apply(root, *argument);
        }
        root
    }

    fn wrap(&mut self, mut root: TermId, count: usize) -> TermId {
        for _ in 0..count {
            root = self.term(Term::Delay(root));
            root = self.term(Term::Force(root));
        }
        root
    }

    fn program(self, root: TermId) -> Program {
        Program::new([1, 1, 0], self.0, root).unwrap()
    }
}

fn model() -> MachineCosts {
    let profile: Json =
        serde_json::from_str(include_str!("../../../../profiles/plutus-v3-pv11.json")).unwrap();
    MachineCosts::from_parameters(
        &profile["cost_model"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().parse().unwrap())
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn custom(updates: &[(usize, i64)]) -> MachineCosts {
    let mut parameters = vec![0; PARAMETER_COUNT];
    for (index, value) in updates {
        parameters[*index] = *value;
    }
    MachineCosts::from_parameters(&parameters).unwrap()
}

fn run(program: &Program) -> Evaluation {
    evaluate(program, &model(), UNLIMITED)
}

fn term(evaluation: Evaluation) -> Json {
    evaluation.result.unwrap().normalize().unwrap()
}

fn constant(value: i64) -> Json {
    json!(["constant", ["integer", value.to_string()]])
}

fn binary(builtin: Builtin, left: impl Into<BigInt>, right: impl Into<BigInt>) -> Program {
    let mut ast = Ast::default();
    let left = ast.integer(left);
    let right = ast.integer(right);
    let root = ast.call(builtin, &[left, right]);
    ast.program(root)
}

fn is_unsupported(evaluation: &Evaluation, message: &str) {
    assert!(
        matches!(
            &evaluation.result,
            Err(MachineError::Runtime(RuntimeError::Unsupported(reason))) if reason.contains(message)
        ),
        "{:?}",
        evaluation.result
    );
}

#[test]
fn every_supported_builtin_has_an_independent_success_and_exact_cost() {
    // Five CEK events = 80100/600 including startup. The formulas below
    // substitute x=y=1 in the pinned 350-entry development coefficient vector.
    for (builtin, expected, cpu, mem) in [
        (Builtin::AddInteger, constant(3), 181_308, 602),
        (Builtin::SubtractInteger, constant(-1), 181_308, 602),
        (Builtin::MultiplyInteger, constant(2), 171_053, 602),
        (
            Builtin::EqualsInteger,
            json!(["constant", ["bool", false]]),
            132_433,
            601,
        ),
        (
            Builtin::LessThanInteger,
            json!(["constant", ["bool", true]]),
            125_390,
            601,
        ),
        (
            Builtin::LessThanEqualsInteger,
            json!(["constant", ["bool", true]]),
            123_937,
            601,
        ),
    ] {
        let result = run(&binary(builtin, 1, 2));
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget { cpu, mem }),
            "{builtin}"
        );
        assert_eq!(term(result), expected, "{builtin}");
    }
    let mut ast = Ast::default();
    let condition = ast.constant(Constant::Bool(false));
    let left = ast.integer(1);
    let right = ast.integer(2);
    let root = ast.call(Builtin::IfThenElse, &[condition, left, right]);
    let result = run(&ast.program(root));
    // Eight CEK events plus 76049/1, including the required successful force.
    assert_eq!(
        result.consumed,
        Some(ExecutionBudget {
            cpu: 204_149,
            mem: 901
        })
    );
    assert_eq!(term(result), constant(2));
}

#[test]
fn integer_sign_zero_word_boundaries_and_large_results_preserve_exactness() {
    let big: BigInt = "18446744073709551616".parse().unwrap();
    for (builtin, left, right, expected, cpu, mem) in [
        (
            Builtin::AddInteger,
            -&big,
            BigInt::from(1),
            "-18446744073709551615",
            181_728,
            603,
        ),
        (
            Builtin::SubtractInteger,
            big.clone(),
            BigInt::from(-1),
            "18446744073709551617",
            181_728,
            603,
        ),
        (
            Builtin::MultiplyInteger,
            big.clone(),
            -&big,
            "-340282366920938463463374607431768211456",
            172_610,
            604,
        ),
        (
            Builtin::MultiplyInteger,
            BigInt::from(0),
            big.clone(),
            "0",
            171_572,
            603,
        ),
    ] {
        let result = run(&binary(builtin, left, right));
        assert_eq!(result.consumed, Some(ExecutionBudget { cpu, mem }));
        assert_eq!(term(result), json!(["constant", ["integer", expected]]));
    }
    let precise: BigInt = "9007199254740993".parse().unwrap();
    assert_eq!(
        term(run(&binary(Builtin::AddInteger, precise, 2))),
        json!(["constant", ["integer", "9007199254740995"]])
    );
    for power in [31, 32, 63, 64, 127, 128, 1024] {
        let value: BigInt = BigInt::from(1) << power;
        for signed in [value.clone(), -value] {
            let result = run(&binary(Builtin::SubtractInteger, signed.clone(), signed));
            assert_eq!(term(result), constant(0));
        }
    }
}

#[test]
fn bare_forced_and_partially_applied_builtins_discharge_as_values() {
    for builtin in [Builtin::AddInteger, Builtin::IfThenElse] {
        let mut ast = Ast::default();
        let root = ast.term(Term::Builtin(builtin));
        let result = run(&ast.program(root));
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget {
                cpu: 16_100,
                mem: 200
            })
        );
        assert_eq!(term(result), json!(["builtin", builtin.tag().to_string()]));
    }
    for (builtin, argument, expected) in [
        (
            Builtin::AddInteger,
            Constant::Integer(42.into()),
            json!(["apply", ["builtin", "0"], constant(42)]),
        ),
        // A wrong-typed first argument is not unlifted until saturation.
        (
            Builtin::AddInteger,
            Constant::Bool(true),
            json!(["apply", ["builtin", "0"], ["constant", ["bool", true]]]),
        ),
        (
            Builtin::IfThenElse,
            Constant::Integer(42.into()),
            json!(["apply", ["force", ["builtin", "26"]], constant(42)]),
        ),
    ] {
        let mut ast = Ast::default();
        let argument = ast.constant(argument);
        let root = ast.call(builtin, &[argument]);
        let result = run(&ast.program(root));
        let events = 3 + i64::from(builtin.force_count());
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget {
                cpu: 100 + 16_000 * events,
                mem: 100 + 100 * events,
            })
        );
        assert_eq!(term(result), expected);
    }
    let mut ast = Ast::default();
    let root = ast.call(Builtin::IfThenElse, &[]);
    assert_eq!(
        term(run(&ast.program(root))),
        json!(["force", ["builtin", "26"]])
    );
}

#[test]
fn missing_and_excess_builtin_forces_fail_after_argument_evaluation() {
    for (builtin, forces, apply_argument, expected) in [
        (Builtin::AddInteger, 1, false, RuntimeError::NonDelayForce),
        (Builtin::IfThenElse, 2, false, RuntimeError::NonDelayForce),
        (
            Builtin::IfThenElse,
            0,
            true,
            RuntimeError::NonFunctionApplication,
        ),
    ] {
        let mut ast = Ast::default();
        let mut root = ast.term(Term::Builtin(builtin));
        for _ in 0..forces {
            root = ast.term(Term::Force(root));
        }
        if apply_argument {
            let argument = ast.constant(Constant::Bool(true));
            root = ast.apply(root, argument);
        }
        let evaluation = run(&ast.program(root));
        assert_eq!(evaluation.result, Err(MachineError::Runtime(expected)));
        assert_eq!(
            evaluation.consumed,
            Some(ExecutionBudget { cpu: 100, mem: 100 })
        );
    }
    let mut ast = Ast::default();
    let builtin = ast.term(Term::Builtin(Builtin::IfThenElse));
    let error = ast.term(Term::Error);
    let root = ast.apply(builtin, error);
    assert_eq!(
        run(&ast.program(root)).result,
        Err(MachineError::Runtime(RuntimeError::ExplicitError))
    );

    let mut ast = Ast::default();
    let one = ast.integer(1);
    let partial = ast.call(Builtin::AddInteger, &[one]);
    let root = ast.term(Term::Force(partial));
    assert_eq!(
        run(&ast.program(root)).result,
        Err(MachineError::Runtime(RuntimeError::NonDelayForce))
    );
}

#[test]
fn saturated_argument_validation_precedes_builtin_charging_and_preserves_order() {
    for builtin in [
        Builtin::AddInteger,
        Builtin::SubtractInteger,
        Builtin::MultiplyInteger,
        Builtin::EqualsInteger,
        Builtin::LessThanInteger,
        Builtin::LessThanEqualsInteger,
    ] {
        for index in [0, 1] {
            let mut ast = Ast::default();
            let good = ast.integer(1);
            let bad = ast.constant(Constant::Unit);
            let args = if index == 0 { [bad, good] } else { [good, bad] };
            let root = ast.call(builtin, &args);
            let ast = ast.program(root);
            let evaluation = run(&ast);
            assert_eq!(
                evaluation.result,
                Err(MachineError::Runtime(RuntimeError::BuiltinTypeMismatch {
                    builtin,
                    argument: index,
                    expected: ArgumentType::Integer,
                }))
            );
            assert_eq!(
                evaluation.consumed,
                Some(ExecutionBudget { cpu: 100, mem: 100 })
            );
            // The validation failure also wins over an invalid computed charge.
            let negative = custom(&[(0, -1), (124, -1), (167, -1), (71, -1), (99, -1), (96, -1)]);
            let zero = evaluate(&ast, &negative, ZERO);
            assert_eq!(zero.result, evaluation.result);
            assert_eq!(zero.consumed, Some(ZERO));
        }
    }
    let mut ast = Ast::default();
    let wrong = ast.integer(0);
    let error = ast.term(Term::Error);
    let root = ast.call(Builtin::IfThenElse, &[wrong, wrong, error]);
    assert_eq!(
        run(&ast.program(root)).result,
        Err(MachineError::Runtime(RuntimeError::ExplicitError))
    );
}

#[test]
fn if_then_else_returns_each_primitive_kind_without_converting_it() {
    for value in [
        Constant::Integer((-7).into()),
        Constant::Bool(false),
        Constant::Unit,
        Constant::ByteString(vec![0, 255]),
        Constant::String("λ🙂".into()),
    ] {
        let mut ast = Ast::default();
        let condition = ast.constant(Constant::Bool(false));
        let ignored = ast.integer(42);
        let wanted = ast.constant(value.clone());
        let root = ast.call(Builtin::IfThenElse, &[condition, ignored, wanted]);
        let actual = run(&ast.program(root)).result.unwrap();
        assert_eq!(actual.terms[actual.root], Term::Constant(value));
    }
}

#[test]
fn if_then_else_captured_closure_can_be_discharged_or_overapplied() {
    // (lambda x. if true (lambda y. x) (lambda y. y)) 42 [then apply 7].
    let mut ast = Ast::default();
    let capture = ast.term(Term::Var(2));
    let chosen = ast.term(Term::Lambda(capture));
    let local = ast.term(Term::Var(1));
    let other = ast.term(Term::Lambda(local));
    let condition = ast.constant(Constant::Bool(true));
    let body = ast.call(Builtin::IfThenElse, &[condition, chosen, other]);
    let closure = ast.term(Term::Lambda(body));
    let forty_two = ast.integer(42);
    let root = ast.apply(closure, forty_two);
    let returned = Program::new([1, 1, 0], ast.0.clone(), root).unwrap();
    assert_eq!(term(run(&returned)), json!(["lambda", constant(42)]));
    let seven = ast.integer(7);
    let root = ast.apply(root, seven);
    assert_eq!(term(run(&ast.program(root))), constant(42));
}

#[test]
fn if_then_else_preserves_delay_capture_and_only_a_later_force_evaluates_it() {
    // (lambda x. if false (delay error) (delay (add x 1))) 41.
    let mut ast = Ast::default();
    let x = ast.term(Term::Var(1));
    let one = ast.integer(1);
    let sum = ast.call(Builtin::AddInteger, &[x, one]);
    let chosen = ast.term(Term::Delay(sum));
    let error = ast.term(Term::Error);
    let ignored = ast.term(Term::Delay(error));
    let condition = ast.constant(Constant::Bool(false));
    let body = ast.call(Builtin::IfThenElse, &[condition, ignored, chosen]);
    let closure = ast.term(Term::Lambda(body));
    let forty_one = ast.integer(41);
    let root = ast.apply(closure, forty_one);
    let returned = Program::new([1, 1, 0], ast.0.clone(), root).unwrap();
    assert_eq!(
        term(run(&returned)),
        json!([
            "delay",
            [
                "apply",
                ["apply", ["builtin", "0"], constant(41)],
                constant(1)
            ]
        ])
    );
    let root = ast.term(Term::Force(root));
    assert_eq!(term(run(&ast.program(root))), constant(42));
}

#[test]
fn if_then_else_returns_partial_builtins_with_their_forces_and_arguments() {
    let mut ast = Ast::default();
    let forty_one = ast.integer(41);
    let chosen = ast.call(Builtin::AddInteger, &[forty_one]);
    let ignored = ast.term(Term::Builtin(Builtin::MultiplyInteger));
    let condition = ast.constant(Constant::Bool(true));
    let root = ast.call(Builtin::IfThenElse, &[condition, chosen, ignored]);
    let returned = Program::new([1, 1, 0], ast.0.clone(), root).unwrap();
    assert_eq!(
        term(run(&returned)),
        json!(["apply", ["builtin", "0"], constant(41)])
    );
    let one = ast.integer(1);
    let root = ast.apply(root, one);
    assert_eq!(term(run(&ast.program(root))), constant(42));

    let mut ast = Ast::default();
    let condition = ast.constant(Constant::Bool(true));
    let selected = ast.call(Builtin::IfThenElse, &[condition]);
    let ignored = ast.integer(0);
    let root = ast.call(Builtin::IfThenElse, &[condition, selected, ignored]);
    let selected = Program::new([1, 1, 0], ast.0.clone(), root).unwrap();
    assert_eq!(
        term(run(&selected)),
        json!([
            "apply",
            ["force", ["builtin", "26"]],
            ["constant", ["bool", true]]
        ])
    );
    let wanted = ast.integer(42);
    let root = ast.apply(root, wanted);
    let root = ast.apply(root, ignored);
    assert_eq!(term(run(&ast.program(root))), constant(42));
}

#[test]
fn builtin_partial_discharge_retains_captured_unevaluated_lambda_bodies() {
    // (lambda x. (force if) true (lambda y. force x)) 42 is still partial.
    let mut ast = Ast::default();
    let x = ast.term(Term::Var(2));
    let forced = ast.term(Term::Force(x));
    let branch = ast.term(Term::Lambda(forced));
    let condition = ast.constant(Constant::Bool(true));
    let partial = ast.call(Builtin::IfThenElse, &[condition, branch]);
    let closure = ast.term(Term::Lambda(partial));
    let forty_two = ast.integer(42);
    let root = ast.apply(closure, forty_two);
    assert_eq!(
        term(run(&ast.program(root))),
        json!([
            "apply",
            [
                "apply",
                ["force", ["builtin", "26"]],
                ["constant", ["bool", true]]
            ],
            ["lambda", ["force", constant(42)]]
        ])
    );
}

#[test]
fn captured_partial_applications_are_immutable_across_multiple_uses() {
    // (lambda f. add (f 2) (f 3)) (add 1) = 3 + 4.
    let mut ast = Ast::default();
    let f = ast.term(Term::Var(1));
    let two = ast.integer(2);
    let three = ast.integer(3);
    let left = ast.apply(f, two);
    let right = ast.apply(f, three);
    let sum = ast.call(Builtin::AddInteger, &[left, right]);
    let lambda = ast.term(Term::Lambda(sum));
    let one = ast.integer(1);
    let partial = ast.call(Builtin::AddInteger, &[one]);
    let root = ast.apply(lambda, partial);
    assert_eq!(term(run(&ast.program(root))), constant(7));

    // Reusing the same unforced ifThenElse must also retain its force slot.
    let mut ast = Ast::default();
    let f = ast.term(Term::Var(1));
    let one = ast.integer(1);
    let two = ast.integer(2);
    let yes = ast.constant(Constant::Bool(true));
    let no = ast.constant(Constant::Bool(false));
    let mut branches = Vec::new();
    for condition in [yes, no] {
        let mut branch = ast.term(Term::Force(f));
        for argument in [condition, one, two] {
            branch = ast.apply(branch, argument);
        }
        branches.push(branch);
    }
    let sum = ast.call(Builtin::AddInteger, &branches);
    let lambda = ast.term(Term::Lambda(sum));
    let builtin = ast.term(Term::Builtin(Builtin::IfThenElse));
    let root = ast.apply(lambda, builtin);
    assert_eq!(term(run(&ast.program(root))), constant(3));
}

#[test]
fn call_by_value_evaluates_both_branches_and_overapplication_arguments() {
    for condition in [true, false] {
        let mut ast = Ast::default();
        let condition = ast.constant(Constant::Bool(condition));
        let value = ast.integer(42);
        let error = ast.term(Term::Error);
        let root = ast.call(Builtin::IfThenElse, &[condition, value, error]);
        let evaluation = run(&ast.program(root));
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(
            evaluation.consumed,
            Some(ExecutionBudget { cpu: 100, mem: 100 })
        );
    }
    for error_argument in [false, true] {
        let mut ast = Ast::default();
        let one = ast.integer(1);
        let sum = ast.call(Builtin::AddInteger, &[one, one]);
        let argument = if error_argument {
            ast.term(Term::Error)
        } else {
            one
        };
        let root = ast.apply(sum, argument);
        let evaluation = run(&ast.program(root));
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(if error_argument {
                RuntimeError::ExplicitError
            } else {
                RuntimeError::NonFunctionApplication
            }))
        );
        // Only startup + saturated add charge; semantic failure leaves steps pending.
        assert_eq!(
            evaluation.consumed,
            Some(ExecutionBudget {
                cpu: 101_308,
                mem: 102
            })
        );
    }
}

#[test]
fn exact_and_one_short_budgets_and_zero_budget_follow_charge_order() {
    let ast = binary(Builtin::AddInteger, 1, 2);
    let exact = ExecutionBudget {
        cpu: 181_308,
        mem: 602,
    };
    assert!(evaluate(&ast, &model(), exact).result.is_ok());
    for limit in [
        ExecutionBudget {
            cpu: exact.cpu - 1,
            ..exact
        },
        ExecutionBudget {
            mem: exact.mem - 1,
            ..exact
        },
    ] {
        let evaluation = evaluate(&ast, &model(), limit);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
        assert_eq!(evaluation.consumed, Some(exact));
    }
    for (limit, attempted) in [
        (ZERO, ExecutionBudget { cpu: 100, mem: 100 }),
        (
            ExecutionBudget { cpu: 100, mem: 100 },
            ExecutionBudget {
                cpu: 101_308,
                mem: 102,
            },
        ),
        (
            ExecutionBudget {
                cpu: 101_307,
                mem: i64::MAX,
            },
            ExecutionBudget {
                cpu: 101_308,
                mem: 102,
            },
        ),
        (
            ExecutionBudget {
                cpu: i64::MAX,
                mem: 101,
            },
            ExecutionBudget {
                cpu: 101_308,
                mem: 102,
            },
        ),
    ] {
        let evaluation = evaluate(&ast, &model(), limit);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
        assert_eq!(evaluation.consumed, Some(attempted));
    }
}

#[test]
fn returned_values_determine_later_force_and_application_behavior() {
    for builtin_branch in [false, true] {
        let mut ast = Ast::default();
        let condition = ast.constant(Constant::Bool(true));
        let chosen = if builtin_branch {
            ast.term(Term::Builtin(Builtin::IfThenElse))
        } else {
            ast.integer(42)
        };
        let other = ast.integer(0);
        let selected = ast.call(Builtin::IfThenElse, &[condition, chosen, other]);
        let forced = ast.term(Term::Force(selected));
        if builtin_branch {
            assert_eq!(
                term(run(&ast.program(forced))),
                json!(["force", ["builtin", "26"]])
            );
        } else {
            let evaluation = run(&ast.program(forced));
            assert_eq!(
                evaluation.result,
                Err(MachineError::Runtime(RuntimeError::NonDelayForce))
            );
            assert_eq!(
                evaluation.consumed,
                Some(ExecutionBudget {
                    cpu: 76_149,
                    mem: 101
                })
            );
        }
    }
    // Saturated ifThenElse rejects an opaque condition before charging.
    let mut ast = Ast::default();
    let unit = ast.constant(Constant::Unit);
    let opaque = ast.term(Term::Delay(unit));
    let root = ast.call(Builtin::IfThenElse, &[opaque, unit, unit]);
    let evaluation = run(&ast.program(root));
    assert_eq!(
        evaluation.result,
        Err(MachineError::Runtime(RuntimeError::BuiltinTypeMismatch {
            builtin: Builtin::IfThenElse,
            argument: 0,
            expected: ArgumentType::Bool,
        }))
    );
    assert_eq!(
        evaluation.consumed,
        Some(ExecutionBudget { cpu: 100, mem: 100 })
    );
}

#[test]
fn generated_constants_and_partial_builtins_survive_closure_capture() {
    // (lambda x. lambda y. x) (add 1 2) retains a produced constant.
    let mut ast = Ast::default();
    let x = ast.term(Term::Var(2));
    let lambda = ast.term(Term::Lambda(x));
    let lambda = ast.term(Term::Lambda(lambda));
    let one = ast.integer(1);
    let two = ast.integer(2);
    let sum = ast.call(Builtin::AddInteger, &[one, two]);
    let root = ast.apply(lambda, sum);
    assert_eq!(
        term(run(&ast.program(root))),
        json!(["lambda", constant(3)])
    );

    // (lambda f. delay f) ((force if) true (add 1)) discharges its full spine.
    let mut ast = Ast::default();
    let f = ast.term(Term::Var(1));
    let delayed = ast.term(Term::Delay(f));
    let lambda = ast.term(Term::Lambda(delayed));
    let one = ast.integer(1);
    let partial = ast.call(Builtin::AddInteger, &[one]);
    let condition = ast.constant(Constant::Bool(true));
    let partial = ast.call(Builtin::IfThenElse, &[condition, partial]);
    let root = ast.apply(lambda, partial);
    assert_eq!(
        term(run(&ast.program(root))),
        json!([
            "delay",
            [
                "apply",
                [
                    "apply",
                    ["force", ["builtin", "26"]],
                    ["constant", ["bool", true]]
                ],
                ["apply", ["builtin", "0"], constant(1)]
            ]
        ])
    );
}

#[test]
fn machine_builtin_is_the_final_category_in_a_batch_flush() {
    // force ((lambda x. delay x) (add 42)) has eight events, no saturation.
    let mut ast = Ast::default();
    let x = ast.term(Term::Var(1));
    let delay = ast.term(Term::Delay(x));
    let lambda = ast.term(Term::Lambda(delay));
    let forty_two = ast.integer(42);
    let partial = ast.call(Builtin::AddInteger, &[forty_two]);
    let apply = ast.apply(lambda, partial);
    let root = ast.term(Term::Force(apply));
    let ast = ast.program(root);
    let costs = custom(&[
        (29, 1),
        (30, 2),
        (31, 4),
        (32, 8),
        (21, 16),
        (22, 32),
        (27, 64),
        (28, 128),
        (17, 256),
        (18, 512),
        (23, 1024),
        (24, 2048),
        (25, 4096),
        (26, 8192),
        (19, 16384),
        (20, 32768),
    ]);
    let mut prefix = ZERO;
    for (cpu, mem) in [
        (1, 2),
        (16, 32),
        (4, 8),
        (64, 128),
        (512, 1024),
        (1024, 2048),
        (4096, 8192),
        (16384, 32768),
    ] {
        prefix.cpu += cpu;
        prefix.mem += mem;
        for limit in [
            ExecutionBudget {
                cpu: prefix.cpu - 1,
                ..UNLIMITED
            },
            ExecutionBudget {
                mem: prefix.mem - 1,
                ..UNLIMITED
            },
        ] {
            let evaluation = evaluate(&ast, &costs, limit);
            assert_eq!(
                evaluation.result,
                Err(MachineError::Budget(BudgetError::Exhausted))
            );
            assert_eq!(evaluation.consumed, Some(prefix));
        }
    }
    assert_eq!(
        prefix,
        ExecutionBudget {
            cpu: 22_101,
            mem: 44_202
        }
    );
    assert!(evaluate(&ast, &costs, prefix).result.is_ok());
}

#[test]
fn builtin_charges_move_before_or_after_the_two_hundred_event_boundary() {
    for (wrappers, consumed) in [
        (
            97,
            ExecutionBudget {
                cpu: 101_308,
                mem: 102,
            },
        ),
        (
            98,
            ExecutionBudget {
                cpu: 16_100,
                mem: 200,
            },
        ),
    ] {
        let mut ast = Ast::default();
        let one = ast.integer(1);
        let sum = ast.call(Builtin::AddInteger, &[one, one]);
        let root = ast.wrap(sum, wrappers);
        let ast = ast.program(root);
        let evaluation = evaluate(&ast, &model(), ExecutionBudget { cpu: 100, mem: 100 });
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
        assert_eq!(evaluation.consumed, Some(consumed));
        let successful = run(&ast);
        let events = 5 + 2 * wrappers as i64;
        assert_eq!(
            successful.consumed,
            Some(ExecutionBudget {
                cpu: 101_308 + 16_000 * events,
                mem: 102 + 100 * events,
            })
        );
    }
    for (wrappers, charged_events) in [(96, 0), (97, 200)] {
        let mut ast = Ast::default();
        let one = ast.integer(1);
        let sum = ast.call(Builtin::AddInteger, &[one, one]);
        let overapply = ast.apply(sum, one);
        let root = ast.wrap(overapply, wrappers);
        let evaluation = run(&ast.program(root));
        assert_eq!(
            evaluation.result,
            Err(MachineError::Runtime(RuntimeError::NonFunctionApplication))
        );
        assert_eq!(
            evaluation.consumed,
            Some(ExecutionBudget {
                cpu: 101_308 + 16_000 * charged_events,
                mem: 102 + 100 * charged_events,
            })
        );
    }
}

#[test]
fn custom_signed_coefficients_can_cancel_but_negative_charges_are_unsupported() {
    let ast = binary(Builtin::AddInteger, BigInt::from(1) << 64, 1);
    // x=2, so -3+2*x=1 and -5+3*x=1. All machine coefficients are zero.
    let costs = custom(&[(0, -3), (1, 2), (2, -5), (3, 3)]);
    let evaluation = evaluate(&ast, &costs, ExecutionBudget { cpu: 1, mem: 1 });
    assert_eq!(
        evaluation.consumed,
        Some(ExecutionBudget { cpu: 1, mem: 1 })
    );
    assert!(evaluation.result.is_ok());
    for updates in [
        &[(0, -3), (1, 1)][..],
        &[(2, -3), (3, 1)][..],
        &[(0, i64::MAX), (1, i64::MAX), (2, -1)][..],
    ] {
        let evaluation = evaluate(&ast, &custom(updates), UNLIMITED);
        is_unsupported(&evaluation, "negative");
        assert_eq!(evaluation.consumed, Some(ZERO));
    }
}

#[test]
fn builtin_charge_and_consumption_overflow_return_null_before_execution() {
    let ast = binary(Builtin::MultiplyInteger, BigInt::from(1) << 64, 1);
    for updates in [
        &[(124, i64::MAX), (125, 1)][..],
        &[(126, i64::MAX), (127, 1)][..],
        &[(29, i64::MAX), (124, 1)][..],
        &[(30, i64::MAX), (126, 1)][..],
    ] {
        let evaluation = evaluate(&ast, &custom(updates), UNLIMITED);
        assert_eq!(
            evaluation.result,
            Err(MachineError::Budget(BudgetError::Overflow))
        );
        assert_eq!(evaluation.consumed, None);
    }
}

#[test]
fn profile_integer_unlifting_bounds_are_checked_only_at_saturation() {
    let boundary = BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS;
    let outside = binary(Builtin::AddInteger, boundary.clone(), 0);
    let evaluation = run(&outside);
    assert_eq!(
        evaluation.result,
        Err(MachineError::Runtime(
            RuntimeError::BuiltinIntegerOutOfBounds {
                builtin: Builtin::AddInteger,
                argument: 0,
            }
        ))
    );
    assert_eq!(
        evaluation.consumed,
        Some(ExecutionBudget { cpu: 100, mem: 100 })
    );
    assert!(
        run(&binary(
            Builtin::EqualsInteger,
            boundary.clone(),
            boundary.clone()
        ))
        .result
        .is_ok()
    );
    assert!(
        run(&binary(Builtin::AddInteger, -&boundary, 0))
            .result
            .is_ok()
    );
    let mut ast = Ast::default();
    let outside = ast.integer(boundary);
    let root = ast.call(Builtin::AddInteger, &[outside]);
    assert!(run(&ast.program(root)).result.is_ok());
}

#[test]
fn zero_cost_builtin_execution_has_a_cumulative_portable_work_bound() {
    let large: BigInt = BigInt::from(1) << 202_368;
    // 3163 * 3163 = 10004569 units > 10M; bounded E inputs, legal result size.
    let ast = binary(Builtin::MultiplyInteger, large.clone(), large);
    let evaluation = evaluate(&ast, &custom(&[]), ZERO);
    is_unsupported(&evaluation, "work bound");
    assert_eq!(evaluation.consumed, Some(ZERO));
    // Charging wins over this same resource limit and leaves no pending flush.
    let evaluation = evaluate(&ast, &model(), ExecutionBudget { cpu: 100, mem: 100 });
    assert_eq!(
        evaluation.result,
        Err(MachineError::Budget(BudgetError::Exhausted))
    );
    assert_eq!(
        evaluation.consumed,
        Some(ExecutionBudget {
            cpu: 5_192_461_845,
            mem: 6426
        })
    );

    // Small per-call work can exceed the cumulative allowance too.
    let mut ast = Ast::default();
    let value = ast.integer(BigInt::from(1) << 640);
    let mut root = value;
    for _ in 0..3 {
        root = ast.call(Builtin::AddInteger, &[root, value]);
    }
    let ast = ast.program(root);
    let evaluation = evaluate_with_limits(
        &ast,
        &custom(&[]),
        ZERO,
        ResourceLimits {
            work: 45,
            ..ResourceLimits::default()
        },
    );
    is_unsupported(&evaluation, "work bound");
}

#[test]
fn generated_constant_payload_is_cumulatively_bounded_even_with_zero_costs() {
    let resources = ResourceLimits {
        constant_bytes: 2,
        ..ResourceLimits::default()
    };
    // Each result fits alone. Three retained one-byte results exceed the total.
    let mut ast = Ast::default();
    let one = ast.integer(1);
    let two = ast.call(Builtin::AddInteger, &[one, one]);
    let three = ast.call(Builtin::AddInteger, &[two, one]);
    let four = ast.call(Builtin::AddInteger, &[three, one]);
    let evaluation = evaluate_with_limits(&ast.program(four), &custom(&[]), ZERO, resources);
    is_unsupported(&evaluation, "runtime constant payload");
    assert_eq!(evaluation.consumed, Some(ZERO));

    let ast = binary(Builtin::AddInteger, 255, 1);
    assert!(
        evaluate_with_limits(&ast, &custom(&[]), ZERO, resources)
            .result
            .is_ok()
    );
    let evaluation = evaluate_with_limits(
        &ast,
        &model(),
        UNLIMITED,
        ResourceLimits {
            constant_bytes: 1,
            ..ResourceLimits::default()
        },
    );
    is_unsupported(&evaluation, "runtime constant payload");
    assert_eq!(
        evaluation.consumed,
        Some(ExecutionBudget {
            cpu: 101_308,
            mem: 102
        })
    );
    // Input constants retain their existing storage and do not debit this cap.
    let mut ast = Ast::default();
    let root = ast.integer(BigInt::from(1) << 1024);
    assert!(
        evaluate_with_limits(&ast.program(root), &custom(&[]), ZERO, resources)
            .result
            .is_ok()
    );
}

#[test]
fn division_success_and_exact_or_one_short_budgets_use_independent_costs() {
    // Polynomial(1,1)=123203+1716+7305+57+960-900=132341;
    // all four memories are one. Startup plus five CEK events adds 80100/600.
    let exact = ExecutionBudget {
        cpu: 212_441,
        mem: 601,
    };
    for (builtin, expected) in DIVISION.into_iter().zip([-3, -2, -1, 2]) {
        let program = binary(builtin, -7, 3);
        let success = evaluate(&program, &model(), exact);
        assert_eq!(success.consumed, Some(exact));
        assert_eq!(term(success), constant(expected));
        for limit in [
            ExecutionBudget {
                cpu: exact.cpu - 1,
                ..exact
            },
            ExecutionBudget {
                mem: exact.mem - 1,
                ..exact
            },
        ] {
            let result = evaluate(&program, &model(), limit);
            assert_eq!(
                result.result,
                Err(MachineError::Budget(BudgetError::Exhausted))
            );
            assert_eq!(result.consumed, Some(exact));
        }
        assert_eq!(
            term(evaluate(&program, &custom(&[]), ZERO)),
            constant(expected)
        );
    }
}

#[test]
fn division_zero_failures_charge_application_without_flushing_pending_steps() {
    let attempted = ExecutionBudget {
        cpu: 132_441,
        mem: 101,
    };
    for builtin in DIVISION {
        for numerator in [-1, 0, 1] {
            let program = binary(builtin, numerator, 0);
            let result = evaluate(&program, &model(), attempted);
            assert_eq!(
                result.result,
                Err(MachineError::Runtime(RuntimeError::BuiltinDivisionByZero {
                    builtin
                }))
            );
            assert_eq!(result.consumed, Some(attempted));
            for limit in [
                ExecutionBudget {
                    cpu: attempted.cpu - 1,
                    ..attempted
                },
                ExecutionBudget {
                    mem: attempted.mem - 1,
                    ..attempted
                },
            ] {
                let result = evaluate(&program, &model(), limit);
                assert_eq!(
                    result.result,
                    Err(MachineError::Budget(BudgetError::Exhausted))
                );
                assert_eq!(result.consumed, Some(attempted));
            }
            let result = evaluate(&program, &model(), ZERO);
            assert_eq!(
                result.result,
                Err(MachineError::Budget(BudgetError::Exhausted))
            );
            assert_eq!(
                result.consumed,
                Some(ExecutionBudget { cpu: 100, mem: 100 })
            );
            let result = evaluate(&program, &custom(&[]), ZERO);
            assert_eq!(
                result.result,
                Err(MachineError::Runtime(RuntimeError::BuiltinDivisionByZero {
                    builtin
                }))
            );
            assert_eq!(result.consumed, Some(ZERO));
        }
    }
}

#[test]
fn division_zero_failure_crosses_the_two_hundred_event_flush_exactly() {
    for builtin in DIVISION {
        for (wrappers, extra_force, charged_events) in
            [(97, false, 0), (97, true, 200), (98, false, 200)]
        {
            let mut ast = Ast::default();
            let one = ast.integer(1);
            let zero = ast.integer(0);
            let call = ast.call(builtin, &[one, zero]);
            let mut root = ast.wrap(call, wrappers);
            if extra_force {
                root = ast.term(Term::Force(root));
            }
            let program = ast.program(root);
            let result = run(&program);
            assert_eq!(
                result.result,
                Err(MachineError::Runtime(RuntimeError::BuiltinDivisionByZero {
                    builtin
                }))
            );
            assert_eq!(
                result.consumed,
                Some(ExecutionBudget {
                    cpu: 132_441 + 16_000 * charged_events,
                    mem: 101 + 100 * charged_events,
                })
            );
            // At 200 events, constant is the first category in the batch and
            // exhausts before the denotation or builtin application charge.
            // With 97 wrappers plus the extra force, both constants are in the
            // batch; 98 wrappers put only the first constant in that batch.
            let result = evaluate(&program, &model(), ExecutionBudget { cpu: 100, mem: 100 });
            assert_eq!(
                result.result,
                Err(MachineError::Budget(BudgetError::Exhausted))
            );
            assert_eq!(
                result.consumed,
                Some(if charged_events == 0 {
                    ExecutionBudget {
                        cpu: 132_441,
                        mem: 101,
                    }
                } else if extra_force {
                    ExecutionBudget {
                        cpu: 32_100,
                        mem: 300,
                    }
                } else {
                    ExecutionBudget {
                        cpu: 16_100,
                        mem: 200,
                    }
                })
            );
        }
    }
}

#[test]
fn division_unlifting_precedes_charging_but_evaluates_both_arguments_first() {
    let outside = BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS;
    for builtin in DIVISION {
        for bad_argument in [0, 1] {
            for bad_type in [false, true] {
                let mut ast = Ast::default();
                let bad = if bad_type {
                    ast.constant(Constant::Bool(true))
                } else {
                    ast.integer(outside.clone())
                };
                let zero = ast.integer(0);
                let arguments = if bad_argument == 0 {
                    [bad, zero]
                } else {
                    [zero, bad]
                };
                let root = ast.call(builtin, &arguments);
                let result = run(&ast.program(root));
                let error = if bad_type {
                    RuntimeError::BuiltinTypeMismatch {
                        builtin,
                        argument: bad_argument,
                        expected: ArgumentType::Integer,
                    }
                } else {
                    RuntimeError::BuiltinIntegerOutOfBounds {
                        builtin,
                        argument: bad_argument,
                    }
                };
                assert_eq!(result.result, Err(MachineError::Runtime(error)));
                assert_eq!(
                    result.consumed,
                    Some(ExecutionBudget { cpu: 100, mem: 100 })
                );
            }
        }
        // Unlifting is deferred until the second argument has become a value.
        let mut ast = Ast::default();
        let wrong_type = ast.constant(Constant::Bool(true));
        let error = ast.term(Term::Error);
        let root = ast.call(builtin, &[wrong_type, error]);
        let result = run(&ast.program(root));
        assert_eq!(
            result.result,
            Err(MachineError::Runtime(RuntimeError::ExplicitError))
        );
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget { cpu: 100, mem: 100 })
        );
    }
}

#[test]
fn division_partial_invalid_arguments_and_excess_forces_remain_structural() {
    let outside: BigInt = BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS;
    for builtin in DIVISION {
        for invalid in [Constant::Bool(true), Constant::Integer(outside.clone())] {
            let mut ast = Ast::default();
            let value = ast.constant(invalid.clone());
            let partial = ast.call(builtin, &[value]);
            let program = Program::new([1, 1, 0], ast.0.clone(), partial).unwrap();
            let expected_argument = match &invalid {
                Constant::Bool(value) => json!(["constant", ["bool", value]]),
                Constant::Integer(value) => json!(["constant", ["integer", value.to_string()]]),
                _ => unreachable!(),
            };
            assert_eq!(
                term(run(&program)),
                json!([
                    "apply",
                    ["builtin", builtin.tag().to_string()],
                    expected_argument
                ])
            );
            // These monomorphic functions require no forces. A force on a
            // partial value fails structurally without attempting unlifting.
            let root = ast.term(Term::Force(partial));
            let result = run(&ast.program(root));
            assert_eq!(
                result.result,
                Err(MachineError::Runtime(RuntimeError::NonDelayForce))
            );
            assert_eq!(
                result.consumed,
                Some(ExecutionBudget { cpu: 100, mem: 100 })
            );
        }
        let mut ast = Ast::default();
        let bare = ast.term(Term::Builtin(builtin));
        let forced = ast.term(Term::Force(bare));
        let result = run(&ast.program(forced));
        assert_eq!(
            result.result,
            Err(MachineError::Runtime(RuntimeError::NonDelayForce))
        );
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget { cpu: 100, mem: 100 })
        );
    }
}

#[test]
fn division_captured_lambda_and_delay_discharge_without_running_their_bodies() {
    for (builtin, expected) in DIVISION.into_iter().zip([-3, -2, -1, 2]) {
        // Capture numerator x inside lambda y. f x y, then apply y=3.
        let mut ast = Ast::default();
        let x = ast.term(Term::Var(2));
        let y = ast.term(Term::Var(1));
        let body = ast.call(builtin, &[x, y]);
        let lambda = ast.term(Term::Lambda(body));
        let capture = ast.term(Term::Lambda(lambda));
        let numerator = ast.integer(-7);
        let closure = ast.apply(capture, numerator);
        let returned = Program::new([1, 1, 0], ast.0.clone(), closure).unwrap();
        assert_eq!(
            term(run(&returned)),
            json!([
                "lambda",
                [
                    "apply",
                    [
                        "apply",
                        ["builtin", builtin.tag().to_string()],
                        constant(-7)
                    ],
                    ["var", "1"]
                ]
            ])
        );
        let denominator = ast.integer(3);
        let root = ast.apply(closure, denominator);
        assert_eq!(term(run(&ast.program(root))), constant(expected));

        // A captured zero divisor in a returned delay is syntax until forced.
        let mut ast = Ast::default();
        let x = ast.term(Term::Var(1));
        let zero = ast.integer(0);
        let body = ast.call(builtin, &[x, zero]);
        let delay = ast.term(Term::Delay(body));
        let capture = ast.term(Term::Lambda(delay));
        let numerator = ast.integer(-7);
        let delayed = ast.apply(capture, numerator);
        let returned = Program::new([1, 1, 0], ast.0.clone(), delayed).unwrap();
        let result = run(&returned);
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget {
                cpu: 64_100,
                mem: 500
            })
        );
        assert_eq!(
            term(result),
            json!([
                "delay",
                [
                    "apply",
                    [
                        "apply",
                        ["builtin", builtin.tag().to_string()],
                        constant(-7)
                    ],
                    constant(0)
                ]
            ])
        );
        let root = ast.term(Term::Force(delayed));
        let result = run(&ast.program(root));
        assert_eq!(
            result.result,
            Err(MachineError::Runtime(RuntimeError::BuiltinDivisionByZero {
                builtin
            }))
        );
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget {
                cpu: 132_441,
                mem: 101
            })
        );
    }
}

#[test]
fn division_partials_survive_if_then_else_and_repeated_capture_use() {
    for (builtin, expected, repeated) in DIVISION
        .into_iter()
        .zip([-3, -2, -1, 2])
        .zip([-1, 0, -2, 1])
        .map(|((b, e), r)| (b, e, r))
    {
        let mut ast = Ast::default();
        let numerator = ast.integer(-7);
        let invalid = ast.constant(Constant::Bool(true));
        let valid = ast.call(builtin, &[numerator]);
        let wrong_type = ast.call(builtin, &[invalid]);
        let condition = ast.constant(Constant::Bool(true));
        let selected = ast.call(Builtin::IfThenElse, &[condition, valid, wrong_type]);
        let denominator = ast.integer(3);
        let root = ast.apply(selected, denominator);
        assert_eq!(term(run(&ast.program(root))), constant(expected));
        // (lambda f. add (f 3) (f -3)) (builtin -7), with shared partial f.
        let mut ast = Ast::default();
        let f = ast.term(Term::Var(1));
        let three = ast.integer(3);
        let negative_three = ast.integer(-3);
        let left = ast.apply(f, three);
        let right = ast.apply(f, negative_three);
        let sum = ast.call(Builtin::AddInteger, &[left, right]);
        let lambda = ast.term(Term::Lambda(sum));
        let numerator = ast.integer(-7);
        let partial = ast.call(builtin, &[numerator]);
        let root = ast.apply(lambda, partial);
        assert_eq!(term(run(&ast.program(root))), constant(repeated));
    }
}

#[test]
fn division_overapplication_and_failed_function_preserve_call_by_value_order() {
    for builtin in DIVISION {
        for (zero_divisor, error_argument) in [(false, false), (false, true), (true, true)] {
            let mut ast = Ast::default();
            let numerator = ast.integer(7);
            let denominator = ast.integer(if zero_divisor { 0 } else { 3 });
            let saturated = ast.call(builtin, &[numerator, denominator]);
            let argument = if error_argument {
                ast.term(Term::Error)
            } else {
                numerator
            };
            let root = ast.apply(saturated, argument);
            let result = run(&ast.program(root));
            let error = if zero_divisor {
                RuntimeError::BuiltinDivisionByZero { builtin }
            } else if error_argument {
                RuntimeError::ExplicitError
            } else {
                RuntimeError::NonFunctionApplication
            };
            assert_eq!(result.result, Err(MachineError::Runtime(error)));
            assert_eq!(
                result.consumed,
                Some(ExecutionBudget {
                    cpu: 132_441,
                    mem: 101
                })
            );
        }
    }
}

#[test]
fn division_profile_minimum_divided_by_minus_one_is_a_permitted_result() {
    let boundary: BigInt = BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS;
    for builtin in DIVISION {
        let result = run(&binary(builtin, -&boundary, -1));
        let expected = if matches!(builtin, Builtin::DivideInteger | Builtin::QuotientInteger) {
            boundary.clone()
        } else {
            0.into()
        };
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget {
                cpu: 967_471_916,
                mem: if matches!(builtin, Builtin::DivideInteger | Builtin::QuotientInteger) {
                    4695
                } else {
                    601
                },
            })
        );
        assert_eq!(
            term(result),
            json!(["constant", ["integer", expected.to_string()]])
        );
    }
}

#[test]
fn division_charge_overflow_or_negative_models_precede_zero_failure() {
    for (builtin, index) in DIVISION.into_iter().zip([49, 130, 141, 114]) {
        let program = binary(builtin, 1, 0);
        for updates in [
            vec![(29, 1), (index + 1, i64::MAX)],
            vec![(30, 1), (index + 8, i64::MAX)],
        ] {
            let result = evaluate(&program, &custom(&updates), UNLIMITED);
            assert_eq!(
                result.result,
                Err(MachineError::Budget(BudgetError::Overflow))
            );
            assert_eq!(result.consumed, None);
        }
        for updates in [
            vec![(index + 1, -1), (index + 7, -1)],
            vec![(index + 8, -1)],
        ] {
            let result = evaluate(&program, &custom(&updates), UNLIMITED);
            is_unsupported(&result, "negative");
            assert_eq!(result.consumed, Some(ZERO));
        }
    }
}

#[test]
fn division_work_limits_follow_charging_and_precede_denotation_even_at_zero_cost() {
    let large: BigInt = BigInt::from(1) << 202_368;
    for builtin in DIVISION {
        // 3163^2+3163=10007732 portable units exceed 10M without executing.
        let program = binary(builtin, large.clone(), large.clone());
        let result = evaluate(&program, &custom(&[]), ZERO);
        is_unsupported(&result, "work bound");
        assert_eq!(result.consumed, Some(ZERO));
        let result = evaluate(&program, &model(), ExecutionBudget { cpu: 100, mem: 100 });
        assert_eq!(
            result.result,
            Err(MachineError::Budget(BudgetError::Exhausted))
        );
        assert_eq!(
            result.consumed,
            Some(ExecutionBudget {
                cpu: 1_199_191_299,
                mem: if matches!(builtin, Builtin::DivideInteger | Builtin::QuotientInteger) {
                    101
                } else {
                    3263
                },
            })
        );
        // Nine CEK transitions precede the two units for this division. A
        // resource failure happens before the zero-divisor denotation.
        let program = binary(builtin, 1, 0);
        for (work, denotation) in [(10, false), (11, true)] {
            let result = evaluate_with_limits(
                &program,
                &model(),
                UNLIMITED,
                ResourceLimits {
                    work,
                    ..ResourceLimits::default()
                },
            );
            if denotation {
                assert_eq!(
                    result.result,
                    Err(MachineError::Runtime(RuntimeError::BuiltinDivisionByZero {
                        builtin
                    }))
                );
            } else {
                is_unsupported(&result, "work bound");
            }
            assert_eq!(
                result.consumed,
                Some(ExecutionBudget {
                    cpu: 132_441,
                    mem: 101
                })
            );
        }
    }
}

#[test]
fn division_results_retain_runtime_payload_and_cumulative_work_limits() {
    for builtin in [Builtin::DivideInteger, Builtin::QuotientInteger] {
        let mut ast = Ast::default();
        let one = ast.integer(1);
        let value = ast.integer(255);
        let mut root = value;
        for _ in 0..3 {
            root = ast.call(builtin, &[root, one]);
        }
        let result = evaluate_with_limits(
            &ast.program(root),
            &custom(&[]),
            ZERO,
            ResourceLimits {
                constant_bytes: 2,
                ..ResourceLimits::default()
            },
        );
        is_unsupported(&result, "runtime constant payload");
        let mut ast = Ast::default();
        let one = ast.integer(1);
        let value = ast.integer(BigInt::from(1) << 640);
        let mut root = value;
        for _ in 0..3 {
            root = ast.call(builtin, &[root, one]);
        }
        let result = evaluate_with_limits(
            &ast.program(root),
            &custom(&[]),
            ZERO,
            ResourceLimits {
                work: 64,
                ..ResourceLimits::default()
            },
        );
        is_unsupported(&result, "work bound");
    }
}
