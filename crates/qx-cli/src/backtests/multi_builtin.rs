//! 多内置策略（双腿套利）回测链：`run_multi_builtin_backtest`。
//!
//! CCXT 形状的内置回测入口不在这里：它取完 OHLCV 后交给 `single_strategy.rs` 的
//! `run_builtin_backtest`，与单标的链共用同一份装配，所以定义点也放在一起。

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
    // 成本绑定在定资之前解析：账户要留的手续费余量必须来自真正生效的那份费率，
    // 而不是事后再补一个估计口径。
    let costs = execution_cost_binding(runtime_config_path)?;
    // 撮合口径与成本同源：两条腿共用运行时配置里声明的那一份（V11 Q1a 第二批）。
    // 具体模型要到 `run_leg` 里才解析得出来——`one_tick_slippage` 的一档取自各腿自己的 spec。
    let fill_configured = configured_fill_model(runtime_config_path)?;
    // 每腿账户必须付得起它宣称的下单量：Bar 内核会拒绝现金不足的现货买入
    // （`qx-xingban/src/backtest.rs` 的 cash-funded spot buy 检查）。定资口径见
    // `multi_leg_leg_cash`：用**本腿自己的**最高价（全局最大值会让便宜腿背下贵腿的名义额）、
    // 手续费余量按**生效成本绑定**算，且算不出来直接报错而不是截断到 i64::MAX。
    let primary_cash = multi_leg_leg_cash(quantity, &primary_bars, costs.rules.taker_bp, "主")?;
    let reference_cash =
        multi_leg_leg_cash(quantity, &reference_bars, costs.rules.taker_bp, "对冲")?;
    let mut strategy_config = BuiltinStrategyConfig {
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
    // 与单标的链同一条读法：多腿入口也收 `--config`（风控、撮合、成本都读它），
    // 信号参数却不能只在这条链上失效（V11 Q64）。
    let signal_source = apply_configured_builtin_signal(&mut strategy_config, runtime_config_path)?;
    println!(
        "[Multi · Signal] {} quantity={}",
        builtin_signal_note(&strategy_config, signal_source),
        quantity
    );
    let mut native = BuiltinStrategy::new(strategy_config)?;
    // 规格先解析：策略侧现金腿要落在主腿记账的那本账簿上，币种只能从 spec 读回来。
    let MarketSpecLoad {
        spec: primary_spec,
        margin: primary_margin,
        source: _,
    } = market_spec_with_margin(&primary_frame.instrument, primary_spec_path, "多腿")?;
    let MarketSpecLoad {
        spec: reference_spec,
        margin: reference_margin,
        source: _,
    } = market_spec_with_margin(&reference_frame.instrument, reference_spec_path, "多腿")?;
    // 规格闸门先于任何撮合：名义额、保证金与资金费全部取自 spec，缺 spec 的腿按现货乘数 1
    // 记账。声称要计提它们却连产品形态都没给，必须先拒而不是跑完再补一句假设。
    multi_leg_spec_guard(
        [
            ("primary", primary_spec.as_ref()),
            ("reference", reference_spec.as_ref()),
        ],
        funding_bps,
        configured_instrument_product(runtime_config_path)?,
    )?;
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
        cash: BTreeMap::from([(
            backtest_settlement_currency(primary_spec.as_ref()),
            primary_cash.raw(),
        )]),
        available_margin_raw: Some(primary_cash.raw()),
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
    let attribution_primary_spec = primary_spec.clone();
    let attribution_reference_spec = reference_spec.clone();
    let attribution_primary_targets = primary_targets.clone();
    let attribution_reference_targets = reference_targets.clone();
    // 本轮到底有没有会被计提保证金/资金费的腿：`margin_peak_raw=0` 既可能是"两条腿都是
    // 现货"，也可能是"衍生品腿漏了 spec"。只报数字不报口径就等于让读者去猜，所以这一行
    // 与产物里的 `market_specs` 一起把两种情形分开（V11 Q58）。
    let margin_model = if [&attribution_primary_spec, &attribution_reference_spec]
        .into_iter()
        .flatten()
        .any(|spec| spec.product.is_derivative())
    {
        "realized-initial-margin-leverage-1"
    } else {
        "none-no-derivative-leg-spec"
    };
    // 两条腿共用同一份规则绑定：多腿链的规则集版本必须与单标的链可比较。
    let risk_binding = backtest_risk_binding(runtime_config_path, true)?;
    // A 股规则是单标的口径（一天的涨跌停锚、整手、T+1 都按一条 `instrument` 生效），
    // 而本入口的 `--config` 只有一份 strategy 段：收下它再按 primary 的代号去套两条腿，
    // 等于给 reference 那条腿安上别人的交易制度，因此整份拒绝而不是猜一条腿（V11 Q61）。
    reject_ashare_rules_config(
        runtime_config_path,
        "backtest multi-builtin",
        "一份 strategy 段无法同时描述两条腿所属标的的交易制度",
    )?;
    let risk_rule_set_version = risk_binding.gate().rule_set().version().to_string();
    // 两条腿同样共用一份成本绑定：多腿归因的费用必须是同一口径，否则净成本差里没有可比性。
    let cost_source = costs.source();
    // 产物里写的版本必须是真正装进引擎的那一份：把每条腿实际生效的版本读回来核对。
    let mut leg_risk_versions: Vec<String> = Vec::new();
    // 撮合模型同样是腿级事实：两条腿可以因为只有一个 spec 而滑点不同，但"用了哪种模型、
    // 口径从哪来"必须一致，否则跨腿净成本差就混进了两套撮合假设。
    let mut leg_fill_models: Vec<(&'static str, &'static str)> = Vec::new();
    let mut run_leg = |frame: &BarFrame,
                       bars: &[Bar],
                       spec: Option<TradingInstrumentSpec>,
                       margin: Box<dyn MarginRule>,
                       targets: BTreeMap<u64, i128>,
                       cash: Money,
                       leg: &str|
     -> Result<qx_xingban::BacktestReport, String> {
        let mut strategy = ScheduledTargetStrategy {
            instrument: frame.instrument.clone(),
            targets,
            policy: None,
            account_id: format!("multi-leg-{leg}"),
        };
        let fill = bar_fill_model(fill_configured.as_deref(), spec.as_ref())?;
        leg_fill_models.push((fill.name, fill.source));
        let mut assembly = BarBacktestAssembly::new(
            &frame.instrument,
            strategy.account_id.clone(),
            20260914,
            &costs,
            fill,
        );
        assembly.instrument_spec = spec;
        assembly.margin = margin;
        assembly.initial_cash = cash;
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
        primary_cash,
        "primary",
    )?;
    let reference_report = run_leg(
        &reference_frame,
        &reference_bars,
        reference_spec,
        reference_margin,
        reference_targets,
        reference_cash,
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
    // 两条腿的撮合口径必须同源同名，否则归因里的净成本差混进了两套假设；
    // 一档滑点的**大小**可以随各腿自己的 price_tick 变化，那是标的规格而非口径分叉。
    let Some((fill_model_name, fill_model_source)) = leg_fill_models.first().copied() else {
        return Err("多腿回测未记录任何腿级撮合模型".into());
    };
    if leg_fill_models
        .iter()
        .any(|leg| *leg != (fill_model_name, fill_model_source))
    {
        return Err(format!(
            "多腿回测腿级撮合模型口径不一致: legs={leg_fill_models:?} declared={fill_model_name}/{fill_model_source}"
        ));
    }
    // 组合收益按钱算：两条腿本金各自按本腿行情定资，平均腿级 bps 念的不是组合收益率（V11 Q71）。
    let primary_equity = primary_report.final_equity();
    let reference_equity = reference_report.final_equity();
    let combined_return_bps = multi_leg_combined_return_bps([
        ("primary", primary_cash.raw(), primary_equity),
        ("reference", reference_cash.raw(), reference_equity),
    ])?;
    println!(
        "[Multi-leg · Backtest] strategy={} primary={} fills={} return_bps={} reference={} fills={} return_bps={} combined_return_bps={} result_hashes={:016x}/{:016x}",
        kind.name(),
        primary_frame.instrument,
        primary_report.fills.len(),
        primary_report.return_bps,
        reference_frame.instrument,
        reference_report.fills.len(),
        reference_report.return_bps,
        combined_return_bps,
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
    let MultiLegAttribution {
        groups,
        residual_fees_raw,
        residual_filled_qty_raw,
        pending_reconcile,
    } = multi_leg_group_attributions(
        &strategy_id,
        &primary_leg,
        &reference_leg,
        &primary_buckets,
        &reference_buckets,
    )?;
    let totals = multi_leg_group_totals(&groups);
    // 组级费用合计必须与两条腿的独立合计闭合，否则 FIFO 分配漏计或重复计入。
    let expected_fees_raw = primary_report.fees_raw + reference_report.fees_raw - residual_fees_raw;
    if totals.0 != expected_fees_raw {
        return Err(format!(
            "多腿归因费用不闭合: groups={} expected={}",
            totals.0, expected_fees_raw
        ));
    }
    let net_cost_raw = totals.0 + totals.2;
    // V11 Q0e：把"计划了多少"和"真正成交了多少"并列导出，并当场核对闭合关系。
    // 组里只允许出现实际成交（`multi_leg_group_attributions` 按 `filled_qty_raw` 配对），
    // 因此"落在成组之外的成交"必然等于"裸腿待对账量"；两侧不等就说明归因又开始了
    // 乐观记账——把没拿到的腿当成拿到了。
    let leg_integrity = [
        multi_leg_leg_integrity(&primary_leg, &primary_buckets),
        multi_leg_leg_integrity(&reference_leg, &reference_buckets),
    ];
    let naked_filled_qty_raw = pending_reconcile
        .iter()
        .map(|item| {
            item.filled_qty_raw
                .parse::<i128>()
                .map_err(|error| format!("裸腿成交量无法解析: {error}"))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .sum::<i128>();
    if naked_filled_qty_raw != residual_filled_qty_raw {
        return Err(format!(
            "多腿裸腿事实与残余成交不闭合: pending={naked_filled_qty_raw} residual={residual_filled_qty_raw}"
        ));
    }
    for (leg, integrity, report) in [
        (&primary_leg, &leg_integrity[0], &primary_report),
        (&reference_leg, &leg_integrity[1], &reference_report),
    ] {
        let filled_from_fills_raw = report
            .fills
            .iter()
            .map(|fill| fill.qty.raw().abs())
            .sum::<i128>();
        if filled_from_fills_raw.to_string() != integrity.filled_qty_raw {
            return Err(format!(
                "{} 腿归因成交量与撮合成交不闭合: attribution={} fills={}",
                leg.label, integrity.filled_qty_raw, filled_from_fills_raw
            ));
        }
    }
    println!(
        "[Multi-leg · Integrity] {}",
        leg_integrity
            .iter()
            .map(multi_leg_integrity_summary)
            .collect::<Vec<_>>()
            .join(" ")
    );

    println!(
        "[Multi-leg · Reconcile] pending={} naked_filled_qty_raw={} policy=mark-pending-reconcile-no-auto-close",
        pending_reconcile.len(),
        naked_filled_qty_raw
    );
    let cost_bps = if totals.1 > 0 {
        i64::try_from(net_cost_raw.saturating_mul(10_000) / totals.1).unwrap_or(i64::MAX)
    } else {
        0
    };
    println!(
        "[Multi-leg · Attribution] strategy={} groups={} turnover_raw={} fees_raw={} funding_raw={} filled_qty_raw={} margin_peak_raw={} net_cost_raw={} cost_bps={} residual_filled_qty_raw={} residual_fees_raw={} funding_bps={} margin_model={} funding_model={}",
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
        margin_model,
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
    println!(
        "[Multi-leg · Execution] fill_model={} source={}",
        fill_model_name, fill_model_source
    );
    if let Some(root) = artifact_root {
        let runs = root.join("runs");
        std::fs::create_dir_all(&runs)
            .map_err(|error| format!("创建多腿归因产物目录失败: {error}"))?;
        let payload = serde_json::to_string_pretty(&serde_json::json!({
            // v2：组只由**实际成交**配对，并新增 legs 计划/成交差距与 pending_reconcile。
            "schema_version": 2,
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
            // 每条腿的记账口径来自哪份文件（null = 没给，按现货乘数 1 记账）。缺了它，
            // "margin_peak_raw=0" 就分不清是现货还是漏配规格（V11 Q58）。
            "market_specs": {
                "primary": primary_spec_path.map(|path| path.display().to_string()),
                "reference": reference_spec_path.map(|path| path.display().to_string()),
            },
            "margin_model": margin_model,
            // 多腿链不写 summary，撮合口径只能记在这里，否则"换了模型"在这条链上不可见。
            "fill_model": { "name": fill_model_name, "source": fill_model_source },
            "accounts": {
                "primary_initial_cash": primary_cash.raw().to_string(),
                "reference_initial_cash": reference_cash.raw().to_string(),
                // 期末权益与期初本金同侧落盘，读者才能自行复算组合收益（V11 Q71）。
                "primary_final_equity_raw": primary_equity.to_string(),
                "reference_final_equity_raw": reference_equity.to_string(),
                "funding_rule": "2*quantity*leg-own-max-high + same-notional taker fee headroom, floor 100000",
            },
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
                "combined_return_bps": combined_return_bps,
            },
            "legs": leg_integrity
            .iter()
            .map(multi_leg_integrity_json)
            .collect::<Vec<_>>(),
            "pending_reconcile":
            multi_leg_pending_reconcile_json(&pending_reconcile),
            "groups": groups,
            "assumptions": [
                "fees=per-leg FIFO allocated to signal ts buckets",
                "margin=spec.initial_margin(|realized fills standing|, bar close, leverage=1), only on legs whose market spec declares a derivative product",
                "funding=realized-fill notional*funding_bps*holding_ms/(8h*10000), long pays positive, only on derivative legs; a leg without market spec is booked as spot multiplier 1 and is refused when funding_bps!=0",
                "cost_bps=net_cost*10000/turnover",
                "groups=paired by ts only when BOTH legs have realized fills; planned-but-unfilled qty is reported per leg, never paired",
                "pending_reconcile=a leg filled while its counterpart did not; no automatic close is executed",
                "fees=one explicit cost binding shared by both legs; market spec carries no maker/taker field, so per-venue fee tiers are not applied",
                "fill_model=one declared model shared by both legs; one_tick_slippage takes each leg's own spec price_tick",
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
