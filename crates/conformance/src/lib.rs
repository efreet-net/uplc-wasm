//! Versioned wire protocol shared by native, Wasm, and independent reference adapters.
//! Integers cross the JSON boundary as decimal strings, never floating-point numbers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;
/// Maximum UTF-8 bytes in the JSON body, excluding a native JSONL framing LF.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub id: String,
    pub program: Program,
    pub profile: Profile,
    pub mode: Mode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum Program {
    Flat { hex: String },
    UplcText { source: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub language: Language,
    pub protocol_major: u16,
    pub cost_model: CostModel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Language {
    PlutusV1,
    PlutusV2,
    PlutusV3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostModel {
    /// Ordered ledger API parameter vector. Ordering is part of the profile.
    pub parameters: Vec<String>,
    /// SHA-256 of `[p0,p1,...]` with ASCII decimal coefficients and no whitespace.
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mode {
    // An empty struct variant makes Serde enforce deny_unknown_fields. An
    // internally tagged unit variant silently discards extra fields.
    Counting {},
    Restricting { budget: Budget },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub cpu: String,
    pub mem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub schema_version: u32,
    pub id: String,
    pub engine: String,
    pub revision: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Success {
        /// Structural, name-free term; see docs/protocol.md.
        term: Value,
        budget: Budget,
        traces: Vec<String>,
    },
    Failure {
        kind: FailureKind,
        budget: Option<Budget>,
        traces: Vec<String>,
        diagnostic: String,
    },
    Unsupported {
        reason: String,
    },
    InfrastructureError {
        diagnostic: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Decode,
    Evaluation,
    BudgetExhausted,
}

pub fn parameters_hash(parameters: &[String]) -> String {
    hex::encode(Sha256::digest(
        format!("[{}]", parameters.join(",")).as_bytes(),
    ))
}

fn decimal_i64(value: &str) -> Result<i64, String> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| "expected an i64 decimal string")?;
    if parsed.to_string() != value {
        return Err("decimal strings must be canonical (no +, leading zeros, or -0)".into());
    }
    Ok(parsed)
}

impl Budget {
    pub fn new(cpu: i64, mem: i64) -> Self {
        Self {
            cpu: cpu.to_string(),
            mem: mem.to_string(),
        }
    }

    pub fn limits(&self) -> Result<(i64, i64), String> {
        let cpu = decimal_i64(&self.cpu)?;
        let mem = decimal_i64(&self.mem)?;
        if cpu < 0 || mem < 0 {
            return Err("budget limits cannot be negative".into());
        }
        Ok((cpu, mem))
    }
}

impl Request {
    pub fn validate(&self) -> Result<Vec<i64>, String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!("unsupported wire schema {}", self.schema_version));
        }
        if self.id.is_empty() || self.profile.id.is_empty() {
            return Err("request and profile IDs must be nonempty".into());
        }
        let parameters = &self.profile.cost_model.parameters;
        if parameters.is_empty() || parameters.len() > 4096 {
            return Err(
                "an explicit cost-model vector of 1..=4096 coefficients is required".into(),
            );
        }
        let parsed = parameters
            .iter()
            .map(|p| decimal_i64(p))
            .collect::<Result<Vec<_>, _>>()?;
        if parameters_hash(parameters) != self.profile.cost_model.sha256 {
            return Err("cost-model SHA-256 does not match its parameters".into());
        }
        if let Mode::Restricting { budget } = &self.mode {
            budget.limits()?;
        }
        if let Program::Flat { hex } = &self.program {
            hex::decode(hex).map_err(|_| "Flat transport must contain hexadecimal bytes")?;
        }
        Ok(parsed)
    }
}

impl Outcome {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported {
            reason: reason.into(),
        }
    }

    pub fn infrastructure(diagnostic: impl Into<String>) -> Self {
        Self::InfrastructureError {
            diagnostic: diagnostic.into(),
        }
    }

    pub fn failure(kind: FailureKind, diagnostic: impl Into<String>) -> Self {
        Self::Failure {
            kind,
            budget: None,
            traces: vec![],
            diagnostic: diagnostic.into(),
        }
    }
}

/// The same transport entry point is used by the native executable and Wasm export.
pub fn dispatch(
    input: &str,
    engine: &str,
    revision: &str,
    eval: impl FnOnce(&Request) -> Outcome,
) -> String {
    let mut id = String::new();
    let outcome = if input.len() > MAX_REQUEST_BYTES {
        Outcome::infrastructure("request exceeds the transport size limit")
    } else {
        match serde_json::from_str::<Request>(input) {
            Ok(request) => {
                id = request.id.clone();
                match request.validate() {
                    Ok(_) => eval(&request),
                    Err(error) => Outcome::infrastructure(format!("invalid request: {error}")),
                }
            }
            Err(error) => Outcome::infrastructure(format!("invalid JSON request: {error}")),
        }
    };
    serde_json::to_string(&Response {
        schema_version: SCHEMA_VERSION,
        id,
        engine: engine.into(),
        revision: revision.into(),
        outcome,
    })
    .expect("the wire schema is JSON serializable")
}

/// Native adapters serve one JSON object per line; stdout is reserved for responses.
#[cfg(not(target_family = "wasm"))]
pub fn serve(
    engine: &str,
    revision: &str,
    eval: impl Fn(&Request) -> Outcome + std::panic::RefUnwindSafe,
) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_streams(
        &mut stdin.lock(),
        &mut stdout.lock(),
        engine,
        revision,
        eval,
    )
}

#[cfg(not(target_family = "wasm"))]
fn serve_streams(
    input: &mut impl std::io::BufRead,
    output: &mut impl std::io::Write,
    engine: &str,
    revision: &str,
    eval: impl Fn(&Request) -> Outcome + std::panic::RefUnwindSafe,
) -> std::io::Result<()> {
    use std::io::{BufRead, Read};
    loop {
        let mut line = Vec::new();
        // One lookahead byte distinguishes a maximum-sized body followed by LF
        // from an overlong body. Never drain the rest of an overlong line.
        let count = input
            .by_ref()
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.len() > MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "JSONL request too large",
            ));
        }
        let line = std::str::from_utf8(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let response = dispatch(line, engine, revision, |request| {
            std::panic::catch_unwind(|| eval(request))
                .unwrap_or_else(|_| Outcome::infrastructure("adapter panicked"))
        });
        writeln!(output, "{response}")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maximum_request_body() -> String {
        let parameters = vec!["0".to_owned()];
        let request = Request {
            schema_version: 1,
            id: "body-size-boundary".into(),
            program: Program::Flat { hex: "".into() },
            profile: Profile {
                id: "multibyte-λ".into(),
                language: Language::PlutusV3,
                protocol_major: 11,
                cost_model: CostModel {
                    sha256: parameters_hash(&parameters),
                    parameters,
                },
            },
            mode: Mode::Restricting {
                budget: Budget::new(0, 0),
            },
        };
        let mut body = serde_json::to_string(&request).unwrap();
        body.push_str(&" ".repeat(MAX_REQUEST_BYTES - body.len()));
        assert_eq!(body.len(), MAX_REQUEST_BYTES);
        assert_eq!(body.chars().count(), MAX_REQUEST_BYTES - 1);
        body
    }

    fn transport_success(_: &Request) -> Outcome {
        Outcome::Success {
            term: serde_json::json!(["constant", ["unit"]]),
            budget: Budget::new(0, 0),
            traces: vec![],
        }
    }

    #[test]
    fn dispatch_request_limit_counts_utf8_body_bytes() {
        let mut body = maximum_request_body();
        let response: Response =
            serde_json::from_str(&dispatch(&body, "test", "test", transport_success)).unwrap();
        assert_eq!(response.id, "body-size-boundary");
        assert!(matches!(response.outcome, Outcome::Success { .. }));

        body.push(' ');
        // This has exactly MAX_REQUEST_BYTES Unicode scalar values, but one
        // more UTF-8 byte. It must fail before invoking evaluation.
        assert_eq!(body.chars().count(), MAX_REQUEST_BYTES);
        let response: Response = serde_json::from_str(&dispatch(&body, "test", "test", |_| {
            panic!("oversized request must not evaluate")
        }))
        .unwrap();
        assert!(matches!(
            response.outcome,
            Outcome::InfrastructureError { .. }
        ));
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn native_request_limit_excludes_lf_and_accepts_final_eof_body() {
        let body = maximum_request_body();
        let mut input = std::io::Cursor::new(format!("{body}\n{body}").into_bytes());
        let mut output = Vec::new();
        serve_streams(&mut input, &mut output, "test", "test", transport_success).unwrap();
        assert_eq!(input.position(), (2 * MAX_REQUEST_BYTES + 1) as u64);
        let output = String::from_utf8(output).unwrap();
        assert!(output.ends_with('\n'));
        let responses: Vec<Response> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 2);
        for response in responses {
            assert_eq!(response.id, "body-size-boundary");
            assert!(matches!(response.outcome, Outcome::Success { .. }));
        }
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn native_oversized_body_terminates_after_bounded_lookahead() {
        for ending in ["", "\n{}\n"] {
            let mut body = maximum_request_body();
            body.push(' ');
            body.push_str(ending);
            let mut input = std::io::Cursor::new(body.into_bytes());
            let mut output = Vec::new();
            let error = serve_streams(&mut input, &mut output, "test", "test", |_| {
                panic!("oversized request must not evaluate")
            })
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(input.position(), (MAX_REQUEST_BYTES + 1) as u64);
            assert!(output.is_empty());
        }
    }

    #[test]
    fn budgets_preserve_integers_beyond_javascript_precision() {
        let value = Budget::new(9_007_199_254_740_993, i64::MAX);
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"9007199254740993\""));
        assert_eq!(
            serde_json::from_str::<Budget>(&json)
                .unwrap()
                .limits()
                .unwrap(),
            (9_007_199_254_740_993, i64::MAX)
        );
    }

    #[test]
    fn malformed_limits_cannot_be_silently_coerced() {
        for cpu in ["+1", "01", "-0", "1.0", "-1", "9223372036854775808"] {
            assert!(
                Budget {
                    cpu: cpu.into(),
                    mem: "0".into()
                }
                .limits()
                .is_err()
            );
        }
        assert!(serde_json::from_str::<Budget>(r#"{"cpu":1,"mem":"0"}"#).is_err());
    }

    #[test]
    fn invalid_wire_request_never_invokes_the_machine() {
        let response = dispatch("{}", "test", "test", |_| panic!("must not evaluate"));
        assert!(matches!(
            serde_json::from_str::<Response>(&response).unwrap().outcome,
            Outcome::InfrastructureError { .. }
        ));
    }

    #[test]
    fn counting_mode_rejects_fields_instead_of_silently_ignoring_them() {
        assert!(serde_json::from_str::<Mode>(r#"{"kind":"counting"}"#).is_ok());
        for json in [
            r#"{"kind":"counting","budget":{"cpu":"1","mem":"1"}}"#,
            r#"{"kind":"counting","extra":true}"#,
        ] {
            assert!(serde_json::from_str::<Mode>(json).is_err());
        }
    }

    #[test]
    fn modified_parameters_cannot_reuse_a_profile_hash() {
        let parameters = vec!["1".into(), "-2".into(), "3".into()];
        let mut request = Request {
            schema_version: 1,
            id: "hash-check".into(),
            program: Program::Flat { hex: "".into() },
            profile: Profile {
                id: "test-model".into(),
                language: Language::PlutusV3,
                protocol_major: 11,
                cost_model: CostModel {
                    sha256: parameters_hash(&parameters),
                    parameters,
                },
            },
            mode: Mode::Restricting {
                budget: Budget::new(1, 1),
            },
        };
        assert!(request.validate().is_ok());
        request.profile.cost_model.parameters[1] = "2".into();
        assert!(request.validate().unwrap_err().contains("SHA-256"));
    }
}
