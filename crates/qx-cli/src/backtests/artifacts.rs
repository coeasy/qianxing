//! 回测产物落盘：清单、工件与 `BacktestArtifacts` 载体。

use super::*;

/// Bar 与 L1/L2 深度回测共用的产物输入；两套引擎写同一组摘要、曲线和成交文件。
pub(crate) struct BacktestArtifacts<'a> {
    pub(crate) strategy_id: &'a str,
    pub(crate) instrument: &'a InstrumentId,
    pub(crate) sample_unit: &'static str,
    pub(crate) samples: usize,
    pub(crate) sample_ts: &'a [u64],
    pub(crate) equity: &'a [i128],
    pub(crate) positions: &'a [i128],
    pub(crate) fills: &'a [qx_core::Fill],
    pub(crate) clock_start: u64,
    pub(crate) clock_end: u64,
    pub(crate) input_data_hash: u64,
    pub(crate) result_hash: u64,
    pub(crate) replay_hash: u64,
    pub(crate) return_bps: i32,
    pub(crate) max_drawdown_bps: u32,
    pub(crate) fees_raw: i128,
    pub(crate) turnover_raw: i128,
    pub(crate) final_equity_raw: i128,
    pub(crate) assumptions: &'a [String],
    pub(crate) model_descriptors: &'a [String],
    pub(crate) risk_rule_set_version: &'a str,
    /// 规则集来源：`runtime-config` 表示吃了 `strategy.risk_rules`，`conservative-default`
    /// 表示该入口没有给出配置文件。产物必须能区分这两者（V10 §4.2）。
    pub(crate) risk_rule_source: &'static str,
    /// 执行成本口径的来源（V11 Q0c）：`cost-rules-file:<路径>` / `runtime-config-default` /
    /// `builtin-default` / `cli-flag` / `ashare-rules`。费率数值本身已经在
    /// `model_descriptors` 里，这里只回答"这组数字从哪来"，让"没配"与"配了同样的值"可区分。
    pub(crate) cost_source: &'a str,
    /// Bar 链的撮合模型与其来源（V11 Q1a 第二批）：`(配置名, 来源)`。模型本身已经在
    /// `model_descriptors` 里（那份是内核自述，带参数与假设），这里补的是"这个口径是
    /// 配置里声明的还是没人提"——与 `execution_costs.source` 同一条理由。
    /// 深度链的 Tick/OrderBook 内核不经过 `FillModel`，所以它是 `None`：没有的东西
    /// 不该在摘要里占一个键。
    pub(crate) fill_model: Option<(&'static str, &'static str)>,
    /// 本次实际使用的撮合内核；深度档不得声称与 Bar 链同一内核。
    pub(crate) matching_kernel: &'static str,
    /// 引擎挡下的委托，按 `(原因, 次数)` 降序。只看 `fills` 分不出"策略没发信号"与
    /// "信号全被风控或现金挡下"，这两件事对使用者的含义相反。
    pub(crate) rejections: &'a [(String, usize)],
}

/// 从引擎事件日志归并拒单原因与次数；三条回测链共用这一份口径。
pub(crate) fn rejection_facts(event_log: &qx_core::EventLog) -> Vec<(String, usize)> {
    let reasons: Vec<String> = event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            qx_core::EventKind::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    let mut grouped: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for reason in reasons {
        *grouped.entry(reason).or_default() += 1;
    }
    let mut pairs: Vec<(String, usize)> = grouped.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

/// 拒单原因的单行写法：`次数x原因`（原因里的空格换成 `_`），无拒单时固定为 `none`。
/// 三条回测链的 CLI 输出与解析共用这一份语法。
pub(crate) fn rejection_facts_line(rejections: &[(String, usize)]) -> String {
    let reasons = rejections
        .iter()
        .map(|(reason, count)| format!("{count}x{}", reason.replace(' ', "_")))
        .collect::<Vec<_>>()
        .join(";");
    if reasons.is_empty() {
        return "none".to_string();
    }
    reasons
}

/// 归并后的拒单总数，与 `rejection_facts_line` 共用同一份事实。
pub(crate) fn rejection_count(rejections: &[(String, usize)]) -> usize {
    rejections.iter().map(|(_, count)| count).sum()
}

pub(crate) fn persist_backtest_artifacts(
    manifest_path: &Path,
    input: &BacktestArtifacts<'_>,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let stem = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backtest.run.json")
        .strip_suffix(".run.json")
        .unwrap_or("backtest");
    let root = manifest_path
        .parent()
        .ok_or_else(|| "回测 RunManifest 缺少父目录".to_string())?;
    let summary_path = root.join(format!("{stem}.summary.json"));
    let equity_path = root.join(format!("{stem}.equity.csv"));
    let fills_path = root.join(format!("{stem}.fills.csv"));
    let mut summary = serde_json::json!({
        "schema_version": 1,
        "strategy_id": input.strategy_id,
        "instrument": input.instrument.to_string(),
        "bars": input.samples,
        "sample_unit": input.sample_unit,
        "fills": input.fills.len(),
        "rejected_orders": rejection_count(input.rejections),
        "rejection_reasons": input
            .rejections
            .iter()
            .map(|(reason, count)| serde_json::json!({ "reason": reason, "count": count }))
            .collect::<Vec<_>>(),
        "clock_start": input.clock_start,
        "clock_end": input.clock_end,
        "input_data_hash": format!("{:016x}", input.input_data_hash),
        "result_hash": format!("{:016x}", input.result_hash),
        "replay_hash": format!("{:016x}", input.replay_hash),
        "metrics": {
            "return_bps": input.return_bps,
            "max_drawdown_bps": input.max_drawdown_bps,
            "fees_raw": input.fees_raw,
            "turnover_raw": input.turnover_raw,
            "final_equity_raw": input.final_equity_raw,
        },
        "assumptions": input.assumptions,
        "model_descriptors": input.model_descriptors,
        "matching_kernel": input.matching_kernel,
        "risk_rules": {
            "rule_set_version": input.risk_rule_set_version,
            "source": input.risk_rule_source,
        },
        "execution_costs": { "source": input.cost_source },
        "run_manifest": manifest_path.to_string_lossy(),
    });
    if let Some((name, source)) = input.fill_model {
        // 只在真的有 `FillModel` 的链上写这个键：深度链的撮合口径在 `model_descriptors`
        // 的四参数描述子里，硬塞一个 "fill_model": null 等于给摘要添一个没人能填的格子。
        summary["fill_model"] = serde_json::json!({ "name": name, "source": source });
    }
    let summary_payload = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("编码回测摘要失败: {error}"))?;
    write_backtest_artifact(&summary_path, &summary_payload, "回测摘要")?;

    let mut equity_payload = String::from("index,ts,equity_raw,position_raw\n");
    for (index, equity) in input.equity.iter().enumerate() {
        let ts = input.sample_ts.get(index).copied().unwrap_or_default();
        let position = input.positions.get(index).copied().unwrap_or_default();
        equity_payload.push_str(&format!("{index},{ts},{equity},{position}\n"));
    }
    write_backtest_artifact(&equity_path, &equity_payload, "权益曲线")?;

    let mut fills_payload = String::from(
        "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id\n",
    );
    for fill in input.fills {
        fills_payload.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            fill.order_id,
            fill.ts,
            fill.qty.raw(),
            fill.price.raw(),
            fill.fee.raw(),
            fill.account_id,
            fill.strategy_id.as_deref().unwrap_or_default(),
            fill.signal_id
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fill.intent_id
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fill.venue_id.as_deref().unwrap_or_default(),
            fill.venue_order_id.as_deref().unwrap_or_default(),
        ));
    }
    write_backtest_artifact(&fills_path, &fills_payload, "成交明细")?;
    Ok((summary_path, equity_path, fills_path))
}
