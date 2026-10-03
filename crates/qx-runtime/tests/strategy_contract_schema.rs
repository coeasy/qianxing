//! 发布契约的第三道钉子：`schemas/strategy_api_v1.schema.json` 必须与 Rust 结构体
//! 逐键同形（V12 R4-k）。
//!
//! 这份 schema 此前**零消费者**，所以它两头都错：Rust 用 serde 强制要求的
//! `instrument/target_qty/confidence/priority/expires_at` 不在它的 `required` 里（缺这些
//! 键的载荷能过 schema、过不了运行时），而 Rust 自己会写出的
//! `margin_mode/position_mode/leverage` 又被它的 `additionalProperties: false` 拒掉。
//! 这里不引第三方 JSON Schema 校验器，只钉住可机械核对的三件事：键全集、必填集、
//! 未知键拒绝——上面两类错形正好各由一件负责。

use std::collections::BTreeSet;

use qx_runtime::{
    StrategyContractIntent, StrategyContractOutput, STRATEGY_CONTRACT_SCHEMA_VERSION,
};
use serde_json::{json, Value};

fn schema() -> Value {
    // 仓库根的 `schemas/`：manifest 目录往上是 crates/qx-runtime 两级。
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("strategy_api_v1.schema.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("读取 schemas/strategy_api_v1.schema.json 失败 {path:?}: {error}")
    });
    serde_json::from_str(&text).expect("strategy_api_v1.schema.json 必须是合法 JSON")
}

/// 每一项都填满：可选键的空缺会让"schema 少写了一个键"看不出来。
fn full_output() -> StrategyContractOutput {
    StrategyContractOutput {
        schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "req-1".into(),
        strategy_id: "strategy-1".into(),
        signal_id: 7,
        instrument: "BTCUSDT.BINANCE".into(),
        target_qty: -3_000,
        confidence: 12 * 1_000_000_000,
        priority: 2,
        expires_at: 1_800_000_000_000,
        intents: vec![StrategyContractIntent {
            intent_id: 1,
            instrument: "BTCUSDT.BINANCE".into(),
            side: "buy".into(),
            qty_raw: 100 * 1_000_000_000,
            limit_price_raw: Some(65_000 * 1_000_000),
            reduce_only: true,
            post_only: true,
            position_side: Some("long".into()),
            margin_mode: Some("cross".into()),
            position_mode: Some("hedge".into()),
            leverage: Some(3),
        }],
    }
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
fn the_schema_declares_exactly_the_fields_rust_writes_and_requires() {
    let schema = schema();
    let encoded = serde_json::to_value(full_output()).expect("契约输出必须可序列化");
    assert_eq!(
        keys_of(&encoded),
        properties(&schema, "/properties"),
        "schema 顶层的键全集与 Rust 写出的键不一致：多出来的键会让读者找不到生产方，\
         缺掉的键会让产物里出现契约没承诺过的东西"
    );
    assert_eq!(
        required_of(&schema),
        actually_required::<StrategyContractOutput>(&encoded),
        "schema 的 required 与 Rust 的实际必填不一致：它承诺可以缺省的键其实不能缺，\
         或反过来放行一个会被运行时拒掉的载荷"
    );

    let intent = encoded["intents"]
        .get(0)
        .expect("样例必须带一条 intent 才能核对 intent 契约");
    let intent_schema = &schema["$defs"]["intent"];
    assert_eq!(
        keys_of(intent),
        properties(intent_schema, "/properties"),
        "schema 的 intent 键全集与 Rust 写出的不一致：per-leg 档位字段曾在两边各写各的"
    );
    assert_eq!(
        required_of(intent_schema),
        actually_required::<StrategyContractIntent>(intent),
        "schema 的 intent.required 与 Rust 的实际必填不一致"
    );
}

fn properties(node: &Value, pointer: &str) -> BTreeSet<String> {
    keys_of(
        node.pointer(pointer)
            .unwrap_or_else(|| panic!("契约缺少 {pointer} 列表: {node}")),
    )
}

/// `additionalProperties: false` 不是装饰：拼错的可选键一旦被静默丢掉，那一腿就按
/// 运行时默认档位成交，而策略作者以为自己在它上面开了 post-only 或对冲。
#[test]
fn unknown_keys_are_rejected_rather_than_dropped() {
    let schema = schema();
    assert_eq!(
        schema["additionalProperties"],
        json!(false),
        "顶层契约必须声明未知键不可接受，否则下面的 Rust 断言只是在替文档背书"
    );
    assert_eq!(
        schema["$defs"]["intent"]["additionalProperties"],
        json!(false),
        "intent 的未知键同样要声明不可接受"
    );
    let mut payload = serde_json::to_value(full_output()).expect("序列化");
    payload["intents"][0]["post_onli"] = json!(true);
    let error = serde_json::from_value::<StrategyContractOutput>(payload)
        .expect_err("契约之外的键必须被拒绝，而不是当成没写");
    assert!(
        error.to_string().contains("post_onli"),
        "报错要点名是哪个未知键: {error}"
    );
}

/// 定点值的线上形态：只有 JSON 整数一种。
///
/// 这里曾按"账户快照产物"的口径以为 Rust 也吃十进制字符串；实测这条 JSONL 通道上
/// `"target_qty": "4000000000000000000000"` 被判 invalid number，而 Rust 自己写出去
/// 的就是整数。契约若放行字符串，等于放行一份运行时必收的载荷——所以两边都要钉住：
/// schema 不许有 string 形态，超出 i64 的整数则必须真的读得回来（否则"integer"是在
/// 偷偷只承诺 i64 量级）。
#[test]
fn fixed_point_fields_travel_as_json_integers_only() {
    let schema = schema();
    for pointer in [
        "/properties/target_qty",
        "/properties/confidence",
        "/$defs/intent/properties/qty_raw",
        "/$defs/intent/properties/limit_price_raw",
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
    let numeric = replace_raw_literal(&canonical_payload(), "target_qty", &big.to_string());
    let parsed: StrategyContractOutput =
        serde_json::from_str(&numeric).expect("超出 i64 的 JSON 整数必须读得回来");
    assert_eq!(parsed.target_qty, big);
    let quoted = replace_raw_literal(&canonical_payload(), "target_qty", &format!("\"{big}\""));
    let error = serde_json::from_str::<StrategyContractOutput>(quoted.as_str())
        .expect_err("字符串形态的定点值必须被拒绝");
    assert!(
        error.to_string().contains("invalid number"),
        "报错得指向这个值不是数: {error}"
    );
}

/// Rust 自己写出的载荷文本——用它而不是手写 JSON，键全集与必填才会随结构体一起变。
fn canonical_payload() -> String {
    serde_json::to_string(&full_output()).expect("契约输出必须可序列化")
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
///
/// `pointer` 走 JSON Pointer 而不是下标：serde_json 的 `v["a/b"]` 只按字面键名查，
/// 拿路径下标会静默得到 `Null`，那时这里就变成"断言永远成立"。
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
