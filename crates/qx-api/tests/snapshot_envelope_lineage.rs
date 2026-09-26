//! 外销信封的血缘取的是真值，缺的那几格不上 wire（V11 L2，并给 F1-N6 补上牙齿）。
//!
//! 这里读的是端点真正产出的那份 JSON，不是手抄的形状（V11 R18 的同一口径）：改构造点就得
//! 连带改这份产品，编一个摘要或把组件名当摘要填进 `source_digest` 都会被这条咬住。
//! 住在 `tests/` 而不进 `src/lib.rs` 的内联用例，是因为那条读面本身在行数棘轮下（V11 T1/T3）。

use qx_api::{ApiService, ApiState};
use qx_core::{
    Event, EventKind, EventLog, InstrumentId, Order, OrderStatus, Priority, Quantity, Side,
};
use qx_protocol::AccountSnapshot;
use serde_json::{json, Value};

fn order(client_id: u64) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: "account-a".into(),
        trace: None,
        policy: None,
    }
}

/// 一份既快照过、又投影过两条订单事实的读面，外加那条投影吃进去的同一份事件源。
/// `publish_snapshot` 一次同时写下全局主账户快照与那份身份投影，因此带键与无键两条读路
/// 都能走到（F1-N6 记的就是无键那一条没有反例）。
fn projected() -> (ApiService, EventLog) {
    let mut snapshot = AccountSnapshot::new(1, "account-a", "portfolio-a", "paper", 10);
    snapshot.cash_raw.insert("USDT".into(), 100);
    let mut state = ApiState::default();
    state
        .publish_snapshot(snapshot)
        .expect("夹具快照必须可发布");

    let mut source = EventLog::new();
    for client_id in [7_u64, 8] {
        source
            .append_checked(Event::new(
                client_id - 7,
                10 + client_id,
                Priority::APPLY,
                EventKind::OrderSubmitted {
                    order: order(client_id),
                },
            ))
            .expect("事件必须按序追加");
    }
    assert_eq!(
        state
            .project_account_event_log("account-a", "paper", &source)
            .unwrap(),
        2
    );
    (ApiService::new(state), source)
}

#[test]
fn the_snapshot_envelope_publishes_the_projection_source_digest() {
    let (service, source) = projected();
    let expected = format!("{:016x}", source.digest());
    for (label, path) in [
        (
            "带键",
            "/account/snapshot/envelope?account_id=account-a&venue_id=paper",
        ),
        ("无键", "/account/snapshot/envelope"),
    ] {
        let response = service.handle("GET", path, "", 1);
        assert_eq!(response.status, 200, "{label}：{}", response.body);
        let envelope: Value = serde_json::from_str(&response.body).expect("信封必须是合法 JSON");
        assert_eq!(
            envelope["lineage"]["source_digest"].as_str(),
            Some(expected.as_str()),
            "{label}信封的摘要必须取自那份被投影的事件源，编一个数就等于伪造血缘"
        );
        for cell in ["dataset_version", "manifest_digest"] {
            assert!(
                envelope["lineage"].get(cell).is_none(),
                "{cell} 没有可说的来源时不得以空串外销: {}",
                envelope["lineage"]
            );
        }
    }
}

/// 事件信封来自进程内总线：那条总线没有可发布的整体摘要，所以三格必须整体缺席。
/// 此前它把总线名 `"api-event-bus"` 填进 `source_digest`，读侧收到的是一个不是摘要的摘要。
#[test]
fn the_event_envelopes_claim_no_lineage_they_cannot_back() {
    let (service, _) = projected();
    let response = service.handle(
        "GET",
        "/events/live?account_id=account-a&venue_id=paper",
        "",
        1,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let envelopes: Vec<Value> = serde_json::from_str(&response.body).expect("事件信封是数组");
    assert_eq!(envelopes.len(), 2, "两条事件都要出现在读面上");
    for envelope in envelopes {
        assert_eq!(
            envelope["lineage"],
            json!({}),
            "没有可核对的摘要可说时，`lineage` 必须整体缺席"
        );
    }
}
