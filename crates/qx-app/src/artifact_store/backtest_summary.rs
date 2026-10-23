//! Construct and persist the canonical rich summary for a completed backtest.

use super::sink;
use crate::artifact_store::persist_verified_run_record;
use qx_core::{InstrumentId, Money};
use qx_strategy::builtin_signal::BuiltinSignalProvenance;
use qx_strategy::BuiltinStrategyConfig;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const BACKTEST_SUMMARY_SCHEMA_VERSION: u32 = 7;

#[derive(Clone, Debug)]
pub struct PersistedBacktestArtifacts {
    pub summary_path: PathBuf,
    pub equity_path: PathBuf,
    pub fills_path: PathBuf,
    pub summary: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct BacktestInputProvenance {
    /// 这一档输入按哪种形状被读成数据集：`barframe` / `depth-frame`。
    pub kind: &'static str,
    /// 运行时读取的那个文件的路径，复核时按它重读同一份输入。
    pub path: String,
    pub dataset_id: String,
    pub dataset_version: String,
    /// 内容指纹：策略链取自注册表已复核过的 `DatasetManifest::fingerprint`，深度链取自
    /// `DepthFrame::input_hash()`。落盘写的与复核重算的只能出自下面那两个读点。
    pub fingerprint: String,
}

impl BacktestInputProvenance {
    /// Canonical wire representation shared by API/SDK and CLI summary writers.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "path": self.path,
            "dataset_id": self.dataset_id,
            "dataset_version": self.dataset_version,
            "fingerprint": self.fingerprint,
        })
    }
}

fn input_provenance_json(input: &BacktestInputProvenance) -> serde_json::Value {
    input.to_json()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacktestAccountBase {
    pub cash: Money,
    pub source: &'static str,
}

/// 本金来源标签的登记处（V11 Q72）："没配" 与 "配了同一个数" 必须分得开。
///
/// 自 WP-2 起定义在应用层：`account_base.source` 是 `&'static str`，而门面解析出的是
/// 字符串——两条链各自写一份字面量，就会出现"同一份配置在 book 上标 X、在 builtin 上标 Y"。
pub const BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE: &str = "builtin-default";
pub const BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE: &str = "strategy-initial-cash";
pub const BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE: &str = "account-registry";
/// 第四格：回测侧那格声明同时被 paper 侧的 `worker.paper_initial_cash_raw` 确认过（V12 R3）。
/// 它必须与"只有回测侧声明"分得开，否则读者看不出这份 runtime 的账户本金是被两处共用的。
pub const BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE: &str =
    "strategy-initial-cash+paper-worker-cash";
/// 多腿链不用前四格：两条腿的本金各按本腿行情由 `multi_leg_leg_cash` 定资。
pub const BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE: &str = "multi-leg-funding-rule";

/// 把门面解析出的来源字符串折回登记过的静态标签；未登记的值一律回落 `builtin-default`。
///
/// 宁可在产物里少说一句，也不把没登记过的标签写进去——标签是给人读的闭集，不是自由文本。
pub fn account_base_source_label(source: Option<&str>) -> &'static str {
    match source {
        Some(BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE) => BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE,
        Some(BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE) => BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE,
        Some(BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE) => {
            BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE
        }
        Some(BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE) => {
            BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE
        }
        _ => BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
    }
}

#[derive(Clone, Copy, Debug)]
pub enum BacktestSampleTimestamps<'a> {
    Values(&'a [u64]),
    Bars(&'a [qx_guanxing::Bar]),
}

impl BacktestSampleTimestamps<'_> {
    fn get(self, index: usize) -> Option<u64> {
        match self {
            Self::Values(values) => values.get(index).copied(),
            Self::Bars(bars) => bars.get(index).map(|bar| bar.ts),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BacktestSignalParams {
    pub kind: &'static str,
    /// 这四项取自 `strategy.builtin_*` 还是内置默认。与 `cost_source` 同一条理由：
    /// "没配" 与 "配了同样数值" 必须在产物里可区分。
    pub source: &'static str,
    /// 这个 kind 的信号读的几项（`none` = 一项都不读，如 MACD 的 12/26/9 是内核常数）。
    pub knobs: String,
    /// 配置声明了、这一轮没上场的键名（`none` = 声明的都上场）。
    pub declared_unused: String,
    pub fast_window: usize,
    pub slow_window: usize,
    pub period: usize,
    pub threshold_bps: i128,
    /// 下单数量（定点 raw）：内置策略每条信号都下这个量，它是结果的一部分，不是配置的附属。
    /// stdout 那行印的是命令行上的人数（`quantity=100`），这里存的是同一数的 raw。
    pub quantity_raw: i128,
}

impl BacktestSignalParams {
    /// Project one resolved native built-in configuration into its persisted signal contract.
    pub fn from_builtin_config(
        config: &BuiltinStrategyConfig,
        provenance: &BuiltinSignalProvenance,
    ) -> Self {
        Self {
            kind: config.kind.name(),
            source: provenance.source,
            knobs: provenance.knobs.clone(),
            declared_unused: provenance.declared_unused.clone(),
            fast_window: config.fast_window,
            slow_window: config.slow_window,
            period: config.period,
            threshold_bps: config.threshold_bps,
            quantity_raw: config.quantity.raw(),
        }
    }

    /// Render the shared one-line signal provenance used by CLI entrypoints.
    pub fn display_note(&self) -> String {
        format!(
            "source={} fast_window={} slow_window={} period={} threshold_bps={} \\
             knobs={} declared_unused={}",
            self.source,
            self.fast_window,
            self.slow_window,
            self.period,
            self.threshold_bps,
            self.knobs,
            self.declared_unused,
        )
    }
}

#[cfg(test)]
mod signal_params_tests {
    use super::BacktestSignalParams;
    use qx_core::{InstrumentId, Quantity};
    use qx_strategy::builtin_signal::{BuiltinSignalKnob, BuiltinSignalProvenance};
    use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};

    #[test]
    fn signal_params_keep_resolved_knobs_provenance_and_raw_quantity() {
        let mut config = BuiltinStrategyConfig::new(
            BuiltinStrategyKind::SmaCross,
            "signal-params-test",
            InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument"),
            Quantity::from_i64(3),
        )
        .expect("valid strategy config");
        config.fast_window = 7;
        config.slow_window = 18;
        let provenance =
            BuiltinSignalProvenance::from_declared(config.kind, [BuiltinSignalKnob::FastWindow]);

        let params = BacktestSignalParams::from_builtin_config(&config, &provenance);

        assert_eq!(params.kind, "sma_cross");
        assert_eq!(params.source, "config");
        assert_eq!(params.knobs, "fast_window,slow_window");
        assert_eq!(params.declared_unused, "none");
        assert_eq!((params.fast_window, params.slow_window), (7, 18));
        assert_eq!(params.quantity_raw, Quantity::from_i64(3).raw());
        assert_eq!(
            params.display_note(),
            "source=config fast_window=7 slow_window=18 period=14 threshold_bps=100 \\
             knobs=fast_window,slow_window declared_unused=none"
        );
    }
}

/// 同账户策略组合的成交归属：哪些策略占过槽、各自成交几笔。
///
/// 它只对组合链有意义，所以是可选块：非组合链不写这两个键（"没有的东西不占键"，
/// 与 `fill_model` 同一条理由）。
#[derive(Clone, Debug)]
pub struct PortfolioFillAttribution {
    pub strategy_ids: Vec<String>,
    pub strategy_fills: std::collections::BTreeMap<String, u64>,
}

pub struct BacktestArtifacts<'a> {
    /// Runtime 配置是离线复算的输入，摘要必须保留精确来源，报告才能把它纳入 EvidenceBundle。
    pub runtime_config_path: Option<&'a Path>,
    /// 外部 market spec 会影响撮合与标的精度，同样必须随摘要进入可复算证据链。
    pub market_spec_path: Option<&'a Path>,
    pub strategy_id: &'a str,
    pub instrument: &'a InstrumentId,
    pub sample_unit: &'static str,
    pub samples: usize,
    pub sample_ts: BacktestSampleTimestamps<'a>,
    pub equity: &'a [i128],
    pub positions: &'a [i128],
    pub fills: &'a [qx_core::Fill],
    /// 每次订单执行评估及其结果，供报告分析成交/未成交成因。
    pub execution_attempts: &'a [qx_xingban::ExecutionAttempt],
    pub clock_start: u64,
    pub clock_end: u64,
    pub input_data_hash: u64,
    pub result_hash: u64,
    /// 事实源与本轮账簿：产物摘要在**这里**做重放校验，而不是让各链自己算一个哈希递进来。
    /// 旧口径把 `report.replay_hash()`（= 把同一段事件切片再哈希一次）当结论写进产物，
    /// 与 `result_hash` 恒等且不可能失败（V11 Q62）。
    pub event_log: &'a qx_core::EventLog,
    pub ledger: &'a qx_core::Ledger,
    pub issuer_capital: &'a [qx_xingban::AshareIssuerCapitalSnapshot],
    pub issuer_capital_at_end: Option<&'a qx_xingban::AshareIssuerCapitalSnapshot>,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub final_equity_raw: i128,
    pub assumptions: &'a [String],
    pub model_descriptors: &'a [String],
    pub risk_rule_set_version: &'a str,
    /// 规则集来源：`runtime-config` 表示吃了 `strategy.risk_rules`，`conservative-default`
    /// 表示该入口没有给出配置文件。产物必须能区分这两者（V10 §4.2）。
    pub risk_rule_source: &'static str,
    /// 执行成本口径的来源（V11 Q0c）：`cost-rules-file:<路径>` / `runtime-config-default` /
    /// `builtin-default` / `cli-flag` / `ashare-rules`。费率数值本身已经在
    /// `model_descriptors` 里，这里只回答"这组数字从哪来"，让"没配"与"配了同样的值"可区分。
    pub cost_source: &'a str,
    /// Bar 链的撮合模型与其来源（V11 Q1a 第二批）：`(配置名, 来源)`。模型本身已经在
    /// `model_descriptors` 里（那份是内核自述，带参数与假设），这里补的是"这个口径是
    /// 配置里声明的还是没人提"——与 `execution_costs.source` 同一条理由。
    /// 深度链的 Tick/OrderBook 内核不经过 `FillModel`，所以它是 `None`：没有的东西
    /// 不该在摘要里占一个键。
    pub fill_model: Option<(&'static str, &'static str)>,
    pub fill_model_details: Option<&'a qx_xingban::FillModelDescriptor>,
    pub execution_model_details: Option<&'a qx_xingban::OrderBookExecutionDescriptor>,
    /// 这一轮的账户本金与它的来源（V11 Q72）：`return_bps` 的分母、风控门看到的可用现金都
    /// 是它，而摘要此前只印结果不印尺度 —— 读者拿 `metrics.final_equity_raw` 减不出本金，
    /// 因为期初那笔钱从没被写出来过。"没配默认 100,000" 与 "配了 100,000" 必须可区分，
    /// 与 `execution_costs.source`、`fill_model.source` 同一条理由。
    pub account_base: BacktestAccountBase,
    /// 本次实际使用的撮合内核；深度档不得声称与 Bar 链同一内核。
    pub matching_kernel: &'static str,
    /// 引擎挡下的委托，按 `(原因, 次数)` 降序。只看 `fills` 分不出"策略没发信号"与
    /// "信号全被风控或现金挡下"，这两件事对使用者的含义相反。
    pub rejections: &'a [(String, usize)],
    /// 这次跑的到底是哪一份输入：路径 + 数据集身份 + 内容指纹（V11 Q66）。
    pub input: BacktestInputProvenance,
    /// 这一轮并进策略配置的那套信号参数与下单数量（V12 R4-j）：那行 `[X · Signal] source=…
    /// fast_window=…` 此前只活在 stdout，摘要读出来分不出 fast=5 与 fast=20。跨语言策略链的口径由策略自己声明、
    /// 内置窗口根本没参与，所以它是 `None`——与 `fill_model` 同一条理由：没有的东西不占键。
    /// 注意这一格说的是**并进配置的口径**：各 kind 的信号读哪几项并不一致（MACD 的 12/26/9
    /// 是 `qx-strategy` 的内核常数），所以它不担保每一项都改变了结果。
    pub signal: Option<BacktestSignalParams>,
    /// 已与本轮 BarFrame 身份、规范化内容摘要和样本范围核对的数据质量读面。
    pub dataset_quality: Option<&'a serde_json::Value>,
    /// 本轮账簿的账户归属；`None` 表示该链沿用缺省账户，摘要不写这一格。
    pub account_id: Option<&'a str>,
    /// 同账户策略组合的成交归属；`None` 表示这不是组合链。
    pub portfolio: Option<&'a PortfolioFillAttribution>,
    /// 链级旧字段（深度链的既有消费方按顶层旧键读摘要）。已存在的统一字段优先，
    /// 这里只补缺——统一字段与旧字段冲突时，统一口径赢，不让一份摘要同时说两种话。
    pub summary_extra: Option<&'a serde_json::Value>,
}

fn compute_risk_ratios(
    equity: &[i128],
    return_bps: i32,
    max_drawdown_bps: u32,
) -> qx_core::BacktestRiskMetrics {
    qx_core::backtest_risk_metrics(equity, return_bps, max_drawdown_bps)
}

fn ratio_to_json(value: Option<f64>) -> serde_json::Value {
    match value {
        Some(finite) if finite.is_finite() => serde_json::Number::from_f64(finite)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    }
}

fn rejection_count(rejections: &[(String, usize)]) -> usize {
    crate::trade_facts::rejection_count(rejections)
}

/// 摘要类产物落盘：委托全仓唯一发布出口（`artifact_store::sink`）。
///
/// 此前这里与 `run_record.rs::write_run_record` 是两份逐字相同的临时文件逻辑；
/// 语义（内容相同幂等 / 不同即冲突 / 崩溃不留半份）现在由 sink 单点持有，
/// 调用方只提供标签用于错误文案。
pub fn write_backtest_artifact(path: &Path, payload: &str, label: &str) -> Result<(), String> {
    sink::publish(path, payload, sink::PublishMode::Idempotent)
        .map_err(|error| format!("{label}落盘失败: {error}"))
}

pub fn persist_backtest_artifacts(
    manifest_path: &Path,
    input: &BacktestArtifacts<'_>,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let persisted = persist_backtest_artifacts_with_summary(manifest_path, input)?;
    Ok((
        persisted.summary_path,
        persisted.equity_path,
        persisted.fills_path,
    ))
}

pub fn persist_backtest_artifacts_with_summary(
    manifest_path: &Path,
    input: &BacktestArtifacts<'_>,
) -> Result<PersistedBacktestArtifacts, String> {
    let manifest_stem = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backtest.run.json")
        .strip_suffix(".run.json")
        .unwrap_or("backtest");
    // Include the summary generation so an upgraded producer can coexist with older output
    // for the same content-addressed RunManifest.
    // 可选质量清单是回测后的分析证据，不改变交易事实或 RunManifest；给它独立产物前缀，
    // 使同一运行可并存「未绑定质量清单」与「绑定某份清单」两组摘要而不互相覆盖。
    let quality_suffix = input
        .dataset_quality
        .and_then(|quality| quality.get("manifest_fingerprint"))
        .and_then(serde_json::Value::as_str)
        .map(|fingerprint| format!(".quality-{fingerprint}"))
        .unwrap_or_default();
    let stem = format!("{manifest_stem}{quality_suffix}.v{BACKTEST_SUMMARY_SCHEMA_VERSION}");
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
    // v5 增加风险比率与 RunRecord 指针；报告只读 summary，复核时会核对 RunRecord 全部产物摘要。
    // P2/P5 风险调整比率：唯一写点在这一处（与上面五格同源），报告与未来的 compare 只念这组
    // 数，不各自重算。算不出来（样本不足 / 分母为 0）写 `null`，读侧照 "absent" 处理。
    let ratios = compute_risk_ratios(input.equity, input.return_bps, input.max_drawdown_bps);
    let mut summary = serde_json::json!({
        // v6：添加 A 股发行人股本和已入账公司行为权益读面；v5 增加报告度量与 RunRecord 指针。
        // v4 开始交代"这一轮压在多少钱上"（`account` 块）；
        // v3 有 `input` 却没有期初本金，于是 `metrics.return_bps` 的分母与期末权益对照物不在产物里（V11 Q72）。
        // v3：摘要开始交代"跑的是哪一份输入"（`input` 块）。v2 只有 `input_data_hash`，那是
        // 引擎对自己手里那段切片的自哈希，回答不了这个问题（V11 Q66）。
        "schema_version": BACKTEST_SUMMARY_SCHEMA_VERSION,
        "strategy_id": input.strategy_id,
        "instrument": input.instrument.to_string(),
        "input": input_provenance_json(&input.input),
        "account": {
            "initial_cash_raw": input.account_base.cash.raw().to_string(),
            "source": input.account_base.source,
        },
        "bars": input.samples,
        "sample_unit": input.sample_unit,
        "fills": input.fills.len(),
        "execution_attempts": input.execution_attempts,
        "execution_attempt_count": input.execution_attempts.len(),
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
            // P2/P5 风险调整比率：唯一写点在这里（与上面五格同源），报告与未来的 compare 只念
            // 这组数，不各自重算。算不出来（样本不足 / 分母为 0）写 `null`，读侧照"absent"处理。
            "sharpe": ratio_to_json(ratios.sharpe),
            "sortino": ratio_to_json(ratios.sortino),
            "calmar": ratio_to_json(ratios.calmar),
            "win_rate": ratio_to_json(ratios.win_rate),
            "profit_factor": ratio_to_json(ratios.profit_factor),
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
        "run_record": summary_path.with_file_name(format!("{stem}.record.json")).to_string_lossy(),
    });
    if let Some(analysis) = crate::ashare_analysis_json(
        input.ledger,
        input.issuer_capital,
        input.issuer_capital_at_end,
    ) {
        summary["ashare_analysis"] = analysis;
    }
    if let Some(dataset_quality) = input.dataset_quality {
        summary["dataset_quality"] = dataset_quality.clone();
    }
    if let Some(account_id) = input.account_id {
        summary["account_id"] = serde_json::json!(account_id);
    }
    if let Some(portfolio) = input.portfolio {
        summary["strategy_ids"] = serde_json::json!(portfolio.strategy_ids);
        summary["strategy_fills"] = serde_json::json!(portfolio.strategy_fills);
    }
    if let Some(path) = input.runtime_config_path {
        summary["runtime_config"] = serde_json::json!(path.to_string_lossy());
    }
    if let Some(path) = input.market_spec_path {
        summary["market_spec"] = serde_json::json!(path.to_string_lossy());
    }
    if let Some((name, source)) = input.fill_model {
        // 只在真的有 `FillModel` 的链上写这个键：深度链的撮合口径在 `model_descriptors`
        // 的四参数描述子里，硬塞一个 "fill_model": null 等于给摘要添一个没人能填的格子。
        let mut fill_model = serde_json::json!({ "name": name, "source": source });
        if let Some(details) = input.fill_model_details {
            fill_model["descriptor_schema_version"] = serde_json::json!(1);
            fill_model["version"] = serde_json::json!(details.version);
            fill_model["data_tier"] = serde_json::json!(details.data_tier);
            fill_model["parameters"] = serde_json::json!(details.parameters);
            fill_model["assumption"] = serde_json::json!(details.assumption);
        }
        summary["fill_model"] = fill_model;
    }
    if let Some(details) = input.execution_model_details {
        summary["execution_model"] = serde_json::to_value(details)
            .map_err(|error| format!("编码订单簿撮合模型描述失败: {error}"))?;
    }
    if let Some(signal) = input.signal.as_ref() {
        // 生效的信号口径此前只活在 stdout 的 `[X · Signal]` 那一行：同一份摘要既可能是
        // fast=5 也可能是 fast=20 跑出来的，事后读产物分不出这两者（V12 R4-j）。
        // 跨语言策略链的口径由策略自己声明，这四项没有落点，所以它不落键——与 `fill_model`
        // 同一条理由。i128 按字符串落盘，与 `account/initial_cash_raw` 同一口径。
        //
        // `knobs` / `declared_unused` 说的是"这四项里哪几项真的进了这个 kind 的信号"
        // （V12 #102）：只有数值没有生效面，摘要仍会让读者以为 `fast_window=5` 选了东西。
        summary["signal"] = serde_json::json!({
            "kind": signal.kind,
            "source": signal.source,
            "knobs": signal.knobs,
            "declared_unused": signal.declared_unused,
            "fast_window": signal.fast_window,
            "slow_window": signal.slow_window,
            "period": signal.period,
            "threshold_bps": signal.threshold_bps.to_string(),
            "quantity_raw": signal.quantity_raw.to_string(),
        });
    }
    // 链级旧字段的落点：统一摘要之外的消费方按顶层旧键读同一份摘要（深度链的
    // `run_id` / `tier` / `equity_points` / `ledger_entries` 等）。它们在这里并入，而不是事后回写——
    // `RunRecord` 按当时落盘的字节给摘要算摘要，先写后补等于让 record 记住一份从未存在的摘要，
    // 报告侧回读就成了自证失败的假阴性。已存在的键一律让位给上面的统一字段。
    if let Some(extra) = input.summary_extra.as_ref() {
        if let (Some(object), Some(fields)) = (summary.as_object_mut(), extra.as_object()) {
            for (key, value) in fields {
                if !object.contains_key(key) {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let summary_payload = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("编码回测摘要失败: {error}"))?;
    write_backtest_artifact(&summary_path, &summary_payload, "回测摘要")?;

    // Writing to String is infallible; do not turn the fmt::Write contract into a panic path.
    let mut equity_payload = String::from("index,ts,equity_raw,position_raw\n");
    for (index, equity) in input.equity.iter().enumerate() {
        let ts = input.sample_ts.get(index).unwrap_or_default();
        let position = input.positions.get(index).copied().unwrap_or_default();
        let _ = writeln!(&mut equity_payload, "{index},{ts},{equity},{position}");
    }
    write_backtest_artifact(&equity_path, &equity_payload, "权益曲线")?;

    let mut fills_payload = String::from(
        "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id\n",
    );
    for fill in input.fills {
        let _ = write!(
            &mut fills_payload,
            "{},{},{},{},{},{},{},",
            fill.order_id,
            fill.ts,
            fill.qty.raw(),
            fill.price.raw(),
            fill.fee.raw(),
            fill.account_id,
            fill.strategy_id.as_deref().unwrap_or_default(),
        );
        if let Some(signal_id) = fill.signal_id {
            let _ = write!(&mut fills_payload, "{signal_id}");
        }
        fills_payload.push(',');
        if let Some(intent_id) = fill.intent_id {
            let _ = write!(&mut fills_payload, "{intent_id}");
        }
        let _ = writeln!(
            &mut fills_payload,
            ",{},{}",
            fill.venue_id.as_deref().unwrap_or_default(),
            fill.venue_order_id.as_deref().unwrap_or_default(),
        );
    }
    write_backtest_artifact(&fills_path, &fills_payload, "成交明细")?;
    persist_verified_run_record(manifest_path, &summary_path, &equity_path, &fills_path)?;
    Ok(PersistedBacktestArtifacts {
        summary_path,
        equity_path,
        fills_path,
        summary,
    })
}
