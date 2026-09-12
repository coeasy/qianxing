//! 订单与成交。
//!
//! 状态机区分"提交意图"与"venue 事实"：
//! `Submitted` 只是离开本地的意图，`Accepted` 才代表获得 venue 确认。

use crate::clock::Ts;
use crate::identity::InstrumentId;
use crate::numeric::{Money, Price, Quantity};
use crate::trading::OrderPolicy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum OrderStatus {
    PendingSubmit,
    Submitted,
    Accepted,
    Working,
    PartiallyFilled,
    Filled,
    CancelPending,
    Cancelled,
    Rejected,
    Expired,
    /// 连接中断或超时后的未知状态，必须进入对账，不能自动补单。
    Unknown,
}

impl OrderStatus {
    /// 是否为终态（不可再迁移）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use OrderStatus::*;
        if self == next {
            return !self.is_terminal();
        }
        matches!(
            (self, next),
            (PendingSubmit, Submitted | Rejected)
                | (Submitted, Accepted | CancelPending | Rejected | Unknown)
                | (Accepted, Working | CancelPending | Rejected | Unknown)
                | (
                    Working,
                    PartiallyFilled | Filled | CancelPending | Rejected | Expired | Unknown
                )
                | (
                    PartiallyFilled,
                    PartiallyFilled | Filled | CancelPending | Expired | Unknown
                )
                | (CancelPending, Cancelled | Filled | Unknown)
                | (
                    Unknown,
                    Accepted | Working | PartiallyFilled | Filled | Cancelled | Rejected | Expired
                )
        )
    }

    pub fn transition(&mut self, next: Self) -> Result<(), String> {
        if self.can_transition_to(next) {
            *self = next;
            Ok(())
        } else {
            Err(format!("非法订单状态迁移: {:?} -> {:?}", self, next))
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderTrace {
    pub strategy_id: Option<String>,
    pub signal_id: Option<u64>,
    pub intent_id: Option<u64>,
    pub rule_version: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Order {
    pub client_id: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Quantity,
    /// None 表示市价单。
    pub limit: Option<Price>,
    pub status: OrderStatus,
    pub filled: Quantity,
    pub account_id: String,
    pub trace: Option<OrderTrace>,
    /// 现货/保证金/永续/期货的执行策略；None 保持现货 Cash/1x 兼容语义。
    #[serde(default)]
    pub policy: Option<OrderPolicy>,
}

impl Order {
    pub fn validate(&self) -> Result<(), String> {
        if self.client_id == 0 {
            return Err("client_order_id 必须为正".into());
        }
        if self.qty.raw() <= 0 {
            return Err("订单数量必须为正".into());
        }
        if self.filled.raw() < 0 || self.filled.raw() > self.qty.raw() {
            return Err("订单已成交数量超出有效范围".into());
        }
        if self.limit.is_some_and(|price| price.raw() <= 0) {
            return Err("限价单价格必须为正".into());
        }
        if self.account_id.trim().is_empty() {
            return Err("订单账户不能为空".into());
        }
        if self.policy.is_some_and(|policy| policy.leverage == 0) {
            return Err("订单杠杆必须大于 0".into());
        }
        Ok(())
    }

    pub fn remaining(&self) -> Quantity {
        Quantity::from_raw(self.qty.raw().saturating_sub(self.filled.raw()).max(0))
    }

    pub fn trace_fill(
        &self,
        fill: &mut Fill,
        venue_id: Option<&str>,
        venue_order_id: Option<&str>,
    ) {
        if fill.account_id.is_empty() {
            fill.account_id = self.account_id.clone();
        }
        fill.venue_id = venue_id.map(str::to_string);
        fill.venue_order_id = venue_order_id.map(str::to_string);
        if let Some(trace) = &self.trace {
            fill.strategy_id = trace.strategy_id.clone();
            fill.signal_id = trace.signal_id;
            fill.intent_id = trace.intent_id;
            fill.rule_version = trace.rule_version.clone();
        }
    }
}

/// 成交：不可变事实。
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: u64,
    pub qty: Quantity,
    pub price: Price,
    pub fee: Money,
    pub ts: Ts,
    /// 成交归属账户。真实 Venue 回报必须填充；旧模拟路径允许为空并由 OMS 补齐。
    pub account_id: String,
    pub strategy_id: Option<String>,
    pub signal_id: Option<u64>,
    pub intent_id: Option<u64>,
    pub venue_id: Option<String>,
    pub venue_order_id: Option<String>,
    pub rule_version: Option<String>,
    pub fee_currency: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_decreases() {
        let o = Order {
            client_id: 1,
            instrument: InstrumentId::parse("X.Y").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(10),
            limit: None,
            status: OrderStatus::PartiallyFilled,
            filled: Quantity::from_i64(4),
            account_id: "a".into(),
            trace: None,
            policy: None,
        };
        assert_eq!(o.remaining().raw(), Quantity::from_i64(6).raw());
        assert!(!o.status.is_terminal());
    }

    #[test]
    fn unknown_requires_reconcile_before_recovery() {
        assert!(OrderStatus::Submitted.can_transition_to(OrderStatus::Unknown));
        assert!(OrderStatus::Unknown.can_transition_to(OrderStatus::Accepted));
        assert!(!OrderStatus::Filled.can_transition_to(OrderStatus::Working));
    }

    #[test]
    fn invalid_order_shape_is_rejected_before_submission() {
        let mut order = Order {
            client_id: 1,
            instrument: InstrumentId::parse("X.Y").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(10)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        };
        assert!(order.validate().is_ok());
        order.qty = Quantity::ZERO;
        assert!(order.validate().is_err());
    }
}
