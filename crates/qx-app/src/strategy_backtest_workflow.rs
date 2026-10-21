//! One application-owned workflow for executing and persisting a resolved strategy backtest.

use crate::artifact_store::{
    persist_strategy_backtest_result, BacktestAccountBase, BacktestInputProvenance,
    BacktestSignalParams, PersistedStrategyBacktestResult, StrategyBacktestArtifactsInput,
};
use crate::{
    run_bar_backtest_with_outcome, run_native_builtin_strategy_backtest_with_outcome, BacktestSpec,
    NativeStrategyBacktestContextBindings, RunContext, StrategyBacktestOutcome,
    StrategyBacktestRunIdentityInput, StrategyBacktestRunManifestInput,
};
use qx_core::InstrumentId;
use qx_guanxing::Bar;
use qx_strategy::BuiltinStrategyConfig;
use qx_xingban::BarStrategy;
use std::collections::BTreeMap;
use std::path::Path;

/// An execution form already resolved by the facade. The application owns dispatch and the
/// result/persistence boundary; the facade owns configuration parsing and adapter construction.
pub enum StrategyBacktestExecution {
    NativeBuiltin {
        strategy: Box<BuiltinStrategyConfig>,
        venue_id: Option<String>,
    },
    Adapter(Box<dyn BarStrategy>),
}

/// Resolved identity and artifact facts needed to publish a completed strategy run.
pub struct StrategyBacktestWorkflowInput<'a> {
    pub spec: &'a BacktestSpec,
    pub bars: &'a [Bar],
    pub execution: StrategyBacktestExecution,
    pub context: &'a RunContext,
    pub run_manifest_root: &'a Path,
    pub strategy_id: &'a str,
    pub strategy_version: &'a str,
    /// Product declared in the runtime config, which is not otherwise part of BacktestSpec.
    pub declared_product: Option<qx_core::TradingProduct>,
    pub config_fingerprint: &'a str,
    pub runtime_config_path: Option<&'a Path>,
    pub market_spec_path: Option<&'a Path>,
    pub instrument_spec_version: &'a str,
    pub runtime_version: &'a str,
    pub code_commit: &'a str,
    pub cost_source: &'a str,
    pub fill_model: (&'static str, &'static str),
    pub account_base: BacktestAccountBase,
    pub input: BacktestInputProvenance,
    pub dataset_bundle_fingerprint: Option<&'a str>,
    pub dataset_components: BTreeMap<String, String>,
    pub signal: Option<BacktestSignalParams>,
    pub dataset_quality: Option<serde_json::Value>,
}

/// Replay, bind the verified input identity, and publish the canonical manifest plus artifacts.
pub fn run_and_persist_strategy_backtest(
    input: StrategyBacktestWorkflowInput<'_>,
) -> Result<StrategyBacktestWorkflowResult, String> {
    if input
        .declared_product
        .is_some_and(qx_core::TradingProduct::is_derivative)
        && input.spec.market_spec.is_none()
    {
        return Err("衍生品跨语言回测必须提供 market-spec.json".into());
    }
    let data_fingerprint = input
        .dataset_bundle_fingerprint
        .map(|fingerprint| format!("dataset-bundle:{fingerprint}"))
        .unwrap_or_else(|| format!("{}:{}", input.input.kind, input.input.fingerprint));
    let instrument = InstrumentId::parse(&input.spec.instrument)
        .ok_or_else(|| format!("BacktestSpec.instrument 非法: {}", input.spec.instrument))?;
    let outcome = match input.execution {
        StrategyBacktestExecution::NativeBuiltin { strategy, venue_id } => {
            let context = NativeStrategyBacktestContextBindings {
                account_id: input
                    .spec
                    .account_id
                    .clone()
                    .ok_or("内置策略 BacktestSpec 缺少 account_id")?,
                venue_id: venue_id.unwrap_or_else(|| instrument.venue.to_string()),
                data_fingerprint: data_fingerprint.clone(),
                as_of: input.bars.first().map(|bar| bar.ts).unwrap_or(1),
                settlement_currency: input.spec.settlement_currency.clone(),
                initial_cash_raw: input.spec.initial_cash_raw,
            };
            run_native_builtin_strategy_backtest_with_outcome(
                input.spec,
                input.bars,
                *strategy,
                context,
                input.context,
            )
            .map_err(|error| format!("内置策略回测失败: {error}"))?
        }
        StrategyBacktestExecution::Adapter(mut strategy) => {
            run_bar_backtest_with_outcome(input.spec, input.bars, strategy.as_mut(), input.context)
                .map_err(|error| format!("跨语言策略回测失败: {error}"))?
        }
    };
    let risk_rule_set_version = outcome.risk_rule_set_version;
    let report = outcome.report;
    let persisted = persist_strategy_backtest_result(
        input.run_manifest_root,
        &report,
        StrategyBacktestRunManifestInput {
            identity: StrategyBacktestRunIdentityInput {
                strategy_id: input.strategy_id,
                instrument: &input.spec.instrument,
                config_fingerprint: input.config_fingerprint,
                data_fingerprint: &data_fingerprint,
                input_kind: input.input.kind,
                dataset_id: &input.input.dataset_id,
                dataset_version: &input.input.dataset_version,
                input_fingerprint: &input.input.fingerprint,
                market_spec: input.spec.market_spec.as_ref(),
                instrument_spec_version: Some(input.instrument_spec_version),
                dataset_quality: input.dataset_quality.as_ref(),
                model_descriptors: &report.model_descriptors,
                cost_source: input.cost_source,
                risk_rule_set_version: &risk_rule_set_version,
                result_hash: report.result_hash(),
            },
            code_commit: input.code_commit,
            strategy_version: input.strategy_version,
            instrument_spec_version: input.instrument_spec_version,
            runtime_version: input.runtime_version,
            input_components: input.dataset_components,
        },
        StrategyBacktestArtifactsInput {
            runtime_config_path: input.runtime_config_path,
            market_spec_path: input.market_spec_path,
            strategy_id: input.strategy_id,
            instrument: &instrument,
            bars: input.bars,
            risk_rule_set_version: &risk_rule_set_version,
            cost_source: input.cost_source,
            fill_model: input.fill_model,
            account_base: input.account_base,
            input: input.input.clone(),
            signal: input.signal,
            dataset_quality: input.dataset_quality.as_ref(),
        },
    )?;
    Ok(StrategyBacktestWorkflowResult {
        outcome: StrategyBacktestOutcome {
            report,
            risk_rule_set_version,
        },
        persisted,
    })
}

pub struct StrategyBacktestWorkflowResult {
    pub outcome: StrategyBacktestOutcome,
    pub persisted: PersistedStrategyBacktestResult,
}
