//! `qx-protocol` 的 crate 内用例（V12 R2 从 `lib.rs` 末尾原样搬家，行为逐字相同）。
//!
//! 搬家理由与线格式子模块同一处：账户快照的行数棘轮只认"整文件下行"，而契约版本这类
//! 每条都要独立成用例的读侧纪律会持续长大，留在 crate 根等于让生产实现替测试付行数。
//! 作为 crate 根的子模块，这里依旧能碰到根内的私有项。
use super::*;

fn snapshot() -> AccountSnapshot {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    snapshot.cash_raw.insert("USDT".into(), 1000);
    snapshot.positions.insert(
        instrument.clone(),
        PositionSnapshot {
            instrument,
            quantity_raw: 2,
            ..PositionSnapshot::default()
        },
    );
    snapshot.seal();
    snapshot
}

#[test]
fn diff_is_deterministic_and_replayable() {
    let base = snapshot();
    let mut target = base.clone();
    target.cash_raw.insert("USDT".into(), 900);
    target.equity_raw = Some(900);
    target.header.snapshot_id = 2;
    target.header.as_of = 11;
    target.header.event_seq = 4;
    target.seal();
    let diff = base.diff(&target).unwrap();
    let rebuilt = diff.apply(&base).unwrap();
    assert_eq!(rebuilt, target);
}

#[test]
fn wrong_base_is_rejected() {
    let base = snapshot();
    let mut target = base.clone();
    target.cash_raw.insert("USDT".into(), 900);
    target.seal();
    let diff = base.diff(&target).unwrap();
    let mut wrong = base.clone();
    wrong.cash_raw.insert("USDT".into(), 800);
    wrong.seal();
    assert_eq!(diff.apply(&wrong), Err(ProtocolError::BaseStateMismatch));
}

#[test]
fn json_wire_format_is_stable_and_uses_raw_integers() {
    let snapshot = snapshot();
    let json = snapshot.to_json();
    assert!(json.starts_with("{\"protocol\":\"QIANXING_ACCOUNT\""));
    assert!(json.contains("\"cash_raw\":{\"USDT\":1000}"));
    assert!(json.contains("\"quantity_raw\":2"));
    assert!(ACCOUNT_SNAPSHOT_JSON_SCHEMA.contains("QIANXING_ACCOUNT"));
    let wire = snapshot.to_wire_json().unwrap();
    assert_eq!(AccountSnapshot::from_wire_json(&wire).unwrap(), snapshot);
    let qifi = snapshot.to_qifi();
    let qifi_json = qifi.to_json();
    assert!(qifi_json.contains("\"protocol\":\"QIFI\""));
    assert_eq!(QifiEnvelope::from_json(&qifi_json).unwrap(), qifi);
}

/// 订单/成交/划转三张表按 `u64` 建键，而 `to_json` 是手写的稳定编码器：键若原样拼进
/// `{}`，产出的就不是合法 JSON，读回路径（`FileSnapshotStore` / SQLite / 平面路由的正文）
/// 会在这份快照一有订单时当场失败。仓库里原有的往返用例只放了 `cash_raw` 与 `positions`，
/// 所以这三张表一直是"从未被自己的读法读过"。
///
/// 编码统一走 serde 之后（V11 R14/R15），键带引号、枚举印变体名由构造保证，这条用例钉的
/// 是"这三张表确实被自己的读法读过"，以及认不出的枚举拼写一律拒而不是回落成默认档位。
#[test]
fn rows_keyed_by_integer_still_produce_and_recover_valid_json() {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    snapshot.cash_raw.insert("USDT".into(), 1000);
    snapshot.orders.insert(
        7,
        OrderSnapshot {
            order_id: 7,
            client_order_id: 7,
            instrument: instrument.clone(),
            side: Side::Buy,
            quantity_raw: 2,
            filled_raw: 0,
            status: OrderStatus::Working,
        },
    );
    snapshot.fills.insert(
        9,
        FillSnapshot {
            fill_id: 9,
            order_id: 7,
            quantity_raw: 2,
            price_raw: 100,
            fee_raw: 1,
            ts: 10,
        },
    );
    snapshot.transfers.insert(
        11,
        TransferSnapshot {
            transfer_id: 11,
            currency: "USDT".into(),
            amount_raw: 500,
            ts: 10,
        },
    );
    snapshot.seal();
    let text = snapshot.to_json();
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("带行的快照线格式必须是合法 JSON: {error}\n{text}"));
    for (table, key) in [("orders", "7"), ("fills", "9"), ("transfers", "11")] {
        assert!(
            parsed[table].get(key).is_some(),
            "{table} 必须按 id 建键，实际正文: {text}"
        );
    }
    assert_eq!(
        AccountSnapshot::from_json(&text).unwrap(),
        snapshot,
        "写出去读不回来，等于这三张表没有可用的持久化路径"
    );
    // 读侧对认不出的枚举一律拒：数字码（上一版编码留下的形状）与拼错的变体名都不能
    // 回落成默认档位，否则"没方向"会被读成"买"。
    for (field, tampered_value) in [
        ("side", serde_json::Value::from(9_u64)),
        ("side", serde_json::Value::from("NotASide")),
        ("status", serde_json::Value::from(99_u64)),
        ("status", serde_json::Value::from("NotAStatus")),
    ] {
        let mut tampered = parsed.clone();
        tampered["orders"]["7"][field] = tampered_value;
        let tampered = serde_json::to_string(&tampered).expect("篡改正文必须可序列化");
        let error = AccountSnapshot::from_json(&tampered)
            .expect_err("订单的未知枚举必须被拒，而不是回落成默认档位");
        assert!(
            matches!(
                error,
                ProtocolError::Serialization(_) | ProtocolError::Invalid(_)
            ),
            "{field} 的未知值必须走错误通道，实际: {error:?}"
        );
    }
    let mut revived = parsed;
    revived["orders"]["7"]["side"] = serde_json::Value::from("Buy");
    revived["orders"]["7"]["status"] = serde_json::Value::from("Working");
    assert_eq!(
        AccountSnapshot::from_json(&serde_json::to_string(&revived).unwrap()).unwrap(),
        snapshot,
        "变体名就是这一版的合法写法，改回去必须读得回来"
    );
}

#[test]
fn stable_json_and_file_snapshot_store_are_recoverable_and_idempotent() {
    let snapshot = snapshot();
    let root = std::env::temp_dir().join(format!(
        "qianxing-protocol-{}-{}",
        std::process::id(),
        snapshot.header.snapshot_id
    ));
    let store = FileSnapshotStore::new(&root);
    let first = store.save(&snapshot).unwrap();
    let second = store.save(&snapshot).unwrap();
    assert_eq!(first, second);
    let json = store
        .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
        .unwrap();
    assert_eq!(AccountSnapshot::from_json(&json).unwrap(), snapshot);
}

/// 契约版本要"会失败"：Rust 侧过去只看顶层与 header 是否自洽，所以一份两边都写 7 的快照
/// 能被接受，而 Python 桥按 `!= 1` 拒收。现在自洽的高版本号同样进不来。
#[test]
fn self_consistent_unknown_schema_version_is_rejected_by_every_entry() {
    let mut value: serde_json::Value =
        serde_json::from_str(&snapshot().to_json()).expect("稳定 JSON 必须可解析");
    value["schema_version"] = serde_json::Value::from(7u64);
    value["header"]["schema_version"] = serde_json::Value::from(7u64);
    let future = serde_json::to_string(&value).unwrap();

    let error = AccountSnapshot::from_json(&future).expect_err("自洽的 schema_version=7 必须被拒");
    assert!(
        matches!(error, ProtocolError::Invalid(ref message)
            if message.contains("schema_version=7") && message.contains("只认 1")),
        "拒绝理由必须点出实到版本与本构建认的版本，实际: {error:?}"
    );

    // 版本号也是哈希的一部分：不能靠"改掉版本就换一份哈希"绕开校验。
    let mut header_only = serde_json::from_str::<serde_json::Value>(&snapshot().to_json()).unwrap();
    header_only["header"]["schema_version"] = serde_json::Value::from(7u64);
    assert!(matches!(
        AccountSnapshot::from_json(&serde_json::to_string(&header_only).unwrap()),
        Err(ProtocolError::Invalid(message)) if message.contains("header.schema_version")
    ));

    // wire 入口同样在闸门之后才谈自洽：一份 v7 的文档在"自己那一版口径"下是自洽的
    // （`validate()` 必须放过它），拒它的只能是版本闸门，不是哈希或字段体检。
    let mut foreign = snapshot();
    foreign.header.schema_version = 7;
    foreign.seal();
    foreign
        .validate()
        .expect("自洽体检不管版本：否则下一条断言的拒绝理由就分不清是谁给的");
    assert_eq!(
        AccountSnapshot::from_wire_json(&foreign.to_wire_json().unwrap()).err(),
        Some(ProtocolError::Invalid(
            "账户快照 schema_version=7 不受支持（本构建只认 1）".into()
        )),
        "wire 入口与稳定 JSON 入口必须共用同一道版本闸门"
    );
}

/// 顶层版本号是唯一真值，因此 header 里**声明了却读不出整数**的那一份只能被拒，
/// 不能被"看它不像数字就当它没写"的补写路径洗成合法值。
#[test]
fn header_version_that_is_not_the_declared_integer_is_rejected() {
    for declared in [
        serde_json::json!("1"),
        serde_json::json!(true),
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.0),
    ] {
        let mut value: serde_json::Value =
            serde_json::from_str(&snapshot().to_json()).expect("稳定 JSON 必须可解析");
        value["header"]["schema_version"] = declared.clone();
        let error = AccountSnapshot::from_json(&serde_json::to_string(&value).unwrap())
            .expect_err("读不出与顶层相同整数的版本号必须被拒");
        assert!(
            matches!(error, ProtocolError::Invalid(ref message)
                if message.contains("header.schema_version") && message.contains(&declared.to_string())),
            "拒绝理由必须带回实到的那一份声明，实际: {error:?}"
        );
    }
}

/// 反向一半：header 没抄第二份版本号是**正常输入**（契约的 header required 列表里没有它，
/// `FileSnapshotStore` 落盘的稳定 JSON 也从不写它），拒绝缺席会让自家存储读不回来。
/// 这条用例是 V12 §6 R2 原文"缺 header 时就拒"被实测否证后留下的边界。
#[test]
fn header_without_version_is_the_stored_shape_and_still_round_trips() {
    let stored: serde_json::Value =
        serde_json::from_str(&snapshot().to_json()).expect("稳定 JSON 必须可解析");
    assert!(
        stored["header"].get("schema_version").is_none(),
        "夹具依赖的事实：自家写侧本来就不在 header 里抄第二份版本号"
    );
    let parsed = AccountSnapshot::from_json(&snapshot().to_json())
        .expect("缺席的 header 版本号按顶层归一，否则自家存储读不回来");
    assert_eq!(parsed, snapshot());
    assert_eq!(
        parsed.header.schema_version,
        ACCOUNT_SNAPSHOT_SCHEMA_VERSION
    );

    let mut value = stored;
    value["schema_version"] = serde_json::Value::from(7u64);
    assert!(
        matches!(
            AccountSnapshot::from_json(&serde_json::to_string(&value).unwrap()),
            Err(ProtocolError::Invalid(ref message)) if message.contains("schema_version=7")
        ),
        "写侧不抄第二份不等于读侧放行任意版本：顶层那一份仍然只认常量"
    );
}

/// 顶层常量闸门不是 `validate()` 的重复品：header 里的版本号由顶层归一（缺席就写顶层那一份，
/// 声明了就要求与顶层一致），所以任何能走到 `validate()` 的文档必然过得了顶层。只有当一份
/// 未来版本文档**本构建根本读不出来**时，两根闸门的分工才显出来：版本闸门必须先把它叫住，
/// 否则使用者拿到的是一条"字段缺失"的反序列化错误，看不出这其实是一份新版契约。
#[test]
fn future_version_that_cannot_be_deserialized_is_still_refused_as_a_version_problem() {
    let mut value: serde_json::Value =
        serde_json::from_str(&snapshot().to_json()).expect("稳定 JSON 必须可解析");
    value["schema_version"] = serde_json::Value::from(7u64);
    value["header"]["schema_version"] = serde_json::Value::from(7u64);
    value
        .as_object_mut()
        .expect("夹具是 JSON object")
        .remove("cash_raw");
    let error = AccountSnapshot::from_json(&serde_json::to_string(&value).unwrap())
        .expect_err("自洽的高版本号即使形状读不出来也必须被拒");
    assert!(
        matches!(error, ProtocolError::Invalid(ref message)
            if message.contains("schema_version=7") && message.contains("只认 1")),
        "版本判定必须排在字段解析之前，实际: {error:?}"
    );
}

/// 常量与 `/schema/account-snapshot-v1` 公布的契约必须同号，
/// 否则"只接受契约声明的 const"这句话本身就是一份假话。
///
/// 这里原本还比一次"内嵌文本 vs 仓库契约文件"：那份内嵌文本是手写的第二份 schema，
/// 两边对 `positions/orders/fills/transfers` 与顶层 `additionalProperties` 的说法并不一致，
/// 而 `deploy/README.md` 宣称两者同源。现在 const 直接 `include_str!` 仓库那一份，
/// 比较只剩自我相等，判据已由门禁改钉"服务端契约必须是 include_str 的那一句"。
#[test]
fn known_version_constant_matches_the_served_contract() {
    let contract: serde_json::Value =
        serde_json::from_str(ACCOUNT_SNAPSHOT_JSON_SCHEMA).expect("内嵌契约必须是合法 JSON");
    assert_eq!(
        contract["properties"]["schema_version"]["const"],
        serde_json::Value::from(ACCOUNT_SNAPSHOT_SCHEMA_VERSION),
        "服务端契约的 const 与 Rust 常量不一致时，版本校验各说一套"
    );
    // 常量写 1、契约写 1 而读者按仓库那份严格 schema 校验：$id 与 title 说错了会让
    // 外部读者拿错契约，所以公布身份也要落在文本里。
    assert_eq!(
        contract["$id"].as_str(),
        Some("https://qianxing.dev/schema/account-snapshot-v1.json"),
        "公布的 $id 必须指向仓库里那份契约，读者才知道自己校验的是哪一份"
    );
}
