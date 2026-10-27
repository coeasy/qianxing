use super::*;
use crate::{
    EventAppender, ExecutionEvent, ExecutionEventEnvelope, OrderStore, RiskVerdict, VenuePort,
};
use qx_core::{Fill, InstrumentId, Price, Quantity, Side};

#[derive(Default)]
struct MemoryOms {
    orders: Vec<Order>,
}

impl OrderStore for MemoryOms {
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
            return Err("duplicate client id".into());
        }
        self.orders.push(order);
        Ok(())
    }
}

impl EventAppender for MemoryOms {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        match envelope.event {
            ExecutionEvent::Accepted {
                client_order_id, ..
            } => {
                self.find_mut(client_order_id)?.status = OrderStatus::Accepted;
            }
            ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => {
                let order = self.find_mut(fill.order_id)?;
                order.filled = Quantity::from_raw(order.filled.raw() + fill.qty.raw());
                order.status = if order.filled == order.qty {
                    OrderStatus::Filled
                } else {
                    OrderStatus::PartiallyFilled
                };
            }
            ExecutionEvent::CancelPending { client_order_id } => {
                self.find_mut(client_order_id)?.status = OrderStatus::CancelPending;
            }
            ExecutionEvent::Cancelled { client_order_id } => {
                self.find_mut(client_order_id)?.status = OrderStatus::Cancelled;
            }
            ExecutionEvent::ReconcileRequired { client_order_id } => {
                self.find_mut(client_order_id)?.status = OrderStatus::Unknown;
            }
            ExecutionEvent::MarketQuote { .. } => {}
            ExecutionEvent::FeeAdjustment { .. } => {}
        }
        Ok(())
    }
}

impl MemoryOms {
    fn find_mut(&mut self, client_order_id: u64) -> Result<&mut Order, String> {
        self.orders
            .iter_mut()
            .find(|order| order.client_id == client_order_id)
            .ok_or_else(|| format!("missing order {client_order_id}"))
    }
}

#[derive(Clone, Copy)]
enum VenueMode {
    Fill,
    Rest,
    Unknown,
}

struct TestVenue {
    mode: VenueMode,
    submits: usize,
    cancels: usize,
}

impl VenuePort for TestVenue {
    fn venue_id(&self) -> &str {
        "algo-test"
    }

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES
    }

    fn execution_readiness(&self) -> Result<(), String> {
        Ok(())
    }

    fn submit_order(&mut self, order: Order, ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        self.submits += 1;
        if matches!(self.mode, VenueMode::Unknown) {
            return Err("simulated timeout".into());
        }
        let mut events = vec![ExecutionEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: format!("venue-{}", order.client_id),
        }];
        if matches!(self.mode, VenueMode::Fill) {
            events.push(ExecutionEvent::Fill(Box::new(Fill {
                order_id: order.client_id,
                qty: order.qty,
                price: Price::from_i64(100),
                ts,
                ..Fill::default()
            })));
        }
        Ok(events)
    }

    fn cancel_order(
        &mut self,
        client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.cancels += 1;
        Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
    }
}

struct Risk(bool);

impl RiskPort for Risk {
    fn evaluate_order(&self, _order: &Order) -> Result<RiskVerdict, String> {
        if self.0 {
            Ok(RiskVerdict::Allow)
        } else {
            Ok(RiskVerdict::Reject {
                reason: "test limit".into(),
            })
        }
    }
}

fn parent_order(quantity_raw: i128) -> Order {
    Order {
        client_id: 100,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_raw(quantity_raw),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    }
}

fn state(child_count: usize) -> AlgoExecutionState {
    let quantity = 11_i128;
    AlgoExecutionState::new(AlgoParentOrder {
        parent_id: "parent-1".into(),
        parent_order: parent_order(quantity),
        plan: qx_zhenlu::plan_twap("parent-1", quantity, 10, 30, child_count).unwrap(),
        child_order_id_base: 500,
    })
    .unwrap()
}

fn gateway<'a>(
    venue: &'a mut TestVenue,
    oms: &'a mut MemoryOms,
    source_seq: &'a mut u64,
    now: u64,
) -> PortExecutionService<'a, TestVenue, MemoryOms> {
    PortExecutionService::new(venue, oms, "algo-worker", now, source_seq)
}

fn market_spec(qty_step: i128, min_qty: i128) -> qx_core::TradingInstrumentSpec {
    qx_core::TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        product: qx_core::TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: qx_core::SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step,
        min_qty,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
        exact_rules: None,
    }
}

#[test]
fn due_children_use_gateway_once_and_resume_from_the_oms_after_checkpoint_replay() {
    let mut state = state(3);
    let risk = Risk(true);
    let mut venue = TestVenue {
        mode: VenueMode::Fill,
        submits: 0,
        cancels: 0,
    };
    let mut oms = MemoryOms::default();
    let mut seq = 0;
    let checkpoint_before_submit = state.to_json().unwrap();

    assert_eq!(
        state
            .step(9, &mut gateway(&mut venue, &mut oms, &mut seq, 9), &risk)
            .unwrap(),
        AlgoExecutionStep::NotDue {
            next_scheduled_ts: 10
        }
    );
    assert!(matches!(
        state
            .step(10, &mut gateway(&mut venue, &mut oms, &mut seq, 10), &risk)
            .unwrap(),
        AlgoExecutionStep::Submitted {
            client_order_id: 500,
            status: OrderStatus::Filled
        }
    ));

    // Simulate a crash after OMS/Venue facts commit but before the scheduler
    // persists its advanced checkpoint. The stable child id reuses that fact.
    let mut replayed = AlgoExecutionState::from_json(&checkpoint_before_submit).unwrap();
    assert!(matches!(
        replayed
            .step(10, &mut gateway(&mut venue, &mut oms, &mut seq, 10), &risk)
            .unwrap(),
        AlgoExecutionStep::Submitted {
            client_order_id: 500,
            status: OrderStatus::Filled
        }
    ));
    assert_eq!(venue.submits, 1);
    state = replayed;

    assert!(matches!(
        state
            .step(20, &mut gateway(&mut venue, &mut oms, &mut seq, 20), &risk)
            .unwrap(),
        AlgoExecutionStep::Submitted {
            client_order_id: 501,
            ..
        }
    ));
    assert!(matches!(
        state
            .step(30, &mut gateway(&mut venue, &mut oms, &mut seq, 30), &risk)
            .unwrap(),
        AlgoExecutionStep::Submitted {
            client_order_id: 502,
            ..
        }
    ));
    assert_eq!(
        state
            .step(30, &mut gateway(&mut venue, &mut oms, &mut seq, 30), &risk)
            .unwrap(),
        AlgoExecutionStep::Completed {
            filled_raw: 11,
            unfilled_raw: 0
        }
    );
    assert_eq!(venue.submits, 3);
    assert_eq!(state.status, AlgoExecutionStatus::Completed);
}

#[test]
fn parent_cancel_stops_unsent_children_and_cancels_one_open_child_per_step() {
    let mut state = state(2);
    let risk = Risk(true);
    let mut venue = TestVenue {
        mode: VenueMode::Rest,
        submits: 0,
        cancels: 0,
    };
    let mut oms = MemoryOms::default();
    let mut seq = 0;

    assert!(matches!(
        state
            .step(10, &mut gateway(&mut venue, &mut oms, &mut seq, 10), &risk)
            .unwrap(),
        AlgoExecutionStep::Submitted {
            client_order_id: 500,
            status: OrderStatus::Accepted
        }
    ));
    state.request_cancel().unwrap();
    assert_eq!(
        state
            .step(11, &mut gateway(&mut venue, &mut oms, &mut seq, 11), &risk)
            .unwrap(),
        AlgoExecutionStep::CancellationSubmitted {
            client_order_id: 500
        }
    );
    assert_eq!(
        state
            .step(12, &mut gateway(&mut venue, &mut oms, &mut seq, 12), &risk)
            .unwrap(),
        AlgoExecutionStep::Cancelled {
            filled_raw: 0,
            unfilled_raw: 11
        }
    );
    assert_eq!(venue.submits, 1);
    assert_eq!(venue.cancels, 1);
}

#[test]
fn risk_rejection_is_terminal_before_venue_side_effect_and_unknown_is_never_retried() {
    let mut rejected = state(2);
    let mut venue = TestVenue {
        mode: VenueMode::Fill,
        submits: 0,
        cancels: 0,
    };
    let mut oms = MemoryOms::default();
    let mut seq = 0;
    let result = rejected
        .step(
            10,
            &mut gateway(&mut venue, &mut oms, &mut seq, 10),
            &Risk(false),
        )
        .unwrap();
    assert!(matches!(result, AlgoExecutionStep::Failed { .. }));
    assert_eq!(venue.submits, 0);
    assert!(oms.orders.is_empty());

    let mut unknown = state(2);
    venue.mode = VenueMode::Unknown;
    let result = unknown
        .step(
            10,
            &mut gateway(&mut venue, &mut oms, &mut seq, 10),
            &Risk(true),
        )
        .unwrap();
    assert!(matches!(
        result,
        AlgoExecutionStep::ReconcileRequired { .. }
    ));
    assert_eq!(venue.submits, 1);
    let result = unknown
        .step(
            20,
            &mut gateway(&mut venue, &mut oms, &mut seq, 20),
            &Risk(true),
        )
        .unwrap();
    assert!(matches!(
        result,
        AlgoExecutionStep::ReconcileRequired { .. }
    ));
    assert_eq!(venue.submits, 1);
}

#[test]
fn construction_rejects_quantity_drift_and_checkpoint_tampering() {
    let mut input = AlgoParentOrder {
        parent_id: "parent-1".into(),
        parent_order: parent_order(11),
        plan: qx_zhenlu::plan_twap("parent-1", 10, 10, 30, 2).unwrap(),
        child_order_id_base: 500,
    };
    assert!(AlgoExecutionState::new(input.clone())
        .unwrap_err()
        .contains("数量不守恒"));
    input.plan = qx_zhenlu::plan_twap("parent-1", 11, 10, 30, 2).unwrap();
    let state = AlgoExecutionState::new(input).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    json["next_slice_position"] = serde_json::json!(1);
    assert!(AlgoExecutionState::from_json(&json.to_string()).is_err());
}

#[test]
fn market_spec_rejects_misaligned_child_before_oms_or_venue_side_effect() {
    let mut state = state(3);
    let risk = Risk(true);
    let mut venue = TestVenue {
        mode: VenueMode::Fill,
        submits: 0,
        cancels: 0,
    };
    let mut oms = MemoryOms::default();
    let mut seq = 0;
    let result = state
        .step(
            10,
            &mut gateway(&mut venue, &mut oms, &mut seq, 10)
                .with_instrument_spec(market_spec(3, 3)),
            &risk,
        )
        .unwrap();
    assert!(matches!(result, AlgoExecutionStep::Failed { .. }));
    assert_eq!(venue.submits, 0);
    assert!(oms.orders.is_empty());
}

#[test]
fn scheduler_clock_cannot_move_backwards_and_legacy_checkpoints_round_trip() {
    let mut state = state(3);
    let legacy_checkpoint = state.to_json().unwrap();
    assert!(!legacy_checkpoint.contains("last_step_ts"));
    assert_eq!(
        AlgoExecutionState::from_json(&legacy_checkpoint).unwrap(),
        state
    );

    let mut venue = TestVenue {
        mode: VenueMode::Fill,
        submits: 0,
        cancels: 0,
    };
    let mut oms = MemoryOms::default();
    let mut seq = 0;
    state
        .step(
            15,
            &mut gateway(&mut venue, &mut oms, &mut seq, 15),
            &Risk(true),
        )
        .unwrap();
    assert_eq!(state.last_step_ts, Some(15));
    assert!(state
        .step(
            14,
            &mut gateway(&mut venue, &mut oms, &mut seq, 14),
            &Risk(true)
        )
        .unwrap_err()
        .contains("时钟不得回退"));
    assert_eq!(venue.submits, 1);
}

#[test]
fn scheduler_clock_must_match_gateway_event_clock_before_mutating_state() {
    let mut state = state(1);
    let mut venue = TestVenue {
        mode: VenueMode::Fill,
        submits: 0,
        cancels: 0,
    };
    let mut oms = MemoryOms::default();
    let mut seq = 0;
    let error = state
        .step(
            10,
            &mut gateway(&mut venue, &mut oms, &mut seq, 9),
            &Risk(true),
        )
        .unwrap_err();

    assert!(error.contains("调度时间与执行网关时间不一致"));
    assert_eq!(state.last_step_ts, None);
    assert_eq!(venue.submits, 0);
    assert!(oms.orders.is_empty());
}
