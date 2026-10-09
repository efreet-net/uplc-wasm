//! Explicit PlutusV3/protocol-11 machine/builtin costs and restricting budgets.
//!
//! There are no default coefficients. Every model is constructed from the supplied
//! 350-entry ledger parameter vector. Builtin charges use the profile's semantics E.
//! Each submitted charge is checked immediately. The CEK machine submits startup
//! directly and otherwise batches up to 200 compute events in the official step
//! order; see [`crate::machine`]. An explicit error has no charge of its own and
//! does not flush an unfinished event batch.
//!
//! A failed charge includes the entire attempted CPU and memory charge. Totals use
//! checked i128 arithmetic, independent of host pointer size. If either attempted
//! total exceeds i64::MAX, it necessarily exceeds the supplied nonnegative i64
//! limit: the wire outcome is budget_exhausted with a null consumed budget, since
//! the wire cannot represent that exact total. A meter is terminal after a failed
//! charge; later attempts return the original error without changing consumption.

use std::fmt;

use crate::builtin::Builtin;

pub const PARAMETER_COUNT: usize = 350;

/// CPU and memory units, with no floating-point conversions at any boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionBudget {
    pub cpu: i64,
    pub mem: i64,
}

/// A builtin application's cost before checking the restricting and wire limits.
/// Keeping the wider value prevents a large valid coefficient from wrapping at
/// the i64 boundary before it reaches the meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinBudget {
    pub cpu: i128,
    pub mem: i128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCostError {
    /// Signed coefficients are valid; a negative resulting charge is unsupported.
    NegativeComputedCharge { dimension: Dimension },
    /// A positive charge exceeds the checked arithmetic range, and therefore any
    /// representable restricting limit. Metering this error gives a null budget.
    Overflow,
}

impl fmt::Display for BuiltinCostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeComputedCharge { dimension } => {
                write!(f, "computed builtin {dimension} charge is negative")
            }
            Self::Overflow => {
                f.write_str("computed builtin charge exceeds checked arithmetic range")
            }
        }
    }
}

impl std::error::Error for BuiltinCostError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinearCost {
    intercept: i64,
    slope: i64,
}

impl LinearCost {
    fn constant(value: i64) -> Self {
        Self {
            intercept: value,
            slope: 0,
        }
    }

    fn evaluate(self, size: u128, dimension: Dimension) -> Result<i128, BuiltinCostError> {
        // With two u64 input sizes, the size product is exact in u128. Avoid
        // converting that product when the slope is zero: a zero custom model
        // must remain zero even for artificial sizes above i128::MAX.
        let scaled = if self.slope == 0 {
            Some(0)
        } else {
            i128::try_from(size)
                .ok()
                .and_then(|size| i128::from(self.slope).checked_mul(size))
        };
        let charge = scaled.and_then(|scaled| scaled.checked_add(i128::from(self.intercept)));
        let Some(charge) = charge else {
            // An i64 intercept cannot cancel an overflowing magnitude enough
            // to make a positive charge fit the i64 wire range, or a negative
            // charge nonnegative. Classifying by slope preserves both policies.
            return Err(if self.slope < 0 {
                BuiltinCostError::NegativeComputedCharge { dimension }
            } else {
                BuiltinCostError::Overflow
            });
        };
        if charge < 0 {
            return Err(BuiltinCostError::NegativeComputedCharge { dimension });
        }
        Ok(charge)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BuiltinCoefficients {
    cpu: LinearCost,
    mem: LinearCost,
}

impl BuiltinCoefficients {
    fn evaluate(self, cpu_size: u128, mem_size: u128) -> Result<BuiltinBudget, BuiltinCostError> {
        let cpu = self.cpu.evaluate(cpu_size, Dimension::Cpu);
        let mem = self.mem.evaluate(mem_size, Dimension::Mem);
        match (cpu, mem) {
            // A negative dimension makes the computed model unsupported even
            // when the other dimension is too large to charge.
            (Err(error @ BuiltinCostError::NegativeComputedCharge { .. }), _)
            | (_, Err(error @ BuiltinCostError::NegativeComputedCharge { .. })) => Err(error),
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(cpu), Ok(mem)) => Ok(BuiltinBudget { cpu, mem }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Cpu,
    Mem,
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cpu => "CPU",
            Self::Mem => "memory",
        })
    }
}

/// Costed CEK events in this milestone. Error and return transitions are uncharged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Startup,
    Var,
    Constant,
    Lambda,
    Apply,
    Delay,
    Force,
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    WrongParameterCount { actual: usize },
    NegativeMachineCost { index: usize, value: i64 },
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongParameterCount { actual } => write!(
                f,
                "expected {PARAMETER_COUNT} PlutusV3/protocol-11 cost parameters, got {actual}"
            ),
            Self::NegativeMachineCost { index, value } => {
                write!(f, "negative machine cost at parameter {index}: {value}")
            }
        }
    }
}

impl std::error::Error for ModelError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineCosts {
    startup: ExecutionBudget,
    var: ExecutionBudget,
    constant: ExecutionBudget,
    lambda: ExecutionBudget,
    apply: ExecutionBudget,
    delay: ExecutionBudget,
    force: ExecutionBudget,
    builtin: ExecutionBudget,
    add_integer: BuiltinCoefficients,
    subtract_integer: BuiltinCoefficients,
    multiply_integer: BuiltinCoefficients,
    equals_integer: BuiltinCoefficients,
    less_than_integer: BuiltinCoefficients,
    less_than_equals_integer: BuiltinCoefficients,
    if_then_else: BuiltinCoefficients,
}

impl MachineCosts {
    /// Read exact ledger-order coefficients; neither profile IDs nor defaults are used.
    /// Negative builtin polynomial coefficients are allowed. Every machine cost,
    /// including the reserved constr/case events, must be nonnegative.
    pub fn from_parameters(parameters: &[i64]) -> Result<Self, ModelError> {
        if parameters.len() != PARAMETER_COUNT {
            return Err(ModelError::WrongParameterCount {
                actual: parameters.len(),
            });
        }

        // The zero-based ledger order is authoritative in the pinned official source:
        // https://github.com/IntersectMBO/plutus/blob/5f785edeac0d1d89622d44344fdda07ef48e8c73/plutus-ledger-api/src/PlutusLedgerApi/V3/ParamName.hs
        // Cross-checked against the independently pinned mappings (no dependency):
        // https://github.com/aiken-lang/aiken/blob/b5c34839ee33a2d608a071f3679410f63ad68bae/crates/uplc/src/machine/cost_model.rs
        // https://github.com/pragma-org/amaru/blob/34a453005bcaaf837ee73bd996b99eab8ef92961/crates/amaru-uplc/src/machine/cost_model/cost_map.rs
        // 17..=32: apply, builtin, const, delay, force, lam, startup, var (CPU/memory).
        // 193..=196: constr, case (CPU/memory). Builtin *coefficients* elsewhere
        // are not all costs and can legitimately be negative in the supplied model.
        for index in (17..=32).chain(193..=196) {
            if parameters[index] < 0 {
                return Err(ModelError::NegativeMachineCost {
                    index,
                    value: parameters[index],
                });
            }
        }
        let pair = |cpu_index: usize| ExecutionBudget {
            cpu: parameters[cpu_index],
            mem: parameters[cpu_index + 1],
        };
        let linear = |index: usize| LinearCost {
            intercept: parameters[index],
            slope: parameters[index + 1],
        };
        let linear_pair = |index: usize| BuiltinCoefficients {
            cpu: linear(index),
            mem: linear(index + 2),
        };
        let comparison = |index: usize| BuiltinCoefficients {
            cpu: linear(index),
            mem: LinearCost::constant(parameters[index + 2]),
        };
        Ok(Self {
            startup: pair(29),
            var: pair(31),
            constant: pair(21),
            lambda: pair(27),
            apply: pair(17),
            delay: pair(23),
            force: pair(25),
            builtin: pair(19),
            add_integer: linear_pair(0),
            subtract_integer: linear_pair(167),
            multiply_integer: linear_pair(124),
            equals_integer: comparison(71),
            less_than_integer: comparison(99),
            less_than_equals_integer: comparison(96),
            if_then_else: BuiltinCoefficients {
                cpu: LinearCost::constant(parameters[84]),
                mem: LinearCost::constant(parameters[85]),
            },
        })
    }

    pub fn cost(&self, step: Step) -> ExecutionBudget {
        match step {
            Step::Startup => self.startup,
            Step::Var => self.var,
            Step::Constant => self.constant,
            Step::Lambda => self.lambda,
            Step::Apply => self.apply,
            Step::Delay => self.delay,
            Step::Force => self.force,
            Step::Builtin => self.builtin,
        }
    }

    /// Compute one fully saturated builtin application from argument memory
    /// units. The caller validates types and profile input bounds first; partial
    /// applications, bad forces, and unlifting failures have no builtin charge.
    /// `ifThenElse` ignores both sizes because both of its costs are constant.
    ///
    /// Formula shapes are from pinned Plutus `builtinCostModelE.json`, with
    /// `CostingFun/Core.hs:627-659` defining added/multiplied/min/max sizes.
    /// Integer arguments have singleton memory streams, so every supported
    /// formula produces one atomic CPU/memory charge. Shape E applies to V3/PV11
    /// per `PlutusLedgerApi/Common/ProtocolVersions.hs:136-138`; coefficients
    /// always come from the request, not from the JSON model's example values.
    pub fn builtin_cost(
        &self,
        builtin: Builtin,
        x: u64,
        y: u64,
    ) -> Result<BuiltinBudget, BuiltinCostError> {
        let maximum = u128::from(x.max(y));
        let minimum = u128::from(x.min(y));
        let (coefficients, cpu_size, mem_size) = match builtin {
            Builtin::AddInteger => (self.add_integer, maximum, maximum),
            Builtin::SubtractInteger => (self.subtract_integer, maximum, maximum),
            Builtin::MultiplyInteger => {
                // A sum or product of two u64 values is exact in u128, even on
                // wasm32. Coefficient arithmetic is checked separately above.
                let product = u128::from(x) * u128::from(y);
                let sum = u128::from(x) + u128::from(y);
                (self.multiply_integer, product, sum)
            }
            Builtin::EqualsInteger => (self.equals_integer, minimum, 0),
            Builtin::LessThanInteger => (self.less_than_integer, minimum, 0),
            Builtin::LessThanEqualsInteger => (self.less_than_equals_integer, minimum, 0),
            Builtin::IfThenElse => (self.if_then_else, 0, 0),
        };
        coefficients.evaluate(cpu_size, mem_size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetError {
    NegativeLimit {
        dimension: Dimension,
    },
    NegativeCharge {
        dimension: Dimension,
    },
    Exhausted,
    /// The full attempted consumption cannot be represented by the wire's i64s.
    /// This necessarily exhausts a valid restricting limit; serialize no budget.
    Overflow,
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeLimit { dimension } => write!(f, "negative {dimension} budget limit"),
            Self::NegativeCharge { dimension } => write!(f, "negative {dimension} budget charge"),
            Self::Exhausted => f.write_str("restricting execution budget exhausted"),
            Self::Overflow => f.write_str(
                "restricting budget exhausted; consumed total exceeds the i64 wire range",
            ),
        }
    }
}

impl std::error::Error for BudgetError {}

#[derive(Debug, Clone)]
pub struct BudgetMeter {
    limits: ExecutionBudget,
    cpu: i128,
    mem: i128,
    terminal: Option<BudgetError>,
}

impl BudgetMeter {
    pub fn new(limits: ExecutionBudget) -> Result<Self, BudgetError> {
        if limits.cpu < 0 {
            return Err(BudgetError::NegativeLimit {
                dimension: Dimension::Cpu,
            });
        }
        if limits.mem < 0 {
            return Err(BudgetError::NegativeLimit {
                dimension: Dimension::Mem,
            });
        }
        Ok(Self {
            limits,
            cpu: 0,
            mem: 0,
            terminal: None,
        })
    }

    /// Charge both dimensions atomically, retaining the full attempted charge on
    /// exhaustion. Equality with either limit is allowed. Negative costs are an
    /// API error, do not change consumption, and are never a semantic UPLC error.
    pub fn charge(&mut self, cost: ExecutionBudget) -> Result<(), BudgetError> {
        self.charge_repeated(cost, 1)
    }

    /// Charge a saturated, correctly typed builtin immediately, without flushing
    /// pending CEK events. Arithmetic overflow is terminal budget exhaustion;
    /// negative computed charges are terminal API errors and must be exposed as
    /// unsupported custom models, never as semantic UPLC failures.
    pub fn charge_builtin(
        &mut self,
        cost: Result<BuiltinBudget, BuiltinCostError>,
    ) -> Result<(), BudgetError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        match cost {
            Ok(cost) => self.charge_wide(cost),
            Err(BuiltinCostError::NegativeComputedCharge { dimension }) => {
                self.fail(BudgetError::NegativeCharge { dimension })
            }
            Err(BuiltinCostError::Overflow) => self.fail(BudgetError::Overflow),
        }
    }

    /// Submit a batch of identical events. Multiplication happens in checked
    /// i128 arithmetic before addition or comparison, so an overflowing i64 batch
    /// is budget exhaustion with no representable consumed budget, never wrapping.
    pub fn charge_repeated(
        &mut self,
        cost: ExecutionBudget,
        count: u32,
    ) -> Result<(), BudgetError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if cost.cpu < 0 {
            return self.fail(BudgetError::NegativeCharge {
                dimension: Dimension::Cpu,
            });
        }
        if cost.mem < 0 {
            return self.fail(BudgetError::NegativeCharge {
                dimension: Dimension::Mem,
            });
        }

        // Each coefficient is at most i64::MAX and count is u32. Check both
        // multiplications regardless, then use the same atomic meter as builtins.
        let Some(cpu) = i128::from(cost.cpu).checked_mul(i128::from(count)) else {
            return self.fail(BudgetError::Overflow);
        };
        let Some(mem) = i128::from(cost.mem).checked_mul(i128::from(count)) else {
            return self.fail(BudgetError::Overflow);
        };
        self.charge_wide(BuiltinBudget { cpu, mem })
    }

    fn charge_wide(&mut self, cost: BuiltinBudget) -> Result<(), BudgetError> {
        if cost.cpu < 0 {
            return self.fail(BudgetError::NegativeCharge {
                dimension: Dimension::Cpu,
            });
        }
        if cost.mem < 0 {
            return self.fail(BudgetError::NegativeCharge {
                dimension: Dimension::Mem,
            });
        }
        let Some(cpu) = self.cpu.checked_add(cost.cpu) else {
            return self.fail(BudgetError::Overflow);
        };
        let Some(mem) = self.mem.checked_add(cost.mem) else {
            return self.fail(BudgetError::Overflow);
        };
        self.cpu = cpu;
        self.mem = mem;
        if self.consumed().is_none() {
            return self.fail(BudgetError::Overflow);
        }
        if self.cpu > i128::from(self.limits.cpu) || self.mem > i128::from(self.limits.mem) {
            return self.fail(BudgetError::Exhausted);
        }
        Ok(())
    }

    /// Full attempted consumption, including the exhausting charge. None means
    /// the total is unrepresentable on the wire; it must never be clamped or wrapped.
    pub fn consumed(&self) -> Option<ExecutionBudget> {
        if self.terminal == Some(BudgetError::Overflow) {
            return None;
        }
        Some(ExecutionBudget {
            cpu: self.cpu.try_into().ok()?,
            mem: self.mem.try_into().ok()?,
        })
    }

    fn fail(&mut self, error: BudgetError) -> Result<(), BudgetError> {
        self.terminal = Some(error);
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_parameters() -> Vec<i64> {
        // Read the actual checked-in model without adding a JSON dependency to
        // the costing module. The profile's array contains decimal JSON strings.
        include_str!("../../../profiles/plutus-v3-pv11.json")
            .split_once("\"parameters\": [")
            .unwrap()
            .1
            .split_once(']')
            .unwrap()
            .0
            .split(',')
            .map(|value| value.trim().trim_matches('"').parse().unwrap())
            .collect()
    }

    #[test]
    fn ledger_indices_are_distinct_for_every_supported_event_and_dimension() {
        let parameters: Vec<i64> = (0..PARAMETER_COUNT).map(|index| index as i64).collect();
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        for (step, cpu, mem) in [
            (Step::Startup, 29, 30),
            (Step::Var, 31, 32),
            (Step::Constant, 21, 22),
            (Step::Lambda, 27, 28),
            (Step::Apply, 17, 18),
            (Step::Delay, 23, 24),
            (Step::Force, 25, 26),
            (Step::Builtin, 19, 20),
        ] {
            assert_eq!(costs.cost(step), ExecutionBudget { cpu, mem });
        }
    }

    #[test]
    fn supplied_development_model_and_custom_coefficients_are_used_exactly() {
        let mut parameters = fixture_parameters();
        assert_eq!(parameters.len(), PARAMETER_COUNT);
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        assert_eq!(
            costs.cost(Step::Startup),
            ExecutionBudget { cpu: 100, mem: 100 }
        );
        for step in [
            Step::Var,
            Step::Constant,
            Step::Lambda,
            Step::Apply,
            Step::Delay,
            Step::Force,
            Step::Builtin,
        ] {
            assert_eq!(
                costs.cost(step),
                ExecutionBudget {
                    cpu: 16_000,
                    mem: 100
                }
            );
        }
        parameters[21] = 9_007_199_254_740_993;
        parameters[22] = 0;
        let custom = MachineCosts::from_parameters(&parameters).unwrap();
        assert_eq!(
            custom.cost(Step::Constant),
            ExecutionBudget {
                cpu: 9_007_199_254_740_993,
                mem: 0
            }
        );
        assert_eq!(custom.cost(Step::Var), costs.cost(Step::Var));
    }

    #[test]
    fn builtin_parameter_positions_and_formula_shapes_are_distinct() {
        let parameters: Vec<i64> = (0..PARAMETER_COUNT).map(|index| index as i64).collect();
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        // Asymmetric sizes distinguish max/min, product/sum, and every offset.
        // Expected arithmetic follows official V3 ParamName order and model E.
        for (builtin, cpu, mem) in [
            (Builtin::AddInteger, 5, 17),
            (Builtin::SubtractInteger, 1_007, 1_019),
            (Builtin::MultiplyInteger, 1_374, 1_015),
            (Builtin::EqualsInteger, 215, 73),
            (Builtin::LessThanInteger, 299, 101),
            (Builtin::LessThanEqualsInteger, 290, 98),
            (Builtin::IfThenElse, 84, 85),
        ] {
            let expected = Ok(BuiltinBudget { cpu, mem });
            assert_eq!(costs.builtin_cost(builtin, 2, 5), expected);
            assert_eq!(costs.builtin_cost(builtin, 5, 2), expected);
        }
    }

    #[test]
    fn supplied_builtin_vector_matches_independent_single_word_and_large_costs() {
        let costs = MachineCosts::from_parameters(&fixture_parameters()).unwrap();
        // Single-word costs also reproduce the unchanged official *-01 budget
        // goldens after 100/100 startup and five 16000/100 CEK events. The
        // ifThenElse-01 program has eight CEK events (including its force).
        for (builtin, one_cpu, one_mem, larger_cpu, larger_mem) in [
            (Builtin::AddInteger, 101_208, 2, 102_888, 6),
            (Builtin::SubtractInteger, 101_208, 2, 102_888, 6),
            (Builtin::MultiplyInteger, 90_953, 2, 95_624, 7),
            (Builtin::EqualsInteger, 52_333, 1, 52_891, 1),
            (Builtin::LessThanInteger, 45_290, 1, 45_831, 1),
            (Builtin::LessThanEqualsInteger, 43_837, 1, 44_389, 1),
            (Builtin::IfThenElse, 76_049, 1, 76_049, 1),
        ] {
            assert_eq!(
                costs.builtin_cost(builtin, 1, 1),
                Ok(BuiltinBudget {
                    cpu: one_cpu,
                    mem: one_mem,
                })
            );
            assert_eq!(
                costs.builtin_cost(builtin, 2, 5),
                Ok(BuiltinBudget {
                    cpu: larger_cpu,
                    mem: larger_mem,
                })
            );
        }
    }

    #[test]
    fn signed_builtin_coefficients_are_preserved_until_evaluation() {
        let mut parameters = vec![0; PARAMETER_COUNT];
        parameters[0] = -3;
        parameters[1] = 2;
        parameters[2] = 8;
        parameters[3] = -2;
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        assert_eq!(
            costs.builtin_cost(Builtin::AddInteger, 2, 3),
            Ok(BuiltinBudget { cpu: 3, mem: 2 })
        );
        assert_eq!(
            costs.builtin_cost(Builtin::AddInteger, 1, 1),
            Err(BuiltinCostError::NegativeComputedCharge {
                dimension: Dimension::Cpu,
            })
        );
        assert_eq!(
            costs.builtin_cost(Builtin::AddInteger, 1, 5),
            Err(BuiltinCostError::NegativeComputedCharge {
                dimension: Dimension::Mem,
            })
        );
        parameters[0] = i64::MIN;
        parameters[1] = i64::MAX;
        parameters[2] = 4;
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        assert_eq!(
            costs.builtin_cost(Builtin::AddInteger, 1, 2),
            Ok(BuiltinBudget {
                cpu: i128::from(i64::MAX) - 1,
                mem: 0,
            })
        );
        // Unused negative builtin coefficients do not reject the entire model.
        parameters[84] = -1;
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        assert_eq!(
            costs.builtin_cost(Builtin::IfThenElse, 0, 0),
            Err(BuiltinCostError::NegativeComputedCharge {
                dimension: Dimension::Cpu,
            })
        );
        assert!(costs.builtin_cost(Builtin::AddInteger, 1, 2).is_ok());
    }

    #[test]
    fn zero_builtin_coefficients_remain_zero_for_every_size() {
        let costs = MachineCosts::from_parameters(&vec![0; PARAMETER_COUNT]).unwrap();
        for builtin in [
            Builtin::AddInteger,
            Builtin::SubtractInteger,
            Builtin::MultiplyInteger,
            Builtin::EqualsInteger,
            Builtin::LessThanInteger,
            Builtin::LessThanEqualsInteger,
            Builtin::IfThenElse,
        ] {
            for (x, y) in [(0, 0), (1, 1), (2, 5), (u64::MAX, u64::MAX)] {
                let charge = costs.builtin_cost(builtin, x, y);
                assert_eq!(charge, Ok(BuiltinBudget { cpu: 0, mem: 0 }));
                let mut meter = BudgetMeter::new(ExecutionBudget { cpu: 0, mem: 0 }).unwrap();
                assert_eq!(meter.charge_builtin(charge), Ok(()));
                assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 0, mem: 0 }));
            }
        }
    }

    #[test]
    fn builtin_expression_overflow_never_wraps_or_masks_negative_charges() {
        for index in [125, 127] {
            let mut parameters = vec![0; PARAMETER_COUNT];
            parameters[index] = i64::MAX;
            let costs = MachineCosts::from_parameters(&parameters).unwrap();
            assert_eq!(
                costs.builtin_cost(Builtin::MultiplyInteger, u64::MAX, u64::MAX),
                Err(BuiltinCostError::Overflow)
            );
            parameters[index] = i64::MIN;
            let costs = MachineCosts::from_parameters(&parameters).unwrap();
            assert_eq!(
                costs.builtin_cost(Builtin::MultiplyInteger, u64::MAX, u64::MAX),
                Err(BuiltinCostError::NegativeComputedCharge {
                    dimension: if index == 125 {
                        Dimension::Cpu
                    } else {
                        Dimension::Mem
                    },
                })
            );
        }
        let mut parameters = vec![0; PARAMETER_COUNT];
        parameters[125] = i64::MAX;
        parameters[126] = -1;
        let costs = MachineCosts::from_parameters(&parameters).unwrap();
        assert_eq!(
            costs.builtin_cost(Builtin::MultiplyInteger, u64::MAX, u64::MAX),
            Err(BuiltinCostError::NegativeComputedCharge {
                dimension: Dimension::Mem,
            })
        );
    }

    #[test]
    fn parameter_shape_and_every_machine_cost_are_validated() {
        for count in [0, 1, 349, 351, 4096] {
            assert_eq!(
                MachineCosts::from_parameters(&vec![0; count]),
                Err(ModelError::WrongParameterCount { actual: count })
            );
        }
        for index in (17..=32).chain(193..=196) {
            let mut parameters = fixture_parameters();
            parameters[index] = -1;
            assert_eq!(
                MachineCosts::from_parameters(&parameters),
                Err(ModelError::NegativeMachineCost { index, value: -1 })
            );
        }
        let mut parameters = fixture_parameters();
        assert!(parameters.iter().any(|cost| *cost < 0));
        parameters[52] = i64::MIN;
        assert!(MachineCosts::from_parameters(&parameters).is_ok());
    }

    #[test]
    fn exact_budget_and_one_unit_short_in_either_dimension() {
        let costs = MachineCosts::from_parameters(&fixture_parameters()).unwrap();
        // Constant program: startup plus one constant event, from the official
        // machine-cost model rather than an evaluator-produced expected result.
        let required = ExecutionBudget {
            cpu: 16_100,
            mem: 200,
        };
        for (limits, expected) in [
            (required, Ok(())),
            (
                ExecutionBudget {
                    cpu: required.cpu - 1,
                    ..required
                },
                Err(BudgetError::Exhausted),
            ),
            (
                ExecutionBudget {
                    mem: required.mem - 1,
                    ..required
                },
                Err(BudgetError::Exhausted),
            ),
        ] {
            let mut meter = BudgetMeter::new(limits).unwrap();
            assert_eq!(meter.charge(costs.cost(Step::Startup)), Ok(()));
            assert_eq!(meter.charge(costs.cost(Step::Constant)), expected);
            assert_eq!(meter.consumed(), Some(required));
        }
    }

    #[test]
    fn zero_limits_zero_costs_and_startup_exhaustion() {
        let zero = ExecutionBudget { cpu: 0, mem: 0 };
        let mut meter = BudgetMeter::new(zero).unwrap();
        assert_eq!(meter.charge(zero), Ok(()));
        assert_eq!(meter.consumed(), Some(zero));
        let startup = MachineCosts::from_parameters(&fixture_parameters())
            .unwrap()
            .cost(Step::Startup);
        assert_eq!(meter.charge(startup), Err(BudgetError::Exhausted));
        assert_eq!(meter.consumed(), Some(startup));
        assert_eq!(meter.charge(zero), Err(BudgetError::Exhausted));
        assert_eq!(meter.consumed(), Some(startup));
    }

    #[test]
    fn exhausting_charge_records_both_dimensions_and_is_terminal() {
        let mut meter = BudgetMeter::new(ExecutionBudget { cpu: 3, mem: 100 }).unwrap();
        assert_eq!(
            meter.charge(ExecutionBudget { cpu: 4, mem: 7 }),
            Err(BudgetError::Exhausted)
        );
        assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 4, mem: 7 }));
        assert_eq!(
            meter.charge(ExecutionBudget {
                cpu: i64::MAX,
                mem: i64::MAX,
            }),
            Err(BudgetError::Exhausted)
        );
        assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 4, mem: 7 }));
    }

    #[test]
    fn maximum_limits_and_unrepresentable_attempts_never_wrap() {
        let maximum = ExecutionBudget {
            cpu: i64::MAX,
            mem: i64::MAX,
        };
        for extra in [
            ExecutionBudget { cpu: 1, mem: 0 },
            ExecutionBudget { cpu: 0, mem: 1 },
            maximum,
        ] {
            let mut meter = BudgetMeter::new(maximum).unwrap();
            assert_eq!(meter.charge(maximum), Ok(()));
            assert_eq!(meter.consumed(), Some(maximum));
            assert_eq!(meter.charge(extra), Err(BudgetError::Overflow));
            assert_eq!(meter.cpu, i128::from(i64::MAX) + i128::from(extra.cpu));
            assert_eq!(meter.mem, i128::from(i64::MAX) + i128::from(extra.mem));
            assert_eq!(meter.consumed(), None);
            let (cpu, mem) = (meter.cpu, meter.mem);
            assert_eq!(meter.charge(maximum), Err(BudgetError::Overflow));
            assert_eq!((meter.cpu, meter.mem), (cpu, mem));
        }
    }

    #[test]
    fn negative_limits_and_charges_are_explicit_api_errors() {
        for (negative, dimension) in [
            (ExecutionBudget { cpu: -1, mem: 0 }, Dimension::Cpu),
            (ExecutionBudget { cpu: 0, mem: -1 }, Dimension::Mem),
        ] {
            assert!(matches!(
                BudgetMeter::new(negative),
                Err(BudgetError::NegativeLimit { dimension: actual }) if actual == dimension
            ));
            let mut meter = BudgetMeter::new(ExecutionBudget { cpu: 10, mem: 10 }).unwrap();
            assert_eq!(
                meter.charge(negative),
                Err(BudgetError::NegativeCharge { dimension })
            );
            assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 0, mem: 0 }));
            assert_eq!(
                meter.charge(ExecutionBudget { cpu: 1, mem: 1 }),
                Err(BudgetError::NegativeCharge { dimension })
            );
            assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 0, mem: 0 }));
        }
    }

    #[test]
    fn repeated_charges_use_exact_widened_multiplication() {
        let mut meter = BudgetMeter::new(ExecutionBudget {
            cpu: 600,
            mem: 1_000,
        })
        .unwrap();
        assert_eq!(
            meter.charge_repeated(ExecutionBudget { cpu: 3, mem: 5 }, 200),
            Ok(())
        );
        assert_eq!(
            meter.consumed(),
            Some(ExecutionBudget {
                cpu: 600,
                mem: 1_000,
            })
        );
        assert_eq!(
            meter.charge_repeated(ExecutionBudget { cpu: 3, mem: 5 }, 0),
            Ok(())
        );
        assert_eq!(
            meter.charge_repeated(ExecutionBudget { cpu: 3, mem: 5 }, 2),
            Err(BudgetError::Exhausted)
        );
        assert_eq!(
            meter.consumed(),
            Some(ExecutionBudget {
                cpu: 606,
                mem: 1_010,
            })
        );

        for cost in [
            ExecutionBudget {
                cpu: i64::MAX,
                mem: 1,
            },
            ExecutionBudget {
                cpu: 1,
                mem: i64::MAX,
            },
        ] {
            let mut meter = BudgetMeter::new(ExecutionBudget {
                cpu: i64::MAX,
                mem: i64::MAX,
            })
            .unwrap();
            assert_eq!(
                meter.charge_repeated(cost, u32::MAX),
                Err(BudgetError::Overflow)
            );
            assert_eq!(meter.cpu, i128::from(cost.cpu) * i128::from(u32::MAX));
            assert_eq!(meter.mem, i128::from(cost.mem) * i128::from(u32::MAX));
            assert_eq!(meter.consumed(), None);
        }
    }

    #[test]
    fn builtin_charges_are_atomic_exact_and_terminal_on_exhaustion() {
        let cost = BuiltinBudget { cpu: 23, mem: 7 };
        for (limits, expected) in [
            (ExecutionBudget { cpu: 23, mem: 7 }, Ok(())),
            (
                ExecutionBudget { cpu: 22, mem: 7 },
                Err(BudgetError::Exhausted),
            ),
            (
                ExecutionBudget { cpu: 23, mem: 6 },
                Err(BudgetError::Exhausted),
            ),
            (
                ExecutionBudget { cpu: 0, mem: 0 },
                Err(BudgetError::Exhausted),
            ),
        ] {
            let mut meter = BudgetMeter::new(limits).unwrap();
            assert_eq!(meter.charge_builtin(Ok(cost)), expected);
            assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 23, mem: 7 }));
            if let Err(error) = expected {
                assert_eq!(
                    meter.charge_builtin(Err(BuiltinCostError::Overflow)),
                    Err(error)
                );
                assert_eq!(meter.consumed(), Some(ExecutionBudget { cpu: 23, mem: 7 }));
            }
        }
    }

    #[test]
    fn builtin_wide_charges_and_unrepresentable_totals_have_null_budgets() {
        let maximum = ExecutionBudget {
            cpu: i64::MAX,
            mem: i64::MAX,
        };
        let beyond_wire = i128::from(i64::MAX) + 1;
        for cost in [
            BuiltinBudget {
                cpu: beyond_wire,
                mem: 7,
            },
            BuiltinBudget {
                cpu: 23,
                mem: beyond_wire,
            },
        ] {
            let mut meter = BudgetMeter::new(maximum).unwrap();
            assert_eq!(meter.charge_builtin(Ok(cost)), Err(BudgetError::Overflow));
            assert_eq!((meter.cpu, meter.mem), (cost.cpu, cost.mem));
            assert_eq!(meter.consumed(), None);
            assert_eq!(
                meter.charge_builtin(Ok(BuiltinBudget { cpu: 0, mem: 0 })),
                Err(BudgetError::Overflow)
            );
            assert_eq!((meter.cpu, meter.mem), (cost.cpu, cost.mem));
        }
        for cost in [
            BuiltinBudget { cpu: 1, mem: 0 },
            BuiltinBudget { cpu: 0, mem: 1 },
            BuiltinBudget {
                cpu: i128::MAX,
                mem: i128::MAX,
            },
        ] {
            let mut meter = BudgetMeter::new(maximum).unwrap();
            meter.charge(maximum).unwrap();
            assert_eq!(meter.charge_builtin(Ok(cost)), Err(BudgetError::Overflow));
            assert_eq!(meter.consumed(), None);
        }
        let mut meter = BudgetMeter::new(maximum).unwrap();
        meter.charge(ExecutionBudget { cpu: 3, mem: 5 }).unwrap();
        assert_eq!(
            meter.charge_builtin(Err(BuiltinCostError::Overflow)),
            Err(BudgetError::Overflow)
        );
        assert_eq!(meter.consumed(), None);
        assert_eq!(
            meter.charge_builtin(Ok(BuiltinBudget { cpu: 0, mem: 0 })),
            Err(BudgetError::Overflow)
        );
    }

    #[test]
    fn negative_builtin_computations_never_credit_the_meter() {
        for dimension in [Dimension::Cpu, Dimension::Mem] {
            let mut meter = BudgetMeter::new(ExecutionBudget { cpu: 100, mem: 100 }).unwrap();
            let startup = ExecutionBudget { cpu: 3, mem: 5 };
            meter.charge(startup).unwrap();
            assert_eq!(
                meter.charge_builtin(Err(BuiltinCostError::NegativeComputedCharge { dimension })),
                Err(BudgetError::NegativeCharge { dimension })
            );
            assert_eq!(meter.consumed(), Some(startup));
            assert_eq!(
                meter.charge_builtin(Ok(BuiltinBudget { cpu: 1, mem: 1 })),
                Err(BudgetError::NegativeCharge { dimension })
            );
            assert_eq!(meter.consumed(), Some(startup));
        }
    }
}
