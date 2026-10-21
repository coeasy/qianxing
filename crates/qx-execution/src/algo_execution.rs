//! Bounded, restartable submission of a deterministic TWAP/VWAP child plan.
//!
//! The scheduler calls `step` with its current clock. Each call submits or
//! cancels at most one child through the ordinary risk/OMS/venue gateway. The
//! caller persists the returned state after every step; replaying an older
//! checkpoint uses the same child client id and the OMS event log remains the
//! authority for whether that child already left the process.

use crate::{ExecutionEventPort, PortExecutionService, RiskPort, VenuePort};
mod validation;
use qx_core::{Order, OrderStatus};
use qx_zhenlu::AlgoChildSlice;
use serde::{Deserialize, Serialize};
use validation::{same_child_identity, validate_checkpoint, validate_input};

pub const ALGO_EXECUTION_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlgoExecutionStatus {
    Ready,
    Running,
    CancelRequested,
    Completed,
    Cancelled,
    Failed,
    ReconcileRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlgoExecutionStep {
    NotDue {
        next_scheduled_ts: u64,
    },
    Submitted {
        client_order_id: u64,
        status: OrderStatus,
    },
    WaitingForChildren {
        open_orders: usize,
        filled_raw: i128,
    },
    CancellationSubmitted {
        client_order_id: u64,
    },
    Completed {
        filled_raw: i128,
        unfilled_raw: i128,
    },
    Cancelled {
        filled_raw: i128,
        unfilled_raw: i128,
    },
    Failed {
        reason: String,
    },
    ReconcileRequired {
        reason: String,
    },
}

/// Serializable parent-order execution checkpoint. Persist this value with a
/// compare-and-swap or append-only store owned by the runtime; it contains no
/// venue client, credentials, thread, or non-deterministic clock state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlgoExecutionState {
    pub schema_version: u32,
    pub parent_id: String,
    pub parent_order: Order,
    pub plan: Vec<AlgoChildSlice>,
    pub child_order_id_base: u64,
    pub next_slice_position: usize,
    pub child_order_ids: Vec<u64>,
    /// Last scheduler timestamp accepted by `step`. Optional for compatibility
    /// with checkpoints written before monotonic clock validation was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_step_ts: Option<u64>,
    pub status: AlgoExecutionStatus,
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Construction input kept separate from the persisted checkpoint so callers
/// cannot accidentally resume a partially advanced state by rebuilding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlgoParentOrder {
    pub parent_id: String,
    pub parent_order: Order,
    pub plan: Vec<AlgoChildSlice>,
    pub child_order_id_base: u64,
}

impl AlgoExecutionState {
    pub fn new(input: AlgoParentOrder) -> Result<Self, String> {
        validate_input(&input)?;
        let state = Self {
            schema_version: ALGO_EXECUTION_STATE_SCHEMA_VERSION,
            parent_id: input.parent_id,
            parent_order: input.parent_order,
            plan: input.plan,
            child_order_id_base: input.child_order_id_base,
            next_slice_position: 0,
            child_order_ids: Vec::new(),
            last_step_ts: None,
            status: AlgoExecutionStatus::Ready,
            cancel_requested: false,
            failure: None,
        };
        validate_checkpoint(&state)?;
        Ok(state)
    }

    pub fn request_cancel(&mut self) -> Result<(), String> {
        validate_checkpoint(self)?;
        match self.status {
            AlgoExecutionStatus::Completed
            | AlgoExecutionStatus::Cancelled
            | AlgoExecutionStatus::Failed
            | AlgoExecutionStatus::ReconcileRequired => {
                return Err(format!("算法父单终态不可取消: {:?}", self.status));
            }
            _ => {}
        }
        self.cancel_requested = true;
        self.status = AlgoExecutionStatus::CancelRequested;
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, String> {
        validate_checkpoint(self)?;
        serde_json::to_string(self).map_err(|error| format!("算法执行状态序列化失败: {error}"))
    }

    pub fn from_json(payload: &str) -> Result<Self, String> {
        let state: Self = serde_json::from_str(payload)
            .map_err(|error| format!("算法执行状态 JSON 无效: {error}"))?;
        validate_checkpoint(&state)?;
        Ok(state)
    }

    /// Advance no more than one scheduled child or one cancellation per call.
    /// This gives an external scheduler a strict work bound and a persistence
    /// boundary after every venue side effect.
    pub fn step<V, P, R>(
        &mut self,
        now: u64,
        gateway: &mut PortExecutionService<'_, V, P>,
        risk: &R,
    ) -> Result<AlgoExecutionStep, String>
    where
        V: VenuePort,
        P: ExecutionEventPort,
        R: RiskPort,
    {
        validate_checkpoint(self)?;
        if gateway.now != now {
            return Err(format!(
                "算法调度时间与执行网关时间不一致: scheduler={now} gateway={}",
                gateway.now
            ));
        }
        if let Some(last) = self.last_step_ts {
            if now < last {
                return Err(format!(
                    "算法执行时钟不得回退: previous={last} current={now}"
                ));
            }
        }
        self.last_step_ts = Some(now);
        if let Some(result) = self.terminal_step(gateway)? {
            return Ok(result);
        }
        if self.cancel_requested {
            return self.cancel_one(gateway);
        }
        if self.next_slice_position < self.plan.len() {
            let slice = &self.plan[self.next_slice_position];
            if now < slice.scheduled_ts {
                return Ok(AlgoExecutionStep::NotDue {
                    next_scheduled_ts: slice.scheduled_ts,
                });
            }
            return self.submit_one(gateway, risk);
        }
        self.finish_or_wait(gateway)
    }

    fn submit_one<V, P, R>(
        &mut self,
        gateway: &mut PortExecutionService<'_, V, P>,
        risk: &R,
    ) -> Result<AlgoExecutionStep, String>
    where
        V: VenuePort,
        P: ExecutionEventPort,
        R: RiskPort,
    {
        let position = self.next_slice_position;
        let slice = self.plan[position].clone();
        let client_order_id = self.child_client_id(position)?;
        let child_order = self.child_order(&slice, client_order_id);

        if let Some(existing) = gateway.order(client_order_id) {
            if !same_child_identity(&existing, &child_order) {
                return self.require_reconcile(format!(
                    "child client id 已被不同订单占用: parent={} child_index={} client_order_id={client_order_id}",
                    self.parent_id, slice.child_index
                ));
            }
            match existing.status {
                OrderStatus::PendingSubmit => {
                    gateway.submit_with_risk_and_correlation(
                        child_order,
                        risk,
                        self.child_correlation(&slice),
                    )?;
                }
                OrderStatus::Unknown | OrderStatus::Submitted | OrderStatus::CancelPending => {
                    return self.require_reconcile(format!(
                        "恢复时子单状态 {:?} 需要对账，禁止重发: client_order_id={client_order_id}",
                        existing.status
                    ));
                }
                _ => {
                    self.child_order_ids.push(client_order_id);
                    self.next_slice_position += 1;
                    self.status = AlgoExecutionStatus::Running;
                    return Ok(AlgoExecutionStep::Submitted {
                        client_order_id,
                        status: existing.status,
                    });
                }
            }
        } else if let Err(error) = gateway.submit_with_risk_and_correlation(
            child_order,
            risk,
            self.child_correlation(&slice),
        ) {
            if gateway
                .order(client_order_id)
                .is_some_and(|order| order.status == OrderStatus::Unknown)
            {
                return self.require_reconcile(format!(
                    "子单提交结果未知，必须先对账: client_order_id={client_order_id}: {error}"
                ));
            }
            self.status = AlgoExecutionStatus::Failed;
            self.failure = Some(error.clone());
            return Ok(AlgoExecutionStep::Failed { reason: error });
        }

        let Some(order) = gateway.order(client_order_id) else {
            return self.require_reconcile(format!(
                "Venue 已返回但 OMS 查不到子单: client_order_id={client_order_id}"
            ));
        };
        if order.status == OrderStatus::Unknown {
            return self.require_reconcile(format!(
                "子单提交后进入待对账: client_order_id={client_order_id}"
            ));
        }
        self.child_order_ids.push(client_order_id);
        self.next_slice_position += 1;
        self.status = AlgoExecutionStatus::Running;
        Ok(AlgoExecutionStep::Submitted {
            client_order_id,
            status: order.status,
        })
    }

    fn cancel_one<V, P>(
        &mut self,
        gateway: &mut PortExecutionService<'_, V, P>,
    ) -> Result<AlgoExecutionStep, String>
    where
        V: VenuePort,
        P: ExecutionEventPort,
    {
        for client_order_id in self.child_order_ids.iter().copied() {
            let Some(order) = gateway.order(client_order_id) else {
                return self.require_reconcile(format!(
                    "撤单前 OMS 查不到算法子单: client_order_id={client_order_id}"
                ));
            };
            match order.status {
                OrderStatus::Accepted | OrderStatus::Working | OrderStatus::PartiallyFilled => {
                    if let Err(error) = gateway.cancel(client_order_id) {
                        if gateway
                            .order(client_order_id)
                            .is_some_and(|order| order.status == OrderStatus::Unknown)
                        {
                            return self.require_reconcile(format!(
                                "子单撤销结果未知，必须先对账: client_order_id={client_order_id}: {error}"
                            ));
                        }
                        return Err(error);
                    }
                    return Ok(AlgoExecutionStep::CancellationSubmitted { client_order_id });
                }
                OrderStatus::Unknown | OrderStatus::Submitted | OrderStatus::CancelPending => {
                    return self.require_reconcile(format!(
                        "撤单遇到 {:?} 子单，需要先对账: client_order_id={client_order_id}",
                        order.status
                    ));
                }
                OrderStatus::PendingSubmit
                | OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired => {}
            }
        }
        let (filled_raw, unfilled_raw) = self.fill_totals(gateway)?;
        self.status = AlgoExecutionStatus::Cancelled;
        Ok(AlgoExecutionStep::Cancelled {
            filled_raw,
            unfilled_raw,
        })
    }

    fn finish_or_wait<V, P>(
        &mut self,
        gateway: &PortExecutionService<'_, V, P>,
    ) -> Result<AlgoExecutionStep, String>
    where
        V: VenuePort,
        P: ExecutionEventPort,
    {
        let mut open_orders = 0;
        for client_order_id in &self.child_order_ids {
            let Some(order) = gateway.order(*client_order_id) else {
                return self.require_reconcile(format!(
                    "算法执行状态引用了缺失子单: client_order_id={client_order_id}"
                ));
            };
            match order.status {
                OrderStatus::Unknown | OrderStatus::Submitted | OrderStatus::CancelPending => {
                    return self.require_reconcile(format!(
                        "算法子单处于 {:?}，必须先对账: client_order_id={client_order_id}",
                        order.status
                    ));
                }
                OrderStatus::Accepted | OrderStatus::Working | OrderStatus::PartiallyFilled => {
                    open_orders += 1;
                }
                OrderStatus::PendingSubmit
                | OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired => {}
            }
        }
        let (filled_raw, unfilled_raw) = self.fill_totals(gateway)?;
        if open_orders > 0 {
            return Ok(AlgoExecutionStep::WaitingForChildren {
                open_orders,
                filled_raw,
            });
        }
        self.status = AlgoExecutionStatus::Completed;
        Ok(AlgoExecutionStep::Completed {
            filled_raw,
            unfilled_raw,
        })
    }

    fn terminal_step<V, P>(
        &mut self,
        gateway: &PortExecutionService<'_, V, P>,
    ) -> Result<Option<AlgoExecutionStep>, String>
    where
        V: VenuePort,
        P: ExecutionEventPort,
    {
        let result = match self.status {
            AlgoExecutionStatus::Completed => {
                let (filled_raw, unfilled_raw) = self.fill_totals(gateway)?;
                Some(AlgoExecutionStep::Completed {
                    filled_raw,
                    unfilled_raw,
                })
            }
            AlgoExecutionStatus::Cancelled => {
                let (filled_raw, unfilled_raw) = self.fill_totals(gateway)?;
                Some(AlgoExecutionStep::Cancelled {
                    filled_raw,
                    unfilled_raw,
                })
            }
            AlgoExecutionStatus::Failed => Some(AlgoExecutionStep::Failed {
                reason: self
                    .failure
                    .clone()
                    .unwrap_or_else(|| "算法执行失败".into()),
            }),
            AlgoExecutionStatus::ReconcileRequired => Some(AlgoExecutionStep::ReconcileRequired {
                reason: self.failure.clone().unwrap_or_else(|| "需要对账".into()),
            }),
            _ => None,
        };
        Ok(result)
    }

    fn fill_totals<V, P>(
        &self,
        gateway: &PortExecutionService<'_, V, P>,
    ) -> Result<(i128, i128), String>
    where
        V: VenuePort,
        P: ExecutionEventPort,
    {
        let filled_raw = self.child_order_ids.iter().try_fold(0_i128, |sum, id| {
            let order = gateway
                .order(*id)
                .ok_or_else(|| format!("算法子单缺失: client_order_id={id}"))?;
            sum.checked_add(order.filled.raw())
                .ok_or_else(|| "算法父单累计成交数量溢出".to_string())
        })?;
        if filled_raw > self.parent_order.qty.raw() {
            return Err(format!(
                "算法子单累计成交超过父单: parent={} filled={} qty={}",
                self.parent_id,
                filled_raw,
                self.parent_order.qty.raw()
            ));
        }
        Ok((filled_raw, self.parent_order.qty.raw() - filled_raw))
    }

    fn child_order(&self, slice: &AlgoChildSlice, client_order_id: u64) -> Order {
        let mut child = self.parent_order.clone();
        child.client_id = client_order_id;
        child.qty = qx_core::Quantity::from_raw(slice.quantity_raw);
        child.filled = qx_core::Quantity::ZERO;
        child.status = OrderStatus::PendingSubmit;
        child
    }

    fn child_client_id(&self, position: usize) -> Result<u64, String> {
        self.child_order_id_base
            .checked_add(position as u64)
            .ok_or_else(|| "算法子单 client id 溢出".into())
    }

    fn child_correlation(&self, slice: &AlgoChildSlice) -> String {
        format!("algo:{}:child:{}", self.parent_id, slice.child_index)
    }

    fn require_reconcile(&mut self, reason: String) -> Result<AlgoExecutionStep, String> {
        self.status = AlgoExecutionStatus::ReconcileRequired;
        self.failure = Some(reason.clone());
        Ok(AlgoExecutionStep::ReconcileRequired { reason })
    }
}

#[cfg(test)]
mod tests;
