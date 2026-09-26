//! 读侧对"摘要里到底有没有这一格"的排版（V12 R1）。
//!
//! 写侧三轮（Q67/Q68/Q70/Q72）已经保证"没算过的钱不落键"；本文件管的是念出来的那一半：
//! 缺键必须印 `absent`，而**声明过的 0 必须仍然印 0**。两者只有在同一份断言里成对出现，
//! 才证明这条规则区分的是"没数"与"数是零"，而不是把空值一律换成另一个字面量。

use super::*;

fn joined(lines: &[String]) -> String {
    lines.join("\n")
}

/// 一份"只有 account 块、且里面没写本金"的 v4 摘要：除刻意保留的格子外全都缺席。
fn sparse_v4_summary() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 4,
        "account": { "source": "runtime-default" },
        "metrics": { "fees_raw": "0" },
    })
}

#[test]
fn absent_keys_print_absent_while_declared_zero_still_prints_zero() {
    let lines = report_readout_lines(&sparse_v4_summary(), "not_declared");
    let text = joined(&lines);
    for expected in [
        "bars=absent",
        "fills=absent",
        "return_bps=absent",
        "max_drawdown_bps=absent",
        "turnover_raw=absent",
        "final_equity_raw=absent",
        "account_initial_cash_raw=absent",
        "replay_events=absent",
        "result_hash=absent",
        "risk_rule_set_version=absent",
    ] {
        assert!(text.contains(expected), "缺席格子被印成了合法值: {text}");
    }
    // 同一行里写过的 0 不许被顺带改成 absent：那是把"算出来是零"抹成"没算"。
    assert!(
        text.contains("fees_raw=0"),
        "声明过的零成本被误报为缺席: {text}"
    );
    assert!(
        text.contains("account_source=runtime-default"),
        "有值的文本格丢了内容: {text}"
    );
}

#[test]
fn string_typed_and_negative_money_fields_still_read_as_numbers() {
    // 定点钱按字符串落盘（i128 超出 JSON number 的安全范围）；只认 number 会把真值读成缺席。
    let summary = serde_json::json!({
        "schema_version": 4,
        "account": { "initial_cash_raw": "100000000000000", "source": "declared" },
        "metrics": { "return_bps": -123, "turnover_raw": 385200000000000i64 },
    });
    let text = joined(&report_readout_lines(&summary, "verified"));
    assert!(
        text.contains("account_initial_cash_raw=100000000000000"),
        "字符串型本金没读出来: {text}"
    );
    assert!(text.contains("return_bps=-123"), "负收益被读成缺席: {text}");
    assert!(
        text.contains("turnover_raw=385200000000000"),
        "成交额没读出来: {text}"
    );
    // 非数字文本格（例如被写成对象）走 absent，而不是 panic。
    let broken = serde_json::json!({"schema_version": 4, "bars": {"nested": 1}});
    assert!(
        joined(&report_readout_lines(&broken, "x")).contains("bars=absent"),
        "形状不对的格子必须按缺席处理"
    );
}

#[test]
fn generation_note_tells_apart_older_schema_from_missing_block() {
    // v1：三个块都还没被引入，缺席是"那一代没声明过"。
    let v1 = serde_json::json!({"schema_version": 1, "strategy_id": "s"});
    let note = summary_generation_note(&v1);
    assert!(
        note.contains("input=not_declared_before_v3")
            && note.contains("account=not_declared_before_v4")
            && note.contains("replay=not_declared_before_v2"),
        "旧世代的缺席要说成世代没有这一代: {note}"
    );
    // v4：account 块本该存在却不在，那是产物缺格，要指名道姓。
    let v4_without_account = serde_json::json!({"schema_version": 4});
    let note = summary_generation_note(&v4_without_account);
    assert!(
        note.contains("account=MISSING_IN_THIS_GENERATION")
            && note.contains("input=MISSING_IN_THIS_GENERATION"),
        "本世代缺块没被点名: {note}"
    );
    // 连世代都没写的产物：报 absent，而不是默认自己就是最新世代。
    let note = summary_generation_note(&serde_json::json!({"strategy_id": "s"}));
    assert!(
        note.starts_with("schema_version=absent")
            && note.contains("account=not_declared_before_v4"),
        "世代未知时不能假定块存在: {note}"
    );
    // 块写了但整块是 null：等同没写。
    let note = summary_generation_note(
        &serde_json::json!({"schema_version": 4, "account": null, "input": {}, "replay": {}}),
    );
    assert!(
        note.contains("account=MISSING_IN_THIS_GENERATION")
            && note.contains("input=declared")
            && note.contains("replay=declared"),
        "null 块与空块应分别按缺席/在场处理: {note}"
    );
}

#[test]
fn status_latest_backtest_lines_use_the_same_absent_wording() {
    let lines = latest_backtest_readout_lines(&sparse_v4_summary());
    assert!(
        lines[0].starts_with("[Latest Backtest] schema_version=4"),
        "status 要先报产物世代: {:?}",
        lines[0]
    );
    let text = joined(&lines);
    assert!(
        text.contains("fills=absent")
            && text.contains("return_bps=absent")
            && text.contains("max_drawdown_bps=absent")
            && text.contains("result_hash=absent"),
        "status 把缺席印成了 0 或 -: {text}"
    );
    // 反向：写过的 0 仍然是 0。
    let declared = serde_json::json!({
        "schema_version": 4,
        "fills": 0,
        "metrics": { "return_bps": 0, "max_drawdown_bps": 0 },
        "result_hash": "abc",
    });
    let text = joined(&latest_backtest_readout_lines(&declared));
    assert!(
        text.contains("fills=0")
            && text.contains("return_bps=0")
            && text.contains("max_drawdown_bps=0")
            && text.contains("result_hash=abc"),
        "声明过的零成交/零收益被改写了: {text}"
    );
}

/// `input_verified=` 是整份报告里唯一回答"这份产物的输入有没有按声明重算过"的一格。
/// 它排在哈希那一行之后，删掉它不会让任何数值断言变红——读者只会看到少了一栏，
/// 而看不到"没复核"这句话，所以这一格的存在与内容都要当场问一次。
#[test]
fn readout_gives_the_recompute_verdict_its_own_field_verbatim() {
    for verdict in [
        "not_declared（该摘要没有 input 块，输入身份未经核对）",
        "/data/declared.bar-frame.json input_kind=barframe input_id=strategy-bars:BTC-USDT.BINANCE input_fingerprint=0123456789abcdef",
    ] {
        let text = joined(&report_readout_lines(&sparse_v4_summary(), verdict));
        assert!(
            text.contains(&format!("input_verified={verdict}")),
            "复核结论必须原样占一格，实际: {text}"
        );
    }
}

#[test]
fn multi_leg_cost_bps_reports_no_denominator_and_refuses_an_unprintable_ratio() {
    // 换手为 0：没有分母，是算不出，不是"成本为零"。
    assert_eq!(multi_leg_cost_bps(0, 0).unwrap(), None);
    assert_eq!(multi_leg_cost_bps(500_000_000_000, 0).unwrap(), None);
    // 有分母时按万分比取整（385.2e12 成交额、5bp 费用 → 5）。
    assert_eq!(
        multi_leg_cost_bps(192_600_000_000, 385_200_000_000_000).unwrap(),
        Some(5)
    );
    // 越界不能夹到 i64::MAX：那会被读成一次真实测得的成本。
    let error = multi_leg_cost_bps(i128::MAX, 1).expect_err("超出可印范围必须失败");
    assert!(
        error.contains("超出 i64 可印范围") && error.contains("ratio_raw="),
        "越界文案要点名数值: {error}"
    );
}
