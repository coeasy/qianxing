//! 读模型对"产物里到底有没有这一格"的单一读法（V12 R1）。
//!
//! 写侧已经收口三轮：没算过的钱不落键而不是落 0（V11 Q67/Q68/Q70）、本金没声明就不印本金
//! （Q72）。本模块管的是**读的那一半**：`report` 与 `status` 念一份摘要时，缺键必须说"缺席"，
//! 不得印一个合法的 `0` 冒充"算过了、结果是零"。
//!
//! 为什么值得单独一个文件：这条规则只有三行代码，但它约束的是**任何**把摘要格子印出来的地方，
//! 而 `config_commands.rs` 已登记的行数预算只降不升（`maturity/line_budgets.yaml`）。
//! 读法住在这里，两个命令共用一份，门禁才能拿"只有一处 `unwrap_or(0)` 形状"当判据。

use serde_json::Value;

/// 一格没写数时印出的词。它必须与 `0`（算出来是零）和空串（算出来是空）都可区分。
pub(crate) const READOUT_ABSENT: &str = "absent";

/// 摘要有这个块吗（`input` 自 v3、`account` 自 v4）。用于把"这个世代的产物没声明过"说出来，
/// 而不是把块里的每一格各自念成缺席——前者是用者需要的信息，后者会让人以为核对过却全空。
pub(crate) fn summary_block<'a>(summary: &'a Value, key: &str) -> Option<&'a Value> {
    summary.get(key).filter(|value| !value.is_null())
}

/// 摘要落盘的 `schema_version`；缺这个键的产物属于"世代未知"，读侧要如实报未知。
pub(crate) fn summary_schema_version(summary: &Value) -> Option<i64> {
    summary.pointer("/schema_version").and_then(Value::as_i64)
}

/// 一格的数值读法：`Some` 才是"这格真写了数"。
///
/// 为什么要同时接字符串：i128 的定点钱在摘要里按字符串落盘（见 `account.initial_cash_raw`），
/// 只认 JSON number 会把一个真实存在的数读成缺席——那是另一种骗人。
pub(crate) fn summary_number(summary: &Value, pointer: &str) -> Option<i128> {
    match summary.pointer(pointer)? {
        Value::Null => None,
        Value::String(text) => text.trim().parse::<i128>().ok(),
        value @ Value::Number(_) => value.to_string().parse::<i128>().ok(),
        _ => None,
    }
}

/// 一格的文本读法。
pub(crate) fn summary_text(summary: &Value, pointer: &str) -> Option<String> {
    match summary.pointer(pointer)? {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        value @ Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

/// 缺席的统一排版：`None` 一律印 [`READOUT_ABSENT`]，绝不落回任何合法取值。
pub(crate) fn render_number(value: Option<i128>) -> String {
    value.map_or_else(|| READOUT_ABSENT.to_string(), |number| number.to_string())
}

/// 文本格的排版同 [`render_number`]；空串按缺席处理（摘要里没有任何一格该用空串表达结论）。
pub(crate) fn render_text(value: Option<String>) -> String {
    value
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| READOUT_ABSENT.to_string())
}

/// 把"产物是哪一代写的、这一代起才有哪几块"印成一行，供人判断该期望哪些格存在。
pub(crate) fn summary_generation_note(summary: &Value) -> String {
    let generation = match summary_schema_version(summary) {
        Some(version) => version.to_string(),
        None => READOUT_ABSENT.to_string(),
    };
    let mut blocks = Vec::new();
    for (key, since) in [("input", 3), ("account", 4), ("replay", 2)] {
        let declared = summary_block(summary, key).is_some();
        let expected = match summary_schema_version(summary) {
            Some(version) => version >= since,
            None => false,
        };
        blocks.push(format!(
            "{key}={}",
            match (declared, expected) {
                (true, _) => "declared".to_string(),
                (false, true) => "MISSING_IN_THIS_GENERATION".to_string(),
                (false, false) => format!("not_declared_before_v{since}"),
            }
        ));
    }
    format!("schema_version={generation} {}", blocks.join(" "))
}

/// `report` 的人读正文（首行 `[Report] summary=` 由调用方印，它带的是路径而非摘要格子）。
///
/// `input_verified` 是调用方按"是否按摘要声明的输入重算过"得到的结论串：排版在这里，
/// 事实来自那里，二者不共用一个函数才不会让"核对过"与"念出来"两件事互相顶替。
pub(crate) fn report_readout_lines(summary: &Value, input_verified: &str) -> Vec<String> {
    vec![
        format!("  {}", summary_generation_note(summary)),
        format!(
            "  strategy={} instrument={} bars={} fills={}",
            render_text(summary_text(summary, "/strategy_id")),
            render_text(summary_text(summary, "/instrument")),
            render_number(summary_number(summary, "/bars")),
            render_number(summary_number(summary, "/fills"))
        ),
        format!(
            "  return_bps={} max_drawdown_bps={} fees_raw={} turnover_raw={} final_equity_raw={}",
            render_number(summary_number(summary, "/metrics/return_bps")),
            render_number(summary_number(summary, "/metrics/max_drawdown_bps")),
            render_number(summary_number(summary, "/metrics/fees_raw")),
            render_number(summary_number(summary, "/metrics/turnover_raw")),
            render_number(summary_number(summary, "/metrics/final_equity_raw"))
        ),
        format!(
            "  input_data_hash={} result_hash={} replay_log_digest={}",
            render_text(summary_text(summary, "/input_data_hash")),
            render_text(summary_text(summary, "/result_hash")),
            render_text(summary_text(summary, "/replay/log_digest"))
        ),
        format!("  input_verified={input_verified}"),
        format!(
            "  account_initial_cash_raw={} account_source={}",
            render_number(summary_number(summary, "/account/initial_cash_raw")),
            render_text(summary_text(summary, "/account/source"))
        ),
        format!(
            "  replay_events={} replay_ledger_entries={}/{}",
            render_number(summary_number(summary, "/replay/events")),
            render_number(summary_number(summary, "/replay/ledger_entries")),
            render_number(summary_number(summary, "/replay/run_ledger_entries"))
        ),
        format!(
            "  risk_rule_set_version={}",
            render_text(summary_text(summary, "/risk_rules/rule_set_version"))
        ),
    ]
}

/// `status` 的 `[Latest Backtest]` 正文：只念人一眼要挑出的那几格，其余留给 `report`。
pub(crate) fn latest_backtest_readout_lines(summary: &Value) -> Vec<String> {
    vec![
        format!("[Latest Backtest] {}", summary_generation_note(summary)),
        format!(
            "  strategy={} instrument={} fills={} return_bps={} max_drawdown_bps={} result_hash={}",
            render_text(summary_text(summary, "/strategy_id")),
            render_text(summary_text(summary, "/instrument")),
            render_number(summary_number(summary, "/fills")),
            render_number(summary_number(summary, "/metrics/return_bps")),
            render_number(summary_number(summary, "/metrics/max_drawdown_bps")),
            render_text(summary_text(summary, "/result_hash"))
        ),
    ]
}
