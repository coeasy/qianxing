//! Durable domain contract for linked protective orders.
//!
//! This module deliberately describes intent and state only. Venue adapters must
//! advertise and implement native group semantics before executing an intent; a
//! pair of ordinary orders is not an atomic OCO order.

use crate::{ConditionalOrderKind, ExactDecimal, Order, OrderStatus};
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
        let take_profit_kind = take_profit
            .exact_amounts
            .and_then(|amounts| amounts.conditional)
            .map(|conditional| conditional.kind);
        let stop_loss_kind = stop_loss
            .exact_amounts
            .and_then(|amounts| amounts.conditional)
            .map(|conditional| conditional.kind);
        let conditional_pair = take_profit_kind == Some(ConditionalOrderKind::TakeProfit)
            && stop_loss_kind == Some(ConditionalOrderKind::StopLoss);
        // Some native OCO contracts (Bitget UTA Spot/Margin) pair a resting TP limit
        // with a conditional SL. Preserve that resource shape in the shared intent;
        // venue adapters still decide whether their own product supports it.
        let limit_take_profit_pair = self.kind == OrderGroupKind::Oco
            && take_profit_kind.is_none()
            && take_profit.limit_price_decimal().is_some()
            && stop_loss_kind == Some(ConditionalOrderKind::StopLoss);
        if !conditional_pair && !limit_take_profit_pair {
            return Err("订单组止盈/止损腿必须是对应条件单，或 OCO 限价止盈加条件止损".into());
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
    CancelPending,
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
    #[serde(default)]
    pub venue_group_id: Option<String>,
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
            venue_group_id: None,
            legs,
        })
    }

    pub fn record_venue_group_id(&mut self, venue_group_id: String) -> Result<(), String> {
        let venue_group_id = venue_group_id.trim();
        if venue_group_id.is_empty() {
            return Err("venue_group_id 不能为空".into());
        }
        if self
            .venue_group_id
            .as_deref()
            .is_some_and(|existing| existing != venue_group_id)
        {
            return Err("订单组 venue_group_id 与已确认 ID 冲突".into());
        }
        self.venue_group_id = Some(venue_group_id.to_string());
        Ok(())
    }

    /// Mark a venue-native group active before it has created ordinary child orders.
    /// A group resource ID must never be copied into the child order IDs.
    pub fn activate_native_group(&mut self) -> Result<(), String> {
        if self.status != OrderGroupStatus::Submitting {
            return Err(format!(
                "订单组 {:?} 不能从当前状态激活原生组资源",
                self.status
            ));
        }
        if self.venue_group_id.as_deref().is_none_or(str::is_empty) {
            return Err("原生组激活前必须先保存 venue_group_id".into());
        }
        if self.legs.iter().any(|leg| {
            !matches!(
                leg.order.status,
                OrderStatus::PendingSubmit | OrderStatus::Submitted
            ) || leg.order.filled.raw() != 0
                || leg
                    .order
                    .exact_amounts
                    .is_some_and(|amounts| amounts.filled.is_positive())
                || leg.venue_order_id.is_some()
        }) {
            return Err("原生组激活只允许尚未生成子订单的未成交腿".into());
        }
        self.status = OrderGroupStatus::Active;
        Ok(())
    }

    /// Durably fence cancellation of a native group resource before the venue RPC.
    /// Only an untriggered group with no child order IDs can use this operation.
    pub fn begin_native_cancel(&mut self) -> Result<(), String> {
        if self.status != OrderGroupStatus::Active {
            return Err(format!("订单组 {:?} 当前不能发起原生组撤销", self.status));
        }
        if self.venue_group_id.as_deref().is_none_or(str::is_empty) {
            return Err("原生组撤销前必须有 venue_group_id".into());
        }
        if self.legs.iter().any(|leg| {
            leg.venue_order_id.is_some()
                || leg.order.filled.raw() != 0
                || leg
                    .order
                    .exact_amounts
                    .is_some_and(|amounts| amounts.filled.is_positive())
        }) {
            return Err("原生组已有子订单身份或成交，必须先按子单事实对账".into());
        }
        self.status = OrderGroupStatus::CancelPending;
        Ok(())
    }

    pub fn complete_native_cancel(&mut self) -> Result<(), String> {
        if self.status != OrderGroupStatus::CancelPending {
            return Err("订单组当前没有待确认的原生撤销".into());
        }
        if self.legs.iter().any(|leg| {
            !matches!(
                leg.order.status,
                OrderStatus::Cancelled | OrderStatus::Expired
            ) || leg.order.filled.raw() != 0
                || leg
                    .order
                    .exact_amounts
                    .is_some_and(|amounts| amounts.filled.is_positive())
                || leg.venue_order_id.is_some()
        }) {
            return Err("原生 OCO 撤销未确认所有无成交子腿均已终止".into());
        }
        self.status = OrderGroupStatus::Cancelled;
        Ok(())
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
        if self
            .venue_group_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("持久订单组含空 venue_group_id".into());
        }
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
        if self.has_bracket_fill_conflict() && self.status != OrderGroupStatus::ReconcileRequired {
            return Err("Bracket 保护腿成交事实冲突，但订单组未设置待对账屏障".into());
        }
        Ok(())
    }

    fn has_bracket_fill_conflict(&self) -> bool {
        if self.intent.kind != OrderGroupKind::Bracket {
            return false;
        }
        let entry = self
            .legs
            .iter()
            .find(|leg| leg.role == OrderGroupLegRole::Entry);
        let protections: Vec<_> = self
            .legs
            .iter()
            .filter(|leg| {
                matches!(
                    leg.role,
                    OrderGroupLegRole::TakeProfit | OrderGroupLegRole::StopLoss
                ) && order_has_fill(&leg.order)
            })
            .collect();
        if protections.len() > 1 {
            return true;
        }
        let Some(entry) = entry else {
            return false;
        };
        let protection_fill = protections
            .first()
            .map_or(ExactDecimal::ZERO, |protection| {
                protection.order.filled_decimal()
            });
        if protection_fill
            .checked_cmp(entry.order.filled_decimal())
            .is_none_or(std::cmp::Ordering::is_gt)
        {
            return true;
        }
        let protection_legs: Vec<_> = self
            .legs
            .iter()
            .filter(|leg| {
                matches!(
                    leg.role,
                    OrderGroupLegRole::TakeProfit | OrderGroupLegRole::StopLoss
                )
            })
            .collect();
        // A terminal entry with remaining exposure and no live protection is
        // not a cancelled or completed Bracket. It must be reconciled as an
        // unmanaged position.
        entry.order.status.is_terminal()
            && order_has_fill(&entry.order)
            && protection_legs
                .iter()
                .all(|leg| leg.order.status.is_terminal())
            && protection_fill
                .checked_cmp(entry.order.filled_decimal())
                .is_some_and(std::cmp::Ordering::is_lt)
    }

    fn bracket_requires_peer_cancel(&self) -> bool {
        if self.intent.kind != OrderGroupKind::Bracket {
            return false;
        }
        let Some(entry) = self
            .legs
            .iter()
            .find(|leg| leg.role == OrderGroupLegRole::Entry)
        else {
            return false;
        };
        let Some(filled_protection) = self.legs.iter().find(|leg| {
            matches!(
                leg.role,
                OrderGroupLegRole::TakeProfit | OrderGroupLegRole::StopLoss
            ) && order_has_fill(&leg.order)
        }) else {
            return false;
        };
        !entry.order.status.is_terminal()
            || self.legs.iter().any(|leg| {
                matches!(
                    leg.role,
                    OrderGroupLegRole::TakeProfit | OrderGroupLegRole::StopLoss
                ) && leg.role != filled_protection.role
                    && !leg.order.status.is_terminal()
            })
    }

    /// Recover persisted bracket snapshots whose fills are unsafe to treat as terminal.
    /// The event reducer applies the same predicate to newly observed venue snapshots.
    pub fn repair_unsafe_bracket_status(&mut self) -> bool {
        if self.has_bracket_fill_conflict() && self.status != OrderGroupStatus::ReconcileRequired {
            self.status = OrderGroupStatus::ReconcileRequired;
            return true;
        }
        false
    }

    /// Repair legacy snapshots where an earlier reducer marked the whole group
    /// rejected while one or more child orders could still be live or unresolved.
    pub fn repair_legacy_rejected_status(&mut self) -> bool {
        if self.status != OrderGroupStatus::Rejected
            || self.legs.iter().all(|leg| leg.order.status.is_terminal())
        {
            return false;
        }
        self.status = OrderGroupStatus::Submitting;
        self.recompute_status();
        true
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
        if venue_order_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("venue_order_id 不能为空".into());
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
        let repeated_terminal_snapshot = leg.order.status.is_terminal()
            && leg.order.status == snapshot.status
            && leg.order.filled.raw() == snapshot.filled.raw()
            && leg.order.exact_amounts.map(|amounts| amounts.filled)
                == snapshot.exact_amounts.map(|amounts| amounts.filled);
        if (!repeated_terminal_snapshot && !leg.order.status.can_transition_to(snapshot.status))
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
            leg.venue_order_id = Some(id);
        }
        self.recompute_status();
        Ok(())
    }

    fn recompute_status(&mut self) {
        if matches!(
            self.status,
            OrderGroupStatus::ReconcileRequired | OrderGroupStatus::CancelPending
        ) {
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
                if peer.is_some_and(|leg| {
                    matches!(
                        leg.order.status,
                        OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired
                    )
                }) && filled_leg.order.status == OrderStatus::Filled
                {
                    self.status = OrderGroupStatus::Filled;
                } else if peer.is_some_and(|leg| {
                    matches!(
                        leg.order.status,
                        OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired
                    )
                }) && matches!(
                    filled_leg.order.status,
                    OrderStatus::Cancelled | OrderStatus::Expired
                ) {
                    self.status = OrderGroupStatus::PartiallyFilled;
                } else {
                    self.status = OrderGroupStatus::PeerCancelRequired;
                }
                return;
            }
        }
        if self.intent.kind == OrderGroupKind::Bracket {
            if self.has_bracket_fill_conflict() {
                self.status = OrderGroupStatus::ReconcileRequired;
                return;
            }
            if self.bracket_requires_peer_cancel() {
                self.status = OrderGroupStatus::PeerCancelRequired;
                return;
            }
            if let (Some(entry), Some(protection)) = (
                self.legs
                    .iter()
                    .find(|leg| leg.role == OrderGroupLegRole::Entry),
                self.legs.iter().find(|leg| {
                    matches!(
                        leg.role,
                        OrderGroupLegRole::TakeProfit | OrderGroupLegRole::StopLoss
                    ) && order_has_fill(&leg.order)
                }),
            ) {
                if entry.order.status.is_terminal()
                    && self.legs.iter().all(|leg| leg.order.status.is_terminal())
                    && protection
                        .order
                        .filled_decimal()
                        .checked_cmp(entry.order.filled_decimal())
                        .is_some_and(std::cmp::Ordering::is_eq)
                {
                    self.status = OrderGroupStatus::Filled;
                    return;
                }
            }
            let entry_has_fill = self
                .legs
                .iter()
                .find(|leg| leg.role == OrderGroupLegRole::Entry)
                .is_some_and(|leg| order_has_fill(&leg.order));
            let entry_is_resolved_or_live = self
                .legs
                .iter()
                .find(|leg| leg.role == OrderGroupLegRole::Entry)
                .is_some_and(|leg| {
                    leg.order.status.is_terminal()
                        || matches!(
                            leg.order.status,
                            OrderStatus::Accepted | OrderStatus::Working
                        )
                });
            let protections_are_live = self
                .legs
                .iter()
                .filter(|leg| {
                    matches!(
                        leg.role,
                        OrderGroupLegRole::TakeProfit | OrderGroupLegRole::StopLoss
                    )
                })
                .all(|leg| {
                    matches!(
                        leg.order.status,
                        OrderStatus::Accepted | OrderStatus::Working
                    )
                });
            if entry_has_fill && entry_is_resolved_or_live && protections_are_live {
                self.status = OrderGroupStatus::Active;
                return;
            }
        }
        if self.has_bracket_fill_conflict() {
            self.status = OrderGroupStatus::ReconcileRequired;
            return;
        }
        if self
            .legs
            .iter()
            .all(|leg| leg.order.status == OrderStatus::Filled)
        {
            self.status = OrderGroupStatus::Filled;
        } else if self.legs.iter().all(|leg| {
            matches!(
                leg.order.status,
                OrderStatus::Cancelled | OrderStatus::Expired
            )
        }) {
            self.status = OrderGroupStatus::Cancelled;
        } else if self
            .legs
            .iter()
            .any(|leg| leg.order.status == OrderStatus::Rejected)
        {
            let active_peer = self.legs.iter().any(|leg| {
                matches!(
                    leg.order.status,
                    OrderStatus::Accepted | OrderStatus::Working | OrderStatus::PartiallyFilled
                )
            });
            if self.intent.kind == OrderGroupKind::Oco && active_peer {
                // A rejected OCO leg does not make its still-live peer safe.
                // Keep the group routable to the audited group-cancel path.
                self.status = OrderGroupStatus::PeerCancelRequired;
            } else if self.legs.iter().all(|leg| leg.order.status.is_terminal()) {
                self.status = OrderGroupStatus::Rejected;
            } else {
                // A rejected leg does not settle a bracket or an OCO whose peer
                // is still submitting/canceling; preserve the barrier for query.
                self.status = OrderGroupStatus::ReconcileRequired;
            }
        } else if self.legs.iter().any(|leg| {
            matches!(
                leg.order.status,
                OrderStatus::Accepted | OrderStatus::Working
            )
        }) && self.legs.iter().all(|leg| {
            matches!(
                leg.order.status,
                OrderStatus::Accepted
                    | OrderStatus::Working
                    | OrderStatus::Cancelled
                    | OrderStatus::Expired
            )
        }) {
            self.status = OrderGroupStatus::Active;
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

fn order_has_fill(order: &Order) -> bool {
    order.filled.raw() > 0
        || order
            .exact_amounts
            .is_some_and(|amounts| amounts.filled.is_positive())
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

    fn bracket_state() -> OrderGroupState {
        let mut entry = protection_order(801, ConditionalOrderKind::TakeProfit);
        entry.side = Side::Buy;
        entry.exact_amounts.as_mut().unwrap().conditional = None;
        OrderGroupState::from_intent(
            OrderGroupIntent::new(
                "bracket-1",
                OrderGroupKind::Bracket,
                OrderGroupExecution::NativeRequired,
                vec![
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::Entry,
                        order: entry,
                    },
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::TakeProfit,
                        order: protection_order(802, ConditionalOrderKind::TakeProfit),
                    },
                    OrderGroupLegIntent {
                        role: OrderGroupLegRole::StopLoss,
                        order: protection_order(803, ConditionalOrderKind::StopLoss),
                    },
                ],
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn native_group_activation_does_not_assign_group_id_to_child_orders() {
        let mut group = oco_state();
        group.begin_submission().unwrap();
        group.record_venue_group_id("okx-algo-123".into()).unwrap();
        group.activate_native_group().unwrap();

        assert_eq!(group.status, OrderGroupStatus::Active);
        assert_eq!(group.venue_group_id.as_deref(), Some("okx-algo-123"));
        assert!(group.legs.iter().all(|leg| leg.venue_order_id.is_none()));
    }

    #[test]
    fn oco_accepts_native_limit_take_profit_with_conditional_stop_loss() {
        let mut take_profit = protection_order(1, ConditionalOrderKind::TakeProfit);
        let amounts = take_profit.exact_amounts.as_mut().unwrap();
        amounts.conditional = None;
        amounts.limit_price = Some(ExactDecimal::parse("70000").unwrap());
        let intent = OrderGroupIntent::new(
            "oco-limit-trigger",
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: take_profit,
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: protection_order(2, ConditionalOrderKind::StopLoss),
                },
            ],
        );

        assert!(intent.is_ok());
    }

    #[test]
    fn native_group_cancel_is_fenced_and_requires_an_untriggered_resource() {
        let mut group = oco_state();
        group.begin_submission().unwrap();
        group.record_venue_group_id("okx-algo-123".into()).unwrap();
        group.activate_native_group().unwrap();
        group.begin_native_cancel().unwrap();
        assert_eq!(group.status, OrderGroupStatus::CancelPending);
        assert!(group.begin_native_cancel().is_err());

        let mut triggered = oco_state();
        triggered.begin_submission().unwrap();
        triggered
            .record_venue_group_id("okx-algo-456".into())
            .unwrap();
        triggered.activate_native_group().unwrap();
        triggered.legs[0].venue_order_id = Some("child-order-1".into());
        assert!(triggered.begin_native_cancel().is_err());
    }

    #[test]
    fn repeated_terminal_leg_snapshot_is_idempotent_but_cannot_change_fills() {
        let mut group = oco_state();
        group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Submitted, "0"), None)
            .unwrap();
        group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::CancelPending, "0"), None)
            .unwrap();
        let cancelled = snapshot(&group, 1, OrderStatus::Cancelled, "0");
        group.apply_order_snapshot(cancelled.clone(), None).unwrap();
        group.apply_order_snapshot(cancelled, None).unwrap();
        assert!(group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Cancelled, "0.001"), None)
            .is_err());
    }

    #[test]
    fn empty_venue_order_id_rejects_snapshot_without_mutating_group() {
        let mut group = oco_state();
        let before = group.clone();
        let update = snapshot(&group, 1, OrderStatus::Accepted, "0");

        assert!(group
            .apply_order_snapshot(update, Some(" \t".into()))
            .is_err());
        assert_eq!(group, before);
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
    fn oco_rejected_leg_keeps_live_peer_in_cancel_or_reconciliation_path() {
        let mut group = oco_state();
        for id in [1, 2] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                group
                    .apply_order_snapshot(snapshot(&group, id, status, "0"), None)
                    .unwrap();
            }
        }

        group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Rejected, "0"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::PeerCancelRequired);
        assert_eq!(group.legs[1].order.status, OrderStatus::Working);

        group
            .apply_order_snapshot(snapshot(&group, 2, OrderStatus::CancelPending, "0"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        group
            .reconcile_order_snapshot(snapshot(&group, 2, OrderStatus::Cancelled, "0"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::Rejected);
        group.validate_persisted().unwrap();
    }

    #[test]
    fn oco_rejected_leg_with_unconfirmed_peer_requires_reconciliation() {
        let mut group = oco_state();
        for id in [1, 2] {
            group
                .apply_order_snapshot(snapshot(&group, id, OrderStatus::Submitted, "0"), None)
                .unwrap();
        }
        group
            .apply_order_snapshot(snapshot(&group, 1, OrderStatus::Rejected, "0"), None)
            .unwrap();

        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        assert_eq!(group.legs[1].order.status, OrderStatus::Submitted);
    }

    #[test]
    fn bracket_rejected_leg_does_not_hide_active_protection_peers() {
        let mut group = bracket_state();
        for id in [801, 802, 803] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                group
                    .apply_order_snapshot(snapshot(&group, id, status, "0"), None)
                    .unwrap();
            }
        }

        group
            .apply_order_snapshot(snapshot(&group, 801, OrderStatus::Rejected, "0"), None)
            .unwrap();

        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        assert_eq!(group.legs[1].order.status, OrderStatus::Working);
        assert_eq!(group.legs[2].order.status, OrderStatus::Working);
    }

    #[test]
    fn bracket_dual_protection_fills_require_reconciliation() {
        let mut group = bracket_state();
        for id in [801, 802, 803] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                group
                    .apply_order_snapshot(snapshot(&group, id, status, "0"), None)
                    .unwrap();
            }
        }
        group
            .apply_order_snapshot(snapshot(&group, 801, OrderStatus::Filled, "0.01"), None)
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 802, OrderStatus::Filled, "0.01"),
                Some("venue-tp".into()),
            )
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 803, OrderStatus::PartiallyFilled, "0.001"),
                Some("venue-sl".into()),
            )
            .unwrap();

        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
    }

    #[test]
    fn bracket_protection_fill_cannot_exceed_confirmed_entry_quantity() {
        let mut group = bracket_state();
        for id in [801, 802, 803] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                group
                    .apply_order_snapshot(snapshot(&group, id, status, "0"), None)
                    .unwrap();
            }
        }
        group
            .apply_order_snapshot(
                snapshot(&group, 801, OrderStatus::PartiallyFilled, "0.004"),
                None,
            )
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 802, OrderStatus::PartiallyFilled, "0.005"),
                Some("venue-tp".into()),
            )
            .unwrap();

        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
    }

    #[test]
    fn bracket_protection_fill_requires_peer_cancel_then_settles_closed_position() {
        let mut group = bracket_state();
        for id in [801, 802, 803] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                group
                    .apply_order_snapshot(snapshot(&group, id, status, "0"), None)
                    .unwrap();
            }
        }
        group
            .apply_order_snapshot(snapshot(&group, 801, OrderStatus::Filled, "0.01"), None)
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::Active);

        group
            .apply_order_snapshot(
                snapshot(&group, 802, OrderStatus::Filled, "0.01"),
                Some("venue-tp".into()),
            )
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::PeerCancelRequired);

        group
            .apply_order_snapshot(
                snapshot(&group, 803, OrderStatus::CancelPending, "0"),
                Some("venue-sl".into()),
            )
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 803, OrderStatus::Cancelled, "0"),
                Some("venue-sl".into()),
            )
            .unwrap();
        assert_eq!(group.status, OrderGroupStatus::Filled);
        group.validate_persisted().unwrap();
    }

    #[test]
    fn bracket_partial_entry_cannot_be_cancelled_without_resolving_exposure() {
        let mut group = bracket_state();
        for id in [801, 802, 803] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                group
                    .apply_order_snapshot(snapshot(&group, id, status, "0"), None)
                    .unwrap();
            }
        }
        group
            .apply_order_snapshot(
                snapshot(&group, 801, OrderStatus::PartiallyFilled, "0.004"),
                None,
            )
            .unwrap();
        group
            .apply_order_snapshot(
                snapshot(&group, 801, OrderStatus::CancelPending, "0.004"),
                None,
            )
            .unwrap();
        group
            .apply_order_snapshot(snapshot(&group, 801, OrderStatus::Cancelled, "0.004"), None)
            .unwrap();
        for id in [802, 803] {
            group
                .apply_order_snapshot(snapshot(&group, id, OrderStatus::CancelPending, "0"), None)
                .unwrap();
            group
                .apply_order_snapshot(snapshot(&group, id, OrderStatus::Cancelled, "0"), None)
                .unwrap();
        }

        assert_eq!(group.status, OrderGroupStatus::ReconcileRequired);
        assert!(group.validate_persisted().is_ok());
        group.status = OrderGroupStatus::Cancelled;
        assert!(group.validate_persisted().is_err());
    }

    #[test]
    fn oco_peer_expiry_resolves_partial_execution_and_empty_expiry() {
        let mut one_live = oco_state();
        for id in [1, 2] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                one_live
                    .apply_order_snapshot(snapshot(&one_live, id, status, "0"), None)
                    .unwrap();
            }
        }
        one_live
            .apply_order_snapshot(snapshot(&one_live, 2, OrderStatus::Expired, "0"), None)
            .unwrap();
        assert_eq!(one_live.status, OrderGroupStatus::Active);

        let mut partial = oco_state();
        for id in [1, 2] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                partial
                    .apply_order_snapshot(snapshot(&partial, id, status, "0"), None)
                    .unwrap();
            }
        }
        partial
            .apply_order_snapshot(
                snapshot(&partial, 1, OrderStatus::PartiallyFilled, "0.004"),
                None,
            )
            .unwrap();
        partial
            .apply_order_snapshot(snapshot(&partial, 2, OrderStatus::Expired, "0"), None)
            .unwrap();
        assert_eq!(partial.status, OrderGroupStatus::PeerCancelRequired);
        partial
            .apply_order_snapshot(snapshot(&partial, 1, OrderStatus::Expired, "0.004"), None)
            .unwrap();
        assert_eq!(partial.status, OrderGroupStatus::PartiallyFilled);

        let mut cancelled_partial = oco_state();
        for id in [1, 2] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
            ] {
                cancelled_partial
                    .apply_order_snapshot(snapshot(&cancelled_partial, id, status, "0"), None)
                    .unwrap();
            }
        }
        cancelled_partial
            .apply_order_snapshot(
                snapshot(&cancelled_partial, 1, OrderStatus::PartiallyFilled, "0.004"),
                None,
            )
            .unwrap();
        cancelled_partial
            .apply_order_snapshot(
                snapshot(&cancelled_partial, 2, OrderStatus::CancelPending, "0"),
                None,
            )
            .unwrap();
        cancelled_partial
            .apply_order_snapshot(
                snapshot(&cancelled_partial, 2, OrderStatus::Cancelled, "0"),
                None,
            )
            .unwrap();
        cancelled_partial
            .apply_order_snapshot(
                snapshot(&cancelled_partial, 1, OrderStatus::CancelPending, "0.004"),
                None,
            )
            .unwrap();
        cancelled_partial
            .apply_order_snapshot(
                snapshot(&cancelled_partial, 1, OrderStatus::Cancelled, "0.004"),
                None,
            )
            .unwrap();
        assert_eq!(cancelled_partial.status, OrderGroupStatus::PartiallyFilled);

        let mut empty = oco_state();
        for id in [1, 2] {
            for status in [
                OrderStatus::Submitted,
                OrderStatus::Accepted,
                OrderStatus::Working,
                OrderStatus::Expired,
            ] {
                empty
                    .apply_order_snapshot(snapshot(&empty, id, status, "0"), None)
                    .unwrap();
            }
        }
        assert_eq!(empty.status, OrderGroupStatus::Cancelled);
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

        let mut legacy = serde_json::to_value(&group).unwrap();
        legacy.as_object_mut().unwrap().remove("venue_group_id");
        let legacy: OrderGroupState = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.venue_group_id, None);
        legacy.validate_persisted().unwrap();
    }
}
