//! 账户投影的身份闸门：放宽只能落在"没有账户域栏位"的那两类事实，成交线必须守住交易所边界。
//!
//! 住在 `tests/` 而不是 `src/lib.rs` 的内联用例里，是为了不把常驻反例的压力加到被行数棘轮
//! 看管的读面上（V11 T1/T3）。

use qx_api::{ApiService, ApiState};
use qx_core::{Event, EventKind, EventLog, LedgerEntry, Priority};

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
