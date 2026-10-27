//! `qx-execution` 的单元测试模块（从 `lib.rs` 拆出，保持 `use super::*` 的私有可见性）。

use super::*;
use qx_adapter::{
    BinanceSpotAuth, BinanceSpotVenue, CcxtProcessVenue, CcxtRpc, HttpRequest, HttpResponse,
    HttpTransport,
};
use qx_control::{CommandKind, ControlCommand, Permission};
use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, Side};
use qx_zhenlu::{FileSpreadOrderGroupStore, SpreadOrderGroupStore, SpreadOrderLeg};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct PortState {
    orders: Vec<Order>,
    events: Vec<ExecutionEventEnvelope>,
}

impl OrderStore for PortState {
    fn orders(&self) -> Vec<Order> {
        self.orders.clone()
    }

    fn register_order(
        &mut self,
        order: Order,
        _ts: u64,
        _correlation_id: Option<String>,
    ) -> Result<(), String> {
        if self
            .orders
            .iter()
            .any(|existing| existing.client_id == order.client_id)
        {
            return Err("duplicate order".into());
        }
        self.orders.push(order);
        Ok(())
    }
}

impl EventAppender for PortState {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        match &envelope.event {
            ExecutionEvent::Accepted {
                client_order_id, ..
            } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == *client_order_id)
                {
                    order.status = OrderStatus::Accepted;
                }
            }
            ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == fill.order_id)
                {
                    if let (Some(order_amounts), Some(fill_amounts)) =
                        (order.exact_amounts.as_mut(), fill.exact_amounts)
                    {
                        order_amounts.filled = order_amounts
                            .filled
                            .checked_add(fill_amounts.quantity)
                            .expect("test fill amount must not overflow");
                        order.status = if order_amounts.filled == order_amounts.quantity {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        };
                    } else {
                        order.filled = Quantity::from_raw(
                            order
                                .filled
                                .raw()
                                .saturating_add(fill.qty.raw())
                                .min(order.qty.raw()),
                        );
                        order.status = if order.filled == order.qty {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        };
                    }
                }
            }
            ExecutionEvent::Cancelled { client_order_id } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == *client_order_id)
                {
                    order.status = OrderStatus::Cancelled;
                }
            }
            ExecutionEvent::CancelPending { client_order_id } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == *client_order_id)
                {
                    order.status = OrderStatus::CancelPending;
                }
            }
            ExecutionEvent::ReconcileRequired { client_order_id } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == *client_order_id)
                {
                    order.status = OrderStatus::Unknown;
                }
            }
            // 行情事实不改订单状态，只作为事件留痕。
            ExecutionEvent::MarketQuote { .. } => {}
            ExecutionEvent::FeeAdjustment { .. } => {}
        }
        self.events.push(envelope);
        Ok(())
    }

    fn append_execution_event_with_outcome(
        &mut self,
        envelope: ExecutionEventEnvelope,
    ) -> Result<EventAppendOutcome, String> {
        if self
            .events
            .iter()
            .any(|event| event.correlation_id == envelope.correlation_id)
        {
            return Ok(EventAppendOutcome::Duplicate);
        }
        self.append_execution_event(envelope)?;
        Ok(EventAppendOutcome::Appended)
    }
}

struct PortVenue {
    result: Result<Vec<ExecutionEvent>, String>,
}

struct NeverCalledVenue;

impl VenuePort for NeverCalledVenue {
    fn venue_id(&self) -> &str {
        "BINANCE"
    }

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES
    }

    fn execution_readiness(&self) -> Result<(), String> {
        Ok(())
    }

    fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        panic!("risk rejection must happen before Venue submit")
    }

    fn cancel_order(
        &mut self,
        _client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        panic!("risk rejection test must not cancel")
    }
}

struct RejectingRisk;

impl RiskPort for RejectingRisk {
    fn evaluate_order(&self, _order: &Order) -> Result<RiskVerdict, String> {
        Ok(RiskVerdict::Reject {
            reason: "max_notional".into(),
        })
    }
}

#[derive(Default)]
struct PortRouter {
    calls: Vec<String>,
    exact_orders: bool,
}

impl VenueRouterPort for PortRouter {
    fn capabilities_for(
        &self,
        _venue_id: &str,
    ) -> Option<qx_zhenlu::venue_capabilities::VenueCapabilities> {
        Some(qx_zhenlu::venue_capabilities::VenueCapabilities {
            exact_orders: self.exact_orders,
            ..qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES
        })
    }

    fn execution_readiness_for(&self, _venue_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn submit_order(
        &mut self,
        venue_id: &str,
        order: Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.calls.push(venue_id.to_string());
        let fill = if let Some(amounts) = order.exact_amounts {
            qx_core::Fill {
                order_id: order.client_id,
                exact_amounts: Some(qx_core::ExactFillAmounts {
                    quantity: amounts.quantity,
                    price: qx_core::ExactDecimal::parse("100").unwrap(),
                    quote_cost: None,
                    quote_budget_complete: false,
                }),
                ts,
                ..qx_core::Fill::default()
            }
        } else {
            qx_core::Fill {
                order_id: order.client_id,
                qty: order.qty,
                price: Price::from_i64(100),
                ts,
                ..qx_core::Fill::default()
            }
        };
        Ok(vec![
            ExecutionEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: format!("{venue_id}-{}", order.client_id),
            },
            ExecutionEvent::Fill(Box::new(fill)),
        ])
    }

    fn cancel_order(
        &mut self,
        venue_id: &str,
        client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.calls.push(format!("cancel:{venue_id}"));
        Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
    }
}

impl VenuePort for PortVenue {
    fn venue_id(&self) -> &str {
        "port-test"
    }

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES
    }

    fn execution_readiness(&self) -> Result<(), String> {
        Ok(())
    }

    fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        self.result.clone()
    }

    fn cancel_order(
        &mut self,
        _client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        Ok(vec![ExecutionEvent::Cancelled { client_order_id: 7 }])
    }
}

/// 只断言回报形状/生命周期事实的用例显式零费：费用会让期望变成噪音，
/// 但生产 Paper 路径的费用模型必须由调用方给出（见 `PaperVenue::new`）。
fn zero_fee() -> Box<dyn FeeModel + Send> {
    Box::new(qx_core::ZeroFeeModel)
}

fn port_order(client_id: u64) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "port-main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    }
}

mod gateway_port;
mod recovery_and_replay;
mod spread_group_barrier;
mod venue_submit_contract;
