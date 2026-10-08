//! Independent UPLC syntax for the first evaluator milestone.
//!
//! Terms live in a flat arena: malformed or deep input cannot trigger recursive
//! AST destruction. The indices are arena locations, while `Var` contains the
//! UPLC one-based De Bruijn index. A directly constructed AST can contain a zero
//! index, which is an open-term runtime error if evaluated. The raw decoder and
//! the wire protocol's normalized-term schema reject zero variable indices.

use num_bigint::BigInt;
use serde_json::{Value, json};

use crate::{
    error::{DecodeError, RuntimeError},
    limits::{
        MAX_AST_DEPTH, MAX_AST_NODES, MAX_CONSTANT_BYTES, MAX_INTEGER_BYTES, MAX_OUTPUT_BYTES,
        MAX_OUTPUT_DEPTH, MAX_OUTPUT_NODES,
    },
};

pub type TermId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constant {
    Integer(BigInt),
    ByteString(Vec<u8>),
    String(String),
    Bool(bool),
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Var(u64),
    Delay(TermId),
    Lambda(TermId),
    Apply { function: TermId, argument: TermId },
    Constant(Constant),
    Force(TermId),
    Error,
}

impl Term {
    /// Children in source order, without recursive syntax ownership.
    pub fn children(&self) -> impl Iterator<Item = TermId> {
        match *self {
            Self::Delay(body) | Self::Lambda(body) | Self::Force(body) => [Some(body), None],
            Self::Apply { function, argument } => [Some(function), Some(argument)],
            _ => [None, None],
        }
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub version: [u64; 3],
    pub terms: Vec<Term>,
    pub root: TermId,
}

impl Program {
    pub fn new(version: [u64; 3], terms: Vec<Term>, root: TermId) -> Result<Self, DecodeError> {
        let program = Self {
            version,
            terms,
            root,
        };
        program.validate()?;
        Ok(program)
    }

    /// Check arena integrity and implementation bounds, including unreachable
    /// arena entries. This deliberately does not require a closed term or gate
    /// the program version; those are evaluator/decoder responsibilities.
    pub fn validate(&self) -> Result<(), DecodeError> {
        if self.terms.len() > MAX_AST_NODES {
            return Err(DecodeError::Unsupported(format!(
                "AST exceeds {MAX_AST_NODES} nodes"
            )));
        }
        if self.root >= self.terms.len() {
            return Err(DecodeError::Malformed("invalid root arena index".into()));
        }
        for term in &self.terms {
            if term.children().any(|child| child >= self.terms.len()) {
                return Err(DecodeError::Malformed("invalid child arena index".into()));
            }
            if let Term::Constant(constant) = term {
                constant.validate()?;
            }
        }

        // 0 = unseen, 1 = active DFS path, 2 = complete. Depths are computed
        // bottom-up, so a shared child remains valid regardless of arena order.
        let mut state = vec![0_u8; self.terms.len()];
        let mut depths = vec![0_usize; self.terms.len()];
        let mut pending = Vec::new();
        for start in 0..self.terms.len() {
            if state[start] == 2 {
                continue;
            }
            pending.push((start, false));
            while let Some((id, exiting)) = pending.pop() {
                if exiting {
                    let depth = self.terms[id]
                        .children()
                        .map(|child| depths[child] + 1)
                        .max()
                        .unwrap_or(0);
                    if depth > MAX_AST_DEPTH {
                        return Err(DecodeError::Unsupported(format!(
                            "AST depth exceeds {MAX_AST_DEPTH}"
                        )));
                    }
                    depths[id] = depth;
                    state[id] = 2;
                } else {
                    match state[id] {
                        1 => return Err(DecodeError::Malformed("cyclic term arena".into())),
                        2 => continue,
                        _ => {}
                    }
                    state[id] = 1;
                    pending.push((id, true));
                    for child in self.terms[id].children() {
                        pending.push((child, false));
                    }
                }
            }
        }
        Ok(())
    }

    /// Structural normalization only: no evaluation, name resolution, or
    /// reduction under lambdas. Machine discharge must substitute environments
    /// before calling this method.
    pub fn normalize(&self) -> Result<Value, RuntimeError> {
        self.validate()
            .map_err(|error| RuntimeError::Unsupported(error.to_string()))?;
        self.check_output_bounds()?;

        // Build only after preflight. This keeps partially built nested JSON
        // values safely bounded even if the arena describes an exponential DAG.
        let mut pending = vec![(self.root, false)];
        let mut values = Vec::new();
        while let Some((id, exiting)) = pending.pop() {
            let term = &self.terms[id];
            if !exiting {
                match term {
                    Term::Apply { function, argument } => {
                        pending.push((id, true));
                        pending.push((*argument, false));
                        pending.push((*function, false));
                    }
                    Term::Delay(body) | Term::Lambda(body) | Term::Force(body) => {
                        pending.push((id, true));
                        pending.push((*body, false));
                    }
                    Term::Constant(constant) => {
                        values.push(json!(["constant", constant.normalize()]));
                    }
                    Term::Var(index) => values.push(json!(["var", index.to_string()])),
                    Term::Error => values.push(json!(["error"])),
                }
                continue;
            }
            let value = match term {
                Term::Apply { .. } => {
                    let argument = values.pop().expect("visited argument");
                    let function = values.pop().expect("visited function");
                    json!(["apply", function, argument])
                }
                Term::Delay(_) | Term::Lambda(_) | Term::Force(_) => {
                    let tag = match term {
                        Term::Delay(_) => "delay",
                        Term::Lambda(_) => "lambda",
                        Term::Force(_) => "force",
                        _ => unreachable!(),
                    };
                    let body = values.pop().expect("visited body");
                    json!([tag, body])
                }
                _ => unreachable!("only parent terms have exit tasks"),
            };
            values.push(value);
        }
        Ok(values.pop().expect("validated nonempty program"))
    }

    fn check_output_bounds(&self) -> Result<(), RuntimeError> {
        let unsupported = |reason| RuntimeError::Unsupported(reason);
        let mut pending = vec![(self.root, 0_usize)];
        let mut nodes = 0_usize;
        let mut bytes = 0_usize;
        while let Some((id, depth)) = pending.pop() {
            nodes += 1;
            if nodes > MAX_OUTPUT_NODES {
                return Err(unsupported(format!(
                    "normalized result exceeds {MAX_OUTPUT_NODES} nodes"
                )));
            }
            if depth > MAX_OUTPUT_DEPTH {
                return Err(unsupported(format!(
                    "normalized result depth exceeds {MAX_OUTPUT_DEPTH}"
                )));
            }
            // Count exact JSON bytes without materializing the expanded tree.
            // Constants are primitive; their temporary JSON value is shallow.
            bytes += match &self.terms[id] {
                Term::Var(0) => {
                    return Err(unsupported(
                        "the normalized-term schema cannot represent De Bruijn index zero".into(),
                    ));
                }
                Term::Var(index) => 10 + index.to_string().len(),
                Term::Delay(_) | Term::Force(_) => 10,
                Term::Lambda(_) => 11,
                Term::Apply { .. } => 11,
                Term::Error => 9,
                Term::Constant(constant) => {
                    if depth + 1 > MAX_OUTPUT_DEPTH {
                        return Err(unsupported(format!(
                            "normalized constant depth exceeds {MAX_OUTPUT_DEPTH}"
                        )));
                    }
                    let encoded = serde_json::to_vec(&constant.normalize())
                        .expect("primitive constants are JSON serializable");
                    13 + encoded.len()
                }
            };
            if bytes > MAX_OUTPUT_BYTES {
                return Err(unsupported(format!(
                    "normalized result exceeds {MAX_OUTPUT_BYTES} serialized bytes"
                )));
            }
            pending.extend(self.terms[id].children().map(|child| (child, depth + 1)));
        }
        Ok(())
    }
}

impl Constant {
    fn validate(&self) -> Result<(), DecodeError> {
        let too_large = match self {
            Self::Integer(value) => value.bits() > (MAX_INTEGER_BYTES as u64) * 8,
            Self::ByteString(value) => value.len() > MAX_CONSTANT_BYTES,
            Self::String(value) => value.len() > MAX_CONSTANT_BYTES,
            Self::Bool(_) | Self::Unit => false,
        };
        if too_large {
            return Err(DecodeError::Unsupported(
                "primitive constant exceeds the implementation size bound".into(),
            ));
        }
        Ok(())
    }

    fn normalize(&self) -> Value {
        match self {
            Self::Integer(value) => json!(["integer", value.to_string()]),
            Self::ByteString(value) => json!(["bytes", hex::encode(value)]),
            Self::String(value) => json!(["string", value]),
            Self::Bool(value) => json!(["bool", value]),
            Self::Unit => json!(["unit"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(terms: Vec<Term>, root: TermId) -> Program {
        Program::new([1, 0, 0], terms, root).unwrap()
    }

    #[test]
    fn primitive_constants_preserve_structure_and_integer_precision() {
        let decimal = "-123456789012345678901234567890123456789012345678901234567890";
        let cases = [
            (
                Constant::Integer(decimal.parse().unwrap()),
                json!(["integer", decimal]),
            ),
            (Constant::ByteString(vec![0, 255]), json!(["bytes", "00ff"])),
            (
                Constant::String("\0\"\\\nλ🙂".into()),
                json!(["string", "\0\"\\\nλ🙂"]),
            ),
            (Constant::Bool(false), json!(["bool", false])),
            (Constant::Bool(true), json!(["bool", true])),
            (Constant::Unit, json!(["unit"])),
        ];
        for (constant, expected) in cases {
            let value = program(vec![Term::Constant(constant)], 0)
                .normalize()
                .unwrap();
            assert_eq!(value, json!(["constant", expected]));
            assert_eq!(
                serde_json::from_str::<Value>(&value.to_string()).unwrap(),
                value
            );
        }
    }

    #[test]
    fn normalization_preserves_unevaluated_syntax_and_child_order() {
        let ast = program(
            vec![
                Term::Var(u64::MAX),
                Term::Lambda(0),
                Term::Error,
                Term::Delay(2),
                Term::Force(3),
                Term::Apply {
                    function: 1,
                    argument: 4,
                },
            ],
            5,
        );
        assert_eq!(
            ast.normalize().unwrap(),
            json!([
                "apply",
                ["lambda", ["var", "18446744073709551615"]],
                ["force", ["delay", ["error"]]]
            ])
        );
    }

    #[test]
    fn validation_checks_all_arena_entries_without_requiring_closed_terms() {
        assert!(Program::new([1, 0, 0], vec![Term::Var(0)], 0).is_ok());
        assert!(Program::new([1, 0, 0], vec![Term::Var(u64::MAX)], 0).is_ok());
        for (terms, root) in [
            (vec![], 0),
            (vec![Term::Error], 1),
            (vec![Term::Lambda(1)], 0),
            (vec![Term::Lambda(0)], 0),
            (vec![Term::Error, Term::Lambda(2), Term::Delay(1)], 0),
        ] {
            assert!(matches!(
                Program::new([1, 0, 0], terms, root),
                Err(DecodeError::Malformed(_))
            ));
        }
    }

    #[test]
    fn shared_children_and_forward_arena_references_are_valid() {
        let ast = program(
            vec![
                Term::Apply {
                    function: 2,
                    argument: 1,
                },
                Term::Delay(2),
                Term::Constant(Constant::Unit),
            ],
            0,
        );
        assert_eq!(
            ast.normalize().unwrap(),
            json!([
                "apply",
                ["constant", ["unit"]],
                ["delay", ["constant", ["unit"]]]
            ])
        );
    }

    #[test]
    fn deep_arenas_and_excessive_constants_are_unsupported() {
        let mut terms = vec![Term::Error];
        for child in 0..MAX_AST_DEPTH {
            terms.push(Term::Lambda(child));
        }
        assert!(Program::new([1, 0, 0], terms.clone(), MAX_AST_DEPTH).is_ok());
        terms.push(Term::Lambda(MAX_AST_DEPTH));
        assert!(matches!(
            Program::new([1, 0, 0], terms, MAX_AST_DEPTH + 1),
            Err(DecodeError::Unsupported(_))
        ));
        for constant in [
            Constant::ByteString(vec![0; MAX_CONSTANT_BYTES + 1]),
            Constant::String("x".repeat(MAX_CONSTANT_BYTES + 1)),
            Constant::Integer(BigInt::from(1) << (MAX_INTEGER_BYTES * 8)),
        ] {
            assert!(matches!(
                Program::new([1, 0, 0], vec![Term::Constant(constant)], 0),
                Err(DecodeError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn normalization_rejects_zero_variables_and_bounds_expanded_dags() {
        assert!(matches!(
            program(vec![Term::Var(0)], 0).normalize(),
            Err(RuntimeError::Unsupported(_))
        ));
        let mut terms = vec![Term::Error];
        for child in 0..20 {
            terms.push(Term::Apply {
                function: child,
                argument: child,
            });
        }
        assert!(matches!(
            program(terms, 20).normalize(),
            Err(RuntimeError::Unsupported(reason)) if reason.contains("nodes")
        ));

        let mut terms = vec![Term::Constant(Constant::String(
            "\0".repeat(MAX_CONSTANT_BYTES),
        ))];
        terms.push(Term::Apply {
            function: 0,
            argument: 0,
        });
        assert!(matches!(
            program(terms, 1).normalize(),
            Err(RuntimeError::Unsupported(reason)) if reason.contains("serialized bytes")
        ));
    }

    #[test]
    fn normalization_depth_boundary_serializes_and_drops_safely() {
        let mut terms = vec![Term::Error];
        for child in 0..MAX_OUTPUT_DEPTH {
            terms.push(Term::Delay(child));
        }
        let at_limit = program(terms.clone(), MAX_OUTPUT_DEPTH)
            .normalize()
            .unwrap();
        assert_eq!(at_limit.to_string().len(), 9 + 10 * MAX_OUTPUT_DEPTH);
        drop(at_limit);
        terms.push(Term::Delay(MAX_OUTPUT_DEPTH));
        assert!(matches!(
            program(terms, MAX_OUTPUT_DEPTH + 1).normalize(),
            Err(RuntimeError::Unsupported(_))
        ));
    }
}
