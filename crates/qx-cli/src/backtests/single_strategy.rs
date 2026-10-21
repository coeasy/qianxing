//! 单策略回测装配：`run_single_strategy_backtest` 与薄壳 `run_builtin_backtest`，
//! 以及只多一步"取 CCXT OHLCV 再交给薄壳"的 `run_ccxt_builtin_backtest`。

use super::{
    ashare_backtest_binding, backtest_account_base_note, market_spec_with_margin, rejection_count,
    rejection_facts_line, run_builtin_backtest_via_app, BacktestInputProvenance, Bar, BarFrame,
    ContractBarStrategy, MarketSpecLoad, RuntimeConfig,
};
use crate::build_identity;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) struct DatasetRunBinding<'a> {
    pub(crate) bundle_fingerprint: &'a str,
    pub(crate) component_fingerprints: &'a BTreeMap<String, String>,
}

pub(crate) struct SingleStrategyBacktestInput<'a> {
    pub(crate) config: &'a RuntimeConfig,
    pub(crate) config_fingerprint: String,
    pub(crate) runtime_config_path: Option<&'a Path>,
    pub(crate) frame: &'a BarFrame,
    pub(crate) bars: &'a [Bar],
    pub(crate) spec_path: Option<&'a Path>,
    pub(crate) strategy_id: &'a str,
    pub(crate) dataset_binding: Option<DatasetRunBinding<'a>>,
    pub(crate) run_manifest_root: &'a Path,
    // 本轮实际消费的那份输入的身份（路径 + 数据集 + 已复核指纹）。它由调用方从**唯一读点**
    // 拿到，在这里只往两处用：RunManifest 的 `data_fingerprint` 与产物摘要的 `input` 块。
    pub(crate) input: BacktestInputProvenance,
    pub(crate) dataset_quality: Option<serde_json::Value>,
}

pub(crate) fn run_single_strategy_backtest(
    request: SingleStrategyBacktestInput<'_>,
) -> Result<(), String> {
    let SingleStrategyBacktestInput {
        config,
        config_fingerprint,
        runtime_config_path,
        frame,
        bars,
        spec_path,
        strategy_id,
        dataset_binding,
        run_manifest_root,
        input,
        dataset_quality,
    } = request;
    let MarketSpecLoad {
        spec: instrument_spec,
        app_margin_model,
        source: instrument_spec_version,
        ..
    } = market_spec_with_margin(&frame.instrument, spec_path, "策略回测")?;
    let mut ashare_rules = None;
    let mut ashare_rules_path: Option<String> = None;
    if let Some(binding) = ashare_backtest_binding(
        config.strategy.ashare_rules_path.as_deref(),
        config.strategy.ashare_actions_path.as_deref(),
        config.strategy.ashare_calendar_path.as_deref(),
        &frame.instrument.to_string(),
    )? {
        ashare_rules = Some(binding.rules);
        ashare_rules_path = Some(binding.rules_path);
    }
    let bindings = qx_service::backtest_bindings::single_strategy::
        single_strategy_backtest_bindings_from_runtime(
            qx_service::backtest_bindings::single_strategy::SingleStrategyBacktestBindingInput {
                config,
                runtime_config_path,
                frame,
                input_path: input.path.clone(),
                strategy_id,
                instrument_spec,
                margin_model: app_margin_model,
                instrument_spec_version,
                ashare_rules,
                ashare_rules_path: ashare_rules_path.as_deref(),
            },
        )?;
    if let Some(note) = bindings.builtin_signal_note.as_deref() {
        println!("[Strategy · Signal] {note}");
    }
    let qx_service::backtest_bindings::single_strategy::SingleStrategyBacktestRuntimeBindings {
        app_spec,
        account_base,
        cost_source,
        fill_model_name,
        fill_model_source,
        builtin_execution,
        builtin_signal,
        ..
    } = bindings;
    let execution = match builtin_execution {
        Some(execution) => execution,
        None => {
            let strategy = ContractBarStrategy::from_config(
                config.clone(),
                frame,
                account_base.cash,
                app_spec.settlement_currency.clone(),
                dataset_binding
                    .as_ref()
                    .map(|binding| binding.bundle_fingerprint),
                app_spec.market_spec.clone(),
            )?;
            qx_app::StrategyBacktestExecution::Adapter(Box::new(strategy))
        }
    };
    let app_context = qx_app::RunContext::new(
        qx_app::CallerCapability::Research,
        format!("strategy-backtest:{strategy_id}:{}", frame.instrument),
    )
    .with_code_commit(build_identity::BUILD_REVISION);
    let runtime_version = format!("runtime-schema-{}", config.schema_version);
    let workflow =
        qx_app::run_and_persist_strategy_backtest(qx_app::StrategyBacktestWorkflowInput {
            spec: &app_spec,
            bars,
            execution,
            context: &app_context,
            run_manifest_root,
            strategy_id,
            strategy_version: &config.strategy.version,
            declared_product: config.strategy.product,
            config_fingerprint: &config_fingerprint,
            runtime_config_path,
            market_spec_path: spec_path,
            instrument_spec_version,
            runtime_version: &runtime_version,
            code_commit: build_identity::BUILD_REVISION,
            cost_source: &cost_source,
            fill_model: (fill_model_name, fill_model_source),
            account_base,
            input,
            dataset_bundle_fingerprint: dataset_binding
                .as_ref()
                .map(|binding| binding.bundle_fingerprint),
            dataset_components: dataset_binding
                .as_ref()
                .map(|binding| binding.component_fingerprints.clone())
                .unwrap_or_default(),
            signal: builtin_signal,
            dataset_quality,
        })?;
    let report = workflow.outcome.report;
    let persisted = workflow.persisted;
    let rejections = persisted.artifacts.rejections;
    println!(
        "[Strategy · Account] {}",
        backtest_account_base_note(account_base)
    );
    println!(
        "[Strategy · Backtest] strategy={} instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        strategy_id,
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    println!(
        "[Strategy · Integrity] rejected_orders={} rejection_reasons={}",
        rejection_count(&rejections),
        rejection_facts_line(&rejections)
    );
    println!(
        "[RunManifest] run_id={} data_fingerprint={} result_hash={} digest={:016x}",
        persisted.manifest.run_id,
        persisted.manifest.data_fingerprint,
        persisted.manifest.result_hash,
        persisted.manifest.digest()
    );
    println!("[RunManifest] path={}", persisted.manifest_path.display());
    println!(
        "[Artifacts] summary={} equity={} fills={}",
        persisted.artifacts.summary_path.display(),
        persisted.artifacts.equity_path.display(),
        persisted.artifacts.fills_path.display()
    );
    Ok(())
}
pub(crate) fn run_builtin_backtest(
    strategy_name: &str,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
    runtime_config_path: Option<&Path>,
    quality_manifest_path: Option<&Path>,
) -> Result<(), String> {
    run_builtin_backtest_via_app(
        strategy_name,
        frame_path,
        spec_path,
        quantity,
        runtime_config_path,
        quality_manifest_path,
    )
}
