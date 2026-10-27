use super::*;
use qx_core::{
    ExactDecimal, ExactOrderAmounts, ExactTradingRules, InstrumentId, QxError,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn binance_remote_fill_projection_keeps_sub_nano_exact_quantity() {
    let (raw, exact) = parse_exact_quantity_projection("0.0000000001", "executedQty").unwrap();
    assert_eq!(raw.raw(), 0);
    assert_eq!(exact, ExactDecimal::parse("0.0000000001").unwrap());

    let (raw, exact) = parse_exact_quantity_projection("1.0000000019", "executedQty").unwrap();
    assert_eq!(raw.raw(), 1_000_000_001);
    assert_eq!(exact, ExactDecimal::parse("1.0000000019").unwrap());
    assert!(parse_exact_quantity_projection("-0.1", "executedQty").is_err());
}

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
    assert_eq!(
        quote.bid_qty_exact,
        Some(qx_core::ExactDecimal::parse("2.5").unwrap())
    );
    assert_eq!(
        quote.ask_qty_exact,
        Some(qx_core::ExactDecimal::parse("3.0").unwrap())
    );
    assert!(parse_book_ticker(
        r#"{"u":43,"s":"BTCUSDT","b":"100.30","B":"1","a":"100.20","A":"1"}"#,
        100,
    )
    .is_err());
}

#[test]
fn binance_book_ticker_preserves_sub_nano_displayed_quantities() {
    let (_, quote) = parse_book_ticker(
        r#"{"u":44,"s":"TINYUSDT","b":"0.01","B":"0.0000000001","a":"0.02","A":"0.0000000002"}"#,
        100,
    )
    .unwrap();
    assert_eq!(quote.bid_qty, Quantity::ZERO);
    assert_eq!(quote.ask_qty, Quantity::ZERO);
    assert_eq!(
        quote.bid_qty_exact,
        Some(qx_core::ExactDecimal::parse("0.0000000001").unwrap())
    );
    assert_eq!(
        quote.ask_qty_exact,
        Some(qx_core::ExactDecimal::parse("0.0000000002").unwrap())
    );
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
fn binance_account_balances_preserve_subnano_coin_amounts() {
    let transport = Arc::new(MockTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![HttpResponse {
            status: 200,
            body:
                r#"{"balances":[{"asset":"SHIB","free":"0.0000000001","locked":"0.0000000002"}]}"#
                    .into(),
        }]),
    });
    let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
    let mut venue =
        BinanceSpotVenue::with_endpoint("binance", auth, transport, "mock.binance", 443);
    let balance = venue.fetch_account_balances(100).unwrap().remove(0);
    assert_eq!(balance.free, Money::ZERO);
    assert_eq!(balance.locked, Money::ZERO);
    assert_eq!(balance.free_exact.to_string(), "0.0000000001");
    assert_eq!(balance.locked_exact.to_string(), "0.0000000002");

    venue
        .ingest_user_event(
            r#"{"e":"outboundAccountPosition","E":120,"B":[{"a":"SHIB","f":"0.0000000003","l":"0.0000000004"}]}"#,
        )
        .unwrap();
    assert_eq!(venue.balances()[0].free_exact.to_string(), "0.0000000003");
    assert_eq!(venue.balances()[0].locked_exact.to_string(), "0.0000000004");
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
        exact_amounts: None,
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

fn binance_btcusdt_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        product: TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: Price::from_dec("0.01").unwrap().raw(),
        qty_step: Quantity::from_dec("0.00001").unwrap().raw(),
        min_qty: Quantity::from_dec("0.00001").unwrap().raw(),
        min_notional_raw: Money::from_dec("5").unwrap().raw(),
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
        exact_rules: None,
    }
}

const BTCUSDT_EXCHANGE_INFO: &str = r#"{"symbols":[{"symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT","filters":[{"filterType":"PRICE_FILTER","minPrice":"0.01000000","maxPrice":"1000000.00000000","tickSize":"0.01000000"},{"filterType":"LOT_SIZE","minQty":"0.00001000","maxQty":"9000.00000000","stepSize":"0.00001000"},{"filterType":"MARKET_LOT_SIZE","minQty":"0.00010000","maxQty":"100.00000000","stepSize":"0.00010000"},{"filterType":"MIN_NOTIONAL","minNotional":"5.00000000","applyToMarket":true,"avgPriceMins":5}]}]}"#;

fn subnano_btcusdt_exchange_info() -> String {
    BTCUSDT_EXCHANGE_INFO
        .replace("0.01000000", "0.0000000001")
        .replace("0.00001000", "0.0000000001")
        .replace("0.00010000", "0.0000000001")
        .replace("5.00000000", "0.00000000")
}

fn subnano_btcusdt_spec() -> TradingInstrumentSpec {
    let mut spec = binance_btcusdt_spec();
    spec.price_tick = 0;
    spec.qty_step = 0;
    spec.min_qty = 0;
    spec.min_notional_raw = 0;
    spec.exact_rules = Some(ExactTradingRules {
        contract_size: ExactDecimal::ONE,
        price_tick: ExactDecimal::parse("0.0000000001").unwrap(),
        quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        minimum_notional: ExactDecimal::ZERO,
    });
    spec
}

#[test]
fn binance_exact_market_rules_and_orders_keep_subnano_precision() {
    let exchange_info = subnano_btcusdt_exchange_info();
    let (mut venue, transport) = venue(&exchange_info);
    let spec = subnano_btcusdt_spec();
    venue.validate_instrument_spec(&spec).unwrap();

    let mut exact = order();
    exact.qty = Quantity::ZERO;
    exact.limit = None;
    exact.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        limit_price: Some(ExactDecimal::parse("100.0000000001").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: ExactDecimal::ZERO,
    });
    venue.validate_order_constraints(&spec, &exact).unwrap();
    assert_eq!(
        venue.market_rules["BTCUSDT"].exact_qty_step.to_string(),
        "0.0000000001"
    );
    assert_eq!(
        venue.market_rules["BTCUSDT"].exact_price_tick.to_string(),
        "0.0000000001"
    );
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[test]
fn binance_exact_submit_and_fill_preserve_subnano_amounts_and_fee() {
    let (mut venue, _) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"FILLED","fills":[{"price":"100.0000000001","qty":"0.0000000001","commission":"0.0000000001","commissionAsset":"BTC","tradeId":9,"transactTime":1700000000101}]}"#,
    );
    let mut exact = order();
    exact.qty = Quantity::ZERO;
    exact.limit = None;
    exact.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        limit_price: Some(ExactDecimal::parse("100.0000000001").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: ExactDecimal::ZERO,
    });

    let events = venue.submit(exact, 1_700_000_000_000).unwrap();
    let fill = match &events[1] {
        VenueEvent::Fill(fill) => fill,
        other => panic!("expected exact fill, got {other:?}"),
    };
    assert_eq!(fill.qty, Quantity::ZERO);
    assert_eq!(fill.price, Price::ZERO);
    assert_eq!(fill.fee, Money::ZERO);
    assert_eq!(
        fill.exact_amounts.unwrap().quantity.to_string(),
        "0.0000000001"
    );
    assert_eq!(
        fill.exact_amounts.unwrap().price.to_string(),
        "100.0000000001"
    );
    assert_eq!(fill.exact_fees[0].amount.to_string(), "0.0000000001");
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Filled);
    assert_eq!(
        venue.snapshot()[0].filled_exact.unwrap().to_string(),
        "0.0000000001"
    );
}

fn btcusdt_dynamic_filters() -> String {
    BTCUSDT_EXCHANGE_INFO.replace(
        r#"{"filterType":"MIN_NOTIONAL","minNotional":"5.00000000","applyToMarket":true,"avgPriceMins":5}"#,
        r#"{"filterType":"PERCENT_PRICE","multiplierDown":"0.5","multiplierUp":"1.5","avgPriceMins":5},{"filterType":"PERCENT_PRICE_BY_SIDE","bidMultiplierDown":"0.8","bidMultiplierUp":"1.2","askMultiplierDown":"0.9","askMultiplierUp":"1.1","avgPriceMins":5},{"filterType":"MIN_NOTIONAL","minNotional":"5.00000000","applyToMarket":true,"avgPriceMins":5}"#,
    )
}

fn btcusdt_max_position_filters() -> String {
    BTCUSDT_EXCHANGE_INFO.replace(
        r#"{"filterType":"MIN_NOTIONAL","minNotional":"5.00000000","applyToMarket":true,"avgPriceMins":5}"#,
        r#"{"filterType":"MIN_NOTIONAL","minNotional":"5.00000000","applyToMarket":true,"avgPriceMins":5},{"filterType":"MAX_POSITION","maxPosition":"1.00000000"}"#,
    )
}

#[test]
fn binance_max_position_counts_account_base_balance_and_other_market_open_buys() {
    let exchange_info = btcusdt_max_position_filters();
    let (mut venue, transport) = venue(&exchange_info);
    let spec = binance_btcusdt_spec();
    venue.validate_instrument_spec(&spec).unwrap();

    // The account also has an open BUY on BTCUSDC. Count its unfilled remainder together
    // with BTC free/locked balances before the new BTCUSDT buy.
    let full_exchange_info = r#"{"symbols":[{"symbol":"BTCUSDT","baseAsset":"BTC"},{"symbol":"BTCUSDC","baseAsset":"BTC"},{"symbol":"ETHUSDT","baseAsset":"ETH"}]}"#;
    let mut responses = transport.responses.lock().unwrap();
    responses.push(HttpResponse {
        status: 200,
        body: r#"{"balances":[{"asset":"BTC","free":"0.2","locked":"0.1"}]}"#.into(),
    });
    responses.push(HttpResponse {
        status: 200,
        body: full_exchange_info.into(),
    });
    responses.push(HttpResponse {
        status: 200,
        body:
            r#"[{"symbol":"BTCUSDC","orderId":9,"side":"BUY","origQty":"0.4","executedQty":"0.1"}]"#
                .into(),
    });
    drop(responses);

    let mut candidate = order();
    candidate.qty = Quantity::from_dec("0.5").unwrap();
    assert!(matches!(
        venue.validate_order_constraints(&spec, &candidate),
        Err(QxError::BusinessViolation(message)) if message.contains("MAX_POSITION 超限")
    ));
    assert!(
        venue.state == ConnectorState::Live,
        "read-only rejection must not poison connector state"
    );
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].path.starts_with("/api/v3/openOrders?"));
    assert!(requests[1].headers.contains_key("X-MBX-APIKEY"));
    assert_eq!(requests[2].path, "/api/v3/exchangeInfo");
    assert!(requests[2].headers.is_empty());
    assert!(requests[3].path.starts_with("/api/v3/account?"));
    assert!(requests[3].headers.contains_key("X-MBX-APIKEY"));
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_max_position_quote_budget_fails_closed_without_private_reads() {
    let exchange_info = btcusdt_max_position_filters();
    let (mut venue, transport) = venue(&exchange_info);
    let spec = binance_btcusdt_spec();
    venue.validate_instrument_spec(&spec).unwrap();
    let mut order = order();
    order.qty = Quantity::ZERO;
    order.limit = None;
    order.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::ZERO,
        limit_price: None,
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: Some(ExactDecimal::parse("10").unwrap()),
        quote_filled: ExactDecimal::ZERO,
    });
    assert!(matches!(
        venue.validate_order_constraints(&spec, &order),
        Err(QxError::BusinessViolation(message)) if message.contains("quoteOrderQty") && message.contains("MAX_POSITION")
    ));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_read_only_account_failure_is_retryable_not_ambiguous_order_state() {
    let (mut venue, transport) = venue(BTCUSDT_EXCHANGE_INFO);
    transport.responses.lock().unwrap()[0] = HttpResponse {
        status: 503,
        body: "temporarily unavailable".into(),
    };
    assert!(matches!(
        venue.fetch_account_balances(1_700_000_000_000),
        Err(QxError::Transient(message)) if message.contains("HTTP 503")
    ));
    assert_eq!(venue.state, ConnectorState::Live);
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[test]
fn binance_frozen_spot_spec_matches_public_exchange_info_without_credentials() {
    let (mut venue, transport) = venue(BTCUSDT_EXCHANGE_INFO);
    let spec = binance_btcusdt_spec();

    venue.validate_instrument_spec(&spec).unwrap();
    // Fresh metadata is reused for 60 seconds, avoiding a public REST call per order.
    venue.validate_instrument_spec(&spec).unwrap();
    venue.market_rules.get_mut("BTCUSDT").unwrap().fetched_at_ms = 0;
    transport.responses.lock().unwrap().push(HttpResponse {
        status: 200,
        body: BTCUSDT_EXCHANGE_INFO.into(),
    });
    venue.validate_instrument_spec(&spec).unwrap();

    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/v3/exchangeInfo?symbol=BTCUSDT");
        assert!(request.headers.is_empty());
    }
}

#[test]
fn binance_frozen_spot_spec_drift_fails_before_any_order_write() {
    let (mut venue, transport) = venue(BTCUSDT_EXCHANGE_INFO);
    let mut spec = binance_btcusdt_spec();
    spec.qty_step = Quantity::from_dec("0.0001").unwrap().raw();

    assert!(matches!(
        venue.validate_instrument_spec(&spec),
        Err(QxError::BusinessViolation(message)) if message.contains("冻结规格不一致")
    ));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_market_order_checks_market_lot_size_before_any_order_write() {
    let (mut venue, transport) = venue(BTCUSDT_EXCHANGE_INFO);
    let spec = binance_btcusdt_spec();
    venue.validate_instrument_spec(&spec).unwrap();
    let mut market_order = order();
    market_order.qty = Quantity::from_dec("0.00015").unwrap();
    market_order.limit = None;

    assert!(matches!(
        venue.validate_order_constraints(&spec, &market_order),
        Err(QxError::BusinessViolation(message)) if message.contains("MARKET_LOT_SIZE.stepSize")
    ));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_percent_price_filters_use_reference_price_and_side_specific_bounds() {
    let exchange_info = btcusdt_dynamic_filters();
    let (mut buy_venue, buy_transport) = venue(&exchange_info);
    let spec = binance_btcusdt_spec();
    buy_venue.validate_instrument_spec(&spec).unwrap();
    buy_transport.responses.lock().unwrap().push(HttpResponse {
        status: 200,
        body: r#"{"symbol":"BTCUSDT","referencePrice":"100.00000000","timestamp":1792790000000}"#
            .into(),
    });

    let mut buy = order();
    buy.limit = Some(Price::from_dec("115").unwrap());
    buy_venue.validate_order_constraints(&spec, &buy).unwrap();

    let (mut venue, transport) = venue(&exchange_info);
    venue.validate_instrument_spec(&spec).unwrap();
    transport.responses.lock().unwrap().push(HttpResponse {
        status: 200,
        body: r#"{"symbol":"BTCUSDT","referencePrice":"100.00000000","timestamp":1792790000000}"#
            .into(),
    });
    let mut sell = order();
    sell.side = Side::Sell;
    sell.limit = Some(Price::from_dec("115").unwrap());
    assert!(matches!(
        venue.validate_order_constraints(&spec, &sell),
        Err(QxError::BusinessViolation(message))
            if message.contains("PERCENT_PRICE_BY_SIDE")
    ));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].path, "/api/v3/referencePrice?symbol=BTCUSDT");
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_market_min_notional_uses_exchange_reference_price() {
    let exchange_info = btcusdt_dynamic_filters();
    let (mut venue, transport) = venue(&exchange_info);
    let spec = binance_btcusdt_spec();
    venue.validate_instrument_spec(&spec).unwrap();
    transport.responses.lock().unwrap().push(HttpResponse {
        status: 200,
        body: r#"{"symbol":"BTCUSDT","referencePrice":"100.00000000","timestamp":1792790000000}"#
            .into(),
    });

    let mut market_order = order();
    market_order.limit = None;
    market_order.qty = Quantity::from_dec("0.0001").unwrap();
    assert!(matches!(
        venue.validate_order_constraints(&spec, &market_order),
        Err(QxError::BusinessViolation(message)) if message.contains("MIN_NOTIONAL")
    ));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].path, "/api/v3/referencePrice?symbol=BTCUSDT");
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_falls_back_only_to_an_average_with_the_filter_window() {
    let exchange_info = btcusdt_dynamic_filters();
    let (mut venue, transport) = venue(&exchange_info);
    let spec = binance_btcusdt_spec();
    venue.validate_instrument_spec(&spec).unwrap();
    transport.responses.lock().unwrap().push(HttpResponse {
        status: 200,
        body: r#"{"mins":1,"price":"100.00000000","closeTime":1792790000000}"#.into(),
    });
    transport.responses.lock().unwrap().push(HttpResponse {
        status: 400,
        body: r#"{"code":-2043,"msg":"This symbol doesn't have a reference price."}"#.into(),
    });

    let mut limit_order = order();
    limit_order.limit = Some(Price::from_dec("115").unwrap());
    assert!(matches!(
        venue.validate_order_constraints(&spec, &limit_order),
        Err(QxError::Transient(message)) if message.contains("窗口为 1 分钟")
    ));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].path, "/api/v3/referencePrice?symbol=BTCUSDT");
    assert_eq!(requests[2].path, "/api/v3/avgPrice?symbol=BTCUSDT");
    assert!(!requests.iter().any(|request| request.method == "POST"));
}

#[test]
fn binance_spot_filter_steps_are_anchored_at_zero() {
    assert!(validate_binance_exact_qty_filter(
        ExactDecimal::parse("6").unwrap(),
        ExactDecimal::parse("3").unwrap(),
        ExactDecimal::parse("1").unwrap(),
        ExactDecimal::parse("10").unwrap(),
        "LOT_SIZE",
    )
    .is_ok());
    assert!(validate_binance_exact_qty_filter(
        ExactDecimal::parse("4").unwrap(),
        ExactDecimal::parse("3").unwrap(),
        ExactDecimal::parse("1").unwrap(),
        ExactDecimal::parse("10").unwrap(),
        "LOT_SIZE",
    )
    .is_err());
    assert!(validate_binance_exact_price_filter(
        ExactDecimal::parse("0.00000008").unwrap(),
        ExactDecimal::parse("0.00000004").unwrap(),
        ExactDecimal::parse("0.00000001").unwrap(),
        ExactDecimal::parse("0.00000010").unwrap(),
    )
    .is_ok());
    assert!(validate_binance_exact_price_filter(
        ExactDecimal::parse("0.00000005").unwrap(),
        ExactDecimal::parse("0.00000004").unwrap(),
        ExactDecimal::parse("0.00000001").unwrap(),
        ExactDecimal::parse("0.00000010").unwrap(),
    )
    .is_err());
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
fn binance_submit_serializes_subnano_exact_amounts() {
    let (mut venue, transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","fills":[]}"#,
    );
    let mut exact = order();
    exact.qty = Quantity::ZERO;
    exact.limit = None;
    exact.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        limit_price: Some(ExactDecimal::parse("100.0000000001").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: Default::default(),
    });

    venue.submit(exact, 1_700_000_000_000).unwrap();
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body.contains("quantity=0.0000000001"));
    assert!(requests[0].body.contains("price=100.0000000001"));
    assert!(requests[0].body.contains("signature="));
}

#[test]
fn binance_quote_budget_market_buy_uses_quote_order_qty() {
    let (mut venue, transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","fills":[]}"#,
    );
    let mut exact = order();
    exact.qty = Quantity::ZERO;
    exact.limit = None;
    exact.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::ZERO,
        limit_price: None,
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: Some(ExactDecimal::parse("10.0000000001").unwrap()),
        quote_filled: ExactDecimal::ZERO,
    });
    venue.submit(exact, 1_700_000_000_000).unwrap();
    let requests = transport.requests.lock().unwrap();
    assert!(requests[0].body.contains("quoteOrderQty=10.0000000001"));
    assert!(!requests[0].body.contains("quantity="));
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
    venue.seen_fill_keys.insert((1, 9, 1, 0, 100, 0, 1, 0));

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
            VenueEvent::FeeAdjustment {
                account_id,
                venue_id,
                order_id,
                instrument,
                fees,
                ts,
            } => (
                qx_runtime::RuntimeExternalEvent::FeeAdjustment {
                    account_id,
                    venue_id,
                    order_id,
                    instrument,
                    fees,
                },
                ts,
            ),
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
fn binance_exact_fill_and_fee_reach_exact_multicurrency_ledger() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-binance-exact-ledger-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut pipeline = qx_runtime::LiveEventPipeline::open(&root, "binance", "USDT").unwrap();
    let spec = subnano_btcusdt_spec();
    let mut exact = order();
    exact.qty = Quantity::ZERO;
    exact.limit = None;
    exact.exact_amounts = Some(ExactOrderAmounts {
        quantity: ExactDecimal::parse("0.0000000001").unwrap(),
        limit_price: Some(ExactDecimal::parse("100.0000000001").unwrap()),
        filled: ExactDecimal::ZERO,
        market_reference_price: None,
        quote_budget: None,
        quote_filled: ExactDecimal::ZERO,
    });
    pipeline.register_order(exact.clone(), 1).unwrap();
    let (mut venue, _) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"FILLED","fills":[{"price":"100.0000000001","qty":"0.0000000001","commission":"0.0000000001","commissionAsset":"BTC","tradeId":9,"transactTime":1700000000101}]}"#,
    );
    let events = venue.submit(exact, 100).unwrap();
    for (source_seq, event) in events.into_iter().enumerate() {
        let source_seq = source_seq as u64 + 1;
        let (runtime_event, ts) = match event {
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
                (
                    qx_runtime::RuntimeExternalEvent::FillWithSpec {
                        fill: Box::new(fill),
                        spec: Box::new(spec.clone()),
                    },
                    ts,
                )
            }
            other => panic!("unexpected Binance event: {other:?}"),
        };
        pipeline
            .ingest(qx_runtime::RuntimeEventEnvelope::venue(
                runtime_event,
                ts,
                ts,
                source_seq,
                format!("binance-exact:{source_seq}"),
            ))
            .unwrap();
    }
    assert!(pipeline.ledger().entries().iter().any(|entry| {
        entry.currency == "USDT"
            && entry
                .exact_amounts
                .is_some_and(|amount| amount.amount.is_negative())
    }));
    assert!(pipeline.ledger().entries().iter().any(|entry| {
        entry.currency == "BTC"
            && entry.kind == qx_core::LedgerEntryKind::Fee
            && entry
                .exact_amounts
                .is_some_and(|amount| amount.amount.is_negative())
    }));
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
