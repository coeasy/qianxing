use super::*;
use qx_zhenlu::venue_capabilities::{VenueMarketScope, VenueOrderType};
use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

const NATIVE_GROUP_TEST_CAPABILITIES: qx_zhenlu::venue_capabilities::VenueCapabilities =
    qx_zhenlu::venue_capabilities::VenueCapabilities {
        market_scope: VenueMarketScope::InstrumentVenue,
        exact_orders: true,
        order_types: &[
            VenueOrderType::Market,
            VenueOrderType::Limit,
            VenueOrderType::StopLoss,
            VenueOrderType::TakeProfit,
        ],
        ..qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES
    };

struct NoNativeGroupVenue {
    submit_calls: usize,
}

impl VenuePort for NoNativeGroupVenue {
    fn venue_id(&self) -> &str {
        "BINANCE"
    }

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        NATIVE_GROUP_TEST_CAPABILITIES
    }

    fn execution_readiness(&self) -> Result<(), String> {
        Ok(())
    }

    fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        self.submit_calls += 1;
        Ok(Vec::new())
    }

    fn cancel_order(
        &mut self,
        _client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        unreachable!()
    }
}

struct AllowGroupRisk;

impl RiskPort for AllowGroupRisk {
    fn evaluate_order(&self, _order: &Order) -> Result<RiskVerdict, String> {
        Ok(RiskVerdict::Allow)
    }
}

fn protective_leg(client_id: u64, kind: qx_core::ConditionalOrderKind, trigger: &str) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side: Side::Sell,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "port-main".into(),
        trace: None,
        policy: None,
        exact_amounts: Some(qx_core::ExactOrderAmounts {
            quantity: qx_core::ExactDecimal::parse("0.01").unwrap(),
            limit_price: None,
            filled: qx_core::ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: qx_core::ExactDecimal::ZERO,
            market_reference_price: None,
            conditional: Some(qx_core::ConditionalOrderIntent {
                kind,
                trigger_price: Some(qx_core::ExactDecimal::parse(trigger).unwrap()),
                trigger_price_type: Some(qx_core::TriggerPriceType::Mark),
                trailing_percent: None,
                trailing_amount: None,
                trailing_trigger_price: None,
            }),
        }),
    }
}

fn native_oco_intent() -> qx_core::OrderGroupIntent {
    qx_core::OrderGroupIntent::new(
        "no-native-group",
        qx_core::OrderGroupKind::Oco,
        qx_core::OrderGroupExecution::NativeRequired,
        vec![
            qx_core::OrderGroupLegIntent {
                role: qx_core::OrderGroupLegRole::TakeProfit,
                order: protective_leg(7001, qx_core::ConditionalOrderKind::TakeProfit, "70000"),
            },
            qx_core::OrderGroupLegIntent {
                role: qx_core::OrderGroupLegRole::StopLoss,
                order: protective_leg(7002, qx_core::ConditionalOrderKind::StopLoss, "60000"),
            },
        ],
    )
    .unwrap()
}

#[test]
fn venue_without_native_group_support_fails_before_store_or_oms_side_effects() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qx-execution-native-group-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let mut venue = NoNativeGroupVenue { submit_calls: 0 };
    let mut pipeline = PortState::default();
    let mut source_seq = 0;
    let error = PortExecutionService::new(
        &mut venue,
        &mut pipeline,
        "group-worker",
        100,
        &mut source_seq,
    )
    .with_order_group_store(&mut store)
    .submit_order_group(native_oco_intent(), &AllowGroupRisk)
    .unwrap_err();

    assert!(error.contains("未声明原生 OCO/Bracket"), "{error}");
    assert_eq!(venue.submit_calls, 0);
    assert!(pipeline.orders.is_empty());
    assert!(pipeline.events.is_empty());
    assert!(store.group_ids().unwrap().is_empty());
    assert_eq!(source_seq, 0);
    std::fs::remove_dir_all(root).unwrap();
}

struct NativeGroupPortVenue {
    preflight_calls: usize,
    group_submit_calls: usize,
}

impl VenuePort for NativeGroupPortVenue {
    fn venue_id(&self) -> &str {
        "BINANCE"
    }

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        NATIVE_GROUP_TEST_CAPABILITIES
    }

    fn execution_readiness(&self) -> Result<(), String> {
        Ok(())
    }

    fn validate_submit_order_group(
        &mut self,
        _group: &qx_core::OrderGroupState,
    ) -> Result<(), String> {
        self.preflight_calls += 1;
        Ok(())
    }

    fn submit_order_group(
        &mut self,
        group: &qx_core::OrderGroupState,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.group_submit_calls += 1;
        Ok(group
            .legs
            .iter()
            .map(|leg| ExecutionEvent::Accepted {
                client_order_id: leg.order.client_id,
                venue_order_id: format!("exchange-{}", leg.order.client_id),
            })
            .collect())
    }

    fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        panic!("native OCO must use one group RPC, never individual child submits")
    }

    fn cancel_order(
        &mut self,
        _client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        unreachable!()
    }
}

#[test]
fn native_oco_registers_legs_persists_exchange_ids_and_never_replays_group_rpc() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qx-execution-native-group-accepted-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let mut venue = NativeGroupPortVenue {
        preflight_calls: 0,
        group_submit_calls: 0,
    };
    let mut pipeline = PortState::default();
    let mut source_seq = 0;
    let intent = native_oco_intent();
    let result = PortExecutionService::new(
        &mut venue,
        &mut pipeline,
        "group-worker",
        100,
        &mut source_seq,
    )
    .with_order_group_store(&mut store)
    .submit_order_group(intent.clone(), &AllowGroupRisk)
    .unwrap();

    assert_eq!(result.event_count, 2);
    assert_eq!(venue.preflight_calls, 1);
    assert_eq!(venue.group_submit_calls, 1);
    assert_eq!(pipeline.orders.len(), 2);
    assert!(pipeline
        .orders
        .iter()
        .all(|order| order.status == OrderStatus::Accepted));
    let restored = store.load("no-native-group").unwrap().unwrap();
    assert_eq!(restored.status, qx_core::OrderGroupStatus::Active);
    assert!(restored.legs.iter().all(|leg| leg.venue_order_id.as_deref()
        == Some(format!("exchange-{}", leg.order.client_id).as_str())));

    let error = PortExecutionService::new(
        &mut venue,
        &mut pipeline,
        "group-worker-restarted",
        101,
        &mut source_seq,
    )
    .with_order_group_store(&mut store)
    .submit_order_group(intent, &AllowGroupRisk)
    .unwrap_err();
    assert!(error.contains("已在 OMS 中"), "{error}");
    assert_eq!(venue.group_submit_calls, 1);

    std::fs::remove_dir_all(root).unwrap();
}

struct UnknownNativeGroupPortVenue {
    group_submit_calls: usize,
}

impl VenuePort for UnknownNativeGroupPortVenue {
    fn venue_id(&self) -> &str {
        "BINANCE"
    }

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        NATIVE_GROUP_TEST_CAPABILITIES
    }

    fn execution_readiness(&self) -> Result<(), String> {
        Ok(())
    }

    fn validate_submit_order_group(
        &mut self,
        _group: &qx_core::OrderGroupState,
    ) -> Result<(), String> {
        Ok(())
    }

    fn submit_order_group(
        &mut self,
        _group: &qx_core::OrderGroupState,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.group_submit_calls += 1;
        Err("timeout after exchange accepted request".into())
    }

    fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        panic!("ambiguous native group must never fall back to separate child submits")
    }

    fn cancel_order(
        &mut self,
        _client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        unreachable!()
    }
}

#[test]
fn unknown_native_group_result_reconciles_every_leg_and_never_retries_rpc() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qx-execution-native-group-unknown-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let mut venue = UnknownNativeGroupPortVenue {
        group_submit_calls: 0,
    };
    let mut pipeline = PortState::default();
    let mut source_seq = 0;
    let intent = native_oco_intent();
    let error = PortExecutionService::new(
        &mut venue,
        &mut pipeline,
        "group-worker",
        100,
        &mut source_seq,
    )
    .with_order_group_store(&mut store)
    .submit_order_group(intent.clone(), &AllowGroupRisk)
    .unwrap_err();

    assert!(error.contains("结果未知"), "{error}");
    assert_eq!(venue.group_submit_calls, 1);
    assert!(pipeline
        .orders
        .iter()
        .all(|order| order.status == OrderStatus::Unknown));
    let reconciled = store.load("no-native-group").unwrap().unwrap();
    assert_eq!(
        reconciled.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );
    assert!(reconciled
        .legs
        .iter()
        .all(|leg| leg.order.status == OrderStatus::Unknown));

    let retry_error = PortExecutionService::new(
        &mut venue,
        &mut pipeline,
        "group-worker-restarted",
        101,
        &mut source_seq,
    )
    .with_order_group_store(&mut store)
    .submit_order_group(intent, &AllowGroupRisk)
    .unwrap_err();
    assert!(retry_error.contains("已在 OMS 中"), "{retry_error}");
    assert_eq!(venue.group_submit_calls, 1);

    std::fs::remove_dir_all(root).unwrap();
}

fn amend_command(order: &Order, command_id: u64) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("amend-{command_id}"),
        operator_id: "operator".into(),
        reason: "update active order terms".into(),
        kind: CommandKind::AmendOrder,
        target: order.client_id.to_string(),
        payload: BTreeMap::from([
            ("account_id".into(), order.account_id.clone()),
            (
                "venue_id".into(),
                order.instrument.venue.as_str().to_string(),
            ),
            ("order_json".into(), serde_json::to_string(order).unwrap()),
        ]),
        permission: Permission::Trading,
        dry_run: false,
    }
}

#[test]
fn amend_command_persists_before_rpc_and_applies_amendment_before_racing_fills() {
    let mut current = port_order(88);
    current.status = OrderStatus::Accepted;
    current.qty = Quantity::from_i64(2);
    let mut amended = current.clone();
    amended.qty = Quantity::from_i64(3);
    amended.limit = Some(Price::from_i64(101));
    let command = amend_command(&amended, 888);
    let mut state = PortState::default();
    state.orders.push(current);
    let mut venue = AmendPortVenue {
        preflight_calls: 0,
        amend_calls: 0,
        result: Ok(vec![ExecutionEvent::Fill(Box::new(qx_core::Fill {
            order_id: 88,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(101),
            ts: 21,
            ..qx_core::Fill::default()
        }))]),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "amend-worker", 20, &mut source_seq)
            .amend_command(&command)
            .expect("confirmed amendment should commit");

    assert_eq!(result.message, "AMENDED venue_events=1");
    assert_eq!(venue.preflight_calls, 1);
    assert_eq!(venue.amend_calls, 1);
    assert!(matches!(
        state.events[0].event,
        ExecutionEvent::OrderAmendmentPending { .. }
    ));
    assert!(matches!(
        state.events[1].event,
        ExecutionEvent::OrderAmended { .. }
    ));
    assert!(matches!(state.events[2].event, ExecutionEvent::Fill(_)));
    assert_eq!(state.orders[0].qty, Quantity::from_i64(3));
    assert_eq!(state.orders[0].filled, Quantity::from_i64(1));
    assert_eq!(state.orders[0].status, OrderStatus::PartiallyFilled);
    assert!(!state.has_pending_amendment(88));
}

#[test]
fn unknown_amendment_result_is_persisted_and_never_retried_by_command_redelivery() {
    let mut current = port_order(89);
    current.status = OrderStatus::Accepted;
    current.qty = Quantity::from_i64(2);
    let mut amended = current.clone();
    amended.qty = Quantity::from_i64(3);
    amended.limit = Some(Price::from_i64(101));
    let command = amend_command(&amended, 889);
    let mut state = PortState::default();
    state.orders.push(current);
    let mut venue = AmendPortVenue {
        preflight_calls: 0,
        amend_calls: 0,
        result: Err("transport timed out after write".into()),
    };
    let mut source_seq = 0;
    let error =
        PortExecutionService::new(&mut venue, &mut state, "amend-worker", 20, &mut source_seq)
            .amend_command(&command)
            .expect_err("unknown remote result must require reconciliation");
    assert!(error.contains("结果未知"), "{error}");
    assert!(state.has_pending_amendment(89));
    assert!(matches!(
        state.events[0].event,
        ExecutionEvent::OrderAmendmentPending { .. }
    ));
    assert!(matches!(
        state.events[1].event,
        ExecutionEvent::ReconcileRequired {
            client_order_id: 89
        }
    ));

    let error = PortExecutionService::new(
        &mut venue,
        &mut state,
        "amend-worker-restarted",
        30,
        &mut source_seq,
    )
    .amend_command(&command)
    .expect_err("redelivery cannot repeat an ambiguous edit RPC");
    assert!(error.contains("禁止重发"), "{error}");
    assert_eq!(venue.preflight_calls, 1);
    assert_eq!(venue.amend_calls, 1);
    assert_eq!(state.events.len(), 2);
}

#[test]
fn execution_gateway_rejects_disconnected_and_snapshotting_venues_before_side_effects() {
    let mut state = PortState::default();
    let mut source_seq = 0;
    let mut paper = PaperVenue::new("paper", zero_fee());
    paper.disconnect();
    let mut paper = VenuePortAdapter::new_for_any_venue(paper);
    let error = PortExecutionService::new(
        &mut paper,
        &mut state,
        "readiness-worker",
        10,
        &mut source_seq,
    )
    .submit(port_order(80))
    .unwrap_err();
    assert!(error.contains("尚未就绪"), "{error}");
    assert!(state.orders.is_empty() && state.events.is_empty());
    assert_eq!(source_seq, 0);

    // Cancel checks OMS state before Venue readiness. Use an order already accepted remotely
    // so this assertion reaches the disconnected Venue readiness gate.
    let mut accepted_order = port_order(81);
    accepted_order.status = OrderStatus::Accepted;
    state.orders.push(accepted_order);
    let error = PortExecutionService::new(
        &mut paper,
        &mut state,
        "readiness-worker",
        10,
        &mut source_seq,
    )
    .cancel(81)
    .unwrap_err();
    assert!(error.contains("尚未就绪"), "{error}");
    assert_eq!(state.events.len(), 0);
    assert_eq!(state.orders[0].status, OrderStatus::Accepted);
    assert_eq!(source_seq, 0);

    struct NoNetwork;
    impl HttpTransport for NoNetwork {
        fn send(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
            panic!("snapshotting venue must be rejected before network I/O")
        }
    }
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
    let mut binance = BinanceSpotVenue::testnet("binance", auth, Arc::new(NoNetwork));
    binance.reconnect(); // Transport reconnect completes before the private snapshot/reconcile.
    let mut binance = VenuePortAdapter::new(binance);
    let error = PortExecutionService::new(
        &mut binance,
        &mut state,
        "readiness-worker",
        10,
        &mut source_seq,
    )
    .submit(port_order(82))
    .unwrap_err();
    assert!(error.contains("尚未就绪"), "{error}");
    assert_eq!(state.orders.len(), 1);
    assert_eq!(state.events.len(), 0);
    assert_eq!(source_seq, 0);
    let router_error = VenueRouterPort::execution_readiness_for(&binance, "binance").unwrap_err();
    assert!(router_error.contains("尚未就绪"), "{router_error}");
    assert!(VenueRouterPort::execution_readiness_for(&binance, "okx").is_err());
}

#[test]
fn port_execution_service_registers_and_appends_standard_facts() {
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 7,
            venue_order_id: "remote-7".into(),
        }]),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
            .submit(port_order(7))
            .unwrap();
    assert_eq!(result.event_count, 1);
    assert_eq!(state.orders.len(), 1);
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.events[0].source_seq, 1);
    assert!(matches!(
        state.events[0].event,
        ExecutionEvent::Accepted { .. }
    ));
}

#[test]
fn cancel_intent_is_appended_before_venue_cancel_and_unknown_orders_require_reconcile() {
    let mut state = PortState::default();
    let mut accepted = port_order(7);
    accepted.status = OrderStatus::Accepted;
    state.orders.push(accepted);
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Cancelled { client_order_id: 7 }]),
    };
    let mut source_seq = 0;
    let outcome =
        PortExecutionService::new(&mut venue, &mut state, "cancel-worker", 20, &mut source_seq)
            .cancel(7)
            .expect("cancel should complete after its durable intent");
    assert_eq!(outcome.event_count, 1, "count only venue response facts");
    assert!(matches!(
        state.events.first().map(|event| &event.event),
        Some(ExecutionEvent::CancelPending { client_order_id: 7 })
    ));
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::Cancelled { client_order_id: 7 })
    ));
    assert_eq!(state.orders[0].status, OrderStatus::Cancelled);

    let mut unknown = port_order(84);
    unknown.status = OrderStatus::Unknown;
    state.orders.push(unknown);
    let prior_events = state.events.len();
    let mut never_called = NeverCalledVenue;
    let error = PortExecutionService::new(
        &mut never_called,
        &mut state,
        "cancel-worker",
        21,
        &mut source_seq,
    )
    .cancel(84)
    .expect_err("unknown remote order must be reconciled before cancellation");
    assert!(error.contains("必须先对账"), "{error}");
    assert_eq!(state.events.len(), prior_events);
    assert_eq!(state.orders[1].status, OrderStatus::Unknown);
}

#[test]
fn duplicate_durable_cancel_intent_does_not_repeat_venue_side_effect() {
    use qx_control::{CommandKind, ControlCommand, Permission};

    let mut state = PortState::default();
    let mut accepted = port_order(86);
    accepted.status = OrderStatus::Accepted;
    state.orders.push(accepted);
    // Simulate a second worker with a stale Accepted snapshot. The shared EventLog
    // already contains the stable intent for this command, so the append receipt
    // must stop this worker before it can contact the Venue.
    state.events.push(ExecutionEventEnvelope {
        event: ExecutionEvent::CancelPending {
            client_order_id: 86,
        },
        event_ts: 20,
        receive_ts: 20,
        source_seq: 1,
        correlation_id: "control:cancel:770:86:pending:86".into(),
    });
    let command = ControlCommand {
        command_id: 770,
        request_id: "cancel-770".into(),
        operator_id: "operator".into(),
        reason: "cancel duplicate delivery".into(),
        kind: CommandKind::CancelOrder,
        target: "86".into(),
        payload: BTreeMap::from([
            ("account_id".into(), "port-main".into()),
            ("venue_id".into(), "BINANCE".into()),
        ]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let mut venue = NeverCalledVenue;
    let mut source_seq = 1;
    let error =
        PortExecutionService::new(&mut venue, &mut state, "retry-worker", 20, &mut source_seq)
            .cancel_command(&command)
            .expect_err("duplicate CancelPending must not trigger a second Venue cancel");
    assert!(error.contains("禁止重复调用 Venue"), "{error}");
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.orders[0].status, OrderStatus::Accepted);
}

#[test]
fn preexisting_pending_submit_requires_reconciliation_before_retry() {
    let mut state = PortState::default();
    let mut pending = port_order(87);
    pending.status = OrderStatus::PendingSubmit;
    state.orders.push(pending.clone());
    let mut venue = NeverCalledVenue;
    let mut source_seq = 0;

    let error =
        PortExecutionService::new(&mut venue, &mut state, "retry-worker", 20, &mut source_seq)
            .submit(pending)
            .expect_err("a persisted pre-submit state is ambiguous after restart");

    assert!(error.contains("必须先对账"), "{error}");
    assert_eq!(source_seq, 1);
    assert_eq!(state.orders[0].status, OrderStatus::Unknown);
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::ReconcileRequired {
            client_order_id: 87
        })
    ));
}

#[test]
fn port_execution_service_fails_closed_on_empty_venue_response() {
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(Vec::new()),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
            .submit(port_order(8));
    assert!(result.is_err());
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::ReconcileRequired { client_order_id: 8 })
    ));
}

#[test]
fn port_execution_service_fails_closed_on_invalid_venue_fact() {
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 999,
            venue_order_id: "wrong-order".into(),
        }]),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
            .submit(port_order(9));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("非法下单事实"));
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::ReconcileRequired { client_order_id: 9 })
    ));
}

#[test]
fn port_execution_service_rejects_before_registration_or_venue_side_effect() {
    let mut state = PortState::default();
    let mut venue = NeverCalledVenue;
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "risk-worker", 10, &mut source_seq)
            .submit_with_risk(port_order(12), &RejectingRisk);

    assert_eq!(
        result.unwrap_err().to_string(),
        "账户级 RiskPort 拒绝订单: max_notional"
    );
    assert!(state.orders.is_empty());
    assert!(state.events.is_empty());
    assert_eq!(source_seq, 0);
}

#[test]
fn undeclared_order_type_is_rejected_before_registration_or_venue_side_effect() {
    struct MarketOnlyVenue {
        submits: usize,
    }
    impl VenuePort for MarketOnlyVenue {
        fn venue_id(&self) -> &str {
            "market-only"
        }
        fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
            use qx_zhenlu::venue_capabilities::{
                VenueCapabilities, VenueMarketScope, VenueOrderType,
            };
            VenueCapabilities {
                market_scope: VenueMarketScope::InstrumentVenue,
                exact_orders: false,
                order_types: &[VenueOrderType::Market],
                data_tiers: &[],
                cancel: false,
                query: false,
                reconciliation: false,
                idempotent_client_order_id: false,
            }
        }
        fn execution_readiness(&self) -> Result<(), String> {
            Ok(())
        }
        fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
            self.submits += 1;
            Ok(Vec::new())
        }
        fn cancel_order(
            &mut self,
            _client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            panic!("undeclared cancel must be rejected before venue call")
        }
    }

    let mut state = PortState::default();
    let mut venue = MarketOnlyVenue { submits: 0 };
    let mut source_seq = 0;
    let error = PortExecutionService::new(
        &mut venue,
        &mut state,
        "capability-worker",
        10,
        &mut source_seq,
    )
    .submit(port_order(13))
    .unwrap_err();

    assert!(error.contains("未声明 Limit 订单能力"), "{error}");
    assert!(state.orders.is_empty());
    assert!(state.events.is_empty());
    assert_eq!(venue.submits, 0);
    assert_eq!(source_seq, 0);
    let cancel_error = PortExecutionService::new(
        &mut venue,
        &mut state,
        "capability-worker",
        10,
        &mut source_seq,
    )
    .cancel(13)
    .unwrap_err();
    assert!(cancel_error.contains("未声明撤单能力"), "{cancel_error}");
}

#[test]
fn venue_market_query_and_reconciliation_capabilities_are_enforced() {
    use qx_zhenlu::venue_capabilities::{
        VenueCapabilities, BINANCE_SPOT_VENUE_CAPABILITIES, CCXT_PROCESS_VENUE_CAPABILITIES,
    };

    let mut ashare = port_order(14);
    ashare.instrument = InstrumentId::parse("600000.XSHG").unwrap();
    assert!(BINANCE_SPOT_VENUE_CAPABILITIES
        .validate_submit(&ashare, "binance")
        .unwrap_err()
        .contains("market 不在 Venue 声明范围内"));

    struct BinanceScopeVenue;
    impl VenuePort for BinanceScopeVenue {
        fn venue_id(&self) -> &str {
            "binance"
        }
        fn capabilities(&self) -> VenueCapabilities {
            BINANCE_SPOT_VENUE_CAPABILITIES
        }
        fn execution_readiness(&self) -> Result<(), String> {
            Ok(())
        }
        fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
            panic!("out-of-scope market must be rejected before submission")
        }
        fn cancel_order(
            &mut self,
            _client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            panic!("this test must not call cancel")
        }
    }
    let mut state = PortState::default();
    let mut venue = BinanceScopeVenue;
    let mut source_seq = 0;
    let error = PortExecutionService::new(
        &mut venue,
        &mut state,
        "market-scope-worker",
        10,
        &mut source_seq,
    )
    .submit(ashare.clone())
    .unwrap_err();
    assert!(error.contains("market 不在 Venue 声明范围内"), "{error}");
    assert!(state.orders.is_empty() && state.events.is_empty());
    assert_eq!(source_seq, 0);

    let mut derivative = port_order(15);
    derivative.instrument = InstrumentId::parse("BTCUSDT-PERP.BINANCE").unwrap();
    assert!(BINANCE_SPOT_VENUE_CAPABILITIES
        .validate_submit(&derivative, "binance")
        .unwrap_err()
        .contains("market 不在 Venue 声明范围内"));

    let spot = port_order(16);
    assert!(CCXT_PROCESS_VENUE_CAPABILITIES
        .validate_submit(&spot, "okx")
        .unwrap_err()
        .contains("market 不在 Venue 声明范围内"));

    let no_reads = VenueCapabilities {
        query: false,
        reconciliation: false,
        ..qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES
    };
    assert!(no_reads
        .validate_query()
        .unwrap_err()
        .contains("未声明查询能力"));
    assert!(no_reads
        .validate_reconciliation()
        .unwrap_err()
        .contains("未声明对账能力"));
}

/// 风控端口的两条通道必须各说各话（V13 §9.12 #172）。
///
/// 改前：两个生产 `RiskPort` 实现只在通过时写 `accepted: true`，拒绝改走 `Err`，
/// 于是 `submit_with_risk` 里「账户级 RiskPort 拒绝订单」那条播报在生产不可达，
/// 一次正常的风控拒绝被上层报成「执行失败」——运维据此去查链路，而链路是好的。
#[test]
fn canonical_risk_port_separates_verdict_from_port_failure() {
    let accepted_context = qx_risk::OrderRiskContext::default();
    assert_eq!(
        CanonicalRiskPort {
            context: &accepted_context,
        }
        .evaluate_order(&port_order(10))
        .unwrap(),
        RiskVerdict::Allow
    );

    // 带账户限额却没有产品规格：fail-closed 的拒绝（P0a 口径），不是端口故障。
    let rejected_context = qx_risk::OrderRiskContext {
        max_order_notional_raw: Some(1),
        ..qx_risk::OrderRiskContext::default()
    };
    let verdict = CanonicalRiskPort {
        context: &rejected_context,
    }
    .evaluate_order(&port_order(11))
    .expect("风控拒绝是端口给出的裁决，不是端口故障");
    let RiskVerdict::Reject { reason } = verdict else {
        panic!("缺规格的超限订单必须给出拒绝裁决: {verdict:?}")
    };
    // 拒绝理由要能就地复核：规则集版本 + 具体违规，二者缺一不可。
    assert!(
        reason.contains(qx_risk::ORDER_RISK_RULE_SET_VERSION),
        "拒绝理由必须点名规则集版本: {reason}"
    );
    assert!(
        reason.contains("TradingInstrumentSpec"),
        "拒绝理由要带违规原文: {reason}"
    );

    // 同一个拒绝经统一提交入口播报时不得说成「执行失败」，且不得留下伪订单。
    let mut state = PortState::default();
    let mut venue = NeverCalledVenue;
    let mut source_seq = 0;
    let error =
        PortExecutionService::new(&mut venue, &mut state, "risk-worker", 10, &mut source_seq)
            .submit_with_risk(
                port_order(11),
                &CanonicalRiskPort {
                    context: &rejected_context,
                },
            )
            .unwrap_err();
    assert!(
        error.starts_with("账户级 RiskPort 拒绝订单: "),
        "风控拒绝不得报成端口故障: {error}"
    );
    assert!(!error.contains("执行失败"), "两条通道又混了: {error}");
    assert!(state.orders.is_empty() && state.events.is_empty());
    assert_eq!(source_seq, 0);
}
