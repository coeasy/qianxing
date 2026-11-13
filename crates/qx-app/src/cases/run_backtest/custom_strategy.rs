//! Strategy artifact identity and caller-supplied strategy entrypoint.

use super::{
    run_backtest_inner_for_experiment, run_backtest_inner_with_strategies, spec_config_hash,
    BacktestSpec,
};
use crate::cases::attach;
use crate::cases::experiment_search::{DrawdownPruningWindow, SearchBacktestAttempt};
use crate::cases::guard::guard_panics;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::run_handle::CancellationToken;
use crate::spec::BacktestOutcome;
use qx_core::Fnv1a;
use qx_strategy::Strategy;
use serde::{Deserialize, Serialize};

/// Immutable identity for a caller-supplied strategy implementation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyArtifactIdentity {
    pub strategy_id: String,
    pub strategy_version: String,
    pub source_sha256: String,
}

impl StrategyArtifactIdentity {
    pub(crate) fn validate_for_experiment(&self, spec: &BacktestSpec) -> Result<(), AppError> {
        let valid_token = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        if self.strategy_id != spec.strategy.strategy_id
            || !valid_token(&self.strategy_id)
            || !valid_token(&self.strategy_version)
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "experiment strategy identity must match the primary strategy slot and carry a valid version and lowercase SHA-256 digest",
            ));
        }
        if spec.strategy.kind.needs_reference_leg() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "single-frame strategy experiments cannot use a two-leg strategy kind",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_for_slot(
        &self,
        expected_id: &str,
        slot: &crate::spec::BuiltinStrategySpec,
    ) -> Result<(), AppError> {
        let valid_token = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        if !valid_token(&self.strategy_id)
            || self.strategy_id != expected_id
            || !valid_token(&self.strategy_version)
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "StrategyArtifactIdentity requires a matching strategy_id, a valid strategy_version, and a lowercase SHA-256 digest",
            ));
        }
        let default_strategy =
            crate::spec::BuiltinStrategySpec::new(slot.kind, slot.strategy_id.clone());
        if *slot != default_strategy {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied strategies must use default BuiltinStrategySpec parameters; built-in signal and sizing knobs do not apply to external callbacks",
            ));
        }
        Ok(())
    }

    fn validate_for_portfolio_slot(&self, spec: &BacktestSpec) -> Result<(), AppError> {
        if self.strategy_id == spec.strategy.strategy_id {
            return self.validate_for_slot(&spec.strategy.strategy_id, &spec.strategy);
        }
        let member = spec
            .portfolio
            .as_ref()
            .and_then(|portfolio| {
                portfolio
                    .additional_strategies
                    .iter()
                    .find(|member| member.strategy.strategy_id == self.strategy_id)
            })
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("外部策略身份不属于 BacktestSpec 组合: {}", self.strategy_id),
                )
            })?;
        self.validate_for_slot(&member.strategy.strategy_id, &member.strategy)
    }

    pub(crate) fn validate_for_depth(
        &self,
        spec: &crate::spec::DepthBacktestSpec,
    ) -> Result<(), AppError> {
        self.validate_depth_identity(spec)?;
        let slot = if self.strategy_id == spec.strategy.strategy_id {
            &spec.strategy
        } else {
            spec.portfolio
                .as_ref()
                .and_then(|portfolio| {
                    portfolio
                        .additional_strategies
                        .iter()
                        .find(|member| member.strategy.strategy_id == self.strategy_id)
                })
                .map(|member| &member.strategy)
                .ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!(
                            "外部策略身份不属于 Tick/OrderBook 组合: {}",
                            self.strategy_id
                        ),
                    )
                })?
        };
        let default_strategy =
            crate::spec::BuiltinStrategySpec::new(slot.kind, slot.strategy_id.clone());
        if *slot != default_strategy {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied strategies must use default BuiltinStrategySpec parameters; built-in signal and sizing knobs do not apply to external callbacks",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_for_depth_experiment(
        &self,
        spec: &crate::spec::DepthBacktestSpec,
    ) -> Result<(), AppError> {
        if self.strategy_id != spec.strategy.strategy_id {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "depth experiment strategy identity must match the primary strategy slot",
            ));
        }
        // The numeric strategy fields are parameter carriers for the caller factory in this
        // experiment-only path; unlike a normal custom callback run, the factory consumes them.
        self.validate_depth_identity(spec)
    }

    fn validate_depth_identity(
        &self,
        spec: &crate::spec::DepthBacktestSpec,
    ) -> Result<(), AppError> {
        let valid_token = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        if !valid_token(&self.strategy_id)
            || !valid_token(&self.strategy_version)
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "StrategyArtifactIdentity requires a matching strategy_id, a valid strategy_version, and a lowercase SHA-256 digest",
            ));
        }
        if spec.market_making.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied depth strategies cannot be combined with market-making strategies",
            ));
        }
        Ok(())
    }
}

pub(in crate::cases) fn run_backtest_inner_with_experiment_strategy(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
    pruning: Option<DrawdownPruningWindow>,
) -> Result<SearchBacktestAttempt<BacktestOutcome>, AppError> {
    identity.validate_for_experiment(spec)?;
    run_backtest_inner_for_experiment(
        spec,
        context,
        cancellation,
        vec![InjectedStrategy { identity, strategy }],
        pruning,
    )
}

pub(in crate::cases) struct InjectedStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

impl BacktestSpec {
    /// Execute caller-authored strategy logic in this shared Rust Bar backtest.
    /// `strategy` on the BacktestSpec carries the strategy ID and slot kind only; all
    /// built-in signal and sizing parameters must remain at their defaults.
    pub fn run_with_strategy<S: Strategy + 'static>(
        &self,
        identity: &StrategyArtifactIdentity,
        strategy: S,
        context: &RunContext,
    ) -> Result<BacktestOutcome, AppError> {
        self.run_with_strategy_slots(vec![(identity.clone(), Box::new(strategy))], context)
    }

    /// Execute one or more caller-authored strategy slots in a shared Bar portfolio.
    /// Every identity must match a declared portfolio slot; omitted slots use their built-in
    /// implementation. This keeps portfolio capital, risk, OMS, matching, ledger and replay
    /// owned by the same Rust engine.
    pub fn run_with_strategy_slots(
        &self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: &RunContext,
    ) -> Result<BacktestOutcome, AppError> {
        context.require(CallerCapability::Research, "多语言策略 Bar 回测")?;
        let injected = strategies
            .into_iter()
            .map(|(identity, strategy)| InjectedStrategy { identity, strategy })
            .collect::<Vec<_>>();
        validate_injected_strategies(self, &injected)?;
        let correlation_id = context.correlation_id().to_string();
        let outcome = guard_panics(&correlation_id, || {
            run_backtest_inner_with_strategies(self, context, None, injected)
        })
        .map_err(|error| attach(error, &correlation_id))?;
        outcome.ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步多语言策略回测未产生报告",
                ),
                &correlation_id,
            )
        })
    }

    /// Start caller-authored strategy logic on the shared cancellable Bar run lifecycle.
    pub fn start_with_strategy<S: Strategy + 'static>(
        self,
        identity: StrategyArtifactIdentity,
        strategy: S,
        context: RunContext,
    ) -> Result<crate::RunHandle<BacktestOutcome>, AppError> {
        self.start_with_strategy_slots(vec![(identity, Box::new(strategy))], context)
    }

    /// Start a cancellable shared-engine run with multiple caller-authored Bar strategies.
    pub fn start_with_strategy_slots(
        self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: RunContext,
    ) -> Result<crate::RunHandle<BacktestOutcome>, AppError> {
        context.require(CallerCapability::Research, "多语言策略 Bar 回测")?;
        self.validate()?;
        let injected = strategies
            .into_iter()
            .map(|(identity, strategy)| InjectedStrategy { identity, strategy })
            .collect::<Vec<_>>();
        validate_injected_strategies(&self, &injected)?;
        let run_id = context.correlation_id().to_string();
        Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_backtest_inner_with_strategies(&self, &context, Some(&cancellation), injected)
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

fn validate_injected_strategies(
    spec: &BacktestSpec,
    strategies: &[InjectedStrategy],
) -> Result<(), AppError> {
    let mut seen = std::collections::BTreeSet::new();
    for strategy in strategies {
        if !seen.insert(strategy.identity.strategy_id.as_str()) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("自定义策略组合身份重复: {}", strategy.identity.strategy_id),
            ));
        }
        strategy.identity.validate_for_portfolio_slot(spec)?;
    }
    if spec.portfolio.is_none() && strategies.len() > 1 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多个自定义策略必须声明同账户 portfolio 组合规格",
        ));
    }
    Ok(())
}

pub(super) fn strategy_slots_config_hash(
    spec: &BacktestSpec,
    identities: &[StrategyArtifactIdentity],
) -> Result<String, AppError> {
    let mut hash = Fnv1a::new();
    hash.write_text(&spec_config_hash(spec)?);
    let mut identities = identities.iter().collect::<Vec<_>>();
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    for identity in identities {
        hash.write_text(&identity.strategy_id);
        hash.write_text(&identity.strategy_version);
        hash.write_text(&identity.source_sha256);
    }
    Ok(format!("{:016x}", hash.finish()))
}
