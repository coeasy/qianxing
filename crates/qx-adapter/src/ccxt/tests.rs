use super::*;
use qx_core::{InstrumentId, MarginMode, OrderPolicy, PositionMode, PositionSide, Quantity, Side};
use std::sync::{Arc, Mutex};

#[test]
fn ccxt_worker_environment_is_minimal_and_validates_credential_names() {
    let path = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("qianxing-ccxt-env-{}.json", std::process::id()));
    std::fs::write(
        &path,
        r#"{"credential_env":{"api_key":"QX_TEST_API_KEY","secret":"QX_TEST_SECRET"}}"#,
    )
    .unwrap();
    let environment = ccxt_worker_environment(&path.to_string_lossy()).unwrap();
    assert!(!environment.contains_key("QX_UNRELATED_PARENT_SECRET"));
    let _ = std::fs::write(&path, r#"{"credential_env":{"secret":"BAD=VARIABLE"}}"#);
    assert!(ccxt_worker_environment(&path.to_string_lossy()).is_err());
    let _ = std::fs::remove_file(path);
}

/// #214：读窗下界必须大到导得出的空闲窗口还能当真是一次回话往返。
/// 用户流那条循环自身不睡，节律全靠这个窗口 —— 下界退化成 1 时窗口是 0，
/// 一个没有成交的账户就把 EventLog 刷新与健康 mark 打成毫秒级热循环。
#[test]
fn ccxt_idle_window_keeps_real_pacing_at_the_timeout_floor() {
    let floor_window = ccxt_idle_window_ms(CCXT_WORKER_MIN_TIMEOUT_MS);
    assert!(
        floor_window >= 500,
        "读窗下界 {CCXT_WORKER_MIN_TIMEOUT_MS}ms 导出的空闲窗口只有 {floor_window}ms，用户流会打成热循环"
    );
    // 回话必须先于读窗到期到达，否则"空闲"永远被读成断链。
    for timeout_ms in [
        CCXT_WORKER_MIN_TIMEOUT_MS,
        5_000,
        30_000,
        CCXT_WORKER_MAX_TIMEOUT_MS,
    ] {
        assert!(
            ccxt_idle_window_ms(timeout_ms) < timeout_ms,
            "读窗 {timeout_ms} 的空闲窗口不再短于读窗，空闲会被具名成链路故障"
        );
    }
}

/// #214：校验的两端都要真的挡 —— 下界以下与上界以上都拒绝，界内按值返回。
#[test]
fn ccxt_worker_timeout_bounds_are_enforced_from_config() {
    let path = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("qianxing-ccxt-timeout-{}.json", std::process::id()));
    let read = |payload: &str| {
        std::fs::write(&path, payload).unwrap();
        ccxt_worker_timeout_ms(&path.to_string_lossy())
    };
    // 低于下界：过去这条是合法的 1，热循环就是从这一格进来的。
    let error = read(r#"{"timeout_ms": 1}"#).unwrap_err();
    assert!(
        error.contains("timeout_ms 必须在"),
        "下界没挡住 timeout_ms=1: {error}"
    );
    assert!(read(r#"{"timeout_ms": 0}"#).is_err());
    assert!(read(r#"{"timeout_ms": 300001}"#).is_err());
    assert_eq!(read(r#"{"timeout_ms": 1000}"#).unwrap(), 1_000);
    assert_eq!(read(r#"{"timeout_ms": 300000}"#).unwrap(), 300_000);
    // 不写字段时仍按默认 30 秒，不受下界改动影响。
    assert_eq!(read("{}").unwrap(), 30_000);
    let _ = std::fs::remove_file(path);
}

struct FakeRpc {
    calls: Vec<Value>,
}

impl CcxtRpc for FakeRpc {
    fn call(&mut self, request: Value) -> Result<Value, String> {
        self.calls.push(request.clone());
        match request.get("op").and_then(Value::as_str) {
            Some("create_order") => Ok(json!({
                "order": {"order_id": "remote-1", "status": "open", "filled_raw": 0}
            })),
            Some("fetch_order") => Ok(json!({
                "order": {
                    "order_id": "remote-1",
                    "status": "closed",
                    "filled_raw": 2_000_000_000i64,
                    "average_raw": 100_000_000_000i64,
                    "fee_raw": 1_000_000_000i64,
                    "fee_currency": "USDT"
                }
            })),
            _ => Ok(json!({})),
        }
    }
}

fn order() -> Order {
    Order {
        client_id: 7,
        instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(2),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

#[test]
fn ccxt_process_venue_submits_and_syncs_fill_without_custom_exchange_protocol() {
    let mut venue = CcxtProcessVenue::new("binance", Box::new(FakeRpc { calls: Vec::new() }));
    assert!(matches!(
        venue.submit(order(), 10).unwrap()[0],
        VenueEvent::Accepted { .. }
    ));
    let events = venue.sync_order(7, 11).unwrap();
    match &events[0] {
        VenueEvent::Fill(fill) => {
            assert_eq!(fill.fee, Money::from_i64(1));
            assert_eq!(fill.fee_currency.as_deref(), Some("USDT"));
        }
        other => panic!("expected fill, got {other:?}"),
    }
    assert_eq!(
        venue.snapshot()[0].filled.raw(),
        Quantity::from_i64(2).raw()
    );
}

#[test]
fn ccxt_rejects_invalid_lifecycle_orders_before_worker_rpc() {
    struct CountingRpc(Arc<Mutex<Vec<Value>>>);
    impl CcxtRpc for CountingRpc {
        fn call(&mut self, request: Value) -> Result<Value, String> {
            self.0.lock().unwrap().push(request);
            Ok(json!({}))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut venue = CcxtProcessVenue::new("binance", Box::new(CountingRpc(Arc::clone(&calls))));
    let mut invalid_spec = order();
    invalid_spec.qty = Quantity::ZERO;
    assert!(matches!(
        venue.submit(invalid_spec, 10),
        Err(QxError::BusinessViolation(message)) if message == "订单数量必须为正"
    ));

    let mut invalid_state = order();
    invalid_state.status = OrderStatus::Working;
    assert!(matches!(
        venue.submit(invalid_state, 10),
        Err(QxError::BusinessViolation(message)) if message == "CCXT 只接受待提交或已提交订单"
    ));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn ccxt_restore_is_idempotent_and_rejects_order_identity_rebinding() {
    let mut venue = CcxtProcessVenue::new("binance", Box::new(FakeRpc { calls: Vec::new() }));
    let first = order();
    venue.restore_order(first.clone(), "remote-7").unwrap();
    venue.restore_order(first.clone(), "remote-7").unwrap();

    let mut conflicting_local = first.clone();
    conflicting_local.account_id = "other-account".into();
    assert!(matches!(
        venue.restore_order(conflicting_local, "remote-7"),
        Err(QxError::ReconcileRequired(_))
    ));

    let mut another = first;
    another.client_id = 8;
    assert!(matches!(
        venue.restore_order(another, "remote-7"),
        Err(QxError::ReconcileRequired(_))
    ));
    assert_eq!(venue.orders.len(), 1);
    assert_eq!(
        venue.remote_ids.get(&7).map(String::as_str),
        Some("remote-7")
    );
    assert!(!venue.remote_ids.contains_key(&8));
}

#[test]
fn ccxt_terminal_cache_retirement_cascades_to_remote_ids_and_fill_ledgers() {
    let mut venue = CcxtProcessVenue::new("binance", Box::new(FakeRpc { calls: Vec::new() }));
    let mut oldest = order();
    oldest.status = OrderStatus::Cancelled;
    venue.restore_order(oldest, "remote-7").unwrap();
    venue
        .seen_trade_ids
        .insert(7, ["trade-1".into()].into_iter().collect());
    venue.cumulative_costs.insert(7, 1);

    let newest_id = crate::venue_cache::MAX_CACHED_ORDERS as u64 + 7;
    for client_id in 8..=newest_id {
        let mut terminal = order();
        terminal.client_id = client_id;
        terminal.status = OrderStatus::Cancelled;
        venue
            .restore_order(terminal, format!("remote-{client_id}"))
            .unwrap();
    }

    assert!(!venue.orders.contains_key(&7));
    assert!(!venue.remote_ids.contains_key(&7));
    assert!(!venue.seen_trade_ids.contains_key(&7));
    assert!(!venue.cumulative_costs.contains_key(&7));
    assert!(venue.orders.contains_key(&newest_id));
}

#[test]
fn ccxt_process_venue_enriches_missing_order_fee_from_incremental_trades() {
    struct TradeFeeRpc;
    impl CcxtRpc for TradeFeeRpc {
        fn call(&mut self, request: Value) -> Result<Value, String> {
            match request.get("op").and_then(Value::as_str) {
                Some("create_order") => Ok(json!({
                    "order": {"order_id": "remote-fee", "status": "open", "filled_raw": 0}
                })),
                Some("fetch_order") => Ok(json!({
                    "order": {
                        "order_id": "remote-fee",
                        "status": "closed",
                        "filled_raw": 2_000_000_000_i64,
                        "average_raw": 100_000_000_000_i64,
                        "fee_raw": 0
                    }
                })),
                Some("fetch_my_trades") => Ok(json!({
                    "trades": [{
                        "trade_id": "trade-fee-1",
                        "fee_raw": 2_000_000_000_i64,
                        "fee_currency": "USDT"
                    }]
                })),
                _ => Ok(json!({})),
            }
        }
    }
    let mut venue = CcxtProcessVenue::new("binance", Box::new(TradeFeeRpc));
    venue.submit(order(), 10).unwrap();
    let events = venue.sync_order(7, 11).unwrap();
    match &events[0] {
        VenueEvent::Fill(fill) => {
            assert_eq!(fill.fee, Money::from_i64(2));
            assert_eq!(fill.fee_currency.as_deref(), Some("USDT"));
        }
        other => panic!("expected fill, got {other:?}"),
    }
}

#[test]
fn ccxt_incremental_trade_fee_deduplicates_replayed_trade_ids() {
    struct ReplayedTradeRpc;
    impl CcxtRpc for ReplayedTradeRpc {
        fn call(&mut self, _request: Value) -> Result<Value, String> {
            Ok(json!({
                "trades": [{
                    "trade_id": "trade-once",
                    "fee_raw": 2_000_000_000_i64,
                    "fee_currency": "USDT"
                }]
            }))
        }
    }

    let mut venue = CcxtProcessVenue::new("okx", Box::new(ReplayedTradeRpc));
    assert_eq!(
        venue
            .incremental_trade_fee(7, "BTC/USDT", "remote-7")
            .unwrap(),
        (2_000_000_000, Some("USDT".into()))
    );
    assert_eq!(
        venue
            .incremental_trade_fee(7, "BTC/USDT", "remote-7")
            .unwrap(),
        (0, None)
    );
    assert_eq!(venue.seen_trade_ids[&7].len(), 1);
}

#[test]
fn ccxt_partial_sync_uses_incremental_price_from_cumulative_average() {
    struct PartialRpc {
        fetches: usize,
    }

    impl CcxtRpc for PartialRpc {
        fn call(&mut self, request: Value) -> Result<Value, String> {
            match request.get("op").and_then(Value::as_str) {
                Some("create_order") => Ok(json!({
                    "order": {"order_id": "partial-1", "status": "open", "filled_raw": 0}
                })),
                Some("fetch_order") => {
                    self.fetches += 1;
                    if self.fetches == 1 {
                        Ok(json!({
                            "order": {
                                "order_id": "partial-1",
                                "status": "open",
                                "filled_raw": 1_000_000_000_i64,
                                "average_raw": 100_000_000_000_i64,
                                "fee_raw": 1
                            }
                        }))
                    } else {
                        Ok(json!({
                            "order": {
                                "order_id": "partial-1",
                                "status": "closed",
                                "filled_raw": 2_000_000_000_i64,
                                "average_raw": 110_000_000_000_i64,
                                "fee_raw": 2
                            }
                        }))
                    }
                }
                _ => Ok(json!({})),
            }
        }
    }

    let mut venue = CcxtProcessVenue::new("binance", Box::new(PartialRpc { fetches: 0 }));
    venue.submit(order(), 10).unwrap();
    let first = venue.sync_order(7, 11).unwrap();
    let second = venue.sync_order(7, 12).unwrap();
    let VenueEvent::Fill(first_fill) = &first[0] else {
        panic!("expected first partial fill")
    };
    let VenueEvent::Fill(second_fill) = &second[0] else {
        panic!("expected second partial fill")
    };
    assert_eq!(first_fill.qty, Quantity::from_i64(1));
    assert_eq!(first_fill.price, Price::from_i64(100));
    assert_eq!(second_fill.qty, Quantity::from_i64(1));
    assert_eq!(second_fill.price, Price::from_i64(120));
}

struct StatusRpc {
    status: &'static str,
    filled_raw: i64,
}

impl CcxtRpc for StatusRpc {
    fn call(&mut self, request: Value) -> Result<Value, String> {
        match request.get("op").and_then(Value::as_str) {
            Some("create_order") => Ok(json!({
                "order": {"order_id": "status-1", "status": "open", "filled_raw": 0}
            })),
            Some("fetch_order") => Ok(json!({
                "order": {
                    "order_id": "status-1",
                    "status": self.status,
                    "filled_raw": self.filled_raw,
                    "average_raw": 100_000_000_000_i64,
                    "fee_raw": 1
                }
            })),
            _ => Ok(json!({})),
        }
    }
}

#[test]
fn ccxt_unknown_status_fails_closed_without_mutating_local_progress() {
    let mut venue = CcxtProcessVenue::new(
        "binance",
        Box::new(StatusRpc {
            status: "mystery",
            filled_raw: 0,
        }),
    );
    venue.submit(order(), 10).unwrap();
    let error = venue.sync_order(7, 11).unwrap_err();
    assert!(matches!(error, QxError::ReconcileRequired(_)));
    assert!(!venue.cumulative_costs.contains_key(&7));
    assert_eq!(venue.snapshot()[0].filled, Quantity::ZERO);
    assert_eq!(venue.snapshot()[0].status, OrderStatus::PendingSubmit);
}

#[test]
fn ccxt_rpc_failure_marks_venue_disconnected_until_rpc_replacement() {
    struct FailingRpc;
    impl CcxtRpc for FailingRpc {
        fn call(&mut self, _request: Value) -> Result<Value, String> {
            Err("transport closed".into())
        }
    }

    let mut venue = CcxtProcessVenue::new("binance", Box::new(FailingRpc));
    assert!(matches!(
        venue.submit(order(), 10),
        Err(QxError::ReconcileRequired(_))
    ));
    assert!(!venue.connected());
    assert!(matches!(
        venue.submit(order(), 11),
        Err(QxError::VenueState(_))
    ));
}

#[test]
fn ccxt_closed_partial_order_requires_reconciliation_without_mutating_local_progress() {
    let mut venue = CcxtProcessVenue::new(
        "binance",
        Box::new(StatusRpc {
            status: "closed",
            filled_raw: 1_000_000_000,
        }),
    );
    venue.submit(order(), 10).unwrap();
    let error = venue.sync_order(7, 11).unwrap_err();
    assert!(matches!(error, QxError::ReconcileRequired(_)));
    assert!(!venue.cumulative_costs.contains_key(&7));
    assert_eq!(venue.snapshot()[0].filled, Quantity::ZERO);
}

#[test]
fn ccxt_partial_cancel_emits_terminal_cancel_and_preserves_fill() {
    let mut venue = CcxtProcessVenue::new(
        "binance",
        Box::new(StatusRpc {
            status: "canceled",
            filled_raw: 1_000_000_000,
        }),
    );
    venue.submit(order(), 10).unwrap();
    let events = venue.sync_order(7, 11).unwrap();
    assert!(events
        .iter()
        .any(|event| matches!(event, VenueEvent::Fill(_))));
    assert!(events
        .iter()
        .any(|event| matches!(event, VenueEvent::Cancelled { .. })));
    assert_eq!(venue.snapshot()[0].filled, Quantity::from_i64(1));
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Cancelled);
}

#[test]
fn ccxt_order_policy_is_translated_before_create_order() {
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    struct PolicyRpc {
        calls: Arc<Mutex<Vec<Value>>>,
    }
    impl CcxtRpc for PolicyRpc {
        fn call(&mut self, request: Value) -> Result<Value, String> {
            self.calls.lock().unwrap().push(request.clone());
            if request.get("op").and_then(Value::as_str) == Some("create_order") {
                Ok(json!({
                    "order": {"order_id": "policy-1", "status": "open", "filled_raw": 0}
                }))
            } else {
                Ok(json!({}))
            }
        }
    }
    let mut requested = order();
    requested.policy = Some(OrderPolicy {
        reduce_only: true,
        position_side: PositionSide::Long,
        margin_mode: MarginMode::Isolated,
        position_mode: PositionMode::Hedge,
        leverage: 3,
        post_only: true,
    });
    let mut venue = CcxtProcessVenue::new(
        "binance",
        Box::new(PolicyRpc {
            calls: Arc::clone(&calls),
        }),
    );
    venue.submit(requested, 10).unwrap();
    let calls = calls.lock().unwrap();
    assert!(calls.iter().any(|call| call["op"] == "set_margin_mode"));
    assert!(calls.iter().any(|call| call["op"] == "set_leverage"));
    assert!(calls.iter().any(|call| call["op"] == "set_position_mode"));
    let create = calls
        .iter()
        .find(|call| call["op"] == "create_order")
        .unwrap();
    assert_eq!(create["params"]["reduceOnly"], true);
    assert_eq!(create["params"]["postOnly"], true);
    assert_eq!(create["params"]["positionSide"], "long");
}

#[path = "tests/lifecycle_cancel.rs"]
mod lifecycle_cancel;
