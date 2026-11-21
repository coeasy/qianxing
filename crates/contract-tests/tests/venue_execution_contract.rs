//! Cross-crate execution and venue adapter contracts.
use qx_adapter::{
    BinanceSpotAuth, BinanceSpotVenue, CcxtProcessVenue, CcxtRpc, HttpRequest, HttpResponse,
    HttpTransport,
};
use qx_core::{FeeModel, InstrumentId, Order, OrderStatus, Price, Quantity, Side};
use qx_execution::*;
use qx_zhenlu::PaperVenue;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct PortState {
    orders: Vec<Order>,
    events: Vec<ExecutionEventEnvelope>,
}

impl OrderStore for PortState {
    fn orders(&self) -> Vec<Order> {
        self.orders.clone()
    }

    fn has_pending_amendment(&self, client_order_id: u64) -> bool {
        let mut pending = false;
        for envelope in &self.events {
            match &envelope.event {
                ExecutionEvent::OrderAmendmentPending { order, .. }
                    if order.client_id == client_order_id =>
                {
                    pending = true
                }
                ExecutionEvent::OrderAmended { order, .. }
                    if order.client_id == client_order_id =>
                {
                    pending = false
                }
                _ => {}
            }
        }
        pending
    }

    fn register_order(
        &mut self,
        mut order: Order,
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
        if order.status == OrderStatus::PendingSubmit {
            order.status = OrderStatus::Submitted;
        }
        self.orders.push(order);
        Ok(())
    }
}

impl EventAppender for PortState {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        match &envelope.event {
            ExecutionEvent::OrderAmendmentPending { .. } => {}
            ExecutionEvent::OrderAmended { order, .. } => {
                if let Some(current) = self
                    .orders
                    .iter_mut()
                    .find(|current| current.client_id == order.client_id)
                {
                    *current = (**order).clone();
                }
            }
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
            ExecutionEvent::Rejected {
                client_order_id, ..
            } => {
                if let Some(order) = self
                    .orders
                    .iter_mut()
                    .find(|order| order.client_id == *client_order_id)
                {
                    order.status = OrderStatus::Rejected;
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

fn assert_submit_cancel_port_contract<V: VenuePort>(
    venue: V,
    order: Order,
    expected_venue_id: &str,
) {
    let mut venue = venue;
    assert_eq!(venue.venue_id(), expected_venue_id);
    let mut state = PortState::default();
    let mut source_seq = 0;
    {
        let mut execution = PortExecutionService::new(
            &mut venue,
            &mut state,
            "contract-worker",
            100,
            &mut source_seq,
        );
        let submitted = execution.submit(order.clone()).unwrap();
        assert!(submitted.event_count >= 1);
    }
    assert!(state.events.iter().any(|event| matches!(
        &event.event,
        ExecutionEvent::Accepted { client_order_id, .. } if *client_order_id == order.client_id
    )));
    let cancelled = {
        let mut execution = PortExecutionService::new(
            &mut venue,
            &mut state,
            "contract-worker",
            100,
            &mut source_seq,
        );
        execution.cancel(order.client_id).unwrap()
    };
    assert_eq!(cancelled.event_count, 1);
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::Cancelled { client_order_id }) if *client_order_id == order.client_id
    ));
}

struct ContractCcxtRpc;

impl CcxtRpc for ContractCcxtRpc {
    fn call(&mut self, request: serde_json::Value) -> Result<serde_json::Value, String> {
        match request.get("op").and_then(serde_json::Value::as_str) {
            Some("fetch_contract_settings") => Ok(serde_json::json!({
                "settings": {"contract": false, "symbol": "BTC/USDT"}
            })),
            Some("create_order") => Ok(serde_json::json!({
                "order": {"order_id": "ccxt-contract-1", "status": "open", "filled_raw": 0}
            })),
            Some("fetch_order") => Ok(serde_json::json!({
                "order": {
                    "order_id": "ccxt-contract-1",
                    "status": "canceled",
                    "filled_raw": 0
                }
            })),
            Some("fetch_my_trades") => Ok(serde_json::json!({"trades": []})),
            _ => Ok(serde_json::json!({})),
        }
    }
}

struct ContractBinanceTransport {
    responses: Mutex<Vec<HttpResponse>>,
}

impl HttpTransport for ContractBinanceTransport {
    fn send(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
        self.responses
            .lock()
            .map_err(|_| "contract transport lock poisoned".to_string())?
            .pop()
            .ok_or_else(|| "contract response exhausted".into())
    }
}

/// 三家 Venue 的“未知结果”契约：submit 失败或超时只能留下一条待对账事实，
/// 禁止伪造 Accepted/Fill，且订单必须保持已注册状态等待远端确认。
fn assert_unknown_submit_fact_contract<V: VenuePort, F: FnOnce() -> V>(
    make_venue: F,
    order: &Order,
) {
    let mut venue = make_venue();
    let mut state = PortState::default();
    let mut source_seq = 0;
    let error = PortExecutionService::new(
        &mut venue,
        &mut state,
        "contract-worker",
        100,
        &mut source_seq,
    )
    .submit(order.clone())
    .unwrap_err();
    assert!(error.contains("待对账"), "{error}");
    assert_eq!(
        state.events.len(),
        1,
        "未知结果只能留下一条事实: {:?}",
        state.events
    );
    assert!(
        matches!(&state.events[0].event,
                ExecutionEvent::ReconcileRequired { client_order_id }
                    if *client_order_id == order.client_id),
        "{:?}",
        state.events[0].event
    );
    assert_eq!(
        state.events[0].correlation_id,
        format!("contract-worker:submit-error:{}", order.client_id)
    );
    assert_eq!(source_seq, 1);
    assert!(state
        .orders
        .iter()
        .any(|registered| registered.client_id == order.client_id));
    assert_eq!(
        state
            .orders
            .iter()
            .find(|registered| registered.client_id == order.client_id)
            .map(|registered| registered.status),
        Some(OrderStatus::Unknown),
        "未知远端结果必须保留为待对账订单"
    );

    // 控制命令可能已经被本地终结为失败，但订单级未知事实必须阻止同一
    // client_order_id 再次触发 Venue 副作用，直到对账确认远端状态。
    let retry_error = PortExecutionService::new(
        &mut venue,
        &mut state,
        "contract-worker",
        101,
        &mut source_seq,
    )
    .submit(order.clone())
    .unwrap_err();
    assert!(retry_error.contains("必须先对账"), "{retry_error}");
    assert_eq!(
        state.events.len(),
        2,
        "重复提交只可追加待对账事实，不得伪造远端回报"
    );
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::ReconcileRequired { client_order_id })
            if *client_order_id == order.client_id
    ));
}

#[test]
fn unknown_submit_results_share_the_reconcile_fact_contract() {
    let order = port_order(41);
    assert_unknown_submit_fact_contract(
        || PortVenue {
            result: Err("simulated remote timeout".into()),
        },
        &order,
    );

    struct TimeoutCcxtRpc;
    impl CcxtRpc for TimeoutCcxtRpc {
        fn call(&mut self, request: serde_json::Value) -> Result<serde_json::Value, String> {
            match request.get("op").and_then(serde_json::Value::as_str) {
                Some("create_order") => Err("ccxt create_order 超时".into()),
                _ => Ok(serde_json::json!({})),
            }
        }
    }
    assert_unknown_submit_fact_contract(
        || {
            VenuePortAdapter::new(CcxtProcessVenue::new(
                "ccxt-binance",
                Box::new(TimeoutCcxtRpc),
            ))
        },
        &order,
    );

    let transport = Arc::new(ContractBinanceTransport {
        responses: Mutex::new(vec![HttpResponse {
            status: 500,
            body: r#"{"code":-1,"msg":"internal error"}"#.into(),
        }]),
    });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
    assert_unknown_submit_fact_contract(
        move || {
            let venue = BinanceSpotVenue::with_endpoint(
                "binance",
                auth,
                transport.clone(),
                "mock.binance",
                443,
            );
            VenuePortAdapter::new(venue)
        },
        &order,
    );
}

#[test]
fn paper_ccxt_and_binance_share_submit_cancel_port_contract() {
    let order = port_order(40);
    assert_submit_cancel_port_contract(
        VenuePortAdapter::new_for_any_venue(PaperVenue::new("paper", zero_fee())),
        order.clone(),
        "paper",
    );

    assert_submit_cancel_port_contract(
        VenuePortAdapter::new(CcxtProcessVenue::new(
            "ccxt-binance",
            Box::new(ContractCcxtRpc),
        )),
        order.clone(),
        "ccxt-binance",
    );

    let transport = Arc::new(ContractBinanceTransport {
            responses: Mutex::new(vec![
                HttpResponse {
                    status: 200,
                    body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-40","status":"CANCELED","executedQty":"0","transactTime":100,"fills":[]}"#.into(),
                },
                HttpResponse {
                    status: 200,
                    body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-40","status":"NEW","executedQty":"0","transactTime":100,"fills":[]}"#.into(),
                },
            ]),
        });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
    let binance = BinanceSpotVenue::with_endpoint("binance", auth, transport, "mock.binance", 443);
    assert_submit_cancel_port_contract(VenuePortAdapter::new(binance), order, "binance");
}

#[test]
fn snapshotting_binance_is_rejected_before_execution_side_effects() {
    let mut state = PortState::default();
    let mut accepted_order = port_order(81);
    accepted_order.status = OrderStatus::Accepted;
    state.orders.push(accepted_order);
    let mut source_seq = 0;
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
