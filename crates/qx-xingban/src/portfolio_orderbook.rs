//! Deterministic multi-strategy dispatch over the shared Tick/OrderBook engine.

use crate::backtest::portfolio_budget::PortfolioBudgetState;
use crate::cost::NoMargin;
use crate::{BookLevel, OrderBookActions, OrderBookSnapshot, OrderBookStrategy, TickStrategy};
use qx_core::{Fill, InstrumentId, Order, OrderTrace, QxError, TradingInstrumentSpec};
use qx_guanxing::QuoteTick;
use std::collections::{BTreeMap, BTreeSet};

struct OrderOwner {
    strategy_id: String,
    local_client_id: u64,
}

pub struct OrderBookStrategySlot {
    pub priority: i32,
    pub strategy_id: String,
    pub reserved_capital_raw: i128,
    strategy: Box<dyn OrderBookStrategy>,
}

impl OrderBookStrategySlot {
    pub fn new(
        priority: i32,
        strategy_id: impl Into<String>,
        reserved_capital_raw: i128,
        strategy: impl OrderBookStrategy + 'static,
    ) -> Self {
        Self::from_box(
            priority,
            strategy_id,
            reserved_capital_raw,
            Box::new(strategy),
        )
    }

    pub fn from_box(
        priority: i32,
        strategy_id: impl Into<String>,
        reserved_capital_raw: i128,
        strategy: Box<dyn OrderBookStrategy>,
    ) -> Self {
        Self {
            priority,
            strategy_id: strategy_id.into(),
            reserved_capital_raw,
            strategy,
        }
    }
}

/// Priority-ordered strategies share one OMS, matcher, risk gate and ledger.
/// Child positions and capital reservations remain strategy-owned.
pub struct PortfolioOrderBookStrategy {
    instrument: InstrumentId,
    slots: Vec<OrderBookStrategySlot>,
    budgets: PortfolioBudgetState,
    order_owners: BTreeMap<u64, OrderOwner>,
    next_client_id: u64,
}

impl PortfolioOrderBookStrategy {
    pub fn new(
        instrument: InstrumentId,
        mut slots: Vec<OrderBookStrategySlot>,
    ) -> Result<Self, QxError> {
        if slots.is_empty() {
            return Err(QxError::BusinessViolation(
                "订单簿策略组合至少需要一个策略".into(),
            ));
        }
        for slot in &slots {
            if !valid_identity(&slot.strategy_id) || slot.reserved_capital_raw <= 0 {
                return Err(QxError::BusinessViolation(
                    "订单簿组合策略 ID 必须合法且 reserved_capital_raw 必须为正".into(),
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
            return Err(QxError::BusinessViolation(
                "订单簿组合策略 ID 不得重复".into(),
            ));
        }
        Ok(Self {
            instrument,
            slots,
            budgets: PortfolioBudgetState::default(),
            order_owners: BTreeMap::new(),
            next_client_id: 1,
        })
    }

    fn budget_map(&self) -> BTreeMap<String, i128> {
        self.slots
            .iter()
            .map(|slot| (slot.strategy_id.clone(), slot.reserved_capital_raw))
            .collect()
    }

    fn owner(&self, client_id: u64) -> Result<&OrderOwner, QxError> {
        self.order_owners.get(&client_id).ok_or_else(|| {
            QxError::ReconcileRequired(format!(
                "订单簿组合找不到策略本地订单映射: client_id={client_id}"
            ))
        })
    }

    fn member_orders(&self, strategy_id: &str, open_order_ids: &[u64]) -> Vec<u64> {
        open_order_ids
            .iter()
            .filter_map(|client_id| {
                self.order_owners
                    .get(client_id)
                    .filter(|owner| owner.strategy_id == strategy_id)
                    .map(|owner| owner.local_client_id)
            })
            .collect()
    }

    fn allocate_order_id(&mut self) -> Result<u64, QxError> {
        let id = self.next_client_id;
        self.next_client_id = id
            .checked_add(1)
            .ok_or_else(|| QxError::Invariant("订单簿组合 client_id 溢出".into()))?;
        Ok(id)
    }
}

impl TickStrategy for PortfolioOrderBookStrategy {
    fn on_tick(&mut self, tick: &QuoteTick, account_position: i128) -> Result<Vec<Order>, String> {
        Ok(self.on_tick_actions(tick, account_position, &[])?.orders)
    }

    fn on_tick_actions(
        &mut self,
        tick: &QuoteTick,
        account_position: i128,
        open_order_ids: &[u64],
    ) -> Result<OrderBookActions, String> {
        let snapshot = OrderBookSnapshot {
            instrument: self.instrument.clone(),
            ts: tick.ts,
            sequence: tick.source_seq,
            bids: vec![BookLevel {
                price: tick.bid,
                qty: tick.bid_qty,
            }],
            asks: vec![BookLevel {
                price: tick.ask,
                qty: tick.ask_qty,
            }],
        };
        self.on_order_book_actions(&snapshot, account_position, open_order_ids)
    }

    fn on_tick_actions_with_margin(
        &mut self,
        tick: &QuoteTick,
        account_position: i128,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let snapshot = OrderBookSnapshot {
            instrument: self.instrument.clone(),
            ts: tick.ts,
            sequence: tick.source_seq,
            bids: vec![BookLevel {
                price: tick.bid,
                qty: tick.bid_qty,
            }],
            asks: vec![BookLevel {
                price: tick.ask,
                qty: tick.ask_qty,
            }],
        };
        self.on_order_book_actions_with_margin(
            &snapshot,
            account_position,
            open_order_ids,
            available_margin_raw,
        )
    }
}

impl OrderBookStrategy for PortfolioOrderBookStrategy {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _account_position: i128,
    ) -> Result<Vec<Order>, String> {
        self.on_order_book_actions(snapshot, 0, &[])
            .map(|actions| actions.orders)
    }

    fn on_order_book_actions(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _account_position: i128,
        open_order_ids: &[u64],
    ) -> Result<OrderBookActions, String> {
        self.on_order_book_actions_with_margin(snapshot, _account_position, open_order_ids, None)
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _account_position: i128,
        open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        let open = open_order_ids.iter().copied().collect::<BTreeSet<_>>();
        self.order_owners
            .retain(|client_id, _| open.contains(client_id));
        let mut actions = OrderBookActions::default();
        for index in 0..self.slots.len() {
            let strategy_id = self.slots[index].strategy_id.clone();
            let position = self.budgets.position(&strategy_id);
            let member_open_orders = self.member_orders(&strategy_id, open_order_ids);
            let member_budget = available_margin_raw
                .map(|available| available.min(self.slots[index].reserved_capital_raw));
            let member_actions = self.slots[index]
                .strategy
                .on_order_book_actions_with_margin(
                    snapshot,
                    position,
                    &member_open_orders,
                    member_budget,
                )?;
            let mut cancellations = BTreeSet::new();
            for local_id in member_actions.cancel_client_ids {
                if !cancellations.insert(local_id) {
                    return Err(format!(
                        "组合策略重复撤销本地订单: {strategy_id}/{local_id}"
                    ));
                }
                let global_id = self
                    .order_owners
                    .iter()
                    .find_map(|(global_id, owner)| {
                        (owner.strategy_id == strategy_id && owner.local_client_id == local_id)
                            .then_some(*global_id)
                    })
                    .ok_or_else(|| format!("组合策略撤销未知本地订单: {strategy_id}/{local_id}"))?;
                actions.cancel_client_ids.push(global_id);
            }
            for mut order in member_actions.orders {
                if order.client_id == 0 {
                    return Err(format!("组合策略本地 client_id 必须为正: {strategy_id}"));
                }
                if self.order_owners.values().any(|owner| {
                    owner.strategy_id == strategy_id && owner.local_client_id == order.client_id
                }) {
                    return Err(format!(
                        "组合策略复用了仍活动的本地 client_id: {strategy_id}/{}",
                        order.client_id
                    ));
                }
                if order.policy.unwrap_or_default().position_mode != qx_core::PositionMode::OneWay {
                    return Err("订单簿策略额度目前要求 OneWay 持仓模式".into());
                }
                let local_client_id = order.client_id;
                let global_client_id = self
                    .allocate_order_id()
                    .map_err(|error| error.to_string())?;
                let trace = order.trace.get_or_insert(OrderTrace {
                    strategy_id: None,
                    signal_id: None,
                    intent_id: None,
                    rule_version: None,
                });
                if trace
                    .strategy_id
                    .as_deref()
                    .is_some_and(|owner| owner != strategy_id)
                {
                    return Err(format!("组合策略订单归属与调度槽不一致: {strategy_id}"));
                }
                trace.strategy_id = Some(strategy_id.clone());
                order.client_id = global_client_id;
                self.order_owners.insert(
                    global_client_id,
                    OrderOwner {
                        strategy_id: strategy_id.clone(),
                        local_client_id,
                    },
                );
                actions.orders.push(order);
            }
        }
        Ok(actions)
    }

    fn check_order_budget(
        &self,
        order: &Order,
        reference_price: i128,
        instrument_spec: Option<&TradingInstrumentSpec>,
    ) -> Result<(), QxError> {
        let budgets = self.budget_map();
        let margin = NoMargin;
        let Some((budget, required)) = self.budgets.order_budget_check(
            |strategy_id| budgets.get(strategy_id).copied(),
            order,
            reference_price,
            instrument_spec,
            1,
            &margin,
        )?
        else {
            return Err(QxError::ReconcileRequired(
                "订单簿组合订单缺少已登记的策略身份".into(),
            ));
        };
        if required > budget {
            return Err(QxError::BusinessViolation(format!(
                "订单簿策略资金额度不足: required={required}, budget={budget}"
            )));
        }
        Ok(())
    }

    fn validate_fill_budget(
        &self,
        order: &Order,
        fill: &Fill,
        instrument_spec: Option<&TradingInstrumentSpec>,
    ) -> Result<(), QxError> {
        let budgets = self.budget_map();
        self.budgets.validate_fill_budget(
            |strategy_id| budgets.get(strategy_id).copied(),
            order,
            fill,
            instrument_spec,
            1,
            &NoMargin,
        )
    }

    fn on_order_accepted(
        &mut self,
        order: &Order,
        reference_price: Option<i128>,
    ) -> Result<(), QxError> {
        self.owner(order.client_id)?;
        let budget = self.budget_map();
        self.budgets.accept_if_owned(
            |strategy_id| budget.get(strategy_id).copied(),
            order,
            reference_price.ok_or_else(|| {
                QxError::ReconcileRequired("策略组合受限订单缺少保证金参考价".into())
            })?,
        )
    }

    fn on_order_cancelled(&mut self, order: &Order) -> Result<(), QxError> {
        let owner = self.owner(order.client_id)?;
        let strategy_id = owner.strategy_id.clone();
        let local_client_id = owner.local_client_id;
        let budget = self.budget_map();
        self.budgets
            .cancel_if_owned(|id| budget.get(id).copied(), order.client_id)?;
        self.order_owners.remove(&order.client_id);
        let index = self
            .slots
            .iter()
            .position(|slot| slot.strategy_id == strategy_id)
            .ok_or_else(|| QxError::ReconcileRequired("撤单策略槽已不存在".into()))?;
        let mut local_order = order.clone();
        local_order.client_id = local_client_id;
        self.slots[index].strategy.on_order_cancelled(&local_order)
    }

    fn on_fill(&mut self, order: &Order, fill: &Fill) -> Result<(), QxError> {
        let owner = self.owner(order.client_id)?;
        let strategy_id = owner.strategy_id.clone();
        let local_client_id = owner.local_client_id;
        let budgets = self.budget_map();
        self.budgets
            .apply_fill_if_owned(|id| budgets.get(id).copied(), order, fill)?;
        let index = self
            .slots
            .iter()
            .position(|slot| slot.strategy_id == strategy_id)
            .ok_or_else(|| QxError::ReconcileRequired("成交策略槽已不存在".into()))?;
        let mut local_order = order.clone();
        local_order.client_id = local_client_id;
        let mut local_fill = fill.clone();
        local_fill.order_id = local_client_id;
        self.slots[index]
            .strategy
            .on_fill(&local_order, &local_fill)?;
        if order.filled.raw().saturating_add(fill.qty.raw()) >= order.qty.raw() {
            self.order_owners.remove(&order.client_id);
        }
        Ok(())
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
#[path = "portfolio_orderbook/tests.rs"]
mod tests;
