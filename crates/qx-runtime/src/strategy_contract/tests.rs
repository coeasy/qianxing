//! 策略进程契约用例：版本、PIT 边界、身份绑定与多意图输出。

use super::*;

#[test]
fn python_strategy_contract_is_versioned_pit_bounded_and_identity_bound() {
    let input = StrategyContractInput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-1".into(),
        strategy_id: "strategy-1".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::from([("BTCUSDT.BINANCE".into(), 2)]),
        cash: BTreeMap::from([("USDT".into(), 100)]),
        available_margin_raw: Some(90),
        risk_state: "verified".into(),
        research_targets: BTreeMap::from([("BTCUSDT.BINANCE".into(), 3)]),
        bars: Some(StrategyContractBars {
            source: "snapshot-1".into(),
            ts: vec![9, 10],
            open_raw: vec![1, 2],
            high_raw: vec![2, 3],
            low_raw: vec![1, 2],
            close_raw: vec![2, 3],
            volume_raw: vec![10, 11],
        }),
    };
    let restored = StrategyContractInput::from_json(&input.to_json().unwrap()).unwrap();
    assert_eq!(restored, input);
    let output = StrategyContractOutput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-1".into(),
        strategy_id: "strategy-1".into(),
        signal_id: 1,
        instrument: "BTCUSDT.BINANCE".into(),
        target_qty: 3,
        confidence: 900,
        priority: 1,
        expires_at: 10,
        intents: Vec::new(),
    };
    let encoded = output.to_json_for(&input).unwrap();
    assert_eq!(
        StrategyContractOutput::from_json_for(&encoded, &input).unwrap(),
        output
    );
    let mut mismatched = output;
    mismatched.request_id = "other".into();
    assert!(mismatched.validate_for(&input).is_err());
}

#[test]
fn legacy_target_contract_emits_close_rebalance_for_zero_target() {
    let input = StrategyContractInput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "close-request".into(),
        strategy_id: "strategy-close".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::from([("BTCUSDT.BINANCE".into(), 2)]),
        cash: BTreeMap::from([("USDT".into(), 100)]),
        available_margin_raw: Some(100),
        risk_state: "verified".into(),
        research_targets: BTreeMap::new(),
        bars: None,
    };
    let output = StrategyContractOutput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: input.request_id.clone(),
        strategy_id: input.strategy_id.clone(),
        signal_id: 1,
        instrument: input.instrument.clone(),
        target_qty: 0,
        confidence: 1_000,
        priority: 0,
        expires_at: 10,
        intents: Vec::new(),
    };
    let plan = output.build_rebalance_plan(&input, 10_000, 1).unwrap();
    assert_eq!(plan.positions.len(), 1);
    assert_eq!(plan.positions[0].target_qty, -2);
}

#[test]
fn strategy_columnar_input_preserves_metadata_and_fixed_width_columns() {
    let input = StrategyContractInput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "columnar-1".into(),
        strategy_id: "strategy-1".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(90),
        risk_state: "verified".into(),
        research_targets: BTreeMap::from([("BTCUSDT.BINANCE".into(), 3)]),
        bars: Some(StrategyContractBars {
            source: "snapshot-1".into(),
            ts: vec![9, 10],
            open_raw: vec![1, 2],
            high_raw: vec![2, 3],
            low_raw: vec![1, 2],
            close_raw: vec![2, 3],
            volume_raw: vec![10, 11],
        }),
    };
    let encoded = encode_strategy_columnar_input(&input).unwrap();
    assert_eq!(&encoded[..4], b"QXCB");
    assert_eq!(u16::from_le_bytes([encoded[4], encoded[5]]), 1);
    let metadata_len = u32::from_le_bytes(encoded[8..12].try_into().unwrap()) as usize;
    let metadata: serde_json::Value = serde_json::from_slice(
        &encoded[STRATEGY_COLUMNAR_HEADER_LEN..STRATEGY_COLUMNAR_HEADER_LEN + metadata_len],
    )
    .unwrap();
    assert!(metadata["bars"].is_null());
    assert_eq!(metadata["__qx_bars_source"], "snapshot-1");
    assert_eq!(
        encoded.len(),
        STRATEGY_COLUMNAR_HEADER_LEN + metadata_len + 2 * (8 + 5 * 16)
    );
}

#[test]
fn strategy_contract_accepts_multiple_order_intents() {
    let input = StrategyContractInput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-intents".into(),
        strategy_id: "strategy-portfolio".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(100),
        risk_state: "ready".into(),
        research_targets: BTreeMap::new(),
        bars: None,
    };
    let output = StrategyContractOutput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: input.request_id.clone(),
        strategy_id: input.strategy_id.clone(),
        signal_id: 99,
        instrument: input.instrument.clone(),
        target_qty: 0,
        confidence: 500,
        priority: 1,
        expires_at: 10,
        intents: vec![
            StrategyContractIntent {
                intent_id: 1001,
                instrument: "BTCUSDT.BINANCE".into(),
                side: "buy".into(),
                qty_raw: 2,
                limit_price_raw: Some(100),
                reduce_only: false,
                post_only: true,
                position_side: Some("net".into()),
                margin_mode: None,
                position_mode: None,
                leverage: None,
            },
            StrategyContractIntent {
                intent_id: 1002,
                instrument: "ETHUSDT.BINANCE".into(),
                side: "sell".into(),
                qty_raw: 1,
                limit_price_raw: None,
                reduce_only: true,
                post_only: false,
                position_side: Some("net".into()),
                margin_mode: None,
                position_mode: None,
                leverage: None,
            },
        ],
    };
    let encoded = output.to_json_for(&input).unwrap();
    let restored = StrategyContractOutput::from_json_for(&encoded, &input).unwrap();
    assert_eq!(restored, output);
}
