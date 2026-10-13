//! Route a shared-account Bar portfolio to the strategy basket for each market.

use super::BarStrategy;
use qx_core::{Fill, InstrumentId, Order};
use qx_guanxing::Bar;
use std::collections::BTreeMap;

/// Dispatches each market event only to that market's strategy basket while keeping
/// fills and account-state refreshes on one shared portfolio boundary.
pub struct MarketRoutedBarStrategy {
    strategies: BTreeMap<InstrumentId, Box<dyn BarStrategy>>,
}

impl MarketRoutedBarStrategy {
    pub fn new(
        strategies: Vec<(InstrumentId, Box<dyn BarStrategy>)>,
    ) -> Result<Self, qx_core::QxError> {
        if strategies.len() < 2 {
            return Err(qx_core::QxError::BusinessViolation(
                "多市场策略路由至少需要两个市场".into(),
            ));
        }
        let mut routed = BTreeMap::new();
        for (instrument, strategy) in strategies {
            if routed.insert(instrument.clone(), strategy).is_some() {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "多市场策略路由存在重复标的 {instrument}"
                )));
            }
        }
        Ok(Self { strategies: routed })
    }
}

impl BarStrategy for MarketRoutedBarStrategy {
    fn update_account_state(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        for strategy in self.strategies.values_mut() {
            strategy.update_account_state(cash, available_margin_raw);
        }
    }

    fn strategy_budget_raw(&self, strategy_id: &str) -> Option<i128> {
        self.strategies
            .values()
            .find_map(|strategy| strategy.strategy_budget_raw(strategy_id))
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        let strategy = self.strategies.get_mut(&order.instrument).ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "成交标的 {} 不属于多市场策略路由",
                order.instrument
            ))
        })?;
        strategy.on_fill(order, fill)
    }

    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        self.on_bar_orders_with_margin_checked(history, instrument, ts, position, None)
    }

    fn on_bar_orders_with_margin_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let strategy = self.strategies.get_mut(instrument).ok_or_else(|| {
            qx_core::QxError::BusinessViolation(format!(
                "Bar 事件标的 {instrument} 不属于多市场策略路由"
            ))
        })?;
        strategy.on_bar_orders_with_margin_checked(
            history,
            instrument,
            ts,
            position,
            available_margin_raw,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Counter(Arc<AtomicUsize>);

    impl BarStrategy for Counter {
        fn on_bar_orders_checked(
            &mut self,
            _history: &[Bar],
            _instrument: &InstrumentId,
            _ts: u64,
            _position: i128,
        ) -> Result<Vec<Order>, qx_core::QxError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    fn instrument(value: &str) -> InstrumentId {
        InstrumentId::parse(value).unwrap()
    }

    #[test]
    fn routes_each_market_event_to_only_its_strategy_basket() {
        let btc_calls = Arc::new(AtomicUsize::new(0));
        let eth_calls = Arc::new(AtomicUsize::new(0));
        let mut router = MarketRoutedBarStrategy::new(vec![
            (
                instrument("BTCUSDT.BINANCE"),
                Box::new(Counter(Arc::clone(&btc_calls))),
            ),
            (
                instrument("ETHUSDT.KRAKEN"),
                Box::new(Counter(Arc::clone(&eth_calls))),
            ),
        ])
        .unwrap();
        let history = [Bar::new(10, 100, 101, 99, 100, 1)];
        router
            .on_bar_orders_checked(&history, &instrument("ETHUSDT.KRAKEN"), 20, 0)
            .unwrap();
        assert_eq!(btc_calls.load(Ordering::SeqCst), 0);
        assert_eq!(eth_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn rejects_duplicate_market_routes_and_unroutable_events() {
        let id = instrument("BTCUSDT.BINANCE");
        let make = || Box::new(Counter(Arc::new(AtomicUsize::new(0)))) as Box<dyn BarStrategy>;
        assert!(MarketRoutedBarStrategy::new(vec![(id.clone(), make()), (id, make())]).is_err());

        let mut router = MarketRoutedBarStrategy::new(vec![
            (instrument("BTCUSDT.BINANCE"), make()),
            (instrument("ETHUSDT.KRAKEN"), make()),
        ])
        .unwrap();
        assert!(router
            .on_bar_orders_checked(
                &[Bar::new(10, 100, 101, 99, 100, 1)],
                &instrument("SOLUSDT.BINANCE"),
                20,
                0,
            )
            .is_err());
    }
}
