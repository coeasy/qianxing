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
        if fill.qty.raw() <= 0 || fill.qty.raw() > order.remaining().raw() {
            return Err(QxError::Invariant("成交数量超过订单剩余数量".into()));
        }
        if !fill.account_id.is_empty() && fill.account_id != order.account_id {
            return Err(QxError::Invariant("成交账户与 OMS 订单账户不一致".into()));
        }
        let mut next = order;
        if matches!(next.status, OrderStatus::Submitted | OrderStatus::Accepted) {
            next.status
                .transition(OrderStatus::Working)
                .map_err(QxError::Invariant)?;
        }
        next.filled = Quantity::from_raw(next.filled.raw() + fill.qty.raw());
        let status = if next.filled.raw() >= next.qty.raw() {
            OrderStatus::Filled
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

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    pub fn open_orders(&self) -> Vec<&Order> {
        self.orders
            .values()
            .filter(|order| !order.status.is_terminal())
            .collect()
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
    use qx_core::{InstrumentId, OrderTrace, Side};

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
        assert!(oms.open_orders().is_empty());
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
}
