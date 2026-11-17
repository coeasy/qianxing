use super::{build_report, read_artifact, resolve_path};
use crate::report_output_paths::same_output_target;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_MARKET_DIAGNOSTICS_BYTES: u64 = 1024 * 1024;
const MAX_MARKET_DIAGNOSTICS: usize = 512;
const MAX_MARKET_COMPARISON_RUNS_WITH_DIAGNOSTICS: usize = 32;
const MAX_MARKET_DIAGNOSTIC_BARS_PER_STRATEGY: usize = 65_536;
const MAX_MARKET_DIAGNOSTIC_STRATEGY_BAR_EVENTS: usize = 1_000_000;
const MAX_MARKET_DIAGNOSTIC_LOOKAHEAD_WORK: usize = 8_000_000;
const MAX_MARKET_DIAGNOSTIC_STARTUP_WORK: usize = 8_000_000;

pub(super) fn bind_market_bar_comparison_diagnostics(
    diagnostics_path: &Path,
    reports: &mut [Value],
) -> Result<(), String> {
    if reports.len() > MAX_MARKET_COMPARISON_RUNS_WITH_DIAGNOSTICS {
        return Err(format!(
            "带逐运行诊断的多市场 Bar 对照最多支持 {MAX_MARKET_COMPARISON_RUNS_WITH_DIAGNOSTICS} 次运行"
        ));
    }
    let diagnostics = read_market_bar_comparison_diagnostics(diagnostics_path)?;
    let expected_run_ids = reports
        .iter()
        .map(|report| {
            report["run_id"]
                .as_str()
                .filter(|run_id| !run_id.trim().is_empty())
                .map(str::to_owned)
                .ok_or("多市场 Bar 对照报告缺少 run_id")
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    if expected_run_ids.len() != reports.len() {
        return Err("多市场 Bar 对照包含重复 run_id，无法绑定诊断".into());
    }
    let mut diagnostics_by_run = parse_market_bar_comparison_diagnostics(&diagnostics)?;
    let actual_run_ids = diagnostics_by_run
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if actual_run_ids != expected_run_ids {
        return Err("多市场 Bar 对照诊断 run_id 集合必须与所有被比较运行完全一致".into());
    }
    for report in reports {
        let run_id = report["run_id"]
            .as_str()
            .ok_or("多市场 Bar 对照报告缺少 run_id")?;
        let diagnostics = diagnostics_by_run
            .remove(run_id)
            .ok_or_else(|| format!("多市场 Bar 对照诊断缺少运行 {run_id}"))?;
        let summary_path = report["summary_path"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| format!("运行 {run_id} 缺少原始摘要路径"))?;
        let artifact = read_artifact(&summary_path)?
            .ok_or_else(|| format!("无法重开多市场 Bar 运行 {run_id} 的原始摘要"))?;
        if artifact.pointer("/spec/run_id").and_then(Value::as_str) != Some(run_id) {
            return Err(format!("多市场 Bar 运行 {run_id} 的摘要身份发生变化"));
        }
        report["market_strategy_diagnostics"] =
            bind_market_bar_diagnostics_value(&diagnostics, &artifact, report)?;
    }
    if !diagnostics_by_run.is_empty() {
        return Err("多市场 Bar 对照诊断包含未被比较的运行".into());
    }
    Ok(())
}

fn read_market_bar_comparison_diagnostics(path: &Path) -> Result<Value, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| {
            format!(
                "读取多市场 Bar 对照诊断属性失败 {}: {error}",
                path.display()
            )
        })?
        .len();
    if size > MAX_MARKET_DIAGNOSTICS_BYTES {
        return Err("多市场 Bar 对照诊断集合超过 1 MiB 上限".into());
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取多市场 Bar 对照诊断失败 {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("多市场 Bar 对照诊断 JSON 无效: {error}"))
}

pub(super) fn parse_market_bar_comparison_diagnostics(
    payload: &Value,
) -> Result<std::collections::BTreeMap<String, Value>, String> {
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1)
        || payload.get("kind").and_then(Value::as_str)
            != Some("qianxing-multi-market-bar-diagnostic-comparison")
        || payload.as_object().is_none_or(|object| {
            object.len() != 3
                || !object.contains_key("schema_version")
                || !object.contains_key("kind")
                || !object.contains_key("runs")
        })
    {
        return Err(
            "多市场 Bar 对照诊断须为 qianxing-multi-market-bar-diagnostic-comparison v1 对象"
                .into(),
        );
    }
    let runs = payload["runs"]
        .as_array()
        .filter(|runs| {
            !runs.is_empty() && runs.len() <= MAX_MARKET_COMPARISON_RUNS_WITH_DIAGNOSTICS
        })
        .ok_or_else(|| {
            format!(
                "多市场 Bar 对照诊断 runs 必须包含 1..={MAX_MARKET_COMPARISON_RUNS_WITH_DIAGNOSTICS} 个运行"
            )
        })?;
    let mut by_run = std::collections::BTreeMap::new();
    for item in runs {
        let object = item
            .as_object()
            .filter(|object| {
                object.len() == 2
                    && object.contains_key("run_id")
                    && object.contains_key("diagnostics")
            })
            .ok_or("多市场 Bar 对照诊断每项只能包含 run_id 与 diagnostics")?;
        let run_id = object["run_id"]
            .as_str()
            .filter(|run_id| !run_id.trim().is_empty())
            .ok_or("多市场 Bar 对照诊断 run_id 不能为空")?;
        let diagnostics = object["diagnostics"].clone();
        if !diagnostics.is_array() {
            return Err(format!(
                "多市场 Bar 对照诊断 {run_id} 的 diagnostics 必须是数组"
            ));
        }
        if by_run.insert(run_id.to_owned(), diagnostics).is_some() {
            return Err(format!("多市场 Bar 对照诊断包含重复 run_id: {run_id}"));
        }
    }
    Ok(by_run)
}

pub(super) fn comparison_diagnostics_for_xlsx(report: &Value) -> Option<Value> {
    let records = report
        .get("runs")?
        .as_array()?
        .iter()
        .flat_map(|run| {
            let run_id = run.get("run_id").cloned().unwrap_or(Value::Null);
            run.get("market_strategy_diagnostics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |diagnostic| {
                    let mut record = diagnostic.clone();
                    record["run_id"] = run_id.clone();
                    record
                })
        })
        .collect::<Vec<_>>();
    (!records.is_empty()).then_some(Value::Array(records))
}

pub(super) fn render_comparison_diagnostics(report: &Value) -> String {
    let diagnostics = report
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|run| {
            let run_id = run["run_id"].as_str().unwrap_or("—");
            run.get("market_strategy_diagnostics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |item| (run_id, item))
        })
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return String::new();
    }
    let mut html = String::from(
        "<section id=\"comparison-diagnostics\"><h2>逐运行策略诊断对照</h2><p class=\"muted\">每份诊断均按对应 run_id、策略槽、标的、输入指纹、Bar 数量和结束时点重新绑定；报告不会重新执行策略。</p><div class=\"toolbar\"><input id=\"comparison-diagnostic-filter\" type=\"search\" placeholder=\"筛选运行、标的或策略\"><select id=\"comparison-lookahead-filter\"><option value=\"\">所有未来数据状态</option><option value=\"potential_lookahead_detected\">检测到变化</option><option value=\"no_change_detected\">扰动后此前决策未变化</option><option value=\"insufficient_history\">历史不足</option><option value=\"insufficient_work_budget\">工作量预算不足</option><option value=\"causal_event_contract\">因果事件接口；未做扰动</option></select></div><table id=\"comparison-diagnostics\"><thead><tr><th>运行</th><th>标的</th><th>策略</th><th>输入覆盖</th><th>未来数据</th><th>预热</th><th>PIT</th><th>证据</th></tr></thead><tbody>",
    );
    for (run_id, item) in diagnostics {
        let diagnostic = &item["report"];
        let instrument = item["instrument"].as_str().unwrap_or("—");
        let strategy_id = item["strategy_id"].as_str().unwrap_or("—");
        let text = |pointer: &str| {
            diagnostic
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or("—")
        };
        let lookahead = text("/lookahead/status");
        let startup = text("/startup/status");
        let pit = text("/data_visibility/status");
        let coverage = format!(
            "{} · {} · {}",
            diagnostic
                .get("input_kind")
                .and_then(Value::as_str)
                .unwrap_or("bar"),
            diagnostic
                .get("input_bars")
                .or_else(|| diagnostic.get("input_events"))
                .map(Value::to_string)
                .as_deref()
                .unwrap_or("—"),
            diagnostic
                .get("as_of")
                .map(Value::to_string)
                .as_deref()
                .unwrap_or("—")
        );
        let detail = serde_json::to_string_pretty(diagnostic).unwrap_or_else(|_| "{}".into());
        let searchable = format!("{run_id} {instrument} {strategy_id}");
        html.push_str(&format!(
            "<tr data-search=\"{}\" data-lookahead=\"{}\"><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><details><summary>查看</summary><pre>{}</pre></details></td></tr>",
            crate::report_html::escape_html(&searchable).to_lowercase(),
            crate::report_html::escape_html(lookahead),
            crate::report_html::escape_html(run_id),
            crate::report_html::escape_html(instrument),
            crate::report_html::escape_html(strategy_id),
            crate::report_html::escape_html(&coverage),
            crate::report_html::escape_html(lookahead),
            crate::report_html::escape_html(startup),
            crate::report_html::escape_html(pit),
            crate::report_html::escape_html(&detail),
        ));
    }
    html.push_str(
        "</tbody></table></section><script>(function(){const input=document.getElementById('comparison-diagnostic-filter'),state=document.getElementById('comparison-lookahead-filter'),rows=[...document.querySelectorAll('#comparison-diagnostics tbody tr')];function apply(){const q=input.value.trim().toLowerCase(),s=state.value;for(const row of rows)row.hidden=!(row.dataset.search.includes(q)&&(!s||row.dataset.lookahead===s));}input.addEventListener('input',apply);state.addEventListener('change',apply);})();</script>",
    );
    html
}

pub(crate) fn generate_builtin_bar_diagnostics(
    artifact_path: &Path,
    output_path: &Path,
    write_html: bool,
) -> Result<(), String> {
    let artifact = read_artifact(artifact_path)?
        .ok_or("strategy diagnose-multi-bar 只接受多市场 Bar 运行产物")?;
    let report = build_report(artifact_path, &artifact)?;
    let spec_text = serde_json::to_string(artifact.get("spec").ok_or("多市场 Bar 产物缺少 spec")?)
        .map_err(|error| format!("序列化多市场 Bar spec 失败: {error}"))?;
    let spec = qx_app::MultiMarketBacktestSpec::from_json(&spec_text)
        .map_err(|error| format!("多市场 Bar spec 无效: {error}"))?;
    let injected_ids = artifact
        .get("strategy_identities")
        .and_then(Value::as_array)
        .ok_or("多市场 Bar 产物缺少策略身份清单")?
        .iter()
        .filter_map(|identity| identity.get("strategy_id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    if !injected_ids.is_empty() {
        return Err(format!(
            "运行包含外部策略槽 {}；自动诊断只支持可从产物重建的 Rust 内置策略",
            injected_ids.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let slot_count = spec
        .markets
        .iter()
        .map(|market| market.strategies.len())
        .sum::<usize>();
    if slot_count == 0 || slot_count > MAX_MARKET_DIAGNOSTICS {
        return Err(format!(
            "多市场 Bar 策略槽数量必须在 1..={MAX_MARKET_DIAGNOSTICS}"
        ));
    }
    let market_data = artifact["market_data"]
        .as_array()
        .ok_or("多市场 Bar 产物缺少 market_data")?;
    let report_markets = report["markets"]
        .as_array()
        .ok_or("复核后的多市场 Bar 报告缺少 markets")?;
    let mut prepared = std::collections::BTreeMap::new();
    let mut total_strategy_bar_events = 0_usize;
    for configured in &spec.markets {
        let instrument = configured.instrument.as_str();
        let source = market_data
            .iter()
            .find(|row| row["instrument"].as_str() == Some(instrument))
            .and_then(|row| row["path"].as_str())
            .ok_or_else(|| format!("运行产物缺少 {instrument} 的 BarFrame 路径"))?;
        let frame_path = resolve_path(source, artifact_path);
        let frame = crate::read_bar_frame_for_backtest(&frame_path)?;
        let fingerprint = qx_data::fingerprint_bar_frame(&frame)?;
        let market_report = report_markets
            .iter()
            .find(|row| row["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("复核报告缺少 {instrument} 的数据身份"))?;
        if frame.instrument.to_string() != instrument
            || market_report["data_fingerprint"].as_str() != Some(fingerprint.as_str())
        {
            return Err(format!("{instrument} BarFrame 在诊断重建时身份发生变化"));
        }
        if frame.len() > MAX_MARKET_DIAGNOSTIC_BARS_PER_STRATEGY {
            return Err(format!(
                "{instrument} 有 {} 根 Bar，诊断单策略上限为 {}; 请使用更小的可复核区间",
                frame.len(),
                MAX_MARKET_DIAGNOSTIC_BARS_PER_STRATEGY
            ));
        }
        if frame.len() < 2 {
            return Err(format!(
                "{instrument} 至少需要两根 Bar 才能执行未来扰动诊断"
            ));
        }
        total_strategy_bar_events = total_strategy_bar_events
            .checked_add(
                frame
                    .len()
                    .checked_mul(configured.strategies.len())
                    .ok_or("多市场诊断 Bar 数乘策略数溢出")?,
            )
            .ok_or("多市场诊断总 Bar 数溢出")?;
        if total_strategy_bar_events > MAX_MARKET_DIAGNOSTIC_STRATEGY_BAR_EVENTS {
            return Err(format!(
                "多市场诊断合计策略 Bar 工作量超过 {}；请缩短区间或减少策略槽",
                MAX_MARKET_DIAGNOSTIC_STRATEGY_BAR_EVENTS
            ));
        }
        let events = (0..frame.len())
            .map(|index| qx_strategy::MarketEvent::Bar {
                instrument: frame.instrument.clone(),
                ts: frame.ts[index],
                open_raw: frame.open_raw[index],
                high_raw: frame.high_raw[index],
                low_raw: frame.low_raw[index],
                close_raw: frame.close_raw[index],
                volume_raw: frame.volume_raw[index],
                exact_ohlcv: frame.exact_ohlcv.as_ref().map(|values| values[index]),
            })
            .collect::<Vec<_>>();
        prepared.insert(instrument.to_string(), (fingerprint, events));
    }

    let protected_sources = report["input_artifacts"]
        .as_array()
        .ok_or("复核报告缺少受保护输入清单")?
        .iter()
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if protected_sources
        .iter()
        .any(|source| same_output_target(output_path, source))
    {
        return Err(
            "策略诊断输出不能覆盖多市场运行产物、BarFrame、质量清单、RunManifest 或报告输入".into(),
        );
    }
    let html_target = if write_html {
        let name = artifact_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("multi-market.json");
        Some(
            artifact_path
                .with_file_name(name.replace(".multi-market.json", ".multi-market.report.html")),
        )
    } else {
        None
    };
    if html_target
        .as_deref()
        .is_some_and(|target| same_output_target(output_path, target))
    {
        return Err("策略诊断 JSON 输出不能覆盖同次生成的 HTML 报告".into());
    }

    let future_work_per_slot = MAX_MARKET_DIAGNOSTIC_LOOKAHEAD_WORK / slot_count;
    let startup_work_per_slot = MAX_MARKET_DIAGNOSTIC_STARTUP_WORK / slot_count;
    let mut diagnostics = Vec::with_capacity(slot_count);
    for configured in &spec.markets {
        let instrument = qx_core::InstrumentId::parse(&configured.instrument)
            .ok_or_else(|| format!("InstrumentId 无效: {}", configured.instrument))?;
        let (fingerprint, events) = prepared
            .get(&configured.instrument)
            .ok_or_else(|| format!("诊断缺少 {} 的已验证 BarFrame", configured.instrument))?;
        let data_fingerprint = format!("barframe:{fingerprint}");
        for member in &configured.strategies {
            let strategy_spec = &member.strategy;
            if strategy_spec.target_notional_raw.is_some()
                || strategy_spec.target_available_margin_bps.is_some()
                || strategy_spec.target_volatility_bps.is_some()
                || strategy_spec.target_kelly_fraction_bps.is_some()
            {
                return Err(format!(
                    "{} / {} 使用动态 sizing；诊断不重建逐时点账户状态，已拒绝生成",
                    configured.instrument, strategy_spec.strategy_id
                ));
            }
            let strategy_version =
                crate::strategy_diagnostics::builtin_strategy_version(strategy_spec);
            let cash = std::collections::BTreeMap::from([(
                spec.settlement_currency.clone(),
                member.reserved_capital_raw,
            )]);
            let context = qx_strategy::StrategyContext {
                strategy_id: strategy_spec.strategy_id.clone(),
                strategy_version: strategy_version.clone(),
                account_id: spec.account_id.clone(),
                venue_id: instrument.venue.to_string(),
                data_fingerprint: data_fingerprint.clone(),
                as_of: events
                    .first()
                    .map(qx_strategy::MarketEvent::ts)
                    .ok_or("BarFrame 为空")?,
                positions_exact: std::collections::BTreeMap::new(),
                cash_exact: std::collections::BTreeMap::new(),
                available_margin_exact: None,
                positions: std::collections::BTreeMap::new(),
                available_margin_raw: Some(member.reserved_capital_raw),
                cash,
                risk_state: "backtest_diagnostic_fixed_context".into(),
            };
            let runtime_strategy = qx_runtime::StrategyRuntimeConfig {
                id: Some(strategy_spec.strategy_id.clone()),
                version: strategy_version,
                target_qty: strategy_spec.quantity_raw,
                builtin_strategy: Some(strategy_spec.kind.name().to_string()),
                builtin_fast_window: Some(strategy_spec.fast_window),
                builtin_slow_window: Some(strategy_spec.slow_window),
                builtin_period: Some(strategy_spec.period),
                builtin_threshold_bps: Some(strategy_spec.threshold_bps),
                ..qx_runtime::StrategyRuntimeConfig::default()
            };
            let strategy_config =
                qx_runtime::builtin_strategy_config_from_runtime(&runtime_strategy, &instrument)
                    .map_err(|error| {
                        format!("{} 内置策略重建失败: {error}", strategy_spec.strategy_id)
                    })?;
            let anchor = strategy_spec
                .fast_window
                .max(strategy_spec.slow_window)
                .max(strategy_spec.period)
                .clamp(20, 250);
            let startup_windows = [anchor, anchor * 2, anchor * 4];
            let effective_window = (anchor * 4).min(events.len()).max(1);
            let trajectory_replays = (startup_work_per_slot / (2 * effective_window)).clamp(3, 96);
            let make_strategy = || -> Result<Box<dyn qx_strategy::Strategy>, String> {
                Ok(Box::new(qx_strategy::BuiltinStrategy::new(
                    strategy_config.clone(),
                )?))
            };
            let startup = qx_strategy::diagnostics::diagnose_startup_windows(
                &context,
                events,
                &startup_windows,
                make_strategy,
            )?;
            let trajectory = qx_strategy::diagnostics::diagnose_startup_trajectory(
                &context,
                events,
                &startup_windows,
                trajectory_replays,
                make_strategy,
            )?;
            let lookahead_checks = (future_work_per_slot / events.len())
                .saturating_sub(1)
                .clamp(1, 16);
            let lookahead = qx_strategy::diagnostics::diagnose_future_perturbations(
                &context,
                events,
                lookahead_checks,
                make_strategy,
            )?;
            let windows = startup
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
                "run_id": spec.run_id,
                "strategy_id": strategy_spec.strategy_id,
                "strategy_version": strategy_config.strategy_version,
                "instrument": configured.instrument,
                "data_fingerprint": fingerprint,
                "input_kind": "bar",
                "input_bars": events.len(),
                "as_of": events.last().map(qx_strategy::MarketEvent::ts),
                "visible_bars_at_as_of": events.len(),
                "lookahead": lookahead,
                "startup": {
                    "status": startup.status,
                    "reference_window_events": startup.reference_window_bars,
                    "decision_timestamp": startup.decision_timestamp,
                    "windows": windows,
                    "trajectory": trajectory,
                    "interpretation": startup.interpretation,
                    "context_assumption": "Each strategy replay uses its declared reserved-capital cash snapshot and empty positions. Dynamic sizing and external strategy slots are rejected; shared-account event-time balances and positions are not reconstructed."
                },
                "data_visibility": {
                    "status": "visibility_unknown",
                    "reason": "the run artifact does not contain a verified provider PIT revision ledger with effective_at and available_at facts"
                }
            }));
        }
    }
    let diagnostics_value = Value::Array(diagnostics);
    // Rebind our own output before publishing it so an internal identity mismatch cannot leave
    // an unattached diagnostic file behind.
    bind_market_bar_diagnostics_value(&diagnostics_value, &artifact, &report)?;
    let encoded = serde_json::to_string_pretty(&diagnostics_value)
        .map_err(|error| format!("序列化多市场 Bar 策略诊断失败: {error}"))?;
    if encoded.len() as u64 > MAX_MARKET_DIAGNOSTICS_BYTES {
        return Err("自动生成的多市场 Bar 诊断超过 1 MiB 上限".into());
    }
    qx_app::artifact_store::sink::publish(
        output_path,
        &format!("{encoded}\n"),
        qx_app::artifact_store::sink::PublishMode::Overwrite,
    )
    .map_err(|error| format!("发布多市场 Bar 策略诊断失败: {error}"))?;
    let perturbation_targets = diagnostics_value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|diagnostic| {
            diagnostic
                .pointer("/lookahead/perturbation_targets_checked")
                .and_then(Value::as_u64)
        })
        .sum::<u64>();
    println!(
        "[Strategy Diagnostics] run_id={} strategies={} strategy_bar_events={} future_perturbation_targets_checked={} startup_replay_budget_per_strategy<=96 pit=visibility_unknown output={}",
        spec.run_id,
        slot_count,
        total_strategy_bar_events,
        perturbation_targets,
        output_path.display()
    );
    if write_html {
        let options = crate::report_command::ReportOptions {
            as_json: false,
            html: true,
            evidence: false,
            output: None,
            against: &[],
            benchmark_bars: None,
            benchmark_calendar: None,
            benchmark_csv: None,
            strategy_diagnostics: Some(output_path),
            xlsx: None,
        };
        crate::report_command::run_report_with_benchmark(artifact_path, options)?;
    }
    Ok(())
}

pub(super) fn read_market_bar_diagnostics(path: &Path) -> Result<Value, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("读取多市场策略诊断文件属性失败 {}: {error}", path.display()))?
        .len();
    if size > MAX_MARKET_DIAGNOSTICS_BYTES {
        return Err("多市场 Bar 策略诊断集合超过 1 MiB 上限".into());
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取多市场策略诊断文件失败 {}: {error}", path.display()))?;
    let diagnostics: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("多市场策略诊断 JSON 无效 {}: {error}", path.display()))?;
    Ok(diagnostics)
}

pub(super) fn bind_market_bar_diagnostics_value(
    diagnostics: &Value,
    artifact: &Value,
    report: &Value,
) -> Result<Value, String> {
    let items = diagnostics
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= MAX_MARKET_DIAGNOSTICS)
        .ok_or_else(|| {
            format!("多市场 Bar 策略诊断必须是 1..={MAX_MARKET_DIAGNOSTICS} 项的 JSON 数组")
        })?;
    let specs = artifact["spec"]["markets"]
        .as_array()
        .ok_or("多市场 Bar spec 缺少 markets")?;
    let markets = report["markets"]
        .as_array()
        .ok_or("多市场 Bar 报告缺少 markets")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut bound = Vec::with_capacity(items.len());
    for diagnostic in items {
        let strategy_id = diagnostic
            .get("strategy_id")
            .and_then(Value::as_str)
            .ok_or("多市场策略诊断缺少 strategy_id")?;
        let instrument = diagnostic
            .get("instrument")
            .and_then(Value::as_str)
            .ok_or("多市场策略诊断缺少 instrument")?;
        if !seen.insert((instrument, strategy_id)) {
            return Err("多市场策略诊断集合包含重复策略/标的身份".into());
        }
        let configured = specs
            .iter()
            .find(|market| market["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("策略诊断标的不在已复核 spec 中: {instrument}"))?;
        let declared = configured["strategies"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|member| member["strategy"]["strategy_id"].as_str() == Some(strategy_id));
        if !declared {
            return Err(format!("{instrument} 策略诊断不属于声明的策略槽"));
        }
        let market = markets
            .iter()
            .find(|market| market["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("策略诊断标的没有已复核的市场数据: {instrument}"))?;
        let fingerprint = market["data_fingerprint"]
            .as_str()
            .ok_or("已复核 Bar 市场缺少数据指纹")?;
        crate::report_command::validate_strategy_diagnostics_identity(
            diagnostic,
            strategy_id,
            instrument,
            fingerprint,
        )?;
        if diagnostic["input_bars"].as_u64() != market["row_count"].as_u64()
            || diagnostic.get("input_events").is_some()
            || !diagnostic
                .get("input_kind")
                .is_none_or(|kind| kind == "bar")
            || diagnostic["as_of"].as_u64() != market["source_end_ts"].as_u64()
        {
            return Err(format!(
                "{instrument} 策略诊断 Bar 数量、输入形态或结束时点与已复核 BarFrame 不一致"
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
