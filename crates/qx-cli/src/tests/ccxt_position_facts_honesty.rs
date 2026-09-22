//! 交易所持仓回报的方向与钱字段必须诚实（V11 Q68，交易链路 live 侧）。
//!
//! `contracts_raw` 是无符号张数，正负全靠 `side`；连接器在交易所没给方向时填
//! `"unknown"`，而此前读侧把它（连同字段缺失）一律当成多头，于是空头仓位以正数量
//! 进入事实流，符号错误顺着权益、保证金、风控一路算下去。同一份回报里的
//! `unrealizedPnl/initialMargin/maintenanceMargin` 此前省略即读成 0，让"交易所没报"
//! 变成"这仓位没有浮亏、没占保证金"并发布出去。这里钉住：方向不明只能拒收，
//! 钱字段没报必须仍是没报。

use super::*;

/// 一份最小的多头持仓行；用例按需增删键，避免每个用例重复整套字段。
fn position_row() -> serde_json::Value {
    serde_json::json!({
        "symbol": "BTC/USDT:USDT",
        "side": "long",
        "contracts_raw": "2000000000",
        "entry_price_raw": 100000000000_i64,
        "mark_price_raw": 99000000000_i64,
        "unrealized_pnl_raw": 2000000000_i64,
        "initial_margin_raw": 10000000000_i64,
        "maintenance_margin_raw": 5000000000_i64,
    })
}

fn facts_for(rows: Vec<serde_json::Value>) -> Result<Vec<AccountPositionSnapshot>, String> {
    ccxt_position_facts(&serde_json::json!({ "positions": rows }), "OKX")
}

/// 缺 `side` 与连接器占位的 `"unknown"` 都不能再靠猜定符号；明确的方向才改变结果。
#[test]
fn unknown_position_side_is_refused_instead_of_guessed_as_long() {
    let missing = {
        let mut row = position_row();
        row.as_object_mut().unwrap().remove("side");
        row
    };
    let placeholder = {
        let mut row = position_row();
        row["side"] = serde_json::Value::String("unknown".into());
        row
    };
    for (name, row) in [("缺字段", &missing), ("连接器占位", &placeholder)] {
        let error = facts_for(vec![row.clone()]).expect_err("{name} 的持仓方向不明时必须拒收");
        assert!(
            error.contains("side") && error.contains("BTC/USDT:USDT"),
            "{name} 的报错必须点名 side 与标的: {error}"
        );
        if name == "连接器占位" {
            assert!(
                error.contains("unknown"),
                "占位值必须原样出现在报错里，否则排查时看不到读到了什么: {error}"
            );
        }
    }

    // 对照组：同一行的方向换成可识别值后结果确实变化，且符号来自 side 而不是猜测。
    let short = {
        let mut row = position_row();
        row["side"] = serde_json::Value::String("SHORT".into());
        row
    };
    let facts = facts_for(vec![short]).expect("明确的空头方向必须被收下");
    assert_eq!(facts.len(), 1);
    assert_eq!(
        facts[0].quantity.raw(),
        -2_000_000_000,
        "空头方向决定数量符号"
    );
}

/// 交易所对已平仓位通常不给方向；这类零数量行必须在方向闸门之前就被跳过，
/// 否则一条 fail-closed 会把整个账户的持仓快照一起打死。
#[test]
fn flat_position_without_side_is_skipped_before_the_direction_gate() {
    let flat = serde_json::json!({
        "symbol": "ETH/USDT:USDT",
        "contracts_raw": 0,
    });
    let facts = facts_for(vec![flat, position_row()]).expect("平仓位不该触发方向闸门");
    assert_eq!(facts.len(), 1, "只应留下那一笔有仓位的行");
    assert_eq!(facts[0].instrument.to_string(), "BTC/USDT:USDT.OKX");
}

/// 三个钱字段：省略、`null`（连接器对未报字段的写法）都是"没报"；报出来的 0 才是 0。
#[test]
fn unreported_position_money_stays_unreported_and_zero_stays_zero() {
    const MONEY_KEYS: [&str; 3] = [
        "unrealized_pnl_raw",
        "initial_margin_raw",
        "maintenance_margin_raw",
    ];
    let omitted = {
        let mut row = position_row();
        for key in MONEY_KEYS {
            assert!(
                row.as_object_mut().unwrap().remove(key).is_some(),
                "夹具必须真的带 {key}，否则这条用例没在测省略"
            );
        }
        row
    };
    let nulled = {
        let mut row = position_row();
        for key in MONEY_KEYS {
            row[key] = serde_json::Value::Null;
        }
        row
    };
    for (name, row) in [("省略", &omitted), ("显式 null", &nulled)] {
        let facts = facts_for(vec![row.clone()]).unwrap();
        for key in MONEY_KEYS {
            let value = match key {
                "unrealized_pnl_raw" => facts[0].unrealized_pnl,
                "initial_margin_raw" => facts[0].initial_margin,
                _ => facts[0].maintenance_margin,
            };
            assert_eq!(value, None, "{name} 的 {key} 必须读成\"没报\"而不是 0");
        }
        // 数量与价格照常 transfer：没报钱不等于这仓位不存在。
        assert_eq!(facts[0].quantity.raw(), 2_000_000_000);
        assert_eq!(facts[0].mark_price, Some(Price::from_raw(99_000_000_000)));
    }

    // 交易所明确报零（含字符串写法）必须留在 Some(0)，且与"没报"不是同一份事实。
    let reported_zero = {
        let mut row = position_row();
        row["unrealized_pnl_raw"] = serde_json::Value::String("0".into());
        row["initial_margin_raw"] = serde_json::json!(0);
        row["maintenance_margin_raw"] = serde_json::json!(0);
        row
    };
    let zero = &facts_for(vec![reported_zero]).unwrap()[0];
    for value in [
        zero.unrealized_pnl,
        zero.initial_margin,
        zero.maintenance_margin,
    ] {
        assert_eq!(value, Some(Money::ZERO), "报出来的零不能被抹成没报");
    }
    let unreported = &facts_for(vec![omitted]).unwrap()[0];
    assert_ne!(
        unreported, zero,
        "\"没报\" 与 \"报了且为零\" 在事实上必须是两份不同内容"
    );
}
