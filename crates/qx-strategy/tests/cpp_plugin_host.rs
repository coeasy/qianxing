use qx_core::InstrumentId;
use qx_strategy::{
    sha256_hex, DynamicCAbiLoadPolicy, DynamicCAbiStrategy, MarketEvent, Strategy, StrategyContext,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn loads_and_runs_cpp_strategy_through_the_rust_c_abi_host() {
    let library = PathBuf::from(
        std::env::var_os("QX_CPP_STRATEGY_LIBRARY")
            .expect("CI must provide the C++ strategy shared-library path"),
    );
    let bytes = std::fs::read(&library).expect("read the C++ strategy shared library");
    let policy = DynamicCAbiLoadPolicy::new(sha256_hex(&bytes))
        .with_max_library_bytes(u64::try_from(bytes.len()).expect("library length fits u64"));
    let mut strategy = unsafe {
        DynamicCAbiStrategy::load_verified(&library, "{}", &policy)
            .expect("load the C++ strategy through the verified C ABI host")
    };
    let context = StrategyContext {
        strategy_id: "cpp-ci-strategy".into(),
        strategy_version: "1".into(),
        account_id: "paper-ci".into(),
        venue_id: "paper".into(),
        data_fingerprint: "cpp-plugin-ci".into(),
        as_of: 10,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "clear".into(),
    };
    strategy.on_init(&context).expect("C++ on_init callback");
    let event = MarketEvent::Bar {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").expect("valid test instrument"),
        ts: 10,
        open_raw: 100,
        high_raw: 101,
        low_raw: 99,
        close_raw: 100,
        volume_raw: 1,
    };
    let decision = strategy
        .on_event(&context, &event)
        .expect("C++ on_event callback");
    assert_eq!(decision.schema_version, 1);
    assert_eq!(decision.request_id, "cpp-ci-strategy:10");
    assert_eq!(decision.strategy_id, context.strategy_id);
    assert_eq!(decision.signal_id, 10);
    assert_eq!(decision.confidence, 500);
    assert!(decision.intents.is_empty());
    decision
        .validate_for(&context, event.ts())
        .expect("C++ decision passes the shared Rust strategy contract");
    strategy.on_stop().expect("C ABI on_stop");
}
