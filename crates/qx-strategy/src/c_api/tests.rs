use super::*;
use ring::signature::KeyPair;
use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTROYED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn fake_create(_config: *const c_char) -> QxStrategyHandle {
    Box::into_raw(Box::new(7_u8)).cast()
}

unsafe extern "C" fn fake_init(
    _handle: QxStrategyHandle,
    _context: *const QxStrategyContext,
    _error: *mut c_char,
    _error_len: usize,
) -> i32 {
    0
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

static FAKE_VTABLE: QxStrategyVTable = QxStrategyVTable {
    abi_version: QX_C_STRATEGY_ABI_VERSION,
    create: Some(fake_create),
    on_init: Some(fake_init),
    on_event: Some(fake_event),
    on_order_update: None,
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
    };
    let strategy = CAbiStrategy {
        vtable: &FAKE_VTABLE,
        handle: ptr::null_mut(),
        diagnostic_state: None,
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
    };
    let strategy = CAbiStrategy {
        vtable: &FAKE_VTABLE,
        handle: ptr::null_mut(),
        diagnostic_state: None,
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
