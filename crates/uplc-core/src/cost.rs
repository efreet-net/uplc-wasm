//! Explicit PlutusV3/protocol-11 machine costs and restricting budget accounting.
//!
//! There are no default coefficients. Every model is constructed from the supplied
//! 350-entry ledger parameter vector. This module does not implement builtin costs.
//! Charges are checked immediately, before the associated machine action; this
//! differs from the references' batching of partial failure costs. Successful costs
//! are unchanged. An explicit UPLC error has no machine charge of its own.
//!
//! A failed charge includes the entire attempted CPU and memory charge. Totals use
//! checked i128 arithmetic, independent of host pointer size. If either attempted
//! total exceeds i64::MAX, it necessarily exceeds the supplied nonnegative i64
//! limit: the wire outcome is budget_exhausted with a null consumed budget, since
//! the wire cannot represent that exact total. A meter is terminal after a failed
//! charge; later attempts return the original error without changing consumption.

use std::fmt;

pub const PARAMETER_COUNT: usize = 350;

/// CPU and memory units, with no floating-point conversions at any boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionBudget {
    pub cpu: i64,
    pub mem: i64,
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
}

impl MachineCosts {
    /// Read exact ledger-order coefficients; neither profile IDs nor defaults are used.
    /// Negative builtin polynomial coefficients are allowed. Every machine cost,
    /// including the reserved builtin/constr/case events, must be nonnegative.
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
        Ok(Self {
            startup: pair(29),
            var: pair(31),
            constant: pair(21),
            lambda: pair(27),
            apply: pair(17),
            delay: pair(23),
            force: pair(25),
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
        }
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

        // Before a successful charge both totals are <= i64::MAX, and a charge
        // is <= i64::MAX, so i128 is ample even for the first exhausted attempt.
        // Check nevertheless: neither debug nor release arithmetic may wrap.
        let Some(cpu) = self.cpu.checked_add(i128::from(cost.cpu)) else {
            return self.fail(BudgetError::Overflow);
        };
        let Some(mem) = self.mem.checked_add(i128::from(cost.mem)) else {
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
}
