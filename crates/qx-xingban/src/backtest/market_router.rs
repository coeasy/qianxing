//! Route a shared-account Bar portfolio to the strategy basket for each market.

use super::BarStrategy;
use qx_core::{Fill, InstrumentId, Order, OrderPolicy};
use qx_guanxing::Bar;
use qx_strategy::{StrategyOrderStatusUpdate, StrategyStopReason};
use std::collections::BTreeMap;

/// Dispatches each market event only to that market's strategy basket while keeping
/// fills and account-state refreshes on one shared portfolio boundary.
pub struct MarketRoutedBarStrategy {
    strategies: BTreeMap<InstrumentId, RoutedMarketStrategy>,
}

struct RoutedMarketStrategy {
    strategy: Box<dyn BarStrategy>,
    default_order_policy: Option<OrderPolicy>,
}

impl MarketRoutedBarStrategy {
    pub fn new(
        strategies: Vec<(InstrumentId, Box<dyn BarStrategy>)>,
    ) -> Result<Self, qx_core::QxError> {
        Self::new_with_order_policies(
            strategies
                .into_iter()
                .map(|(instrument, strategy)| (instrument, strategy, None))
                .collect(),
        )
    }

    /// Route each market to its strategy basket and attach its product policy to
    /// generated orders that do not already carry an explicit per-order policy.
    pub fn new_with_order_policies(
        strategies: Vec<(InstrumentId, Box<dyn BarStrategy>, Option<OrderPolicy>)>,
    ) -> Result<Self, qx_core::QxError> {
        if strategies.is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "市场策略路由至少需要一个市场".into(),
            ));
        }
        let mut routed = BTreeMap::new();
        for (instrument, strategy, default_order_policy) in strategies {
            if routed
                .insert(
                    instrument.clone(),
                    RoutedMarketStrategy {
                        strategy,
                        default_order_policy,
                    },
                )
                .is_some()
            {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "多市场策略路由存在重复标的 {instrument}"
                )));
            }
        }
        Ok(Self { strategies: routed })
    }
}

impl BarStrategy for MarketRoutedBarStrategy {
    fn on_init(&mut self) -> Result<(), qx_core::QxError> {
        for (instrument, route) in &mut self.strategies {
            route.strategy.on_init().map_err(|error| {
                qx_core::QxError::BusinessViolation(format!(
                    "市场 {instrument} 策略 on_init 失败: {error}"
                ))
            })?;
        }
        Ok(())
    }

    fn on_start(&mut self) -> Result<(), qx_core::QxError> {
        for (instrument, route) in &mut self.strategies {
            route.strategy.on_start().map_err(|error| {
                qx_core::QxError::BusinessViolation(format!(
                    "市场 {instrument} 策略 on_start 失败: {error}"
                ))
            })?;
        }
        Ok(())
    }

    fn on_stop(&mut self) -> Result<(), qx_core::QxError> {
        self.on_stop_with_reason(StrategyStopReason::HostStop)
    }

    fn on_stop_with_reason(&mut self, reason: StrategyStopReason) -> Result<(), qx_core::QxError> {
        let mut first_error = None;
        for (instrument, route) in &mut self.strategies {
            if let Err(error) = route.strategy.on_stop_with_reason(reason) {
                if first_error.is_none() {
                    first_error = Some(qx_core::QxError::BusinessViolation(format!(
                        "市场 {instrument} 策略 on_stop 失败: {error}"
                    )));
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn update_account_state(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        for route in self.strategies.values_mut() {
            route
                .strategy
                .update_account_state(cash, available_margin_raw);
        }
    }

    fn update_account_state_exact(
        &mut self,
        cash: &BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
        cash_exact: &BTreeMap<String, qx_core::ExactDecimal>,
        available_margin_exact: Option<qx_core::ExactDecimal>,
    ) {
        for route in self.strategies.values_mut() {
            route.strategy.update_account_state_exact(
                cash,
                available_margin_raw,
                cash_exact,
                available_margin_exact,
            );
        }
    }

    fn strategy_budget_raw(&self, strategy_id: &str) -> Option<i128> {
        self.strategies
            .values()
            .find_map(|route| route.strategy.strategy_budget_raw(strategy_id))
    }

    fn strategy_budget_exact(&self, strategy_id: &str) -> Option<qx_core::ExactDecimal> {
        self.strategies
            .values()
            .find_map(|route| route.strategy.strategy_budget_exact(strategy_id))
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        let strategy = self.strategies.get_mut(&order.instrument).ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "成交标的 {} 不属于多市场策略路由",
                order.instrument
            ))
        })?;
        strategy.strategy.on_fill(order, fill)
    }

    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        let strategy = self.strategies.get_mut(&order.instrument).ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "成交标的 {} 不属于多市场策略路由",
                order.instrument
            ))
        })?;
        strategy
            .strategy
            .on_fill_with_event_seq(order, fill, event_seq)
    }

    fn on_order_status(
        &mut self,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<(), qx_core::QxError> {
        // A strategy may place an order for a different routed instrument, so
        // ownership follows strategy identity rather than the order target market.
        let owners = self
            .strategies
            .iter()
            .filter(|(_, route)| route.strategy.owns_order_status(update))
            .map(|(instrument, _)| instrument.clone())
            .collect::<Vec<_>>();
        let owner = match owners.as_slice() {
            [instrument] => instrument,
            [] => {
                return Err(qx_core::QxError::ReconcileRequired(format!(
                    "多市场订单状态归属未知 strategy_id={}",
                    update.order.strategy_id
                )));
            }
            _ => {
                return Err(qx_core::QxError::ReconcileRequired(format!(
                    "多市场订单状态归属不唯一 strategy_id={}",
                    update.order.strategy_id
                )));
            }
        };
        self.strategies
            .get_mut(owner)
            .ok_or_else(|| qx_core::QxError::Invariant("策略归属路由在分发时消失".into()))?
            .strategy
            .on_order_status(update)?;
        Ok(())
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
        let mut orders = strategy.strategy.on_bar_orders_with_margin_checked(
            history,
            instrument,
            ts,
            position,
            available_margin_raw,
        )?;
        if let Some(policy) = strategy.default_order_policy {
            for order in &mut orders {
                order.policy.get_or_insert(policy);
            }
        }
        Ok(orders)
    }

    fn on_bar_orders_with_margin_exact_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
        position_exact: Option<qx_core::ExactDecimal>,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let strategy = self.strategies.get_mut(instrument).ok_or_else(|| {
            qx_core::QxError::BusinessViolation(format!(
                "Bar 事件标的 {instrument} 不属于多市场策略路由"
            ))
        })?;
        let mut orders = strategy.strategy.on_bar_orders_with_margin_exact_checked(
            history,
            instrument,
            ts,
            position,
            position_exact,
            available_margin_raw,
        )?;
        if let Some(policy) = strategy.default_order_policy {
            for order in &mut orders {
                order.policy.get_or_insert(policy);
            }
        }
        Ok(orders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    struct Counter(Arc<AtomicUsize>);

    struct OneOrder;

    struct StopReasonProbe(Arc<Mutex<Vec<StrategyStopReason>>>);

    impl BarStrategy for StopReasonProbe {
        fn on_bar_orders_checked(
            &mut self,
            _history: &[Bar],
            _instrument: &InstrumentId,
            _ts: u64,
            _position: i128,
        ) -> Result<Vec<Order>, qx_core::QxError> {
            Ok(Vec::new())
        }

        fn on_stop_with_reason(
            &mut self,
            reason: StrategyStopReason,
        ) -> Result<(), qx_core::QxError> {
            self.0.lock().unwrap().push(reason);
            Ok(())
        }
    }

    impl BarStrategy for OneOrder {
        fn on_bar_orders_checked(
            &mut self,
            _history: &[Bar],
            instrument: &InstrumentId,
            _ts: u64,
            _position: i128,
        ) -> Result<Vec<Order>, qx_core::QxError> {
            Ok(vec![Order {
                client_id: 1,
                instrument: instrument.clone(),
                side: qx_core::Side::Buy,
                qty: qx_core::Quantity::from_raw(1_000),
                limit: None,
                status: qx_core::OrderStatus::PendingSubmit,
                filled: qx_core::Quantity::from_raw(0),
                account_id: "test-account".into(),
                trace: None,
                policy: None,
                exact_amounts: None,
            }])
        }
    }

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

    #[test]
    fn market_route_applies_default_product_policy_to_strategy_orders() {
        let btc = instrument("BTCUSDT.BINANCE");
        let policy = OrderPolicy {
            margin_mode: qx_core::MarginMode::Isolated,
            leverage: 5,
            ..OrderPolicy::default()
        };
        let make = || Box::new(Counter(Arc::new(AtomicUsize::new(0)))) as Box<dyn BarStrategy>;
        let mut router = MarketRoutedBarStrategy::new_with_order_policies(vec![
            (btc.clone(), Box::new(OneOrder), Some(policy)),
            (instrument("ETHUSDT.BINANCE"), make(), None),
        ])
        .unwrap();

        let orders = router
            .on_bar_orders_checked(&[Bar::new(10, 100, 101, 99, 100, 1)], &btc, 20, 0)
            .unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].policy, Some(policy));
    }

    #[test]
    fn forwards_stop_reason_to_each_market_strategy() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut router = MarketRoutedBarStrategy::new(vec![(
            instrument("BTCUSDT.BINANCE"),
            Box::new(StopReasonProbe(Arc::clone(&seen))),
        )])
        .unwrap();

        router
            .on_stop_with_reason(StrategyStopReason::Cancelled)
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), vec![StrategyStopReason::Cancelled]);
    }
}
