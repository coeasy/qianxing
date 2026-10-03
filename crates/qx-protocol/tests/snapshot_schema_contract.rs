//! S10 收口 · `GET /schema/account-snapshot-v1` 发出去的那份契约，与写侧/读侧是否一致。
//!
//! 这份 schema 此前有两处不诚实：仓库里的 `schemas/account-snapshot-v1.json` 与服务常量
//! 是两份手抄（谁都不是权威），而两份都漏掉了写侧真实印出的八个钱字段。收口方式是在
//! 编译期只留一份文本（常量 `include_str!` 那个文件），再把"契约说的"与"两侧做的"逐个对齐：
//! 声明的键集合 = 写侧产物的键集合、未算的钱仍然是 `null`、读侧按 `const` 的版本号收口。

use qx_protocol::{AccountSnapshot, ACCOUNT_SNAPSHOT_JSON_SCHEMA, ACCOUNT_SNAPSHOT_SCHEMA_VERSION};
use serde_json::Value;

/// 写侧真实产出的那一份快照（V11 R18 的地面真值夹具，由 `to_json` 原样产出）。
const WRITER_SAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../python/tests/fixtures/account-snapshot-v1.sample.json"
);

fn schema() -> Value {
    serde_json::from_str(ACCOUNT_SNAPSHOT_JSON_SCHEMA)
        .expect("服务出去的 schema 必须是合法 JSON——它现在只有一份文本，出错就是那份文件坏了")
}

fn writer_sample() -> Value {
    let text = std::fs::read_to_string(WRITER_SAMPLE)
        .unwrap_or_else(|error| panic!("写侧夹具必须存在: {error}"));
    serde_json::from_str(&text).expect("夹具必须是合法 JSON")
}

fn keys_of(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("应为 JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// 取读侧拒绝的理由文本：`ProtocolError` 没有 Display，且不预期的变体（例如解析错误）
/// 在这里与"根本没拒绝"是同一种失败——判据换掉了就该红，而不是被换个错误码混过去。
fn reject_reason<T>(result: Result<T, qx_protocol::ProtocolError>, must_reject: &str) -> String {
    match result {
        Err(qx_protocol::ProtocolError::Invalid(message)) => message,
        Err(error) => panic!("{must_reject}：必须按 Invalid 拒绝，实际 {error:?}"),
        Ok(_) => panic!("{must_reject}：读侧把它收了下来"),
    }
}

/// 契约里的一个字符串数组声明（`required` 那一类）。
fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("应为数组")
        .iter()
        .map(|item| item.as_str().expect("数组项应为字符串").to_string())
        .collect()
}

/// 只留一份文本：常量按字节包含仓库里那份 schema 文件。此前两边各存一份手抄，
/// 比对时没有谁是权威，于是"文件多 `title`、双方都少八个钱字段"能同时成立。
#[test]
fn served_schema_is_the_repository_file_verbatim() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/account-snapshot-v1.json"
    );
    let on_disk = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("schema 文件必须存在: {error}"));
    // 两侧工作副本的换行由 `core.autocrlf` 决定，比对规范化后的内容而不是磁盘字节。
    assert_eq!(
        ACCOUNT_SNAPSHOT_JSON_SCHEMA.replace("\r\n", "\n"),
        on_disk.replace("\r\n", "\n"),
        "服务常量与 schemas/ 下的文件必须是同一份文本"
    );
}

/// schema 声明的顶层键必须正好盖住写侧印出的那些：少一个键，客户端按契约校验自家解出来
/// 的快照会失败；多一个键，就是把不存在的字段说成契约。`required` 另钉一侧——它只能是
/// 写侧永远印出来的键，否则"必填"本身就是一句做不到的承诺。
#[test]
fn served_schema_declares_every_key_the_writer_emits() {
    let contract = schema();
    let declared = keys_of(&contract["properties"]);
    let emitted = keys_of(&writer_sample());
    assert_eq!(
        declared, emitted,
        "schema 的 properties 必须与写侧产物的顶层键一一对应"
    );
    let required = string_array(&contract["required"]);
    for key in &required {
        assert!(
            emitted.contains(key),
            "{key} 被列为必填，但写侧产物里没有这个键: {emitted:?}"
        );
    }
    // 七个汇总钱字段可以缺席于 `required`（未算时印 null），但必须出现在 properties 里，
    // 否则客户端按契约就解不出自家读模型每天发的那一份。
    for key in [
        "equity_raw",
        "available_raw",
        "margin_raw",
        "frozen_raw",
        "realized_pnl_raw",
        "unrealized_pnl_raw",
        "fees_raw",
        "funding_raw",
    ] {
        assert!(
            declared.contains(&key.to_string()),
            "写侧的钱字段 {key} 没有出现在契约里"
        );
    }
}

/// 契约里的"可空"必须逐项等于写侧印得出的形状：印得出 `null` 的那一格才可空（V11 T3，合流轮改判据）。
/// 这里原本手抄着一份"七个可空、权益不可空"的名单，而 Q70 把 `equity_raw` 也变成了 `Option<i128>`
/// ——缺标记价时报缺席，而不是拿剩余现金冒充权益。名单不会自己跟上类型，所以判据改成让写侧自己交
/// 两份产物：一份"这一层什么都没算"、一份"八项全算得出"。印 null 的必须在契约里可空，而契约允许的
/// null 必须真被 `from_json` 收得回——否则"可空"只是纸面口径。
#[test]
fn schema_keeps_uncomputed_money_nullable() {
    const SCALARS: [&str; 8] = [
        "equity_raw",
        "available_raw",
        "margin_raw",
        "frozen_raw",
        "realized_pnl_raw",
        "unrealized_pnl_raw",
        "fees_raw",
        "funding_raw",
    ];
    let contract = schema();
    let properties = contract["properties"]
        .as_object()
        .expect("properties 应为对象");
    let mut uncomputed = AccountSnapshot::new(11, "main", "default", "BINANCE", 10);
    uncomputed.seal();
    let mut computed = AccountSnapshot::new(12, "main", "default", "BINANCE", 10);
    computed.equity_raw = Some(1_000);
    for slot in [
        &mut computed.available_raw,
        &mut computed.margin_raw,
        &mut computed.frozen_raw,
        &mut computed.realized_pnl_raw,
        &mut computed.unrealized_pnl_raw,
        &mut computed.fees_raw,
        &mut computed.funding_raw,
    ] {
        *slot = Some(0);
    }
    computed.seal();
    let uncomputed_text = uncomputed.to_json();
    let computed_text = computed.to_json();
    let uncomputed_doc: Value =
        serde_json::from_str(&uncomputed_text).expect("写侧产物必须是合法 JSON");
    let computed_doc: Value =
        serde_json::from_str(&computed_text).expect("写侧产物必须是合法 JSON");
    for key in SCALARS {
        let declared = properties[key]["type"]
            .as_array()
            .unwrap_or_else(|| panic!("{key} 必须声明为可空类型（写侧没算过时印 null）"))
            .iter()
            .map(|value| value.as_str().expect("类型项应为字符串"))
            .collect::<Vec<_>>();
        assert_eq!(
            declared,
            vec!["integer", "null"],
            "{key} 的类型必须是 integer|null，别的写法都在改动「未算」的形状"
        );
        assert!(
            uncomputed_doc[key].is_null(),
            "判据读错了东西：写侧「没算过」时并没有给 {key} 印 null"
        );
        assert!(
            computed_doc[key].is_i64(),
            "判据读错了东西：写侧算出时 {key} 不是整数"
        );
    }
    let read_back = AccountSnapshot::from_json(&uncomputed_text)
        .expect("契约声明可空的每一格，读侧都必须收得回写侧印出的 null");
    assert!(
        read_back.equity_raw.is_none()
            && read_back.available_raw.is_none()
            && read_back.margin_raw.is_none()
            && read_back.frozen_raw.is_none()
            && read_back.realized_pnl_raw.is_none()
            && read_back.unrealized_pnl_raw.is_none()
            && read_back.fees_raw.is_none()
            && read_back.funding_raw.is_none(),
        "读回来的快照把未算的钱折成了某个数：{uncomputed_text}"
    );
    // 跨语言夹具同时存在两种状态：算得出的权益与未算的七格。上面那份"全部未算"是构造出来的，
    // 夹具这一份才是读模型每天真的发出去的形状。
    let sample = writer_sample();
    let nulls = SCALARS
        .iter()
        .filter(|key| sample[**key].is_null())
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(
        nulls,
        vec![
            "available_raw",
            "margin_raw",
            "frozen_raw",
            "realized_pnl_raw",
            "unrealized_pnl_raw",
            "fees_raw",
            "funding_raw",
        ],
        "夹具里未算的钱字段与可空声明不符：两种状态必须同场"
    );
    assert!(
        sample["equity_raw"].is_i64(),
        "夹具里那一份权益必须是算得出的整数，否则「两种状态同场」的证据就没了"
    );
}

/// 读侧必须按契约里那个 `"const": 1` 收口。此前它只核对协议字符串与两处版本号的自洽性，
/// 一份 `schema_version: 7` 的文档会被当成 v1 静默解出来——而对面 Python 的
/// `load_account_snapshot` 对同一份产物是直接抛错的（一宽一严）。
#[test]
fn reader_rejects_versions_the_served_schema_does_not_declare() {
    let supported = ACCOUNT_SNAPSHOT_SCHEMA_VERSION as i64;
    assert_eq!(
        schema()["properties"]["schema_version"]["const"].as_i64(),
        Some(supported),
        "契约声明的版本号必须就是读侧支持的那一个"
    );
    let base = writer_sample();
    for version in [supported - 1, supported + 1, supported + 6] {
        let mut document = base.clone();
        document["schema_version"] = Value::from(version);
        let message = reject_reason(
            AccountSnapshot::from_json(&document.to_string()),
            &format!("版本 {version} 不是契约声明的版本"),
        );
        assert!(
            message.contains("不受支持"),
            "拒绝理由必须说清是版本不受支持，而不是把它混成解析错误: {message}"
        );
    }
    let mut document = base.clone();
    document["schema_version"] = Value::from(supported);
    AccountSnapshot::from_json(&document.to_string()).expect("契约声明的那一份必须读得回来");

    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("schema_version");
    let message = reject_reason(
        AccountSnapshot::from_json(&missing.to_string()),
        "缺版本号不能被当成 v1",
    );
    assert!(
        message.contains("缺失"),
        "缺版本号的理由应当独立于版本不受支持: {message}"
    );

    // 顶层与 header 各说各话仍是另一条判据：一致才自洽，不能靠后写的那一份把另一份兜掉。
    let mut drifted = base.clone();
    drifted["header"]["schema_version"] = Value::from(supported + 1);
    let message = reject_reason(
        AccountSnapshot::from_json(&drifted.to_string()),
        "header 与顶层版本不一致",
    );
    assert!(
        message.contains("前后不一致"),
        "版本自洽性判据被换掉了: {message}"
    );
}

/// 上一条钉的是"改版本号会撞哈希"，这一条钉的是真正危险的那一份：用另一个版本号
/// **自洽封存**的文档。它的 `state_hash` 里本来就带着那个版本号，哈希替不了版本闸门，
/// 而此前 `from_json` 只看两处版本号是否自相矛盾 —— 这样的文档会被当成 v1 收下，
/// 字段含义却由另一份契约决定（V11 S10 的后半：契约说 `const 1`，读侧不照做）。
#[test]
fn reader_refuses_a_self_consistent_document_from_another_version() {
    let foreign = ACCOUNT_SNAPSHOT_SCHEMA_VERSION + 1;
    let mut document = AccountSnapshot::new(7, "main", "default", "BINANCE", 10);
    document.equity_raw = Some(1000);
    document.header.schema_version = foreign;
    document.seal();
    let encoded = document.to_json();
    assert!(
        encoded.contains(&format!("\"schema_version\":{foreign}")),
        "夹具必须真的带着另一个版本号，否则这条用例什么都没测: {encoded}"
    );
    document
        .validate()
        .expect("跨版本文档在自己那一版口径下是自洽的——所以闸门只能是版本判断");
    let message = reject_reason(
        AccountSnapshot::from_json(&encoded),
        &format!("v{foreign} 的文档不能被当成 v1 收下"),
    );
    assert!(
        message.contains("不受支持"),
        "必须按版本不受支持拒绝，而不是靠别的不变式误伤: {message}"
    );
}

/// 契约的骨架：协议名、草稿版本与 `$id` 都是客户端拿它做校验的入口，改动即换契约。
#[test]
fn schema_frame_matches_the_protocol_it_describes() {
    let frame = schema();
    assert_eq!(
        frame["$schema"].as_str(),
        Some("https://json-schema.org/draft/2020-12/schema")
    );
    assert_eq!(
        frame["$id"].as_str(),
        Some("https://qianxing.dev/schema/account-snapshot-v1.json"),
        "$id 里的 v1 必须与 schema_version 常量说的是同一份契约"
    );
    assert_eq!(frame["type"].as_str(), Some("object"));
    assert_eq!(
        frame["properties"]["protocol"]["const"].as_str(),
        Some("QIANXING_ACCOUNT"),
        "读侧 `from_json` 认的协议名必须就是契约声明的那一个"
    );
    // header 的契约是一份嵌套声明：外层 properties 只说"这是对象"，必填列在它自己的
    // `required` 里，漏掉任何一列都会让读模型少印字段时仍然"合规"。
    let header_required = string_array(&frame["properties"]["header"]["required"]);
    for key in [
        "snapshot_id",
        "account_id",
        "portfolio_id",
        "venue_id",
        "as_of",
        "event_seq",
        "state_hash",
    ] {
        assert!(
            header_required.contains(&key.to_string()),
            "header 的契约里没有 {key}，而读模型每天都在印它"
        );
    }
}
