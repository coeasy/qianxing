use super::*;
use qx_sdk::{
    BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client, InstrumentId,
    MultiLegBacktestSpec, TradingInstrumentSpec, TradingProduct,
};

#[test]
fn rust_sdk_sizes_multi_leg_linear_contracts_from_frozen_market_specs() {
    fn derivative(instrument: &str, inverse: bool, contract_size: i128) -> TradingInstrumentSpec {
        let (base, quote) = instrument.split_once('.').unwrap().0.split_at(3);
        TradingInstrumentSpec {
            instrument: InstrumentId::parse(instrument).unwrap(),
            product: TradingProduct::Perpetual,
            base_currency: base.into(),
            quote_currency: quote.into(),
            settlement_currency: "USDT".into(),
            contract_size,
            linear: !inverse,
            inverse,
            price_tick: 1_000,
            qty_step: 10_000_000,
            min_qty: 10_000_000,
            min_notional_raw: 0,
            max_leverage: 20,
            maintenance_margin_bps: 500,
            valid_from: 0,
            valid_to: None,
            exact_rules: None,
        }
    }

    let output_dir = scratch("sdk-multi-leg-fixed-notional");
    let root = super::repo_root().join("deploy");
    let mut strategy = BuiltinStrategySpec::new(
        BuiltinStrategyKind::PairsArbitrage,
        "sdk-fixed-notional-pair",
    );
    strategy.target_notional_raw = Some(1_000 * 1_000_000_000);
    let mut spec = MultiLegBacktestSpec::new(
        qx_sdk::MultiLegDatasetInput {
            run_id: ("sdk-fixed-notional-pair").into(),
            primary_bars_path: (root
                .join("qianxing.bar-frame.pairs-primary.example.json")
                .to_string_lossy())
            .into(),
            reference_bars_path: (root
                .join("qianxing.bar-frame.pairs-reference.example.json")
                .to_string_lossy())
            .into(),
        },
        "USDT",
        10_000 * 1_000_000_000,
        10_000 * 1_000_000_000,
        7,
        output_dir.to_string_lossy(),
        strategy,
    );
    spec.primary_market_spec = Some(derivative("BTCUSDT.BINANCE", false, 1_000_000_000));
    spec.reference_market_spec = Some(derivative("ETHUSDT.BINANCE", false, 1_000_000_000));

    let client = Client::new(CallerCapability::Research, "sdk-fixed-notional-pair");
    let result = client
        .run_multi_leg_backtest(&spec)
        .expect("public Rust SDK routes frozen per-leg sizing through qx-app");
    assert!(result.primary_fills > 0 && result.reference_fills > 0);
    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&result.attribution_artifact).expect("multi-leg artifact"),
    )
    .expect("valid multi-leg artifact");
    for leg in artifact["legs"].as_array().expect("leg facts") {
        let label = leg["label"].as_str().expect("leg label");
        let planned = leg["planned_qty_raw"]
            .as_str()
            .expect("raw planned qty")
            .parse::<i128>()
            .expect("integer planned qty");
        let lot = artifact["market_specs"][label]["qty_step"]
            .as_i64()
            .expect("frozen qty step") as i128;
        assert!(planned > 0);
        assert_eq!(planned % lot, 0);
    }
}
