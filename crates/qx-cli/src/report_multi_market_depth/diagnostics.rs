//! Rebuild and bind bounded built-in strategy diagnostics to verified depth reports.

use super::{build_report, read_artifact, same_output_target};
use qx_core::InstrumentId;
use qx_strategy::diagnostics::{
    diagnose_future_perturbations, diagnose_startup_trajectory, diagnose_startup_windows,
};
use qx_strategy::{BuiltinStrategy, MarketEvent, Strategy, StrategyContext};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_MARKET_DIAGNOSTICS_BYTES: u64 = 1024 * 1024;
const MAX_MARKET_DIAGNOSTICS: usize = 32;
const MAX_DEPTH_COMPARISON_RUNS_WITH_DIAGNOSTICS: usize = 32;

/// Recompute bounded startup diagnostics from the same verified DepthFrame and built-in
/// strategy configuration used by a persisted multi-market depth run.
pub(crate) fn generate_builtin_depth_diagnostics(
    artifact_path: &Path,
    output_path: &Path,
) -> Result<(), String> {
    let artifact =
        read_artifact(artifact_path)?.ok_or("strategy diagnose-depth 只接受多市场深度运行产物")?;
    // Re-run all report-side identity checks before invoking strategy code. This validates each
    // source file, optional quality manifest, combined fingerprint, and RunManifest digest.
    let report = build_report(artifact_path, &artifact, false)?;
    let spec_json = serde_json::to_string(artifact.get("spec").ok_or("多市场深度产物缺少 spec")?)
        .map_err(|error| format!("序列化多市场深度 spec 失败: {error}"))?;
    let spec = qx_app::MultiMarketDepthBacktestSpec::from_json(&spec_json)
        .map_err(|error| format!("多市场深度 spec 无效: {error}"))?;
    let injected_ids = artifact
        .get("strategy_identities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|identity| identity.get("strategy_id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    if !injected_ids.is_empty() {
        return Err(format!(
            "运行包含外部策略槽 {}；自动诊断目前只支持可从产物重建的 Rust 内置策略",
            injected_ids.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let mut protected_paths = vec![artifact_path.to_path_buf()];
    if let Some(path) = artifact.get("run_manifest_path").and_then(Value::as_str) {
        protected_paths.push(PathBuf::from(path));
    }
    for market in artifact
        .get("market_data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(path) = market.get("path").and_then(Value::as_str) {
            protected_paths.push(PathBuf::from(path));
        }
        if let Some(path) = market
            .pointer("/dataset_quality/manifest_path")
            .and_then(Value::as_str)
        {
            protected_paths.push(PathBuf::from(path));
        }
    }
    if protected_paths
        .iter()
        .any(|source| same_output_target(output_path, source))
    {
        return Err("诊断 JSON 输出不能覆盖运行产物、RunManifest、DepthFrame 或质量清单".into());
    }

    let market_data = artifact["market_data"]
        .as_array()
        .ok_or("多市场深度产物缺少 market_data")?;
    let report_markets = report["markets"]
        .as_array()
        .ok_or("复核后的多市场报告缺少 markets")?;
    let mut diagnostics = Vec::with_capacity(spec.markets.len());
    for configured in &spec.markets {
        let strategy_spec = &configured.strategy;
        if strategy_spec.target_notional_raw.is_some()
            || strategy_spec.target_available_margin_bps.is_some()
            || strategy_spec.target_volatility_bps.is_some()
            || strategy_spec.target_kelly_fraction_bps.is_some()
        {
            return Err(format!(
                "{} 使用依赖动态账户状态的 sizing；当前自动预热诊断无法重建逐时点持仓/购买力，已拒绝生成",
                configured.instrument
            ));
        }
        let market_data_row = market_data
            .iter()
            .find(|row| row["instrument"].as_str() == Some(configured.instrument.as_str()))
            .ok_or_else(|| format!("运行产物缺少 {} 的数据输入", configured.instrument))?;
        let market_report = report_markets
            .iter()
            .find(|row| row["instrument"].as_str() == Some(configured.instrument.as_str()))
            .ok_or_else(|| format!("复核报告缺少 {} 的数据身份", configured.instrument))?;
        let frame_path = market_data_row["path"]
            .as_str()
            .ok_or("market_data 缺少 DepthFrame 路径")?;
        let frame_payload = std::fs::read_to_string(frame_path)
            .map_err(|error| format!("读取 {} DepthFrame 失败: {error}", configured.instrument))?;
        let frame = qx_xingban::DepthFrame::from_json(&frame_payload)
            .map_err(|error| format!("{} DepthFrame 无效: {error}", configured.instrument))?;
        let instrument = InstrumentId::parse(&configured.instrument)
            .ok_or_else(|| format!("InstrumentId 无效: {}", configured.instrument))?;
        let mut events = Vec::with_capacity(frame.snapshots.len());
        for snapshot in &frame.snapshots {
            let (Some(bid), Some(ask)) = (snapshot.bids.first(), snapshot.asks.first()) else {
                return Err(format!(
                    "{} 含缺少买一或卖一的快照，无法复现 DepthBarStrategy 输入",
                    configured.instrument
                ));
            };
            let mid = bid.price.raw().saturating_add(ask.price.raw()) / 2;
            let volume = snapshot
                .bids
                .iter()
                .chain(snapshot.asks.iter())
                .fold(0_i128, |sum, level| sum.saturating_add(level.qty.raw()));
            events.push(MarketEvent::Bar {
                instrument: instrument.clone(),
                ts: snapshot.ts,
                open_raw: mid,
                high_raw: mid,
                low_raw: mid,
                close_raw: mid,
                volume_raw: volume,
                exact_ohlcv: None,
            });
        }
        if events.len() != frame.snapshots.len() {
            return Err(format!(
                "{} 深度快照到策略事件转换数量不一致",
                configured.instrument
            ));
        }
        let fingerprint = market_report["data_fingerprint"]
            .as_str()
            .ok_or("复核后的市场数据缺少指纹")?;
        let context = StrategyContext {
            strategy_id: strategy_spec.strategy_id.clone(),
            strategy_version: format!("{}-v1", strategy_spec.strategy_id),
            account_id: spec.account_id.clone(),
            venue_id: instrument.venue.to_string(),
            data_fingerprint: fingerprint.to_string(),
            as_of: events.first().map(MarketEvent::ts).ok_or("深度输入为空")?,
            positions: BTreeMap::new(),
            cash: BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]),
            available_margin_raw: None,
            risk_state: "backtest_diagnostic_fixed_context".into(),
            positions_exact: BTreeMap::new(),
            cash_exact: BTreeMap::new(),
            available_margin_exact: None,
        };
        let runtime_strategy = qx_runtime::StrategyRuntimeConfig {
            id: Some(strategy_spec.strategy_id.clone()),
            version: format!("{}-v1", strategy_spec.strategy_id),
            target_qty: strategy_spec.quantity_raw,
            builtin_strategy: Some(strategy_spec.kind.name().to_string()),
            builtin_fast_window: Some(strategy_spec.fast_window),
            builtin_slow_window: Some(strategy_spec.slow_window),
            builtin_period: Some(strategy_spec.period),
            builtin_threshold_bps: Some(strategy_spec.threshold_bps),
            ..qx_runtime::StrategyRuntimeConfig::default()
        };
        let mut strategy_config =
            qx_runtime::builtin_strategy_config_from_runtime(&runtime_strategy, &instrument)?;
        strategy_config.primary_policy = configured.order_policy;
        strategy_config.validate()?;

        // Keep the three comparison windows tied to the configured strategy scale, while
        // bounding diagnostic work for unusually large periods.
        let anchor = strategy_spec
            .fast_window
            .max(strategy_spec.slow_window)
            .max(strategy_spec.period)
            .clamp(20, 250);
        let startup_windows = [anchor, anchor * 2, anchor * 4];
        let startup = diagnose_startup_windows(
            &context,
            &events,
            &startup_windows,
            || -> Result<Box<dyn Strategy>, String> {
                Ok(Box::new(BuiltinStrategy::new(strategy_config.clone())?))
            },
        )?;
        let trajectory =
            diagnose_startup_trajectory(&context, &events, &startup_windows, 96, || {
                Ok(Box::new(BuiltinStrategy::new(strategy_config.clone())?) as Box<dyn Strategy>)
            })?;
        let lookahead = diagnose_future_perturbations(&context, &events, 16, || {
            Ok(Box::new(BuiltinStrategy::new(strategy_config.clone())?) as Box<dyn Strategy>)
        })?;
        let startup_windows_json = startup
            .windows
            .iter()
            .map(|window| {
                json!({
                    "requested_events": window.requested_bars,
                    "available_events": window.available_bars,
                    "history_truncated": window.history_truncated,
                    "decision": window.decision,
                    "changed_decision_fields": window.changed_fields,
                    "intermediate_state": window.intermediate_state,
                    "changed_intermediate_fields": window.changed_intermediate_fields,
                })
            })
            .collect::<Vec<_>>();
        diagnostics.push(json!({
            "schema_version": 1,
            "kind": "qianxing-strategy-bias-diagnostics",
            "strategy": "rust_builtin",
            "worker_kind": "rust_builtin_replay",
            "strategy_id": strategy_spec.strategy_id,
            "strategy_version": strategy_config.strategy_version,
            "instrument": configured.instrument,
            "data_fingerprint": fingerprint,
            "input_kind": "depth_midpoint_bar",
            "input_events": events.len(),
            "as_of": events.last().map(MarketEvent::ts),
            "visible_events_at_as_of": events.len(),
            "lookahead": lookahead,
            "startup": {
                "status": startup.status,
                "reference_window_events": startup.reference_window_bars,
                "decision_timestamp": startup.decision_timestamp,
                "windows": startup_windows_json,
                "trajectory": trajectory,
                "interpretation": startup.interpretation,
                "context_assumption": "Each replay uses a fixed initial account cash snapshot and empty positions. Dynamic sizing strategies are rejected; event-time portfolio state is not reconstructed."
            },
            "data_visibility": {
                "status": "visibility_unknown",
                "reason": "the run artifact does not contain a verified provider PIT revision ledger with effective_at and available_at facts"
            }
        }));
    }

    let encoded = serde_json::to_string_pretty(&diagnostics)
        .map_err(|error| format!("序列化策略诊断失败: {error}"))?;
    if encoded.len() as u64 > MAX_MARKET_DIAGNOSTICS_BYTES {
        return Err("自动生成的多市场深度诊断超过 1 MiB 上限".into());
    }
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建诊断输出目录失败: {error}"))?;
    }
    qx_app::artifact_store::sink::publish(
        output_path,
        &encoded,
        qx_app::artifact_store::sink::PublishMode::Overwrite,
    )
    .map_err(|error| format!("发布策略诊断文件失败: {error}"))?;
    let perturbation_targets = diagnostics
        .iter()
        .filter_map(|diagnostic| {
            diagnostic
                .pointer("/lookahead/perturbation_targets_checked")
                .and_then(Value::as_u64)
        })
        .sum::<u64>();
    println!(
        "[Strategy Diagnostics] markets={} startup_windows=3 startup_replay_budget_per_market=96 future_perturbation_budget_per_market=16 future_perturbation_targets_checked={} pit=visibility_unknown output={}",
        diagnostics.len(),
        perturbation_targets,
        output_path.display()
    );
    Ok(())
}

pub(super) fn bind_market_depth_diagnostics(
    path: &Path,
    artifact: &Value,
    report: &Value,
) -> Result<Value, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取多市场策略诊断文件属性失败 {}: {error}", path.display()))?
        .len();
    if size > MAX_MARKET_DIAGNOSTICS_BYTES {
        return Err("多市场深度策略诊断集合超过 1 MiB 上限".into());
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取多市场策略诊断文件失败 {}: {error}", path.display()))?;
    let diagnostics: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("多市场策略诊断 JSON 无效 {}: {error}", path.display()))?;
    bind_market_depth_diagnostics_value(&diagnostics, artifact, report)
}

pub(super) fn bind_market_depth_diagnostics_value(
    diagnostics: &Value,
    artifact: &Value,
    report: &Value,
) -> Result<Value, String> {
    let items = diagnostics
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= MAX_MARKET_DIAGNOSTICS)
        .ok_or_else(|| {
            format!("多市场深度策略诊断必须是 1..={MAX_MARKET_DIAGNOSTICS} 项的 JSON 数组")
        })?;
    let specs = artifact["spec"]["markets"]
        .as_array()
        .ok_or("多市场深度 spec 缺少 markets")?;
    let custom_ids = artifact["strategy_identities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|identity| identity.get("strategy_id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    let markets = report["markets"]
        .as_array()
        .ok_or("多市场深度报告缺少 markets")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut bound = Vec::with_capacity(items.len());
    for diagnostic in items {
        let strategy_id = diagnostic
            .get("strategy_id")
            .and_then(Value::as_str)
            .ok_or("多市场深度策略诊断缺少 strategy_id")?;
        let instrument = diagnostic
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or("多市场深度策略诊断缺少 instrument")?;
        if !seen.insert((instrument, strategy_id)) {
            return Err("多市场深度策略诊断集合包含重复策略/标的身份".into());
        }
        let configured = specs
            .iter()
            .find(|market| market["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("策略诊断标的不在已复核 spec 中: {instrument}"))?;
        if configured["strategy"]["strategy_id"].as_str() != Some(strategy_id) {
            return Err(format!("{instrument} 策略诊断不匹配该标的的策略槽"));
        }
        let market = markets
            .iter()
            .find(|market| market["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("策略诊断标的没有已复核的市场数据: {instrument}"))?;
        let fingerprint = market["data_fingerprint"]
            .as_str()
            .ok_or("已复核深度市场缺少数据指纹")?;
        crate::report_command::validate_strategy_diagnostics_identity(
            diagnostic,
            strategy_id,
            instrument,
            fingerprint,
        )?;
        let expected_kind = if !custom_ids.contains(strategy_id) {
            "depth_midpoint_bar"
        } else if configured["data_tier"].as_str().unwrap_or("l2") == "l1" {
            "tick"
        } else {
            "order_book"
        };
        if diagnostic["input_kind"].as_str() != Some(expected_kind)
            || diagnostic["input_events"].as_u64() != market["snapshot_count"].as_u64()
            || diagnostic["as_of"].as_u64() != market["source_end_ts"].as_u64()
            || diagnostic.get("input_bars").is_some()
        {
            return Err(format!(
                "{instrument} 策略诊断输入形态、事件数或结束时点与已复核 DepthFrame 不一致"
            ));
        }
        bound.push(json!({
            "instrument": instrument,
            "strategy_id": strategy_id,
            "identity_bound": true,
            "computation": "client_computed_not_reexecuted_by_cli",
            "report": diagnostic,
        }));
    }
    Ok(Value::Array(bound))
}

pub(super) fn bind_depth_comparison_diagnostics(
    diagnostics_path: &Path,
    reports: &mut [Value],
) -> Result<(), String> {
    if reports.len() > MAX_DEPTH_COMPARISON_RUNS_WITH_DIAGNOSTICS {
        return Err(format!(
            "带逐运行诊断的多市场深度对照最多支持 {MAX_DEPTH_COMPARISON_RUNS_WITH_DIAGNOSTICS} 次运行"
        ));
    }
    let size = std::fs::metadata(diagnostics_path)
        .map_err(|error| format!("读取对照诊断文件属性失败: {error}"))?
        .len();
    if size > MAX_MARKET_DIAGNOSTICS_BYTES {
        return Err("多市场深度对照诊断集合超过 1 MiB 上限".into());
    }
    let bytes = std::fs::read(diagnostics_path)
        .map_err(|error| format!("读取多市场深度对照诊断失败: {error}"))?;
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("多市场深度对照诊断 JSON 无效: {error}"))?;
    let expected_run_ids = reports
        .iter()
        .map(|report| {
            report["run_id"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or("多市场深度对照报告缺少 run_id")
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    if expected_run_ids.len() != reports.len() {
        return Err("多市场深度对照包含重复 run_id，无法绑定诊断".into());
    }
    let mut diagnostics_by_run = parse_depth_comparison_diagnostics(&payload)?;
    let actual_run_ids = diagnostics_by_run
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if actual_run_ids != expected_run_ids {
        return Err("对照诊断 run_id 集合必须与所有被比较运行完全一致".into());
    }
    for report in reports {
        let run_id = report["run_id"]
            .as_str()
            .ok_or("多市场深度对照报告缺少 run_id")?;
        let diagnostics = diagnostics_by_run
            .remove(run_id)
            .ok_or_else(|| format!("对照诊断缺少运行 {run_id}"))?;
        let summary_path = report["summary_path"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| format!("运行 {run_id} 缺少原始摘要路径"))?;
        let artifact = read_artifact(&summary_path)?
            .ok_or_else(|| format!("无法重开运行 {run_id} 的原始深度摘要"))?;
        if artifact.pointer("/spec/run_id").and_then(Value::as_str) != Some(run_id) {
            return Err(format!("运行 {run_id} 的摘要身份发生变化"));
        }
        let bound = bind_market_depth_diagnostics_value(&diagnostics, &artifact, report)?;
        report["market_strategy_diagnostics"] = bound;
    }
    if !diagnostics_by_run.is_empty() {
        return Err("对照诊断包含未被比较的运行".into());
    }
    Ok(())
}

pub(super) fn parse_depth_comparison_diagnostics(
    payload: &Value,
) -> Result<BTreeMap<String, Value>, String> {
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1)
        || payload.get("kind").and_then(Value::as_str)
            != Some("qianxing-multi-market-depth-diagnostic-comparison")
        || payload.as_object().is_none_or(|object| {
            object.len() != 3
                || !object.contains_key("schema_version")
                || !object.contains_key("kind")
                || !object.contains_key("runs")
        })
    {
        return Err("对照诊断必须是 schema_version=1 的 qianxing-multi-market-depth-diagnostic-comparison 对象".into());
    }
    let runs = payload["runs"]
        .as_array()
        .filter(|runs| !runs.is_empty() && runs.len() <= MAX_DEPTH_COMPARISON_RUNS_WITH_DIAGNOSTICS)
        .ok_or_else(|| {
            format!(
                "对照诊断 runs 必须包含 1..={MAX_DEPTH_COMPARISON_RUNS_WITH_DIAGNOSTICS} 个运行"
            )
        })?;
    let mut diagnostics_by_run = BTreeMap::new();
    for item in runs {
        let object = item
            .as_object()
            .filter(|object| {
                object.len() == 2
                    && object.contains_key("run_id")
                    && object.contains_key("diagnostics")
            })
            .ok_or("对照诊断每个 runs 项只能包含 run_id 与 diagnostics")?;
        let run_id = object["run_id"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or("对照诊断 runs 项的 run_id 不能为空")?;
        let diagnostics = object["diagnostics"].clone();
        if !diagnostics.is_array() {
            return Err(format!("对照诊断 {run_id} 的 diagnostics 必须是数组"));
        }
        if diagnostics_by_run
            .insert(run_id.to_owned(), diagnostics)
            .is_some()
        {
            return Err(format!("对照诊断包含重复 run_id: {run_id}"));
        }
    }
    Ok(diagnostics_by_run)
}

pub(crate) fn verify_strategy_diagnostics_for_evidence(
    artifact_path: &Path,
    diagnostics: &Value,
) -> Result<(), String> {
    let artifact =
        read_artifact(artifact_path)?.ok_or("多市场深度诊断证据引用的摘要不是深度回测产物")?;
    let report = build_report(artifact_path, &artifact, false)?;
    bind_market_depth_diagnostics_value(diagnostics, &artifact, &report).map(|_| ())
}
