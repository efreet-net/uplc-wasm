//! Independent metadata and denotations for the supported builtin subset.
//!
//! Sources are pinned Plutus revision `5f785edeac0d1d89622d44344fdda07ef48e8c73`:
//! `PlutusCore/Default/Builtins.hs` (tags, signatures, denotations),
//! `PlutusCore/Default/Universe/Cardano.hs` and `Default/Universe.hs` (variant E
//! input bounds), and `Evaluation/Machine/ExMemoryUsage.hs` (64-bit memory units).
//! All eleven functions belong to the original builtin batch and are available
//! in PlutusV3 / protocol 11, whose builtin semantics variant is E.
//!
//! The CEK machine owns forcing, argument collection, charging, and opaque
//! values. It must call `validate_arguments` only at full saturation, before
//! costing: pinned `Builtin/Meaning.hs` defers unlifting until saturation and
//! gives unlifting failures zero builtin cost. A wrong-typed partial application
//! remains a value. Once validation succeeds, charge the builtin budget, debit
//! `work_units` from the independent work allowance, and only then `evaluate`.

use std::fmt;

use num_bigint::{BigInt, Sign};

use crate::{ast::Constant, error::RuntimeError, limits::MAX_INTEGER_BYTES};

/// The exponent in the profile E input range [-2^262143, 2^262143 - 1].
/// This is a semantic argument bound, separate from the implementation's 64 KiB
/// primitive integer limit. Equality and arithmetic results are not so bounded.
pub const CARDANO_INTEGER_MAXIMUM_BITS: u64 = 262_143;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    AddInteger,
    SubtractInteger,
    MultiplyInteger,
    DivideInteger,
    QuotientInteger,
    RemainderInteger,
    ModInteger,
    EqualsInteger,
    LessThanInteger,
    LessThanEqualsInteger,
    IfThenElse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentType {
    Integer,
    Bool,
    Any,
}

impl fmt::Display for ArgumentType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Integer => "integer",
            Self::Bool => "boolean",
            Self::Any => "any value",
        })
    }
}

/// A borrowed view; opaque arguments can be closures, delays, or partial
/// builtins. `ifThenElse` accepts either view for either branch.
#[derive(Debug, Clone, Copy)]
pub enum BuiltinArgument<'a> {
    Constant(&'a Constant),
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinResult {
    Constant(Constant),
    /// Return the already-evaluated argument at this zero-based position.
    Argument(usize),
}

impl Builtin {
    pub const fn tag(self) -> u8 {
        match self {
            Self::AddInteger => 0,
            Self::SubtractInteger => 1,
            Self::MultiplyInteger => 2,
            Self::DivideInteger => 3,
            Self::QuotientInteger => 4,
            Self::RemainderInteger => 5,
            Self::ModInteger => 6,
            Self::EqualsInteger => 7,
            Self::LessThanInteger => 8,
            Self::LessThanEqualsInteger => 9,
            Self::IfThenElse => 26,
        }
    }

    /// Known but unimplemented builtin tags are not represented in this enum.
    /// The decoder separately distinguishes those tags from invalid tags.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::AddInteger),
            1 => Some(Self::SubtractInteger),
            2 => Some(Self::MultiplyInteger),
            3 => Some(Self::DivideInteger),
            4 => Some(Self::QuotientInteger),
            5 => Some(Self::RemainderInteger),
            6 => Some(Self::ModInteger),
            7 => Some(Self::EqualsInteger),
            8 => Some(Self::LessThanInteger),
            9 => Some(Self::LessThanEqualsInteger),
            26 => Some(Self::IfThenElse),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::AddInteger => "addInteger",
            Self::SubtractInteger => "subtractInteger",
            Self::MultiplyInteger => "multiplyInteger",
            Self::DivideInteger => "divideInteger",
            Self::QuotientInteger => "quotientInteger",
            Self::RemainderInteger => "remainderInteger",
            Self::ModInteger => "modInteger",
            Self::EqualsInteger => "equalsInteger",
            Self::LessThanInteger => "lessThanInteger",
            Self::LessThanEqualsInteger => "lessThanEqualsInteger",
            Self::IfThenElse => "ifThenElse",
        }
    }

    pub const fn force_count(self) -> u8 {
        match self {
            Self::IfThenElse => 1,
            _ => 0,
        }
    }

    pub const fn arity(self) -> usize {
        match self {
            Self::IfThenElse => 3,
            _ => 2,
        }
    }

    /// The caller supplies an argument index strictly smaller than `arity()`.
    pub fn argument_type(self, index: usize) -> ArgumentType {
        assert!(index < self.arity(), "builtin argument index exceeds arity");
        match (self, index) {
            (Self::IfThenElse, 0) => ArgumentType::Bool,
            (Self::IfThenElse, _) => ArgumentType::Any,
            _ => ArgumentType::Integer,
        }
    }

    /// Validate a saturated application, in argument order. Do not call this
    /// while collecting a partial application: official unlifting is deferred.
    pub fn validate_arguments(self, args: &[BuiltinArgument<'_>]) -> Result<(), RuntimeError> {
        if args.len() != self.arity() {
            return Err(RuntimeError::BuiltinArity {
                builtin: self,
                expected: self.arity(),
                actual: args.len(),
            });
        }
        for (argument, value) in args.iter().enumerate() {
            let expected = self.argument_type(argument);
            match (expected, value) {
                (ArgumentType::Any, _)
                | (ArgumentType::Bool, BuiltinArgument::Constant(Constant::Bool(_))) => {}
                (ArgumentType::Integer, BuiltinArgument::Constant(Constant::Integer(integer))) => {
                    if integer.bits() > (MAX_INTEGER_BYTES as u64) * 8 {
                        return Err(integer_size_error());
                    }
                    if self != Self::EqualsInteger && !fits_cardano_integer_range(integer) {
                        return Err(RuntimeError::BuiltinIntegerOutOfBounds {
                            builtin: self,
                            argument,
                        });
                    }
                }
                _ => {
                    return Err(RuntimeError::BuiltinTypeMismatch {
                        builtin: self,
                        argument,
                        expected,
                    });
                }
            }
        }
        Ok(())
    }

    /// Portable implementation work units, separate from Plutus execution
    /// units. The machine debits these after the builtin charge and before
    /// execution. Maximum input word count bounds linear operations; the
    /// product of word counts accounts for multiplication. Division charges
    /// x*y + max(x,y), covering division and sign-adjustment traversal, including
    /// zero-divisor calls. No internal BigInt limb size or host pointer width
    /// participates. Even zero-cost models remain
    /// subject to the same cumulative work allowance.
    pub fn work_units(self, args: &[BuiltinArgument<'_>]) -> Result<usize, RuntimeError> {
        self.validate_arguments(args)?;
        if self == Self::IfThenElse {
            return Ok(1);
        }
        let (left, right) = integer_arguments(args);
        let left = integer_memory(left);
        let right = integer_memory(right);
        let work = match self {
            Self::MultiplyInteger => left.checked_mul(right),
            Self::DivideInteger
            | Self::QuotientInteger
            | Self::RemainderInteger
            | Self::ModInteger => left
                .checked_mul(right)
                .and_then(|product| product.checked_add(left.max(right))),
            _ => Some(left.max(right)),
        };
        work.and_then(|work| usize::try_from(work).ok())
            .ok_or_else(|| {
                RuntimeError::Unsupported("builtin implementation work bound exceeded".into())
            })
    }

    /// Compute only after the machine has successfully charged and debited
    /// work. Branch selection returns an index so opaque values keep their
    /// environments and runtime state without evaluating underneath them.
    pub fn evaluate(self, args: &[BuiltinArgument<'_>]) -> Result<BuiltinResult, RuntimeError> {
        self.validate_arguments(args)?;
        if self == Self::IfThenElse {
            let BuiltinArgument::Constant(Constant::Bool(condition)) = args[0] else {
                unreachable!("validated boolean argument")
            };
            return Ok(BuiltinResult::Argument(if *condition { 1 } else { 2 }));
        }
        let (left, right) = integer_arguments(args);
        let result = match self {
            Self::AddInteger => checked_integer_result(left + right)?,
            Self::SubtractInteger => checked_integer_result(left - right)?,
            Self::MultiplyInteger => {
                // Nonzero products have at least xbits + ybits - 1 bits.
                // Reject guaranteed oversized results before multiplication.
                // The remaining uncertain boundary can need at most one bit
                // beyond the result cap; all inputs were independently bounded.
                if left.bits() != 0
                    && right.bits() != 0
                    && left.bits() + right.bits() - 1 > (MAX_INTEGER_BYTES as u64) * 8
                {
                    return Err(integer_size_error());
                }
                checked_integer_result(left * right)?
            }
            Self::DivideInteger
            | Self::QuotientInteger
            | Self::RemainderInteger
            | Self::ModInteger => {
                // Pinned Builtins.hs uses nonZeroSecondArg in the denotation,
                // not during CInteger unlifting. The machine has already
                // charged the builtin and debited portable work at this point.
                if right.sign() == Sign::NoSign {
                    return Err(RuntimeError::BuiltinDivisionByZero { builtin: self });
                }
                let value = match self {
                    Self::DivideInteger => floor_quotient(left, right),
                    Self::QuotientInteger => left / right,
                    Self::RemainderInteger => left % right,
                    Self::ModInteger => {
                        let remainder = left % right;
                        if remainder.sign() != Sign::NoSign && remainder.sign() != right.sign() {
                            remainder + right
                        } else {
                            remainder
                        }
                    }
                    _ => unreachable!("division family"),
                };
                // All four E signatures return ordinary Integer. In particular
                // minBound / -1 is permitted even though its result cannot be
                // supplied to another CInteger argument.
                checked_integer_result(value)?
            }
            Self::EqualsInteger => Constant::Bool(left == right),
            Self::LessThanInteger => Constant::Bool(left < right),
            Self::LessThanEqualsInteger => Constant::Bool(left <= right),
            Self::IfThenElse => unreachable!("handled polymorphic builtin"),
        };
        Ok(BuiltinResult::Constant(result))
    }
}

fn floor_quotient(left: &BigInt, right: &BigInt) -> BigInt {
    if left.sign() == Sign::NoSign || left.sign() == right.sign() {
        return left / right;
    }
    // For a,b>0, floor(-a/b) = -trunc((a-1)/b)-1. Reduce the
    // numerator magnitude by one before truncating, so exact and inexact
    // divisions both need only one division and no oversized intermediate.
    let reduced = if left.sign() == Sign::Minus {
        left + 1
    } else {
        left - 1
    };
    reduced / right - 1
}

impl fmt::Display for Builtin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Plutus memory units are signed-magnitude integer bits grouped into 64-bit
/// words, with zero occupying one word: floor(log2(abs(n))/64)+1 for n != 0.
/// `BigInt::bits` is a representation-independent magnitude measurement.
pub fn integer_memory(value: &BigInt) -> u64 {
    value.bits().div_ceil(64).max(1)
}

pub fn fits_cardano_integer_range(value: &BigInt) -> bool {
    value.bits() <= CARDANO_INTEGER_MAXIMUM_BITS
        || (value.sign() == Sign::Minus
            && value.bits() == CARDANO_INTEGER_MAXIMUM_BITS + 1
            && value.trailing_zeros() == Some(CARDANO_INTEGER_MAXIMUM_BITS))
}

fn integer_arguments<'a>(args: &[BuiltinArgument<'a>]) -> (&'a BigInt, &'a BigInt) {
    let [
        BuiltinArgument::Constant(Constant::Integer(left)),
        BuiltinArgument::Constant(Constant::Integer(right)),
    ] = args
    else {
        unreachable!("validated integer arguments")
    };
    (left, right)
}

fn integer_size_error() -> RuntimeError {
    RuntimeError::Unsupported(format!(
        "builtin integer magnitude exceeds {MAX_INTEGER_BYTES} bytes"
    ))
}

fn checked_integer_result(value: BigInt) -> Result<Constant, RuntimeError> {
    if value.bits() > (MAX_INTEGER_BYTES as u64) * 8 {
        return Err(integer_size_error());
    }
    Ok(Constant::Integer(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTEGER_BUILTINS: [Builtin; 10] = [
        Builtin::AddInteger,
        Builtin::SubtractInteger,
        Builtin::MultiplyInteger,
        Builtin::DivideInteger,
        Builtin::QuotientInteger,
        Builtin::RemainderInteger,
        Builtin::ModInteger,
        Builtin::EqualsInteger,
        Builtin::LessThanInteger,
        Builtin::LessThanEqualsInteger,
    ];

    fn call(builtin: Builtin, left: BigInt, right: BigInt) -> Result<BuiltinResult, RuntimeError> {
        builtin.evaluate(&[
            BuiltinArgument::Constant(&Constant::Integer(left)),
            BuiltinArgument::Constant(&Constant::Integer(right)),
        ])
    }

    #[test]
    fn metadata_matches_pinned_tags_and_signatures() {
        for (builtin, tag) in INTEGER_BUILTINS.into_iter().zip(0..=9) {
            assert_eq!(builtin.tag(), tag);
            assert_eq!(Builtin::from_tag(tag), Some(builtin));
            assert_eq!(builtin.force_count(), 0);
            assert_eq!(builtin.arity(), 2);
            assert_eq!(builtin.argument_type(0), ArgumentType::Integer);
            assert_eq!(builtin.argument_type(1), ArgumentType::Integer);
        }
        let builtin = Builtin::IfThenElse;
        assert_eq!(builtin.tag(), 26);
        assert_eq!(Builtin::from_tag(26), Some(builtin));
        assert_eq!(builtin.force_count(), 1);
        assert_eq!(builtin.arity(), 3);
        assert_eq!(builtin.argument_type(0), ArgumentType::Bool);
        assert_eq!(builtin.argument_type(1), ArgumentType::Any);
        assert_eq!(builtin.argument_type(2), ArgumentType::Any);
        for tag in 0..=127 {
            assert_eq!(
                Builtin::from_tag(tag).is_some(),
                [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 26].contains(&tag)
            );
        }
    }

    #[test]
    fn integer_memory_is_portable_at_zero_and_signed_word_boundaries() {
        assert_eq!(integer_memory(&0.into()), 1);
        for (bits, words) in [
            (0, 1),
            (31, 1),
            (32, 1),
            (63, 1),
            (64, 2),
            (127, 2),
            (128, 3),
            (8191, 128),
        ] {
            let value = BigInt::from(1) << bits;
            assert_eq!(integer_memory(&value), words);
            assert_eq!(integer_memory(&-&value), words);
            if bits != 0 {
                assert_eq!(integer_memory(&(&value - 1)), (bits as u64).div_ceil(64));
            }
        }
        let largest: BigInt = (BigInt::from(1) << (MAX_INTEGER_BYTES * 8)) - 1;
        assert_eq!(integer_memory(&largest), (MAX_INTEGER_BYTES / 8) as u64);
    }

    #[test]
    fn integer_semantics_preserve_sign_and_arbitrary_precision() {
        let left: BigInt = "9007199254740993".parse().unwrap();
        let right: BigInt = "-18446744073709551616".parse().unwrap();
        for (builtin, decimal) in [
            (Builtin::AddInteger, "-18437736874454810623"),
            (Builtin::SubtractInteger, "18455751272964292609"),
            (
                Builtin::MultiplyInteger,
                "-166153499473114502559719956244594688",
            ),
        ] {
            assert_eq!(
                call(builtin, left.clone(), right.clone()).unwrap(),
                BuiltinResult::Constant(Constant::Integer(decimal.parse().unwrap()))
            );
        }
        for (left, right, equal, less, less_equal) in [
            (0, 0, true, false, true),
            (-1, 0, false, true, true),
            (1, -1, false, false, false),
            (-3, -4, false, false, false),
        ] {
            for (builtin, expected) in [
                (Builtin::EqualsInteger, equal),
                (Builtin::LessThanInteger, less),
                (Builtin::LessThanEqualsInteger, less_equal),
            ] {
                assert_eq!(
                    call(builtin, left.into(), right.into()).unwrap(),
                    BuiltinResult::Constant(Constant::Bool(expected))
                );
            }
        }
        assert_eq!(
            call(Builtin::MultiplyInteger, right, 0.into()).unwrap(),
            BuiltinResult::Constant(Constant::Integer(0.into()))
        );
    }

    #[test]
    fn signed_profile_bounds_are_semantic_arguments_not_result_limits() {
        let boundary = BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS;
        for value in [&boundary - 1, -&boundary, -&boundary + 1] {
            assert!(fits_cardano_integer_range(&value));
            assert!(call(Builtin::AddInteger, value, 0.into()).is_ok());
        }
        for value in [boundary.clone(), -&boundary - 1] {
            assert!(!fits_cardano_integer_range(&value));
            for builtin in INTEGER_BUILTINS {
                let result = call(builtin, value.clone(), 0.into());
                if builtin == Builtin::EqualsInteger {
                    assert!(result.is_ok());
                } else {
                    assert!(matches!(
                        result,
                        Err(RuntimeError::BuiltinIntegerOutOfBounds { argument: 0, .. })
                    ));
                }
            }
        }
        assert_eq!(
            call(Builtin::AddInteger, &boundary - 1, 1.into()).unwrap(),
            BuiltinResult::Constant(Constant::Integer(boundary.clone()))
        );
        assert_eq!(
            call(Builtin::SubtractInteger, -&boundary, 1.into()).unwrap(),
            BuiltinResult::Constant(Constant::Integer(-&boundary - 1))
        );
        assert_eq!(
            call(Builtin::MultiplyInteger, -&boundary, (-2).into()).unwrap(),
            BuiltinResult::Constant(Constant::Integer(&boundary * 2))
        );
    }

    #[test]
    fn type_validation_is_saturation_only_and_branches_are_opaque() {
        let boolean = Constant::Bool(true);
        let unit = Constant::Unit;
        for builtin in INTEGER_BUILTINS {
            assert!(matches!(
                builtin.validate_arguments(&[
                    BuiltinArgument::Constant(&boolean),
                    BuiltinArgument::Opaque
                ]),
                Err(RuntimeError::BuiltinTypeMismatch { argument: 0, .. })
            ));
            assert!(matches!(
                builtin.validate_arguments(&[BuiltinArgument::Constant(&boolean)]),
                Err(RuntimeError::BuiltinArity { .. })
            ));
        }
        for (condition, index) in [(true, 1), (false, 2)] {
            assert_eq!(
                Builtin::IfThenElse
                    .evaluate(&[
                        BuiltinArgument::Constant(&Constant::Bool(condition)),
                        BuiltinArgument::Opaque,
                        BuiltinArgument::Constant(&unit),
                    ])
                    .unwrap(),
                BuiltinResult::Argument(index)
            );
        }
        assert!(matches!(
            Builtin::IfThenElse.validate_arguments(&[
                BuiltinArgument::Constant(&unit),
                BuiltinArgument::Opaque,
                BuiltinArgument::Opaque
            ]),
            Err(RuntimeError::BuiltinTypeMismatch { argument: 0, .. })
        ));
    }

    #[test]
    fn independent_work_scales_with_portable_integer_words() {
        let left = Constant::Integer(BigInt::from(1) << 128);
        let right = Constant::Integer(BigInt::from(1) << 64);
        let args = [
            BuiltinArgument::Constant(&left),
            BuiltinArgument::Constant(&right),
        ];
        for builtin in INTEGER_BUILTINS {
            assert_eq!(
                builtin.work_units(&args).unwrap(),
                match builtin {
                    Builtin::MultiplyInteger => 6,
                    Builtin::DivideInteger
                    | Builtin::QuotientInteger
                    | Builtin::RemainderInteger
                    | Builtin::ModInteger => 9,
                    _ => 3,
                }
            );
        }
        assert_eq!(
            Builtin::IfThenElse
                .work_units(&[
                    BuiltinArgument::Constant(&Constant::Bool(true)),
                    BuiltinArgument::Opaque,
                    BuiltinArgument::Opaque
                ])
                .unwrap(),
            1
        );
        let large = Constant::Integer((BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS) - 1);
        assert_eq!(
            Builtin::MultiplyInteger
                .work_units(&[BuiltinArgument::Constant(&large); 2])
                .unwrap(),
            4096 * 4096
        );
    }

    #[test]
    fn implementation_magnitude_limit_is_separate_from_semantic_failures() {
        let outside = BigInt::from(1) << (MAX_INTEGER_BYTES * 8);
        for value in [outside.clone(), -outside.clone()] {
            assert!(matches!(
                checked_integer_result(value.clone()),
                Err(RuntimeError::Unsupported(_))
            ));
            assert!(matches!(
                call(Builtin::EqualsInteger, value.clone(), value),
                Err(RuntimeError::Unsupported(_))
            ));
        }
        assert!(checked_integer_result(&outside - 1).is_ok());
        assert!(checked_integer_result(-&outside + 1).is_ok());
    }

    const DIVISION: [Builtin; 4] = [
        Builtin::DivideInteger,
        Builtin::QuotientInteger,
        Builtin::RemainderInteger,
        Builtin::ModInteger,
    ];

    fn integer_result(builtin: Builtin, left: &BigInt, right: &BigInt) -> BigInt {
        let BuiltinResult::Constant(Constant::Integer(result)) =
            call(builtin, left.clone(), right.clone()).unwrap()
        else {
            panic!("division must return integer")
        };
        result
    }

    #[test]
    fn division_sign_table_covers_floor_truncation_exactness_and_zero() {
        for (left, right, expected) in [
            (7, 3, [2, 2, 1, 1]),
            (-7, 3, [-3, -2, -1, 2]),
            (7, -3, [-3, -2, 1, -2]),
            (-7, -3, [2, 2, -1, -1]),
            (6, 3, [2, 2, 0, 0]),
            (-6, 3, [-2, -2, 0, 0]),
            (6, -3, [-2, -2, 0, 0]),
            (-6, -3, [2, 2, 0, 0]),
            (1, 3, [0, 0, 1, 1]),
            (-1, 3, [-1, 0, -1, 2]),
            (1, -3, [-1, 0, 1, -2]),
            (-1, -3, [0, 0, -1, -1]),
            (3, 3, [1, 1, 0, 0]),
            (-3, 3, [-1, -1, 0, 0]),
            (0, 1, [0, 0, 0, 0]),
            (0, -1, [0, 0, 0, 0]),
            (-1, -1, [1, 1, 0, 0]),
        ] {
            for (builtin, expected) in DIVISION.into_iter().zip(expected) {
                assert_eq!(
                    integer_result(builtin, &left.into(), &right.into()),
                    BigInt::from(expected),
                    "{builtin} {left} {right}"
                );
            }
        }
    }

    #[test]
    fn division_results_satisfy_unique_integer_quotient_and_remainder_laws() {
        // The reconstruction, magnitude and sign laws uniquely identify both
        // pairs; expected values do not use the implementation's floor formula.
        let large: BigInt = (BigInt::from(1) << 257) + 9007199254740993_u64;
        let mut values = (-25..=25).map(BigInt::from).collect::<Vec<_>>();
        for exponent in [31, 32, 63, 64, 127, 128] {
            let power: BigInt = BigInt::from(1) << exponent;
            values.extend([&power - 1, power.clone(), &power + 1, -&power]);
        }
        values.extend([large.clone(), -large]);
        for left in &values {
            for right in values.iter().filter(|value| value.sign() != Sign::NoSign) {
                let [floor, trunc, rem, modulo] = DIVISION.map(|f| integer_result(f, left, right));
                assert_eq!(left, &(&floor * right + &modulo));
                assert_eq!(left, &(&trunc * right + &rem));
                assert!(modulo.magnitude() < right.magnitude());
                assert!(rem.magnitude() < right.magnitude());
                assert!(modulo.sign() == Sign::NoSign || modulo.sign() == right.sign());
                assert!(rem.sign() == Sign::NoSign || rem.sign() == left.sign());
            }
        }
    }

    #[test]
    fn each_division_signature_bounds_both_arguments_but_not_its_result() {
        let boundary = BigInt::from(1) << CARDANO_INTEGER_MAXIMUM_BITS;
        for builtin in DIVISION {
            for value in [&boundary - 1, -&boundary] {
                for args in [[value.clone(), 1.into()], [1.into(), value]] {
                    assert!(call(builtin, args[0].clone(), args[1].clone()).is_ok());
                }
            }
            for value in [boundary.clone(), -&boundary - 1] {
                for argument in [0, 1] {
                    let mut args = [BigInt::from(1), BigInt::from(1)];
                    args[argument] = value.clone();
                    assert_eq!(
                        call(builtin, args[0].clone(), args[1].clone()),
                        Err(RuntimeError::BuiltinIntegerOutOfBounds { builtin, argument })
                    );
                }
            }
            let expected = match builtin {
                Builtin::DivideInteger | Builtin::QuotientInteger => boundary.clone(),
                _ => 0.into(),
            };
            assert_eq!(integer_result(builtin, &-&boundary, &(-1).into()), expected);
        }
    }

    #[test]
    fn zero_divisor_is_denotation_failure_after_successful_unlifting_and_work() {
        for builtin in DIVISION {
            for value in [-1, 0, 1] {
                let args = [Constant::Integer(value.into()), Constant::Integer(0.into())];
                let args = args.each_ref().map(BuiltinArgument::Constant);
                assert_eq!(builtin.validate_arguments(&args), Ok(()));
                assert_eq!(builtin.work_units(&args), Ok(2));
                assert_eq!(
                    builtin.evaluate(&args),
                    Err(RuntimeError::BuiltinDivisionByZero { builtin })
                );
            }
        }
    }
}
