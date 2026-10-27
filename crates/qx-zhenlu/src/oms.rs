//! 牵星订单管理系统（OMS）。原 `qx-oms` crate，V10 P2a 按"订单管理归针路"并入。
//!
//! OMS 只拥有本地订单状态和状态迁移，不调用 Venue、不写 Ledger；Venue 回报由
//! `apply_fill` 归约进来。Paper、CCXT 和其他执行器都通过同一个订单状态机。

use qx_core::{Fill, Order, OrderStatus, Quantity, QxError, QxResult};
use std::collections::BTreeMap;

/// 可重放的本地订单管理器。
///
/// 使用 `BTreeMap` 而非 `HashMap`，保证快照和回放的遍历顺序确定；OMS 不接受
/// 已完成订单的重复注册，也不允许成交绕过状态机直接改变订单状态。
#[derive(Clone)]
pub struct Oms {
    orders: BTreeMap<u64, Order>,
}

impl Default for Oms {
    fn default() -> Self {
        Self::new()
    }
}

impl Oms {
    pub fn new() -> Self {
        Self {
            orders: BTreeMap::new(),
        }
    }

    pub fn submit(&mut self, order: Order) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if !matches!(
            order.status,
            OrderStatus::PendingSubmit | OrderStatus::Submitted
        ) {
            return Err(QxError::BusinessViolation(
                "OMS 只接受待提交或已提交订单".into(),
            ));
        }
        if self.orders.contains_key(&order.client_id) {
            return Err(QxError::Invariant(format!(
                "重复的 client_order_id: {}",
                order.client_id
            )));
        }
        self.orders.insert(order.client_id, order);
        Ok(())
    }

    /// Venue 确认——`Submitted` 只是意图，`Accepted` 才是事实。
    pub fn accept(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::Permanent("订单不存在".into()))?;
        order
            .status
            .transition(OrderStatus::Accepted)
            .map_err(QxError::Invariant)?;
        Ok(())
    }

    /// Confirm a local simulated cancel through the same order state machine used by venues.
    pub fn cancel(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::Permanent("订单不存在".into()))?;
        if order.status.is_terminal() {
            return Err(QxError::BusinessViolation("终态订单不能撤销".into()));
        }
        if order.status != OrderStatus::CancelPending {
            order
                .status
                .transition(OrderStatus::CancelPending)
                .map_err(QxError::Invariant)?;
        }
        order
            .status
            .transition(OrderStatus::Cancelled)
            .map_err(QxError::Invariant)?;
        Ok(())
    }

    /// 只接受带账户身份的成交，成交数量必须落在订单剩余数量内。
    pub fn apply_fill(&mut self, fill: &Fill) -> QxResult<()> {
        let next = self.reduce_fill(fill)?;
        self.commit_fill(next);
        Ok(())
    }

    /// 在订单副本上校验成交并推进状态机，不改动任何持久状态。
    pub(crate) fn reduce_fill(&self, fill: &Fill) -> QxResult<Order> {
        let order = self
            .orders
            .get(&fill.order_id)
            .ok_or_else(|| QxError::Invariant("成交对应的订单不存在".into()))?
            .clone();
        if !fill.account_id.is_empty() && fill.account_id != order.account_id {
            return Err(QxError::Invariant("成交账户与 OMS 订单账户不一致".into()));
        }
        fill.validate_exact_amounts().map_err(QxError::Invariant)?;
        let mut next = order;
        if next.exact_amounts.is_some() || fill.exact_amounts.is_some() {
            let exact_fill = fill
                .exact_amounts
                .ok_or_else(|| QxError::Invariant("精确订单收到缺少精确数量/价格的成交".into()))?;
            let exact_order = next
                .exact_amounts
                .as_mut()
                .ok_or_else(|| QxError::Invariant("旧精度订单收到精确成交".into()))?;
            let quote_budget = exact_order.quote_budget;
            if exact_fill.quote_budget_complete && quote_budget.is_none() {
                return Err(QxError::Invariant(
                    "非 quote_budget 订单收到预算完成标记".into(),
                ));
            }
            if let Some(budget) = quote_budget {
                let cost = exact_fill
                    .quote_cost
                    .ok_or_else(|| QxError::Invariant("计价币预算订单成交缺少精确花费".into()))?;
                let remaining_budget = budget
                    .checked_sub(exact_order.quote_filled)
                    .ok_or_else(|| QxError::Invariant("计价币预算剩余量计算溢出".into()))?;
                if cost
                    .checked_cmp(remaining_budget)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::Invariant("成交花费超过计价币预算".into()));
                }
                exact_order.quote_filled = exact_order
                    .quote_filled
                    .checked_add(cost)
                    .ok_or_else(|| QxError::Invariant("计价币累计成交花费溢出".into()))?;
            } else {
                let remaining = exact_order
                    .quantity
                    .checked_sub(exact_order.filled)
                    .ok_or_else(|| QxError::Invariant("精确订单剩余数量计算溢出".into()))?;
                if exact_fill
                    .quantity
                    .checked_cmp(remaining)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::Invariant("成交数量超过订单剩余数量".into()));
                }
            }
            exact_order.filled = exact_order
                .filled
                .checked_add(exact_fill.quantity)
                .ok_or_else(|| QxError::Invariant("精确订单累计成交数量溢出".into()))?;
            if matches!(next.status, OrderStatus::Submitted | OrderStatus::Accepted) {
                next.status
                    .transition(OrderStatus::Working)
                    .map_err(QxError::Invariant)?;
            }
            let is_filled = quote_budget.map_or_else(
                || {
                    exact_order
                        .filled
                        .checked_cmp(exact_order.quantity)
                        .is_some_and(|ordering| ordering.is_ge())
                },
                |budget| {
                    exact_fill.quote_budget_complete
                        || exact_order
                            .quote_filled
                            .checked_cmp(budget)
                            .is_some_and(|ordering| ordering.is_ge())
                },
            );
            let status = if is_filled {
                OrderStatus::Filled
            } else if next.status == OrderStatus::CancelPending {
                // A fill can race a venue cancel acknowledgement. Keep the cancel
                // operation in flight until the venue confirms or rejects it.
                OrderStatus::CancelPending
            } else {
                OrderStatus::PartiallyFilled
            };
            next.status.transition(status).map_err(QxError::Invariant)?;
            return Ok(next);
        }
        if fill.qty.raw() <= 0 || fill.qty.raw() > next.remaining().raw() {
            return Err(QxError::Invariant("成交数量超过订单剩余数量".into()));
        }
        if matches!(next.status, OrderStatus::Submitted | OrderStatus::Accepted) {
            next.status
                .transition(OrderStatus::Working)
                .map_err(QxError::Invariant)?;
        }
        next.filled = Quantity::from_raw(next.filled.raw() + fill.qty.raw());
        let status = if next.filled.raw() >= next.qty.raw() {
            OrderStatus::Filled
        } else if next.status == OrderStatus::CancelPending {
            // Do not lose the durable cancel-in-flight state on a partial fill.
            OrderStatus::CancelPending
        } else {
            OrderStatus::PartiallyFilled
        };
        next.status.transition(status).map_err(QxError::Invariant)?;
        Ok(next)
    }

    /// 写回 `reduce_fill` 的产物。除内核成交归约接缝外，调用方应走 `apply_fill`。
    pub(crate) fn commit_fill(&mut self, order: Order) {
        self.orders.insert(order.client_id, order);
    }

    pub fn get(&self, client_order_id: u64) -> Option<&Order> {
        self.orders.get(&client_order_id)
    }

    /// Runtime EventLog 归约器需要在保留额外事实检查的同时更新 OMS；订单存储
    /// 仍然只存在于 OMS 内部，调用方不能替换或绕过 client id 索引。
    pub fn get_mut(&mut self, client_order_id: u64) -> Option<&mut Order> {
        self.orders.get_mut(&client_order_id)
    }

    pub fn insert_replayed(&mut self, order: Order) -> QxResult<()> {
        self.submit(order)
    }

    pub fn all_orders(&self) -> Vec<Order> {
        self.orders.values().cloned().collect()
    }

    /// Borrow all orders without cloning the OMS snapshot.
    pub fn all_orders_ref(&self) -> impl Iterator<Item = &Order> {
        self.orders.values()
    }

    pub fn active_order_ids(&self) -> Vec<u64> {
        self.orders
            .iter()
            .filter(|(_, order)| !order.status.is_terminal())
            .map(|(client_order_id, _)| *client_order_id)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}

/// 内核的成交归约接缝只通过这三个方法读写订单状态，避免 `qx-core` 反向依赖 OMS。
impl qx_core::OrderFillBook for Oms {
    fn order_state(&self, client_order_id: u64) -> Option<Order> {
        self.get(client_order_id).cloned()
    }

    fn reduce_fill(&self, fill: &Fill) -> QxResult<Order> {
        Oms::reduce_fill(self, fill)
    }

    fn commit_fill(&mut self, order: Order) {
        Oms::commit_fill(self, order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        ExactDecimal, ExactFillAmounts, ExactOrderAmounts, InstrumentId, OrderTrace, Side,
    };

    fn order() -> Order {
        Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTC/USDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(OrderTrace {
                strategy_id: Some("oms-test".into()),
                signal_id: Some(1),
                intent_id: Some(1),
                rule_version: Some("v1".into()),
            }),
            policy: None,

            exact_amounts: None,
        }
    }

    #[test]
    fn oms_is_idempotence_boundary_for_client_order_and_fills() {
        let mut oms = Oms::new();
        oms.submit(order()).unwrap();
        assert!(oms.submit(order()).is_err());
        oms.accept(1).unwrap();
        let mut fill = Fill {
            order_id: 1,
            qty: Quantity::from_i64(1),
            price: qx_core::Price::from_i64(100),
            account_id: "main".into(),
            ..Fill::default()
        };
        oms.apply_fill(&fill).unwrap();
        assert_eq!(oms.get(1).unwrap().status, OrderStatus::PartiallyFilled);
        fill.qty = Quantity::from_i64(1);
        oms.apply_fill(&fill).unwrap();
        assert_eq!(oms.get(1).unwrap().status, OrderStatus::Filled);
        assert!(oms
            .all_orders()
            .iter()
            .all(|order| order.status.is_terminal()));
    }

    #[test]
    fn partial_fill_during_cancel_pending_preserves_cancel_until_confirmation() {
        let mut oms = Oms::new();
        let mut pending = order();
        pending.status = OrderStatus::Submitted;
        oms.submit(pending).unwrap();
        oms.accept(1).unwrap();
        oms.get_mut(1).unwrap().status = OrderStatus::CancelPending;

        oms.apply_fill(&Fill {
            order_id: 1,
            qty: Quantity::from_i64(1),
            price: qx_core::Price::from_i64(100),
            account_id: "main".into(),
            ..Fill::default()
        })
        .unwrap();

        let after_fill = oms.get(1).unwrap();
        assert_eq!(after_fill.status, OrderStatus::CancelPending);
        assert_eq!(after_fill.filled, Quantity::from_i64(1));

        oms.cancel(1).unwrap();
        let after_cancel = oms.get(1).unwrap();
        assert_eq!(after_cancel.status, OrderStatus::Cancelled);
        assert_eq!(after_cancel.filled, Quantity::from_i64(1));
    }

    #[test]
    fn exact_partial_fill_during_cancel_pending_preserves_cancel_until_confirmation() {
        let mut oms = Oms::new();
        let mut pending = order();
        pending.qty = Quantity::ZERO;
        pending.status = OrderStatus::Submitted;
        pending.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000002").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        oms.submit(pending).unwrap();
        oms.accept(1).unwrap();
        oms.get_mut(1).unwrap().status = OrderStatus::CancelPending;

        oms.apply_fill(&Fill {
            order_id: 1,
            account_id: "main".into(),
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                price: ExactDecimal::parse("0.00000001").unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            ..Fill::default()
        })
        .unwrap();

        let after_fill = oms.get(1).unwrap();
        assert_eq!(after_fill.status, OrderStatus::CancelPending);
        assert_eq!(
            after_fill.filled_decimal(),
            ExactDecimal::parse("0.0000000001").unwrap()
        );

        oms.cancel(1).unwrap();
        let after_cancel = oms.get(1).unwrap();
        assert_eq!(after_cancel.status, OrderStatus::Cancelled);
        assert_eq!(
            after_cancel.filled_decimal(),
            ExactDecimal::parse("0.0000000001").unwrap()
        );
    }

    /// 状态机拒下的成交不能把订单留在"已加仓但未迁移状态"的半程上。
    #[test]
    fn rejected_fill_leaves_no_partial_order_state() {
        let mut oms = Oms::new();
        oms.submit(Order {
            status: OrderStatus::PendingSubmit,
            ..order()
        })
        .unwrap();
        let fill = Fill {
            order_id: 1,
            qty: Quantity::from_i64(1),
            price: qx_core::Price::from_i64(100),
            account_id: "main".into(),
            ..Fill::default()
        };
        assert!(oms.apply_fill(&fill).is_err());
        let stored = oms.get(1).unwrap();
        assert_eq!(stored.status, OrderStatus::PendingSubmit);
        assert_eq!(stored.filled, Quantity::ZERO);
    }

    #[test]
    fn oms_accumulates_exact_fill_quantity_without_legacy_scale_loss() {
        let mut oms = Oms::new();
        let mut precise_order = order();
        precise_order.qty = Quantity::ZERO;
        precise_order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000002").unwrap(),
            limit_price: None,
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        oms.submit(precise_order).unwrap();
        oms.accept(1).unwrap();

        for expected_status in [OrderStatus::PartiallyFilled, OrderStatus::Filled] {
            let fill = Fill {
                order_id: 1,
                account_id: "main".into(),
                exact_amounts: Some(ExactFillAmounts {
                    quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                    price: ExactDecimal::parse("0.00000001").unwrap(),
                    quote_cost: None,
                    quote_budget_complete: false,
                }),
                ..Fill::default()
            };
            oms.apply_fill(&fill).unwrap();
            let stored = oms.get(1).unwrap();
            assert_eq!(stored.status, expected_status);
        }
        assert_eq!(
            oms.get(1).unwrap().filled_decimal(),
            ExactDecimal::parse("0.0000000002").unwrap()
        );
    }

    #[test]
    fn oms_tracks_quote_budget_spend_while_accumulating_base_fills() {
        let mut oms = Oms::new();
        let mut budget_order = order();
        budget_order.qty = Quantity::ZERO;
        budget_order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::ZERO,
            limit_price: None,
            filled: ExactDecimal::ZERO,
            quote_budget: Some(ExactDecimal::parse("10").unwrap()),
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: None,
        });
        oms.submit(budget_order).unwrap();
        oms.accept(1).unwrap();

        for (quantity, price, cost, expected_status) in [
            ("0.5", "10", "5", OrderStatus::PartiallyFilled),
            ("0.25", "20", "5", OrderStatus::Filled),
        ] {
            oms.apply_fill(&Fill {
                order_id: 1,
                account_id: "main".into(),
                exact_amounts: Some(ExactFillAmounts {
                    quantity: ExactDecimal::parse(quantity).unwrap(),
                    price: ExactDecimal::parse(price).unwrap(),
                    quote_cost: Some(ExactDecimal::parse(cost).unwrap()),
                    quote_budget_complete: false,
                }),
                ..Fill::default()
            })
            .unwrap();
            assert_eq!(oms.get(1).unwrap().status, expected_status);
        }
        let amounts = oms.get(1).unwrap().exact_amounts.unwrap();
        assert_eq!(amounts.filled, ExactDecimal::parse("0.75").unwrap());
        assert_eq!(amounts.quote_filled, ExactDecimal::parse("10").unwrap());
    }

    #[test]
    fn oms_finishes_quote_budget_order_when_lot_step_leaves_unspent_dust() {
        let mut oms = Oms::new();
        let mut budget_order = order();
        budget_order.qty = Quantity::ZERO;
        budget_order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::ZERO,
            limit_price: None,
            filled: ExactDecimal::ZERO,
            quote_budget: Some(ExactDecimal::parse("10").unwrap()),
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: None,
        });
        oms.submit(budget_order).unwrap();
        oms.accept(1).unwrap();

        oms.apply_fill(&Fill {
            order_id: 1,
            account_id: "main".into(),
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("3.333").unwrap(),
                price: ExactDecimal::parse("3").unwrap(),
                quote_cost: Some(ExactDecimal::parse("9.999").unwrap()),
                quote_budget_complete: true,
            }),
            ..Fill::default()
        })
        .unwrap();

        assert_eq!(oms.get(1).unwrap().status, OrderStatus::Filled);
        assert_eq!(
            oms.get(1).unwrap().quote_filled_decimal(),
            ExactDecimal::parse("9.999").unwrap()
        );
    }

    #[test]
    fn oms_rejects_quote_budget_overrun_without_advancing_order() {
        let mut oms = Oms::new();
        let mut budget_order = order();
        budget_order.qty = Quantity::ZERO;
        budget_order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::ZERO,
            limit_price: None,
            filled: ExactDecimal::ZERO,
            quote_budget: Some(ExactDecimal::parse("10").unwrap()),
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: None,
        });
        oms.submit(budget_order).unwrap();
        oms.accept(1).unwrap();
        let result = oms.apply_fill(&Fill {
            order_id: 1,
            account_id: "main".into(),
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("1").unwrap(),
                price: ExactDecimal::parse("10.01").unwrap(),
                quote_cost: Some(ExactDecimal::parse("10.01").unwrap()),
                quote_budget_complete: false,
            }),
            ..Fill::default()
        });
        assert!(result.is_err());
        assert_eq!(oms.get(1).unwrap().filled_decimal(), ExactDecimal::ZERO);
        assert_eq!(
            oms.get(1).unwrap().quote_filled_decimal(),
            ExactDecimal::ZERO
        );
    }

    #[test]
    fn oms_accumulates_exact_fills_when_raw_alignment_would_overflow_before_normalization() {
        let mut oms = Oms::new();
        let mut precise_order = order();
        precise_order.qty = Quantity::ZERO;
        precise_order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("340282366920938463463.37460743176821145").unwrap(),
            limit_price: None,
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        oms.submit(precise_order).unwrap();
        oms.accept(1).unwrap();

        for quantity in [
            "170141183460469231731.687303715884105727",
            "170141183460469231731.687303715884105723",
        ] {
            let fill = Fill {
                order_id: 1,
                account_id: "main".into(),
                exact_amounts: Some(ExactFillAmounts {
                    quantity: ExactDecimal::parse(quantity).unwrap(),
                    price: ExactDecimal::parse("0.00000001").unwrap(),
                    quote_cost: None,
                    quote_budget_complete: false,
                }),
                ..Fill::default()
            };
            oms.apply_fill(&fill).unwrap();
        }

        let stored = oms.get(1).unwrap();
        assert_eq!(stored.status, OrderStatus::Filled);
        assert_eq!(
            stored.filled_decimal().to_string(),
            "340282366920938463463.37460743176821145"
        );
    }
}
