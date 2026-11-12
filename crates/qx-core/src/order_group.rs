//! Durable domain contract for linked protective orders.
//!
//! This module deliberately describes intent and state only. Venue adapters must
//! advertise and implement native group semantics before executing an intent; a
//! pair of ordinary orders is not an atomic OCO order.

use crate::{ConditionalOrderKind, Order, OrderStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const ORDER_GROUP_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderGroupKind {
    Oco,
    Bracket,
}

/// The caller must explicitly opt into synthetic behavior. Production adapters
/// should currently accept only `NativeRequired` unless a separately risk-limited
/// synthetic OCO implementation is available.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderGroupExecution {
    NativeRequired,
    SyntheticRiskAcknowledged,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderGroupLegRole {
    Entry,
    TakeProfit,
    StopLoss,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderGroupLegIntent {
    pub role: OrderGroupLegRole,
    pub order: Order,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderGroupIntent {
    pub schema_version: u16,
    pub group_id: String,
    pub kind: OrderGroupKind,
    pub execution: OrderGroupExecution,
    pub legs: Vec<OrderGroupLegIntent>,
}

impl OrderGroupIntent {
    pub fn new(
        group_id: impl Into<String>,
        kind: OrderGroupKind,
        execution: OrderGroupExecution,
        legs: Vec<OrderGroupLegIntent>,
    ) -> Result<Self, String> {
        let intent = Self {
            schema_version: ORDER_GROUP_SCHEMA_VERSION,
            group_id: group_id.into(),
            kind,
            execution,
            legs,
        };
        intent.validate()?;
        Ok(intent)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ORDER_GROUP_SCHEMA_VERSION {
            return Err(format!(
                "不支持的订单组 schema_version: {}",
                self.schema_version
            ));
        }
        if self.group_id.trim().is_empty() {
            return Err("订单组 group_id 不能为空".into());
        }
        let expected_roles: BTreeSet<_> = match self.kind {
            OrderGroupKind::Oco => [OrderGroupLegRole::TakeProfit, OrderGroupLegRole::StopLoss]
                .into_iter()
                .collect(),
            OrderGroupKind::Bracket => [
                OrderGroupLegRole::Entry,
                OrderGroupLegRole::TakeProfit,
                OrderGroupLegRole::StopLoss,
            ]
            .into_iter()
            .collect(),
        };
        let actual_roles: BTreeSet<_> = self.legs.iter().map(|leg| leg.role).collect();
        if actual_roles.len() != self.legs.len() || actual_roles != expected_roles {
            return Err(format!(
                "{:?} 订单组必须包含且仅包含 {:?} 腿",
                self.kind, expected_roles
            ));
        }
        let Some(first) = self.legs.first() else {
            return Err("订单组不能为空".into());
        };
        let role_order = |role| {
            self.legs
                .iter()
                .find(|leg| leg.role == role)
                .map(|leg| &leg.order)
        };
        let take_profit = role_order(OrderGroupLegRole::TakeProfit)
            .ok_or_else(|| "订单组缺少止盈腿".to_string())?;
        let stop_loss = role_order(OrderGroupLegRole::StopLoss)
            .ok_or_else(|| "订单组缺少止损腿".to_string())?;
        if take_profit.side != stop_loss.side {
            return Err("止盈腿与止损腿必须使用相同方向".into());
        }
        if self.kind == OrderGroupKind::Bracket {
            let entry = role_order(OrderGroupLegRole::Entry)
                .ok_or_else(|| "Bracket 订单组缺少入场腿".to_string())?;
            if entry.side == take_profit.side {
                return Err("Bracket 保护腿方向必须与入场腿相反".into());
            }
            if entry
                .exact_amounts
                .and_then(|amounts| amounts.conditional)
                .is_some()
            {
                return Err("Bracket 入场腿不能是条件保护单".into());
            }
        }
        for (order, expected) in [
            (take_profit, ConditionalOrderKind::TakeProfit),
            (stop_loss, ConditionalOrderKind::StopLoss),
        ] {
            if order
                .exact_amounts
                .and_then(|amounts| amounts.conditional)
                .map(|conditional| conditional.kind)
                != Some(expected)
            {
                return Err(format!(
                    "订单组 {:?} 腿必须显式携带对应条件单意图",
                    expected
                ));
            }
        }
        let mut client_ids = BTreeSet::new();
        for leg in &self.legs {
            leg.order.validate()?;
            if !client_ids.insert(leg.order.client_id) {
                return Err("订单组子腿 client_order_id 必须唯一".into());
            }
            if leg.order.status != OrderStatus::PendingSubmit
                || leg.order.filled.raw() != 0
                || leg
                    .order
                    .exact_amounts
                    .is_some_and(|amounts| !amounts.filled.is_zero())
            {
                return Err("订单组初始腿必须为未成交的 PendingSubmit 订单".into());
            }
            if leg.order.account_id != first.order.account_id
                || leg.order.instrument != first.order.instrument
            {
                return Err("订单组子腿必须属于同一账户和交易标的".into());
            }
            if !same_order_quantity(&leg.order, &first.order) {
                return Err("订单组各腿数量必须一致".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderGroupStatus {
    Planned,
    Submitting,
    Active,
    PartiallyFilled,
    PeerCancelRequired,
    ReconcileRequired,
    Filled,
    Cancelled,
    Rejected,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderGroupLegState {
    pub role: OrderGroupLegRole,
    pub order: Order,
    pub venue_order_id: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderGroupState {
    pub schema_version: u16,
    pub intent: OrderGroupIntent,
    pub status: OrderGroupStatus,
    pub legs: Vec<OrderGroupLegState>,
}

impl OrderGroupState {
    pub fn from_intent(intent: OrderGroupIntent) -> Result<Self, String> {
        intent.validate()?;
        let legs = intent
            .legs
            .iter()
            .map(|leg| OrderGroupLegState {
                role: leg.role,
                order: leg.order.clone(),
                venue_order_id: None,
            })
            .collect();
        Ok(Self {
            schema_version: ORDER_GROUP_SCHEMA_VERSION,
            intent,
            status: OrderGroupStatus::Planned,
            legs,
        })
    }

    /// Durably fence the one allowed submit attempt. Once a group reached
    /// `Submitting`, a restarted worker must reconcile it instead of replaying
    /// the venue side effect.
    pub fn begin_submission(&mut self) -> Result<(), String> {
        if self.status != OrderGroupStatus::Planned {
            return Err(format!("订单组 {:?} 不允许再次启动提交", self.status));
        }
        self.status = OrderGroupStatus::Submitting;
        Ok(())
    }

    pub fn require_reconciliation(&mut self) {
        self.status = OrderGroupStatus::ReconcileRequired;
    }

    pub fn validate_persisted(&self) -> Result<(), String> {
        if self.schema_version != ORDER_GROUP_SCHEMA_VERSION {
            return Err(format!(
                "不支持的订单组状态 schema_version: {}",
                self.schema_version
            ));
        }
        self.intent.validate()?;
        if self.legs.len() != self.intent.legs.len() {
            return Err("持久订单组腿数量与原始意图不一致".into());
        }
        for intended in &self.intent.legs {
            let state = self
                .legs
                .iter()
                .find(|leg| leg.role == intended.role)
                .ok_or_else(|| format!("持久订单组缺少 {:?} 腿", intended.role))?;
            state.order.validate()?;
            if !state.order.same_submission_identity(&intended.order) {
                return Err(format!("持久订单组 {:?} 腿与原始意图不一致", intended.role));
            }
            if state
                .venue_order_id
                .as_deref()
                .is_some_and(|id| id.trim().is_empty())
            {
                return Err(format!(
                    "持久订单组 {:?} 腿含空 venue_order_id",
                    intended.role
                ));
            }
        }
        Ok(())
    }

    /// Applies a confirmed venue query after an Unknown result. Reconciliation is
    /// allowed to clear the group barrier only when every leg is known and OCO
    /// fills do not imply a double execution.
    pub fn reconcile_order_snapshot(
        &mut self,
        snapshot: Order,
        venue_order_id: Option<String>,
    ) -> Result<(), String> {
        if self.status != OrderGroupStatus::ReconcileRequired {
            return Err("订单组当前不需要对账".into());
        }
        if snapshot.status == OrderStatus::Unknown {
            return Err("对账快照仍为 Unknown，订单组继续保持待对账".into());
        }
        let previous = self.clone();
        self.status = OrderGroupStatus::Submitting;
        if let Err(error) = self.apply_order_snapshot(snapshot, venue_order_id) {
            *self = previous;
            return Err(error);
        }
        if self
            .legs
            .iter()
            .any(|leg| leg.order.status == OrderStatus::Unknown)
        {
            self.status = OrderGroupStatus::ReconcileRequired;
        }
        Ok(())
    }

    /// Persist a known per-leg fact while another leg still keeps the group
    /// behind its reconciliation barrier. This does not resolve the barrier;
    /// only `reconcile_order_snapshot` may do that after an authoritative query.
    pub fn record_snapshot_during_reconciliation(
        &mut self,
        snapshot: Order,
        venue_order_id: Option<String>,
    ) -> Result<(), String> {
        if self.status != OrderGroupStatus::ReconcileRequired {
            return Err("订单组当前不处于待对账状态".into());
        }
        let previous = self.clone();
        self.status = OrderGroupStatus::Submitting;
        if let Err(error) = self.apply_order_snapshot(snapshot, venue_order_id) {
            *self = previous;
            return Err(error);
        }
        self.status = OrderGroupStatus::ReconcileRequired;
        Ok(())
    }

    /// Applies an authoritative OMS snapshot. Unknown outcomes stop automatic
    /// progress; an OCO fill requires cancelling its peer before the group settles.
    pub fn apply_order_snapshot(
        &mut self,
        snapshot: Order,
        venue_order_id: Option<String>,
    ) -> Result<(), String> {
        if self.status == OrderGroupStatus::ReconcileRequired {
            return Err("需先完成对账才能继续更新订单组".into());
        }
        let leg = self
            .legs
            .iter_mut()
            .find(|leg| leg.order.client_id == snapshot.client_id)
            .ok_or_else(|| format!("订单组不存在子腿 {}", snapshot.client_id))?;
        if !leg.order.same_submission_identity(&snapshot) {
            return Err("订单组子腿快照的订单身份与原始意图不一致".into());
        }
        if let Some(current) = leg.venue_order_id.as_deref() {
            if venue_order_id
                .as_deref()
                .is_some_and(|next| current != next)
            {
                return Err("订单组子腿 venue_order_id 与已确认 ID 冲突".into());
            }
        }
        if !leg.order.status.can_transition_to(snapshot.status)
            || snapshot.filled.raw() < leg.order.filled.raw()
            || snapshot
                .exact_amounts
                .zip(leg.order.exact_amounts)
                .is_some_and(|(next, prior)| {
                    next.filled
                        .checked_cmp(prior.filled)
                        .is_none_or(|v| v.is_lt())
                })
        {
            return Err("订单组子腿状态或累计成交量倒退".into());
        }
        if snapshot.status == OrderStatus::Unknown {
            self.status = OrderGroupStatus::ReconcileRequired;
        }
        leg.order = snapshot;
        if let Some(id) = venue_order_id {
            if id.trim().is_empty() {
                return Err("venue_order_id 不能为空".into());
            }
            leg.venue_order_id = Some(id);
        }
        self.recompute_status();
        Ok(())
    }

    fn recompute_status(&mut self) {
        if self.status == OrderGroupStatus::ReconcileRequired {
            return;
        }
        if self.intent.kind == OrderGroupKind::Oco {
            let filled_legs: Vec<_> = self
                .legs
                .iter()
                .filter(|leg| {
                    leg.order.filled.raw() > 0
                        || leg
                            .order
                            .exact_amounts
                            .is_some_and(|amounts| amounts.filled.is_positive())
                })
                .collect();
            if filled_legs.len() > 1 {
                self.status = OrderGroupStatus::ReconcileRequired;
                return;
            }
            if let Some(filled_leg) = filled_legs.first() {
                let peer = self
                    .legs
                    .iter()
                    .find(|leg| leg.order.client_id != filled_leg.order.client_id);
                if peer.is_some_and(|leg| leg.order.status == OrderStatus::Cancelled)
                    && filled_leg.order.status == OrderStatus::Filled
                {
                    self.status = OrderGroupStatus::Filled;
                } else {
                    self.status = OrderGroupStatus::PeerCancelRequired;
                }
                return;
            }
        }
        if self
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Filled)
        {
            self.status = OrderGroupStatus::Filled;
        } else if self
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Cancelled)
        {
            self.status = OrderGroupStatus::Cancelled;
        } else if self
            .legs
            .iter()
            .any(|leg| leg.order.status == OrderStatus::Rejected)
        {
            self.status = OrderGroupStatus::Rejected;
        } else if self
            .legs
            .iter()
            .any(|leg| leg.order.status == OrderStatus::PartiallyFilled)
        {
            self.status = OrderGroupStatus::PartiallyFilled;
        } else if self.legs.iter().all(|leg| {
            matches!(
                leg.order.status,
                OrderStatus::Accepted | OrderStatus::Working
            )
        }) {
            self.status = OrderGroupStatus::Active;
        } else if self
            .legs
            .iter()
            .any(|leg| leg.order.status != OrderStatus::PendingSubmit)
        {
            self.status = OrderGroupStatus::Submitting;
        }
    }
}

fn same_order_quantity(left: &Order, right: &Order) -> bool {
    match (left.exact_amounts, right.exact_amounts) {
        (Some(left), Some(right)) => left.quantity == right.quantity,
        (None, None) => left.qty == right.qty,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConditionalOrderIntent, ExactDecimal, ExactOrderAmounts, InstrumentId, Quantity, Side,
        TriggerPriceType,
    };

    fn protection_order(client_id: u64, kind: ConditionalOrderKind) -> Order {
        let trigger = match kind {
            ConditionalOrderKind::TakeProfit => "70000",
            ConditionalOrderKind::StopLoss => "60000",
            _ => unreachable!(),
        };
        Order {
            client_id,
            instrument: InstrumentId::parse("BTC/USDT.OKX").unwrap(),
            side: Side::Sell,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.01").unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
                conditional: Some(ConditionalOrderIntent {
                    kind,
                    trigger_price: Some(ExactDecimal::parse(trigger).unwrap()),
                    trigger_price_type: Some(TriggerPriceType::Mark),
                    trailing_percent: None,
                    trailing_amount: None,
                    trailing_trigger_price: None,
                }),
            }),
        }
    }

    fn oco_state() -> OrderGroupState {
        OrderGroupState::from_intent(
            OrderGroupIntent::new(
                "oco-1",
                OrderGroupKind::Oco,
                OrderGroupExecution::NativeRequired,
                vec![
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::TakeProfit,
                        order: protection_order(1, ConditionalOrderKind::TakeProfit),
                    },
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::StopLoss,
                        order: protection_order(2, ConditionalOrderKind::StopLoss),
                    },
                ],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn snapshot(
        state: &OrderGroupState,
        client_id: u64,
        status: OrderStatus,
        filled: &str,
    ) -> Order {
        let mut order = state
            .legs
            .iter()
            .find(|leg| leg.order.client_id == client_id)
            .unwrap()
            .order
            .clone();
        order.status = status;
        order.exact_amounts.as_mut().unwrap().filled = ExactDecimal::parse(filled).unwrap();
        order
    }

    #[test]
    fn oco_partial_fill_keeps_peer_cancel_required_until_peer_is_cancelled() {
        let mut group = oco_state();
        for id in [1, 2] {
            group
                .apply_order_snapshot(snapshot(&group, id, OrderStatus::Submitted, "0"), None)
                .unwrap();
            group
                .apply_order_snapshot(
                    snapshot(&group, id, OrderStatus::Accepted, "0"),
                    Some(format!("venue-{id}")),
                )
                .unwrap();
        }
        for id in [1, 2] {
            group
                .apply_order_snapshot(snapshot(&group, id, OrderStatus::Working, "0"), None)
                .unwrap();
        }
        group
            .apply_order_snapshot(
                snapshot(&group, 1, OrderStatus::PartiallyFilled, "0.004"),
                None,
            )
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::PeerCancelRequired);
        group
            .apply_order_snapshot(snapshot(&group, 2, OrderStatus::CancelPending, "0"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::PeerCancelRequired);
        group
            .apply_order_snapshot(snapshot(&group, 2, OrderStatus::Cancelled, "0"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::PeerCancelRequired);
        group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Filled, "0.01"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::Filled);
        group.validate_persisted().unwrap();
    }

    #[test]
    fn dual_oco_fills_require_reconciliation_and_unknown_requires_query_snapshot() {
        let mut group = oco_state();
        group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Unknown, "0"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        assert!(group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Accepted, "0"), None)
            .is_err());
        group
            .reconcile_order_snapshot(
                snapshot(&group, 1, OrderStatus::Accepted, "0"),
                Some("venue-1".into()),
            )
            .unwrap();
        assert_ne!(group.status, OrderGroupStatus::ReconcileRequired);

        group
            .apply_order_snapshot(snapshot(&group, 2, OrderStatus::Submitted, "0"), None)
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 2, OrderStatus::Accepted, "0"),
                Some("venue-2".into()),
            )
            .unwrap();
        for id in [1, 2] {
            group
                .apply_order_snapshot(snapshot(&group, id, OrderStatus::Working, "0"), None)
                .unwrap();
        }
        group
            .apply_order_snapshot(
                snapshot(&group, 1, OrderStatus::PartiallyFilled, "0.002"),
                None,
            )
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 2, OrderStatus::PartiallyFilled, "0.001"),
                None,
            )
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        group
            .record_snapshot_during_reconciliation(
                snapshot(&group, 2, OrderStatus::PartiallyFilled, "0.001"),
                Some("venue-2".into()),
            )
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        assert_eq!(group.legs[1].venue_order_id.as_deref(), Some("venue-2"));
        group
            .reconcile_order_snapshot(snapshot(&group, 1, OrderStatus::Filled, "0.01"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
    }

    #[test]
    fn order_group_snapshot_round_trips_with_schema_and_identity_validation() {
        let group = oco_state();
        let encoded = serde_json::to_vec(&group).unwrap();
        let decoded: OrderGroupState = serde_json::from_slice(&encoded).unwrap();
        decoded.validate_persisted().unwrap();
        assert_eq!(decoded, group);
    }
}
