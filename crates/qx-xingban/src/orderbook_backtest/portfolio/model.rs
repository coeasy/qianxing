use super::super::funding::SpotCashReservations;
use crate::orderbook_strategies::OrderBookStrategy;
use crate::{
    DataTier, OrderBookExecutionDescriptor, OrderBookExecutionModel, OrderBookMatchingEngine,
    OrderBookSnapshot,
};
use qx_core::{EventLog, ExactDecimal, Fill, Fnv1a, Ledger, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

pub struct OrderBookPortfolioMarket<'a> {
    pub instrument_spec: TradingInstrumentSpec,
    pub data_tier: DataTier,
    pub priority: u32,
    pub snapshots: &'a [OrderBookSnapshot],
    pub execution_model: OrderBookExecutionModel,
    pub strategy: &'a mut dyn OrderBookStrategy,
}

pub(super) fn validate_market_data_tier(
    market: &OrderBookPortfolioMarket<'_>,
) -> Result<(), qx_core::QxError> {
    match market.data_tier {
        DataTier::L1
            if market
                .snapshots
                .iter()
                .any(|snapshot| !crate::orderbook::has_l1_shape(snapshot)) =>
        {
            Err(qx_core::QxError::BusinessViolation(format!(
                "共享 L1 Tick 市场 {} 必须在每份快照中恰有一档 bid/ask",
                market.instrument_spec.instrument
            )))
        }
        DataTier::L1 | DataTier::L2L3 => Ok(()),
        DataTier::Bar => Err(qx_core::QxError::BusinessViolation(
            "订单簿组合不能声明 Bar 数据档位".into(),
        )),
    }?;
    if market.execution_model.fee_bps < 0 {
        return Err(qx_core::QxError::BusinessViolation(
            "共享订单簿手续费不能为负".into(),
        ));
    }
    market
        .execution_model
        .validate()
        .map_err(qx_core::QxError::BusinessViolation)
}

pub(super) fn write_market_identity(
    hash: &mut Fnv1a,
    market: &OrderBookPortfolioMarket<'_>,
) -> Result<(), qx_core::QxError> {
    hash.write_text(&market.instrument_spec.instrument.to_string());
    hash.write_text(match market.data_tier {
        DataTier::L1 => "l1",
        DataTier::L2L3 => "l2",
        DataTier::Bar => "bar",
    });
    hash.write_u64(market.priority as u64);
    hash.write_text(
        &serde_json::to_string(&market.instrument_spec).map_err(|error| {
            qx_core::QxError::Invariant(format!("共享订单簿产品规格序列化失败: {error}"))
        })?,
    );
    market.execution_model.write_identity(hash);
    for snapshot in market.snapshots {
        hash.write_u64(snapshot.ts);
        hash.write_u64(snapshot.sequence);
        hash.write_u64(snapshot.bids.len() as u64);
        hash.write_u64(snapshot.asks.len() as u64);
        for level in snapshot.bids.iter().chain(snapshot.asks.iter()) {
            level.write_identity(hash);
        }
    }
    Ok(())
}

pub struct OrderBookPortfolioBacktestEngine<'a> {
    pub(super) account_id: String,
    pub(super) currency: String,
    pub(super) initial_cash_raw: i128,
    pub(super) initial_cash_exact: ExactDecimal,
    pub(super) exact_initial_cash_contract: bool,
    pub(super) risk: qx_zhenlu::RiskGate,
    pub(super) markets: Vec<OrderBookPortfolioMarket<'a>>,
    pub(super) funding_bps: BTreeMap<qx_core::InstrumentId, i64>,
    pub(super) funding_events: Vec<qx_core::FundingRateEvent>,
}

pub struct OrderBookPortfolioBacktestReport {
    pub fills: Vec<Fill>,
    pub fills_by_instrument: BTreeMap<qx_core::InstrumentId, Vec<Fill>>,
    pub execution_models: BTreeMap<qx_core::InstrumentId, OrderBookExecutionDescriptor>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub equity: Vec<(u64, i128)>,
    pub equity_exact: Vec<(u64, ExactDecimal)>,
    pub initial_equity_exact: ExactDecimal,
    pub pending_orders: usize,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub fees_exact_by_currency: BTreeMap<String, ExactDecimal>,
    pub turnover_exact: ExactDecimal,
    pub turnover_currency: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
}

impl OrderBookPortfolioBacktestReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn exact_metrics_json(&self) -> Result<serde_json::Value, String> {
        let values = self
            .equity_exact
            .iter()
            .map(|(_, equity)| *equity)
            .collect::<Vec<_>>();
        let mut metrics =
            super::super::report::exact_equity_metrics_json(self.initial_equity_exact, &values)?;
        metrics["turnover"] = serde_json::json!({
            "amount": self.turnover_exact.to_string(),
            "currency": self.turnover_currency,
        });
        metrics["fees_by_currency"] = serde_json::json!(self
            .fees_exact_by_currency
            .iter()
            .map(|(currency, amount)| (currency.clone(), amount.to_string()))
            .collect::<BTreeMap<_, _>>());
        metrics["legacy_raw_projection"] = serde_json::json!({
            "fees_raw": self.fees_raw.to_string(),
            "turnover_raw": self.turnover_raw.to_string(),
        });
        let mut funding_by_currency = BTreeMap::<String, ExactDecimal>::new();
        let mut funding_count = 0_u64;
        for entry in self
            .ledger
            .entries()
            .iter()
            .filter(|entry| entry.kind == qx_core::LedgerEntryKind::Funding)
        {
            funding_count = funding_count
                .checked_add(1)
                .ok_or_else(|| "资金费结算计数溢出".to_string())?;
            let amount = entry
                .exact_amounts
                .map_or_else(
                    || ExactDecimal::from_parts(entry.amount.raw(), 9),
                    |exact| Ok(exact.amount),
                )
                .map_err(|error| error.to_string())?;
            let total = funding_by_currency
                .entry(entry.currency.clone())
                .or_insert(ExactDecimal::ZERO);
            *total = total
                .checked_add(amount)
                .ok_or_else(|| "资金费现金流累计溢出".to_string())?;
        }
        metrics["funding"] = serde_json::json!({
            "settlement_count": funding_count,
            "cashflow_by_currency": funding_by_currency
                .iter()
                .map(|(currency, amount)| (currency.clone(), amount.to_string()))
                .collect::<BTreeMap<_, _>>(),
        });
        metrics["input_precision"] = serde_json::json!(
            "Shared-account multi-market DepthFrame v2 supports either spec-bound Spot base-quantity portfolios with exact cash/inventory reservations or homogeneous linear/inverse derivative portfolios with one settlement currency and aggregate exact margin/fee projection; derivative fills recheck actual-price margin, funding cashflows preserve exact quantity and mark values, queue estimation is disabled, and mixed Spot/derivative or cross-currency portfolios fail closed. v1 inputs keep the 1e-9 projection."
        );
        Ok(metrics)
    }

    pub fn verify(&self) -> Result<(), qx_core::QxError> {
        self.event_log.validate()?;
        qx_core::ReplayVerifier::verify(self.event_log.events(), &self.ledger)?;
        Ok(())
    }
}

pub(super) struct MarketRuntime<'a> {
    pub(super) instrument: qx_core::InstrumentId,
    pub(super) spec: TradingInstrumentSpec,
    pub(super) fee_bps: i128,
    pub(super) strategy: &'a mut dyn OrderBookStrategy,
    pub(super) matcher: OrderBookMatchingEngine,
    pub(super) reservations: SpotCashReservations,
    pub(super) exact_queue_estimation_enabled: bool,
    pub(super) cancellations_for_next_snapshot: Vec<u64>,
}

pub(super) fn total_reserved_cash(markets: &[MarketRuntime<'_>]) -> Result<i128, qx_core::QxError> {
    markets.iter().try_fold(0_i128, |total, market| {
        total
            .checked_add(market.reservations.total()?)
            .ok_or_else(|| qx_core::QxError::Invariant("共享盘口现金预留求和溢出".into()))
    })
}

pub(super) fn total_reserved_cash_exact(
    markets: &[MarketRuntime<'_>],
) -> Result<ExactDecimal, qx_core::QxError> {
    markets
        .iter()
        .try_fold(ExactDecimal::ZERO, |total, market| {
            total
                .checked_add(market.reservations.total_cash_exact()?)
                .ok_or_else(|| qx_core::QxError::Invariant("共享精确盘口现金预留求和溢出".into()))
        })
}

pub(super) fn reference_price(snapshot: &OrderBookSnapshot) -> Option<Price> {
    match (
        snapshot.bids.first().map(|level| level.price),
        snapshot.asks.first().map(|level| level.price),
    ) {
        (Some(bid), Some(ask)) => {
            let spread = ask.raw().checked_sub(bid.raw())?;
            Some(Price::from_raw(bid.raw().checked_add(spread / 2)?))
        }
        (Some(bid), None) => Some(bid),
        (None, Some(ask)) => Some(ask),
        (None, None) => None,
    }
}
