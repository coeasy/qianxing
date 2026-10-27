//! 策略契约**输入方向**的钉子：`schemas/strategy-api-input-v1.json` 必须与 Rust 结构体
//! 逐键同形。
//!
//! 规划（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §4.2 第 8 条 / §18 M0）
//! 把「策略输入方向还缺少与 Rust 结构同步的机器可读 schema」列为已知缺口：output 方向有
//! `strategy_api_v1.schema.json` 与它自己的钉子（`strategy_contract_schema.rs`），输入方向却
//! 「Rust 有结构、靠文档」。文档不会随结构体一起变，于是字段漂移只有运行时才炸。这里补上另一半，
//! 用的正是 output 侧同一套可机械核对的三件事：键全集、必填集、未知键拒绝。
//!
//! 不引第三方 JSON Schema 校验器：能机械核对的部分就足够抓住「schema 少写一个键」和
//! 「schema 把可选键写成必填」这两类真实错形。

use std::collections::{BTreeMap, BTreeSet};

use qx_runtime::{
    StrategyContractBars, StrategyContractInput, StrategyContractMarketOrderBookV1,
    StrategyContractOrderBookLevelV1, STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION, STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION,
    STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION, STRATEGY_CONTRACT_SCHEMA_VERSION,
};
use serde_json::{json, Value};

fn schema() -> Value {
    // 仓库根的 `schemas/`：manifest 目录往上是 crates/qx-runtime 两级。
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v1.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("读取 schemas/strategy-api-input-v1.json 失败 {path:?}: {error}")
    });
    serde_json::from_str(&text).expect("strategy-api-input-v1.json 必须是合法 JSON")
}

/// 每一项都填满：可选键的空缺会让「schema 少写了一个键」看不出来。
fn full_input() -> StrategyContractInput {
    StrategyContractInput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "req-1".into(),
        strategy_id: "strategy-1".into(),
        strategy_version: "1.2.3".into(),
        data_fingerprint: "fp-1".into(),
        as_of: 1_800_000_000_000,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::from([
            ("BTCUSDT.BINANCE".to_string(), -3_000_000_000_i128),
            ("ETHUSDT.BINANCE".to_string(), 5_000_000_000_i128),
        ]),
        positions_exact: BTreeMap::from([
            ("BTCUSDT.BINANCE".to_string(), "-3.0000000001".to_string()),
            ("ETHUSDT.BINANCE".to_string(), "5".to_string()),
        ]),
        cash: BTreeMap::from([("USDT".to_string(), 1_000_000_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(750_000_000_000),
        risk_state: "normal".into(),
        research_targets: BTreeMap::from([("BTCUSDT.BINANCE".to_string(), 2_000_000_000_i128)]),
        market_bars: BTreeMap::new(),
        bars: Some(StrategyContractBars {
            source: "synthetic".into(),
            ts: vec![1_700_000_000_000, 1_700_000_060_000],
            open_raw: vec![65_000 * 1_000_000, 65_100 * 1_000_000],
            high_raw: vec![65_200 * 1_000_000, 65_300 * 1_000_000],
            low_raw: vec![64_900 * 1_000_000, 65_000 * 1_000_000],
            close_raw: vec![65_100 * 1_000_000, 65_250 * 1_000_000],
            volume_raw: vec![12_000_000_000, 9_000_000_000],
            exact_ohlcv: None,
        }),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
    }
}

#[test]
fn input_v2_schema_requires_committed_order_status_facts_and_v1_stays_compatible() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v2.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("读取 strategy-api-input-v2.json"),
    )
    .expect("strategy-api-input-v2.json 必须是合法 JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], json!(2));
    assert!(required_of(&schema).contains("execution_updates"));
    assert_eq!(
        schema["properties"]["execution_updates"]["minItems"],
        json!(1)
    );
    assert_eq!(
        schema["properties"]["execution_updates"]["items"]["$ref"],
        json!("strategy-order-status-update-v1.json")
    );

    let mut input = full_input();
    input.schema_version = STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION;
    let mut payload = serde_json::to_value(input).expect("serialize v2 input");
    payload["execution_updates"] = json!([{
        "schema_version": 1,
        "event": "order_status",
        "event_seq": 0,
        "ts": 1_700_000_060_001_u64,
        "status": "submitted",
        "order": {
            "order_id": 7,
            "account_id": "account-1",
            "instrument": "BTCUSDT.BINANCE",
            "side": "Buy",
            "qty_raw": 1_000_000_000_i64,
            "filled_raw": 0,
            "strategy_id": "strategy-1"
        }
    }]);
    assert!(StrategyContractInput::from_json(&payload.to_string()).is_ok());
}

#[test]
fn input_v3_schema_requires_event_log_bound_fills_and_allows_status_updates() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v3.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("读取 strategy-api-input-v3.json"),
    )
    .expect("strategy-api-input-v3.json 必须是合法 JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], json!(3));
    assert!(required_of(&schema).contains("fill_updates"));
    assert!(!required_of(&schema).contains("execution_updates"));
    assert_eq!(schema["properties"]["fill_updates"]["minItems"], json!(1));
    assert_eq!(
        schema["properties"]["fill_updates"]["items"]["$ref"],
        json!("strategy-fill-update-v2.json")
    );

    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let order = qx_core::Order {
        client_id: 7,
        instrument,
        side: qx_core::Side::Buy,
        qty: qx_core::Quantity::from_raw(1_000_000_000),
        limit: None,
        status: qx_core::OrderStatus::Accepted,
        filled: qx_core::Quantity::ZERO,
        account_id: "account-1".into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some("strategy-1".into()),
            signal_id: Some(1),
            intent_id: Some(7),
            rule_version: Some("rules-v1".into()),
        }),
        policy: None,
        exact_amounts: None,
    };
    let fill = qx_core::Fill {
        order_id: 7,
        qty: qx_core::Quantity::from_raw(500_000_000),
        price: qx_core::Price::from_raw(25_000_000_000),
        ts: 1_700_000_060_001,
        account_id: "account-1".into(),
        strategy_id: Some("strategy-1".into()),
        ..qx_core::Fill::default()
    };
    let fill_update = qx_strategy::StrategyFillUpdateV2::from_fill(&order, &fill, "strategy-1", 9)
        .expect("fill update must carry valid EventLog identity");
    let mut input = full_input();
    input.schema_version = STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION;
    input.execution_updates = vec![serde_json::from_value(json!({
        "schema_version": 1,
        "event": "order_status",
        "event_seq": 9,
        "ts": 1_700_000_060_001_u64,
        "status": "partially_filled",
        "order": {
            "order_id": 7,
            "account_id": "account-1",
            "instrument": "BTCUSDT.BINANCE",
            "side": "Buy",
            "qty_raw": 1_000_000_000_i64,
            "filled_raw": 500_000_000_i64,
            "strategy_id": "strategy-1"
        }
    }))
    .expect("order update matches v1 status contract")];
    input.fill_updates = vec![fill_update];
    input.as_of = 1_800_000_000_000;
    input.validate().expect("valid v3 input");

    let encoded = serde_json::to_value(&input).expect("serialize v3 input");
    assert_eq!(
        keys_of(&encoded),
        properties(&schema, "/properties"),
        "v3 schema keys must match the Rust wire shape"
    );
    let mut rust_required = actually_required::<StrategyContractInput>(&encoded);
    // The wire field has serde(default), while v3 semantic validation rejects an empty/missing list.
    rust_required.insert("fill_updates".into());
    assert_eq!(
        required_of(&schema),
        rust_required,
        "v3 schema required fields must match Rust deserialization and semantic validation"
    );
    assert!(StrategyContractInput::from_json(&encoded.to_string()).is_ok());

    let mut empty = input;
    empty.fill_updates.clear();
    assert!(empty.validate().unwrap_err().contains("非空 fill_updates"));
}

#[test]
fn input_v4_binds_a_strategy_checkpoint_and_keeps_execution_facts_optional() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v4.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("读取 strategy-api-input-v4.json"),
    )
    .expect("strategy-api-input-v4.json 必须是合法 JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], json!(4));
    assert!(required_of(&schema).contains("strategy_checkpoint"));
    assert!(!required_of(&schema).contains("execution_updates"));
    assert!(!required_of(&schema).contains("fill_updates"));
    assert_eq!(
        schema["properties"]["strategy_checkpoint"]["$ref"],
        json!("#/$defs/checkpoint")
    );

    let mut input = full_input();
    input.schema_version = STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION;
    input.strategy_checkpoint = Some(qx_strategy::contract::StrategyCheckpointV1 {
        schema_version: 1,
        strategy_id: input.strategy_id.clone(),
        strategy_version: input.strategy_version.clone(),
        adapter_state: json!({"schema_version": 1, "bars_seen": {}}),
        strategy_state: json!({"counter": 2}),
    });
    input.validate().expect("v4 checkpoint identity must match");

    let mut encoded = serde_json::to_value(&input).expect("serialize v4 input");
    // Empty vectors are omitted by serde for legacy byte compatibility, but remain legal v4 fields.
    encoded["execution_updates"] = json!([]);
    encoded["fill_updates"] = json!([]);
    assert_eq!(
        keys_of(&encoded),
        properties(&schema, "/properties"),
        "v4 schema keys must match the Rust wire shape"
    );
    let mut rust_required = actually_required::<StrategyContractInput>(&encoded);
    rust_required.insert("strategy_checkpoint".into());
    assert_eq!(required_of(&schema), rust_required);
    assert!(StrategyContractInput::from_json(&encoded.to_string()).is_ok());
    assert_eq!(
        properties(&schema, "/$defs/checkpoint/properties"),
        BTreeSet::from([
            "schema_version".to_string(),
            "strategy_id".to_string(),
            "strategy_version".to_string(),
            "adapter_state".to_string(),
            "strategy_state".to_string(),
        ])
    );

    let mut mismatched = input.clone();
    mismatched
        .strategy_checkpoint
        .as_mut()
        .unwrap()
        .strategy_version = "other".into();
    assert!(mismatched
        .validate()
        .unwrap_err()
        .contains("身份/版本不匹配"));
    let mut oversized = input;
    oversized
        .strategy_checkpoint
        .as_mut()
        .unwrap()
        .strategy_state = json!({"blob": "x".repeat(65 * 1024)});
    assert!(oversized.validate().unwrap_err().contains("64 KiB"));
}

#[test]
fn input_v5_preserves_exact_ohlcv_and_checks_legacy_projection() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v5.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("读取 strategy-api-input-v5.json"),
    )
    .expect("strategy-api-input-v5.json 必须是合法 JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], json!(5));
    assert!(!required_of(&schema).contains("strategy_checkpoint"));
    assert_eq!(
        schema["$defs"]["bars"]["properties"]["exact_ohlcv"]["items"]["oneOf"][1]["$ref"],
        json!("#/$defs/exact_ohlcv")
    );

    let exact = qx_core::ExactOhlcv {
        open: qx_core::ExactDecimal::parse("0.0000000005").unwrap(),
        high: qx_core::ExactDecimal::parse("0.0000000005").unwrap(),
        low: qx_core::ExactDecimal::parse("0.0000000005").unwrap(),
        close: qx_core::ExactDecimal::parse("0.0000000005").unwrap(),
        volume: qx_core::ExactDecimal::parse("0.0000000000005").unwrap(),
    };
    let mut input = full_input();
    input.bars = Some(StrategyContractBars {
        source: "subnano-source".into(),
        ts: vec![1_700_000_000_000],
        open_raw: vec![0],
        high_raw: vec![0],
        low_raw: vec![0],
        close_raw: vec![0],
        volume_raw: vec![0],
        exact_ohlcv: Some(vec![Some(exact)]),
    });
    input.schema_version = input.schema_version_for_features();
    assert_eq!(
        input.schema_version,
        STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION
    );
    input
        .validate()
        .expect("v5 exact OHLCV input must validate");
    let encoded = input.to_json().expect("serialize v5 input");
    let decoded = StrategyContractInput::from_json(&encoded).expect("decode v5 input");
    assert_eq!(decoded, input);
    let value: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(value["bars"]["open_raw"][0], json!(0));
    assert_eq!(
        value["bars"]["exact_ohlcv"][0]["open"]["coefficient"],
        json!("5")
    );

    let mut legacy_version = input.clone();
    legacy_version.schema_version = STRATEGY_CONTRACT_SCHEMA_VERSION;
    assert!(legacy_version.validate().unwrap_err().contains("v1-v4"));
    let mut drifted = input;
    drifted.bars.as_mut().unwrap().close_raw[0] = 1;
    assert!(drifted.validate().unwrap_err().contains("raw 投影不一致"));
}

fn keys_of(object: &Value) -> BTreeSet<String> {
    object
        .as_object()
        .unwrap_or_else(|| panic!("契约里这一项必须是对象: {object}"))
        .keys()
        .cloned()
        .collect()
}

fn required_of(node: &Value) -> BTreeSet<String> {
    node["required"]
        .as_array()
        .unwrap_or_else(|| panic!("契约必须写明 required 列表: {node}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("required 项必须是键名字符串: {value}"))
                .to_string()
        })
        .collect()
}

fn properties(node: &Value, pointer: &str) -> BTreeSet<String> {
    keys_of(
        node.pointer(pointer)
            .unwrap_or_else(|| panic!("契约缺少 {pointer} 列表: {node}")),
    )
}

/// 删掉一个键再看 serde 认不认：认不下才是真的必填，而不是文档里写着必填。
fn actually_required<T>(sample: &Value) -> BTreeSet<String>
where
    T: serde::de::DeserializeOwned,
{
    let mut result = BTreeSet::new();
    for key in keys_of(sample) {
        let mut payload = sample.clone();
        payload
            .as_object_mut()
            .expect("样例必须是对象")
            .remove(&key);
        if serde_json::from_value::<T>(payload).is_err() {
            result.insert(key);
        }
    }
    result
}

#[test]
fn the_input_schema_declares_exactly_the_fields_rust_writes_and_requires() {
    let schema = schema();
    let encoded = serde_json::to_value(full_input()).expect("契约输入必须可序列化");
    assert_eq!(
        keys_of(&encoded),
        properties(&schema, "/properties"),
        "schema 顶层的键全集与 Rust 写出的键不一致：多出来的键会让策略作者写出一份运行时收不下的载荷，\
         缺掉的键会让运行时交出契约没承诺过的格子"
    );
    assert_eq!(
        required_of(&schema),
        actually_required::<StrategyContractInput>(&encoded),
        "schema 的 required 与 Rust 的实际必填不一致：bars、market_bars、positions_exact 带 #[serde(default)]，\
         其余每一格都必须在载荷里出现（缺席与算出来是零是两件事）"
    );

    let bars = encoded["bars"]
        .as_object()
        .expect("样例必须带 bars 才能核对 bars 契约");
    let bars_schema = &schema["$defs"]["bars"];
    assert_eq!(
        keys_of(&Value::Object(bars.clone())),
        properties(bars_schema, "/properties"),
        "schema 的 bars 键全集与 Rust 写出的不一致：列名漂移会让策略侧按错列读行情"
    );
    assert_eq!(
        required_of(bars_schema),
        actually_required::<StrategyContractBars>(&Value::Object(bars.clone())),
        "schema 的 bars.required 与 Rust 的实际必填不一致"
    );
}

/// `bars`、`market_bars`、`positions_exact` 和 `available_margin_raw` 可缺省：策略可不声明列式历史，
/// 精确持仓字段缺省时仍兼容旧 raw 输入，可用保证金缺省表示运行时无法提供。
#[test]
fn bars_and_market_bars_are_optional_and_can_be_absent() {
    let schema = schema();
    let required = required_of(&schema);
    assert!(
        !required.contains("bars"),
        "bars 带 #[serde(default)]，不得出现在 required 里: {required:?}"
    );
    assert!(
        !required.contains("market_bars"),
        "market_bars 带 #[serde(default)]，不得出现在 required 里: {required:?}"
    );
    let mut payload = serde_json::to_value(full_input()).expect("序列化");
    payload
        .as_object_mut()
        .expect("样例必须是对象")
        .remove("bars");
    let parsed: StrategyContractInput =
        serde_json::from_value(payload).expect("不带 bars 的输入必须仍然读得进来");
    assert!(parsed.bars.is_none());
    assert!(parsed.market_bars.is_empty());
}

#[test]
fn market_bars_require_a_distinct_instrument_and_no_future_timestamp() {
    let mut input = full_input();
    input.market_bars.insert(
        "ETHUSDT.OKX".into(),
        StrategyContractBars {
            source: "secondary-snapshot".into(),
            ts: vec![1_700_000_000_000, 1_700_000_060_000],
            open_raw: vec![1, 2],
            high_raw: vec![2, 3],
            low_raw: vec![1, 2],
            close_raw: vec![2, 3],
            volume_raw: vec![4, 5],
            exact_ohlcv: None,
        },
    );
    input.validate().unwrap();

    let mut future = input.clone();
    future.market_bars.get_mut("ETHUSDT.OKX").unwrap().ts[1] = future.as_of + 1;
    assert!(future.validate().unwrap_err().contains("as_of 之后"));

    let mut duplicate = input;
    duplicate.market_bars.insert(
        duplicate.instrument.clone(),
        duplicate.bars.clone().unwrap(),
    );
    assert!(duplicate.validate().unwrap_err().contains("重复主行情"));
}

/// `additionalProperties: false` 不是装饰：输入方向拼错的键一旦被静默丢掉，策略看到的
/// 账户就是错的（少一格持仓），而策略作者以为自己在读它。
#[test]
fn unknown_keys_are_rejected_rather_than_dropped() {
    let schema = schema();
    assert_eq!(
        schema["additionalProperties"],
        json!(false),
        "顶层契约必须声明未知键不可接受，否则下面的 Rust 断言只是在替文档背书"
    );
    assert_eq!(
        schema["$defs"]["bars"]["additionalProperties"],
        json!(false),
        "bars 的未知键同样要声明不可接受（Rust 侧未加 deny_unknown_fields，schema 更严是有意的）"
    );
    let mut payload = serde_json::to_value(full_input()).expect("序列化");
    payload["risk_stat"] = json!("normal");
    let error = serde_json::from_value::<StrategyContractInput>(payload)
        .expect_err("契约之外的键必须被拒绝，而不是当成没写");
    assert!(
        error.to_string().contains("risk_stat"),
        "报错要点名是哪个未知键: {error}"
    );
}

/// 定点值的线上形态：只有 JSON 整数一种。
///
/// 输入方向的定点格散布在三张账（positions / cash / research_targets）与 bars 五列里，
/// 所以不能只查顶层那几格；`raw_ledger` 的 `additionalProperties` 与 `raw_series` 的
/// `items` 才是真正承载定点值的地方。
#[test]
fn fixed_point_fields_travel_as_json_integers_only() {
    let schema = schema();
    for pointer in [
        "/properties/available_margin_raw",
        "/$defs/raw_ledger/additionalProperties",
        "/$defs/raw_series/items",
    ] {
        let allowed = allowed_types(&schema, pointer);
        assert!(
            allowed.contains("integer"),
            "{pointer} 没允许整数形态，而那是 Rust 唯一读写定点值的形态: {allowed:?}"
        );
        assert!(
            !allowed.contains("string"),
            "{pointer} 放行字符串形态，等于放行一份 Rust 收不下的载荷"
        );
    }

    let big = 4_000_000_000_000_000_000_000i128;
    let numeric = replace_raw_literal(
        &canonical_payload(),
        "available_margin_raw",
        &big.to_string(),
    );
    let parsed: StrategyContractInput =
        serde_json::from_str(&numeric).expect("超出 i64 的 JSON 整数必须读得回来");
    assert_eq!(parsed.available_margin_raw, Some(big));

    let quoted = replace_raw_literal(
        &canonical_payload(),
        "available_margin_raw",
        &format!("\"{big}\""),
    );
    let error = serde_json::from_str::<StrategyContractInput>(quoted.as_str())
        .expect_err("字符串形态的定点值必须被拒绝");
    assert!(
        error.to_string().contains("invalid number"),
        "报错得指向这个值不是数: {error}"
    );
}

/// 契约里写的 `schema_version` 常量必须就是运行时认的那一版：它一旦与
/// `STRATEGY_CONTRACT_SCHEMA_VERSION` 分叉，读者按契约产出的载荷就会被运行时拒收。
#[test]
fn the_const_schema_version_is_the_version_the_runtime_accepts() {
    let schema = schema();
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        json!(STRATEGY_CONTRACT_SCHEMA_VERSION),
        "契约声明的策略 API 版本必须等于运行时接受的版本"
    );
}

#[test]
fn input_v8_schema_and_rust_contract_cover_exact_order_book_snapshots() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v8.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("读取 strategy-api-input-v8.json"),
    )
    .expect("strategy-api-input-v8.json 必须是合法 JSON");
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        json!(STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION)
    );
    assert!(required_of(&schema).contains("market_order_books"));
    let books_schema = &schema["properties"]["market_order_books"];
    assert_eq!(books_schema["minItems"], json!(1));
    assert_eq!(books_schema["maxItems"], json!(10_000));
    for side in ["bids", "asks"] {
        assert_eq!(
            books_schema["items"]["properties"][side]["minItems"],
            json!(1)
        );
        assert_eq!(
            books_schema["items"]["properties"][side]["maxItems"],
            json!(100)
        );
    }

    let exact_level = |price: &str, quantity: &str| StrategyContractOrderBookLevelV1 {
        price_raw: 0,
        qty_raw: 0,
        price_exact: Some(price.into()),
        qty_exact: Some(quantity.into()),
    };
    let mut input = full_input();
    input.schema_version = STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION;
    input
        .market_order_books
        .push(StrategyContractMarketOrderBookV1 {
            stream_id: "market:binance".into(),
            event_seq: 9,
            source_seq: 12,
            ts: input.as_of,
            instrument: input.instrument.clone(),
            sequence: 88,
            bids: vec![exact_level("0.0000000007", "0.0000000002")],
            asks: vec![exact_level("0.0000000009", "0.0000000004")],
        });
    assert_eq!(input.schema_version_for_features(), 8);
    input
        .validate()
        .expect("exact low-price order book is valid");

    let mut encoded = serde_json::to_value(&input).expect("serialize v8 input");
    // Rust omits feature-optional empty fields; include their wire-level empty forms
    // here so this v8 fixture also checks the complete schema property set.
    for (key, value) in [
        ("execution_updates", json!([])),
        ("fill_updates", json!([])),
        ("strategy_checkpoint", Value::Null),
        ("market_ticks", json!([])),
        ("account_updates", json!([])),
    ] {
        encoded[key] = value;
    }
    assert_eq!(
        keys_of(&encoded),
        properties(&schema, "/properties"),
        "v8 schema keys must match the Rust wire shape"
    );
    assert!(StrategyContractInput::from_json(&encoded.to_string()).is_ok());
}

#[test]
fn input_v9_schema_carries_exact_cash_and_margin_without_requiring_order_books() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy-api-input-v9.json");
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("读取 strategy-api-input-v9.json"),
    )
    .expect("strategy-api-input-v9.json 必须是合法 JSON");
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        json!(STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION)
    );
    assert!(schema["properties"]["cash_exact"].is_object());
    assert!(schema["properties"]["available_margin_exact"].is_object());
    assert!(!required_of(&schema).contains("market_order_books"));
    assert!(schema["anyOf"]
        .as_array()
        .is_some_and(|arms| arms.len() == 2));

    let mut input = full_input();
    input.cash_exact = BTreeMap::from([("USDT".into(), "1000.0000000007".into())]);
    input.available_margin_exact = Some("750.0000000009".into());
    input.schema_version = input.schema_version_for_features();
    assert_eq!(
        input.schema_version,
        STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION
    );
    input
        .validate()
        .expect("exact cash and margin must validate");

    let mut encoded = serde_json::to_value(&input).expect("serialize v9 input");
    // Optional historical feature fields may be omitted by Rust. Explicit empty forms also
    // remain valid v9 inputs and keep this schema key-set assertion tied to the Rust struct.
    for (key, value) in [
        ("execution_updates", json!([])),
        ("fill_updates", json!([])),
        ("strategy_checkpoint", Value::Null),
        ("market_ticks", json!([])),
        ("account_updates", json!([])),
        ("market_order_books", json!([])),
    ] {
        encoded[key] = value;
    }
    assert_eq!(
        keys_of(&encoded),
        properties(&schema, "/properties"),
        "v9 schema keys must match the Rust wire shape"
    );
    assert_eq!(encoded["cash_exact"]["USDT"], json!("1000.0000000007"));
    assert_eq!(encoded["available_margin_exact"], json!("750.0000000009"));
    assert!(StrategyContractInput::from_json(&encoded.to_string()).is_ok());

    let mut bad_cash = input.clone();
    bad_cash
        .cash_exact
        .insert("USDT".into(), "1000.000000001".into());
    assert!(bad_cash.validate().unwrap_err().contains("raw 投影冲突"));
    let mut bad_margin = input;
    bad_margin.available_margin_exact = Some("-1".into());
    assert!(bad_margin.validate().unwrap_err().contains("不能为负"));
}

/// Rust 自己写出的载荷文本——用它而不是手写 JSON，键全集与必填才会随结构体一起变。
fn canonical_payload() -> String {
    serde_json::to_string(&full_input()).expect("契约输入必须可序列化")
}

/// 只替换 `"key": <值>` 的值字面量，其余保持 Rust 的写法。
fn replace_raw_literal(payload: &str, key: &str, literal: &str) -> String {
    let marker = format!("\"{key}\":");
    let start = payload
        .find(&marker)
        .unwrap_or_else(|| panic!("Rust 写出的载荷里没有 {key}: {payload}"));
    let value_start = start + marker.len();
    let offset = payload[value_start..]
        .find([',', '}'])
        .unwrap_or_else(|| panic!("{key} 的值后面缺分隔符: {payload}"));
    let mut result = String::from(&payload[..value_start]);
    result.push_str(literal);
    result.push_str(&payload[value_start + offset..]);
    result
}

/// 把 `$ref` 展平一层，取这一格允许的类型名集合（`oneOf` 的每个分支都算）。
fn allowed_types(schema: &Value, pointer: &str) -> BTreeSet<&'static str> {
    let mut result = BTreeSet::new();
    let node = schema
        .pointer(pointer)
        .unwrap_or_else(|| panic!("契约缺少 {pointer}: {schema}"));
    collect_types(schema, node, &mut result, 0);
    result
}

fn collect_types<'a>(
    schema: &'a Value,
    node: &'a Value,
    out: &mut BTreeSet<&'static str>,
    depth: u8,
) {
    if depth > 4 {
        return;
    }
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        let target = reference
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("契约只接受文档内 $ref: {reference}"));
        let node = schema
            .pointer(target)
            .unwrap_or_else(|| panic!("契约 $ref 指不到东西: {reference}"));
        collect_types(schema, node, out, depth + 1);
        return;
    }
    match &node["type"] {
        Value::String(name) => {
            if let Some(name) = as_static_type(name) {
                out.insert(name);
            }
        }
        Value::Array(names) => {
            for name in names {
                if let Some(name) = name.as_str().and_then(as_static_type) {
                    out.insert(name);
                }
            }
        }
        _ => {}
    }
    if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
        for branch in branches {
            collect_types(schema, branch, out, depth + 1);
        }
    }
}

fn as_static_type(name: &str) -> Option<&'static str> {
    [
        "null", "boolean", "object", "array", "number", "string", "integer",
    ]
    .into_iter()
    .find(|candidate| *candidate == name)
}
