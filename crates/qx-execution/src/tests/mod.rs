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
            ExecutionEvent::Cancelled { client_order_id } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == *client_order_id)
                {
                    order.status = OrderStatus::Cancelled;
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
        }
        self.events.push(envelope);
        Ok(())
    }
}

struct PortVenue {
    result: Result<Vec<ExecutionEvent>, String>,
}

struct NeverCalledVenue;

impl VenuePort for NeverCalledVenue {
    fn venue_id(&self) -> &str {
        "never-called"
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
    fn evaluate_order(&self, _order: &Order) -> Result<RiskDecision, String> {
        Ok(RiskDecision {
            accepted: false,
            reason_code: "max_notional",
        })
    }
}

#[derive(Default)]
struct PortRouter {
    calls: Vec<String>,
}

impl VenueRouterPort for PortRouter {
    fn submit_order(
        &mut self,
        venue_id: &str,
        order: Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.calls.push(venue_id.to_string());
        Ok(vec![
            ExecutionEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: format!("{venue_id}-{}", order.client_id),
            },
            ExecutionEvent::Fill(Box::new(qx_core::Fill {
                order_id: order.client_id,
                qty: order.qty,
                price: Price::from_i64(100),
                ts,
                ..qx_core::Fill::default()
            })),
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
    }
}

mod gateway_port;
mod recovery_and_replay;
mod spread_group_barrier;
mod venue_submit_contract;
