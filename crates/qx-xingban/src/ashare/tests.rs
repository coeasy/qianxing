//! A 股市场规则用例：T+1/整手、涨跌停封板、公司行为 JSON 装载与交易日历窗口。
//!
//! 从 `ashare.rs` 尾部 `#[cfg(test)] mod tests` 纯搬家拆出。

use super::*;
use qx_core::{InstrumentId, OrderStatus, Quantity};

fn order(side: Side, qty: i128) -> Order {
    Order {
        client_id: 1,
        instrument: InstrumentId::parse("000001.SZSE").unwrap(),
        side,
        qty: Quantity::from_raw(qty),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

#[test]
fn t_plus_one_and_lot_rules_are_enforced() {
    let rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    let lot = rules.lot_size;
    assert!(rules
        .validate_order(&order(Side::Buy, lot), 0, 0, 1)
        .is_ok());
    assert!(rules
        .validate_order(&order(Side::Buy, lot + 1), 0, 0, 1)
        .is_err());
    assert!(rules
        .validate_order(&order(Side::Sell, lot), lot, lot, 1)
        .is_err());
    assert!(rules
        .validate_order(&order(Side::Sell, lot), lot, 0, 1 + DAY_MS)
        .is_ok());
}

#[test]
fn sealed_limit_and_halt_block_the_correct_side() {
    let rules = AshareRuleConfig {
        enabled: true,
        halted_timestamps: vec![2],
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    let up = 10 * SCALE;
    let up_limit = rules.limits(up).0;
    let sealed = Bar::new(1, up_limit, up_limit, up_limit, up_limit, 1);
    assert!(rules.blocks_fill(Side::Buy, &sealed, Some(up)));
    assert!(!rules.blocks_fill(Side::Sell, &sealed, Some(up)));
    assert!(!rules.is_trading(2));
}

#[test]
fn supported_data_actions_convert_and_complex_actions_fail_closed() {
    let supported = vec![qx_data::CorporateAction {
        instrument: "000001.SZSE".into(),
        timestamp: 1,
        action_type: qx_data::CorporateActionType::Dividend,
        value_raw: 5,
        secondary_value_raw: 0,
        ratio_num: 1,
        ratio_den: 1,
        price_raw: 0,
        published_at: None,
        effective_at: None,
        source: "test".into(),
    }];
    let events = AshareRuleConfig::corporate_actions_from_data("000001.SZSE", &supported).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cash_dividend_raw, 5);
    let complex = qx_data::CorporateAction {
        instrument: "000001.SZSE".into(),
        timestamp: 2,
        action_type: qx_data::CorporateActionType::RightsIssue,
        value_raw: 0,
        secondary_value_raw: 0,
        ratio_num: 1,
        ratio_den: 10,
        price_raw: 8,
        published_at: None,
        effective_at: None,
        source: "test".into(),
    };
    assert!(AshareRuleConfig::corporate_actions_from_data("000001.SZSE", &[complex]).is_err());
}

#[test]
fn python_action_json_is_converted_with_suspension_and_pit_fields() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"cash_dividend","cash_dividend_raw":100000000,"published_at":"2024-05-20T09:00:00+08:00","source":"akshare"},
            {"instrument":"000001.SZSE","ex_date":"2024-06-04","action_type":"suspension","source":"akshare"}
        ]"#;
    assert_eq!(
        rules
            .apply_corporate_actions_json("000001.SZSE", payload)
            .unwrap(),
        2
    );
    assert_eq!(rules.corporate_actions.len(), 1);
    assert_eq!(rules.corporate_actions[0].cash_dividend_raw, 100_000_000);
    assert_eq!(rules.halted_timestamps.len(), 1);
    assert!(!rules.is_trading(rules.halted_timestamps[0]));
}

#[test]
fn python_action_json_rejects_invalid_ratio_and_instrument() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let wrong_instrument =
        r#"[{"instrument":"600000.SSE","ex_date":"2024-06-03","action_type":"cash_dividend"}]"#;
    assert!(rules
        .apply_corporate_actions_json("000001.SZSE", wrong_instrument)
        .is_err());
    let invalid_ratio = r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"bonus_share","share_ratio_num":0}]"#;
    assert!(rules
        .apply_corporate_actions_json("000001.SZSE", invalid_ratio)
        .is_err());
}

#[test]
fn capital_change_requires_explicit_issuer_snapshot() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"capital_change",
             "issuer_total_shares_raw":"1000000000000","issuer_free_float_shares_raw":"700000000000",
             "source":"exchange"}
        ]"#;
    rules
        .apply_corporate_actions_json("000001.SZSE", payload)
        .unwrap();
    let snapshots = rules.issuer_capital_snapshots("000001.SZSE", None).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].total_shares_raw, 1_000_000_000_000);
    assert_eq!(snapshots[0].free_float_shares_raw, Some(700_000_000_000));

    let missing_total =
        r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"capital_change"}]"#;
    assert!(rules
        .apply_corporate_actions_json("000001.SZSE", missing_total)
        .is_err());
    assert_eq!(rules.corporate_actions.len(), 1);
}

#[test]
fn capital_change_from_data_maps_absolute_supply_fields() {
    let action = qx_data::CorporateAction {
        instrument: "000001.SZSE".into(),
        timestamp: 10,
        action_type: qx_data::CorporateActionType::CapitalChange,
        value_raw: 1_000,
        secondary_value_raw: 700,
        ratio_num: 1,
        ratio_den: 1,
        price_raw: 0,
        published_at: None,
        effective_at: None,
        source: "provider".into(),
    };
    let events = AshareRuleConfig::corporate_actions_from_data("000001.SZSE", &[action]).unwrap();
    assert_eq!(
        events[0].action_type,
        AshareCorporateActionType::CapitalChange
    );
    assert_eq!(events[0].issuer_total_shares_raw, 1_000);
    assert_eq!(events[0].issuer_free_float_shares_raw, Some(700));
}

#[test]
fn python_rights_action_json_preserves_lifecycle_dates() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","record_date":"2024-05-31","subscription_start":"2024-06-04","subscription_end":"2024-06-07","payment_date":"2024-06-10","action_type":"rights_issue","rights_instrument":"700001.SZSE","rights_issue_price_raw":5000000000,"rights_issue_ratio_num":20,"rights_issue_ratio_den":100,"subscription_qty_raw":10,"source":"akshare"}]"#;
    rules
        .apply_corporate_actions_json("000001.SZSE", payload)
        .unwrap();
    let event = &rules.corporate_actions[0];
    assert!(event.record_ts.is_some());
    assert!(event.subscription_start_ts.unwrap() > event.ts);
    assert!(event.subscription_end_ts.unwrap() > event.subscription_start_ts.unwrap());
    assert!(event.payment_ts.unwrap() > event.ts);
}

#[test]
fn python_action_json_keeps_explicit_complex_instruction_fields() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"rights_issue",
             "rights_instrument":"000001.SZSE","rights_issue_price_raw":5000000000,
             "rights_issue_ratio_num":100000000,"rights_issue_ratio_den":1000000000,
             "subscription_qty_raw":10000000000}
        ]"#;
    rules
        .apply_corporate_actions_json("000001.SZSE", payload)
        .unwrap();
    let event = &rules.corporate_actions[0];
    assert_eq!(event.rights_instrument.as_deref(), Some("000001.SZSE"));
    assert_eq!(event.subscription_qty_raw, 10_000_000_000);
    assert!(AshareRuleConfig::corporate_action_supported_by_ledger(
        AshareCorporateActionType::RightsIssue
    ));
}

#[test]
fn python_action_json_accepts_explicit_rights_expiry_quantity() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-10","action_type":"rights_issue_expiry",
             "rights_instrument":"700001.SZSE","rights_expiry_qty_raw":10000000000}
        ]"#;
    rules
        .apply_corporate_actions_json("000001.SZSE", payload)
        .unwrap();
    let event = &rules.corporate_actions[0];
    assert_eq!(
        event.action_type,
        AshareCorporateActionType::RightsIssueExpiry
    );
    assert_eq!(event.rights_expiry_qty_raw, 10_000_000_000);
}

#[test]
fn python_action_json_accepts_convertible_bond_lifecycle_fields() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"convertible_bond_issue",
             "convertible_bond_instrument":"123001.SZSE","issue_price_raw":100000000000,
             "subscription_qty_raw":100000000000},
            {"instrument":"000001.SZSE","ex_date":"2024-06-10","record_date":"2024-06-10",
             "payment_date":"2024-06-20","action_type":"convertible_bond_interest",
             "convertible_bond_instrument":"123001.SZSE","interest_per_bond_raw":5000000000},
            {"instrument":"000001.SZSE","ex_date":"2024-06-30","payment_date":"2024-06-30",
             "action_type":"convertible_bond_redemption","convertible_bond_instrument":"123001.SZSE",
             "settlement_qty_raw":100000000000,"settlement_price_raw":105000000000},
            {"instrument":"000001.SZSE","ex_date":"2024-07-31","payment_date":"2024-07-31",
             "action_type":"convertible_bond_call","convertible_bond_instrument":"123001.SZSE",
             "settlement_price_raw":106000000000}
        ]"#;
    assert_eq!(
        rules
            .apply_corporate_actions_json("000001.SZSE", payload)
            .unwrap(),
        4
    );
    assert_eq!(
        rules.corporate_actions[0].action_type,
        AshareCorporateActionType::ConvertibleBondIssue
    );
    assert_eq!(
        rules.corporate_actions[1].interest_per_bond_raw,
        5_000_000_000
    );
    assert_eq!(
        rules.corporate_actions[2].settlement_price_raw,
        105_000_000_000
    );
    assert_eq!(
        rules.corporate_actions[3].action_type,
        AshareCorporateActionType::ConvertibleBondCall
    );
    assert_eq!(rules.corporate_actions[3].settlement_qty_raw, 0);
    assert!(AshareRuleConfig::corporate_action_supported_by_ledger(
        AshareCorporateActionType::ConvertibleBondInterest
    ));
}

#[test]
fn calendar_json_controls_daily_and_intraday_trading_windows() {
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let payload = r#"{
            "calendar_id":"cn-2024",
            "trading_days":["2024-06-03"],
            "sessions":[["09:30","11:30"],["13:00","15:00"]]
        }"#;
    assert_eq!(rules.apply_calendar_json(payload).unwrap(), 1);
    let day = rules.trading_days[0];
    assert!(rules.is_trading(day));
    assert!(rules.is_trading(day + 9 * 3_600_000 + 30 * 60_000));
    assert!(!rules.is_trading(day + 12 * 3_600_000));
    assert!(!rules.is_trading(day + DAY_MS));
}

/// 未声明涨跌停带时按板块表推导：创业板/科创板 ±20%、北交所 ±30%，不再是扁平 ±10%。
/// 板块表此前写好了却没有读者，serde 默认把所有人都按 ±10% 封板。
#[test]
fn unset_limit_band_comes_from_the_board_not_a_flat_ten_percent() {
    let previous_close = 10 * SCALE;
    for (board, band) in [
        (AshareBoard::Main, 1_000),
        (AshareBoard::ChiNext, 2_000),
        (AshareBoard::Star, 2_000),
        (AshareBoard::Beijing, 3_000),
    ] {
        let rules = AshareRuleConfig {
            enabled: true,
            board,
            ..AshareRuleConfig::default()
        };
        rules.validate().unwrap();
        assert_eq!(
            (
                rules.effective_limit_up_bp(),
                rules.effective_limit_down_bp()
            ),
            (band, band),
            "{board:?} 未声明时必须按板块表推导"
        );
        assert_eq!(
            rules.limits(previous_close),
            (
                previous_close * (10_000 + i128::from(band)) / 10_000,
                previous_close * (10_000 - i128::from(band)) / 10_000
            ),
            "{board:?} 涨跌停价必须按生效带宽算"
        );
        assert!(
            rules.descriptor().contains(&format!("limit_up_bp={band}")),
            "{board:?} 的生效带宽要能在产物指纹里看出来"
        );
    }
}

/// 显式声明仍然压过板块表：既有规则 JSON 的口径不变。
#[test]
fn declared_limit_band_still_overrides_the_board_table() {
    let rules = AshareRuleConfig {
        enabled: true,
        board: AshareBoard::ChiNext,
        limit_up_bp: 1_000,
        limit_down_bp: 1_000,
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    assert_eq!(
        rules.limits(10 * SCALE),
        (11 * SCALE, 9 * SCALE),
        "声明 ±10% 的创业板规则不能被板块表改成 ±20%"
    );
}

#[test]
fn negative_declared_limit_band_is_rejected_at_the_gate() {
    let rules = AshareRuleConfig {
        enabled: true,
        limit_up_bp: -1,
        ..AshareRuleConfig::default()
    };
    assert!(
        rules.validate().is_err(),
        "0 才是\"按板块推导\"的哨兵，负数不能混过去"
    );
}

#[test]
fn limit_band_anchors_to_the_previous_session_close_not_the_previous_bar() {
    let rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    rules.validate().unwrap();
    let yuan = |cents: i128| cents * SCALE / 100;
    let minute = 60_000;
    let day_a = 100 * DAY_MS;
    let day_b = day_a + DAY_MS;
    // 昨天尾盘 9.90 -> 10.00 收盘；今天 10.50 起跳，涨到 11.00 封死在涨停板上。
    let bars = vec![
        Bar::new(
            day_a + 555 * minute,
            yuan(990),
            yuan(995),
            yuan(985),
            yuan(990),
            1_000,
        ),
        Bar::new(
            day_a + 560 * minute,
            yuan(995),
            yuan(1_000),
            yuan(990),
            yuan(1_000),
            1_000,
        ),
        Bar::new(
            day_b,
            yuan(1_050),
            yuan(1_055),
            yuan(1_045),
            yuan(1_050),
            1_000,
        ),
        Bar::new(
            day_b + 5 * minute,
            yuan(1_100),
            yuan(1_100),
            yuan(1_100),
            yuan(1_100),
            1_000,
        ),
    ];
    // 同一交易日内的前一根不是昨收，所以今天头一根之前都没有锚。
    assert_eq!(rules.previous_close(&bars, 0), None);
    assert_eq!(rules.previous_close(&bars, 1), None);
    assert_eq!(rules.previous_close(&bars, 2), Some(yuan(1_000)));
    // 关键一格：今天第二根的锚仍是昨天收，而不是 10.50 那根。
    assert_eq!(rules.previous_close(&bars, 3), Some(yuan(1_000)));
    assert_eq!(rules.limits(yuan(1_000)).0, yuan(1_100));
    assert!(rules.blocks_fill(Side::Buy, &bars[3], rules.previous_close(&bars, 3)));
    // 上一根 Bar 收价那种旧口径把板推到 11.55，这根封死的分钟线就照样成交。
    let (stale_up, _) = rules.limits(bars[2].close);
    assert_eq!(stale_up, yuan(1_155));
    assert!(!rules.blocks_fill(Side::Buy, &bars[3], Some(stale_up)));
    // 覆盖表赢过推导：除权除息日的昨收只能由数据侧给。
    let overridden = AshareRuleConfig {
        previous_close_raw: [(bars[3].ts, yuan(1_050))].into_iter().collect(),
        ..rules.clone()
    };
    overridden.validate().unwrap();
    assert_eq!(overridden.previous_close(&bars, 3), Some(yuan(1_050)));
}

/// 除权除息日的锚不再退回**不复权**昨收：折算读的就是公司行为 JSON 装载进来的那份事实。
///
/// 这根封在 10.45 的线在两种锚下结果不同 —— 它是 Q60 那条"锚错一格就照样成交"的除息版。
#[test]
fn ex_rights_anchor_folds_the_cash_dividend_loaded_with_the_rules() {
    let yuan = |cents: i128| cents * SCALE / 100;
    let previous_session = date_to_shanghai_midnight_ms("2024-05-31").unwrap();
    let ex_date = date_to_shanghai_midnight_ms("2024-06-03").unwrap();
    let bars = vec![
        Bar::new(
            previous_session,
            yuan(980),
            yuan(1_000),
            yuan(975),
            yuan(1_000),
            1_000,
        ),
        Bar::new(
            ex_date,
            yuan(1_045),
            yuan(1_045),
            yuan(1_045),
            yuan(1_045),
            1_000,
        ),
        Bar::new(
            ex_date + 5 * 60_000,
            yuan(1_045),
            yuan(1_045),
            yuan(1_045),
            yuan(1_045),
            1_000,
        ),
    ];
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    // 每股 0.50 元现金红利：参考价 (10.00 − 0.50) = 9.50，涨停 10.45。
    rules
        .apply_corporate_actions_json(
            "000001.SZSE",
            r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"cash_dividend","cash_dividend_raw":500000000,"source":"akshare"}]"#,
        )
        .unwrap();
    rules.validate().unwrap();
    assert_eq!(
        rules.previous_close(&bars, 1),
        Some(yuan(950)),
        "除息日的锚必须扣掉已装载的每股现金红利"
    );
    assert_eq!(rules.limits(yuan(950)).0, yuan(1_045));
    assert!(rules.blocks_fill(Side::Buy, &bars[1], rules.previous_close(&bars, 1)));
    assert!(
        rules.blocks_fill(Side::Buy, &bars[2], rules.previous_close(&bars, 2)),
        "除息日内的分钟线用同一格折算锚"
    );
    // 旧口径（不复权昨收 10.00）把板推到 11.00，这根封死的线就照样成交。
    assert!(!rules.blocks_fill(Side::Buy, &bars[1], Some(yuan(1_000))));
    // 手工覆盖仍优先于折算：数据侧给出调整后昨收时，以它为准。
    let overridden = AshareRuleConfig {
        previous_close_raw: [(ex_date, yuan(1_000))].into_iter().collect(),
        ..rules.clone()
    };
    overridden.validate().unwrap();
    assert_eq!(overridden.previous_close(&bars, 1), Some(yuan(1_000)));
}

/// 同一除权日上的红利 + 送转 + 配股按 `(昨收 + 配股价×配股比例 − 红利) ÷ (1 + 送转 + 配股)`
/// 一次折算，落到最小报价单位。
#[test]
fn ex_rights_anchor_folds_dividend_bonus_shares_and_rights_issue_together() {
    let yuan = |cents: i128| cents * SCALE / 100;
    let previous_session = date_to_shanghai_midnight_ms("2024-05-31").unwrap();
    let ex_date = date_to_shanghai_midnight_ms("2024-06-03").unwrap();
    let bars = vec![
        Bar::new(
            previous_session,
            yuan(980),
            yuan(1_000),
            yuan(975),
            yuan(1_000),
            1_000,
        ),
        Bar::new(ex_date, yuan(756), yuan(756), yuan(756), yuan(756), 1_000),
    ];
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    rules
        .apply_corporate_actions_json(
            "000001.SZSE",
            r#"[
                {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"cash_dividend","cash_dividend_raw":500000000,"source":"akshare"},
                {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"bonus_share","share_ratio_num":13,"share_ratio_den":10,"source":"akshare"},
                {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"rights_issue","rights_instrument":"700001.SZSE","rights_issue_price_raw":5000000000,"rights_issue_ratio_num":3,"rights_issue_ratio_den":10,"source":"akshare"}
            ]"#,
        )
        .unwrap();
    rules.validate().unwrap();
    // (10.00 + 5.00×0.3 − 0.50) ÷ 1.6 = 6.875 → 落到 0.01 刻度是 6.88。
    assert_eq!(
        rules.previous_close(&bars, 1),
        Some(yuan(688)),
        "同一除权日的三类事实必须一次折算，而不是只读红利"
    );
    assert!(rules.blocks_fill(Side::Buy, &bars[1], rules.previous_close(&bars, 1)));
    // 只折算红利（锚 9.50、板 10.45）时，这根封在 7.56 的线照样成交 —— 送转与配股不能漏。
    assert!(!rules.blocks_fill(Side::Buy, &bars[1], Some(yuan(950))));
}

/// 跨语言对照夹具目录：Python 写侧的产物，Rust 读侧在这里读回。
const CROSS_CHECK_FIXTURE_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../python/tests/fixtures");

fn cross_check_fixture(name: &str) -> serde_json::Value {
    let path = std::path::Path::new(CROSS_CHECK_FIXTURE_DIR).join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("跨语言对照夹具 {name} 必须存在: {error}"))
        .replace("\r\n", "\n");
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("跨语言对照夹具 {name} 必须是合法 JSON: {error}"))
}

fn cross_check_raw(row: &serde_json::Value, key: &str) -> i128 {
    row[key].as_i64().expect("定点整数格") as i128
}

/// V13 R1-A2：Python 写出的 v1 信封必须由 Rust 读侧原样读回，并喂进 A1 那道折算锚。
///
/// 三份文件一条链：`*.rows.json` 是数据源原始行，`*.payload.json` 由 Python 的
/// `serialize_corporate_actions` 写出，`*.expectations.json` 由 payload 派生。两侧各自
/// 读同一份期望值、谁都不抄它（与 V11 R17 的日历指纹夹具同一条设计）：Python 改了字段
/// 名册或单位口径，Rust 这侧就红；Rust 改了读法或除权口径，Python 那侧就红。
#[test]
fn python_written_action_envelope_round_trips_into_the_rust_reader() {
    let expectations = cross_check_fixture("ashare_actions_cross_check.expectations.json");
    let payload = std::fs::read_to_string(
        std::path::Path::new(CROSS_CHECK_FIXTURE_DIR)
            .join("ashare_actions_cross_check.payload.json"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    let document = &expectations["document"];

    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    let report = rules
        .apply_corporate_actions_json_with_report("000001.SZSE", &payload)
        .unwrap();
    rules.validate().unwrap();

    assert_eq!(
        report.schema_version,
        document["schema_version"].as_u64().unwrap() as u32
    );
    assert_eq!(report.source, document["source"].as_str().unwrap());
    assert_eq!(report.instrument, document["instrument"].as_str().unwrap());
    assert!(document["as_of"].is_null(), "夹具约定不带 as_of");
    assert_eq!(report.as_of_ms, None, "无 as_of 时不得凭空造出 PIT 截止");
    assert_eq!(
        report.row_count,
        document["row_count"].as_u64().unwrap() as usize
    );
    assert_eq!(
        report.applied_actions,
        document["applied_actions"].as_u64().unwrap() as usize
    );
    assert_eq!(
        report.hidden_actions,
        document["hidden_actions"].as_u64().unwrap() as usize
    );
    assert_eq!(
        report.halted_timestamps,
        document["halted_days"].as_u64().unwrap() as usize,
        "停牌行必须由 Rust 认成停牌事实，而不是当作一条普通资金事件"
    );
    assert_eq!(
        rules.halted_timestamps,
        vec![
            date_to_shanghai_midnight_ms(expectations["halted_days"][0].as_str().unwrap()).unwrap()
        ],
        "Rust 侧的停牌日必须与 Python 写出的一致"
    );

    let rows = expectations["applied_actions"].as_array().unwrap();
    assert_eq!(rules.corporate_actions.len(), rows.len());
    for (event, row) in rules.corporate_actions.iter().zip(rows) {
        let label = row["action_type"].as_str().unwrap();
        assert_eq!(
            event.ts,
            date_to_shanghai_midnight_ms(row["ex_date"].as_str().unwrap()).unwrap(),
            "{label} 的生效日"
        );
        assert_eq!(
            serde_json::to_value(event.action_type)
                .unwrap()
                .as_str()
                .unwrap(),
            label,
            "{label} 的动作名册在读侧变了"
        );
        assert_eq!(
            event.cash_dividend_raw,
            cross_check_raw(row, "cash_dividend_raw"),
            "{label} 的每股红利"
        );
        assert_eq!(
            event.split_num,
            cross_check_raw(row, "share_ratio_num"),
            "{label} 的送转分子"
        );
        assert_eq!(
            event.split_den,
            cross_check_raw(row, "share_ratio_den"),
            "{label} 的送转分母"
        );
        assert_eq!(
            event.rights_issue_price_raw,
            cross_check_raw(row, "rights_issue_price_raw"),
            "{label} 的配股价"
        );
        assert_eq!(
            event.rights_issue_ratio_num,
            cross_check_raw(row, "rights_issue_ratio_num"),
            "{label} 的配股比例分子"
        );
        assert_eq!(
            event.interest_per_bond_raw,
            cross_check_raw(row, "interest_per_bond_raw"),
            "{label} 的每张债券利息"
        );
        assert_eq!(
            event.issuer_total_shares_raw,
            cross_check_raw(row, "issuer_total_shares_raw"),
            "{label} 的总股本"
        );
        assert_eq!(
            event.source,
            row["source"].as_str().unwrap(),
            "{label} 的血缘来源"
        );
        assert_eq!(
            event.published_at_ms,
            Some(date_to_shanghai_midnight_ms(row["published_at"].as_str().unwrap()).unwrap()),
            "{label} 的 PIT 可见时间"
        );
    }
}

/// 同一份 payload 走 A1 那道折算锚：除权日的参考价口径必须由两侧共读的期望值钉住。
#[test]
fn python_written_actions_fold_into_the_shared_ex_rights_reference() {
    let expectations = cross_check_fixture("ashare_actions_cross_check.expectations.json");
    let payload = std::fs::read_to_string(
        std::path::Path::new(CROSS_CHECK_FIXTURE_DIR)
            .join("ashare_actions_cross_check.payload.json"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    let anchor = &expectations["anchor"];
    let yuan = |cents: i128| cents * SCALE / 100;
    let previous_session = date_to_shanghai_midnight_ms("2024-05-31").unwrap();
    let ex_date = date_to_shanghai_midnight_ms(anchor["ex_date"].as_str().unwrap()).unwrap();
    let previous_close = cross_check_raw(anchor, "previous_close_raw");
    let reference = cross_check_raw(anchor, "expected_reference_raw");
    assert_eq!(previous_close, yuan(1_000));
    assert_eq!(reference, yuan(700));
    let bars = vec![
        Bar::new(
            previous_session,
            previous_close,
            previous_close,
            previous_close,
            previous_close,
            1_000,
        ),
        Bar::new(
            ex_date,
            7_560_000_000,
            7_560_000_000,
            7_560_000_000,
            7_560_000_000,
            1_000,
        ),
    ];
    let mut rules = AshareRuleConfig {
        enabled: true,
        ..AshareRuleConfig::default()
    };
    rules
        .apply_corporate_actions_json("000001.SZSE", &payload)
        .unwrap();
    rules.validate().unwrap();

    assert!(
        rules.previous_close_raw.is_empty(),
        "夹具走的是折算，不是数据侧覆盖"
    );
    assert_eq!(
        rules.previous_close(&bars, 1),
        Some(cross_check_raw(anchor, "expected_reference_raw")),
        "除权日锚定的参考价必须等于两侧共读的那一格期望值"
    );
    // 不折算就是不复权昨收 10.00：这两格必须分开，否则整条对照是空的。
    assert_ne!(rules.previous_close(&bars, 1), Some(previous_close));
    assert!(
        rules.limits(previous_close).0 > rules.limits(reference).0,
        "同一根 Bar 在未折算与折算后的锚下必须落在不同的涨停板上"
    );
}
