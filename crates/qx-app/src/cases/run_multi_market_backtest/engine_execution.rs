//! Shared-account PortfolioBarEngine assembly, lifecycle and execution-window gate.

use super::super::run_multi_market_manifest::{take_runtime_market, PreparedMarket};
use super::MultiMarketExecutionWindow;
use crate::{AppError, AppErrorCategory, MultiMarketBacktestSpec};
use qx_core::{InstrumentId, Money};
use qx_xingban::{
    MarketRoutedBarStrategy, PortfolioBarEngine, PortfolioBarSessionConfig,
    PortfolioBarSessionReport,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;

pub(super) fn run_multi_market_bar_engine(
    spec: &MultiMarketBacktestSpec,
    prepared: &mut [PreparedMarket],
    routed_strategies: Vec<(
        InstrumentId,
        Box<dyn qx_xingban::BarStrategy>,
        Option<qx_core::OrderPolicy>,
    )>,
    execution_window: Option<MultiMarketExecutionWindow>,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<(PortfolioBarSessionReport, qx_core::ExactDecimal)>, AppError> {
    let runtime_markets = prepared
        .iter_mut()
        .map(take_runtime_market)
        .collect::<Result<Vec<_>, AppError>>()?;
    let strategy = MarketRoutedBarStrategy::new_with_order_policies(routed_strategies)
        .map_err(|error| AppError::from_qx_error(&error))?;
    let mut strategy: Box<dyn qx_xingban::BarStrategy> =
        match execution_window.and_then(|window| window.trade_start_ts) {
            Some(start) => Box::new(TradeStartGate {
                inner: strategy,
                start,
                last_callback_ts: 0,
            }),
            None => Box::new(strategy),
        };
    let funding_bps = spec
        .markets
        .iter()
        .filter(|market| market.funding_bps != 0)
        .map(|market| {
            let instrument = InstrumentId::parse(&market.instrument).ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "通过校验的资金费标的无法再次解析",
                )
            })?;
            Ok((instrument, market.funding_bps))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    let funding_events = spec
        .markets
        .iter()
        .flat_map(|market| market.funding_events.iter().cloned())
        .collect::<Vec<_>>();
    let initial_cash_exact = spec.initial_cash_exact_value()?;
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: spec.account_id.clone(),
            currency: spec.settlement_currency.clone(),
            initial_cash: Money::from_raw(spec.initial_cash_raw),
            initial_cash_exact: Some(initial_cash_exact),
            risk: RiskGate::conservative_default(),
            funding_bps,
            funding_events,
        },
        runtime_markets,
    )
    .map_err(|error| AppError::from_qx_error(&error))?;
    let report = super::super::strategy_lifecycle::run_bar_strategy_with_lifecycle(
        strategy.as_mut(),
        |strategy| {
            match cancellation {
                Some(token) => engine.run_with_cancel(strategy, || token.is_cancelled()),
                None => engine.run(strategy).map(Some),
            }
            .map_err(|error| AppError::from_qx_error(&error))
        },
    )?;
    Ok(report.map(|report| (report, initial_cash_exact)))
}

struct TradeStartGate {
    inner: MarketRoutedBarStrategy,
    start: u64,
    last_callback_ts: u64,
}

impl qx_xingban::BarStrategy for TradeStartGate {
    fn on_init(&mut self) -> Result<(), qx_core::QxError> {
        self.inner.on_init()
    }

    fn on_start(&mut self) -> Result<(), qx_core::QxError> {
        self.inner.on_start()
    }

    fn on_stop(&mut self) -> Result<(), qx_core::QxError> {
        self.inner.on_stop()
    }

    fn on_stop_with_reason(
        &mut self,
        reason: qx_strategy::StrategyStopReason,
    ) -> Result<(), qx_core::QxError> {
        self.inner.on_stop_with_reason(reason)
    }

    fn update_account_state(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        self.inner.update_account_state(cash, available_margin_raw);
    }

    fn update_account_state_exact(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
        cash_exact: &BTreeMap<String, qx_core::ExactDecimal>,
        available_margin_exact: Option<qx_core::ExactDecimal>,
    ) {
        self.inner.update_account_state_exact(
            cash,
            available_margin_raw,
            cash_exact,
            available_margin_exact,
        );
    }

    fn strategy_budget_raw(&self, strategy_id: &str) -> Option<i128> {
        self.inner.strategy_budget_raw(strategy_id)
    }

    fn strategy_budget_exact(&self, strategy_id: &str) -> Option<qx_core::ExactDecimal> {
        self.inner.strategy_budget_exact(strategy_id)
    }

    fn on_fill(
        &mut self,
        order: &qx_core::Order,
        fill: &qx_core::Fill,
    ) -> Result<(), qx_core::QxError> {
        self.inner.on_fill(order, fill)
    }

    fn on_bar_orders_checked(
        &mut self,
        history: &[qx_guanxing::Bar],
        instrument: &qx_core::InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<qx_core::Order>, qx_core::QxError> {
        self.on_bar_orders_with_margin_checked(history, instrument, ts, position, None)
    }

    fn on_bar_orders_with_margin_checked(
        &mut self,
        history: &[qx_guanxing::Bar],
        instrument: &qx_core::InstrumentId,
        ts: u64,
        position: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<qx_core::Order>, qx_core::QxError> {
        self.last_callback_ts = ts;
        let orders = self.inner.on_bar_orders_with_margin_checked(
            history,
            instrument,
            ts,
            position,
            available_margin_raw,
        )?;
        Ok(if ts < self.start { Vec::new() } else { orders })
    }

    fn drain_decision_facts(&mut self) -> Vec<qx_core::StrategyDecisionFact> {
        let facts = self.inner.drain_decision_facts();
        if self.last_callback_ts < self.start {
            Vec::new()
        } else {
            facts
        }
    }
}
