use pallas_primitives::conway::Language as AikenLanguage;
use serde_json::{Value, json};
use uplc::{
    ast::{Constant, DeBruijn, NamedDeBruijn, Program, Term, Type},
    machine::{Error, cost_model::ExBudget},
    parser,
};
use uplc_conformance::{Budget, FailureKind, Language, Mode, Outcome, Program as Input, Request};

const REVISION: &str = "b5c34839ee33a2d608a071f3679410f63ad68bae";

/// Encode a parsed source without evaluating it. The bytes constant is a
/// transport envelope for the fixture builder, not an evaluation result.
fn encode_flat(request: &Request) -> Outcome {
    if !matches!(request.profile.language, Language::PlutusV3)
        || request.profile.protocol_major != 11
    {
        return Outcome::unsupported("initial encoder profile: PlutusV3 / protocol 11");
    }
    let Input::UplcText { source } = &request.program else {
        return Outcome::unsupported("--encode-flat requires textual UPLC input");
    };
    let program = match parser::program(source) {
        Ok(program) => program,
        Err(error) => return Outcome::failure(FailureKind::Decode, error.to_string()),
    };
    let program = match program.to_debruijn() {
        Ok(program) => program,
        Err(error) => return Outcome::failure(FailureKind::Evaluation, error.to_string()),
    };
    match program.to_flat() {
        Ok(bytes) => Outcome::Success {
            term: json!(["constant", ["bytes", hex::encode(bytes)]]),
            budget: Budget::new(0, 0),
            traces: vec![],
        },
        Err(error) => Outcome::failure(FailureKind::Decode, error.to_string()),
    }
}

fn evaluate(request: &Request, normalize_only: bool) -> Outcome {
    let Mode::Restricting { budget } = &request.mode else {
        return Outcome::unsupported(
            "this Aiken adapter exposes restricting mode only; a large budget is not counting mode",
        );
    };
    if !matches!(request.profile.language, Language::PlutusV3)
        || request.profile.protocol_major != 11
    {
        return Outcome::unsupported("initial adapter profile: PlutusV3 / protocol 11");
    }
    let costs = match request.validate() {
        Ok(costs) if costs.len() == 350 => costs,
        Ok(_) => return Outcome::infrastructure("expected the 350-parameter V3/PV11 cost model"),
        Err(error) => return Outcome::infrastructure(error),
    };
    let (cpu, mem) = budget.limits().expect("validated budget");
    let program: Program<NamedDeBruijn> = match &request.program {
        Input::UplcText { source } => {
            let parsed = match parser::program(source) {
                Ok(program) => program,
                Err(error) => return Outcome::failure(FailureKind::Decode, error.to_string()),
            };
            match parsed.to_named_debruijn() {
                Ok(program) => program,
                // Aiken's own conformance harness classifies unbound names as evaluation failure.
                Err(error) => return Outcome::failure(FailureKind::Evaluation, error.to_string()),
            }
        }
        Input::Flat { hex } => {
            let bytes = hex::decode(hex).expect("validated hex");
            match Program::<DeBruijn>::from_flat(&bytes) {
                Ok(program) => program.into(),
                Err(error) => return Outcome::failure(FailureKind::Decode, error.to_string()),
            }
        }
    };
    if normalize_only {
        return match normalize(&program.term, 0) {
            Ok(term) => Outcome::Success {
                term,
                budget: Budget::new(0, 0),
                traces: vec![],
            },
            Err(reason) => Outcome::unsupported(reason),
        };
    }
    let result = program.eval_as_with_protocol(
        &AikenLanguage::PlutusV3,
        11,
        &costs,
        Some(&ExBudget { cpu, mem }),
    );
    let used = result.cost();
    let budget = Budget::new(used.cpu, used.mem);
    let traces = result.logs();
    match result.result {
        Ok(term) => match normalize(&term, 0) {
            Ok(term) => Outcome::Success {
                term,
                budget,
                traces,
            },
            Err(reason) => Outcome::unsupported(reason),
        },
        Err(error) => Outcome::Failure {
            kind: if matches!(error, Error::OutOfExError(_)) {
                FailureKind::BudgetExhausted
            } else {
                FailureKind::Evaluation
            },
            budget: Some(budget),
            traces,
            diagnostic: error.to_string(),
        },
    }
}

fn normalize(term: &Term<NamedDeBruijn>, depth: usize) -> Result<Value, String> {
    if depth > 512 {
        return Err("result normalization depth exceeds 512".into());
    }
    let n = |term: &Term<NamedDeBruijn>| normalize(term, depth + 1);
    Ok(match term {
        Term::Var(index) => json!(["var", index.index.inner().to_string()]),
        Term::Lambda { body, .. } => json!(["lambda", n(body)?]),
        Term::Apply { function, argument } => json!(["apply", n(function)?, n(argument)?]),
        Term::Delay(term) => json!(["delay", n(term)?]),
        Term::Force(term) => json!(["force", n(term)?]),
        Term::Error => json!(["error"]),
        Term::Builtin(builtin) => json!(["builtin", (*builtin as u8).to_string()]),
        Term::Constr { tag, fields } => json!([
            "constr",
            tag.to_string(),
            fields.iter().map(n).collect::<Result<Vec<_>, _>>()?
        ]),
        Term::Case { constr, branches } => json!([
            "case",
            n(constr)?,
            branches.iter().map(n).collect::<Result<Vec<_>, _>>()?
        ]),
        Term::Constant(value) => json!(["constant", constant(value, depth + 1)?]),
    })
}

fn constant(value: &Constant, depth: usize) -> Result<Value, String> {
    if depth > 512 {
        return Err("constant normalization depth exceeds 512".into());
    }
    Ok(match value {
        Constant::Integer(value) => json!(["integer", value.to_string()]),
        Constant::ByteString(value) => json!(["bytes", hex::encode(value)]),
        Constant::String(value) => json!(["string", value]),
        Constant::Bool(value) => json!(["bool", value]),
        Constant::Unit => json!(["unit"]),
        Constant::ProtoList(typ, values) => json!([
            "list",
            type_name(typ, depth + 1)?,
            values
                .iter()
                .map(|v| constant(v, depth + 1))
                .collect::<Result<Vec<_>, _>>()?
        ]),
        Constant::ProtoPair(a, b, x, y) => json!([
            "pair",
            type_name(a, depth + 1)?,
            type_name(b, depth + 1)?,
            constant(x, depth + 1)?,
            constant(y, depth + 1)?
        ]),
        _ => {
            return Err(
                "result normalization for Data, BLS elements, and ledger Value is not implemented"
                    .into(),
            );
        }
    })
}

fn type_name(typ: &Type, depth: usize) -> Result<Value, String> {
    if depth > 512 {
        return Err("type normalization depth exceeds 512".into());
    }
    Ok(match typ {
        Type::Integer => json!("integer"),
        Type::ByteString => json!("bytes"),
        Type::String => json!("string"),
        Type::Bool => json!("bool"),
        Type::Unit => json!("unit"),
        Type::List(inner) => json!(["list", type_name(inner, depth + 1)?]),
        Type::Pair(a, b) => json!(["pair", type_name(a, depth + 1)?, type_name(b, depth + 1)?]),
        _ => {
            return Err(
                "result type normalization is not implemented for this builtin type".into(),
            );
        }
    })
}

fn main() -> std::io::Result<()> {
    let mode = std::env::args().nth(1);
    if mode.as_deref() == Some("--encode-flat") {
        return uplc_conformance::serve("aiken-flat-encoder", REVISION, encode_flat);
    }
    let normalize_only = mode.as_deref() == Some("--normalize");
    uplc_conformance::serve(
        if normalize_only {
            "aiken-normalizer"
        } else {
            "aiken"
        },
        REVISION,
        |request| evaluate(request, normalize_only),
    )
}
