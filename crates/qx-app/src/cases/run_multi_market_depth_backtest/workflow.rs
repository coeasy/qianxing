use super::super::artifacts::{read_text, write_text};
use crate::{
    AppError, AppErrorCategory, MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec, RunContext, StrategyArtifactIdentity,
};
use qx_core::{Fnv1a, Money, Quantity, ReplayVerifier};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{
    DepthBarStrategy, DepthFrame, NativeOrderBookStrategy, OrderBookExecutionModel,
    OrderBookPortfolioBacktestEngine, OrderBookPortfolioMarket, OrderBookStrategy,
};
use qx_zhenlu::RiskGate;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) struct InjectedStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

#[derive(Serialize)]
struct DataIdentity {
    instrument: String,
    data_tier: String,
    source: String,
    content_hash: String,
    snapshot_count: usize,
}

struct PreparedMarket {
    request: MultiMarketDepthBacktestMarket,
    frame: DepthFrame,
    data_identity: DataIdentity,
    execution_model: OrderBookExecutionModel,
    strategy: Box<dyn OrderBookStrategy>,
}

enum MarketStrategy {
    Builtin(Box<BuiltinStrategy>),
    Callback(Box<dyn Strategy>),
}

pub(super) fn run_inner(
    spec: &MultiMarketDepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<MultiMarketDepthBacktestOutcome>, AppError> {
    run_inner_with_strategies(spec, context, cancellation, BTreeMap::new())
}

pub(super) fn run_inner_with_strategies(
    spec: &MultiMarketDepthBacktestSpec,
    _context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    mut injected: BTreeMap<String, InjectedStrategy>,
) -> Result<Option<MultiMarketDepthBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;
    let spec_json = spec.to_json()?;
    let mut spec_hasher = Fnv1a::new();
    spec_hasher.write_text(&spec_json);
    let mut data_hasher = Fnv1a::new();
    let mut identities = Vec::new();
    let mut prepared = Vec::with_capacity(spec.markets.len());

    for request in &spec.markets {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let payload = read_text(&request.depth_path)?;
        let frame = DepthFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} DepthFrame 无效: {error}",
                request.instrument
            ))
        })?;
        if frame.instrument.to_string() != request.instrument {
            return Err(invalid(format!(
                "市场 {} 的深度帧实际标的是 {}",
                request.instrument, frame.instrument
            )));
        }
        let data_tier = request.data_tier.as_deref().unwrap_or("l2");
        if data_tier == "l1"
            && frame
                .snapshots
                .iter()
                .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
        {
            return Err(invalid(format!(
                "市场 {} 声明 l1，但至少一份快照不是单档 bid/ask",
                request.instrument
            )));
        }
        if frame.snapshots.len() < 2 {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要两份盘口快照", request.instrument),
            ));
        }
        if request.latency_snapshots > 0
            && request.latency_snapshots >= (frame.snapshots.len() as u64).saturating_sub(1)
        {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "市场 {} 的 latency_snapshots 无法在当前样本中成熟成交",
                    request.instrument
                ),
            ));
        }
        let first_ts = frame.snapshots.first().expect("at least two snapshots").ts;
        let last_ts = frame.snapshots.last().expect("at least two snapshots").ts;
        if first_ts < request.market_spec.valid_from
            || request
                .market_spec
                .valid_to
                .is_some_and(|valid_to| last_ts >= valid_to)
        {
            return Err(invalid(format!(
                "市场 {} 的产品规格有效区间未覆盖深度帧时间范围",
                request.instrument
            )));
        }
        let frame_hash = frame.input_hash();
        let data_fingerprint = format!("depth:{data_tier}:{}:{frame_hash:016x}", frame.source);
        data_hasher.write_text(&request.instrument);
        data_hasher.write_text(data_tier);
        data_hasher.write_text(&frame.source);
        data_hasher.write_u64(frame_hash);

        let external = injected.remove(&request.strategy.strategy_id);
        let (strategy_id, strategy_version, strategy) = match external {
            Some(value) => {
                identities.push(value.identity.clone());
                (
                    value.identity.strategy_id.clone(),
                    value.identity.strategy_version.clone(),
                    MarketStrategy::Callback(value.strategy),
                )
            }
            None => {
                let config = BuiltinStrategyConfig {
                    fast_window: request.strategy.fast_window,
                    slow_window: request.strategy.slow_window,
                    period: request.strategy.period,
                    threshold_bps: request.strategy.threshold_bps,
                    target_notional_raw: request.strategy.target_notional_raw,
                    target_available_margin_bps: request.strategy.target_available_margin_bps,
                    target_volatility_bps: request.strategy.target_volatility_bps,
                    target_kelly_fraction_bps: request.strategy.target_kelly_fraction_bps,
                    kelly_lookback_bars: request.strategy.kelly_lookback_bars,
                    primary_policy: request.order_policy,
                    ..BuiltinStrategyConfig::new(
                        request.strategy.kind,
                        request.strategy.strategy_id.clone(),
                        frame.instrument.clone(),
                        Quantity::from_raw(request.strategy.quantity_raw),
                    )
                    .map_err(invalid)?
                };
                config.validate().map_err(invalid)?;
                (
                    request.strategy.strategy_id.clone(),
                    format!("{}-v1", request.strategy.strategy_id),
                    MarketStrategy::Builtin(Box::new(
                        BuiltinStrategy::new(config).map_err(invalid)?,
                    )),
                )
            }
        };
        let strategy_context = StrategyContext {
            strategy_id,
            strategy_version,
            account_id: spec.account_id.clone(),
            venue_id: frame.instrument.venue.to_string(),
            data_fingerprint,
            as_of: first_ts,
            positions: BTreeMap::new(),
            cash: BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]),
            available_margin_raw: Some(spec.initial_cash_raw),
            risk_state: "backtest".into(),
        };
        let tier = if data_tier == "l1" {
            qx_xingban::DataTier::L1
        } else {
            qx_xingban::DataTier::L2L3
        };
        let strategy: Box<dyn OrderBookStrategy> = match strategy {
            MarketStrategy::Builtin(strategy) => {
                let mut adapter =
                    DepthBarStrategy::new(*strategy, strategy_context, frame.instrument.clone());
                adapter.initialize().map_err(invalid)?;
                Box::new(adapter)
            }
            MarketStrategy::Callback(strategy) => {
                let mut adapter =
                    NativeOrderBookStrategy::new(CallbackStrategy(strategy), strategy_context)
                        .with_data_tier(tier);
                adapter
                    .strategy
                    .on_init(&adapter.context)
                    .map_err(invalid)?;
                Box::new(adapter)
            }
        };
        let mut model = OrderBookExecutionModel::new(request.fee_bps).map_err(invalid)?;
        model.latency_snapshots = request.latency_snapshots;
        model.queue_position_bps = request.queue_position_bps;
        model.market_impact_bps = request.market_impact_bps;
        model.validate().map_err(invalid)?;
        let data_identity = DataIdentity {
            instrument: request.instrument.clone(),
            data_tier: data_tier.into(),
            source: frame.source.clone(),
            content_hash: format!("{frame_hash:016x}"),
            snapshot_count: frame.snapshots.len(),
        };
        prepared.push(PreparedMarket {
            request: request.clone(),
            frame,
            data_identity,
            execution_model: model,
            strategy,
        });
    }
    if !injected.is_empty() {
        return Err(invalid("多市场盘口自定义策略包含未消费的孤儿槽"));
    }
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    for identity in &identities {
        spec_hasher.write_text(&identity.strategy_id);
        spec_hasher.write_text(&identity.strategy_version);
        spec_hasher.write_text(&identity.source_sha256);
    }
    let spec_fingerprint = format!("{:016x}", spec_hasher.finish());
    let data_fingerprint = format!("{:016x}", data_hasher.finish());
    let mut markets = prepared
        .iter_mut()
        .map(|market| OrderBookPortfolioMarket {
            instrument_spec: market.request.market_spec.clone(),
            data_tier: if market.request.data_tier.as_deref() == Some("l1") {
                qx_xingban::DataTier::L1
            } else {
                qx_xingban::DataTier::L2L3
            },
            priority: market.request.priority,
            snapshots: &market.frame.snapshots,
            execution_model: market.execution_model,
            strategy: market.strategy.as_mut(),
        })
        .collect::<Vec<_>>();
    let report = OrderBookPortfolioBacktestEngine::new(
        spec.account_id.clone(),
        spec.settlement_currency.clone(),
        Money::from_raw(spec.initial_cash_raw),
        RiskGate::conservative_default(),
        std::mem::take(&mut markets),
    )
    .map_err(|error| AppError::from_qx_error(&error))?
    .run_with_cancel(|| cancellation.is_some_and(|token| token.is_cancelled()))
    .map_err(|error| AppError::from_qx_error(&error))?;
    let Some(report) = report else {
        return Ok(None);
    };
    report
        .event_log
        .validate()
        .map_err(|error| AppError::from_qx_error(&error))?;
    let replay = ReplayVerifier::verify(report.event_log.events(), &report.ledger)
        .map_err(|error| AppError::from_qx_error(&error))?;
    if replay.log_digest != report.result_hash() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场盘口回测结果哈希与 ReplayVerifier 不一致",
        ));
    }
    let final_equity_raw = report
        .equity
        .last()
        .map(|(_, value)| *value)
        .unwrap_or(spec.initial_cash_raw);
    let return_bps = final_equity_raw
        .checked_sub(spec.initial_cash_raw)
        .and_then(|pnl| pnl.checked_mul(10_000))
        .and_then(|value| value.checked_div(spec.initial_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid("多市场盘口收益无法表示"))?;
    let outcome_path =
        PathBuf::from(&spec.output_dir).join(format!("{}.multi-market-depth.json", spec.run_id));
    let artifact_path = outcome_path.to_string_lossy().into_owned();
    let outcome = MultiMarketDepthBacktestOutcome {
        run_id: spec.run_id.clone(),
        account_id: spec.account_id.clone(),
        spec_fingerprint,
        data_fingerprint,
        shared_result_hash: format!("{:016x}", report.result_hash()),
        markets_total: spec.markets.len(),
        successful_fills: u64::try_from(report.fills.len())
            .map_err(|_| invalid("多市场盘口成交数量超出 u64 范围"))?,
        pending_orders: report.pending_orders,
        equity_points: u64::try_from(report.equity.len())
            .map_err(|_| invalid("多市场盘口权益点数超出 u64 范围"))?,
        final_equity_raw,
        return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        artifact_path: artifact_path.clone(),
    };
    let per_market_fills = report
        .fills_by_instrument
        .iter()
        .map(|(instrument, fills)| (instrument.to_string(), fills.len()))
        .collect::<BTreeMap<_, _>>();
    let data_identities = prepared
        .iter()
        .map(|market| &market.data_identity)
        .collect::<Vec<_>>();
    let artifact_json = serde_json::to_string(&serde_json::json!({
        "schema_version": 1,
        "spec": spec,
        "outcome": outcome,
        "market_data": data_identities,
        "strategy_identities": identities,
        "per_market_fills": per_market_fills,
        "equity": report.equity,
        "event_log": report.event_log.events(),
        "ledger_entries": report.ledger.entries(),
        "replay_verified": true,
    }))
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多市场盘口产物目录", &error))?;
    match std::fs::read_to_string(&outcome_path) {
        Ok(existing) if existing == artifact_json => return Ok(Some(outcome)),
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口运行产物已存在且内容不同: {artifact_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取多市场盘口回测产物", &error)),
    }
    write_text(&artifact_path, &artifact_json)?;
    Ok(Some(outcome))
}

struct CallbackStrategy(Box<dyn Strategy>);

impl Strategy for CallbackStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        self.0.on_event(context, event)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_order_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.0.on_stop()
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
