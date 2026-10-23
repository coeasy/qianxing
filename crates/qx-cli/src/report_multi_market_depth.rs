//! Quality and portfolio readout for persisted multi-market depth runs.

use crate::report_multi_market_depth_render::{
    render_comparison_html, render_html, strval, workbook_rows,
};
use crate::report_output_paths::same_output_target;
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

fn read_artifact(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let payload = std::fs::read(path)
        .map_err(|error| format!("读取报告输入失败 {}: {error}", path.display()))?;
    let Ok(value) = serde_json::from_slice::<Value>(&payload) else {
        return Ok(None);
    };
    if value.pointer("/outcome/shared_result_hash").is_some()
        && value.pointer("/outcome/pending_orders").is_some()
        && value.get("market_data").and_then(Value::as_array).is_some()
        && value.pointer("/spec/markets").is_some()
    {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

// Keep CLI report options explicit; the caller owns their parsing and validation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn maybe_run(
    path: &Path,
    as_json: bool,
    html: bool,
    evidence: bool,
    output: Option<&Path>,
    against: &[PathBuf],
    xlsx: Option<&Path>,
    strategy_diagnostics: Option<&Path>,
) -> Result<bool, String> {
    let Some(artifact) = read_artifact(path)? else {
        return Ok(false);
    };
    if evidence && !against.is_empty() {
        return Err("多市场深度 EvidenceBundle 只支持单次运行，不能与 --against 一起使用".into());
    }
    if strategy_diagnostics.is_some() && !html && !as_json && xlsx.is_none() {
        return Err("--strategy-diagnostics 必须与 --html、--json 或 --xlsx 一起使用".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|html_path| {
        xlsx.is_some_and(|xlsx_path| same_output_target(html_path, xlsx_path))
    }) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    let mut reports = vec![build_report(path, &artifact)?];
    let mut seen = std::collections::BTreeSet::from([path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())]);
    for candidate in against {
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("解析对照运行路径失败 {}: {error}", candidate.display()))?;
        if !seen.insert(canonical) {
            return Err(format!(
                "多市场深度对照不能重复输入: {}",
                candidate.display()
            ));
        }
        let Some(value) = read_artifact(candidate)? else {
            return Err(format!(
                "--against 只接受多市场深度运行产物: {}",
                candidate.display()
            ));
        };
        reports.push(build_report(candidate, &value)?);
    }
    let mut source_paths = reports
        .iter()
        .filter_map(|report| report["summary_path"].as_str().map(PathBuf::from))
        .collect::<Vec<_>>();
    source_paths.extend(strategy_diagnostics.map(Path::to_path_buf));
    let comparison = reports.len() > 1;
    if comparison {
        let fingerprint = reports[0]["data_fingerprint"].clone();
        let instruments = market_instruments(&reports[0])?;
        for report in &reports {
            if report["data_fingerprint"] != fingerprint
                || market_instruments(report)? != instruments
            {
                return Err("多市场深度对照要求市场顺序和数据指纹完全相同".into());
            }
        }
        reports.sort_by(|left, right| {
            right["return_bps"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["return_bps"].as_i64().unwrap_or_default())
                .then_with(|| {
                    left["max_drawdown_bps"]
                        .as_u64()
                        .cmp(&right["max_drawdown_bps"].as_u64())
                })
                .then_with(|| left["run_id"].as_str().cmp(&right["run_id"].as_str()))
        });
    }
    if comparison {
        if let Some(diagnostics_path) = strategy_diagnostics {
            bind_depth_comparison_diagnostics(diagnostics_path, &mut reports)?;
        }
    }
    let mut report = if comparison {
        json!({"comparison_kind":"same_multi_market_depth_input","data_fingerprint":reports[0]["data_fingerprint"],"markets":market_instruments(&reports[0])?,"ranking":reports.iter().enumerate().map(|(index, report)|json!({"rank":index+1,"run_id":report["run_id"],"return_bps":report["return_bps"],"max_drawdown_bps":report["max_drawdown_bps"],"successful_fills":report["successful_fills"],"result_hash":report["result_hash"],"summary_path":report["summary_path"]})).collect::<Vec<_>>(),"runs":reports.iter().map(|report|json!({"run_id":report["run_id"],"market_strategy_diagnostics":report.get("market_strategy_diagnostics").cloned().unwrap_or(Value::Null)})).collect::<Vec<_>>()})
    } else {
        reports.remove(0)
    };
    let mut diagnostics_payload = None;
    if !comparison {
        if let Some(diagnostics_path) = strategy_diagnostics {
            let bound = bind_market_depth_diagnostics(diagnostics_path, &artifact, &report)?;
            diagnostics_payload = Some(Value::Array(
                bound
                    .as_array()
                    .ok_or("多市场深度诊断结果必须是数组")?
                    .iter()
                    .map(|item| item["report"].clone())
                    .collect(),
            ));
            report["market_strategy_diagnostics"] = bound;
        }
    }
    let html_path = if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("multi-market-depth.json");
            path.with_file_name(name.replace(
                ".multi-market-depth.json",
                if comparison {
                    ".multi-market-depth.comparison.html"
                } else {
                    ".multi-market-depth.report.html"
                },
            ))
        });
        if !target
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("html") || value.eq_ignore_ascii_case("htm")
            })
        {
            return Err(format!(
                "HTML 报告输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        if source_paths
            .iter()
            .any(|source| same_output_target(&target, source))
        {
            return Err("HTML 输出不能覆盖多市场深度输入产物".into());
        }
        let rendered = if comparison {
            render_comparison_html(&report)
        } else {
            render_html(&report)
        };
        if let Some(parent) = target
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建报告目录失败: {error}"))?;
        }
        qx_app::artifact_store::sink::publish(
            &target,
            &rendered,
            qx_app::artifact_store::sink::PublishMode::Overwrite,
        )
        .map_err(|error| format!("发布多市场深度 HTML 失败: {error}"))?;
        Some(target)
    } else {
        None
    };
    let xlsx_path = if let Some(target) = xlsx {
        if source_paths
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("XLSX 输出不能覆盖多市场深度输入产物".into());
        }
        if comparison {
            let overview = vec![
                vec!["Field".into(), "Value".into()],
                vec![
                    "Data fingerprint".into(),
                    strval(&report["data_fingerprint"]),
                ],
                vec![
                    "Comparison kind".into(),
                    "same_multi_market_depth_input".into(),
                ],
            ];
            let mut ranking = vec![vec![
                "rank".into(),
                "run_id".into(),
                "return_bps".into(),
                "max_drawdown_bps".into(),
                "successful_fills".into(),
                "result_hash".into(),
                "summary_path".into(),
            ]];
            for row in report["ranking"].as_array().into_iter().flatten() {
                ranking.push(
                    [
                        "rank",
                        "run_id",
                        "return_bps",
                        "max_drawdown_bps",
                        "successful_fills",
                        "result_hash",
                        "summary_path",
                    ]
                    .into_iter()
                    .map(|key| strval(&row[key]))
                    .collect(),
                );
            }
            Some(
                crate::report_xlsx::write_multi_market_depth_comparison_xlsx(
                    target,
                    overview,
                    ranking,
                    report.get("runs"),
                )?,
            )
        } else {
            let (overview, quality) = workbook_rows(&report);
            Some(crate::report_xlsx::write_multi_market_depth_xlsx(
                target,
                overview,
                quality,
                report.get("market_strategy_diagnostics"),
            )?)
        }
    } else {
        None
    };
    let evidence_path = if evidence {
        let diagnostics = strategy_diagnostics.zip(diagnostics_payload.as_ref());
        Some(
            crate::run_evidence_multi_market::write_multi_market_depth_run_evidence(
                path,
                diagnostics,
            )?,
        )
    } else {
        None
    };
    if as_json {
        let mut value = json!({"schema_version":1,"report_kind":if comparison {"multi_market_depth_comparison"} else {"multi_market_depth"},"report":report});
        if let Some(path) = html_path {
            value["generated_artifacts"]["html"] = json!(path);
        }
        if let Some(path) = xlsx_path {
            value["generated_artifacts"]["xlsx"] = json!(path);
        }
        if let Some(path) = evidence_path {
            value["generated_artifacts"]["evidence"] = json!(path);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
        );
    } else {
        if let Some(path) = html_path {
            println!("[Report] html={}", path.display());
        }
        if let Some(path) = xlsx_path {
            println!("[Report] xlsx={}", path.display());
        }
        if let Some(path) = evidence_path {
            println!("[Report] evidence={}", path.display());
        }
        if comparison {
            println!(
                "[Report] comparison_fingerprint={} compared_runs={}",
                report["data_fingerprint"],
                report["ranking"].as_array().map_or(0, Vec::len)
            );
        } else {
            println!(
                "[Report] run_id={} return_bps={} max_drawdown_bps={} markets={}",
                report["run_id"],
                report["return_bps"],
                report["max_drawdown_bps"],
                report["markets"].as_array().map_or(0, Vec::len)
            );
        }
    }
    Ok(true)
}

fn market_instruments(report: &Value) -> Result<Vec<Value>, String> {
    report
        .get("markets")
        .and_then(Value::as_array)
        .ok_or_else(|| "多市场深度报告缺少 markets 列表".to_string())?
        .iter()
        .map(|market| {
            market
                .get("instrument")
                .cloned()
                .ok_or_else(|| "多市场深度报告的市场条目缺少 instrument".to_string())
        })
        .collect()
}

fn build_report(path: &Path, artifact: &Value) -> Result<Value, String> {
    let outcome = artifact
        .get("outcome")
        .ok_or("多市场深度产物缺少 outcome")?;
    let markets = artifact
        .get("market_data")
        .and_then(Value::as_array)
        .ok_or("多市场深度产物缺少 market_data")?;
    let mut rows = Vec::with_capacity(markets.len());
    let mut data_hasher = qx_core::Fnv1a::new();
    let mut input_components = std::collections::BTreeMap::new();
    for market in markets {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or("market_data 缺少 instrument")?;
        let depth_path = market
            .get("path")
            .and_then(Value::as_str)
            .ok_or("market_data 缺少 depth path")?;
        let frame = crate::read_depth_frame_for_backtest(Path::new(depth_path))?;
        if frame.instrument.to_string() != instrument
            || market.get("content_hash").and_then(Value::as_str)
                != Some(format!("{:016x}", frame.input_hash()).as_str())
            || market.get("source").and_then(Value::as_str) != Some(frame.source.as_str())
            || market.get("snapshot_count").and_then(Value::as_u64)
                != Some(frame.snapshots.len() as u64)
        {
            return Err(format!("{instrument} 深度输入身份与运行产物不一致"));
        }
        let tier = market
            .get("data_tier")
            .and_then(Value::as_str)
            .ok_or("market_data 缺少 data_tier")?;
        if !matches!(tier, "l1" | "l2") {
            return Err(format!("{instrument} 深度档位非法"));
        }
        data_hasher.write_text(instrument);
        data_hasher.write_text(tier);
        data_hasher.write_text(&frame.source);
        data_hasher.write_u64(frame.input_hash());
        input_components.insert(
            format!("depthframe:{instrument}"),
            format!("depth:{tier}:{}:{:016x}", frame.source, frame.input_hash()),
        );
        let quality = match market
            .get("dataset_quality")
            .filter(|value| !value.is_null())
        {
            Some(quality) => Some(verify_quality(
                instrument, depth_path, tier, &frame, quality,
            )?),
            None => None,
        };
        if let Some(quality) = &quality {
            let fingerprint = quality["manifest_fingerprint"]
                .as_str()
                .ok_or("质量清单缺少 fingerprint")?;
            data_hasher.write_text(fingerprint);
            input_components.insert(
                format!("dataset-quality:{instrument}"),
                fingerprint.to_string(),
            );
        }
        rows.push(json!({
            "instrument": instrument,
            "data_tier": tier,
            "source": frame.source,
            "content_hash": market["content_hash"],
            "data_fingerprint": format!("depth:{tier}:{}:{}", frame.source, market["content_hash"].as_str().unwrap_or_default()),
            "snapshot_count": frame.snapshots.len(),
            "source_end_ts": frame.snapshots.last().map(|snapshot| snapshot.ts),
            "fills": artifact.pointer(&format!("/per_market_fills/{instrument}")).cloned().unwrap_or(json!(0)),
            "dataset_quality": quality,
        }));
    }
    let composed = format!("{:016x}", data_hasher.finish());
    let run_manifest_path = artifact["run_manifest_path"]
        .as_str()
        .ok_or("多市场深度产物缺少 RunManifest 路径")?;
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(run_manifest_path)
            .map_err(|error| format!("读取 RunManifest 失败: {error}"))?,
    )?;
    if composed != outcome["data_fingerprint"].as_str().unwrap_or_default()
        || manifest.data_fingerprint != composed
        || manifest.input_components != input_components
        || manifest.result_hash != outcome["shared_result_hash"].as_str().unwrap_or_default()
    {
        return Err("多市场深度报告输入与 RunManifest 身份不一致".into());
    }
    Ok(json!({
        "run_id": outcome["run_id"], "account_id": outcome["account_id"],
        "result_hash": outcome["shared_result_hash"], "data_fingerprint": outcome["data_fingerprint"],
        "markets_total": outcome["markets_total"], "successful_fills": outcome["successful_fills"],
        "pending_orders": outcome["pending_orders"], "equity_points": outcome["equity_points"],
        "final_equity_raw": outcome["final_equity_raw"], "return_bps": outcome["return_bps"],
        "max_drawdown_bps": outcome["max_drawdown_bps"], "summary_path": path, "markets": rows
    }))
}

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
    let report = build_report(artifact_path, &artifact)?;
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

fn bind_market_depth_diagnostics(
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

fn bind_market_depth_diagnostics_value(
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

fn bind_depth_comparison_diagnostics(
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

fn parse_depth_comparison_diagnostics(payload: &Value) -> Result<BTreeMap<String, Value>, String> {
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
    let report = build_report(artifact_path, &artifact)?;
    bind_market_depth_diagnostics_value(diagnostics, &artifact, &report).map(|_| ())
}

fn verify_quality(
    instrument: &str,
    frame_path: &str,
    tier: &str,
    frame: &qx_xingban::DepthFrame,
    quality: &Value,
) -> Result<Value, String> {
    let manifest_path = quality
        .get("manifest_path")
        .and_then(Value::as_str)
        .ok_or("质量摘要缺少 manifest_path")?;
    let bytes = std::fs::read(manifest_path)
        .map_err(|error| format!("读取 {instrument} 质量清单失败: {error}"))?;
    let sha = qx_data::sha256_hex_bytes(&bytes);
    if quality.get("manifest_sha256").and_then(Value::as_str) != Some(sha.as_str()) {
        return Err(format!("{instrument} 清单 SHA-256 不匹配"));
    }
    let manifest = qx_data::DatasetManifestV2::from_json(
        std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
    )?;
    let expected_tier = if tier == "l1" {
        qx_data::DatasetTier::L1
    } else {
        qx_data::DatasetTier::L2
    };
    let input_hash = qx_strategy::file_digest::sha256_file_hex(Path::new(frame_path))?;
    let count = u64::try_from(frame.snapshots.len()).map_err(|_| "DepthFrame 快照数溢出")?;
    let interval = manifest
        .quality_report
        .interval_ms
        .filter(|value| *value > 0)
        .ok_or("质量清单缺少正 interval_ms")?;
    let (duplicates, out_of_order, missing) = frame
        .snapshots
        .windows(2)
        .try_fold((0_u64, 0_u64, 0_u64), |(d, o, m), pair| {
            if pair[1].ts < pair[0].ts {
                Some((d, o.checked_add(1)?, m))
            } else if pair[1].ts == pair[0].ts {
                Some((d.checked_add(1)?, o, m))
            } else {
                Some((
                    d,
                    o,
                    m.checked_add((pair[1].ts - pair[0].ts).div_ceil(interval) - 1)?,
                ))
            }
        })
        .ok_or("深度质量统计溢出")?;
    if manifest.instrument != instrument
        || manifest.tier != expected_tier
        || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
        || manifest.content_hash != input_hash
        || manifest.dataset.fingerprint != format!("{:016x}", frame.input_hash())
        || manifest.dataset.source != frame.source
        || manifest.dataset.schema_version != frame.schema_version
        || manifest.dataset.start_timestamp != frame.snapshots.first().ok_or("DepthFrame 为空")?.ts
        || manifest.dataset.end_timestamp != frame.snapshots.last().ok_or("DepthFrame 为空")?.ts
        || manifest.quality_report.row_count != count
        || manifest.quality_report.usable_tiers != [tier]
        || manifest.quality_report.duplicate_rows != duplicates
        || manifest.quality_report.out_of_order_rows != out_of_order
        || manifest.quality_report.missing_intervals != missing
        || quality.get("manifest_fingerprint").and_then(Value::as_str)
            != Some(manifest.fingerprint()?.as_str())
        || quality.get("quality_report")
            != Some(
                &serde_json::to_value(&manifest.quality_report)
                    .map_err(|error| error.to_string())?,
            )
    {
        return Err(format!("{instrument} 质量清单与 DepthFrame 不一致"));
    }
    Ok(
        json!({"manifest_fingerprint": manifest.fingerprint()?, "manifest_sha256": sha, "quality_report": manifest.quality_report, "status": if missing + duplicates + out_of_order == 0 {"clean"} else {"issues"}}),
    )
}

#[cfg(test)]
mod diagnostics_tests {
    use super::{
        bind_depth_comparison_diagnostics, bind_market_depth_diagnostics_value,
        parse_depth_comparison_diagnostics,
    };
    use serde_json::{json, Value};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn inputs() -> (Value, Value, Value) {
        let artifact = json!({
            "spec": {"markets": [{
                "instrument": "BTCUSDT.BINANCE",
                "data_tier": "l2",
                "strategy": {"strategy_id": "sma-btc"}
            }]},
            "strategy_identities": []
        });
        let report = json!({
            "markets": [{
                "instrument": "BTCUSDT.BINANCE",
                "data_fingerprint": "depth:l2:binance:abcd",
                "snapshot_count": 3,
                "source_end_ts": 300
            }]
        });
        let diagnostics = json!([{
            "schema_version": 1,
            "kind": "qianxing-strategy-bias-diagnostics",
            "strategy_id": "sma-btc",
            "instrument": "BTCUSDT.BINANCE",
            "data_fingerprint": "depth:l2:binance:abcd",
            "input_kind": "depth_midpoint_bar",
            "input_events": 3,
            "as_of": 300,
            "lookahead": {"status": "no_change_detected"},
            "startup": {"status": "stable_across_requested_windows"}
        }]);
        (artifact, report, diagnostics)
    }

    #[test]
    fn depth_diagnostic_attachment_binds_market_strategy_and_event_coverage() {
        let (artifact, report, diagnostics) = inputs();
        let bound = bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
            .expect("matching diagnostic attachment");
        assert_eq!(bound[0]["identity_bound"], true);
        assert_eq!(bound[0]["report"]["input_events"], 3);

        let mut rendered_report = report;
        rendered_report["run_id"] = json!("run-1");
        rendered_report["result_hash"] = json!("result-1");
        rendered_report["markets_total"] = json!(1);
        rendered_report["successful_fills"] = json!(0);
        rendered_report["pending_orders"] = json!(0);
        rendered_report["equity_points"] = json!(1);
        rendered_report["final_equity_raw"] = json!(100);
        rendered_report["return_bps"] = json!(0);
        rendered_report["max_drawdown_bps"] = json!(0);
        rendered_report["market_strategy_diagnostics"] = bound;
        let html = crate::report_multi_market_depth_render::render_html(&rendered_report);
        assert!(html.contains("diagnostic-filter"));
        assert!(html.contains("BTCUSDT.BINANCE"));
        assert!(html.contains("stable_across_requested_windows"));
    }

    #[test]
    fn depth_diagnostic_attachment_rejects_wrong_coverage_or_strategy_slot() {
        let (artifact, report, mut diagnostics) = inputs();
        diagnostics[0]["input_events"] = json!(2);
        assert!(
            bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
                .unwrap_err()
                .contains("事件数")
        );

        let (_, report, mut diagnostics) = inputs();
        diagnostics[0]["strategy_id"] = json!("other-strategy");
        assert!(
            bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
                .unwrap_err()
                .contains("策略槽")
        );

        let (_, report, mut diagnostics) = inputs();
        diagnostics[0]["input_kind"] = json!("order_book");
        assert!(
            bind_market_depth_diagnostics_value(&diagnostics, &artifact, &report)
                .unwrap_err()
                .contains("输入形态")
        );
    }

    #[test]
    fn depth_comparison_diagnostics_are_bound_by_exact_run_identity() {
        let (mut artifact, mut report, diagnostics) = inputs();
        artifact["spec"]["run_id"] = json!("run-alpha");
        artifact["outcome"] = json!({"shared_result_hash":"hash-alpha","pending_orders":[]});
        artifact["market_data"] = json!([]);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after unix epoch")
            .as_nanos();
        let summary_path = std::env::temp_dir().join(format!(
            "qx-depth-comparison-{nonce}.multi-market-depth.json"
        ));
        std::fs::write(
            &summary_path,
            serde_json::to_vec(&artifact).expect("serialize summary fixture"),
        )
        .expect("write summary fixture");
        report["run_id"] = json!("run-alpha");
        report["summary_path"] = json!(summary_path.to_string_lossy());
        let mut reports = vec![report];
        let payload = json!({
            "schema_version": 1,
            "kind": "qianxing-multi-market-depth-diagnostic-comparison",
            "runs": [{"run_id":"run-alpha","diagnostics":diagnostics}]
        });
        let parsed = parse_depth_comparison_diagnostics(&payload).expect("parse comparison input");
        assert_eq!(parsed.len(), 1);
        let diagnostics_path = summary_path.with_extension("diagnostics.json");
        std::fs::write(
            &diagnostics_path,
            serde_json::to_vec(&payload).expect("serialize comparison diagnostics"),
        )
        .expect("write comparison diagnostics");
        bind_depth_comparison_diagnostics(&diagnostics_path, &mut reports)
            .expect("bind per-run diagnostic to reopened summary");
        assert_eq!(
            reports[0]["market_strategy_diagnostics"][0]["identity_bound"],
            true
        );
        assert_eq!(
            reports[0]["market_strategy_diagnostics"][0]["report"]["input_events"],
            3
        );

        let html = crate::report_multi_market_depth_render::render_comparison_html(&json!({
            "data_fingerprint":"same-input",
            "ranking":[{"rank":1,"run_id":"run-alpha","return_bps":25,"max_drawdown_bps":4,"successful_fills":2,"result_hash":"hash-alpha"}],
            "runs":[{"run_id":"run-alpha","market_strategy_diagnostics":reports[0]["market_strategy_diagnostics"]}]
        }));
        assert!(html.contains("comparison-diagnostic-filter"));
        assert!(html.contains("run-alpha"));
        assert!(html.contains("stable_across_requested_windows"));
        let _ = std::fs::remove_file(summary_path);
        let _ = std::fs::remove_file(diagnostics_path);
    }

    #[test]
    fn depth_comparison_diagnostics_reject_duplicate_or_missing_run_ids() {
        let mut duplicate = json!({
            "schema_version":1,
            "kind":"qianxing-multi-market-depth-diagnostic-comparison",
            "runs":[
                {"run_id":"run-alpha","diagnostics":[]},
                {"run_id":"run-alpha","diagnostics":[]}
            ]
        });
        assert!(parse_depth_comparison_diagnostics(&duplicate)
            .unwrap_err()
            .contains("重复 run_id"));
        duplicate["runs"] = json!([{"run_id":"run-alpha","diagnostics":[]}]);
        assert_eq!(
            parse_depth_comparison_diagnostics(&duplicate)
                .unwrap()
                .len(),
            1
        );
        duplicate["kind"] = json!("unknown");
        assert!(parse_depth_comparison_diagnostics(&duplicate).is_err());
    }
}
