//! N-market shared-account Bar backtest through the single PortfolioBarEngine.

use super::attach;
use super::guard::guard_panics;
use super::run_multi_market_manifest::{dataset_manifest_fingerprint, invalid};
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiMarketBacktestOutcome,
    MultiMarketBacktestSpec, RunContext, StrategyArtifactIdentity,
};
use qx_core::Fnv1a;
use qx_strategy::Strategy;
use std::collections::BTreeMap;

pub(crate) mod canonical_spec;
mod engine_execution;
mod market_preparation;
mod report_persistence;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MultiMarketExecutionWindow {
    pub(crate) trade_start_ts: Option<u64>,
    pub(crate) data_end_ts_exclusive: Option<u64>,
}

pub fn run_multi_market_backtest(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
) -> Result<MultiMarketBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多市场共享账户 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner(spec, context, None, BTreeMap::new())
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多市场回测意外返回取消状态",
            ),
            &correlation_id,
        )
    })
}

impl MultiMarketBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiMarketBacktestOutcome> {
        let run_id = self.run_id.clone();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场共享账户 Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), BTreeMap::new())
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Run selected market slots with caller-authored strategies on the same Rust engine.
    pub fn run_with_strategy_slots(
        &self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: &RunContext,
    ) -> Result<MultiMarketBacktestOutcome, AppError> {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多市场多语言策略 Bar 回测")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        let injected =
            validate_injected(self, strategies).map_err(|error| attach(error, &correlation_id))?;
        guard_panics(&correlation_id, || run_inner(self, context, None, injected))
            .map_err(|error| attach(error, &correlation_id))?
            .ok_or_else(|| {
                attach(
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "同步多市场自定义策略回测未产生报告",
                    ),
                    &correlation_id,
                )
            })
    }

    /// Start a cancellable multi-market run with caller-authored strategy slots.
    pub fn start_with_strategy_slots(
        self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: RunContext,
    ) -> Result<crate::RunHandle<MultiMarketBacktestOutcome>, AppError> {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多市场多语言策略 Bar 回测")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        let injected =
            validate_injected(&self, strategies).map_err(|error| attach(error, &correlation_id))?;
        let run_id = self.run_id.clone();
        Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), injected)
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

pub(super) struct InjectedMarketStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

pub(super) fn validate_injected(
    spec: &MultiMarketBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError> {
    if strategies.is_empty() {
        return Err(invalid("多市场自定义策略组合至少需要一个回调槽"));
    }
    let mut injected = BTreeMap::new();
    for (identity, strategy) in strategies {
        let slot = spec
            .markets
            .iter()
            .flat_map(|market| &market.strategies)
            .find(|member| member.strategy.strategy_id == identity.strategy_id)
            .ok_or_else(|| {
                invalid(format!(
                    "自定义策略身份不属于多市场组合: {}",
                    identity.strategy_id
                ))
            })?;
        identity.validate_for_slot(&slot.strategy.strategy_id, &slot.strategy)?;
        let id = identity.strategy_id.clone();
        if injected
            .insert(id.clone(), InjectedMarketStrategy { identity, strategy })
            .is_some()
        {
            return Err(invalid(format!("多市场自定义策略身份重复: {id}")));
        }
    }
    Ok(injected)
}

pub(super) fn run_inner(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    injected: BTreeMap<String, InjectedMarketStrategy>,
) -> Result<Option<MultiMarketBacktestOutcome>, AppError> {
    run_inner_with_window(spec, context, cancellation, injected, None, None)
}

pub(super) fn run_inner_with_window(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    injected: BTreeMap<String, InjectedMarketStrategy>,
    execution_window: Option<MultiMarketExecutionWindow>,
    experiment_parameters: Option<&BTreeMap<String, i128>>,
) -> Result<Option<MultiMarketBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    if execution_window.is_some_and(|window| {
        window
            .trade_start_ts
            .zip(window.data_end_ts_exclusive)
            .is_some_and(|(start, end)| start >= end)
    }) {
        return Err(invalid(
            "多市场实验执行窗口必须满足 trade_start_ts < data_end_ts_exclusive",
        ));
    }
    spec.validate()?;
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.bars_path.clear();
        market.dataset_manifest_path = None;
    }
    let spec_json = identity_spec.to_json()?;
    let mut spec_hasher = Fnv1a::new();
    spec_hasher.write_text(&spec_json);
    if let Some(window) = execution_window {
        spec_hasher.write_text(&serde_json::to_string(&window).map_err(|error| {
            AppError::new(AppErrorCategory::InternalInvariant, error.to_string())
        })?);
    }
    if let Some(parameters) = experiment_parameters {
        spec_hasher.write_text(&serde_json::to_string(parameters).map_err(|error| {
            AppError::new(AppErrorCategory::InternalInvariant, error.to_string())
        })?);
    }
    let spec_fingerprint_base = spec_hasher.finish();

    let Some(prepared_markets) = market_preparation::prepare_multi_market_markets(
        spec,
        cancellation,
        execution_window,
        spec_fingerprint_base,
        injected,
    )?
    else {
        return Ok(None);
    };
    let mut prepared = prepared_markets.prepared;
    let routed_strategies = prepared_markets.routed_strategies;
    let strategy_count = prepared_markets.strategy_count;
    let strategy_identities = prepared_markets.strategy_identities;
    let spec_fingerprint = prepared_markets.spec_fingerprint;
    let data_identities = prepared_markets.data_identities;
    let input_identity = crate::multi_market_report::multi_market_input_set_identity_v1(
        spec,
        &prepared
            .iter()
            .map(|market| {
                Ok(crate::MultiMarketInputIdentityV1 {
                    instrument: market.identity.instrument.clone(),
                    bar_fingerprint: market.identity.content_hash.clone(),
                    quality_fingerprint: market
                        .dataset_quality
                        .as_ref()
                        .map(dataset_manifest_fingerprint)
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?,
    )?;
    let data_fingerprint = input_identity.data_fingerprint;
    let Some((report, initial_cash_exact)) = engine_execution::run_multi_market_bar_engine(
        spec,
        &mut prepared,
        routed_strategies,
        execution_window,
        cancellation,
    )?
    else {
        return Ok(None);
    };
    report
        .verify()
        .map_err(|error| AppError::from_qx_error(&error))?;
    report_persistence::persist_multi_market_report(
        spec,
        context,
        report,
        prepared,
        strategy_identities,
        data_identities,
        data_fingerprint,
        spec_fingerprint,
        strategy_count,
        execution_window,
        experiment_parameters,
        input_identity.input_components,
        initial_cash_exact,
    )
}
