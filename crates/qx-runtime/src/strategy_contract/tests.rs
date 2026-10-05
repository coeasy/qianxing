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
    let mut mismatched = output.clone();
    mismatched.request_id = "other".into();
    // 折叠成一句「与输入身份、标的或有效期不一致」时，最需要点名的那一格反而读不出来：
    // worker 侧（Python 已按同口径逐臂点名）放行、宿主侧才挡下的跨语言分叉，只有字段名有用。
    assert!(mismatched.validate_for(&input).is_err());
    type ArmCase = (fn(&mut StrategyContractOutput), &'static str);
    let cases: [ArmCase; 6] = [
        (|output| output.schema_version = 0, "schema_version"),
        (|output| output.request_id = "other".into(), "request_id"),
        (|output| output.strategy_id = "other".into(), "strategy_id"),
        (|output| output.signal_id = 0, "signal_id"),
        (
            |output| output.instrument = "ETHUSDT.BINANCE".into(),
            "instrument 与输入不一致",
        ),
        (|output| output.expires_at = 9, "expires_at"),
    ];
    let mut messages: Vec<String> = Vec::new();
    for (mutate, needle) in cases {
        let mut probe = output.clone();
        mutate(&mut probe);
        let error = probe.validate_for(&input).unwrap_err();
        assert!(error.contains(needle), "诊断没有点名 {needle}：{error}");
        messages.push(error);
    }
    // 六臂各说各话：抄六遍同一句也能过上面的 contains，所以这里按去重后的条数判。
    messages.sort();
    messages.dedup();
    assert_eq!(messages.len(), 6, "逐臂点名退化成了同一句话");
    // 非法标的这一臂只在两格相等时走得到，所以连输入一起换。
    let unparsed = StrategyContractOutput {
        instrument: "BTCUSDT".into(),
        ..output.clone()
    };
    let unparsed_input = StrategyContractInput {
        instrument: "BTCUSDT".into(),
        ..input.clone()
    };
    assert!(unparsed
        .validate_for(&unparsed_input)
        .unwrap_err()
        .contains("instrument 非法"));
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
    // 契约的 `intents` 上写着 `uniqueItems`，运行时兑现它的只有 `validate_for` 里那次
    // `intent_ids.insert`。这两条 intent 除 id 外完全不同，所以去掉那条判据后没有别的
    // 臂会顺手把它们拒掉——约束就此退成只剩文档。
    let mut duplicated = restored;
    duplicated.intents[1].intent_id = duplicated.intents[0].intent_id;
    assert!(duplicated
        .validate_for(&input)
        .unwrap_err()
        .contains("重复"));
}

#[test]
fn strategy_output_decode_entry_rejects_foreign_schema_version() {
    let input = StrategyContractInput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-version".into(),
        strategy_id: "strategy-1".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-1".into(),
        as_of: 10,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(90),
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
        target_qty: 3,
        confidence: 900,
        priority: 1,
        // 与 `as_of` 相等是"恰好到期仍可"那一格：判定写成 `<=` 就在这里当场红。
        expires_at: 10,
        intents: Vec::new(),
    };
    // 宿主读策略产出的唯一入口是 `from_json_for`，而 `schema_version` 是 `u32`——serde 对
    // 任何版本号都解得开。删掉入口里那句 `value.validate_for(request)?` 之后，别家语言接受
    // 的版本号会在这条入口上被原样收下，所以版本判定必须钉在入口而不是只钉在 `validate_for`。
    let encoded = output.to_json_for(&input).unwrap();
    let foreign = encoded.replace("\"schema_version\":1", "\"schema_version\":2");
    assert_ne!(foreign, encoded, "版本号那一格没被改到，这条是一发空枪");
    let error = StrategyContractOutput::from_json_for(&foreign, &input).unwrap_err();
    // 要的是"版本这一臂"的话，不是解码失败：两种错法在文本上分得开。
    assert!(error.contains("schema_version"), "{error}");
}
