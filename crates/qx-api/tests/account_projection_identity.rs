//! 账户投影的身份闸门：放宽只能落在"没有账户域栏位"的那两类事实，成交线必须守住交易所边界。
//!
//! 住在 `tests/` 而不是 `src/lib.rs` 的内联用例里，是为了不把常驻反例的压力加到被行数棘轮
//! 看管的读面上（V11 T1/T3）。

use qx_api::{ApiService, ApiState};
use qx_core::{Event, EventKind, EventLog, LedgerEntry, Priority};
use qx_protocol::AccountSnapshot;

/// V11 R13 的常驻反例。订单与账簿条目没有"账户域"栏位，只有标的后缀，
/// 所以拿 `BTCUSDT.BINANCE` 的 `BINANCE` 当账户 venue，会让 paper 账户
/// **自己的**成交链事实被判定成外来事实并被拒投影。这里同时钉住三条口径：
/// 本账户的无 venue 事实必须收下并出现在读模型里、换账户必须拒绝、
/// 带 venue 的成交仍必须守住交易所边界（放宽只限于这两类事件）。
#[test]
fn account_projection_checks_venue_less_facts_by_account_identity_only() {
    fn order(account_id: &str) -> qx_core::Order {
        qx_core::Order {
            client_id: 7,
            instrument: qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_i64(1),
            limit: None,
            status: qx_core::OrderStatus::Accepted,
            filled: qx_core::Quantity::ZERO,
            account_id: account_id.into(),
            trace: None,
            policy: None,
        }
    }
    fn ledger_entry(account_id: &str) -> LedgerEntry {
        LedgerEntry {
            id: 1,
            account_id: account_id.into(),
            currency: "USDT".into(),
            kind: qx_core::LedgerEntryKind::TradeCash,
            amount: qx_core::Money::from_i64(-50_000),
            instrument: Some(qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap()),
            quantity: qx_core::Quantity::from_i64(1),
            price: Some(qx_core::Price::from_i64(50_000)),
            order_id: Some(7),
            ts: 30,
            multiplier: 1,
            position_side: None,
        }
    }
    fn fill(account_id: &str, venue_id: Option<&str>) -> qx_core::Fill {
        qx_core::Fill {
            order_id: 7,
            qty: qx_core::Quantity::from_i64(1),
            price: qx_core::Price::from_i64(50_000),
            fee: qx_core::Money::ZERO,
            ts: 30,
            account_id: account_id.into(),
            venue_id: venue_id.map(str::to_string),
            ..qx_core::Fill::default()
        }
    }
    fn log(events: Vec<Event>) -> EventLog {
        let mut source = EventLog::new();
        for event in events {
            source.append_checked(event).unwrap();
        }
        source
    }
    fn event(seq: u64, kind: EventKind) -> Event {
        Event::new(seq, 10 + seq, Priority::APPLY, kind)
    }

    let own_facts = log(vec![
        event(
            0,
            EventKind::OrderSubmitted {
                order: order("account-a"),
            },
        ),
        event(
            1,
            EventKind::LedgerApplied {
                entry: ledger_entry("account-a"),
            },
        ),
        event(
            2,
            EventKind::Filled {
                fill: fill("account-a", Some("PAPER")),
            },
        ),
    ]);
    let mut state = ApiState::default();
    assert_eq!(
        state
            .project_account_event_log("account-a", "paper", &own_facts)
            .unwrap(),
        3,
        "paper 账户交易 BINANCE 标的时，自己的订单与账簿事实必须能投影"
    );
    let service = ApiService::new(state);
    let events = service.handle("GET", "/events?account_id=account-a&venue_id=paper", "", 1);
    assert_eq!(events.status, 200);
    assert!(events.body.contains("\"symbol\":\"BTCUSDT\""));
    assert!(events.body.contains("OrderSubmitted"));
    assert!(events.body.contains("LedgerApplied"));

    let foreign_order = log(vec![event(
        0,
        EventKind::OrderSubmitted {
            order: order("account-b"),
        },
    )]);
    let mut state = ApiState::default();
    let error = state
        .project_account_event_log("account-a", "paper", &foreign_order)
        .unwrap_err();
    assert!(error.contains("事件身份与 API 投影不匹配"), "{error}");
    assert!(error.contains("account-b"), "{error}");

    let foreign_ledger = log(vec![event(
        0,
        EventKind::LedgerApplied {
            entry: ledger_entry("account-b"),
        },
    )]);
    let mut state = ApiState::default();
    assert!(state
        .project_account_event_log("account-a", "paper", &foreign_ledger)
        .unwrap_err()
        .contains("事件身份与 API 投影不匹配"));

    let cross_venue_fill = log(vec![event(
        0,
        EventKind::Filled {
            fill: fill("account-a", Some("binance")),
        },
    )]);
    let mut state = ApiState::default();
    assert!(state
        .project_account_event_log("account-a", "paper", &cross_venue_fill)
        .unwrap_err()
        .contains("事件身份与 API 投影不匹配"));
}

/// 七个带键读面对"这份部署里没有这个账户"必须回同一个 404（V13 R2 第十二遍 #191）。
///
/// 缺陷形态不是"回错了码"，而是**根本没有这一格**：`/account/snapshot` 在投影不存在时回
/// `404 snapshot_not_found`，而同一条件下的 `/account/orders`、`/account/positions`、
/// `/account/balances`、`/events`、`/events/live`、`/account/snapshot/envelope` 回的是
/// 200 + 空数组/`{}`/`null`。操作员把 `account_id` 少打一个字符，读到的不是"这个账户不存在"，
/// 而是"这个账户干净得一张单都没有"——在交易平台上这是最贵的一种误读。
///
/// 这里同时钉住收口后必须**不**被合并进来的三格，否则这条判据会顺手把别的口径改掉：
/// 形状非法的键仍是 400（400 在 404 之前判），投影存在但还没算出快照仍是各面自己承诺的
/// 200 空数组，不带键的读法走全局投影、完全不受影响。
#[test]
fn unknown_projection_key_is_404_on_every_scoped_read_face() {
    const SCOPED_FACES: [&str; 7] = [
        "/account/snapshot",
        "/account/snapshot/envelope",
        "/account/orders",
        "/account/positions",
        "/account/balances",
        "/events",
        "/events/live",
    ];

    let mut state = ApiState::default();
    let mut source = EventLog::new();
    source
        .append_checked(Event::new(
            0,
            10,
            Priority::APPLY,
            EventKind::LedgerApplied {
                entry: LedgerEntry {
                    id: 1,
                    account_id: "account-a".into(),
                    currency: "USDT".into(),
                    kind: qx_core::LedgerEntryKind::TradeCash,
                    amount: qx_core::Money::from_i64(-50_000),
                    instrument: Some(qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap()),
                    quantity: qx_core::Quantity::from_i64(1),
                    price: Some(qx_core::Price::from_i64(50_000)),
                    order_id: Some(7),
                    ts: 30,
                    multiplier: 1,
                    position_side: None,
                },
            },
        ))
        .unwrap();
    assert_eq!(
        state
            .project_account_event_log("account-a", "paper", &source)
            .unwrap(),
        1
    );
    state
        .publish_snapshot_for(
            "account-a",
            "paper",
            AccountSnapshot::new(1, "account-a", "default", "paper", 40),
        )
        .expect("快照挂在同一份投影上");
    let service = ApiService::new(state);

    for face in SCOPED_FACES {
        let response = service.handle(
            "GET",
            &format!("{face}?account_id=account-a-typo&venue_id=paper"),
            "",
            1,
        );
        assert_eq!(
            response.status, 404,
            "{face} 对不存在的投影必须回 404，实际:\n{}",
            response.body
        );
        assert!(
            response.body.contains("account_projection_not_found"),
            "{face} 的 404 要能被客户端按 `error` 分支识别:\n{}",
            response.body
        );
        // venue 写错与账户写错是同一格：判据只看这一对键在不在仓内。
        let wrong_venue = service.handle(
            "GET",
            &format!("{face}?account_id=account-a&venue_id=binance"),
            "",
            1,
        );
        assert_eq!(wrong_venue.status, 404, "{face} 上 venue 写错同样是 404");
    }

    // 投影存在：七个面各按自己的口径回数据，一个都不许被上面的 404 顺手接管。
    for face in SCOPED_FACES {
        let response = service.handle(
            "GET",
            &format!("{face}?account_id=account-a&venue_id=paper"),
            "",
            1,
        );
        assert_eq!(
            response.status, 200,
            "{face} 上真实投影不能被读成 404:\n{}",
            response.body
        );
    }

    // 只给一半的键是形状非法，仍在 404 之前判 400。
    for face in SCOPED_FACES {
        let response = service.handle("GET", &format!("{face}?account_id=account-a"), "", 1);
        assert_eq!(
            response.status, 400,
            "{face} 上缺 venue_id 仍按参数非法处理，不是投影缺失"
        );
    }

    // 不带键：走全局投影，这份状态里没有全局快照，所以快照面维持原口径。
    assert_eq!(
        service.handle("GET", "/account/snapshot", "", 1).status,
        404,
        "不带键时仍是原来的 snapshot_not_found 通道"
    );
    assert!(service
        .handle("GET", "/account/snapshot", "", 1)
        .body
        .contains("snapshot_not_found"));
    assert_eq!(
        service
            .handle(
                "GET",
                "/account/orders?account_id=account-a&venue_id=paper",
                "",
                1
            )
            .status,
        200
    );
}

/// 投影存在但还没算出快照，不能被 #191 的 404 吞掉（V13 R2 第十二遍）。
///
/// 这一格刻意与上一段分开发：`projections` 里挂了投影、`snapshot` 却是 `None` 的状态是
/// "刚接上账户、第一条事实还没落"的中间态。文档对它的承诺是 200 空数组 / `null`，
/// 与"这个账户根本不在这份部署里"是两件事——合并成一个码就再也读不回来了。
#[test]
fn projection_without_a_snapshot_keeps_the_empty_200_contract() {
    let mut state = ApiState::default();
    state
        .projections
        .insert(api_projection_key(), Default::default());
    let service = ApiService::new(state);
    for face in [
        "/account/orders",
        "/account/positions",
        "/account/balances",
        "/events",
        "/events/live",
    ] {
        let response = service.handle(
            "GET",
            &format!("{face}?account_id=account-a&venue_id=paper"),
            "",
            1,
        );
        assert_eq!(
            response.status, 200,
            "{face} 上'有投影、无快照'仍承诺 200 空:\n{}",
            response.body
        );
    }
    let snapshot = service.handle(
        "GET",
        "/account/snapshot?account_id=account-a&venue_id=paper",
        "",
        1,
    );
    assert_eq!(snapshot.status, 404);
    assert!(
        snapshot.body.contains("snapshot_not_found"),
        "快照面自己那一格仍是 snapshot_not_found:\n{}",
        snapshot.body
    );
}

fn api_projection_key() -> qx_api::ApiProjectionKey {
    qx_api::ApiProjectionKey {
        account_id: "account-a".into(),
        venue_id: "paper".into(),
    }
}
