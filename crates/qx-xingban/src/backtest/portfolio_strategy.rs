//! Deterministic multi-strategy scheduling for a shared Bar ledger.

use super::{BarStrategy, NativeBarStrategy};
use qx_core::{Fill, InstrumentId, Order};
use qx_guanxing::Bar;
use qx_strategy::{Strategy, StrategyOrderStatusUpdate, StrategyStopReason};

pub struct PortfolioStrategySlot {
    pub priority: i32,
    pub strategy_id: String,
    pub reserved_capital_raw: i128,
    pub reserved_capital_exact: qx_core::ExactDecimal,
    strategy: Box<dyn BarStrategy>,
}

impl PortfolioStrategySlot {
    pub fn new(
        priority: i32,
        strategy_id: impl Into<String>,
        reserved_capital_raw: i128,
        strategy: impl BarStrategy + 'static,
    ) -> Self {
        Self {
            priority,
            strategy_id: strategy_id.into(),
            reserved_capital_raw,
            reserved_capital_exact: qx_core::ExactDecimal::from_parts(reserved_capital_raw, 9)
                .expect("legacy strategy budget fits its raw money representation"),
            strategy: Box::new(strategy),
        }
    }

    pub fn with_reserved_capital_exact(mut self, amount: qx_core::ExactDecimal) -> Self {
        self.reserved_capital_exact = amount;
        self
    }

    pub fn native<S: Strategy + 'static>(
        priority: i32,
        strategy_id: impl Into<String>,
        reserved_capital_raw: i128,
        strategy: NativeBarStrategy<S>,
    ) -> Self {
        Self::new(
            priority,
            strategy_id,
            reserved_capital_raw,
            strategy.with_virtual_position(),
        )
    }
}

/// Runs child decisions in declared priority then stable strategy-id order.
/// All resulting orders are still risk checked, matched and booked by one engine.
pub struct PortfolioBarStrategy {
    slots: Vec<PortfolioStrategySlot>,
}

impl PortfolioBarStrategy {
    pub fn new(mut slots: Vec<PortfolioStrategySlot>) -> Result<Self, qx_core::QxError> {
        if slots.is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "组合回测至少需要一个策略".into(),
            ));
        }
        for slot in &slots {
            let projected = slot
                .reserved_capital_exact
                .checked_mul_at_scale(
                    qx_core::ExactDecimal::ONE,
                    9,
                    qx_core::DecimalRounding::TowardZero,
                )
                .and_then(|value| value.to_raw(9).ok());
            if !valid_identity(&slot.strategy_id)
                || !slot.reserved_capital_exact.is_positive()
                || projected != Some(slot.reserved_capital_raw)
            {
                return Err(qx_core::QxError::BusinessViolation(
                    "组合策略 ID 必须合法且 reserved_capital_exact 必须为正并匹配 raw 投影".into(),
                ));
            }
        }
        slots.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.strategy_id.cmp(&right.strategy_id))
        });
        if slots
            .windows(2)
            .any(|pair| pair[0].strategy_id == pair[1].strategy_id)
        {
            return Err(qx_core::QxError::BusinessViolation(
                "组合策略 ID 不得重复".into(),
            ));
        }
        Ok(Self { slots })
    }

    fn route_fill(
        &mut self,
        order: &Order,
        fill: &Fill,
        event_seq: Option<u64>,
    ) -> Result<(), qx_core::QxError> {
        let owner = order
            .trace
            .as_ref()
            .and_then(|trace| trace.strategy_id.as_deref())
            .ok_or_else(|| {
                qx_core::QxError::ReconcileRequired(
                    "组合策略成交缺少 strategy_id，无法路由成交回调".into(),
                )
            })?;
        // Account-level protective exits are intentionally unavailable to a true
        // multi-strategy portfolio because their fills cannot be attributed to one
        // virtual position. A single-strategy backtest still uses this adapter for
        // a common execution path, so route its synthetic close to that sole owner.
        let strategy = if owner == "qx-protective-exit" && self.slots.len() == 1 {
            &mut self.slots[0].strategy
        } else {
            &mut self
                .slots
                .iter_mut()
                .find(|slot| slot.strategy_id == owner)
                .ok_or_else(|| {
                    qx_core::QxError::ReconcileRequired(format!(
                        "组合策略成交归属未知 strategy_id={owner}"
                    ))
                })?
                .strategy
        };
        match event_seq {
            Some(event_seq) => strategy.on_fill_with_event_seq(order, fill, event_seq),
            None => strategy.on_fill(order, fill),
        }
    }
}

impl BarStrategy for PortfolioBarStrategy {
    fn on_init(&mut self) -> Result<(), qx_core::QxError> {
        for slot in &mut self.slots {
            slot.strategy.on_init().map_err(|error| {
                qx_core::QxError::BusinessViolation(format!(
                    "组合策略 {} on_init 失败: {error}",
                    slot.strategy_id
                ))
            })?;
        }
        Ok(())
    }

    fn on_start(&mut self) -> Result<(), qx_core::QxError> {
        for slot in &mut self.slots {
            slot.strategy.on_start().map_err(|error| {
                qx_core::QxError::BusinessViolation(format!(
                    "组合策略 {} on_start 失败: {error}",
                    slot.strategy_id
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
        for slot in &mut self.slots {
            if let Err(error) = slot.strategy.on_stop_with_reason(reason) {
                if first_error.is_none() {
                    first_error = Some(qx_core::QxError::BusinessViolation(format!(
                        "组合策略 {} on_stop 失败: {error}",
                        slot.strategy_id
                    )));
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn update_account_state(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
    ) {
        for slot in &mut self.slots {
            let child_margin =
                available_margin_raw.map(|available| available.min(slot.reserved_capital_raw));
            slot.strategy.update_account_state(cash, child_margin);
        }
    }

    fn update_account_state_exact(
        &mut self,
        cash: &std::collections::BTreeMap<String, i128>,
        available_margin_raw: Option<i128>,
        cash_exact: &std::collections::BTreeMap<String, qx_core::ExactDecimal>,
        available_margin_exact: Option<qx_core::ExactDecimal>,
    ) {
        for slot in &mut self.slots {
            let child_margin_raw =
                available_margin_raw.map(|available| available.min(slot.reserved_capital_raw));
            let budget_exact = slot.reserved_capital_exact;
            let child_margin_exact = available_margin_exact.map(|available| {
                if available
                    .checked_cmp(budget_exact)
                    .is_some_and(|ordering| ordering.is_gt())
                {
                    budget_exact
                } else {
                    available
                }
            });
            slot.strategy.update_account_state_exact(
                cash,
                child_margin_raw,
                cash_exact,
                child_margin_exact,
            );
        }
    }

    fn strategy_budget_raw(&self, strategy_id: &str) -> Option<i128> {
        self.slots
            .iter()
            .find(|slot| slot.strategy_id == strategy_id)
            .map(|slot| slot.reserved_capital_raw)
    }

    fn strategy_budget_exact(&self, strategy_id: &str) -> Option<qx_core::ExactDecimal> {
        self.slots
            .iter()
            .find(|slot| slot.strategy_id == strategy_id)
            .map(|slot| slot.reserved_capital_exact)
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), qx_core::QxError> {
        self.route_fill(order, fill, None)
    }

    fn on_fill_with_event_seq(
        &mut self,
        order: &Order,
        fill: &Fill,
        event_seq: u64,
    ) -> Result<(), qx_core::QxError> {
        self.route_fill(order, fill, Some(event_seq))
    }

    fn on_order_status(
        &mut self,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<(), qx_core::QxError> {
        let owner = update.order.strategy_id.as_str();
        if owner == "qx-protective-exit" && self.slots.len() == 1 {
            return self.slots[0].strategy.on_order_status(update);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.strategy_id == owner)
            .ok_or_else(|| {
                qx_core::QxError::ReconcileRequired(format!(
                    "组合策略订单状态归属未知 strategy_id={owner}"
                ))
            })?;
        slot.strategy.on_order_status(update)
    }

    fn owns_order_status(&self, update: &StrategyOrderStatusUpdate) -> bool {
        let owner = update.order.strategy_id.as_str();
        (owner == "qx-protective-exit" && self.slots.len() == 1)
            || self.slots.iter().any(|slot| slot.strategy_id == owner)
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
        account_position: i128,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let mut orders = Vec::new();
        for slot in &mut self.slots {
            let child_budget =
                available_margin_raw.map(|available| available.min(slot.reserved_capital_raw));
            let mut child_orders = slot.strategy.on_bar_orders_with_margin_checked(
                history,
                instrument,
                ts,
                account_position,
                child_budget,
            )?;
            for order in &mut child_orders {
                let trace = order.trace.get_or_insert(qx_core::OrderTrace {
                    strategy_id: None,
                    signal_id: None,
                    intent_id: None,
                    rule_version: None,
                });
                if trace
                    .strategy_id
                    .as_deref()
                    .is_some_and(|strategy_id| strategy_id != slot.strategy_id)
                {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "组合策略订单归属与调度槽不一致: slot={}, order={}",
                        slot.strategy_id,
                        trace.strategy_id.as_deref().unwrap_or_default()
                    )));
                }
                trace.strategy_id = Some(slot.strategy_id.clone());
                if order.policy.unwrap_or_default().position_mode != qx_core::PositionMode::OneWay {
                    return Err(qx_core::QxError::BusinessViolation(
                        "组合策略预算目前要求 OneWay 持仓，Hedge 必须使用独立腿级预算".into(),
                    ));
                }
            }
            orders.extend(child_orders);
        }
        Ok(orders)
    }

    fn on_bar_orders_with_margin_exact_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        account_position: i128,
        position_exact: Option<qx_core::ExactDecimal>,
        available_margin_raw: Option<i128>,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let mut orders = Vec::new();
        for slot in &mut self.slots {
            let child_budget =
                available_margin_raw.map(|available| available.min(slot.reserved_capital_raw));
            let mut child_orders = slot.strategy.on_bar_orders_with_margin_exact_checked(
                history,
                instrument,
                ts,
                account_position,
                position_exact,
                child_budget,
            )?;
            for order in &mut child_orders {
                let trace = order.trace.get_or_insert(qx_core::OrderTrace {
                    strategy_id: None,
                    signal_id: None,
                    intent_id: None,
                    rule_version: None,
                });
                if trace
                    .strategy_id
                    .as_deref()
                    .is_some_and(|strategy_id| strategy_id != slot.strategy_id)
                {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "组合策略订单归属与调度槽不一致: slot={}, order={}",
                        slot.strategy_id,
                        trace.strategy_id.as_deref().unwrap_or_default()
                    )));
                }
                trace.strategy_id = Some(slot.strategy_id.clone());
                if order.policy.unwrap_or_default().position_mode != qx_core::PositionMode::OneWay {
                    return Err(qx_core::QxError::BusinessViolation(
                        "组合策略预算目前要求 OneWay 持仓，Hedge 必须使用独立腿级预算".into(),
                    ));
                }
            }
            orders.extend(child_orders);
        }
        Ok(orders)
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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

    #[test]
    fn portfolio_forwards_stop_reason_to_each_strategy_slot() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut portfolio = PortfolioBarStrategy::new(vec![
            PortfolioStrategySlot::new(
                0,
                "alpha",
                1_000_000_000,
                StopReasonProbe(Arc::clone(&seen)),
            ),
            PortfolioStrategySlot::new(
                1,
                "beta",
                1_000_000_000,
                StopReasonProbe(Arc::clone(&seen)),
            ),
        ])
        .unwrap();

        portfolio
            .on_stop_with_reason(StrategyStopReason::Failed)
            .unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![StrategyStopReason::Failed, StrategyStopReason::Failed]
        );
    }
}
