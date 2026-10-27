//! Shared Rust execution and evidence orchestration for depth runs.

use super::artifacts::{config_hash, persist, portfolio_fill_attribution, DepthArtifactInput};
use super::portfolio::{build_portfolio_strategy, has_builtin_sizing, strategy_specs};
use super::quality::verified_depth_dataset_quality;
use super::{invalid_input, CallbackStrategy, InjectedDepthStrategy, DEFAULT_ACCOUNT_ID};
use crate::cases::artifacts::read_text;
use crate::cases::risk_gate::{risk_gate_for, risk_rule_source_label};
use crate::run_handle::CancellationToken;
use crate::{AppError, AppErrorCategory, DepthBacktestOutcome, DepthBacktestSpec, RunContext};
use qx_core::{Money, Quantity, ReplayVerifier};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, StrategyContext};
use qx_xingban::{
    depth_frame_to_ticks, DataTier, DepthBarStrategy, DepthFrame, MarketMakingStrategy,
    NativeOrderBookStrategy, NativeTickStrategy, OrderBookBacktestConfig, OrderBookBacktestEngine,
    OrderBookExecutionModel, TickBacktestConfig, TickBacktestEngine,
};
use std::collections::BTreeMap;
use std::path::Path;

pub(in crate::cases) fn run_inner(
    spec: &DepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<DepthBacktestOutcome>, AppError> {
    run_inner_with_strategy(spec, context, cancellation, Vec::new())
}

pub(in crate::cases) fn run_inner_with_strategy(
    spec: &DepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    mut injected: Vec<InjectedDepthStrategy>,
) -> Result<Option<DepthBacktestOutcome>, AppError> {
    spec.validate()?;
    let mut seen_custom = std::collections::BTreeSet::new();
    for custom in &injected {
        if !seen_custom.insert(custom.identity.strategy_id.as_str()) {
            return Err(invalid_input(format!(
                "自定义 Tick/OrderBook 策略身份重复: {}",
                custom.identity.strategy_id
            )));
        }
        if custom.experiment_parameters {
            custom.identity.validate_for_depth_experiment(spec)?;
        } else {
            custom.identity.validate_for_depth(spec)?;
        }
        if strategy_specs(spec)
            .find(|strategy| strategy.strategy_id == custom.identity.strategy_id)
            .is_some_and(has_builtin_sizing)
        {
            return Err(invalid_input(format!(
                "自定义 Tick/OrderBook 策略 {} 不能同时声明仅适用于内置策略的 sizing 参数",
                custom.identity.strategy_id
            )));
        }
    }
    if spec.portfolio.is_none() && injected.len() > 1 {
        return Err(invalid_input(
            "多个自定义 Tick/OrderBook 策略必须声明同账户 portfolio 组合规格",
        ));
    }
    let payload = read_text(&spec.depth_path)?;
    let frame = DepthFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("DepthFrame 校验失败 {}: {error}", spec.depth_path),
        )
    })?;
    if frame.snapshots.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "Tick/OrderBook 回测至少需要一份快照",
        ));
    }
    // 延迟的**帧相对**上界是唯一的拒点（三个入口共用这一条）：`latency_snapshots` 数的是"提交后还要
    // 再等几份快照"，而内核每份快照只撮合一轮，成熟条件恒为 `1 + 延迟 + 1 <= 帧内快照数`。越过这条线
    // 不是"延迟更长"，而是任何订单都永远等不到成熟——撮合一笔都不成交却照常退 0、落一份空成交表。
    // 文案点名旗标与这份帧实际可用的上界，否则使用者无从判断该改成多少。
    if spec.latency_snapshots > 0
        && spec.latency_snapshots >= (frame.snapshots.len() as u64).saturating_sub(1)
    {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "深度延迟永远等不到成交：--latency-snapshots={} 在这份 {} 份快照的帧上无法成熟，最多用 {}（延迟按快照个数计，订单要等到提交后第 延迟+1 份才成熟）",
                spec.latency_snapshots,
                frame.snapshots.len(),
                (frame.snapshots.len() as u64).saturating_sub(2)
            ),
        ));
    }
    // 帧本身短不是旗标的错：单份快照的缺省延迟照样可以跑，只是任何策略都等不到第二份快照的信号。
    if spec.tier == "l1"
        && frame
            .snapshots
            .iter()
            .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
    {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "L1 Tick 回测只接受一档盘口：每份快照必须恰有一个 bid 和一个 ask 档位",
        ));
    }

    let instrument = frame.instrument.clone();
    if let Some(market_spec) = spec.market_spec.as_ref() {
        if market_spec.instrument != instrument {
            return Err(invalid_input(
                "market_spec.instrument 与 Tick/OrderBook 输入标的不一致",
            ));
        }
        let first_ts = frame
            .snapshots
            .first()
            .map(|snapshot| snapshot.ts)
            .ok_or_else(|| invalid_input("Tick/OrderBook DepthFrame has no snapshots"))?;
        let last_ts = frame
            .snapshots
            .last()
            .map(|snapshot| snapshot.ts)
            .ok_or_else(|| invalid_input("Tick/OrderBook DepthFrame has no snapshots"))?;
        if first_ts < market_spec.valid_from
            || market_spec
                .valid_to
                .is_some_and(|valid_to| last_ts >= valid_to)
        {
            return Err(invalid_input(format!(
                "market_spec 有效区间 [{}, {:?}) 未覆盖输入时间 [{first_ts}, {last_ts}]",
                market_spec.valid_from, market_spec.valid_to
            )));
        }
    }
    let strategy_id = injected
        .iter()
        .find(|custom| custom.identity.strategy_id == spec.strategy.strategy_id)
        .map(|custom| custom.identity.strategy_id.clone())
        .unwrap_or_else(|| spec.strategy.strategy_id.clone());
    let account_id = spec
        .account_id
        .as_deref()
        .unwrap_or(DEFAULT_ACCOUNT_ID)
        .to_string();
    let strategy_version = injected
        .iter()
        .find(|custom| custom.identity.strategy_id == spec.strategy.strategy_id)
        .map(|custom| custom.identity.strategy_version.clone())
        .unwrap_or_else(|| format!("builtin-{}-v1", spec.strategy.kind.name()));
    // 风控门与吃单费率都只在用例里解析一次：门面各自的兜底会出现"同一份配置在 book 上用
    // 一套口径、在 multi-builtin 上用另一套"（V10 §4.2 / V11 Q0c）。
    let risk = risk_gate_for(spec.risk_rules.as_ref(), spec.allow_short());
    let risk_rule_set_version = risk.rule_set().version().to_string();
    let risk_rule_source = risk_rule_source_label(spec.risk_rules.as_ref());
    let fee_bps = spec.resolved_fee_bps();
    let cost_source = spec.resolved_cost_source();
    let mut model = OrderBookExecutionModel::new(fee_bps).map_err(invalid_input)?;
    model.latency_snapshots = spec.latency_snapshots;
    model.queue_position_bps = spec.queue_position_bps;
    model.queue_position_model = spec.queue_position_model;
    model.market_impact_bps = spec.market_impact_bps;
    model.validate().map_err(invalid_input)?;
    let data_fingerprint = format!("depth:{}:{:016x}", frame.source, frame.input_hash());
    // 质量清单是输入的一部分：它与本帧的身份、档位与时间轴在这里按本帧**重算**核对，不再由门面
    // 各自核对（否则同一条 quality_manifest_path 在三个入口上有三种含义）。
    let dataset_quality = spec
        .quality_manifest_path
        .as_deref()
        .map(Path::new)
        .map(|path| {
            verified_depth_dataset_quality(path, Path::new(&spec.depth_path), &frame, &spec.tier)
        })
        .transpose()?;
    // 自定义策略的身份按 strategy_id 排序后并入指纹：结果取决于"是谁在跑"，与入参顺序无关。
    let mut custom_identities: Vec<&crate::cases::StrategyArtifactIdentity> =
        injected.iter().map(|custom| &custom.identity).collect();
    custom_identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let canonical_custom_identities = custom_identities
        .iter()
        .map(|identity| (*identity).clone())
        .collect::<Vec<_>>();
    let config_hash = config_hash(
        spec,
        &frame,
        fee_bps,
        &risk_rule_set_version,
        dataset_quality
            .as_ref()
            .and_then(|quality| quality["manifest_fingerprint"].as_str()),
        &custom_identities,
    );
    let strategy_config = BuiltinStrategyConfig {
        fast_window: spec.strategy.fast_window,
        slow_window: spec.strategy.slow_window,
        period: spec.strategy.period,
        threshold_bps: spec.strategy.threshold_bps,
        target_notional_raw: spec.strategy.target_notional_raw,
        target_available_margin_bps: spec.strategy.target_available_margin_bps,
        target_volatility_bps: spec.strategy.target_volatility_bps,
        target_kelly_fraction_bps: spec.strategy.target_kelly_fraction_bps,
        kelly_lookback_bars: spec.strategy.kelly_lookback_bars,
        primary_policy: spec.order_policy,
        ..BuiltinStrategyConfig::new(
            spec.strategy.kind,
            strategy_id.clone(),
            instrument.clone(),
            Quantity::from_raw(spec.strategy.quantity_raw),
        )
        .map_err(invalid_input)?
    };
    strategy_config.validate().map_err(invalid_input)?;
    let strategy = BuiltinStrategy::new(strategy_config).map_err(invalid_input)?;
    let cash = BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]);
    let strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: account_id.clone(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: frame.snapshots[0].ts,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        available_margin_raw: crate::cases::strategy_available_margin_raw(
            &cash,
            &spec.settlement_currency,
        ),
        cash,
        risk_state: "backtest".into(),
    };
    let mut depth_strategy =
        DepthBarStrategy::new(strategy, strategy_context.clone(), instrument.clone());
    depth_strategy.initialize().map_err(invalid_input)?;
    let standalone_custom = if spec.portfolio.is_none() {
        injected.pop()
    } else {
        None
    };
    let mut portfolio_strategy = spec
        .portfolio
        .as_ref()
        .map(|_| {
            build_portfolio_strategy(
                spec,
                &instrument,
                &account_id,
                &data_fingerprint,
                frame.snapshots[0].ts,
                injected,
            )
        })
        .transpose()?;

    let report = match spec.tier.as_str() {
        "l1" => {
            let ticks = depth_frame_to_ticks(&frame).map_err(invalid_input)?;
            let engine = TickBacktestEngine::new(TickBacktestConfig {
                instrument: instrument.clone(),
                account_id: account_id.clone(),
                currency: spec.settlement_currency.clone(),
                initial_cash: Money::from_raw(spec.initial_cash_raw),
                fee_bps,
                instrument_spec: spec.market_spec.clone(),
                risk,
            })
            .with_execution_model(model)
            .with_funding_bps(spec.funding_bps)
            .with_funding_events(spec.funding_events.clone());
            match (cancellation, portfolio_strategy.as_mut(), standalone_custom) {
                (_, _, Some(custom)) => {
                    let mut strategy = NativeTickStrategy::new(
                        CallbackStrategy(custom.strategy),
                        strategy_context.clone(),
                        instrument.clone(),
                    );
                    strategy.initialize().map_err(invalid_input)?;
                    match cancellation {
                        Some(cancellation) => engine
                            .run_with_cancel(&ticks, &mut strategy, || cancellation.is_cancelled())
                            .map_err(|error| AppError::from_qx_error(&error))?,
                        None => Some(
                            engine
                                .run(&ticks, &mut strategy)
                                .map_err(|error| AppError::from_qx_error(&error))?,
                        ),
                    }
                }
                (cancellation, portfolio, None) => match (cancellation, portfolio) {
                    (Some(cancellation), Some(strategy)) => engine
                        .run_with_cancel(&ticks, strategy, || cancellation.is_cancelled())
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    (None, Some(strategy)) => Some(
                        engine
                            .run(&ticks, strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                    (Some(cancellation), None) => engine
                        .run_with_cancel(&ticks, &mut depth_strategy, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    (None, None) => Some(
                        engine
                            .run(&ticks, &mut depth_strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                },
            }
        }
        "l2" => {
            let engine = OrderBookBacktestEngine::new(OrderBookBacktestConfig {
                instrument: instrument.clone(),
                account_id: account_id.clone(),
                currency: spec.settlement_currency.clone(),
                initial_cash: Money::from_raw(spec.initial_cash_raw),
                fee_bps,
                instrument_spec: spec.market_spec.clone(),
                risk,
                data_tier: DataTier::L2L3,
            })
            .with_execution_model(model)
            .with_funding_bps(spec.funding_bps)
            .with_funding_events(spec.funding_events.clone());
            if let Some(config) = spec.market_making {
                let mut maker = MarketMakingStrategy::new(
                    config,
                    instrument.clone(),
                    &account_id,
                    strategy_id.clone(),
                    spec.order_policy,
                )
                .map_err(invalid_input)?;
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, &mut maker, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, &mut maker)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            } else if let Some(custom) = standalone_custom {
                let mut strategy = NativeOrderBookStrategy::new(
                    CallbackStrategy(custom.strategy),
                    strategy_context.clone(),
                );
                strategy.initialize().map_err(invalid_input)?;
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, &mut strategy, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, &mut strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            } else if let Some(strategy) = portfolio_strategy.as_mut() {
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, strategy, || cancellation.is_cancelled())
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            } else {
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, &mut depth_strategy, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, &mut depth_strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            }
        }
        _ => unreachable!("DepthBacktestSpec::validate constrains tier"),
    };
    let Some(report) = report else {
        return Ok(None);
    };

    let replay =
        ReplayVerifier::verify(report.event_log.events(), &report.ledger).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook 事件日志未通过重放校验: {error}"),
            )
        })?;
    if replay.log_digest != report.result_hash() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook 结果哈希与重放哈希不一致",
        ));
    }
    if report.snapshot_ts.len() != report.equity.len()
        || report.equity_exact.len() != report.equity.len()
    {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook raw/exact 权益曲线与输入快照数量不一致",
        ));
    }

    // 四份产物由用例经统一发布出口落盘：内容寻址、失败不留下半份、重复落同一份内容才算幂等。
    let persisted = persist(DepthArtifactInput {
        spec,
        frame: &frame,
        report: &report,
        risk_rule_set_version: &risk_rule_set_version,
        risk_rule_source,
        cost_source: &cost_source,
        config_hash: &config_hash,
        data_fingerprint: &data_fingerprint,
        dataset_quality: dataset_quality.as_ref(),
        strategy_id: &strategy_id,
        strategy_version: &strategy_version,
        custom_strategies: &canonical_custom_identities,
        effective_account_id: &account_id,
        account_id: spec.account_id.as_deref(),
        portfolio: portfolio_fill_attribution(spec, &report.fills)?,
        context,
    })?;
    let artifacts = persisted.artifacts;
    let manifest_digest = format!("{:016x}", persisted.manifest.digest());

    Ok(Some(DepthBacktestOutcome {
        run_id: spec.run_id.clone(),
        manifest_run_id: spec.manifest_run_id.clone(),
        instrument: instrument.to_string(),
        tier: spec.tier.clone(),
        result_hash: format!("{:016x}", report.result_hash()),
        data_fingerprint,
        fills: report.fills.len() as u64,
        equity_points: report.equity.len() as u64,
        snapshots: report.snapshot_ts.len() as u64,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        artifacts,
        manifest_digest,
        account_id: spec.account_id.clone(),
        display_summary: persisted.summary,
    }))
}
