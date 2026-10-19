//! 单策略回测装配：`run_single_strategy_backtest` 与薄壳 `run_builtin_backtest`，
//! 以及只多一步"取 CCXT OHLCV 再交给薄壳"的 `run_ccxt_builtin_backtest`。

use super::{
    account_base_from_config, ashare_backtest_binding, backtest_account_base_note,
    backtest_settlement_currency, bar_fill_model, builtin_signal_note, builtin_signal_params,
    builtin_signal_provenance, config_margin_mode, execution_cost_binding_from_config,
    market_spec_with_margin, persist_backtest_artifacts, rejection_count, rejection_facts,
    rejection_facts_line, run_builtin_backtest_via_app, strategy_allows_short, BacktestArtifacts,
    BacktestInputProvenance, Bar, BarFrame, BuiltinStrategyKind, ContractBarStrategy, InstrumentId,
    MarketSpecLoad, RuntimeConfig, TradingProduct, BAR_MATCHING_KERNEL,
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
    if config
        .strategy
        .product
        .is_some_and(TradingProduct::is_derivative)
        && instrument_spec.is_none()
    {
        return Err("衍生品跨语言回测必须提供 market-spec.json".into());
    }
    // None 表示沿用成本绑定给出的费率模型（`cost_rules_path` 或内核默认 2/5bp）。
    // A 股规则快照自带一套佣金/印花税模型，它会覆盖成本绑定的费率——产物必须改口说明。
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
    let account_id = config
        .strategy
        .account_id
        .clone()
        .unwrap_or_else(|| "backtest".into());
    let account_base = account_base_from_config(config)?;
    let initial_cash = account_base.cash;
    // 风险规则输入与 Paper/Live 共用策略配置解析；应用 spec 是 Bar 执行语义的归一形状。
    let allow_short = strategy_allows_short(config, config_margin_mode(config));
    // 成本绑定取自内存里这一份策略配置：多策略清单里每条策略都有自己的
    // `cost_rules_path`，从磁盘重读只会拿到默认那条。
    let costs = execution_cost_binding_from_config(config, runtime_config_path)?;
    // 撮合口径与费用同源：也从这一份策略配置解析（V11 Q1a 第二批）。放在 spec 之后、
    // 装配之前，因为 `one_tick_slippage` 的一档只能取自刚解析出来的 market spec。
    let fill = bar_fill_model(
        config.strategy.fill_model.as_deref(),
        instrument_spec.as_ref(),
    )?;
    let (fill_model_name, fill_model_source) = (fill.name, fill.source);
    let fill_model = fill.model;
    let cost_source = costs.source_with_ashare_rules(ashare_rules_path.as_deref());
    let app_strategy =
        qx_app::BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, strategy_id.to_string());
    let risk_rules = super::app_backtest_risk_rules(config.strategy.risk_rules.as_ref());
    let app_spec = qx_app::assemble_strategy_backtest_spec(
        qx_app::BacktestDatasetInput {
            run_id: "strategy-backtest".into(),
            instrument: frame.instrument.to_string(),
            bars_path: input.path.clone(),
        },
        app_strategy,
        qx_app::StrategyBacktestBindings {
            settlement_currency: instrument_spec
                .as_ref()
                .map(|spec| spec.settlement_currency.clone())
                .unwrap_or_else(|| backtest_settlement_currency(None)),
            initial_cash_raw: initial_cash.raw(),
            seed: 20260911,
            output_dir: ".".into(),
            account_id: account_id.clone(),
            market_spec: instrument_spec.clone(),
            margin_model: app_margin_model,
            execution_cost_rules: Some(costs.rules.clone()),
            ashare_rules: ashare_rules.clone(),
            risk_rules,
            allow_short,
            fill_model,
            max_bar_volume_participation_bps: config.strategy.max_bar_volume_participation_bps,
        },
    );
    // 摘要要回答"这一轮跑的是哪套窗口"，而那四项只有内置链参与（V12 R4-j）。跨语言策略链的
    // 口径由策略自己声明，内置窗口根本没上场，所以它留 None——摘要里不落这个键。
    let mut builtin_signal = None;
    let outcome = if config.strategy.builtin_strategy.is_some() {
        if let Some(configured) = config.strategy.instrument.as_deref() {
            let configured = InstrumentId::parse(configured)
                .ok_or_else(|| format!("Strategy instrument 非法: {configured}"))?;
            if configured != frame.instrument {
                return Err(format!(
                    "内置策略回测 instrument 不一致: strategy={} frame={}",
                    configured, frame.instrument
                ));
            }
        }
        let builtin_config =
            qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, &frame.instrument)
                .map_err(|error| match runtime_config_path {
                Some(path) => format!("{error}；runtime_config={}", path.display()),
                None => error,
            })?;
        let signal_provenance = builtin_signal_provenance(builtin_config.kind, &config.strategy);
        // 口径要在 `builtin_config` 被策略吃掉之前取走，摘要才有这句话可写（V12 R4-j）。
        builtin_signal = Some(builtin_signal_params(&builtin_config, &signal_provenance));
        println!(
            "[Strategy · Signal] {}",
            builtin_signal_note(&builtin_config, &signal_provenance)
        );
        let canonical_barframe_fingerprint = qx_data::fingerprint_bar_frame(frame)?;
        let app_context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("strategy-backtest:{strategy_id}:{}", frame.instrument),
        )
        .with_code_commit(build_identity::BUILD_REVISION);
        qx_app::run_native_builtin_strategy_backtest_with_outcome(
            &app_spec,
            bars,
            builtin_config,
            qx_app::NativeStrategyBacktestContextBindings {
                account_id: account_id.clone(),
                venue_id: config
                    .strategy
                    .venue_id
                    .clone()
                    .unwrap_or_else(|| frame.instrument.venue.to_string()),
                data_fingerprint: dataset_binding
                    .as_ref()
                    .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
                    .unwrap_or_else(|| format!("barframe:{canonical_barframe_fingerprint}")),
                as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
                settlement_currency: app_spec.settlement_currency.clone(),
                initial_cash_raw: initial_cash.raw(),
            },
            &app_context,
        )
        .map_err(|error| format!("内置策略回测失败: {error}"))?
    } else {
        let mut strategy = ContractBarStrategy::from_config(
            config.clone(),
            frame,
            initial_cash,
            app_spec.settlement_currency.clone(),
            dataset_binding
                .as_ref()
                .map(|binding| binding.bundle_fingerprint),
            instrument_spec.clone(),
        )?;
        let context = qx_app::RunContext::new(
            qx_app::CallerCapability::Research,
            format!("strategy-backtest:{strategy_id}:{}", frame.instrument),
        )
        .with_code_commit(build_identity::BUILD_REVISION);
        qx_app::run_bar_backtest_with_outcome(&app_spec, bars, &mut strategy, &context)
            .map_err(|error| format!("跨语言策略回测失败: {error}"))?
    };
    let risk_rule_set_version = outcome.risk_rule_set_version;
    let report = outcome.report;
    // Bundle 场景仍然优先：那是跨多个组件的合成指纹。没有 Bundle 时，这一格必须是**被数据集
    // 注册表复核过**的那份输入指纹，而不是 `report.input_data_hash`——后者是引擎对自己手里那段
    // 切片的自哈希，既不含 instrument 也不含来源，换掉输入文件它照算不误（V11 Q66 / Q1b）。
    let data_fingerprint = dataset_binding
        .as_ref()
        .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
        .unwrap_or_else(|| format!("{}:{}", input.kind, input.fingerprint));
    // App owns the durable identity schema so other facades cannot drift from CLI naming.
    let (_run_id, run_manifest, run_manifest_path) =
        qx_app::artifact_store::persist_strategy_backtest_run_manifest(
            run_manifest_root,
            &report,
            qx_app::StrategyBacktestRunManifestInput {
                identity: qx_app::StrategyBacktestRunIdentityInput {
                    strategy_id,
                    instrument: &frame.instrument.to_string(),
                    config_fingerprint: &config_fingerprint,
                    data_fingerprint: &data_fingerprint,
                    input_kind: input.kind,
                    dataset_id: &input.dataset_id,
                    dataset_version: &input.dataset_version,
                    input_fingerprint: &input.fingerprint,
                    market_spec: instrument_spec.as_ref(),
                    instrument_spec_version: Some(instrument_spec_version),
                    dataset_quality: dataset_quality.as_ref(),
                    model_descriptors: &report.model_descriptors,
                    cost_source: &cost_source,
                    risk_rule_set_version: &risk_rule_set_version,
                    result_hash: report.result_hash(),
                },
                code_commit: build_identity::BUILD_REVISION,
                strategy_version: &config.strategy.version,
                instrument_spec_version,
                runtime_version: &format!("runtime-schema-{}", config.schema_version),
                input_components: dataset_binding
                    .map(|binding| binding.component_fingerprints.clone())
                    .unwrap_or_default(),
            },
        )?;
    let rejections = rejection_facts(&report.event_log);
    let (summary_path, equity_path, fills_path) = persist_backtest_artifacts(
        &run_manifest_path,
        &BacktestArtifacts {
            runtime_config_path,
            market_spec_path: spec_path,
            strategy_id,
            instrument: &frame.instrument,
            sample_unit: "bar",
            samples: bars.len(),
            sample_ts: qx_app::artifact_store::BacktestSampleTimestamps::Bars(bars),
            equity: &report.equity,
            positions: &report.positions,
            fills: &report.fills,
            execution_attempts: &report.execution_attempts,
            clock_start: report.clock_start,
            clock_end: report.clock_end,
            input_data_hash: report.input_data_hash,
            result_hash: report.result_hash(),
            event_log: &report.event_log,
            ledger: &report.ledger,
            issuer_capital: &report.issuer_capital_snapshots,
            issuer_capital_at_end: report.issuer_capital_at(report.clock_end),
            return_bps: report.return_bps,
            max_drawdown_bps: report.max_drawdown_bps,
            fees_raw: report.fees_raw,
            turnover_raw: report.turnover_raw,
            final_equity_raw: report.final_equity(),
            assumptions: &report.assumptions,
            model_descriptors: &report.model_descriptors,
            risk_rule_set_version: &risk_rule_set_version,
            risk_rule_source: "runtime-config",
            cost_source: &cost_source,
            fill_model: Some((fill_model_name, fill_model_source)),
            fill_model_details: Some(&report.fill_model_descriptor),
            execution_model_details: None,
            account_base,
            matching_kernel: BAR_MATCHING_KERNEL,
            rejections: &rejections,
            input,
            signal: builtin_signal,
            dataset_quality: dataset_quality.as_ref(),
        },
    )?;
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
        run_manifest.run_id,
        run_manifest.data_fingerprint,
        run_manifest.result_hash,
        run_manifest.digest()
    );
    println!("[RunManifest] path={}", run_manifest_path.display());
    println!(
        "[Artifacts] summary={} equity={} fills={}",
        summary_path.display(),
        equity_path.display(),
        fills_path.display()
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
