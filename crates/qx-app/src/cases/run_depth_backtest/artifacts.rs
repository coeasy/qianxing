//! 深度链的产物装配：配置指纹、输入来源、账户/信号口径与四份产物的落盘。
//!
//! 自 WP-2 起这条链的产物由**用例**自己写（不再由门面写）：配置指纹、输入来源、账户本金、
//! 信号口径与四份产物必须出自同一段代码，否则 CLI / HTTP / Python 三个入口会用三种口径
//! 描述同一轮运行（§6 第 7 条）。指纹函数与旧 CLI 实现逐位一致——证据包的离线复算靠
//! `RunManifest` 逐字节相等判"同一次运行"，改口径等于让旧证据包全部失效。

use super::super::kernels::{ORDER_BOOK_MATCHING_KERNEL, TICK_MATCHING_KERNEL};
use crate::artifact_store::{
    account_base_source_label, persist_backtest_artifacts_with_summary, persist_run_manifest,
    BacktestAccountBase, BacktestArtifacts as BacktestArtifactInput, BacktestInputProvenance,
    BacktestSampleTimestamps, BacktestSignalParams, PortfolioFillAttribution,
};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::BacktestArtifacts;
use crate::trade_facts::rejection_facts;
use crate::DepthBacktestSpec;
use qx_core::{Fill, Fnv1a, Money, RunManifest};
use qx_xingban::{DepthFrame, OrderBookBacktestReport, RunManifestIdentity};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 深度链的样本单位：一份盘口快照。它必须与 Bar 链的 `bar` 分得开，否则报告会拿
/// "每根 Bar 一点"去读一条逐快照的曲线。
pub(super) const DEPTH_SAMPLE_UNIT: &str = "depth_snapshot";
/// 深度链的运行期版本：进 `RunManifest.runtime_version`，证据包按它判"哪一代产物"。
pub(super) const DEPTH_RUNTIME_VERSION: &str = "depth-backtest-v1";
/// 深度帧数据集的版本（与 `qx-data` 的注册口径同源）。
pub(super) const DEPTH_FRAME_DATASET_VERSION: &str = qx_data::DEPTHFRAME_DATASET_VERSION;

/// 本链实际使用的撮合内核：L1 走 Tick（只吃首档），L2/L3 走逐档簿。
pub(super) fn matching_kernel(tier: &str) -> &'static str {
    if tier == "l1" {
        TICK_MATCHING_KERNEL
    } else {
        ORDER_BOOK_MATCHING_KERNEL
    }
}

/// 这一轮的账户本金：金额来自 spec（门面已解析），来源标签取 spec 的登记值。
pub(super) fn account_base(spec: &DepthBacktestSpec) -> BacktestAccountBase {
    BacktestAccountBase {
        cash: Money::from_raw(spec.initial_cash_raw),
        source: account_base_source_label(spec.account_base_source.as_deref()),
    }
}

/// 这一轮并进策略配置的信号口径。
///
/// `source` 与 `declared_unused` 是**解析事实**（"配置里声明了没有"、"声明的哪几项这个 kind
/// 不读"），值本身已由门面写进 `spec.strategy`；用例只把它们与生效数值拼成同一块。
pub(super) fn signal_params(spec: &DepthBacktestSpec) -> BacktestSignalParams {
    let kind = spec.strategy.kind;
    BacktestSignalParams {
        kind: kind.name(),
        source: match spec.signal_source.as_deref() {
            Some("config") => "config",
            _ => "builtin-default",
        },
        knobs: kind.signal_knob_list(),
        declared_unused: spec
            .signal_declared_unused
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        fast_window: spec.strategy.fast_window,
        slow_window: spec.strategy.slow_window,
        period: spec.strategy.period,
        threshold_bps: spec.strategy.threshold_bps,
        quantity_raw: spec.strategy.quantity_raw,
    }
}

/// 同账户组合的成交归属：哪些槽位占过、各自成交几笔。
///
/// 归属必须闭合：一笔成交缺 `strategy_id`、或归属于组合外策略，都是内部不变量被破坏，
/// 不是"摘要少写一格"。组合外的链返回 `None`——没有的东西不占键（与 `fill_model` 同一条理由）。
pub(super) fn portfolio_fill_attribution(
    spec: &DepthBacktestSpec,
    fills: &[Fill],
) -> Result<Option<PortfolioFillAttribution>, AppError> {
    let Some(portfolio) = &spec.portfolio else {
        return Ok(None);
    };
    let mut members = vec![(
        portfolio.primary_priority,
        spec.strategy.strategy_id.clone(),
    )];
    members.extend(
        portfolio
            .additional_strategies
            .iter()
            .map(|member| (member.priority, member.strategy.strategy_id.clone())),
    );
    members.sort();
    let mut fill_counts = members
        .iter()
        .map(|(_, strategy_id)| (strategy_id.clone(), 0_u64))
        .collect::<BTreeMap<_, _>>();
    for fill in fills {
        let strategy_id = fill.strategy_id.as_deref().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "Tick/OrderBook 组合成交缺少 strategy_id 归属",
            )
        })?;
        let count = fill_counts.get_mut(strategy_id).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook 组合成交归属未知策略: {strategy_id}"),
            )
        })?;
        *count += 1;
    }
    Ok(Some(PortfolioFillAttribution {
        strategy_ids: members.iter().map(|(_, id)| id.clone()).collect(),
        strategy_fills: fill_counts,
    }))
}

/// 摘要里"跑的是哪一份输入"那一块：路径 + 数据集身份 + 内容指纹（V11 Q66）。
pub(super) fn input_provenance(
    spec: &DepthBacktestSpec,
    frame: &DepthFrame,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "depth-frame",
        path: spec.depth_path.clone(),
        dataset_id: format!("depth-bars:{}", frame.instrument),
        dataset_version: DEPTH_FRAME_DATASET_VERSION.to_string(),
        fingerprint: format!("{:016x}", frame.input_hash()),
    }
}

/// 深度回测的配置指纹：显式参数 + 生效风控规则集版本 + 输入指纹 + 可选质量清单指纹。
///
/// 与旧 CLI 实现逐位一致（同样的写入顺序、同样的定点口径）：证据包离线复算比较的是
/// `RunManifest` 整体，这一格一漂，历史证据包就全部对不上。
/// 深度链摘要的「旧字段」：统一摘要之外的既有消费方仍按顶层旧键读同一份摘要。
///
/// `verify_depth_run`、`qx-cli report --evidence` 与离线重放对摘要取的是顶层
/// `run_id` / `data_fingerprint` / `tier` / `equity_points` / `final_equity_raw` /
/// `replay_events` / `ledger_entries`——这些是深度链的既有契约，不随摘要统一收口而消失。
///
/// 这里**只补字段，不重算**：一律取自已构建的 `RunManifest` 与报告读数；已存在的统一
/// 字段优先（`summary_extra` 只填空位），不让一份摘要同时说两种话。补在这一格而不是
/// 事后回写文件，是为了让 `RunRecord` 记住的摘要摘要与落盘字节一致。
fn depth_legacy_summary_fields(
    spec: &DepthBacktestSpec,
    manifest: &RunManifest,
    run_id: &str,
    tier: &str,
    initial_cash_raw: i128,
    report: &OrderBookBacktestReport,
) -> serde_json::Value {
    let mut fields = serde_json::json!({
        "run_id": run_id,
        "manifest_run_id": manifest.run_id,
        "manifest": manifest,
        "config_hash": manifest.config_hash,
        "data_fingerprint": manifest.data_fingerprint,
        "tier": tier,
        "equity_points": report.equity.len() as u64,
        "final_equity_raw": report.final_equity(),
        "return_bps": report.return_bps,
        "max_drawdown_bps": report.max_drawdown_bps,
        "fees_raw": report.fees_raw,
        "turnover_raw": report.turnover_raw,
        "initial_cash_raw": initial_cash_raw,
        "replay_events": report.event_log.events().len() as u64,
        "replay_ledger_entries": report.ledger.entries().len() as u64,
        "ledger_entries": report.ledger.entries().len() as u64,
    });
    // 深度专有执行配置仍是公开摘要契约的一部分。统一摘要承载共同字段，这几格继续
    // 由深度用例投影，避免回测产品/策略参数在汇总时悄悄丢失。
    if let Some(market_spec) = spec.market_spec.as_ref() {
        fields["market_spec"] = serde_json::json!(market_spec);
    }
    if let Some(order_policy) = spec.order_policy.as_ref() {
        fields["order_policy"] = serde_json::json!(order_policy);
    }
    if let Some(market_making) = spec.market_making.as_ref() {
        fields["market_making"] = serde_json::json!(market_making);
    }
    fields
}

pub(super) fn config_hash(
    spec: &DepthBacktestSpec,
    frame: &DepthFrame,
    fee_bps: i128,
    risk_rule_set_version: &str,
    quality_manifest_fingerprint: Option<&str>,
    custom_identities: &[&crate::cases::StrategyArtifactIdentity],
) -> String {
    let mut hash = Fnv1a::new();
    hash.write_text(DEPTH_RUNTIME_VERSION);
    hash.write_text(&spec.tier);
    hash.write_text(spec.strategy.kind.name());
    // 命令行位置参数是"几单位"，不是定点 raw：旧口径按整数入哈希，这里按同一口径折算。
    hash.write_i128(spec.strategy.quantity_raw / qx_core::SCALE);
    hash.write_i128(fee_bps);
    // 两项撮合参数改变结果，就必须改变配置指纹；漏掉它们会让两次不同口径的回测
    // 声称同一份配置，产物落盘时还会互相判成"同一回测已有不同内容"。
    hash.write_u64(spec.latency_snapshots);
    hash.write_i128(spec.market_impact_bps);
    hash.write_text(risk_rule_set_version);
    hash.write_u64(frame.input_hash());
    if let Some(fingerprint) = quality_manifest_fingerprint {
        hash.write_text(fingerprint);
    }
    // 自定义策略的身份改变结果，就必须改变配置指纹；按 strategy_id 排序，让指纹与入参顺序无关。
    for identity in custom_identities {
        hash.write_text(&identity.strategy_id);
        hash.write_text(&identity.strategy_version);
        hash.write_text(&identity.source_sha256);
    }
    format!("{:016x}", hash.finish())
}

/// 一轮深度回测的产物装配输入。所有字段都是**已算出的读数**：这里不碰引擎。
pub(super) struct DepthArtifactInput<'a> {
    pub spec: &'a DepthBacktestSpec,
    pub frame: &'a DepthFrame,
    pub report: &'a OrderBookBacktestReport,
    pub risk_rule_set_version: &'a str,
    pub risk_rule_source: &'static str,
    pub cost_source: &'a str,
    pub config_hash: &'a str,
    pub data_fingerprint: &'a str,
    pub dataset_quality: Option<&'a serde_json::Value>,
    /// 实际执行的策略身份：注入的自定义策略用自己的版本，内置策略用 `builtin-<kind>-v1`。
    pub strategy_id: &'a str,
    pub strategy_version: &'a str,
    pub account_id: Option<&'a str>,
    pub portfolio: Option<PortfolioFillAttribution>,
    pub context: &'a crate::RunContext,
}

/// 落盘结果：清单路径 + 四份产物路径 + 清单摘要（stdout 与证据包核对共用）。
pub(super) struct PersistedDepthRun {
    pub manifest: RunManifest,
    pub artifacts: BacktestArtifacts,
    pub summary: serde_json::Value,
}

/// 构建 `RunManifest`、按内容寻址落盘，再写四份产物。
pub(super) fn persist(input: DepthArtifactInput<'_>) -> Result<PersistedDepthRun, AppError> {
    let spec = input.spec;
    let report = input.report;
    let account_base = account_base(spec);
    let matching_kernel = matching_kernel(&spec.tier);
    let rejections = rejection_facts(&report.event_log);
    let input_provenance = input_provenance(spec, input.frame);
    // RunManifest 的身份：`depth-backtest:<tier>:<kind>:<instrument>`。它是证据包契约的一部分
    // （离线复算按这个形状解析），因此由 spec 显式携带；缺省时才退回 run_id。
    let manifest_run_id = spec
        .manifest_run_id
        .clone()
        .unwrap_or_else(|| spec.run_id.clone());
    let strategy_version = input.strategy_version.to_string();
    let instrument_spec_version = match spec.market_spec_source.as_deref() {
        Some(label) => label.to_string(),
        None if spec.market_spec.is_some() => "trading-instrument-spec-v1".to_string(),
        None => "instrument-spec:none".to_string(),
    };
    let manifest = report
        .run_manifest(
            RunManifestIdentity {
                run_id: &manifest_run_id,
                code_commit: input.context.code_commit(),
                config_hash: input.config_hash,
                strategy_version: &strategy_version,
                instrument_spec_version: &instrument_spec_version,
                runtime_version: DEPTH_RUNTIME_VERSION,
            },
            input.data_fingerprint,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook RunManifest 构建失败: {error}"),
            )
        })?;
    let root = PathBuf::from(&spec.output_dir);
    let manifest_path = persist_run_manifest(&root, &manifest)?;
    let strategy_id = input.strategy_id.to_string();
    let run_id = spec.run_id.clone();
    let tier = spec.tier.clone();
    let initial_cash_raw = spec.initial_cash_raw;
    // 统一摘要之外，深度链还有既有消费方按「顶层旧字段」读取（verify-depth、report --evidence、
    // 交叉入口测试）。这些字段取自已构建的 RunManifest 与报告读数，不在此处重新推导；
    // 在摘要序列化**之前**并入，RunRecord 记住的摘要摘要才能与落盘字节一致。
    let legacy_summary_fields =
        depth_legacy_summary_fields(spec, &manifest, &run_id, &tier, initial_cash_raw, report);
    let persisted_artifacts = persist_backtest_artifacts_with_summary(
        &manifest_path,
        &BacktestArtifactInput {
            runtime_config_path: spec.runtime_config_path.as_deref().map(Path::new),
            market_spec_path: spec.market_spec_path.as_deref().map(Path::new),
            strategy_id: &strategy_id,
            instrument: &input.frame.instrument,
            sample_unit: DEPTH_SAMPLE_UNIT,
            samples: input.frame.snapshots.len(),
            sample_ts: BacktestSampleTimestamps::Values(&report.snapshot_ts),
            equity: &report.equity,
            positions: &report.positions,
            fills: &report.fills,
            execution_attempts: &report.execution_attempts,
            clock_start: report.clock_start,
            clock_end: report.clock_end,
            input_data_hash: report.input_data_hash,
            result_hash: report.result_hash(),
            event_log: &report.event_log,
            ledger: &report.ledger,
            issuer_capital: &[],
            issuer_capital_at_end: None,
            return_bps: report.return_bps,
            max_drawdown_bps: report.max_drawdown_bps,
            fees_raw: report.fees_raw,
            turnover_raw: report.turnover_raw,
            final_equity_raw: report.final_equity(),
            assumptions: &report.assumptions,
            model_descriptors: &report.model_descriptors,
            risk_rule_set_version: input.risk_rule_set_version,
            risk_rule_source: input.risk_rule_source,
            cost_source: input.cost_source,
            // 深度链不经过 FillModel：撮合口径是四参数描述子，摘要里不该出现 fill_model 键。
            fill_model: None,
            fill_model_details: None,
            execution_model_details: Some(&report.execution_model_descriptor),
            account_base,
            matching_kernel,
            rejections: &rejections,
            input: input_provenance,
            signal: Some(signal_params(spec)),
            dataset_quality: input.dataset_quality,
            account_id: input.account_id,
            portfolio: input.portfolio.as_ref(),
            summary_extra: Some(&legacy_summary_fields),
        },
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("深度回测产物落盘失败: {error}"),
        )
    })?;
    let summary_path = persisted_artifacts.summary_path;
    let equity_path = persisted_artifacts.equity_path;
    let fills_path = persisted_artifacts.fills_path;
    let summary = persisted_artifacts.summary;
    let artifacts = BacktestArtifacts {
        run_manifest: manifest_path.to_string_lossy().into_owned(),
        summary: summary_path.to_string_lossy().into_owned(),
        equity: equity_path.to_string_lossy().into_owned(),
        fills: fills_path.to_string_lossy().into_owned(),
    };
    Ok(PersistedDepthRun {
        manifest,
        artifacts,
        summary,
    })
}
