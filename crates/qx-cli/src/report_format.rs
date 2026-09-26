//! 回测摘要的人读报告排版：唯一一处定义"这一格没有时念什么"。
//!
//! 拆出 `config_commands.rs` 不是为了行数，而是因为这几行输出是旧产物的唯一翻译层：
//! 摘要里少一格，报告就必须说出"没作过声明"，不能替读者补一个看起来像结论的 0。

/// 摘要里没有这一格时念出来的标记。它必须和"这一格是 0"不同字，否则旧产物会被读成
/// 做过它没做过的声明。
pub(crate) const NOT_DECLARED: &str = "not_declared";

/// 摘要里的一个数值格：缺失与 null 一律念 `not_declared`，真 0 照旧念 0。
///
/// `report status` 与 `report` 命令共用这一条判定，否则同一个缺失字段在一个命令里诚实、
/// 在另一个里被印成 0（V11 D 轮 S6 在 `[Latest Backtest]` 行上的另一半）。
pub(crate) fn summary_number(summary: &serde_json::Value, pointer: &str) -> String {
    match summary.pointer(pointer) {
        Some(value) if value.is_null() => NOT_DECLARED.to_string(),
        Some(value) => value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
            .map(|parsed| parsed.to_string())
            .or_else(|| value.as_str().map(str::to_string))
            .unwrap_or_else(|| value.to_string()),
        None => NOT_DECLARED.to_string(),
    }
}

/// 摘要里的一个字符串格：缺失时念调用方给的口径，而不是空串。
pub(crate) fn summary_text(summary: &serde_json::Value, pointer: &str, fallback: &str) -> String {
    summary
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

/// 文本报告的那几行摘要读数，措辞与"缺失怎么念"只在这里定义一次。
///
/// 抽出来是为了让"没有这一格"能当成断言对象：摘要里的每个数字都可能是"这轮真算出来的 0"，
/// 也可能是"这份产物压根没写过这一格"。把后者念成 0 等于替旧产物做出一句它没做过的安全声明
/// —— 仓库自带的 16 份 v1 摘要全无 `replay` 块，报告却一直在印 `replay_events=0`，读者只会
/// 读出"重放过、0 个事件"（V11 D 轮 S6）。
pub(crate) fn backtest_report_lines(summary: &serde_json::Value, input_line: &str) -> Vec<String> {
    // 重放是三件事（日志摘要、吞下的事件数、重建出的账簿条数），缺整块时印三个 0 会被读成
    // "重放通过、0 个事件"。这里除了逐格 not_declared，再补一句这块究竟在不在。
    let replay_note = if summary.get("replay").is_some() {
        ""
    } else {
        "（该摘要没有 replay 块，重放结论未经核对）"
    };
    let number = |pointer: &str| summary_number(summary, pointer);
    let text = |pointer: &str, fallback: &str| summary_text(summary, pointer, fallback);
    vec![
        format!(
            "  strategy={} instrument={} bars={} sample_unit={} fills={}",
            text("/strategy_id", "-"),
            text("/instrument", "-"),
            number("/bars"),
            text("/sample_unit", NOT_DECLARED),
            number("/fills")
        ),
        format!(
            "  return_bps={} max_drawdown_bps={} fees_raw={} turnover_raw={} final_equity_raw={}",
            number("/metrics/return_bps"),
            number("/metrics/max_drawdown_bps"),
            number("/metrics/fees_raw"),
            number("/metrics/turnover_raw"),
            number("/metrics/final_equity_raw")
        ),
        format!(
            "  input_data_hash={} result_hash={} replay_log_digest={}",
            text("/input_data_hash", "-"),
            text("/result_hash", "-"),
            text("/replay/log_digest", "-")
        ),
        format!("  {input_line}"),
        format!(
            "  replay_events={} replay_ledger_entries={}/{}{}",
            number("/replay/events"),
            number("/replay/ledger_entries"),
            number("/replay/run_ledger_entries"),
            replay_note
        ),
    ]
}
