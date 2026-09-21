//! 深度盘口回测：`run_depth_backtest` 与其 `depth_run_config_hash`。

use super::*;

/// L1/L2 深度档位回测：把 `DepthFrame` 喂进逐档撮合内核，输出与 Bar 回测同构的
/// RunManifest、摘要、权益曲线与成交明细。策略侧沿用 Bar 协议（中间价折叠视图）。
#[allow(clippy::too_many_arguments)] // V10 P0a/P1b：风控与组存储形参由编译器逐个点名，不合并成参数包。
pub(crate) fn run_depth_backtest(
    tier: &str,
    strategy_name: &str,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
    fee_bps: i128,
    root: &Path,
    runtime_config_path: Option<&Path>,
) -> Result<(), String> {
    let single_level = match tier {
        "l1" => true,
        "l2" => false,
        other => return Err(format!("--fill-tier 仅支持 l1 或 l2，实际 {other}")),
    };
    if quantity <= 0 {
        return Err("深度回测 quantity 必须为正整数".into());
    }
    if !(0..=10_000).contains(&fee_bps) {
        return Err("深度回测 --fee-bps 必须在 0..=10000 内".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if matches!(
        kind,
        BuiltinStrategyKind::PairsArbitrage
            | BuiltinStrategyKind::BasisArbitrage
            | BuiltinStrategyKind::CrossVenueArbitrage
            | BuiltinStrategyKind::SpotFuturesArbitrage
    ) {
        return Err("深度档位回测只支持单标的策略，多腿配对请使用 multi-builtin".into());
    }
    let payload = std::fs::read_to_string(frame_path)
        .map_err(|error| format!("读取深度数据帧失败 {}: {error}", frame_path.display()))?;
    let frame = DepthFrame::from_json(&payload)
        .map_err(|error| format!("{}: {error}", frame_path.display()))?;
    let (instrument_spec, _) = market_spec_with_margin(&frame.instrument, spec_path, "深度回测")?;
    let data_fingerprint = format!("depth:{}:{:016x}", frame.source, frame.input_hash());
    let context = NativeStrategyContext {
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "main".into(),
        venue_id: frame.instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: frame.snapshots.first().map(|item| item.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "ready".into(),
    };
    let strategy = BuiltinStrategy::new(BuiltinStrategyConfig::new(
        kind,
        format!("builtin-{}", kind.name()),
        frame.instrument.clone(),
        Quantity::from_i64(quantity),
    )?)?;
    let mut strategy = DepthBarStrategy::new(strategy, context, frame.instrument.clone());
    strategy
        .initialize()
        .map_err(|error| format!("初始化深度策略失败: {error:?}"))?;
    let risk_binding = backtest_risk_binding(runtime_config_path, false)?;
    let risk_gate = risk_binding.gate();
    let risk_rule_set_version = risk_gate.rule_set().version().to_string();
    // 深度链走 Tick / OrderBook 两套撮合，产物里必须写明实际内核，不得冒充 Bar 内核。
    let matching_kernel = if single_level {
        TICK_MATCHING_KERNEL
    } else {
        ORDER_BOOK_MATCHING_KERNEL
    };
    let report = if single_level {
        let ticks = depth_frame_to_ticks(&frame)?;
        TickBacktestEngine::new(TickBacktestConfig {
            instrument: frame.instrument.clone(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(100_000),
            fee_bps,
            instrument_spec,
            risk: risk_gate,
        })
        .run(&ticks, &mut strategy)
        .map_err(|error| format!("L1 深度回测失败: {error:?}"))?
    } else {
        OrderBookBacktestEngine::new(OrderBookBacktestConfig {
            instrument: frame.instrument.clone(),
            account_id: "main".into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(100_000),
            fee_bps,
            instrument_spec,
            risk: risk_gate,
            data_tier: DataTier::L2L3,
        })
        .run(&frame.snapshots, &mut strategy)
        .map_err(|error| format!("L2 深度回测失败: {error:?}"))?
    };
    let run_manifest = report.run_manifest(
        RunManifestIdentity {
            run_id: &format!("depth-backtest:{tier}:{}:{}", kind.name(), frame.instrument),
            code_commit: env!("QX_GIT_COMMIT"),
            config_hash: &depth_run_config_hash(
                tier,
                kind.name(),
                quantity,
                fee_bps,
                &risk_rule_set_version,
                &frame,
            ),
            strategy_version: &format!("builtin-{}-v1", kind.name()),
            instrument_spec_version: if spec_path.is_some() {
                "ccxt-market-spec-v1"
            } else {
                "default-instrument-spec-v1"
            },
            runtime_version: "depth-backtest-v1",
        },
        &data_fingerprint,
    )?;
    let run_manifest_path = persist_backtest_run_manifest(root, &run_manifest)?;
    let (summary_path, equity_path, fills_path) = persist_backtest_artifacts(
        &run_manifest_path,
        &BacktestArtifacts {
            strategy_id: &format!("builtin-{}", kind.name()),
            instrument: &frame.instrument,
            sample_unit: "depth_snapshot",
            samples: frame.snapshots.len(),
            sample_ts: &report.snapshot_ts,
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
            risk_rule_source: risk_binding.source(),
            matching_kernel,
        },
    )?;
    println!(
        "[Depth · Backtest] tier={tier} strategy={} instrument={} snapshots={} fills={} fee_bps={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        kind.name(),
        frame.instrument,
        frame.snapshots.len(),
        report.fills.len(),
        fee_bps,
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

/// 深度回测的撮合参数全部来自命令行，因此用显式参数 + 生效风控规则集版本 + 输入指纹合成配置摘要。
pub(crate) fn depth_run_config_hash(
    tier: &str,
    strategy_name: &str,
    quantity: i64,
    fee_bps: i128,
    risk_rule_set_version: &str,
    frame: &DepthFrame,
) -> String {
    let mut hash = qx_core::Fnv1a::new();
    hash.write_text("depth-backtest-v1");
    hash.write_text(tier);
    hash.write_text(strategy_name);
    hash.write_i128(quantity as i128);
    hash.write_i128(fee_bps);
    hash.write_text(risk_rule_set_version);
    hash.write_u64(frame.input_hash());
    format!("{:016x}", hash.finish())
}
