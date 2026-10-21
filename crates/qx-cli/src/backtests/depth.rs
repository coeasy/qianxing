//! 深度盘口回测的命令行门面：旗标校验 → 行情/配置读入 → 组装 `DepthBacktestSpec` → 交给共享用例。
//!
//! 自 WP-2 起这里只剩门面职责。撮合、风控门构造、成本三级优先级、产物发布，以及"配置声明了
//! 本链接不住的口径"这条拒绝面，全部住在 `qx-app` 的 `DepthBacktestSpec::validate` 与
//! `cases::run_depth_backtest` 里：同一份配置在 CLI / HTTP / Python 三个入口上拿到的是同一条
//! `InvalidInput` 与同一个 `result_hash`，而不是各在门面里抄一份判断。
//!
//! 产物由用例落盘，本文件使用用例随结果返回的内存摘要投影印出原来的 `[Depth · …]`：
//! stdout 与摘要来自同一次投影，不依赖再打开落盘文件。

use super::{
    market_spec_with_margin, read_depth_frame_for_backtest, BuiltinStrategyKind, MarketSpecLoad,
};
use crate::{build_identity, read_runtime_config};
use qx_app::{run_depth_backtest, CallerCapability, DepthBacktestSpec, RunContext};
use std::path::Path;

/// `backtest book` 的两个高保真撮合参数（V11 Q1a）。全 0 就是原有的"逐档吃单"行为；
/// 只有使用者显式点名时才改变撮合，且两项都会写进产物的 `model_descriptors`，
/// 让"能配"与"配了什么"在产物里同口径可见。
///
/// 内核的第三项 `queue_position_bps` 故意不接：它只作用于限价单所在档位，而本入口
/// 只跑内置策略、内置策略只发市价单，非零值换不动任何一笔成交，属于 Q0b 判掉的假旗标。
#[derive(Clone, Copy, Default)]
pub(crate) struct DepthExecutionModel {
    pub(crate) latency_snapshots: u64,
    pub(crate) market_impact_bps: i64,
}

pub(crate) struct DepthBacktestCommand<'a> {
    pub(crate) tier: &'a str,
    pub(crate) strategy_name: &'a str,
    pub(crate) frame_path: &'a Path,
    pub(crate) spec_path: Option<&'a Path>,
    pub(crate) quantity: i64,
    pub(crate) fee_bps: Option<i64>,
    pub(crate) execution: DepthExecutionModel,
    pub(crate) root: &'a Path,
    pub(crate) runtime_config_path: Option<&'a Path>,
    pub(crate) quality_manifest_path: Option<&'a Path>,
}

/// L1/L2 深度档位回测的门面实现。旗标在这里校验，行情在这里读，配置在这里解析成**已解析的
/// 值 + 来源标签**，剩下的全部交给共享用例。
pub(crate) fn run_depth_backtest_with_quality_manifest(
    command: DepthBacktestCommand<'_>,
) -> Result<(), String> {
    let DepthBacktestCommand {
        tier,
        strategy_name,
        frame_path,
        spec_path,
        quantity,
        fee_bps,
        execution,
        root,
        runtime_config_path,
        quality_manifest_path,
    } = command;
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    DepthBacktestSpec::validate_entry(
        tier,
        kind,
        i128::from(quantity) * qx_core::SCALE,
        fee_bps.map(i128::from),
    )
    .map_err(|error| error.to_string())?;
    let frame = read_depth_frame_for_backtest(frame_path)?;
    // 延迟的帧相对上界、L1 档位形状、快照下限都住在用例里：这一族拒绝面只有一处，
    // 三个入口拿到同一条错误（Q0b「能配却不生效」的收口点）。门面只校验纯旗标范围。
    let MarketSpecLoad {
        spec: instrument_spec,
        source: instrument_spec_version,
        ..
    } = market_spec_with_margin(&frame.instrument, spec_path, "深度回测")?;
    // 运行时配置只读一次：风控、成本、信号三项与三条"声明即拒"的口径都从这一份取。
    let runtime = runtime_config_path.map(read_runtime_config).transpose()?;
    let spec = qx_service::backtest_bindings::depth_backtest_spec_from_runtime(
        qx_service::backtest_bindings::DepthBacktestSpecBindingInput {
            tier,
            kind,
            frame: &frame,
            frame_path,
            market_spec: instrument_spec,
            market_spec_path: spec_path,
            market_spec_version: instrument_spec_version,
            quantity,
            fee_bps,
            latency_snapshots: execution.latency_snapshots,
            market_impact_bps: execution.market_impact_bps,
            root,
            runtime: runtime.as_ref(),
            runtime_config_path,
            quality_manifest_path,
        },
    )?;
    let context = RunContext::new(
        CallerCapability::Research,
        format!("book-{}-{}-{}", tier, kind.name(), frame.instrument),
    )
    .with_code_commit(build_identity::BUILD_REVISION);
    let outcome = run_depth_backtest(&spec, &context)
        .map_err(|error| format!("深度档位回测失败: {error}"))?;
    let summary = &outcome.display_summary;
    let execution_model = &summary["execution_model"];
    let account = &summary["account"];
    let signal = &summary["signal"];
    println!(
        "[Depth · Account] initial_cash_raw={} account_base_source={}",
        account["initial_cash_raw"], account["source"]
    );
    println!(
        "[Depth · Backtest] tier={} strategy={} instrument={} snapshots={} fills={} fee_bps={} cost_source={} return_bps={} max_drawdown_bps={} result_hash={}",
        outcome.tier,
        summary["strategy_id"],
        outcome.instrument,
        outcome.snapshots,
        outcome.fills,
        execution_model["fee_bps"],
        summary["execution_costs"]["source"],
        outcome.return_bps,
        outcome.max_drawdown_bps,
        outcome.result_hash,
    );
    println!(
        "[Depth · Execution] latency_snapshots={} market_impact_bps={}",
        execution_model["latency_snapshots"], execution_model["market_impact_bps"]
    );
    println!(
        "[Depth · Signal] source={} fast_window={} slow_window={} period={} threshold_bps={} knobs={} declared_unused={} quantity={}",
        signal["source"],
        signal["fast_window"],
        signal["slow_window"],
        signal["period"],
        signal["threshold_bps"],
        signal["knobs"],
        signal["declared_unused"],
        quantity
    );
    let quality = &summary["dataset_quality"];
    if quality != &serde_json::Value::Null {
        println!(
            "[Data · Quality] manifest_fingerprint={} rows={} interval_ms={} missing_intervals={}",
            quality["manifest_fingerprint"]
                .as_str()
                .unwrap_or("unknown"),
            quality["quality_report"]["row_count"]
                .as_u64()
                .unwrap_or_default(),
            quality["quality_report"]["interval_ms"]
                .as_u64()
                .unwrap_or_default(),
            quality["quality_report"]["missing_intervals"]
                .as_u64()
                .unwrap_or_default(),
        );
    }
    println!(
        "[Depth · Integrity] rejected_orders={} rejection_reasons={}",
        summary["rejected_orders"],
        rejection_reasons_line(&summary["rejection_reasons"])
    );
    println!(
        "[RunManifest] run_id={} data_fingerprint={} result_hash={} digest={}",
        outcome.run_id, outcome.data_fingerprint, outcome.result_hash, outcome.manifest_digest
    );
    println!(
        "[RunManifest] path={}",
        Path::new(&outcome.artifacts.run_manifest).display()
    );
    println!(
        "[Artifacts] summary={} equity={} fills={}",
        Path::new(&outcome.artifacts.summary).display(),
        Path::new(&outcome.artifacts.equity).display(),
        Path::new(&outcome.artifacts.fills).display()
    );
    Ok(())
}

/// 摘要里的拒单分布与 stdout 那行共用一个投影：条数在前、空格换成下划线，与 `trade_facts`
/// 的排序口径一致（条数降序、原因升序）。
fn rejection_reasons_line(reasons: &serde_json::Value) -> String {
    let Some(entries) = reasons.as_array() else {
        return "none".to_string();
    };
    let line = entries
        .iter()
        .filter_map(|entry| {
            Some((
                entry["count"].as_u64()?,
                entry["reason"].as_str()?.replace(' ', "_"),
            ))
        })
        .map(|(count, reason)| format!("{count}x{reason}"))
        .collect::<Vec<_>>()
        .join(";");
    if line.is_empty() {
        "none".to_string()
    } else {
        line
    }
}
