//! N-market shared-account Bar backtest through the single PortfolioBarEngine.

use super::artifacts::{read_text, write_text};
use super::attach;
use super::guard::guard_panics;
use super::run_backtest::strategy_slot::{
    build_strategy_slot, strategy_version, StrategySlotContext,
};
use super::run_multi_leg_backtest::execution_market_spec;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiMarketBacktestOutcome,
    MultiMarketBacktestSpec, RunContext, StrategyArtifactIdentity,
};
use qx_core::{Fnv1a, InstrumentId, MakerTakerFeeModel, Money};
use qx_datastruct::BarFrame;
use qx_strategy::Strategy;
use qx_xingban::{
    LeverageMargin, MarketRoutedBarStrategy, NextBarOpenFillModel, NoMargin, PortfolioBarEngine,
    PortfolioBarMarket, PortfolioBarSessionConfig, PortfolioBarStrategy, ZeroLatency,
};
use qx_zhenlu::RiskGate;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize)]
struct DataIdentity {
    instrument: String,
    content_hash: String,
    row_count: usize,
}

struct PreparedMarket {
    instrument: InstrumentId,
    bars: Vec<qx_guanxing::Bar>,
    priority: u32,
    contract: qx_core::TradingInstrumentSpec,
    fee: Option<MakerTakerFeeModel>,
    margin: Option<Box<dyn qx_xingban::MarginRule>>,
    seed: u64,
    identity: DataIdentity,
}

pub fn run_multi_market_backtest(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
) -> Result<MultiMarketBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多市场共享账户 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner(spec, context, None, BTreeMap::new())
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多市场回测意外返回取消状态",
            ),
            &correlation_id,
        )
    })
}

impl MultiMarketBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiMarketBacktestOutcome> {
        let run_id = self.run_id.clone();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场共享账户 Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), BTreeMap::new())
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Run selected market slots with caller-authored strategies on the same Rust engine.
    pub fn run_with_strategy_slots(
        &self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: &RunContext,
    ) -> Result<MultiMarketBacktestOutcome, AppError> {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多市场多语言策略 Bar 回测")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        let injected =
            validate_injected(self, strategies).map_err(|error| attach(error, &correlation_id))?;
        guard_panics(&correlation_id, || run_inner(self, context, None, injected))
            .map_err(|error| attach(error, &correlation_id))?
            .ok_or_else(|| {
                attach(
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "同步多市场自定义策略回测未产生报告",
                    ),
                    &correlation_id,
                )
            })
    }

    /// Start a cancellable multi-market run with caller-authored strategy slots.
    pub fn start_with_strategy_slots(
        self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: RunContext,
    ) -> Result<crate::RunHandle<MultiMarketBacktestOutcome>, AppError> {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多市场多语言策略 Bar 回测")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        let injected =
            validate_injected(&self, strategies).map_err(|error| attach(error, &correlation_id))?;
        let run_id = self.run_id.clone();
        Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), injected)
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

struct InjectedMarketStrategy {
    identity: StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
}

fn validate_injected(
    spec: &MultiMarketBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError> {
    if strategies.is_empty() {
        return Err(invalid("多市场自定义策略组合至少需要一个回调槽"));
    }
    let mut injected = BTreeMap::new();
    for (identity, strategy) in strategies {
        let slot = spec
            .markets
            .iter()
            .flat_map(|market| &market.strategies)
            .find(|member| member.strategy.strategy_id == identity.strategy_id)
            .ok_or_else(|| {
                invalid(format!(
                    "自定义策略身份不属于多市场组合: {}",
                    identity.strategy_id
                ))
            })?;
        identity.validate_for_slot(&slot.strategy.strategy_id, &slot.strategy)?;
        let id = identity.strategy_id.clone();
        if injected
            .insert(id.clone(), InjectedMarketStrategy { identity, strategy })
            .is_some()
        {
            return Err(invalid(format!("多市场自定义策略身份重复: {id}")));
        }
    }
    Ok(injected)
}

fn run_inner(
    spec: &MultiMarketBacktestSpec,
    _context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    mut injected: BTreeMap<String, InjectedMarketStrategy>,
) -> Result<Option<MultiMarketBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;
    let spec_json = spec.to_json()?;
    let mut spec_hasher = Fnv1a::new();
    spec_hasher.write_text(&spec_json);
    let spec_fingerprint_base = spec_hasher.finish();

    let mut prepared = Vec::with_capacity(spec.markets.len());
    let mut data_hasher = Fnv1a::new();
    let mut runtime_markets = Vec::with_capacity(spec.markets.len());
    let mut routed_strategies = Vec::with_capacity(spec.markets.len());
    let mut strategy_count = 0_usize;
    let mut strategy_identities = Vec::new();
    for market in &spec.markets {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let payload = read_text(&market.bars_path)?;
        let frame = BarFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} BarFrame 无效: {error:?}",
                market.instrument
            ))
        })?;
        if frame.instrument.to_string() != market.instrument {
            return Err(invalid(format!(
                "市场 {} 的 BarFrame 实际标的是 {}",
                market.instrument, frame.instrument
            )));
        }
        let bars: Vec<_> = (&frame).into();
        if bars.len() < crate::MIN_BACKTEST_BARS {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要两根 Bar", market.instrument),
            ));
        }
        let instrument = frame.instrument.clone();
        let data_hash = frame.digest();
        data_hasher.write_text(&instrument.to_string());
        data_hasher.write_u64(data_hash);
        let market_data_fingerprint = format!("barframe:{data_hash:016x}");
        let contract = execution_market_spec(
            market.market_spec.as_ref(),
            &instrument,
            &spec.settlement_currency,
        );
        let margin: Box<dyn qx_xingban::MarginRule> = if contract.product.supports_leverage() {
            Box::new(LeverageMargin {
                leverage: 1,
                maintenance_bp: i64::from(contract.maintenance_margin_bps),
            })
        } else {
            Box::new(NoMargin)
        };
        let fee = market
            .fee_schedule
            .map(|schedule| schedule.model())
            .unwrap_or_else(MakerTakerFeeModel::default_maker_taker);
        let mut strategy_slots = Vec::with_capacity(market.strategies.len());
        for member in &market.strategies {
            let external = injected.remove(&member.strategy.strategy_id);
            let version = external
                .as_ref()
                .map(|item| item.identity.strategy_version.clone())
                .unwrap_or_else(|| strategy_version(&member.strategy));
            if let Some(item) = &external {
                strategy_identities.push(item.identity.clone());
            }
            let slot = build_strategy_slot(
                &member.strategy,
                StrategySlotContext {
                    priority: member.priority,
                    reserved_capital_raw: member.reserved_capital_raw,
                    instrument: &instrument,
                    account_id: &spec.account_id,
                    currency: &spec.settlement_currency,
                    data_fingerprint: &market_data_fingerprint,
                    as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
                    strategy_version: Some(&version),
                },
                external.map(|item| item.strategy),
            )?;
            strategy_slots.push(slot);
            strategy_count += 1;
        }
        let strategy_basket = PortfolioBarStrategy::new(strategy_slots)
            .map_err(|error| AppError::from_qx_error(&error))?;
        routed_strategies.push((
            instrument.clone(),
            Box::new(strategy_basket) as Box<dyn qx_xingban::BarStrategy>,
        ));
        let row_count = bars.len();
        prepared.push(PreparedMarket {
            instrument,
            bars,
            priority: market.priority,
            contract,
            fee: Some(fee),
            margin: Some(margin),
            seed: spec.seed,
            identity: DataIdentity {
                instrument: market.instrument.clone(),
                content_hash: format!("{data_hash:016x}"),
                row_count,
            },
        });
    }
    if !injected.is_empty() {
        return Err(invalid("多市场自定义策略存在未消费的孤儿策略槽"));
    }
    strategy_identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let spec_fingerprint = if strategy_identities.is_empty() {
        format!("{spec_fingerprint_base:016x}")
    } else {
        let mut fingerprint = Fnv1a::new();
        fingerprint.write_u64(spec_fingerprint_base);
        for identity in &strategy_identities {
            fingerprint.write_text(&identity.strategy_id);
            fingerprint.write_text(&identity.strategy_version);
            fingerprint.write_text(&identity.source_sha256);
        }
        format!("{:016x}", fingerprint.finish())
    };
    let data_identities = prepared
        .iter()
        .map(|market| DataIdentity {
            instrument: market.identity.instrument.clone(),
            content_hash: market.identity.content_hash.clone(),
            row_count: market.identity.row_count,
        })
        .collect::<Vec<_>>();
    runtime_markets.extend(prepared.iter_mut().map(|market| PortfolioBarMarket {
        instrument: market.instrument.clone(),
        priority: market.priority,
        bars: market.bars.as_slice(),
        spec: market.contract.clone(),
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(market.fee.take().expect("fee prepared")),
        latency: Box::new(ZeroLatency),
        margin: market.margin.take().expect("margin prepared"),
        seed: market.seed,
    }));
    let data_fingerprint = format!("{:016x}", data_hasher.finish());
    let mut strategy = MarketRoutedBarStrategy::new(routed_strategies)
        .map_err(|error| AppError::from_qx_error(&error))?;
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: spec.account_id.clone(),
            currency: spec.settlement_currency.clone(),
            initial_cash: Money::from_raw(spec.initial_cash_raw),
            risk: RiskGate::conservative_default(),
            funding_bps: BTreeMap::new(),
            funding_events: Vec::new(),
        },
        runtime_markets,
    )
    .map_err(|error| AppError::from_qx_error(&error))?;
    let report = match cancellation {
        Some(token) => engine.run_with_cancel(&mut strategy, || token.is_cancelled()),
        None => engine.run(&mut strategy).map(Some),
    }
    .map_err(|error| AppError::from_qx_error(&error))?;
    let Some(report) = report else {
        return Ok(None);
    };
    report
        .verify()
        .map_err(|error| AppError::from_qx_error(&error))?;
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
        .ok_or_else(|| invalid("多市场组合收益无法表示"))?;
    let max_drawdown_bps = max_drawdown_bps(&report.equity)?;
    let successful_fills =
        u64::try_from(report.fills.len()).map_err(|_| invalid("多市场成交数超出 u64 范围"))?;
    let output_path =
        PathBuf::from(&spec.output_dir).join(format!("{}.multi-market.json", spec.run_id));
    let artifact_path = output_path.to_string_lossy().into_owned();
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多市场回测产物目录", &error))?;
    let outcome = MultiMarketBacktestOutcome {
        run_id: spec.run_id.clone(),
        account_id: spec.account_id.clone(),
        spec_fingerprint,
        data_fingerprint,
        shared_result_hash: format!("{:016x}", report.result_hash()),
        markets_total: spec.markets.len(),
        strategies_total: strategy_count,
        successful_fills,
        equity_points: u64::try_from(report.equity.len())
            .map_err(|_| invalid("多市场权益点数超出 u64 范围"))?,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
        artifact_path: artifact_path.clone(),
    };
    let per_market_fills = report
        .fills_by_instrument
        .iter()
        .map(|(instrument, fills)| (instrument.to_string(), fills.len()))
        .collect::<BTreeMap<_, _>>();
    let artifact = serde_json::json!({
        "schema_version": 1,
        "spec": spec,
        "outcome": outcome,
        "market_data": data_identities,
        "strategy_identities": strategy_identities,
        "per_market_fills": per_market_fills,
        "equity": report.equity,
        "event_log": report.event_log.events(),
        "ledger_entries": report.ledger.entries(),
        "replay_verified": true,
    });
    let artifact_json = serde_json::to_string(&artifact)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    match std::fs::read_to_string(&output_path) {
        Ok(existing) if existing == artifact_json => return Ok(Some(outcome)),
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场运行产物已存在且内容不同: {artifact_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取多市场回测产物", &error)),
    }
    write_text(&outcome.artifact_path, &artifact_json)?;
    Ok(Some(outcome))
}

fn max_drawdown_bps(equity: &[(u64, i128)]) -> Result<u32, AppError> {
    let mut peak = 0_i128;
    let mut maximum = 0_u32;
    for (_, value) in equity {
        peak = peak.max(*value);
        if peak <= 0 || *value >= peak {
            continue;
        }
        let drawdown = peak
            .checked_sub(*value)
            .and_then(|loss| loss.checked_mul(10_000))
            .and_then(|loss| loss.checked_div(peak))
            .unwrap_or(10_000)
            .clamp(0, 10_000) as u32;
        maximum = maximum.max(drawdown);
    }
    Ok(maximum)
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
