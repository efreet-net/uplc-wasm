//! Errors distinguish invalid programs from valid features outside this milestone.

use std::fmt;

use crate::builtin::{ArgumentType, Builtin};

/// Raw decoding never treats an implementation resource limit as a bad program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Malformed(String),
    Unsupported(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(reason) => write!(formatter, "malformed Flat program: {reason}"),
            Self::Unsupported(reason) => write!(formatter, "unsupported program: {reason}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Semantic CEK failures are separate from evaluator and result-format limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    ExplicitError,
    OpenTerm {
        index: u64,
        environment_size: usize,
    },
    NonFunctionApplication,
    NonDelayForce,
    BuiltinArity {
        builtin: Builtin,
        expected: usize,
        actual: usize,
    },
    BuiltinTypeMismatch {
        builtin: Builtin,
        argument: usize,
        expected: ArgumentType,
    },
    BuiltinIntegerOutOfBounds {
        builtin: Builtin,
        argument: usize,
    },
    BuiltinDivisionByZero {
        builtin: Builtin,
    },
    Unsupported(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitError => formatter.write_str("explicit UPLC error"),
            Self::OpenTerm {
                index,
                environment_size,
            } => write!(
                formatter,
                "unbound De Bruijn index {index} in environment of size {environment_size}"
            ),
            Self::NonFunctionApplication => formatter.write_str("application of a non-function"),
            Self::NonDelayForce => formatter.write_str("force of a non-delay"),
            Self::BuiltinArity {
                builtin,
                expected,
                actual,
            } => write!(
                formatter,
                "{builtin} expects {expected} arguments, got {actual}"
            ),
            Self::BuiltinTypeMismatch {
                builtin,
                argument,
                expected,
            } => write!(
                formatter,
                "{builtin} argument {} must be {expected}",
                argument + 1
            ),
            Self::BuiltinIntegerOutOfBounds { builtin, argument } => write!(
                formatter,
                "{builtin} argument {} is outside the profile's signed integer range",
                argument + 1
            ),
            Self::BuiltinDivisionByZero { builtin } => {
                write!(formatter, "{builtin}: division by zero")
            }
            Self::Unsupported(reason) => write!(formatter, "unsupported evaluation: {reason}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
