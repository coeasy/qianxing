//! 快照包络必须满足**同一进程公布**的那份契约（V12 R4-h）。
//!
//! 缺陷形态：`/account/snapshot/envelope` 把 `AccountSnapshot` 的 serde 派生形状塞进
//! `data`，而 `/schema/account-snapshot-v1` 公布的契约是 `to_json()` 线格式。派生形状
//! 没有 `protocol`、没有顶层 `schema_version`，orders/fills 的 key 还从数字变成字符串
//! —— 客户端照本进程自己的 schema 校验这份包络，第一道门就过不去。

use qx_api::{ApiService, ApiState};
use qx_protocol::AccountSnapshot;

fn service_with_snapshot() -> ApiService {
    let mut state = ApiState::default();
    let mut snapshot = AccountSnapshot::new(1, "account-a", "portfolio-a", "paper", 7);
    snapshot.cash_raw.insert("USDT".into(), 123);
    snapshot.equity_raw = Some(123);
    state
        .publish_snapshot_for("account-a", "paper", snapshot)
        .expect("用例快照必须能发布");
    ApiService::new(state)
}

fn get_json(service: &ApiService, path: &str) -> serde_json::Value {
    let response = service.handle("GET", path, "", 1);
    assert_eq!(
        response.status, 200,
        "GET {path} 必须 200:\n{}",
        response.body
    );
    serde_json::from_str(&response.body).expect("{path} 的正文必须是 JSON")
}

/// 取数点必须是路由本身，而不是编译期常量：客户端能读到的只有这条路由。
fn published_schema(service: &ApiService) -> serde_json::Value {
    let response = service.handle("GET", "/schema/account-snapshot-v1", "", 1);
    assert_eq!(response.status, 200, "契约路由必须可用");
    let schema: serde_json::Value =
        serde_json::from_str(&response.body).expect("公布的契约必须是 JSON");
    assert_eq!(
        schema["$id"].as_str(),
        Some("https://qianxing.dev/schema/account-snapshot-v1.json"),
        "契约路由必须真的在公布账户快照 schema，用例才谈得上按它校验"
    );
    schema
}

#[test]
fn envelope_data_satisfies_the_schema_the_same_server_publishes() {
    let service = service_with_snapshot();
    let schema = published_schema(&service);
    let required = schema["required"]
        .as_array()
        .expect("契约必须列出 required 字段");
    assert!(
        required.len() >= 2,
        "required 名单过短说明契约本身退化: {required:?}"
    );
    let envelope = get_json(
        &service,
        "/account/snapshot/envelope?account_id=account-a&venue_id=paper",
    );
    let data = &envelope["data"];
    for key in required {
        let key = key.as_str().expect("required 项必须是字段名");
        assert!(
            data.get(key).is_some(),
            "包络的 data 缺契约必需字段 {key:?}；公布的 required 为 {required:?}\ndata 实际字段: {:?}",
            data.as_object().map(|object| object.keys().collect::<Vec<_>>()),
        );
    }
    // const 约束同样要落在真身上，不然"字段在"不等于"字段对"。
    for field in ["protocol", "schema_version"] {
        let expected = &schema["properties"][field]["const"];
        assert!(
            !expected.is_null(),
            "契约的 {field} 必须声明 const，否则这条断言是空转"
        );
        assert_eq!(
            &data[field], expected,
            "包络 data 的 {field} 必须等于契约公布的 {expected}"
        );
    }
    for key in schema["properties"]["header"]["required"]
        .as_array()
        .expect("契约必须列出 header 的 required 字段")
    {
        let key = key.as_str().expect("header required 项必须是字段名");
        assert!(
            data["header"].get(key).is_some(),
            "包络 data 的 header 缺契约必需字段 {key:?}"
        );
    }
    assert_eq!(
        envelope["kind"].as_str(),
        Some("account_snapshot"),
        "包络必须自报内容类型，客户端才知道用哪份 schema 校验 data"
    );
}

/// 同一个读模型的两条路由只能有一份编码器：包络里的快照与平面路由的正文必须逐字段相同。
#[test]
fn envelope_data_is_the_same_wire_document_the_flat_route_serves() {
    let service = service_with_snapshot();
    let flat = get_json(
        &service,
        "/account/snapshot?account_id=account-a&venue_id=paper",
    );
    let envelope = get_json(
        &service,
        "/account/snapshot/envelope?account_id=account-a&venue_id=paper",
    );
    assert_eq!(
        envelope["data"], flat,
        "包络的 data 与平面快照路由必须是同一份线格式"
    );
    // 钱的口径也要跟着走同一份编码：未算过的字段是 null，不是 0。
    assert_eq!(
        flat["equity_raw"].as_i64(),
        Some(123),
        "用例前提：算得出的权益必须是 123\n{flat}"
    );
    assert!(
        flat["margin_raw"].is_null(),
        "没算过的保证金必须仍是 null，不能因换编码器变成 0\n{flat}"
    );
}
