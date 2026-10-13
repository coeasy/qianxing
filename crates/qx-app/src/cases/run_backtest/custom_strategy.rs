//! Strategy artifact identity and caller-supplied strategy entrypoint.

use super::{run_backtest_inner_with_strategy, spec_config_hash, BacktestSpec};
use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::BacktestOutcome;
use qx_core::Fnv1a;
use qx_strategy::Strategy;

/// Immutable identity for a caller-supplied strategy implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyArtifactIdentity {
    pub strategy_id: String,
    pub strategy_version: String,
    pub source_sha256: String,
}

impl StrategyArtifactIdentity {
    pub(super) fn validate_for(&self, spec: &BacktestSpec) -> Result<(), AppError> {
        let valid_token = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        if !valid_token(&self.strategy_id)
            || self.strategy_id != spec.strategy.strategy_id
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
        if spec.portfolio.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied strategies cannot be combined with the built-in portfolio field",
            ));
        }
        let default_strategy = crate::spec::BuiltinStrategySpec::new(
            spec.strategy.kind,
            spec.strategy.strategy_id.clone(),
        );
        if spec.strategy != default_strategy {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied strategies must use default BuiltinStrategySpec parameters; built-in signal and sizing knobs do not apply to external callbacks",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_for_depth(
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
            || self.strategy_id != spec.strategy.strategy_id
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
        if spec.portfolio.is_some() || spec.market_making.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied depth strategies cannot be combined with built-in portfolio or market-making strategies",
            ));
        }
        let default_strategy = crate::spec::BuiltinStrategySpec::new(
            spec.strategy.kind,
            spec.strategy.strategy_id.clone(),
        );
        if spec.strategy != default_strategy {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "caller-supplied strategies must use default BuiltinStrategySpec parameters; built-in signal and sizing knobs do not apply to external callbacks",
            ));
        }
        Ok(())
    }
}

pub(super) struct InjectedStrategy {
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
        context.require(CallerCapability::Research, "多语言策略 Bar 回测")?;
        identity.validate_for(self)?;
        let correlation_id = context.correlation_id().to_string();
        let injected = InjectedStrategy {
            identity: identity.clone(),
            strategy: Box::new(strategy),
        };
        let outcome = guard_panics(&correlation_id, || {
            run_backtest_inner_with_strategy(self, context, None, Some(injected))
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
        context.require(CallerCapability::Research, "多语言策略 Bar 回测")?;
        self.validate()?;
        identity.validate_for(&self)?;
        let run_id = context.correlation_id().to_string();
        Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            let injected = InjectedStrategy {
                identity,
                strategy: Box::new(strategy),
            };
            guard_panics(&correlation_id, || {
                run_backtest_inner_with_strategy(
                    &self,
                    &context,
                    Some(&cancellation),
                    Some(injected),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

pub(super) fn strategy_config_hash(
    spec: &BacktestSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<String, AppError> {
    let mut hash = Fnv1a::new();
    hash.write_text(&spec_config_hash(spec)?);
    hash.write_text(&identity.strategy_id);
    hash.write_text(&identity.strategy_version);
    hash.write_text(&identity.source_sha256);
    Ok(format!("{:016x}", hash.finish()))
}
