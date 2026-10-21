use super::*;
use qx_core::{InstrumentId, QxError};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn credentials_redact_secret_and_construct_auth() {
    let credentials = BinanceSpotCredentials::new("key", "super-secret").unwrap();
    let debug = format!("{credentials:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("super-secret"));
    assert_eq!(credentials.api_key(), "key");
    assert!(credentials.into_auth().is_ok());
}

#[test]
fn credentials_load_from_projected_files_and_reject_control_bytes() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-binance-credentials-{}-{}",
            std::process::id(),
            UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    let key_path = root.join("api-key");
    let secret_path = root.join("secret");
    std::fs::write(&key_path, b"api-key\r\n").unwrap();
    std::fs::write(&secret_path, b"api-secret\n").unwrap();
    let credentials = BinanceSpotCredentials::from_files(&key_path, &secret_path).unwrap();
    assert_eq!(credentials.api_key(), "api-key");

    std::fs::write(&secret_path, b"bad\nsecret").unwrap();
    assert!(BinanceSpotCredentials::from_files(&key_path, &secret_path).is_err());
    let _ = std::fs::remove_file(key_path);
    let _ = std::fs::remove_file(secret_path);
    let _ = std::fs::remove_dir(root);
}

#[test]
fn user_stream_run_config_rejects_missing_endpoint_context() {
    let policy = BinanceStreamRetryPolicy::new(
        2,
        std::time::Duration::from_millis(1),
        std::time::Duration::from_millis(10),
    )
    .unwrap();
    assert!(
        BinanceUserStreamRunConfig::new("", "id", std::time::Duration::from_secs(1), policy)
            .is_err()
    );
    assert!(BinanceUserStreamRunConfig::new(
        "ws-api.binance.com",
        "",
        std::time::Duration::from_secs(1),
        policy
    )
    .is_err());
    assert!(BinanceUserStreamRunConfig::new(
        "ws-api.binance.com",
        "id",
        std::time::Duration::ZERO,
        policy
    )
    .is_err());
}

#[test]
fn binance_hmac_matches_official_ascii_payload() {
    let auth = BinanceSpotAuth::with_clock(
        "vmPUZE6mv9SD5VNHk4HlWFsOr6aKE2zvsw0MuIgwCIPy6utIco14y7Ju91duEh8A",
        b"NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j",
        || 1_499_827_319_559,
    )
    .unwrap();
    let parameters: Vec<(String, String)> = [
        ("symbol", "LTCBTC"),
        ("side", "BUY"),
        ("type", "LIMIT"),
        ("timeInForce", "GTC"),
        ("quantity", "1"),
        ("price", "0.1"),
        ("recvWindow", "5000"),
        ("timestamp", "1499827319559"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    // 官方样例的 query 顺序不是 BTreeMap 字典序，因此直接钉住“签名的就是发出去的那段
    // 编码串”这一条私有能力；公开的 ordered 入口没有任何生产读者（V12 §16）。
    assert_eq!(
        auth.sign_encoded_payload(&form_encode_pairs(&parameters)),
        "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
    );
}

#[test]
fn binance_user_stream_subscription_payload_is_signed_and_explicit() {
    let auth = BinanceSpotAuth::with_clock("api-key", b"secret", || 1_700_000_000_123).unwrap();
    let payload: Value =
        serde_json::from_str(&auth.user_stream_subscribe_payload("request-1")).unwrap();
    assert_eq!(payload["id"], "request-1");
    assert_eq!(payload["method"], "userDataStream.subscribe.signature");
    assert_eq!(payload["params"]["apiKey"], "api-key");
    assert_eq!(payload["params"]["timestamp"], 1_700_000_000_123_u64);
    assert_eq!(payload["params"]["recvWindow"], 5_000_u64);
    let mut parameters = BTreeMap::new();
    parameters.insert("apiKey".into(), "api-key".into());
    parameters.insert("recvWindow".into(), "5000".into());
    parameters.insert("timestamp".into(), "1700000000123".into());
    assert_eq!(
        payload["params"]["signature"],
        auth.sign_parameters(&parameters)
    );
}

#[test]
fn binance_book_ticker_maps_to_l1_quote_and_rejects_crossed_market() {
    let (symbol, quote) = parse_book_ticker(
        r#"{"u":42,"s":"BTCUSDT","b":"100.10","B":"2.5","a":"100.20","A":"3.0"}"#,
        99,
    )
    .unwrap();
    assert_eq!(symbol, "BTCUSDT");
    assert_eq!(quote.ts, 99);
    assert_eq!(quote.source_seq, 42);
    assert_eq!(quote.bid, Price::from_dec("100.10").unwrap());
    assert_eq!(quote.ask_qty, Quantity::from_dec("3.0").unwrap());
    assert!(parse_book_ticker(
        r#"{"u":43,"s":"BTCUSDT","b":"100.30","B":"1","a":"100.20","A":"1"}"#,
        100,
    )
    .is_err());
}

#[test]
fn binance_market_data_fetches_public_rest_book_ticker() {
    let transport = Arc::new(MockTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![HttpResponse {
            status: 200,
            body: r#"{"u":9,"s":"BTCUSDT","b":"100","B":"1","a":"101","A":"2"}"#.into(),
        }]),
    });
    let mut market = BinanceSpotMarketData::with_endpoint(transport.clone(), "mock.binance", 443);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let quote = market.fetch_book_ticker(&instrument, 123).unwrap();
    assert_eq!(quote.mid(), Some(Price::from_dec("100.5").unwrap()));
    let request = transport.requests.lock().unwrap().pop().unwrap();
    assert_eq!(request.path, "/api/v3/ticker/bookTicker?symbol=BTCUSDT");
    assert!(request.headers.is_empty());
}

#[test]
fn binance_book_ticker_accepts_official_rest_field_names() {
    let (symbol, quote) = parse_book_ticker(
            r#"{"symbol":"BTCUSDT","bidPrice":"100.10","bidQty":"2.5","askPrice":"100.20","askQty":"3.0"}"#,
            99,
        )
        .unwrap();
    assert_eq!(symbol, "BTCUSDT");
    assert_eq!(quote.ts, 99);
    assert_eq!(quote.bid, Price::from_dec("100.10").unwrap());
    assert_eq!(quote.ask_qty, Quantity::from_dec("3.0").unwrap());
}

#[test]
fn binance_account_snapshot_and_position_events_update_balances() {
    let transport = Arc::new(MockTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![HttpResponse {
            status: 200,
            body: r#"{"balances":[{"asset":"USDT","free":"12.5","locked":"1"}]}"#.into(),
        }]),
    });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
    let mut venue =
        BinanceSpotVenue::with_endpoint("binance", auth, transport.clone(), "mock.binance", 443);
    let balances = venue.fetch_account_balances(100).unwrap();
    assert_eq!(balances[0].asset, "USDT");
    assert_eq!(balances[0].free, Money::from_dec("12.5").unwrap());
    let request = transport.requests.lock().unwrap().pop().unwrap();
    assert!(request.path.starts_with("/api/v3/account?"));
    venue
        .ingest_user_event(
            r#"{"e":"outboundAccountPosition","E":120,"B":[{"a":"USDT","f":"13","l":"0.5"}]}"#,
        )
        .unwrap();
    assert_eq!(venue.balances()[0].free, Money::from_dec("13").unwrap());
    assert_eq!(venue.last_event_ts, 120);
}

#[test]
fn restored_orders_are_available_before_user_stream_events() {
    let (mut venue, _transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","transactTime":1700000000000,"fills":[]}"#,
    );
    venue.restore_orders([order()]).unwrap();
    let events = venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"NEW","X":"NEW","i":42,"T":1700000000100}"#,
            )
            .unwrap();
    assert!(matches!(
        events.as_slice(),
        [VenueEvent::Accepted {
            client_order_id: 7,
            ..
        }]
    ));
}

#[test]
fn binance_reconcile_uses_all_orders_and_restores_remote_order_id() {
    let (mut venue, transport) = venue(
        r#"[{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","executedQty":"0"}]"#,
    );
    let mut local = order();
    local.status = OrderStatus::Working;
    venue.restore_orders([local]).unwrap();
    venue.set_reconcile_symbols(["BTCUSDT"]).unwrap();
    venue.reconnect();

    let issues = venue.fetch_and_reconcile().unwrap();

    assert!(issues.is_empty());
    assert_eq!(venue.state, ConnectorState::Live);
    assert_eq!(venue.venue_order_ids.get(&7), Some(&"42".to_string()));
    let request = transport.requests.lock().unwrap().pop().unwrap();
    assert!(request.path.starts_with("/api/v3/allOrders?"));
    assert!(request.path.contains("limit=1000"));
    assert!(request.path.contains("symbol=BTCUSDT"));
}

struct MockTransport {
    requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<Vec<HttpResponse>>,
}

impl HttpTransport for MockTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| "mock response exhausted".into())
    }
}

fn order() -> Order {
    Order {
        client_id: 7,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: Some(Price::from_dec("100").unwrap()),
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "spot-a".into(),
        trace: None,
        policy: None,
    }
}

fn venue(response: &str) -> (BinanceSpotVenue, Arc<MockTransport>) {
    let transport = Arc::new(MockTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![HttpResponse {
            status: 200,
            body: response.into(),
        }]),
    });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 1_700_000_000_000).unwrap();
    (
        BinanceSpotVenue::with_endpoint(
            "binance-spot",
            auth,
            transport.clone(),
            "mock.binance",
            443,
        ),
        transport,
    )
}

#[test]
fn binance_submit_is_form_signed_and_maps_full_fill() {
    let (mut venue, transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"orderListId":-1,"clientOrderId":"qx-7","transactTime":1700000000100,"price":"100","origQty":"1","executedQty":"1","status":"FILLED","timeInForce":"GTC","type":"LIMIT","side":"BUY","fills":[{"price":"100","qty":"1","commission":"0.001","commissionAsset":"BTC","tradeId":9,"transactTime":1700000000101}]}"#,
    );
    let events = venue.submit(order(), 1_700_000_000_000).unwrap();
    assert!(matches!(events[0], VenueEvent::Accepted { .. }));
    assert!(matches!(events[1], VenueEvent::Fill(ref fill) if fill.qty == Quantity::from_i64(1)));
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Filled);
    let request = &transport.requests.lock().unwrap()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/v3/order");
    assert!(request.body.contains("newClientOrderId=qx-7"));
    assert!(request.body.contains("signature="));
    assert_eq!(request.headers.get("X-MBX-APIKEY"), Some(&"key".into()));
}

#[test]
fn binance_rejects_invalid_lifecycle_orders_before_transport_io() {
    let (mut venue, transport) = venue("");
    let mut invalid_spec = order();
    invalid_spec.qty = Quantity::ZERO;
    assert!(matches!(
        venue.submit(invalid_spec, 1_700_000_000_000),
        Err(QxError::BusinessViolation(message)) if message == "订单数量必须为正"
    ));

    let mut invalid_state = order();
    invalid_state.status = OrderStatus::Working;
    assert!(matches!(
        venue.submit(invalid_state, 1_700_000_000_000),
        Err(QxError::BusinessViolation(message)) if message == "Binance Spot 只接受待提交或已提交订单"
    ));
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[test]
fn binance_unknown_cancel_state_fails_closed_before_transport_io() {
    let (mut venue, transport) = venue("");
    let mut unknown = order();
    unknown.status = OrderStatus::Unknown;
    venue.restore_orders([unknown]).unwrap();

    assert!(matches!(
        venue.cancel(7, 1_700_000_000_000),
        Err(QxError::ReconcileRequired(_))
    ));
    assert!(transport.requests.lock().unwrap().is_empty());
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Unknown);
}

#[test]
fn binance_terminal_cache_retirement_cascades_to_remote_ids_and_fill_keys() {
    let (mut venue, _transport) = venue("");
    let mut orders = Vec::with_capacity(crate::venue_cache::MAX_CACHED_ORDERS + 1);
    for client_id in 1..=crate::venue_cache::MAX_CACHED_ORDERS as u64 + 1 {
        let mut terminal = order();
        terminal.client_id = client_id;
        terminal.status = OrderStatus::Cancelled;
        orders.push(terminal);
    }
    venue.venue_order_ids.insert(1, "remote-1".into());
    venue.seen_fill_keys.insert((1, 9, 1, 100, 1));

    venue.restore_orders(orders).unwrap();

    assert!(!venue.orders.contains_key(&1));
    assert!(!venue.venue_order_ids.contains_key(&1));
    assert!(!venue.seen_fill_keys.iter().any(|key| key.0 == 1));
    assert!(venue
        .orders
        .contains_key(&(crate::venue_cache::MAX_CACHED_ORDERS as u64 + 1)));
}

#[test]
fn binance_submit_events_reach_live_pipeline_and_ledger() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-adapter-pipeline-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut pipeline = qx_runtime::LiveEventPipeline::open(&root, "binance", "USDT").unwrap();
    let local_order = order();
    pipeline.register_order(local_order.clone(), 1).unwrap();
    let (mut venue, _transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"orderListId":-1,"clientOrderId":"qx-7","transactTime":1700000000100,"price":"100","origQty":"1","executedQty":"1","status":"FILLED","timeInForce":"GTC","type":"LIMIT","side":"BUY","fills":[{"price":"100","qty":"1","commission":"0.001","commissionAsset":"BTC","tradeId":9,"transactTime":1700000000101}]}"#,
    );
    let events = venue.submit(local_order, 100).unwrap();
    let mut source_seq = 0_u64;
    for event in events {
        source_seq += 1;
        let (event, ts) = match event {
            VenueEvent::Accepted {
                client_order_id,
                ts,
                ..
            } => (
                qx_runtime::RuntimeExternalEvent::Accepted {
                    client_order_id,
                    venue_order_id: None,
                },
                ts,
            ),
            VenueEvent::Fill(fill) => {
                let ts = fill.ts;
                (qx_runtime::RuntimeExternalEvent::Fill { fill }, ts)
            }
            VenueEvent::Cancelled {
                client_order_id,
                ts,
            } => (
                qx_runtime::RuntimeExternalEvent::Cancelled { client_order_id },
                ts,
            ),
        };
        pipeline
            .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                event,
                ts,
                ts,
                source_seq,
                format!("binance:{source_seq}"),
            ))
            .unwrap();
    }
    assert_eq!(pipeline.ledger().entries().len(), 3);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert_eq!(pipeline.snapshot().log_len, 6);
    let restored = qx_runtime::LiveEventPipeline::open(&root, "binance", "USDT").unwrap();
    assert_eq!(restored.snapshot(), pipeline.snapshot());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn user_event_is_idempotent_and_cancel_is_mapped() {
    let (mut venue, transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","transactTime":1700000000000,"fills":[]}"#,
    );
    venue.submit(order(), 1_700_000_000_000).unwrap();
    let first = venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100,"t":11}"#,
            )
            .unwrap();
    assert!(matches!(first.as_slice(), [VenueEvent::Fill(_)]));
    assert!(venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100,"t":11}"#,
            )
            .unwrap()
            .is_empty());
    *transport.responses.lock().unwrap() = vec![HttpResponse {
        status: 200,
        body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"CANCELED"}"#
            .into(),
    }];
    let canceled = venue.cancel(7, 1_700_000_000_100).unwrap();
    assert!(matches!(
        canceled.as_slice(),
        [VenueEvent::Cancelled { .. }]
    ));
}

#[test]
fn user_event_without_trade_id_requires_reconcile_instead_of_zeroing_it() {
    let (mut venue, _transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","transactTime":1700000000000,"fills":[]}"#,
    );
    venue.submit(order(), 1_700_000_000_000).unwrap();
    // 缺 "t" 字段的成交回报必须转对账，不得把 trade_id 静默归零：trade_id 是
    // seen_fill_keys 去重键的一部分，归零会让两笔都没有 t 的成交互相误去重，
    // 漏计一笔成交且无人告警。CCXT 侧（ccxt.rs）对同一情形本来就 fail-closed。
    let error = venue
            .ingest_user_event(
                r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100}"#,
            )
            .expect_err("缺 trade_id 的成交回报必须被拒，不能当成有效成交入账");
    assert!(
        matches!(error, QxError::ReconcileRequired(_)),
        "缺 trade_id 应转对账而不是静默归零，实际 {error:?}"
    );
}

#[test]
fn binance_five_xx_is_ambiguous_and_requires_reconcile() {
    let transport = Arc::new(MockTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![HttpResponse {
            status: 500,
            body: r#"{"code":-1000,"msg":"unknown"}"#.into(),
        }]),
    });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 1_700_000_000_000).unwrap();
    let mut venue =
        BinanceSpotVenue::with_endpoint("binance-spot", auth, transport, "mock.binance", 443);
    assert!(matches!(
        venue.submit(order(), 1_700_000_000_000),
        Err(QxError::Ambiguous(_))
    ));
    assert_eq!(venue.state, ConnectorState::ReconcileRequired);
}

#[path = "tests/lifecycle_cancel.rs"]
mod lifecycle_cancel;
