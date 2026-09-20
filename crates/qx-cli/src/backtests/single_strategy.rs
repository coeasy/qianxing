//! 单策略回测装配：`run_single_strategy_backtest` 与薄壳 `run_builtin_backtest`。

use super::*;

pub(crate) struct DatasetRunBinding<'a> {
    pub(crate) bundle_fingerprint: &'a str,
    pub(crate) component_fingerprints: &'a BTreeMap<String, String>,
}

pub(crate) fn run_single_strategy_backtest(
    config: &RuntimeConfig,
    frame: &BarFrame,
    bars: &[Bar],
    spec_path: Option<&Path>,
    strategy_id: &str,
    dataset_binding: Option<DatasetRunBinding<'_>>,
    run_manifest_root: &Path,
) -> Result<(), String> {
    let (instrument_spec, margin) =
        market_spec_with_margin(&frame.instrument, spec_path, "策略回测")?;
    if config
        .strategy
        .product
        .is_some_and(TradingProduct::is_derivative)
        && instrument_spec.is_none()
    {
        return Err("衍生品跨语言回测必须提供 market-spec.json".into());
    }
    let mut virtual_trading = VirtualTradingConfig::default();
    // None 表示沿用内核默认的 maker/taker 2/5 bp 费率。
    let mut fee: Option<Box<dyn FeeModel>> = None;
    if let Some(path) = config.strategy.ashare_rules_path.as_deref() {
        let payload = std::fs::read_to_string(path)
            .map_err(|error| format!("读取 A 股规则快照失败 {path}: {error}"))?;
        let mut rules: AshareRuleConfig = serde_json::from_str(&payload)
            .map_err(|error| format!("A 股规则快照 JSON 无效 {path}: {error}"))?;
        if let Some(actions_path) = config.strategy.ashare_actions_path.as_deref() {
            let actions_payload = std::fs::read_to_string(actions_path)
                .map_err(|error| format!("读取 A 股公司行为 JSON 失败 {actions_path}: {error}"))?;
            rules
                .apply_corporate_actions_json(&frame.instrument.to_string(), &actions_payload)
                .map_err(|error| format!("A 股公司行为 JSON 非法: {error}"))?;
        }
        if let Some(calendar_path) = config.strategy.ashare_calendar_path.as_deref() {
            let calendar_payload = std::fs::read_to_string(calendar_path)
                .map_err(|error| format!("读取 A 股交易日历 JSON 失败 {calendar_path}: {error}"))?;
            rules
                .apply_calendar_json(&calendar_payload)
                .map_err(|error| format!("A 股交易日历 JSON 非法: {error}"))?;
        }
        rules
            .validate()
            .map_err(|error| format!("A 股规则快照非法: {error}"))?;
        if !rules.enabled {
            return Err("配置 ashare_rules_path 后 enabled 必须为 true".into());
        }
        virtual_trading.ashare_rules = Some(rules.clone());
        fee = Some(Box::new(AShareFeeModel {
            commission_bp: rules.commission_bp,
            min_commission: rules.min_commission,
            stamp_duty_bp: rules.stamp_duty_bp,
            transfer_fee_bp: rules.transfer_fee_bp,
        }));
    }
    if (config.strategy.ashare_actions_path.is_some()
        || config.strategy.ashare_calendar_path.is_some())
        && config.strategy.ashare_rules_path.is_none()
    {
        return Err(
            "配置 ashare_actions_path 或 ashare_calendar_path 时必须同时配置 ashare_rules_path"
                .into(),
        );
    }
    let account_id = config
        .strategy
        .account_id
        .clone()
        .unwrap_or_else(|| "backtest".into());
    let currency = instrument_spec
        .as_ref()
        .map(|spec| spec.settlement_currency.clone())
        .unwrap_or_else(|| "USDT".into());
    let initial_cash = Money::from_i64(100_000);
    // 门禁只构造一次：回测判定与摘要里的 rule_set_version 必须来自同一份配置，
    // 且与 Paper/Live worker 走同一个 `strategy_risk_gate` 入口。
    let risk_gate = strategy_risk_gate(
        config.strategy.risk_rules.as_ref(),
        strategy_allows_short(config, config_margin_mode(config)),
    );
    let risk_rule_set_version = risk_gate.rule_set().version().to_string();
    let mut assembly = BarBacktestAssembly::new(&frame.instrument, account_id, 20260911);
    assembly.instrument_spec = instrument_spec;
    assembly.margin = margin;
    assembly.currency = currency;
    assembly.initial_cash = initial_cash;
    assembly.risk = risk_gate;
    assembly.virtual_trading = virtual_trading;
    if let Some(fee) = fee {
        assembly.fee = fee;
    }
    let backtest_config = assembly.into_config();
    let report = if config.strategy.builtin_strategy.is_some() {
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
            builtin_strategy_config_from_runtime(&config.strategy, &frame.instrument)?;
        let context = NativeStrategyContext {
            strategy_id: builtin_config.strategy_id.clone(),
            strategy_version: builtin_config.strategy_version.clone(),
            account_id: config
                .strategy
                .account_id
                .clone()
                .unwrap_or_else(|| "backtest".into()),
            venue_id: config
                .strategy
                .venue_id
                .clone()
                .unwrap_or_else(|| frame.instrument.venue.to_string()),
            data_fingerprint: dataset_binding
                .as_ref()
                .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
                .unwrap_or_else(|| format!("barframe:{:016x}", frame.digest())),
            as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
            positions: BTreeMap::new(),
            cash: BTreeMap::from([(backtest_config.currency.clone(), initial_cash.raw())]),
            available_margin_raw: Some(initial_cash.raw()),
            risk_state: "backtest-verified".into(),
        };
        run_builtin_strategy_on_bars(backtest_config, builtin_config, context, bars)?
    } else {
        let mut strategy = ContractBarStrategy::from_config(
            config.clone(),
            frame,
            initial_cash,
            backtest_config.currency.clone(),
            dataset_binding
                .as_ref()
                .map(|binding| binding.bundle_fingerprint),
        )?;
        BacktestEngine::new(backtest_config)
            .run(bars, &mut strategy)
            .map_err(|error| format!("跨语言策略回测失败: {error:?}"))?
    };
    let data_fingerprint = dataset_binding
        .as_ref()
        .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
        .unwrap_or_else(|| format!("{:016x}", report.input_data_hash));
    let run_manifest = report.run_manifest_with_input_components(
        RunManifestIdentity {
            run_id: &format!("strategy-backtest:{strategy_id}:{}", frame.instrument),
            code_commit: "workspace",
            config_hash: &config.fingerprint()?,
            strategy_version: &config.strategy.version,
            instrument_spec_version: if spec_path.is_some() {
                "ccxt-market-spec-v1"
            } else {
                "default-instrument-spec-v1"
            },
            runtime_version: &format!("runtime-schema-{}", config.schema_version),
        },
        &data_fingerprint,
        dataset_binding
            .map(|binding| binding.component_fingerprints.clone())
            .unwrap_or_default(),
    )?;
    let run_manifest_path = persist_backtest_run_manifest(run_manifest_root, &run_manifest)?;
    let bar_ts: Vec<u64> = bars.iter().map(|bar| bar.ts).collect();
    let (summary_path, equity_path, fills_path) = persist_backtest_artifacts(
        &run_manifest_path,
        &BacktestArtifacts {
            strategy_id,
            instrument: &frame.instrument,
            sample_unit: "bar",
            samples: bars.len(),
            sample_ts: &bar_ts,
            equity: &report.equity,
            positions: &report.positions,
            fills: &report.fills,
            clock_start: report.clock_start,
            clock_end: report.clock_end,
            input_data_hash: report.input_data_hash,
            result_hash: report.result_hash(),
            replay_hash: report.replay_hash(),
            return_bps: report.return_bps,
            max_drawdown_bps: report.max_drawdown_bps,
            fees_raw: report.fees_raw,
            turnover_raw: report.turnover_raw,
            final_equity_raw: report.final_equity(),
            assumptions: &report.assumptions,
            model_descriptors: &report.model_descriptors,
            risk_rule_set_version: &risk_rule_set_version,
            risk_rule_source: "runtime-config",
            matching_kernel: BAR_MATCHING_KERNEL,
        },
    )?;
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
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    let payload = std::fs::read_to_string(frame_path).map_err(|error| {
        format!(
            "读取内置策略 BarFrame 失败 {}: {error}",
            frame_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "内置策略 BarFrame 校验失败 {}: {error:?}",
            frame_path.display()
        )
    })?;
    let bars: Vec<Bar> = (&frame).into();
    if bars.len() < 3 {
        return Err("内置策略回测至少需要三根 Bar".into());
    }

    let (instrument_spec, margin) =
        market_spec_with_margin(&frame.instrument, spec_path, "内置策略")?;
    let risk_binding = backtest_risk_binding(runtime_config_path, false)?;
    let mut assembly = BarBacktestAssembly::new(&frame.instrument, "main", 20260914);
    assembly.instrument_spec = instrument_spec;
    assembly.margin = margin;
    assembly.risk = risk_binding.gate();
    let context = NativeStrategyContext {
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "main".into(),
        venue_id: frame.instrument.venue.to_string(),
        data_fingerprint: format!("barframe:{:?}", frame.source),
        as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "ready".into(),
    };
    let report = run_builtin_strategy_on_bars(
        assembly.into_config(),
        BuiltinStrategyConfig::new(
            kind,
            format!("builtin-{}", kind.name()),
            frame.instrument.clone(),
            Quantity::from_i64(quantity),
        )?,
        context,
        &bars,
    )?;
    println!(
        "[Builtin · Backtest] strategy={} instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        kind.name(),
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    println!(
        "[Builtin · Risk] rule_set_version={} source={} kernel={}",
        risk_binding.gate().rule_set().version(),
        risk_binding.source(),
        BAR_MATCHING_KERNEL
    );
    Ok(())
}
