use super::*;
use crate::StrategyErrorStage;
use ring::signature::KeyPair;
use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTROYED: AtomicUsize = AtomicUsize::new(0);
static LAST_ORDER_STATUS_EVENT_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static LAST_TYPED_ORDER_STATUS_OK: AtomicUsize = AtomicUsize::new(0);
static LAST_FILL_EVENT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_FILL_SCHEMA_VERSION: AtomicUsize = AtomicUsize::new(0);
static LAST_ACCOUNT_UPDATE_EVENT_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static ACCOUNT_UPDATE_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_EXACT_MARKET_EVENT_OK: AtomicUsize = AtomicUsize::new(0);
static LAST_EXACT_ORDERBOOK_EVENT_OK: AtomicUsize = AtomicUsize::new(0);
static LAST_TIMER_MARKET_EVENT_OK: AtomicUsize = AtomicUsize::new(0);
static LAST_WARMUP_BARS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_COMPLETE_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static WARMUP_COMPLETE_EVENT_OK: AtomicUsize = AtomicUsize::new(0);
static STOP_V1_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_STOP_REASON: AtomicUsize = AtomicUsize::new(0);
static STRATEGY_ERROR_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_STRATEGY_ERROR_STAGE: AtomicUsize = AtomicUsize::new(0);
static EXACT_CONTEXT_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_EXACT_CONTEXT_VALUES_OK: AtomicUsize = AtomicUsize::new(0);
static EXACT_CONTEXT_REJECTED_INIT_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static CHECKPOINT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "C" fn fake_checkpoint_snapshot(
    _handle: QxStrategyHandle,
    output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
) -> i32 {
    let Some(output_len) = output_len.as_mut() else {
        return 1;
    };
    let Ok(payload) = serde_json::to_vec(&serde_json::json!({
        "counter": CHECKPOINT_COUNTER.load(Ordering::SeqCst)
    })) else {
        return 2;
    };
    if output.is_null() || payload.len() > output_capacity {
        return 3;
    }
    std::ptr::copy_nonoverlapping(payload.as_ptr(), output.cast::<u8>(), payload.len());
    *output_len = payload.len();
    0
}

unsafe extern "C" fn fake_checkpoint_restore(
    _handle: QxStrategyHandle,
    input: *const c_char,
    input_len: usize,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    if input.is_null() || input_len == 0 {
        return 1;
    }
    let bytes = std::slice::from_raw_parts(input.cast::<u8>(), input_len);
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return 2;
    };
    let Some(counter) = value.get("counter").and_then(serde_json::Value::as_u64) else {
        return 3;
    };
    CHECKPOINT_COUNTER.store(counter, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_strategy_error(
    _handle: QxStrategyHandle,
    payload: *const c_char,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let Some(payload) = payload.as_ref() else {
        return 1;
    };
    let Ok(payload) = std::ffi::CStr::from_ptr(payload).to_str() else {
        return 1;
    };
    let Ok(error) = serde_json::from_str::<StrategyErrorV1>(payload) else {
        return 1;
    };
    if error.validate().is_err() || error.message != "test callback failed" {
        return 1;
    }
    let stage = match error.stage {
        StrategyErrorStage::MarketEvent => 1,
        _ => 0,
    };
    LAST_STRATEGY_ERROR_STAGE.store(stage, Ordering::SeqCst);
    STRATEGY_ERROR_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_exact_context_v2(
    _handle: QxStrategyHandle,
    context: *const QxStrategyContextV2,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let Some(context) = context.as_ref() else {
        return 1;
    };
    if context.struct_size as usize != std::mem::size_of::<QxStrategyContextV2>()
        || context.schema_version != QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2
        || context.context_v1.schema_version != QX_C_STRATEGY_API_VERSION
        || context.positions_exact_len != 1
        || context.cash_exact_len != 1
        || context.has_available_margin_exact != 1
        || context.context_v1.positions_len != 1
        || context.context_v1.cash_len != 1
    {
        return 2;
    }
    let positions =
        std::slice::from_raw_parts(context.positions_exact, context.positions_exact_len);
    let cash = std::slice::from_raw_parts(context.cash_exact, context.cash_exact_len);
    let position_key =
        std::slice::from_raw_parts(positions[0].key.cast::<u8>(), positions[0].key_len);
    let cash_key = std::slice::from_raw_parts(cash[0].key.cast::<u8>(), cash[0].key_len);
    let exact_values_ok = position_key == b"ALTUSDT.BINANCE"
        && cash_key == b"USDT"
        && positions[0].value.coefficient.into_i128() == 1
        && positions[0].value.scale == 18
        && cash[0].value.coefficient.into_i128() == 7
        && cash[0].value.scale == 18
        && context.available_margin_exact.coefficient.into_i128() == 9
        && context.available_margin_exact.scale == 18;
    LAST_EXACT_CONTEXT_VALUES_OK.store(if exact_values_ok { 1 } else { 0 }, Ordering::SeqCst);
    EXACT_CONTEXT_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    if exact_values_ok {
        0
    } else {
        3
    }
}

unsafe extern "C" fn fake_reject_exact_context_v2(
    _handle: QxStrategyHandle,
    _context: *const QxStrategyContextV2,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    42
}

unsafe extern "C" fn fake_stop_v1(
    _handle: QxStrategyHandle,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    STOP_V1_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_stop_v2(
    _handle: QxStrategyHandle,
    context_json: *const c_char,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let Some(context_json) = context_json.as_ref() else {
        return 1;
    };
    let Ok(payload) = std::ffi::CStr::from_ptr(context_json).to_str() else {
        return 1;
    };
    let Ok(stop) = serde_json::from_str::<StrategyStopContextV1>(payload) else {
        return 1;
    };
    let reason = match stop.reason {
        StrategyStopReason::HostStop => 1,
        StrategyStopReason::Shutdown => 2,
        _ => 3,
    };
    LAST_STOP_REASON.store(reason, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_create(_config: *const c_char) -> QxStrategyHandle {
    Box::into_raw(Box::new(7_u8)).cast()
}

#[test]
fn contextual_stop_falls_back_to_legacy_dynamic_or_static_stop_callback() {
    STOP_V1_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_stop = Some(fake_stop_v1);
    let stop = StrategyStopContextV1::host_stop(None);

    strategy.on_stop_with_context(&stop).unwrap();
    assert_eq!(STOP_V1_CALLBACK_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn stop_hooks_receive_default_host_stop_and_explicit_shutdown_reason() {
    LAST_STOP_REASON.store(0, Ordering::SeqCst);
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_stop_v2 = Some(fake_stop_v2);

    Strategy::on_stop(&mut strategy).unwrap();
    assert_eq!(LAST_STOP_REASON.load(Ordering::SeqCst), 1);

    strategy
        .on_stop_with_context(&StrategyStopContextV1::new(
            StrategyStopReason::Shutdown,
            None,
        ))
        .unwrap();
    assert_eq!(LAST_STOP_REASON.load(Ordering::SeqCst), 2);
}

#[test]
fn optional_error_callback_receives_bounded_versioned_strategy_error_json() {
    STRATEGY_ERROR_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    LAST_STRATEGY_ERROR_STAGE.store(0, Ordering::SeqCst);
    let mut strategy = unsafe {
        CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}")
            .unwrap()
            .with_error_callback(fake_strategy_error)
    };
    let context = StrategyContext {
        strategy_id: "error-test".into(),
        strategy_version: "1.0.0".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "error-fixture".into(),
        as_of: 100,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "clear".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let error = StrategyErrorV1::new(
        StrategyErrorStage::MarketEvent,
        &context,
        Some(101),
        Some(InstrumentId::parse("BTCUSDT.BINANCE").unwrap()),
        "test callback failed",
    );

    strategy.on_error(&error).unwrap();
    assert_eq!(STRATEGY_ERROR_CALLBACK_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(LAST_STRATEGY_ERROR_STAGE.load(Ordering::SeqCst), 1);
}

unsafe extern "C" fn fake_init(
    _handle: QxStrategyHandle,
    _context: *const QxStrategyContext,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    0
}

unsafe extern "C" fn fake_count_exact_context_rejected_init(
    _handle: QxStrategyHandle,
    _context: *const QxStrategyContext,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    EXACT_CONTEXT_REJECTED_INIT_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_history_requirements(
    _handle: QxStrategyHandle,
    warmup_bars: *mut u64,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let Some(warmup_bars) = warmup_bars.as_mut() else {
        return 1;
    };
    *warmup_bars = 2;
    0
}

unsafe extern "C" fn fake_warmup_complete(
    _handle: QxStrategyHandle,
    event: *const QxStrategyWarmupCompleteV1,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let Some(event) = event.as_ref() else {
        return 1;
    };
    let instrument = if event.instrument.is_null() {
        None
    } else {
        std::ffi::CStr::from_ptr(event.instrument).to_str().ok()
    };
    let valid = event.struct_size as usize == std::mem::size_of::<QxStrategyWarmupCompleteV1>()
        && event.schema_version == QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION_V1
        && event.has_instrument == 1
        && event.instrument_len == "BTCUSDT.BINANCE".len()
        && instrument == Some("BTCUSDT.BINANCE")
        && event.observed_bars == 2;
    WARMUP_COMPLETE_EVENT_OK.store(usize::from(valid), Ordering::SeqCst);
    WARMUP_COMPLETE_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    i32::from(!valid)
}

unsafe extern "C" fn fake_event(
    _handle: QxStrategyHandle,
    _context: *const QxStrategyContext,
    event: *const QxMarketEvent,
    decision: *mut QxStrategyDecision,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let intents = Box::new([QxOrderIntent {
        intent_id: (*event).ts,
        instrument: c"BTCUSDT.BINANCE".as_ptr(),
        instrument_len: c"BTCUSDT.BINANCE".to_bytes().len(),
        side: QX_ORDER_SIDE_BUY,
        qty_raw: QxRaw128::from_i128(1_000_000_000),
        limit_price_raw: QxRaw128::from_i128(0),
        has_limit_price: 0,
        reduce_only: 0,
        post_only: 0,
        position_side: ptr::null(),
        position_side_len: 0,
    }]);
    (*decision).schema_version = QX_C_STRATEGY_API_VERSION;
    (*decision).request_id = c"abi-request".as_ptr();
    (*decision).request_id_len = c"abi-request".to_bytes().len();
    (*decision).strategy_id = c"abi-test".as_ptr();
    (*decision).strategy_id_len = c"abi-test".to_bytes().len();
    (*decision).signal_id = (*event).ts;
    (*decision).confidence = QxRaw128::from_i128(1);
    (*decision).priority = 0;
    (*decision).expires_at = (*event).ts;
    (*decision).intents = Box::into_raw(intents).cast();
    (*decision).intents_len = 1;
    (*decision).intents_capacity = 1;
    0
}

unsafe extern "C" fn fake_timer_event(
    handle: QxStrategyHandle,
    context: *const QxStrategyContext,
    event: *const QxMarketEvent,
    decision: *mut QxStrategyDecision,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    let Some(event) = event.as_ref() else {
        return 1;
    };
    let name = if event.timer_name.is_null() {
        None
    } else {
        std::ffi::CStr::from_ptr(event.timer_name).to_str().ok()
    };
    let valid = event.schema_version == QX_C_STRATEGY_API_VERSION
        && event.kind == QxMarketEventKind::Timer
        && event.ts == 1_000
        && name == Some("scheduler")
        && event.instrument.is_null();
    LAST_TIMER_MARKET_EVENT_OK.store(usize::from(valid), Ordering::SeqCst);
    if !valid {
        return 2;
    }
    fake_event(
        handle,
        context,
        event as *const QxMarketEvent,
        decision,
        error,
        error_len,
    )
}

unsafe extern "C" fn fake_market_event_v2(
    handle: QxStrategyHandle,
    context: *const QxStrategyContext,
    event: *const QxMarketEventV2,
    decision: *mut QxStrategyDecision,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    let Some(event) = event.as_ref() else {
        return 1;
    };
    let exact = &event.exact_ohlcv;
    let valid = event.struct_size as usize == std::mem::size_of::<QxMarketEventV2>()
        && event.schema_version == QX_MARKET_EVENT_SCHEMA_VERSION_V2
        && event.raw_event.kind == QxMarketEventKind::Bar
        && event.raw_event.schema_version == QX_C_STRATEGY_API_VERSION
        && event.has_exact_ohlcv == 1
        && exact.open.coefficient.into_i128() == 5
        && exact.open.scale == 10
        && exact.close.coefficient.into_i128() == 5
        && exact.close.scale == 10
        && exact.volume.coefficient.into_i128() == 5
        && exact.volume.scale == 13;
    LAST_EXACT_MARKET_EVENT_OK.store(usize::from(valid), Ordering::SeqCst);
    if !valid {
        return 2;
    }
    fake_event(
        handle,
        context,
        std::ptr::addr_of!(event.raw_event),
        decision,
        error,
        error_len,
    )
}

unsafe extern "C" fn fake_market_event_v3(
    handle: QxStrategyHandle,
    context: *const QxStrategyContext,
    event: *const QxMarketEventV3,
    decision: *mut QxStrategyDecision,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    let Some(event) = event.as_ref() else {
        return 1;
    };
    let valid = event.struct_size as usize == std::mem::size_of::<QxMarketEventV3>()
        && event.schema_version == QX_MARKET_EVENT_SCHEMA_VERSION_V3
        && event.event_v2.raw_event.kind == QxMarketEventKind::OrderBook
        && event.event_v2.raw_event.bids_len == 1
        && event.event_v2.raw_event.asks_len == 1
        && event.exact_bids_len == 1
        && event.exact_asks_len == 1
        && !event.exact_bids.is_null()
        && !event.exact_asks.is_null()
        && (*event.exact_bids).has_exact == 1
        && (*event.exact_bids).price.coefficient.into_i128() == 12_345
        && (*event.exact_bids).price.scale == 12
        && (*event.exact_bids).qty.coefficient.into_i128() == 5
        && (*event.exact_bids).qty.scale == 18
        && (*event.exact_asks).has_exact == 1
        && (*event.exact_asks).price.coefficient.into_i128() == 12_346
        && (*event.exact_asks).qty.coefficient.into_i128() == 6;
    LAST_EXACT_ORDERBOOK_EVENT_OK.store(usize::from(valid), Ordering::SeqCst);
    if !valid {
        return 2;
    }
    fake_event(
        handle,
        context,
        std::ptr::addr_of!(event.event_v2.raw_event),
        decision,
        error,
        error_len,
    )
}

unsafe extern "C" fn fake_free(_handle: QxStrategyHandle, decision: *mut QxStrategyDecision) {
    if !(*decision).intents.is_null() {
        let pointer = std::ptr::slice_from_raw_parts_mut((*decision).intents, 1);
        drop(Box::from_raw(pointer));
        (*decision).intents = ptr::null_mut();
        (*decision).intents_len = 0;
        (*decision).intents_capacity = 0;
        (*decision).request_id_len = 0;
        (*decision).strategy_id_len = 0;
    }
}

unsafe extern "C" fn fake_destroy(handle: QxStrategyHandle) {
    drop(Box::from_raw(handle.cast::<u8>()));
    DESTROYED.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn fake_diagnostic_state(
    _handle: QxStrategyHandle,
    output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
) -> i32 {
    let payload = br#"{"warmup_count":7,"rolling_sum_raw":"4200"}"#;
    if output.is_null() || output_len.is_null() || output_capacity < payload.len() {
        return 1;
    }
    std::ptr::copy_nonoverlapping(payload.as_ptr(), output.cast::<u8>(), payload.len());
    *output_len = payload.len();
    0
}

unsafe extern "C" fn fake_diagnostic_state_oversize(
    _handle: QxStrategyHandle,
    _output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
) -> i32 {
    if output_len.is_null() {
        return 1;
    }
    *output_len = output_capacity + 1;
    0
}

unsafe extern "C" fn fake_order_status(
    _handle: QxStrategyHandle,
    update_json: *const c_char,
    _decision: *mut QxStrategyDecision,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    if update_json.is_null() {
        return 1;
    }
    let Ok(payload) = std::ffi::CStr::from_ptr(update_json).to_str() else {
        return 2;
    };
    let Ok(update) = serde_json::from_str::<serde_json::Value>(payload) else {
        return 3;
    };
    let Some(event_seq) = update.get("event_seq").and_then(serde_json::Value::as_u64) else {
        if !error.is_null() && error_len > 0 {
            *error = b'e' as c_char;
            if error_len > 1 {
                *error.add(1) = 0;
            }
        }
        return 4;
    };
    LAST_ORDER_STATUS_EVENT_SEQ.store(event_seq, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_fill_update_v2(
    _handle: QxStrategyHandle,
    update_json: *const c_char,
    _decision: *mut QxStrategyDecision,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    if update_json.is_null() {
        return 1;
    }
    let Ok(payload) = std::ffi::CStr::from_ptr(update_json).to_str() else {
        return 2;
    };
    let Ok(update) = serde_json::from_str::<serde_json::Value>(payload) else {
        return 3;
    };
    if update["event"] != "fill" || update["schema_version"] != 2 || update["ts"] != 10 {
        if !error.is_null() && error_len > 0 {
            *error = b'i' as c_char;
            if error_len > 1 {
                *error.add(1) = 0;
            }
        }
        return 4;
    }
    let Some(event_seq) = update["event_seq"].as_u64() else {
        return 5;
    };
    LAST_FILL_EVENT_SEQ.store(event_seq, Ordering::SeqCst);
    LAST_FILL_SCHEMA_VERSION.store(2, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_account_update_v1(
    _handle: QxStrategyHandle,
    update_json: *const c_char,
    _decision: *mut QxStrategyDecision,
    error: *mut c_char,
    error_len: usize,
) -> i32 {
    if update_json.is_null() {
        return 1;
    }
    let Ok(payload) = std::ffi::CStr::from_ptr(update_json).to_str() else {
        return 2;
    };
    let Ok(update) = serde_json::from_str::<serde_json::Value>(payload) else {
        return 3;
    };
    if update["schema_version"] != 1
        || update["event"] != "account_snapshot"
        || update["account_id"] != "paper"
        || update["venue_id"] != "BINANCE"
        || update["ts"] != 10
    {
        if !error.is_null() && error_len > 0 {
            *error = b'i' as c_char;
            if error_len > 1 {
                *error.add(1) = 0;
            }
        }
        return 4;
    }
    let Some(event_seq) = update["event_seq"].as_u64() else {
        return 5;
    };
    LAST_ACCOUNT_UPDATE_EVENT_SEQ.store(event_seq, Ordering::SeqCst);
    ACCOUNT_UPDATE_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_legacy_fill_update(
    _handle: QxStrategyHandle,
    update_json: *const c_char,
    _decision: *mut QxStrategyDecision,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    if update_json.is_null() {
        return 1;
    }
    let Ok(payload) = std::ffi::CStr::from_ptr(update_json).to_str() else {
        return 2;
    };
    let Ok(update) = serde_json::from_str::<serde_json::Value>(payload) else {
        return 3;
    };
    let Some(event_seq) = update["order"]["order_id"].as_u64() else {
        return 4;
    };
    if update["schema_version"] != 1 || update["event"] != "fill" {
        return 5;
    }
    LAST_FILL_EVENT_SEQ.store(event_seq, Ordering::SeqCst);
    LAST_FILL_SCHEMA_VERSION.store(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn fake_order_status_v2(
    _handle: QxStrategyHandle,
    update: *const QxStrategyOrderStatusUpdateV2,
    _decision: *mut QxStrategyDecision,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let Some(update) = update.as_ref() else {
        return 1;
    };
    let exact_pointer = update.order.exact_amounts;
    let exact_matches = !exact_pointer.is_null()
        && (*exact_pointer).quantity.scale == 15
        && (*exact_pointer).quantity.coefficient.into_i128() == 123_456_789
        && (*exact_pointer).filled.coefficient.into_i128() == 0;
    let text_matches = |pointer: *const c_char, length: usize, expected: &[u8]| {
        !pointer.is_null()
            && length == expected.len()
            && std::slice::from_raw_parts(pointer.cast::<u8>(), length) == expected
    };
    let valid = update.schema_version == 1
        && update.event == QX_ORDER_STATUS_EVENT_ORDER_STATUS
        && update.event_seq == 23
        && update.ts == 10
        && update.status == QX_ORDER_STATUS_ACCEPTED
        && update.order.order_id == 7
        && update.order.side == QX_ORDER_SIDE_BUY
        && update.order.qty_raw.into_i128() == 0
        && update.order.filled_raw.into_i128() == 0
        && text_matches(
            update.order.account_id,
            update.order.account_id_len,
            b"paper",
        )
        && text_matches(
            update.order.instrument,
            update.order.instrument_len,
            b"BTCUSDT.BINANCE",
        )
        && text_matches(
            update.order.strategy_id,
            update.order.strategy_id_len,
            b"update-test",
        )
        && update.has_reason == 1
        && !update.reason.is_null()
        && update.reason_len == 0
        && exact_matches;
    LAST_ORDER_STATUS_EVENT_SEQ.store(update.event_seq, Ordering::SeqCst);
    LAST_TYPED_ORDER_STATUS_OK.store(usize::from(valid), Ordering::SeqCst);
    if valid {
        0
    } else {
        2
    }
}

unsafe extern "C" fn fake_order_policy(
    _handle: QxStrategyHandle,
    _intent_id: u64,
    output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let payload =
        br#"{"schema_version":1,"margin_mode":"cross","position_mode":"hedge","leverage":7}"#;
    if output.is_null() || output_len.is_null() || output_capacity < payload.len() {
        return 1;
    }
    std::ptr::copy_nonoverlapping(payload.as_ptr(), output.cast::<u8>(), payload.len());
    *output_len = payload.len();
    0
}

unsafe extern "C" fn fake_order_policy_unknown_field(
    _handle: QxStrategyHandle,
    _intent_id: u64,
    output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let payload = br#"{"schema_version":1,"leverage":7,"margin_mod":"cross"}"#;
    if output.is_null() || output_len.is_null() || output_capacity < payload.len() {
        return 1;
    }
    std::ptr::copy_nonoverlapping(payload.as_ptr(), output.cast::<u8>(), payload.len());
    *output_len = payload.len();
    0
}

unsafe extern "C" fn fake_order_policy_zero_leverage(
    _handle: QxStrategyHandle,
    _intent_id: u64,
    output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let payload = br#"{"schema_version":1,"leverage":0}"#;
    if output.is_null() || output_len.is_null() || output_capacity < payload.len() {
        return 1;
    }
    std::ptr::copy_nonoverlapping(payload.as_ptr(), output.cast::<u8>(), payload.len());
    *output_len = payload.len();
    0
}

unsafe extern "C" fn fake_order_policy_partial(
    _handle: QxStrategyHandle,
    _intent_id: u64,
    output: *mut c_char,
    output_capacity: usize,
    output_len: *mut usize,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    let payload = br#"{"schema_version":1,"leverage":7}"#;
    if output.is_null() || output_len.is_null() || output_capacity < payload.len() {
        return 1;
    }
    std::ptr::copy_nonoverlapping(payload.as_ptr(), output.cast::<u8>(), payload.len());
    *output_len = payload.len();
    0
}

static FAKE_VTABLE: QxStrategyVTable = QxStrategyVTable {
    abi_version: QX_C_STRATEGY_ABI_VERSION,
    create: Some(fake_create),
    on_init: Some(fake_init),
    on_event: Some(fake_event),
    on_order_update: None,
    free_decision: Some(fake_free),
    destroy: Some(fake_destroy),
};

static EXACT_CONTEXT_REJECTED_INIT_VTABLE: QxStrategyVTable = QxStrategyVTable {
    abi_version: QX_C_STRATEGY_ABI_VERSION,
    create: Some(fake_create),
    on_init: Some(fake_count_exact_context_rejected_init),
    on_event: Some(fake_event),
    on_order_update: None,
    free_decision: Some(fake_free),
    destroy: Some(fake_destroy),
};

static FAKE_TIMER_VTABLE: QxStrategyVTable = QxStrategyVTable {
    abi_version: QX_C_STRATEGY_ABI_VERSION,
    create: Some(fake_create),
    on_init: Some(fake_init),
    on_event: Some(fake_timer_event),
    on_order_update: None,
    free_decision: Some(fake_free),
    destroy: Some(fake_destroy),
};

static LEGACY_FILL_VTABLE: QxStrategyVTable = QxStrategyVTable {
    abi_version: QX_C_STRATEGY_ABI_VERSION,
    create: Some(fake_create),
    on_init: Some(fake_init),
    on_event: Some(fake_event),
    on_order_update: Some(fake_legacy_fill_update),
    free_decision: Some(fake_free),
    destroy: Some(fake_destroy),
};

#[test]
fn raw128_round_trips_signed_extremes() {
    for value in [0, 1, -1, i128::MAX, i128::MIN] {
        assert_eq!(QxRaw128::from_i128(value).into_i128(), value);
    }
}

#[test]
fn c_layout_matches_expected_pointer_free_sizes() {
    assert_eq!(std::mem::size_of::<QxRaw128>(), 16);
    assert_eq!(std::mem::align_of::<QxRaw128>(), 8);
    assert_eq!(QX_C_STRATEGY_API_VERSION, crate::STRATEGY_API_VERSION);
    assert_eq!(QX_C_STRATEGY_ABI_VERSION, 2);
}

#[test]
fn plugin_return_strings_use_explicit_bounded_utf8_lengths() {
    assert_eq!(
        read_required_string(c"qianxing".as_ptr(), 8, "strategy_id").unwrap(),
        "qianxing"
    );
    assert!(
        read_required_string(c"x".as_ptr(), MAX_PLUGIN_STRING_BYTES + 1, "strategy_id")
            .unwrap_err()
            .contains("长度非法")
    );
    assert!(read_required_string(c"x".as_ptr(), 2, "strategy_id")
        .unwrap_err()
        .contains("内嵌 NUL"));
}

#[test]
fn old_c_abi_vtable_is_rejected_before_plugin_callbacks() {
    static OLD_VTABLE: QxStrategyVTable = QxStrategyVTable {
        abi_version: QX_C_STRATEGY_API_VERSION,
        create: Some(fake_create),
        on_init: Some(fake_init),
        on_event: Some(fake_event),
        on_order_update: None,
        free_decision: Some(fake_free),
        destroy: Some(fake_destroy),
    };
    let error = unsafe { CAbiStrategy::from_vtable(&OLD_VTABLE, "{}") }
        .err()
        .unwrap();
    assert!(error.contains("版本"));
}

#[test]
fn c_abi_optional_diagnostic_state_is_bounded_json_and_read_only() {
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.diagnostic_state = Some(fake_diagnostic_state);
    assert_eq!(
        Strategy::diagnostic_state(&strategy),
        Some(serde_json::json!({
            "warmup_count": 7,
            "rolling_sum_raw": "4200"
        }))
    );
    strategy.diagnostic_state = Some(fake_diagnostic_state_oversize);
    assert_eq!(Strategy::diagnostic_state(&strategy), None);
}

#[test]
fn c_abi_state_checkpoint_requires_pair_and_round_trips_bounded_json() {
    CHECKPOINT_COUNTER.store(0, Ordering::SeqCst);
    let mut strategy = unsafe {
        CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}")
            .unwrap()
            .with_checkpoint_snapshot(fake_checkpoint_snapshot)
            .with_checkpoint_restore(fake_checkpoint_restore)
    };
    assert!(strategy.supports_state_checkpoint());
    strategy
        .restore_strategy_checkpoint(&serde_json::json!({ "counter": 42 }))
        .unwrap();
    assert_eq!(
        strategy.snapshot_strategy_checkpoint().unwrap(),
        serde_json::json!({ "counter": 42 })
    );

    let mut incomplete = unsafe {
        CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}")
            .unwrap()
            .with_checkpoint_snapshot(fake_checkpoint_snapshot)
    };
    assert!(!incomplete.supports_state_checkpoint());
    assert!(incomplete.has_partial_state_checkpoint_exports());
    assert!(incomplete
        .restore_strategy_checkpoint(&serde_json::json!({ "counter": 7 }))
        .is_err());
    assert!(incomplete.snapshot_strategy_checkpoint().is_err());
}

#[test]
fn c_abi_order_status_uses_optional_versioned_export_without_changing_vtable() {
    let context = StrategyContext {
        strategy_id: "update-test".into(),
        strategy_version: "v1".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let update: StrategyOrderStatusUpdate = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-order-status-accepted-v1.json"
    ))
    .unwrap();
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_order_status = Some(fake_order_status);
    LAST_ORDER_STATUS_EVENT_SEQ.store(0, Ordering::SeqCst);

    let decision = Strategy::on_strategy_order_status(&mut strategy, &context, &update).unwrap();

    assert!(decision.is_none());
    assert_eq!(LAST_ORDER_STATUS_EVENT_SEQ.load(Ordering::SeqCst), 23);
}

#[test]
fn c_abi_order_status_prefers_typed_v2_and_preserves_exact_decimal_values() {
    let context = StrategyContext {
        strategy_id: "update-test".into(),
        strategy_version: "v1".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let mut update: StrategyOrderStatusUpdate = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-order-status-accepted-v1.json"
    ))
    .unwrap();
    update.order.qty_raw = 0;
    update.order.filled_raw = 0;
    update.order.exact_amounts = Some(qx_core::ExactOrderAmounts {
        quantity: qx_core::ExactDecimal::parse("0.000000123456789").unwrap(),
        limit_price: Some(qx_core::ExactDecimal::parse("0.000041").unwrap()),
        filled: qx_core::ExactDecimal::ZERO,
        quote_budget: None,
        quote_filled: qx_core::ExactDecimal::ZERO,
        market_reference_price: None,
    });
    update.reason = Some(String::new());
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_order_status = Some(fake_order_status);
    strategy.on_order_status_v2 = Some(fake_order_status_v2);
    LAST_ORDER_STATUS_EVENT_SEQ.store(0, Ordering::SeqCst);
    LAST_TYPED_ORDER_STATUS_OK.store(0, Ordering::SeqCst);

    let decision = Strategy::on_strategy_order_status(&mut strategy, &context, &update).unwrap();

    assert!(decision.is_none());
    assert_eq!(LAST_ORDER_STATUS_EVENT_SEQ.load(Ordering::SeqCst), 23);
    assert_eq!(LAST_TYPED_ORDER_STATUS_OK.load(Ordering::SeqCst), 1);
}

#[test]
fn c_abi_fill_v2_receives_event_sequence_and_falls_back_to_legacy_projection() {
    let context = StrategyContext {
        strategy_id: "update-test".into(),
        strategy_version: "v1".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let update: StrategyFillUpdateV2 = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-fill-update-v2.json"
    ))
    .unwrap();
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_fill_update_v2 = Some(fake_fill_update_v2);
    LAST_FILL_EVENT_SEQ.store(0, Ordering::SeqCst);
    LAST_FILL_SCHEMA_VERSION.store(0, Ordering::SeqCst);

    let decision = Strategy::on_strategy_fill_update_v2(&mut strategy, &context, &update).unwrap();

    assert!(decision.is_none());
    assert!(strategy.supports_fill_updates());
    assert_eq!(LAST_FILL_EVENT_SEQ.load(Ordering::SeqCst), 9);
    assert_eq!(LAST_FILL_SCHEMA_VERSION.load(Ordering::SeqCst), 2);

    let mut legacy = unsafe { CAbiStrategy::from_vtable(&LEGACY_FILL_VTABLE, "{}") }.unwrap();
    LAST_FILL_EVENT_SEQ.store(0, Ordering::SeqCst);
    LAST_FILL_SCHEMA_VERSION.store(0, Ordering::SeqCst);
    let decision = Strategy::on_strategy_fill_update_v2(&mut legacy, &context, &update).unwrap();

    assert!(decision.is_none());
    assert!(legacy.supports_fill_updates());
    assert_eq!(LAST_FILL_EVENT_SEQ.load(Ordering::SeqCst), 7);
    assert_eq!(LAST_FILL_SCHEMA_VERSION.load(Ordering::SeqCst), 1);
}

#[test]
fn c_abi_account_snapshot_callback_receives_versioned_eventlog_fact() {
    let context = StrategyContext {
        strategy_id: "update-test".into(),
        strategy_version: "v1".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-account-update-sequence-v1.json"
    ))
    .expect("shared account update fixture is valid JSON");
    let update: crate::contract::StrategyAccountUpdateV1 =
        serde_json::from_value(fixture["events"][0].clone())
            .expect("Rust account update accepts shared fixture");
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_account_update = Some(fake_account_update_v1);
    LAST_ACCOUNT_UPDATE_EVENT_SEQ.store(0, Ordering::SeqCst);
    ACCOUNT_UPDATE_CALLBACK_COUNT.store(0, Ordering::SeqCst);

    let decision = Strategy::on_strategy_account_update(&mut strategy, &context, &update).unwrap();

    assert!(decision.is_none());
    assert!(strategy.supports_account_updates());
    assert_eq!(LAST_ACCOUNT_UPDATE_EVENT_SEQ.load(Ordering::SeqCst), 0);
    assert_eq!(ACCOUNT_UPDATE_CALLBACK_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn strategy_account_update_rejects_wrong_account_or_venue() {
    let context = StrategyContext {
        strategy_id: "update-test".into(),
        strategy_version: "v1".into(),
        account_id: "paper".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let update = crate::contract::StrategyAccountUpdateV1 {
        schema_version: 1,
        event: "account_snapshot".into(),
        event_seq: 24,
        ts: 10,
        account_id: "other".into(),
        venue_id: "BINANCE".into(),
        balances: Some(Vec::new()),
        positions: None,
    };
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    let error = Strategy::on_strategy_account_update(&mut strategy, &context, &update).unwrap_err();
    assert!(error.contains("account/venue"));
}

#[test]
fn c_abi_optional_order_policy_maps_derivative_fields_without_changing_vtable() {
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000_000),
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.order_policy = Some(fake_order_policy);

    let decision = strategy
        .on_event(
            &context,
            &MarketEvent::Timer {
                name: "heartbeat".into(),
                ts: 10,
            },
        )
        .unwrap();

    let policy = decision.intents[0].policy.unwrap();
    assert_eq!(policy.margin_mode, qx_core::MarginMode::Cross);
    assert_eq!(policy.position_mode, qx_core::PositionMode::Hedge);
    assert_eq!(policy.leverage, 7);
    assert_eq!(policy.position_side, PositionSide::Net);
}

#[test]
fn c_abi_timer_event_matches_the_shared_rust_and_python_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-market-timer-v1.json"
    ))
    .expect("shared Timer event fixture is valid JSON");
    let event: MarketEvent =
        serde_json::from_value(fixture).expect("Rust MarketEvent accepts Timer fixture");
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "timer:test".into(),
        as_of: 1_000,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_TIMER_VTABLE, "{}") }.unwrap();

    let decision = strategy
        .on_event(&context, &event)
        .expect("C ABI host maps the shared Timer fixture");

    assert_eq!(LAST_TIMER_MARKET_EVENT_OK.load(Ordering::SeqCst), 1);
    assert_eq!(decision.signal_id, 1_000);
}

#[test]
fn c_abi_optional_warmup_callbacks_match_the_shared_lifecycle_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-warmup-complete-v1.json"
    ))
    .expect("shared warmup lifecycle fixture is valid JSON");
    let warmup_bars = fixture["warmup_bars"].as_u64().unwrap();
    let event = &fixture["warmup_complete"];
    let instrument = InstrumentId::parse(event["instrument"].as_str().unwrap()).unwrap();
    let observed_bars = event["observed_bars"].as_u64().unwrap();
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy = unsafe { strategy.with_history_requirements_callback(fake_history_requirements) };
    strategy = unsafe { strategy.with_warmup_complete_callback(fake_warmup_complete) };

    let requirements = strategy.history_requirements().unwrap();
    LAST_WARMUP_BARS.store(requirements.warmup_bars, Ordering::SeqCst);
    assert_eq!(requirements.warmup_bars, warmup_bars);
    WARMUP_COMPLETE_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    WARMUP_COMPLETE_EVENT_OK.store(0, Ordering::SeqCst);
    strategy
        .on_warmup_complete(
            &StrategyContext {
                strategy_id: "warmup-test".into(),
                strategy_version: "1".into(),
                account_id: "main".into(),
                venue_id: "BINANCE".into(),
                data_fingerprint: "warmup-fixture".into(),
                as_of: 2,
                positions: BTreeMap::new(),
                cash: BTreeMap::new(),
                available_margin_raw: None,
                risk_state: "clear".into(),
                positions_exact: BTreeMap::new(),
                cash_exact: BTreeMap::new(),
                available_margin_exact: None,
            },
            Some(&instrument),
            observed_bars,
        )
        .unwrap();

    assert_eq!(LAST_WARMUP_BARS.load(Ordering::SeqCst), 2);
    assert_eq!(WARMUP_COMPLETE_CALLBACK_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(WARMUP_COMPLETE_EVENT_OK.load(Ordering::SeqCst), 1);
}

#[test]
fn c_abi_optional_order_policy_rejects_unknown_fields_and_zero_leverage() {
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000_000),
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    for (callback, expected) in [
        (
            fake_order_policy_unknown_field as QxStrategyOrderPolicyCallbackV1,
            "unknown field",
        ),
        (fake_order_policy_zero_leverage, "leverage 必须大于0"),
        (
            fake_order_policy_partial,
            "margin_mode、position_mode、leverage 必须同时提供",
        ),
    ] {
        let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
        strategy.order_policy = Some(callback);
        let result = strategy.on_event(
            &context,
            &MarketEvent::Timer {
                name: "heartbeat".into(),
                ts: 10,
            },
        );
        assert!(result.unwrap_err().contains(expected));
    }
}

#[test]
fn verified_loader_rejects_invalid_digest_and_size() {
    let path = std::env::temp_dir().join(format!(
        "qianxing-strategy-digest-{}.bin",
        std::process::id()
    ));
    fs::write(&path, b"qianxing-plugin").unwrap();
    let bad = DynamicCAbiLoadPolicy::new("00".repeat(32));
    assert!(verify_library_file(&path, &bad)
        .unwrap_err()
        .contains("SHA-256 不匹配"));
    let digest = ring::digest::digest(&ring::digest::SHA256, b"qianxing-plugin");
    let expected = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let too_small = DynamicCAbiLoadPolicy::new(expected).with_max_library_bytes(1);
    assert!(verify_library_file(&path, &too_small)
        .unwrap_err()
        .contains("大小非法"));
    let _ = fs::remove_file(path);
}

#[test]
fn verified_loader_accepts_and_rejects_detached_ed25519_signature() {
    let path = std::env::temp_dir().join(format!(
        "qianxing-strategy-signature-{}.bin",
        std::process::id()
    ));
    let bytes = b"qianxing-signed-plugin";
    fs::write(&path, bytes).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let expected = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let seed = [9_u8; 32];
    let key_pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let signature = key_pair.sign(bytes);
    let public_key = key_pair
        .public_key()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let signature_hex = signature
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let policy = DynamicCAbiLoadPolicy::new(expected.clone())
        .with_ed25519_signature(public_key, signature_hex.clone());
    verify_library_file(&path, &policy).unwrap();
    let bad =
        DynamicCAbiLoadPolicy::new(expected).with_ed25519_signature("00".repeat(32), signature_hex);
    assert!(verify_library_file(&path, &bad).is_err());
    let _ = fs::remove_file(path);
}

#[test]
fn decode_decision_rejects_an_invalid_c_side_value() {
    // Keep plugin-controlled `side` as a plain u32: invalid values for a
    // repr(C) enum would be UB before the rejection arm can run.
    let intent = Box::new(QxOrderIntent {
        intent_id: 1,
        instrument: c"BTCUSDT.BINANCE".as_ptr(),
        instrument_len: c"BTCUSDT.BINANCE".to_bytes().len(),
        side: 3,
        qty_raw: QxRaw128::from_i128(1_000_000_000),
        limit_price_raw: QxRaw128::from_i128(0),
        has_limit_price: 0,
        reduce_only: 0,
        post_only: 0,
        position_side: ptr::null(),
        position_side_len: 0,
    });
    let raw = QxStrategyDecision {
        schema_version: QX_C_STRATEGY_API_VERSION,
        request_id: c"req-1".as_ptr(),
        request_id_len: c"req-1".to_bytes().len(),
        strategy_id: c"abi-test".as_ptr(),
        strategy_id_len: c"abi-test".to_bytes().len(),
        signal_id: 1,
        confidence: QxRaw128::from_i128(0),
        priority: 0,
        expires_at: 100,
        intents: Box::into_raw(intent).cast(),
        intents_len: 1,
        intents_capacity: 1,
    };
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000_000),
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let strategy = CAbiStrategy {
        vtable: &FAKE_VTABLE,
        handle: ptr::null_mut(),
        diagnostic_state: None,
        checkpoint_snapshot: None,
        checkpoint_restore: None,
        on_start: None,
        on_stop: None,
        on_stop_v2: None,
        on_error: None,
        history_requirements: None,
        on_warmup_complete: None,
        on_order_status: None,
        on_account_update: None,
        on_order_status_v2: None,
        on_fill_update_v2: None,
        on_market_event_v2: None,
        on_market_event_v3: None,
        on_context_v2: None,
        order_policy: None,
    };
    let error = strategy.decode_decision(&raw, &context, 10).unwrap_err();
    // Two deliberate leaks: `decode_decision` returns before `free_decision`
    // runs (that happens in `on_event`), and the strategy's `handle` is null,
    // so its `Drop` would call `fake_destroy` on a null pointer.
    std::mem::forget(strategy);
    assert!(
        error.contains("C ABI intent side 非法") && error.contains("3"),
        "the rejection must name the offending field and value: {error}",
    );
}

#[test]
fn decode_decision_rejects_intent_length_above_declared_capacity() {
    let raw = QxStrategyDecision {
        schema_version: QX_C_STRATEGY_API_VERSION,
        request_id: ptr::null(),
        request_id_len: 0,
        strategy_id: ptr::null(),
        strategy_id_len: 0,
        signal_id: 1,
        confidence: QxRaw128::from_i128(0),
        priority: 0,
        expires_at: 10,
        intents: ptr::null_mut(),
        intents_len: 1,
        intents_capacity: 0,
    };
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 1,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    let strategy = CAbiStrategy {
        vtable: &FAKE_VTABLE,
        handle: ptr::null_mut(),
        diagnostic_state: None,
        checkpoint_snapshot: None,
        checkpoint_restore: None,
        on_start: None,
        on_stop: None,
        on_stop_v2: None,
        on_error: None,
        history_requirements: None,
        on_warmup_complete: None,
        on_order_status: None,
        on_account_update: None,
        on_order_status_v2: None,
        on_fill_update_v2: None,
        on_market_event_v2: None,
        on_market_event_v3: None,
        on_context_v2: None,
        order_policy: None,
    };
    let error = strategy.decode_decision(&raw, &context, 1).unwrap_err();
    std::mem::forget(strategy);
    assert!(
        error.contains("len=1 capacity=0"),
        "unexpected error: {error}"
    );
}

#[test]
fn c_abi_strategy_copies_plugin_decision_before_freeing_it() {
    let destroyed_before = DESTROYED.load(Ordering::SeqCst);
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000_000),
        risk_state: "ready".into(),
    };
    let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    strategy.on_init(&context).unwrap();
    let decision = strategy
        .on_event(
            &context,
            &MarketEvent::Timer {
                name: "heartbeat".into(),
                ts: 10,
            },
        )
        .unwrap();
    assert_eq!(decision.intents.len(), 1);
    assert_eq!(decision.intents[0].qty.raw(), 1_000_000_000);
    drop(strategy);
    assert!(
        DESTROYED.load(Ordering::SeqCst) > destroyed_before,
        "dropping the adapter must call the plugin destructor"
    );
}

#[test]
fn c_abi_exact_context_v2_preserves_sub_nano_account_state_and_legacy_fails_closed() {
    EXACT_CONTEXT_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    LAST_EXACT_CONTEXT_VALUES_OK.store(0, Ordering::SeqCst);
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-context-v2.json"
    ))
    .unwrap();
    assert_eq!(fixture["schema_version"], 2);
    let context_v1 = &fixture["context_v1"];
    let raw_map = |field: &str| {
        context_v1[field]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), i128::from(value.as_i64().unwrap())))
            .collect::<BTreeMap<_, _>>()
    };
    let exact_map = |field: &str| {
        fixture[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                let coefficient = item["coefficient"]
                    .as_str()
                    .unwrap()
                    .parse::<i128>()
                    .unwrap();
                let scale = u8::try_from(item["scale"].as_u64().unwrap()).unwrap();
                (
                    item["key"].as_str().unwrap().to_string(),
                    ExactDecimal::from_parts(coefficient, scale).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let available_margin_exact = &fixture["available_margin_exact"];
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: context_v1["account_id"].as_str().unwrap().into(),
        venue_id: context_v1["venue_id"].as_str().unwrap().into(),
        data_fingerprint: context_v1["data_fingerprint"].as_str().unwrap().into(),
        as_of: context_v1["as_of"].as_u64().unwrap(),
        positions: raw_map("positions"),
        cash: raw_map("cash"),
        available_margin_raw: Some(i128::from(
            context_v1["available_margin_raw"].as_i64().unwrap(),
        )),
        risk_state: context_v1["risk_state"].as_str().unwrap().into(),
        positions_exact: exact_map("positions_exact"),
        cash_exact: exact_map("cash_exact"),
        available_margin_exact: Some(
            ExactDecimal::from_parts(
                available_margin_exact["coefficient"]
                    .as_str()
                    .unwrap()
                    .parse::<i128>()
                    .unwrap(),
                u8::try_from(available_margin_exact["scale"].as_u64().unwrap()).unwrap(),
            )
            .unwrap(),
        ),
    };

    EXACT_CONTEXT_REJECTED_INIT_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    let mut legacy =
        unsafe { CAbiStrategy::from_vtable(&EXACT_CONTEXT_REJECTED_INIT_VTABLE, "{}") }.unwrap();
    let error = legacy.on_init(&context).unwrap_err();
    assert!(error.contains("qx_strategy_on_context_v2"), "{error}");
    assert_eq!(
        EXACT_CONTEXT_REJECTED_INIT_CALLBACK_COUNT.load(Ordering::SeqCst),
        0,
        "a lossy legacy context must fail before calling plugin on_init"
    );

    let mut precise = unsafe {
        CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}")
            .unwrap()
            .with_context_callback_v2(fake_exact_context_v2)
    };
    assert!(precise.supports_exact_context());
    precise.on_init(&context).unwrap();
    assert_eq!(EXACT_CONTEXT_CALLBACK_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(LAST_EXACT_CONTEXT_VALUES_OK.load(Ordering::SeqCst), 1);

    let mut rejected = unsafe {
        CAbiStrategy::from_vtable(&EXACT_CONTEXT_REJECTED_INIT_VTABLE, "{}")
            .unwrap()
            .with_context_callback_v2(fake_reject_exact_context_v2)
    };
    let error = rejected.on_init(&context).unwrap_err();
    assert!(error.contains("code=42"), "{error}");
    assert_eq!(
        EXACT_CONTEXT_REJECTED_INIT_CALLBACK_COUNT.load(Ordering::SeqCst),
        0,
        "a rejected v2 context must fail before calling plugin on_init"
    );

    precise
        .on_event(
            &context,
            &MarketEvent::Timer {
                name: "heartbeat".into(),
                ts: 10,
            },
        )
        .unwrap();
    assert_eq!(EXACT_CONTEXT_CALLBACK_COUNT.load(Ordering::SeqCst), 2);
    assert_eq!(LAST_EXACT_CONTEXT_VALUES_OK.load(Ordering::SeqCst), 1);
}

#[test]
fn c_abi_market_event_v2_preserves_exact_ohlcv_and_v1_fails_closed() {
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000_000),
        risk_state: "ready".into(),
    };
    let event = MarketEvent::Bar {
        instrument: InstrumentId::parse("ALTUSDT.BINANCE").unwrap(),
        ts: 10,
        open_raw: 0,
        high_raw: 0,
        low_raw: 0,
        close_raw: 0,
        volume_raw: 0,
        exact_ohlcv: Some(qx_core::ExactOhlcv {
            open: ExactDecimal::parse("0.0000000005").unwrap(),
            high: ExactDecimal::parse("0.0000000005").unwrap(),
            low: ExactDecimal::parse("0.0000000005").unwrap(),
            close: ExactDecimal::parse("0.0000000005").unwrap(),
            volume: ExactDecimal::parse("0.0000000000005").unwrap(),
        }),
    };

    let mut legacy =
        unsafe { CAbiStrategy::from_vtable(&EXACT_CONTEXT_REJECTED_INIT_VTABLE, "{}") }.unwrap();
    let error = legacy.on_event(&context, &event).unwrap_err();
    assert!(error.contains("拒绝降级为 raw v1"), "{error}");

    let mut precise = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    precise = unsafe { precise.with_market_event_callback_v2(fake_market_event_v2) };
    assert!(precise.supports_exact_market_data());
    let decision = precise.on_event(&context, &event).unwrap();
    assert_eq!(LAST_EXACT_MARKET_EVENT_OK.load(Ordering::SeqCst), 1);
    assert_eq!(decision.intents.len(), 1);
}

#[test]
fn c_abi_exact_orderbook_requires_v3_and_preserves_sub_raw_precision() {
    let context = StrategyContext {
        strategy_id: "abi-test".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "depth-v2".into(),
        as_of: 10,
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "ready".into(),
    };
    let event: MarketEvent = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-market-orderbook-v1.json"
    ))
    .expect("shared precise order-book fixture is valid JSON");
    event.validate().unwrap();

    let mut raw_book = event.clone();
    if let MarketEvent::OrderBook { bids, asks, .. } = &mut raw_book {
        bids[0].price_raw = 12_000_000_000;
        bids[0].qty_raw = 1_000_000_000;
        bids[0].price_exact = None;
        bids[0].qty_exact = None;
        asks[0].price_raw = 12_000_000_001;
        asks[0].qty_raw = 1_000_000_000;
        asks[0].price_exact = None;
        asks[0].qty_exact = None;
    }
    raw_book.validate().unwrap();
    let mut raw_legacy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    let raw_decision = raw_legacy.on_event(&context, &raw_book).unwrap();
    assert_eq!(raw_decision.intents.len(), 1);

    let mut legacy =
        unsafe { CAbiStrategy::from_vtable(&EXACT_CONTEXT_REJECTED_INIT_VTABLE, "{}") }.unwrap();
    legacy = unsafe { legacy.with_market_event_callback_v2(fake_market_event_v2) };
    let error = legacy.on_event(&context, &event).unwrap_err();
    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/strategy-lifecycle-matrix-v1.json"
    ))
    .unwrap();
    let unsupported = &matrix["unsupported_exact_orderbook"];
    assert!(
        error.contains(unsupported["error_contains"].as_str().unwrap()),
        "{error}"
    );
    assert!(unsupported["must_not_downgrade_to_raw"].as_bool().unwrap());
    assert!(error.contains("拒绝降级为 raw v1/v2"), "{error}");

    let mut precise = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
    precise = unsafe { precise.with_market_event_callback_v3(fake_market_event_v3) };
    assert!(precise.supports_exact_orderbook());
    let decision = precise.on_event(&context, &event).unwrap();
    assert_eq!(LAST_EXACT_ORDERBOOK_EVENT_OK.load(Ordering::SeqCst), 1);
    assert_eq!(decision.intents.len(), 1);
}
