//! 单策略回测装配：`run_single_strategy_backtest` 与薄壳 `run_builtin_backtest`，
//! 以及只多一步"取 CCXT OHLCV 再交给薄壳"的 `run_ccxt_builtin_backtest`。

use super::*;

pub(crate) struct DatasetRunBinding<'a> {
    pub(crate) bundle_fingerprint: &'a str,
    pub(crate) component_fingerprints: &'a BTreeMap<String, String>,
}

#[allow(clippy::too_many_arguments)] // 同深度/多腿入口：配置路径与产物根由编译器逐个点名，不合并成参数包。
pub(crate) fn run_single_strategy_backtest(
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
    frame: &BarFrame,
    bars: &[Bar],
    spec_path: Option<&Path>,
    strategy_id: &str,
    dataset_binding: Option<DatasetRunBinding<'_>>,
    run_manifest_root: &Path,
    // 本轮实际消费的那份输入的身份（路径 + 数据集 + 已复核指纹）。它由调用方从**唯一读点**
    // 拿到，在这里只往两处用：RunManifest 的 `data_fingerprint` 与产物摘要的 `input` 块。
    input: BacktestInputProvenance,
) -> Result<(), String> {
    let MarketSpecLoad {
        spec: instrument_spec,
        margin,
        source: instrument_spec_version,
    } = market_spec_with_margin(&frame.instrument, spec_path, "策略回测")?;
    if config
        .strategy
        .product
        .is_some_and(TradingProduct::is_derivative)
        && instrument_spec.is_none()
    {
        return Err("衍生品跨语言回测必须提供 market-spec.json".into());
    }
    let mut virtual_trading = VirtualTradingConfig::default();
    // None 表示沿用成本绑定给出的费率模型（`cost_rules_path` 或内核默认 2/5bp）。
    // A 股规则快照自带一套佣金/印花税模型，它会覆盖成本绑定的费率——产物必须改口说明。
    let mut fee: Option<Box<dyn FeeModel>> = None;
    let mut ashare_rules_path: Option<String> = None;
    if let Some(binding) = ashare_backtest_binding(
        config.strategy.ashare_rules_path.as_deref(),
        config.strategy.ashare_actions_path.as_deref(),
        config.strategy.ashare_calendar_path.as_deref(),
        &frame.instrument.to_string(),
    )? {
        virtual_trading.ashare_rules = Some(binding.rules);
        fee = Some(binding.fee);
        ashare_rules_path = Some(binding.rules_path);
    }
    let account_id = config
        .strategy
        .account_id
        .clone()
        .unwrap_or_else(|| "backtest".into());
    let account_base = account_base_from_config(config)?;
    let initial_cash = account_base.cash;
    // 门禁只构造一次：回测判定与摘要里的 rule_set_version 必须来自同一份配置，
    // 且与 Paper/Live worker 走同一个 `strategy_risk_gate` 入口。
    let risk_gate = strategy_risk_gate(
        config.strategy.risk_rules.as_ref(),
        strategy_allows_short(config, config_margin_mode(config)),
    );
    let risk_rule_set_version = risk_gate.rule_set().version().to_string();
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
    let mut assembly = BarBacktestAssembly::new(
        &frame.instrument,
        account_id.clone(),
        initial_cash,
        20260911,
        &costs,
        fill,
    );
    assembly.instrument_spec = instrument_spec;
    assembly.margin = margin;
    assembly.risk = risk_gate;
    assembly.virtual_trading = virtual_trading;
    if let Some(fee) = fee {
        assembly.fee = fee;
    }
    let cost_source = match (ashare_rules_path, costs.loaded_from.as_deref()) {
        // A 股规则快照自带佣金模型，它顶掉成本文件里的费率，但成本文件的延迟仍然生效：
        // 两个来源都要写进产物，否则摘要会把延迟口径也算到 A 股规则头上。
        (Some(rules_path), Some(path)) => {
            format!(
                "ashare-rules:{rules_path}+cost-rules-file:{}",
                path.display()
            )
        }
        (Some(rules_path), None) => format!("ashare-rules:{rules_path}"),
        (None, _) => costs.source(),
    };
    let backtest_config = assembly.into_config();
    // 摘要要回答"这一轮跑的是哪套窗口"，而那四项只有内置链参与（V12 R4-j）。跨语言策略链的
    // 口径由策略自己声明，内置窗口根本没上场，所以它留 None——摘要里不落这个键。
    let mut builtin_signal = None;
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
        let signal_provenance =
            BuiltinSignalProvenance::render(builtin_config.kind, &config.strategy);
        // 口径要在 `builtin_config` 被策略吃掉之前取走，摘要才有这句话可写（V12 R4-j）。
        builtin_signal = Some(builtin_signal_params(&builtin_config, &signal_provenance));
        println!(
            "[Strategy · Signal] {}",
            builtin_signal_note(&builtin_config, &signal_provenance)
        );
        let context = NativeStrategyContext {
            strategy_id: builtin_config.strategy_id.clone(),
            strategy_version: builtin_config.strategy_version.clone(),
            account_id: account_id.clone(),
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
    // Bundle 场景仍然优先：那是跨多个组件的合成指纹。没有 Bundle 时，这一格必须是**被数据集
    // 注册表复核过**的那份输入指纹，而不是 `report.input_data_hash`——后者是引擎对自己手里那段
    // 切片的自哈希，既不含 instrument 也不含来源，换掉输入文件它照算不误（V11 Q66 / Q1b）。
    let data_fingerprint = dataset_binding
        .as_ref()
        .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
        .unwrap_or_else(|| format!("{}:{}", input.kind, input.fingerprint));
    let run_manifest = report.run_manifest_with_input_components(
        RunManifestIdentity {
            run_id: &format!("strategy-backtest:{strategy_id}:{}", frame.instrument),
            code_commit: env!("QX_GIT_COMMIT"),
            config_hash: &config.fingerprint()?,
            strategy_version: &config.strategy.version,
            instrument_spec_version,
            runtime_version: &format!("runtime-schema-{}", config.schema_version),
        },
        &data_fingerprint,
        dataset_binding
            .map(|binding| binding.component_fingerprints.clone())
            .unwrap_or_default(),
    )?;
    let run_manifest_path = persist_backtest_run_manifest(run_manifest_root, &run_manifest)?;
    let bar_ts: Vec<u64> = bars.iter().map(|bar| bar.ts).collect();
    let rejections = rejection_facts(&report.event_log);
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
            event_log: &report.event_log,
            ledger: &report.ledger,
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
            account_base,
            matching_kernel: BAR_MATCHING_KERNEL,
            rejections: &rejections,
            input,
            signal: builtin_signal,
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

    let MarketSpecLoad {
        spec: instrument_spec,
        margin,
        source: instrument_spec_version,
    } = market_spec_with_margin(&frame.instrument, spec_path, "内置策略")?;
    let risk_binding = backtest_risk_binding(runtime_config_path, false)?;
    let costs = execution_cost_binding(runtime_config_path)?;
    // 撮合口径与风控、成本同一来源：给了 `--config` 就必须认它声明的 `strategy.fill_model`，
    // 否则同一份配置在 `strategy backtest` 与 `backtest builtin` 上会得到两种成交价（V11 §15.4）。
    let fill = bar_fill_model(
        configured_fill_model(runtime_config_path)?.as_deref(),
        instrument_spec.as_ref(),
    )?;
    let (fill_model_name, fill_model_source) = (fill.name, fill.source);
    // 本金与风控、成本、撮合同源：这条链读得到 `--config`，却不读它声明的本金，等于让同一份
    // 配置在两个入口压在不同的账户尺度上（V11 Q72）。
    let account_base = configured_account_base(runtime_config_path)?;
    // A 股规则与费率同一条链同源：本链读 `strategy backtest` 读不到的那段，就会让同一份
    // 配置在两个入口得到两种成交与两种费用（V11 Q61）。
    let ashare = configured_ashare_binding(runtime_config_path, &frame.instrument.to_string())?;
    let mut assembly = BarBacktestAssembly::new(
        &frame.instrument,
        "main",
        account_base.cash,
        20260914,
        &costs,
        fill,
    );
    assembly.instrument_spec = instrument_spec;
    assembly.margin = margin;
    assembly.risk = risk_binding.gate();
    let mut ashare_note: Option<String> = None;
    let cost_source = match ashare {
        Some(binding) => {
            let rules = binding.rules;
            let rules_path = binding.rules_path;
            ashare_note = Some(format!(
                "t_plus_one={} lot_size={} price_tick={} commission_bp={} stamp_duty_bp={} transfer_fee_bp={} path={rules_path}",
                rules.t_plus_one,
                rules.lot_size,
                rules.price_tick,
                rules.commission_bp,
                rules.stamp_duty_bp,
                rules.transfer_fee_bp,
            ));
            assembly.virtual_trading.ashare_rules = Some(rules);
            assembly.fee = binding.fee;
            match costs.loaded_from.as_deref() {
                Some(path) => {
                    format!(
                        "ashare-rules:{rules_path}+cost-rules-file:{}",
                        path.display()
                    )
                }
                None => format!("ashare-rules:{rules_path}"),
            }
        }
        None => costs.source(),
    };
    let backtest_config = assembly.into_config();
    let context = NativeStrategyContext {
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "main".into(),
        venue_id: frame.instrument.venue.to_string(),
        data_fingerprint: format!("barframe:{:?}", frame.source),
        as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        // 现金腿落在引擎实际记账的那本账簿上，币种和金额都从装配回读。
        cash: BTreeMap::from([(
            backtest_config.currency.clone(),
            backtest_config.initial_cash.raw(),
        )]),
        available_margin_raw: Some(backtest_config.initial_cash.raw()),
        risk_state: "ready".into(),
    };
    let mut strategy_config = BuiltinStrategyConfig::new(
        kind,
        format!("builtin-{}", kind.name()),
        frame.instrument.clone(),
        Quantity::from_i64(quantity),
    )?;
    let signal_provenance =
        apply_configured_builtin_signal(&mut strategy_config, runtime_config_path)?;
    let signal_note = format!(
        "{} quantity={}",
        builtin_signal_note(&strategy_config, &signal_provenance),
        quantity
    );
    let report = run_builtin_strategy_on_bars(backtest_config, strategy_config, context, &bars)?;
    let rejections = rejection_facts(&report.event_log);
    // 本金决定收益率的分母，也决定风控门看到的可用现金。这条链不落摘要，所以它必须印出来：
    // 零成交与"这策略不赚不赔"在两行数字里长得一样，而差别全在那个数没被人看见的账户尺度上。
    println!(
        "[Builtin · Account] {}",
        backtest_account_base_note(account_base)
    );
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
        "[Builtin · Integrity] rejected_orders={} rejection_reasons={}",
        rejection_count(&rejections),
        rejection_facts_line(&rejections)
    );
    println!(
        "[Builtin · Risk] rule_set_version={} source={} kernel={}",
        risk_binding.gate().rule_set().version(),
        risk_binding.source(),
        BAR_MATCHING_KERNEL
    );
    // 本入口不写摘要文件，成本口径只能靠这行交代来源；不印出来就等于"用了什么费率无人知晓"。
    // A 股规则快照会把成本文件里那对 maker/taker 整个顶掉，所以费用字段只能按真正生效的
    // 那一套印：否则读者拿 2/5bp 去核对本轮成交，永远核不上。
    match &ashare_note {
        None => println!(
            "[Builtin · Cost] source={} maker_bp={} taker_bp={} latency_base_ns={} latency_insert_ns={}",
            cost_source,
            costs.rules.maker_bp,
            costs.rules.taker_bp,
            costs.rules.latency_base_ns,
            costs.rules.latency_insert_ns,
        ),
        Some(note) => {
            println!(
                "[Builtin · Cost] source={cost_source} fee_model=ashare-rules latency_base_ns={} latency_insert_ns={}",
                costs.rules.latency_base_ns, costs.rules.latency_insert_ns,
            );
            println!("[Builtin · A 股规则] {note}");
        }
    }
    // 同上：撮合模型换了成交价，成交额与费用都跟着换，而这条链不落摘要——只能印出来。
    // 规格来源也一起印：本入口没有 `RunManifest` 可写，`instrument_spec_version` 那行
    // 在这条链上根本不存在，不印就等于"这份 spec 按哪种形状读的无人知晓"（V11 Q63）。
    println!(
        "[Builtin · Execution] fill_model={} source={} spec_source={}",
        fill_model_name, fill_model_source, instrument_spec_version
    );
    // 信号参数决定"有没有单"，与费用一样是这条不落摘要的链上必须交代的口径。
    println!("[Builtin · Signal] {}", signal_note);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_ccxt_builtin_backtest(
    ccxt_config_path: &Path,
    strategy_name: &str,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    spec_path: Option<&Path>,
    quantity: i64,
    runtime_config_path: Option<&Path>,
) -> Result<(), String> {
    if end_ms < start_ms {
        return Err("CCXT 内置策略回测 end_ms 不能早于 start_ms".into());
    }
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)
        .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    let temp_path = std::env::temp_dir().join(format!(
        "qianxing-ccxt-builtin-{}-{}.json",
        std::process::id(),
        runtime_timestamp_ms()
    ));
    std::fs::write(
        &temp_path,
        serde_json::to_string(frame)
            .map_err(|error| format!("编码 CCXT BarFrame 失败: {error}"))?,
    )
    .map_err(|error| format!("写入临时 CCXT BarFrame 失败: {error}"))?;
    let result = run_builtin_backtest(
        strategy_name,
        &temp_path,
        spec_path,
        quantity,
        runtime_config_path,
    );
    let _ = std::fs::remove_file(&temp_path);
    result
}
