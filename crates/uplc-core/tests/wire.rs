//! Wire-level expectations come from committed independent goldens, never from
//! the candidate. Hand-built depth probes below use the Flat specification's
//! 4-bit term tags and 0*1 final filler; their CEK costs are counted explicitly.

use serde_json::{Value, json};
use uplc_conformance::{Budget, Mode, Outcome, Request, Response, parameters_hash};
use uplc_core::evaluate;

const MILESTONE: &str = include_str!("../../../fixtures/milestone.jsonl");
const DECODER: &str = include_str!("../../../fixtures/milestone-decoder.jsonl");
const UNSUPPORTED: &str = include_str!("../../../fixtures/milestone-unsupported.jsonl");

fn cases(corpus: &str) -> impl Iterator<Item = Value> + '_ {
    corpus
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
}

fn request(case: &Value) -> Request {
    let mut profile: Value =
        serde_json::from_str(include_str!("../../../profiles/plutus-v3-pv11.json")).unwrap();
    profile.as_object_mut().unwrap().retain(|key, _| {
        matches!(
            key.as_str(),
            "id" | "language" | "protocol_major" | "cost_model"
        )
    });
    serde_json::from_value(json!({
        "schema_version": 1, "id": case["id"], "program": case["program"],
        "profile": profile, "mode": case["mode"],
    }))
    .unwrap()
}

fn constant_request() -> Request {
    request(&cases(MILESTONE).next().unwrap())
}

fn rehash(request: &mut Request) {
    request.profile.cost_model.sha256 = parameters_hash(&request.profile.cost_model.parameters);
}

fn wire(request: &Request) -> Value {
    let response: Response = serde_json::from_str(&uplc_core::evaluate_json(
        &serde_json::to_string(request).unwrap(),
    ))
    .unwrap();
    assert_eq!(response.id, request.id);
    assert_eq!(response.schema_version, 1);
    assert_eq!(response.engine, "uplc-core");
    assert!(!response.revision.is_empty());
    serde_json::to_value(response.outcome).unwrap()
}

#[test]
fn wire_matches_all_independent_semantic_cost_and_decoder_goldens() {
    let mut counts = [0, 0];
    for (index, corpus) in [MILESTONE, DECODER].into_iter().enumerate() {
        for case in cases(corpus) {
            let actual = wire(&request(&case));
            for (key, expected) in case["expected"].as_object().unwrap() {
                assert_eq!(actual[key], *expected, "{}: {key}", case["id"]);
            }
            assert_eq!(actual["traces"], json!([]), "{}", case["id"]);
            if actual["kind"] == "decode" {
                assert!(actual["budget"].is_null(), "{}", case["id"]);
            }
            counts[index] += 1;
        }
    }
    assert_eq!(counts, [68, 18]);
}

#[test]
fn future_features_are_unsupported_even_when_hidden_inside_values() {
    let mut count = 0;
    for case in cases(UNSUPPORTED) {
        // Retain the original deferred fixture bytes and provenance. The bare
        // addInteger value has now graduated to supported structural syntax.
        if case["id"] == "milestone/unsupported/builtin" {
            let outcome = wire(&request(&case));
            assert_eq!(outcome["status"], "success");
            assert_eq!(outcome["term"], json!(["builtin", "0"]));
            continue;
        }
        assert_eq!(
            wire(&request(&case))["status"],
            "unsupported",
            "{}",
            case["id"]
        );
        count += 1;
    }
    assert_eq!(count, 6);
    // lambda (builtin divideInteger): deferred builtins remain unsupported.
    let mut request = constant_request();
    request.program = uplc_conformance::Program::Flat {
        hex: raw_flat("001001110000011"),
    };
    assert_eq!(wire(&request)["status"], "unsupported");
    // Supported builtin syntax is discharged without evaluating the body.
    request.program = uplc_conformance::Program::Flat {
        hex: raw_flat("001001110000000"),
    };
    let outcome = wire(&request);
    assert_eq!(outcome["status"], "success");
    assert_eq!(outcome["term"], json!(["lambda", ["builtin", "0"]]));
}

#[test]
fn explicit_custom_coefficients_and_profile_labels_are_respected() {
    let mut request = constant_request();
    let original = wire(&request);
    request.profile.id = "a descriptive label cannot choose defaults".into();
    assert_eq!(wire(&request), original);

    request.profile.cost_model.parameters[21] = "9007199254740993".into();
    request.profile.cost_model.parameters[22] = "9007199254740993".into();
    request.mode = Mode::Restricting {
        budget: Budget::new(i64::MAX, i64::MAX),
    };
    rehash(&mut request);
    let actual = wire(&request);
    assert_eq!(actual["status"], "success");
    assert_eq!(actual["term"], original["term"]);
    assert_eq!(
        actual["budget"],
        json!({"cpu": "9007199254741093", "mem": "9007199254741093"})
    );

    request.profile.cost_model.parameters[21] = i64::MAX.to_string();
    rehash(&mut request);
    let actual = wire(&request);
    assert_eq!(actual["status"], "failure");
    assert_eq!(actual["kind"], "budget_exhausted");
    assert!(actual["budget"].is_null());
}

#[test]
fn invalid_models_and_unsupported_profiles_cannot_fall_back_to_defaults() {
    let original = constant_request();
    let mut request = original.clone();
    request.profile.cost_model.parameters[21] = "1".into();
    assert_eq!(wire(&request)["status"], "infrastructure_error");
    request = original.clone();
    request.profile.cost_model.parameters.pop();
    rehash(&mut request);
    assert_eq!(wire(&request)["status"], "infrastructure_error");
    for index in [17, 21, 29, 193, 196] {
        request = original.clone();
        request.profile.cost_model.parameters[index] = "-1".into();
        rehash(&mut request);
        assert_eq!(wire(&request)["status"], "unsupported");
    }
    // Builtin polynomial coefficients can be negative and are irrelevant to
    // this slice. They must not be mistaken for negative machine-step charges.
    request = original.clone();
    request.profile.cost_model.parameters[52] = i64::MIN.to_string();
    rehash(&mut request);
    assert_eq!(wire(&request), wire(&original));

    request = original.clone();
    request.profile.protocol_major = 10;
    assert_eq!(wire(&request)["status"], "unsupported");
    request = original.clone();
    request.profile.language = uplc_conformance::Language::PlutusV2;
    assert_eq!(wire(&request)["status"], "unsupported");
    request = original.clone();
    request.mode = Mode::Counting {};
    assert_eq!(wire(&request)["status"], "unsupported");
    request = original;
    request.program = uplc_conformance::Program::UplcText {
        source: "(program 1.0.0 (con integer 0))".into(),
    };
    assert_eq!(wire(&request)["status"], "unsupported");
}

#[test]
fn failure_charging_and_zero_cost_success_are_explicit_on_the_wire() {
    let mut request = constant_request();
    request.mode = Mode::Restricting {
        budget: Budget::new(0, 0),
    };
    let actual = wire(&request);
    assert_eq!(actual["kind"], "budget_exhausted");
    assert_eq!(actual["budget"], json!({"cpu": "100", "mem": "100"}));
    for index in (17..=32).chain(193..=196) {
        request.profile.cost_model.parameters[index] = "0".into();
    }
    rehash(&mut request);
    let actual = wire(&request);
    assert_eq!(actual["status"], "success");
    assert_eq!(actual["budget"], json!({"cpu": "0", "mem": "0"}));

    // Force(Error) accrues a force event but fails before a successful flush.
    request = constant_request();
    request.program = uplc_conformance::Program::Flat {
        hex: raw_flat("01010110"),
    };
    let actual = wire(&request);
    assert_eq!(actual["kind"], "evaluation");
    assert_eq!(actual["budget"], json!({"cpu": "100", "mem": "100"}));
}

fn raw_flat(term_bits: &str) -> String {
    let mut bits = term_bits.to_owned();
    bits.push_str(&"0".repeat(7 - bits.len() % 8));
    bits.push('1');
    let mut bytes = vec![1, 0, 0];
    bytes.extend(
        bits.as_bytes()
            .chunks_exact(8)
            .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 2).unwrap()),
    );
    hex::encode(bytes)
}

#[test]
fn depth_boundaries_are_portable_and_return_unsupported_not_semantic_failures() {
    let mut request = constant_request();
    // 256 nested force(delay(...)) pairs have AST depth 512 and compute
    // 513 costed events including the final unit constant.
    request.program = uplc_conformance::Program::Flat {
        hex: raw_flat(&("01010001".repeat(256) + "0100100110")),
    };
    let actual = wire(&request);
    assert_eq!(actual["term"], json!(["constant", ["unit"]]));
    assert_eq!(actual["budget"], json!({"cpu": "8208100", "mem": "51400"}));
    request.program = uplc_conformance::Program::Flat {
        hex: raw_flat(&("01010001".repeat(256) + "01010100100110")),
    };
    let actual = wire(&request);
    assert_eq!(actual["status"], "unsupported");
    assert!(actual["reason"].as_str().unwrap().contains("AST depth"));

    for (depth, supported) in [(128, true), (129, false)] {
        request.program = uplc_conformance::Program::Flat {
            hex: raw_flat(&("0010".repeat(depth) + "0110")),
        };
        // The actual API serialization is exercised here too; Rust's JSON
        // deserializer has a separate default nesting bound, so inspect the
        // typed outcome and ensure dispatch serializes without a recursive trap.
        let outcome = evaluate(&request);
        assert_eq!(matches!(outcome, Outcome::Success { .. }), supported);
        let serialized = uplc_core::evaluate_json(&serde_json::to_string(&request).unwrap());
        assert!(serialized.contains(if supported {
            "\"status\":\"success\""
        } else {
            "\"status\":\"unsupported\""
        }));
    }
}
