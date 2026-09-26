//! R4-4 · `schemas/strategy_api_v1.schema.json` 与活的跨语言策略契约是否同一份形状。
//!
//! 这份 schema 此前零读者，且与读侧矛盾：顶层漏了写侧恒印的 `instrument` 与 `target_qty`，
//! 意图层漏了多腿策略那三格政策，而 `additionalProperties: false` 让每一份真实载荷都过不了它。
//! 一份没人读、读一遍就会否掉全部合法输入的契约文件，比没有契约更糟——它会让人以为
//! Python/C++ 侧的形状与 Rust 不同。这里把判据接到**读侧本身**：键集与必填集合都拿
//! `StrategyContractOutput::to_json_for` 真正序列化出来的那份 JSON 比，不抄第二份清单。

use serde_json::{json, Value};
use std::collections::BTreeMap;

const SCHEMA_JSON: &str = include_str!("../../../schemas/strategy_api_v1.schema.json");

fn request() -> qx_runtime::StrategyContractInput {
    qx_runtime::StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
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
        bars: None,
    }
}

/// 每一格都填上值的输出：键集比对要覆盖到 serde 会印出去的全部字段。
fn fullest_output() -> qx_runtime::StrategyContractOutput {
    qx_runtime::StrategyContractOutput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-1".into(),
        strategy_id: "strategy-1".into(),
        signal_id: 7,
        instrument: "BTCUSDT.BINANCE".into(),
        target_qty: -3,
        confidence: 900,
        priority: 1,
        expires_at: 10,
        intents: vec![qx_runtime::StrategyContractIntent {
            intent_id: 1,
            instrument: "BTCUSDT.BINANCE".into(),
            side: "buy".into(),
            qty_raw: 20,
            limit_price_raw: Some(31_000),
            reduce_only: true,
            post_only: false,
            position_side: Some("long".into()),
            margin_mode: Some("cross".into()),
            position_mode: Some("hedge".into()),
            leverage: Some(5),
        }],
    }
}

fn schema() -> Value {
    serde_json::from_str(SCHEMA_JSON).expect("仓库里的策略契约必须是合法 JSON")
}

fn key_set(object: &Value) -> Vec<String> {
    let mut keys: Vec<String> = object
        .as_object()
        .expect("properties 必须是对象")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn string_set(array: &Value) -> Vec<String> {
    let mut keys: Vec<String> = array
        .as_array()
        .expect("required 必须是数组")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("required 逐项必须是字符串")
                .to_owned()
        })
        .collect();
    keys.sort();
    keys
}

fn payload_keys(payload: &Value) -> Vec<String> {
    let mut keys: Vec<String> = payload
        .as_object()
        .expect("序列化出的 JSON 必须是对象")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// 手抄清单比的是集合而不是次序：判据不该因为清单少排了一格就红一次。
fn sorted(names: &[&str]) -> Vec<String> {
    let mut keys: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    keys.sort();
    keys
}

#[test]
fn the_schema_declares_exactly_the_fields_the_writer_emits() {
    let schema = schema();
    let output = fullest_output();
    let encoded = output.to_json_for(&request()).unwrap();
    let payload: Value = serde_json::from_str(&encoded).unwrap();

    let declared = &schema["properties"];
    assert_eq!(
        key_set(declared),
        payload_keys(&payload),
        "schema 的顶层字段集与写侧序列化出的键集分叉了：新增一格字段而不改这份文件，\
         第三方策略作者照文件写的载荷会被写成漏项"
    );
    let declared_optional: Vec<String> = {
        let required = string_set(&schema["required"]);
        payload_keys(&payload)
            .into_iter()
            .filter(|key| !required.contains(key))
            .collect()
    };
    assert_eq!(
        declared_optional,
        sorted(&["intents"]),
        "读侧只给顶层 `intents` 打了 #[serde(default)]：可选集合一涨，就是有人加了字段却忘了在 schema 里标必填"
    );

    let intents = payload["intents"].as_array().unwrap();
    let intent_schema = &schema["properties"]["intents"]["items"];
    assert_eq!(
        key_set(&intent_schema["properties"]),
        payload_keys(&intents[0]),
        "schema 的意图字段集与写侧序列化出的键集分叉了"
    );
    let intent_optional: Vec<String> = {
        let required = string_set(&intent_schema["required"]);
        payload_keys(&intents[0])
            .into_iter()
            .filter(|key| !required.contains(key))
            .collect()
    };
    assert_eq!(
        intent_optional,
        sorted(&[
            "leverage",
            "limit_price_raw",
            "margin_mode",
            "post_only",
            "position_mode",
            "position_side",
            "reduce_only"
        ]),
        "读侧给意图哪些格打了 #[serde(default)]，schema 的必填集合就得让开哪些格"
    );
}

#[test]
fn every_required_key_is_something_the_reader_actually_cannot_default() {
    let schema = schema();
    let request = request();

    let mut top: Value =
        serde_json::from_str(&fullest_output().to_json_for(&request).unwrap()).unwrap();
    for key in string_set(&schema["required"]) {
        let mut probe = top.clone();
        assert!(
            probe.as_object_mut().unwrap().remove(&key).is_some(),
            "schema 点名了顶层 {key}，写侧却没印这一格"
        );
        assert!(
            qx_runtime::StrategyContractOutput::from_json_for(&probe.to_string(), &request)
                .is_err(),
            "schema 把顶层 {key} 标成必填，读侧却没有这一格也照样解得出来"
        );
    }
    // 反过来：唯一没打必填标记的顶层键必须真的能缺省。
    top.as_object_mut().unwrap().remove("intents");
    assert!(qx_runtime::StrategyContractOutput::from_json_for(&top.to_string(), &request).is_ok());

    let intents = schema["properties"]["intents"]["items"].clone();
    for key in string_set(&intents["required"]) {
        let mut probe: Value =
            serde_json::from_str(&fullest_output().to_json_for(&request).unwrap()).unwrap();
        probe["intents"][0]
            .as_object_mut()
            .unwrap()
            .remove(&key)
            .unwrap();
        assert!(
            qx_runtime::StrategyContractOutput::from_json_for(&probe.to_string(), &request)
                .is_err(),
            "schema 把意图 {key} 标成必填，读侧却没有这一格也照样解得出来"
        );
    }
    for key in [
        "limit_price_raw",
        "reduce_only",
        "post_only",
        "position_side",
        "margin_mode",
        "position_mode",
        "leverage",
    ] {
        let mut probe: Value =
            serde_json::from_str(&fullest_output().to_json_for(&request).unwrap()).unwrap();
        probe["intents"][0]
            .as_object_mut()
            .unwrap()
            .remove(key)
            .unwrap();
        assert!(
            qx_runtime::StrategyContractOutput::from_json_for(&probe.to_string(), &request).is_ok(),
            "读侧给意图 {key} 打了 #[serde(default)]，schema 却把它列成必填"
        );
    }
}

#[test]
fn the_schema_versions_the_contract_the_runtime_serves() {
    let schema = schema();
    assert_eq!(
        schema["properties"]["schema_version"]["const"],
        json!(qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION),
        "契约版本挪了而 schema 那格 const 还停在旧值：读侧会拒绝自己文件描述的载荷"
    );
}

/// 词表那一半：契约列出的每个词都必须是运行时真收得下的，运行时收不下的词契约里也不许有。
/// 顺带把"契约只写小写、读侧按 ASCII 大小写折叠认词"这件事写成期望值——它是这份文件故意比
/// 读侧严的一处，漂了就会变成第三方照契约写却被拒（或反之）的第二套真相。
#[test]
fn the_schema_words_are_the_ones_the_writer_emits_and_the_reader_accepts() {
    let schema = schema();
    let request = request();
    let declared_fields = &schema["properties"]["intents"]["items"]["properties"];
    let accepted = |field: &str, word: &str| -> bool {
        let mut probe: Value =
            serde_json::from_str(&fullest_output().to_json_for(&request).unwrap()).unwrap();
        probe["intents"][0][field] = json!(word);
        qx_runtime::StrategyContractOutput::from_json_for(&probe.to_string(), &request).is_ok()
    };
    for field in ["side", "position_side", "margin_mode", "position_mode"] {
        let words: Vec<String> = declared_fields[field]["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("契约里 {field} 没有 enum：那一格又退回自由字符串了"))
            .iter()
            .filter_map(|value| value.as_str())
            .map(str::to_owned)
            .collect();
        assert!(!words.is_empty(), "契约里 {field} 的 enum 一个词都没有");
        for word in &words {
            assert!(
                accepted(field, word),
                "契约声明的 {field}={word} 运行时不收：照这份文件写的载荷会被自家读侧拒掉"
            );
            assert!(
                accepted(field, &word.to_uppercase()),
                "读侧认词是 ASCII 大小写折叠的，{field}={word} 的大写形式却不收了：\
                 那行为变了，契约里那份小写清单就成了第二套真相"
            );
            assert!(
                !accepted(field, &format!("{word}x")),
                "运行时收下了契约没声明的 {field}={word}x：读侧词表涨了而契约没跟着涨"
            );
        }
    }
}
