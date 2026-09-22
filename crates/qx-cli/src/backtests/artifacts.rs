//! 回测产物落盘：清单、工件与 `BacktestArtifacts` 载体。

use super::*;

/// 一次回测实际消费的输入身份（V11 Q66 / Q1b 第一批）。
///
/// 摘要此前只有 `input_data_hash`——引擎对自己手里那段 `&[Bar]` 的自哈希（`qx-guanxing` 的
/// `bars_digest` 只吃条数与 ts/OHLCV，连 instrument 都不看）。于是"这份产物跑的是哪一份数据"在
/// 产物里没有任何可核对的答案：策略链那个真被数据集注册表复核过的 `DatasetManifest`（带
/// dataset_id / version / source / schema / 范围 / 指纹）只印在 stdout 上就丢了，输入文件的路径
/// 也从不落盘。事后篡改那份 frame，拿着旧产物检不出来，Q1b 的两条用例（篡改即拒、旧产物可检出
/// 不一致）都因此落不了地。
#[derive(Clone, Debug)]
pub(crate) struct BacktestInputProvenance {
    /// 这一档输入按哪种形状被读成数据集：`barframe` / `depth-frame`。
    pub(crate) kind: &'static str,
    /// 运行时读取的那个文件的路径，复核时按它重读同一份输入。
    pub(crate) path: String,
    pub(crate) dataset_id: String,
    pub(crate) dataset_version: String,
    /// 内容指纹：策略链取自注册表已复核过的 `DatasetManifest::fingerprint`，深度链取自
    /// `DepthFrame::input_hash()`。落盘写的与复核重算的只能出自下面那两个读点。
    pub(crate) fingerprint: String,
}

/// BarFrame 文件的**唯一**入口读点：`strategy backtest` 与 `qx report` 复核走同一个函数，
/// 所以"当时读成的形状"与"事后重读成的形状"不可能被写成两件事。
pub(crate) fn read_bar_frame_for_backtest(path: &Path) -> Result<BarFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取策略回测 BarFrame 失败 {}: {error}", path.display()))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("策略回测 BarFrame 校验失败 {}: {error:?}", path.display()))
}

/// BarFrame → 数据集身份的**唯一**读点：`strategy backtest` 用它登记与复核，`qx report` 用它把
/// 产物上声明的指纹重算一遍。两处各解一遍 JSON 时，"声明的输入"与"重算的输入"会读成两个答案。
pub(crate) fn barframe_dataset_identity(
    frame_path: &Path,
    frame: &BarFrame,
) -> Result<(Vec<Bar>, qx_data::DatasetManifest), String> {
    let bars: Vec<Bar> = frame.into();
    let provider =
        JsonBarFrameProvider::new(frame.source.0.clone(), BARFRAME_DATASET_VERSION, frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{}", frame.instrument),
        &frame.instrument.to_string(),
        bars.first().map(|bar| bar.ts).unwrap_or(1),
        bars.last().map(|bar| bar.ts).unwrap_or(1),
    )?;
    if provider_bars.len() != bars.len()
        || provider_bars.iter().zip(&bars).any(|(left, right)| {
            left.timestamp != right.ts
                || left.open_raw != right.open
                || left.high_raw != right.high
                || left.low_raw != right.low
                || left.close_raw != right.close
                || left.volume_raw != right.volume
        })
    {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝开始回测".into());
    }
    Ok((bars, manifest))
}

/// 深度帧的**唯一**读点，理由同 [`barframe_dataset_identity`]。
pub(crate) fn read_depth_frame_for_backtest(path: &Path) -> Result<DepthFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取深度数据帧失败 {}: {error}", path.display()))?;
    DepthFrame::from_json(&payload).map_err(|error| format!("{}: {error}", path.display()))
}

/// 策略链那一档输入的身份：BarFrame 文件 + 注册表用过的指纹。
pub(crate) fn barframe_input_provenance(
    frame_path: &Path,
    manifest: &qx_data::DatasetManifest,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "barframe",
        path: frame_path.to_string_lossy().into_owned(),
        dataset_id: manifest.dataset_id.clone(),
        dataset_version: manifest.version.clone(),
        fingerprint: manifest.fingerprint.clone(),
    }
}

/// 深度链那一档输入的身份。它不进数据集注册表（那套 key 是 `strategy-bars:<instrument>`），
/// 所以 id 与 version 由这一档输入自己的形状命名。
pub(crate) fn depth_frame_input_provenance(
    frame_path: &Path,
    frame: &DepthFrame,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "depth-frame",
        path: frame_path.to_string_lossy().into_owned(),
        dataset_id: format!("depth-bars:{}", frame.instrument),
        dataset_version: DEPTH_FRAME_DATASET_VERSION.to_string(),
        fingerprint: format!("{:016x}", frame.input_hash()),
    }
}

/// `input` 块在摘要里的唯一写法。
fn input_provenance_json(input: &BacktestInputProvenance) -> serde_json::Value {
    serde_json::json!({
        "kind": input.kind,
        "path": input.path,
        "dataset_id": input.dataset_id,
        "dataset_version": input.dataset_version,
        "fingerprint": input.fingerprint,
    })
}

/// 复核一份回测摘要声明的输入：按它写的路径重读同一份文件、走**同一个读点**重算指纹，再逐字段
/// 比对。声明与实况不符就报错，这就是 Q1b 要的那条"真会失败"的检查。
///
/// 返回 `None` 只有一种情况：这份摘要根本没写 `input` 块（旧 schema，或本来就不落产物的入口）。
/// 调用方不得把它说成"已核对"。
pub(crate) fn recompute_declared_backtest_input(
    summary: &serde_json::Value,
) -> Result<Option<BacktestInputProvenance>, String> {
    let Some(declared) = summary.get("input") else {
        return Ok(None);
    };
    let field = |name: &str| -> Result<String, String> {
        declared
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(|value| value.to_string())
            .ok_or_else(|| format!("回测摘要的 input 块缺 {name}"))
    };
    // 先把声明读全，再按声明的路径重读：一块连字段都不全的摘要，报"缺 X"比报"文件读不到"
    // 更贴近读者要修的那件事。
    let kind = match field("kind")?.as_str() {
        "barframe" => "barframe",
        "depth-frame" => "depth-frame",
        other => return Err(format!("未知的回测输入种类: {other}")),
    };
    let declared = BacktestInputProvenance {
        kind,
        path: field("path")?,
        dataset_id: field("dataset_id")?,
        dataset_version: field("dataset_version")?,
        fingerprint: field("fingerprint")?,
    };
    let path = PathBuf::from(&declared.path);
    let recomputed = match kind {
        "barframe" => {
            let frame = read_bar_frame_for_backtest(&path)?;
            let (_, manifest) = barframe_dataset_identity(&path, &frame)?;
            barframe_input_provenance(&path, &manifest)
        }
        _ => depth_frame_input_provenance(&path, &read_depth_frame_for_backtest(&path)?),
    };
    for (name, (declared_value, actual_value)) in [
        ("dataset_id", (&declared.dataset_id, &recomputed.dataset_id)),
        (
            "dataset_version",
            (&declared.dataset_version, &recomputed.dataset_version),
        ),
        (
            "fingerprint",
            (&declared.fingerprint, &recomputed.fingerprint),
        ),
    ] {
        if declared_value != actual_value {
            return Err(format!(
                "回测产物声明的输入与实况不符: {name} 声明={declared_value} 重算={actual_value}（输入文件 {}）",
                recomputed.path
            ));
        }
    }
    Ok(Some(recomputed))
}

/// BarFrame 数据集的版本号：文件名后缀会一直跟着它，改它就是声明"换了另一种输入形状"。
pub(crate) const BARFRAME_DATASET_VERSION: &str = "barframe-json-v1";
/// 深度帧输入的版本号（它没有注册表条目，只有这一档形状）。
pub(crate) const DEPTH_FRAME_DATASET_VERSION: &str = "depth-frame-v1";

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
    /// 事实源与本轮账簿：产物摘要在**这里**做重放校验，而不是让各链自己算一个哈希递进来。
    /// 旧口径把 `report.replay_hash()`（= 把同一段事件切片再哈希一次）当结论写进产物，
    /// 与 `result_hash` 恒等且不可能失败（V11 Q62）。
    pub(crate) event_log: &'a qx_core::EventLog,
    pub(crate) ledger: &'a qx_core::Ledger,
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
    /// 这次跑的到底是哪一份输入：路径 + 数据集身份 + 内容指纹（V11 Q66）。
    pub(crate) input: BacktestInputProvenance,
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
    // 重放校验排在写文件之前：跑不通过的重放没有资格往产物里写一个"看起来校验过"的数字。
    let replay = qx_core::ReplayVerifier::verify(input.event_log.events(), input.ledger)
        .map_err(|error| format!("回测事件日志未通过重放校验: {error}"))?;
    if replay.log_digest != input.result_hash {
        return Err(format!(
            "回测结果哈希与重放哈希不一致: result={:016x} replay={:016x}",
            input.result_hash, replay.log_digest
        ));
    }
    let mut summary = serde_json::json!({
        // v3：摘要开始交代"跑的是哪一份输入"（`input` 块）。v2 只有 `input_data_hash`，那是
        // 引擎对自己手里那段切片的自哈希，回答不了这个问题（V11 Q66）。
        "schema_version": 3,
        "strategy_id": input.strategy_id,
        "instrument": input.instrument.to_string(),
        "input": input_provenance_json(&input.input),
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
        // 重放不是一个哈希，而是三条能被读者各自核对的结论：事实源重新过一遍闸门得到的日志摘要、
        // 重放吞下的事件条数、以及这些事实重建出的账簿条数（必须等于本轮账簿条数，否则不落盘）。
        "replay": {
            "log_digest": format!("{:016x}", replay.log_digest),
            "events": replay.events,
            "ledger_entries": replay.ledger_entries,
            "run_ledger_entries": input.ledger.entries().len(),
        },
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
