//! `qx-protocol` 的 crate 内用例（V12 R2 从 `lib.rs` 末尾原样搬家，行为逐字相同）。
//!
//! 账户快照契约按读侧不变量拆分用例，便于逐项验证版本、字段与兼容行为。
//! 作为 crate 根的子模块，这里依旧能碰到根内的私有项。
use super::*;

fn snapshot() -> AccountSnapshot {
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    snapshot.cash_raw.insert("USDT".into(), 1000);
    snapshot.positions.insert(
        instrument.clone(),
        PositionSnapshot {
            instrument: instrument.clone(),
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
    let json = snapshot.to_json().expect("protocol JSON serializes");
    assert!(json.starts_with("{\"protocol\":\"QIANXING_ACCOUNT\""));
    assert!(json.contains("\"cash_raw\":{\"USDT\":1000}"));
    assert!(json.contains("\"quantity_raw\":2"));
    assert!(ACCOUNT_SNAPSHOT_JSON_SCHEMA.contains("QIANXING_ACCOUNT"));
    let wire = snapshot.to_wire_json().unwrap();
    assert_eq!(AccountSnapshot::from_wire_json(&wire).unwrap(), snapshot);
    let qifi = snapshot.to_qifi();
    let qifi_json = qifi.to_json().expect("protocol JSON serializes");
    assert!(qifi_json.contains("\"protocol\":\"QIFI\""));
    assert_eq!(QifiEnvelope::from_json(&qifi_json).unwrap(), qifi);
}

#[test]
fn precise_balance_and_position_amounts_survive_snapshot_and_diff() {
    let instrument = InstrumentId::parse("TINY/USDT:USDT.BINANCE").unwrap();
    let mut base = AccountSnapshot::new(1, "main", "portfolio", "BINANCE", 10);
    let balance = AccountBalance {
        asset: "USDT".into(),
        free: Money::from_raw(0),
        locked: Money::from_raw(0),
        borrowed: Money::from_raw(0),
        net_cash_balance: None,
        exact_amounts: Some(qx_core::ExactAccountBalanceAmounts {
            free: qx_core::ExactDecimal::parse("0.0000000003").unwrap(),
            locked: qx_core::ExactDecimal::ZERO,
            borrowed: qx_core::ExactDecimal::ZERO,
            net_cash_balance: None,
        }),
    };
    base.upsert_balance(&balance).unwrap();
    assert_eq!(base.cash_raw["USDT"], 0);
    assert_eq!(base.cash_exact["USDT"].to_string(), "0.0000000003");
    base.positions.insert(
        instrument.clone(),
        PositionSnapshot {
            instrument: instrument.clone(),
            quantity_raw: 0,
            exact_amounts: Some(qx_core::ExactAccountPositionAmounts {
                quantity: qx_core::ExactDecimal::parse("0.0000000002").unwrap(),
                average_price: None,
                mark_price: None,
                liquidation_price: None,
                unrealized_pnl: Some(qx_core::ExactDecimal::parse("0.0000000007").unwrap()),
                initial_margin: None,
                maintenance_margin: None,
            }),
            ..PositionSnapshot::default()
        },
    );
    base.seal();

    let encoded = base.to_json().unwrap();
    let decoded = AccountSnapshot::from_json(&encoded).unwrap();
    assert_eq!(decoded, base);

    let mut target = base.clone();
    target.cash_exact.insert(
        "USDT".into(),
        qx_core::ExactDecimal::parse("0.0000000004").unwrap(),
    );
    target
        .positions
        .get_mut(&instrument)
        .unwrap()
        .exact_amounts
        .as_mut()
        .unwrap()
        .unrealized_pnl = Some(qx_core::ExactDecimal::parse("0.0000000008").unwrap());
    target.seal();
    let diff = base.diff(&target).unwrap();
    assert_eq!(diff.apply(&base).unwrap(), target);
}

#[test]
fn v3_hash_commits_settlement_currency_and_v1_rejects_new_fields() {
    let mut without_currency = snapshot();
    let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let before = without_currency.state_hash();
    without_currency
        .positions
        .get_mut(&instrument)
        .unwrap()
        .settlement_currency = Some("USDT".into());
    without_currency.seal();
    assert_ne!(before, without_currency.state_hash());
    assert_eq!(
        AccountSnapshot::from_json(&without_currency.to_json().unwrap()).unwrap(),
        without_currency
    );

    let mut legacy = snapshot();
    legacy.header.schema_version = 1;
    legacy.seal();
    let mut incompatible = legacy;
    incompatible
        .positions
        .get_mut(&instrument)
        .unwrap()
        .settlement_currency = Some("USDT".into());
    incompatible.seal();
    assert!(matches!(
        incompatible.validate(),
        Err(ProtocolError::Invalid(message)) if message.contains("v1")
    ));
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
            exact_amounts: None,
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
            fee_currency: None,
            fees: vec![],
            exact_amounts: None,
            exact_fees: vec![],
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
    let text = snapshot.to_json().expect("protocol JSON serializes");
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
fn exact_order_and_fill_amounts_round_trip_through_snapshot_and_state_hash() {
    let mut snapshot = AccountSnapshot::new(9, "main", "default", "OKX", 10);
    let instrument = InstrumentId::parse("PEPE/USDT.OKX").unwrap();
    snapshot.orders.insert(
        21,
        OrderSnapshot {
            order_id: 21,
            client_order_id: 21,
            instrument,
            side: Side::Buy,
            quantity_raw: 0,
            filled_raw: 0,
            status: OrderStatus::PartiallyFilled,
            exact_amounts: Some(qx_core::ExactOrderAmounts {
                quantity: qx_core::ExactDecimal::parse("1000.0000000001").unwrap(),
                limit_price: Some(qx_core::ExactDecimal::parse("0.00000001").unwrap()),
                filled: qx_core::ExactDecimal::parse("0.0000000001").unwrap(),
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        },
    );
    snapshot.fills.insert(
        22,
        FillSnapshot {
            fill_id: 22,
            order_id: 21,
            quantity_raw: 0,
            price_raw: 0,
            fee_raw: 0,
            fee_currency: None,
            fees: Vec::new(),
            exact_amounts: Some(qx_core::ExactFillAmounts {
                quantity: qx_core::ExactDecimal::parse("0.0000000001").unwrap(),
                price: qx_core::ExactDecimal::parse("0.00000001").unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            exact_fees: vec![qx_core::ExactFillFee {
                amount: qx_core::ExactDecimal::parse("0.000000000000000002").unwrap(),
                currency: Some("USDT".into()),
                source_trade_ids: vec!["trade-22".into()],
            }],
            ts: 11,
        },
    );
    snapshot.seal();
    let hash = snapshot.state_hash();
    let json = snapshot.to_json().unwrap();
    let restored = AccountSnapshot::from_json(&json).unwrap();
    assert_eq!(restored, snapshot);
    assert_eq!(restored.state_hash(), hash);

    let mut changed = snapshot.clone();
    changed
        .orders
        .get_mut(&21)
        .unwrap()
        .exact_amounts
        .as_mut()
        .unwrap()
        .quantity = qx_core::ExactDecimal::parse("1000.0000000002").unwrap();
    assert_ne!(changed.state_hash(), hash);

    let mut changed_fee = snapshot.clone();
    changed_fee.fills.get_mut(&22).unwrap().exact_fees[0].amount =
        qx_core::ExactDecimal::parse("0.000000000000000003").unwrap();
    assert_ne!(
        changed_fee.state_hash(),
        hash,
        "exact fee changes must alter snapshot identity"
    );

    let mut legacy = snapshot;
    legacy.header.schema_version = ACCOUNT_SNAPSHOT_V1_SCHEMA_VERSION;
    legacy.header.state_hash = 0;
    assert!(
        matches!(legacy.validate(), Err(ProtocolError::Invalid(error)) if error.contains("精确订单、成交金额或手续费"))
    );
}

#[test]
fn stable_json_and_file_snapshot_store_are_recoverable_and_idempotent() {
    let snapshot = snapshot();
    // Keep filesystem scratch beneath the workspace. Some Windows hosts deny direct writes
    // under the configured system Temp directory while allowing repository-local test output.
    let root = std::env::current_dir()
        .expect("workspace current directory")
        .join("target")
        .join("qx-protocol-test-tmp")
        .join(format!(
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
    std::fs::remove_dir_all(root).expect("remove workspace-local snapshot test data");
}

/// 契约版本要"会失败"：读侧接受声明过的 v1/v2，并明确拒绝超出范围的自洽版本。
#[test]
fn self_consistent_unknown_schema_version_is_rejected_by_every_entry() {
    let mut value: serde_json::Value =
        serde_json::from_str(&snapshot().to_json().expect("protocol JSON serializes"))
            .expect("稳定 JSON 必须可解析");
    value["schema_version"] = serde_json::Value::from(7u64);
    value["header"]["schema_version"] = serde_json::Value::from(7u64);
    let future = serde_json::to_string(&value).unwrap();

    let error = AccountSnapshot::from_json(&future).expect_err("自洽的 schema_version=7 必须被拒");
    assert!(
        matches!(error, ProtocolError::Invalid(ref message)
            if message.contains("schema_version=7") && message.contains("支持 1..=3")),
        "拒绝理由必须点出实到版本与本构建认的版本，实际: {error:?}"
    );

    // 版本号也是哈希的一部分：不能靠"改掉版本就换一份哈希"绕开校验。
    let mut header_only = serde_json::from_str::<serde_json::Value>(
        &snapshot().to_json().expect("protocol JSON serializes"),
    )
    .unwrap();
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
            "账户快照 schema_version=7 不受支持（本构建支持 1..=3）".into()
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
            serde_json::from_str(&snapshot().to_json().expect("protocol JSON serializes"))
                .expect("稳定 JSON 必须可解析");
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
        serde_json::from_str(&snapshot().to_json().expect("protocol JSON serializes"))
            .expect("稳定 JSON 必须可解析");
    assert!(
        stored["header"].get("schema_version").is_none(),
        "夹具依赖的事实：自家写侧本来就不在 header 里抄第二份版本号"
    );
    let parsed =
        AccountSnapshot::from_json(&snapshot().to_json().expect("protocol JSON serializes"))
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
        serde_json::from_str(&snapshot().to_json().expect("protocol JSON serializes"))
            .expect("稳定 JSON 必须可解析");
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
            if message.contains("schema_version=7") && message.contains("支持 1..=3")),
        "版本判定必须排在字段解析之前，实际: {error:?}"
    );
}

/// 当前常量与 `/schema/account-snapshot-v3` 公布的契约必须同号，
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
        Some("https://qianxing.dev/schema/account-snapshot-v3.json"),
        "公布的 $id 必须指向当前契约，读者才知道自己校验的是哪一份"
    );
    let legacy: serde_json::Value =
        serde_json::from_str(ACCOUNT_SNAPSHOT_JSON_SCHEMA_V1).expect("v1 契约必须是合法 JSON");
    assert_eq!(legacy["properties"]["schema_version"]["const"], 1);
}
