use super::*;

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
            Some("create_order") => Ok(serde_json::json!({
                "order": {"order_id": "ccxt-contract-1", "status": "open", "filled_raw": 0}
            })),
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
}

#[test]
fn paper_ccxt_and_binance_share_unknown_submit_fact_contract() {
    let order = port_order(41);
    assert_unknown_submit_fact_contract(
        || {
            let mut venue = PaperVenue::new("paper");
            venue.disconnect();
            VenuePortAdapter::new_for_any_venue(venue)
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
        VenuePortAdapter::new_for_any_venue(PaperVenue::new("paper")),
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
                    body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-40","status":"CANCELED","transactTime":100,"fills":[]}"#.into(),
                },
                HttpResponse {
                    status: 200,
                    body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-40","status":"NEW","transactTime":100,"fills":[]}"#.into(),
                },
            ]),
        });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
    let binance = BinanceSpotVenue::with_endpoint("binance", auth, transport, "mock.binance", 443);
    assert_submit_cancel_port_contract(VenuePortAdapter::new(binance), order, "binance");
}
