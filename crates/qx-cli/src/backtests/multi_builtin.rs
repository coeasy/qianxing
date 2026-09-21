//! 多内置策略与 CCXT 内置回测链：`run_multi_builtin_backtest` / `run_ccxt_builtin_backtest`。

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_multi_builtin_backtest(
    strategy_name: &str,
    primary_path: &Path,
    reference_path: &Path,
    primary_spec_path: Option<&Path>,
    reference_spec_path: Option<&Path>,
    quantity: i64,
    funding_bps: i64,
    artifact_root: Option<&Path>,
    runtime_config_path: Option<&Path>,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("多腿内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if !MULTI_LEG_KINDS.contains(&kind) {
        return Err(format!("多腿回测只接受套利策略，当前为 {}", kind.name()));
    }
    let primary_frame = read_bar_frame_for_multi_backtest(primary_path, "主腿")?;
    let reference_frame = read_bar_frame_for_multi_backtest(reference_path, "对冲腿")?;
    if primary_frame.ts != reference_frame.ts {
        return Err("多腿回测要求两条 BarFrame 的时间戳完全对齐".into());
    }
    let primary_bars: Vec<Bar> = (&primary_frame).into();
    let reference_bars: Vec<Bar> = (&reference_frame).into();
    if primary_bars.len() < 3 {
        return Err("多腿内置策略回测至少需要三根对齐 Bar".into());
    }
    // 每腿账户必须付得起它宣称的下单量：Bar 内核现在拒绝现金不足的现货买入
    // （`qx-xingban/src/backtest.rs` 的 cash-funded spot buy 检查）。沿用装配默认的
    // 100_000 USDT，示例里 2-BTC 腿的第三笔买入（名义 ≈122_400）就是废单，
    // 归因会少一条腿；因此按"全帧最高价 × quantity × 2"给足头寸余量，
    // 且不低于装配默认值。
    let max_mark_raw = primary_bars
        .iter()
        .chain(&reference_bars)
        .map(|bar| bar.high)
        .max()
        .unwrap_or(0);
    let leg_initial_cash = Money::from_i64(
        100_000_i128
            .max(
                i128::from(quantity)
                    .saturating_mul(max_mark_raw / qx_core::SCALE)
                    .saturating_mul(2),
            )
            .min(i64::MAX as i128) as i64,
    );
    let strategy_config = BuiltinStrategyConfig {
        kind,
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        instrument: primary_frame.instrument.clone(),
        quantity: Quantity::from_i64(quantity),
        fast_window: 5,
        slow_window: 20,
        period: 14,
        threshold_bps: 100,
        reference_instrument: Some(reference_frame.instrument.clone()),
        primary_policy: None,
        reference_policy: None,
    };
    let mut native = BuiltinStrategy::new(strategy_config)?;
    let mut context = NativeStrategyContext {
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "multi-leg-backtest".into(),
        venue_id: primary_frame.instrument.venue.to_string(),
        data_fingerprint: format!("{:?}:{:?}", primary_frame.source, reference_frame.source),
        as_of: primary_bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::from([
            (primary_frame.instrument.to_string(), 0),
            (reference_frame.instrument.to_string(), 0),
        ]),
        cash: BTreeMap::from([("USDT".into(), leg_initial_cash.raw())]),
        available_margin_raw: Some(leg_initial_cash.raw()),
        risk_state: "multi-leg-backtest".into(),
    };
    native
        .on_init(&context)
        .map_err(|error| format!("初始化多腿内置策略失败: {error}"))?;
    let mut primary_targets = BTreeMap::new();
    let mut reference_targets = BTreeMap::new();
    for (primary_bar, reference_bar) in primary_bars.iter().zip(&reference_bars) {
        context.as_of = primary_bar.ts;
        let reference_event = NativeMarketEvent::Bar {
            instrument: reference_frame.instrument.clone(),
            ts: reference_bar.ts,
            open_raw: reference_bar.open,
            high_raw: reference_bar.high,
            low_raw: reference_bar.low,
            close_raw: reference_bar.close,
            volume_raw: reference_bar.volume,
        };
        native
            .on_event(&context, &reference_event)
            .map_err(|error| format!("处理多腿对冲 Bar 失败: {error}"))?;
        let primary_event = NativeMarketEvent::Bar {
            instrument: primary_frame.instrument.clone(),
            ts: primary_bar.ts,
            open_raw: primary_bar.open,
            high_raw: primary_bar.high,
            low_raw: primary_bar.low,
            close_raw: primary_bar.close,
            volume_raw: primary_bar.volume,
        };
        let decision = native
            .on_event(&context, &primary_event)
            .map_err(|error| format!("处理多腿主 Bar 失败: {error}"))?;
        for intent in decision.intents {
            let instrument_key = intent.instrument.to_string();
            let current = context.positions.get(&instrument_key).copied().unwrap_or(0);
            let delta = if intent.side == Side::Buy {
                intent.qty.raw()
            } else {
                -intent.qty.raw()
            };
            let target = current.checked_add(delta).ok_or("多腿回测目标仓位溢出")?;
            context.positions.insert(instrument_key.clone(), target);
            if intent.instrument == primary_frame.instrument {
                primary_targets.insert(primary_bar.ts, target);
            } else if intent.instrument == reference_frame.instrument {
                reference_targets.insert(reference_bar.ts, target);
            }
        }
    }
    let (primary_spec, primary_margin) =
        market_spec_with_margin(&primary_frame.instrument, primary_spec_path, "多腿")?;
    let (reference_spec, reference_margin) =
        market_spec_with_margin(&reference_frame.instrument, reference_spec_path, "多腿")?;
    if primary_spec
        .as_ref()
        .is_some_and(|spec| spec.product.is_derivative())
        && primary_spec_path.is_none()
    {
        return Err("主腿衍生品多腿回测必须提供 market spec".into());
    }
    let attribution_primary_spec = primary_spec.clone();
    let attribution_reference_spec = reference_spec.clone();
    let attribution_primary_targets = primary_targets.clone();
    let attribution_reference_targets = reference_targets.clone();
    // 两条腿共用同一份规则绑定：多腿链的规则集版本必须与单标的链可比较。
    let risk_binding = backtest_risk_binding(runtime_config_path, true)?;
    let risk_rule_set_version = risk_binding.gate().rule_set().version().to_string();
    // 两条腿同样共用一份成本绑定：多腿归因的费用必须是同一口径，否则净成本差里没有可比性。
    let costs = execution_cost_binding(runtime_config_path)?;
    let cost_source = costs.source();
    // 产物里写的版本必须是真正装进引擎的那一份：把每条腿实际生效的版本读回来核对。
    let mut leg_risk_versions: Vec<String> = Vec::new();
    let mut run_leg = |frame: &BarFrame,
                       bars: &[Bar],
                       spec: Option<TradingInstrumentSpec>,
                       margin: Box<dyn MarginRule>,
                       targets: BTreeMap<u64, i128>,
                       leg: &str|
     -> Result<qx_xingban::BacktestReport, String> {
        let currency = spec
            .as_ref()
            .map(|value| value.settlement_currency.clone())
            .unwrap_or_else(|| "USDT".into());
        let mut strategy = ScheduledTargetStrategy {
            instrument: frame.instrument.clone(),
            targets,
            policy: None,
            account_id: format!("multi-leg-{leg}"),
        };
        let mut assembly =
            BarBacktestAssembly::new(&frame.instrument, strategy.account_id.clone(), 20260914, &costs);
        assembly.instrument_spec = spec;
        assembly.margin = margin;
        assembly.currency = currency;
        assembly.initial_cash = leg_initial_cash;
        assembly.risk = risk_binding.gate();
        leg_risk_versions.push(assembly.risk.rule_set().version().to_string());
        BacktestEngine::new(assembly.into_config())
            .run(bars, &mut strategy)
            .map_err(|error| format!("{leg} 多腿回测失败: {error:?}"))
    };
    let primary_report = run_leg(
        &primary_frame,
        &primary_bars,
        primary_spec,
        primary_margin,
        primary_targets,
        "primary",
    )?;
    let reference_report = run_leg(
        &reference_frame,
        &reference_bars,
        reference_spec,
        reference_margin,
        reference_targets,
        "reference",
    )?;
    if leg_risk_versions
        .iter()
        .any(|version| version != &risk_rule_set_version)
    {
        return Err(format!(
            "多腿回测腿级生效风控规则集与产物声明不一致: legs={leg_risk_versions:?} declared={risk_rule_set_version}"
        ));
    }
    println!(
        "[Multi-leg · Backtest] strategy={} primary={} fills={} return_bps={} reference={} fills={} return_bps={} combined_return_bps={} result_hashes={:016x}/{:016x}",
        kind.name(),
        primary_frame.instrument,
        primary_report.fills.len(),
        primary_report.return_bps,
        reference_frame.instrument,
        reference_report.fills.len(),
        reference_report.return_bps,
        (i64::from(primary_report.return_bps) + i64::from(reference_report.return_bps)) / 2,
        primary_report.result_hash(),
        reference_report.result_hash()
    );
    let strategy_id = format!("builtin-{}-multi", kind.name());
    let primary_leg = MultiLegLegFacts {
        label: "primary",
        instrument: &primary_frame.instrument,
        targets: &attribution_primary_targets,
        bars: &primary_bars,
        spec: attribution_primary_spec.as_ref(),
        report: &primary_report,
    };
    let reference_leg = MultiLegLegFacts {
        label: "reference",
        instrument: &reference_frame.instrument,
        targets: &attribution_reference_targets,
        bars: &reference_bars,
        spec: attribution_reference_spec.as_ref(),
        report: &reference_report,
    };
    let primary_buckets = multi_leg_leg_buckets(&primary_leg, funding_bps)?;
    let reference_buckets = multi_leg_leg_buckets(&reference_leg, funding_bps)?;
    let (groups, residual_fees_raw, residual_filled_qty_raw) = multi_leg_group_attributions(
        &strategy_id,
        &primary_leg,
        &reference_leg,
        &primary_buckets,
        &reference_buckets,
    )?;
    let totals = groups.iter().fold(
        (0_i128, 0_i128, 0_i128, 0_i128, 0_i128),
        |mut acc, group| {
            acc.0 += group.total_fees_raw;
            acc.1 += group.total_turnover_raw;
            acc.2 += group.total_funding_raw;
            acc.3 += group.total_filled_qty_raw;
            acc.4 = acc.4.max(group.total_margin_raw);
            acc
        },
    );
    // 组级费用合计必须与两条腿的独立合计闭合，否则 FIFO 分配漏计或重复计入。
    let expected_fees_raw = primary_report.fees_raw + reference_report.fees_raw - residual_fees_raw;
    if totals.0 != expected_fees_raw {
        return Err(format!(
            "多腿归因费用不闭合: groups={} expected={}",
            totals.0, expected_fees_raw
        ));
    }
    let net_cost_raw = totals.0 + totals.2;
    let cost_bps = if totals.1 > 0 {
        i64::try_from(net_cost_raw.saturating_mul(10_000) / totals.1).unwrap_or(i64::MAX)
    } else {
        0
    };
    println!(
        "[Multi-leg · Attribution] strategy={} groups={} turnover_raw={} fees_raw={} funding_raw={} filled_qty_raw={} margin_peak_raw={} net_cost_raw={} cost_bps={} residual_filled_qty_raw={} residual_fees_raw={} funding_bps={} margin_model=realized-initial-margin-leverage-1 funding_model={}",
        strategy_id,
        groups.len(),
        totals.1,
        totals.0,
        totals.2,
        totals.3,
        totals.4,
        net_cost_raw,
        cost_bps,
        residual_filled_qty_raw,
        residual_fees_raw,
        funding_bps,
        if funding_bps == 0 {
            "disabled"
        } else {
            "8h-pro-rata-by-holding-time"
        }
    );
    println!(
        "[Multi-leg · Risk] rule_set_version={} source={} kernel={}",
        risk_rule_set_version,
        risk_binding.source(),
        BAR_MATCHING_KERNEL
    );
    if let Some(root) = artifact_root {
        let runs = root.join("runs");
        std::fs::create_dir_all(&runs)
            .map_err(|error| format!("创建多腿归因产物目录失败: {error}"))?;
        let payload = serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "strategy_id": strategy_id,
            "primary_instrument": primary_frame.instrument.to_string(),
            "reference_instrument": reference_frame.instrument.to_string(),
            "funding_bps": funding_bps,
            "risk_rules": {
                "rule_set_version": risk_rule_set_version,
                "source": risk_binding.source(),
                "matching_kernel": BAR_MATCHING_KERNEL,
            },
            "execution_costs": { "source": cost_source },
            "totals": {
                "turnover_raw": totals.1.to_string(),
                "fees_raw": totals.0.to_string(),
                "funding_raw": totals.2.to_string(),
                "filled_qty_raw": totals.3.to_string(),
                "margin_peak_raw": totals.4.to_string(),
                "net_cost_raw": net_cost_raw.to_string(),
                "cost_bps": cost_bps,
                "residual_filled_qty_raw": residual_filled_qty_raw.to_string(),
                "residual_fees_raw": residual_fees_raw.to_string(),
            },
            "groups": groups,
            "assumptions": [
                "fees=per-leg FIFO allocated to signal ts buckets",
                "margin=spec.initial_margin(|realized fills standing|, bar close, leverage=1)",
                "funding=realized-fill notional*funding_bps*holding_ms/(8h*10000), long pays positive",
                "cost_bps=net_cost*10000/turnover",
            ],
        }))
        .map_err(|error| format!("编码多腿归因产物失败: {error}"))?;
        let fingerprint = primary_report
            .result_hash()
            .wrapping_mul(31)
            .wrapping_add(reference_report.result_hash());
        let path = runs.join(format!(
            "multi-leg-{}-{:016x}.spread-attribution.json",
            kind.name(),
            fingerprint
        ));
        write_backtest_artifact(&path, &payload, "多腿归因产物")?;
        println!("[Artifacts] spread_attribution={}", path.display());
    }
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
