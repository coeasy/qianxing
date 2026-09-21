//! 深度盘口回测：`run_depth_backtest` 与其 `depth_run_config_hash`。

use super::*;
use qx_xingban::OrderBookExecutionModel;

/// `backtest book` 的两个高保真撮合参数（V11 Q1a）。全 0 就是原有的"逐档吃单"行为；
/// 只有使用者显式点名时才改变撮合，且两项都会写进产物的 `model_descriptors`，
/// 让"能配"与"配了什么"在产物里同口径可见。
///
/// 内核的第三项 `queue_position_bps` 故意不接：它只作用于限价单所在档位，而本入口
/// 只跑内置策略、内置策略只发市价单，非零值换不动任何一笔成交，属于 Q0b 判掉的假旗标。
#[derive(Clone, Copy, Default)]
pub(crate) struct DepthExecutionModel {
    pub(crate) latency_snapshots: u64,
    pub(crate) market_impact_bps: i64,
}

impl DepthExecutionModel {
    /// 转成内核口径；`fee_bps` 由成本绑定的三层优先级决定，不在这里重复。
    /// 队列前置固定为 0，与 `OrderBookExecutionModel::new` 的默认口径同源。
    fn to_kernel(self, fee_bps: i128) -> Result<OrderBookExecutionModel, String> {
        let mut model = OrderBookExecutionModel::new(fee_bps)?;
        model.latency_snapshots = self.latency_snapshots;
        model.market_impact_bps = i128::from(self.market_impact_bps);
        Ok(model)
    }
}

/// L1/L2 深度档位回测：把 `DepthFrame` 喂进逐档撮合内核，输出与 Bar 回测同构的
/// RunManifest、摘要、权益曲线与成交明细。策略侧沿用 Bar 协议（中间价折叠视图）。
///
/// `fee_bps` 为 `None` 时吃单费率取自成本规则（`strategy.cost_rules_path`），最终缺省仍是
/// 内核常数；深度内核只吃单一吃单费率，因此成本规则里的延迟与 maker 费率在本链路无处落地，
/// 非零延迟会直接报错而不是被静默忽略（V11 Q0c）。
#[allow(clippy::too_many_arguments)] // V10 P0a/P1b：风控与组存储形参由编译器逐个点名，不合并成参数包。
pub(crate) fn run_depth_backtest(
    tier: &str,
    strategy_name: &str,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
    fee_bps: Option<i64>,
    execution: DepthExecutionModel,
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
    if fee_bps.is_some_and(|bps| !(0..=10_000).contains(&bps)) {
        return Err("深度回测 --fee-bps 必须在 0..=10000 内".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if MULTI_LEG_KINDS.contains(&kind) {
        return Err("深度档位回测只支持单标的策略，多腿配对请使用 multi-builtin".into());
    }
    let payload = std::fs::read_to_string(frame_path)
        .map_err(|error| format!("读取深度数据帧失败 {}: {error}", frame_path.display()))?;
    let frame = DepthFrame::from_json(&payload)
        .map_err(|error| format!("{}: {error}", frame_path.display()))?;
    let (instrument_spec, _) = market_spec_with_margin(&frame.instrument, spec_path, "深度回测")?;
    let currency = backtest_settlement_currency(instrument_spec.as_ref());
    let data_fingerprint = format!("depth:{}:{:016x}", frame.source, frame.input_hash());
    let context = NativeStrategyContext {
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "main".into(),
        venue_id: frame.instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: frame.snapshots.first().map(|item| item.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(currency.clone(), Money::from_i64(100_000).raw())]),
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
    let costs = execution_cost_binding(runtime_config_path)?;
    // 深度内核只有单一吃单费率：maker 与延迟都没有落点。延迟非零却照样跑完，
    // 就是把"能配"当成"生效"——正是 Q0b 删 `--config` 时判掉的形状，所以直接拒绝。
    if costs.rules.latency_base_ns != 0 || costs.rules.latency_insert_ns != 0 {
        return Err(format!(
            "深度档回测不接受成本规则里的延迟设置（{}），它的延迟口径是 --latency-snapshots（按快照个数）；请把 latency_base_ns/latency_insert_ns 置 0，或用 --latency-snapshots 表达深度延迟",
            costs.source()
        ));
    }
    // 三层优先级：显式 --fee-bps > 成本规则文件的 taker_bp > 内核默认。
    let cost_source = if fee_bps.is_some() {
        "cli-flag".to_string()
    } else {
        costs.source()
    };
    let fee_bps = fee_bps.unwrap_or(costs.rules.taker_bp) as i128;
    // 深度链走 Tick / OrderBook 两套撮合，产物里必须写明实际内核，不得冒充 Bar 内核。
    let matching_kernel = if single_level {
        TICK_MATCHING_KERNEL
    } else {
        ORDER_BOOK_MATCHING_KERNEL
    };
    // 两项撮合参数与最终吃单费率一起交给内核执行模型；全 0 时与历史行为逐位一致，
    // 但描述子仍会把它们写出来，产物因此能区分"没配"和"配了 0"之外的真实口径。
    let execution_model = execution.to_kernel(fee_bps)?;
    let report = if single_level {
        let ticks = depth_frame_to_ticks(&frame)?;
        TickBacktestEngine::new(TickBacktestConfig {
            instrument: frame.instrument.clone(),
            account_id: "main".into(),
            currency,
            initial_cash: Money::from_i64(100_000),
            fee_bps,
            instrument_spec,
            risk: risk_gate,
        })
        .with_execution_model(execution_model)
        .run(&ticks, &mut strategy)
        .map_err(|error| format!("L1 深度回测失败: {error:?}"))?
    } else {
        OrderBookBacktestEngine::new(OrderBookBacktestConfig {
            instrument: frame.instrument.clone(),
            account_id: "main".into(),
            currency,
            initial_cash: Money::from_i64(100_000),
            fee_bps,
            instrument_spec,
            risk: risk_gate,
            data_tier: DataTier::L2L3,
        })
        .with_execution_model(execution_model)
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
                execution,
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
    let rejections = rejection_facts(&report.event_log);
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
            cost_source: &cost_source,
            matching_kernel,
            rejections: &rejections,
        },
    )?;
    println!(
        "[Depth · Backtest] tier={tier} strategy={} instrument={} snapshots={} fills={} fee_bps={} cost_source={cost_source} return_bps={} max_drawdown_bps={} result_hash={:016x}",
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
        "[Depth · Execution] latency_snapshots={} market_impact_bps={}",
        execution.latency_snapshots, execution.market_impact_bps
    );
    println!(
        "[Depth · Integrity] rejected_orders={} rejection_reasons={}",
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

/// 深度回测的撮合参数全部来自命令行，因此用显式参数 + 生效风控规则集版本 + 输入指纹合成配置摘要。
pub(crate) fn depth_run_config_hash(
    tier: &str,
    strategy_name: &str,
    quantity: i64,
    fee_bps: i128,
    execution: DepthExecutionModel,
    risk_rule_set_version: &str,
    frame: &DepthFrame,
) -> String {
    let mut hash = qx_core::Fnv1a::new();
    hash.write_text("depth-backtest-v1");
    hash.write_text(tier);
    hash.write_text(strategy_name);
    hash.write_i128(quantity as i128);
    hash.write_i128(fee_bps);
    // 两项撮合参数改变结果，就必须改变配置指纹；漏掉它们会让两次不同口径的回测
    // 声称同一份配置，产物落盘时还会互相判成"同一回测已有不同内容"。
    hash.write_u64(execution.latency_snapshots);
    hash.write_i128(i128::from(execution.market_impact_bps));
    hash.write_text(risk_rule_set_version);
    hash.write_u64(frame.input_hash());
    format!("{:016x}", hash.finish())
}
