//! Offline research report for shared-account multi-market Bar runs.

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
const MAX_MULTI_MARKET_EVENTS: u64 = 5_000_000;
const MAX_MULTI_MARKET_EVENT_PAGE: usize = 300;

fn read_artifact(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(path).map_err(|e| format!("读取报告输入失败 {}: {e}", path.display()))?;
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let version = value.get("schema_version").and_then(Value::as_u64);
    let has_event_source = match version {
        Some(1) => value.get("event_log").and_then(Value::as_array).is_some(),
        Some(2) => {
            value
                .get("event_trace_path")
                .and_then(Value::as_str)
                .is_some()
                && value.get("event_count").and_then(Value::as_u64).is_some()
                && value.get("event_log").is_none()
        }
        _ => false,
    };
    if matches!(version, Some(1 | 2))
        && value.pointer("/outcome/shared_result_hash").is_some()
        && value.pointer("/outcome/pending_orders").is_none()
        && value.get("market_data").and_then(Value::as_array).is_some()
        && value.pointer("/spec/markets").is_some()
        && has_event_source
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
        return Err("多市场 --against 横向对照不生成单次运行 EvidenceBundle".into());
    }
    if strategy_diagnostics.is_some() && !html && !as_json && xlsx.is_none() {
        return Err("--strategy-diagnostics 必须与 --html、--json 或 --xlsx 一起使用".into());
    }
    if output.is_some() && !html {
        return Err("--output 只能与 --html 一起使用".into());
    }
    if output.is_some_and(|h| xlsx.is_some_and(|x| same_output_target(h, x))) {
        return Err("HTML 与 XLSX 报告必须使用不同输出路径".into());
    }
    let mut reports = vec![build_report(path, &artifact)?];
    let mut seen = std::collections::BTreeSet::from([path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())]);
    for candidate in against {
        let canonical = candidate
            .canonicalize()
            .map_err(|e| format!("解析对照路径失败 {}: {e}", candidate.display()))?;
        if !seen.insert(canonical) {
            return Err(format!("多市场对照不能重复输入: {}", candidate.display()));
        }
        let Some(value) = read_artifact(candidate)? else {
            return Err(format!(
                "--against 只接受多市场 Bar 运行产物: {}",
                candidate.display()
            ));
        };
        reports.push(build_report(candidate, &value)?);
    }
    let comparison = reports.len() > 1;
    if comparison {
        let fingerprint = reports[0]["data_fingerprint"].clone();
        let instruments = market_ids(&reports[0]);
        if reports
            .iter()
            .any(|r| r["data_fingerprint"] != fingerprint || market_ids(r) != instruments)
        {
            return Err("多市场 Bar 对照要求市场顺序和数据指纹完全相同".into());
        }
        reports.sort_by(|a, b| {
            b["return_bps"]
                .as_i64()
                .cmp(&a["return_bps"].as_i64())
                .then_with(|| {
                    a["max_drawdown_bps"]
                        .as_u64()
                        .cmp(&b["max_drawdown_bps"].as_u64())
                })
                .then_with(|| a["run_id"].as_str().cmp(&b["run_id"].as_str()))
        });
        if let Some(diagnostics_path) = strategy_diagnostics {
            bind_market_bar_comparison_diagnostics(diagnostics_path, &mut reports)?;
        }
    }
    let mut protected_sources = reports
        .iter()
        .flat_map(|report| {
            let summary = report["summary_path"]
                .as_str()
                .map(PathBuf::from)
                .into_iter();
            let inputs = report["input_artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(PathBuf::from);
            summary.chain(inputs)
        })
        .collect::<Vec<_>>();
    protected_sources.extend(strategy_diagnostics.map(Path::to_path_buf));
    let mut report = if comparison {
        json!({"comparison_kind":"same_multi_market_bar_input","data_fingerprint":reports[0]["data_fingerprint"],
            "markets":market_ids(&reports[0]),"market_sources":reports[0]["markets"],"runs":reports})
    } else {
        reports.remove(0)
    };
    let mut diagnostics_payload = None;
    if !comparison {
        if let Some(diagnostics_path) = strategy_diagnostics {
            let payload = read_market_bar_diagnostics(diagnostics_path)?;
            report["market_strategy_diagnostics"] =
                bind_market_bar_diagnostics_value(&payload, &artifact, &report)?;
            diagnostics_payload = Some(payload);
        }
    }
    let mut generated = serde_json::Map::new();
    if evidence {
        let diagnostics = strategy_diagnostics.zip(diagnostics_payload.as_ref());
        let bundle_path =
            crate::run_evidence_multi_market::write_multi_market_run_evidence(path, diagnostics)?;
        generated.insert("evidence".into(), json!(bundle_path));
    }
    if html {
        let target = output.map(Path::to_path_buf).unwrap_or_else(|| {
            let name = path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("multi-market.json");
            path.with_file_name(name.replace(
                ".multi-market.json",
                if comparison {
                    ".multi-market.comparison.html"
                } else {
                    ".multi-market.report.html"
                },
            ))
        });
        if !target
            .extension()
            .and_then(|v| v.to_str())
            .is_some_and(|v| v.eq_ignore_ascii_case("html") || v.eq_ignore_ascii_case("htm"))
        {
            return Err(format!(
                "HTML 输出路径必须以 .html 或 .htm 结尾: {}",
                target.display()
            ));
        }
        if protected_sources
            .iter()
            .any(|source| same_output_target(&target, source))
        {
            return Err("HTML 输出不能覆盖任何输入运行产物".into());
        }
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败: {e}"))?;
        }
        let rendered = render_html(&report, comparison)?;
        let rendered = if comparison {
            rendered.replace(
                "</main>",
                &format!("{}</main>", render_comparison_diagnostics(&report)),
            )
        } else {
            rendered
        };
        qx_app::artifact_store::sink::publish(
            &target,
            &rendered,
            qx_app::artifact_store::sink::PublishMode::Overwrite,
        )
        .map_err(|e| format!("发布多市场 Bar HTML 失败: {e}"))?;
        generated.insert("html".into(), json!(target));
    }
    if let Some(target) = xlsx {
        if protected_sources
            .iter()
            .any(|source| same_output_target(target, source))
        {
            return Err("XLSX 输出不能覆盖任何输入运行产物".into());
        }
        let (overview, markets) = if comparison {
            (
                vec![
                    vec!["Comparison".into(), "same_multi_market_bar_input".into()],
                    vec![
                        "Data fingerprint".into(),
                        report["data_fingerprint"].as_str().unwrap_or("").into(),
                    ],
                ],
                report["runs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        vec![
                            r["run_id"].to_string(),
                            r["return_bps"].to_string(),
                            r["max_drawdown_bps"].to_string(),
                            r["successful_fills"].to_string(),
                            r["result_hash"].to_string(),
                        ]
                    })
                    .collect(),
            )
        } else {
            (
                vec![
                    vec!["Field".into(), "Value".into()],
                    vec!["run_id".into(), report["run_id"].to_string()],
                    vec!["return_bps".into(), report["return_bps"].to_string()],
                    vec![
                        "max_drawdown_bps".into(),
                        report["max_drawdown_bps"].to_string(),
                    ],
                ],
                report["markets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| {
                        vec![
                            m["instrument"].to_string(),
                            m["row_count"].to_string(),
                            m["fills"].to_string(),
                            m["content_hash"].to_string(),
                            m["quality_status"].to_string(),
                        ]
                    })
                    .collect(),
            )
        };
        let mut m: Vec<Vec<String>> = markets;
        if comparison {
            m.insert(
                0,
                vec![
                    "run_id".into(),
                    "return_bps".into(),
                    "max_drawdown_bps".into(),
                    "fills".into(),
                    "result_hash".into(),
                ],
            );
        } else {
            m.insert(
                0,
                vec![
                    "instrument".into(),
                    "bar_rows".into(),
                    "fills".into(),
                    "content_hash".into(),
                    "quality".into(),
                ],
            );
        }
        let diagnostics_for_xlsx = if comparison {
            comparison_diagnostics_for_xlsx(&report)
        } else {
            report.get("market_strategy_diagnostics").cloned()
        };
        crate::report_xlsx::write_multi_market_depth_xlsx(
            target,
            overview,
            m,
            diagnostics_for_xlsx.as_ref(),
        )?;
        generated.insert("xlsx".into(), json!(target));
    }
    if as_json {
        let mut result = json!({"schema_version":1,"report_kind":if comparison {"multi_market_bar_comparison"} else {"multi_market_bar"},"report":report});
        if !generated.is_empty() {
            result["generated_artifacts"] = Value::Object(generated.clone());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
        );
    } else if comparison {
        println!(
            "[Report] comparison_fingerprint={} compared_runs={}",
            report["data_fingerprint"],
            report["runs"].as_array().map_or(0, Vec::len)
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
    if !as_json {
        for (kind, path) in generated {
            println!("[Report] {kind}={}", path.as_str().unwrap_or(""));
        }
    }
    Ok(true)
}

fn bind_market_bar_comparison_diagnostics(
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

fn parse_market_bar_comparison_diagnostics(
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

fn comparison_diagnostics_for_xlsx(report: &Value) -> Option<Value> {
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

fn render_comparison_diagnostics(report: &Value) -> String {
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

fn market_ids(report: &Value) -> Vec<Value> {
    report["markets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| m["instrument"].clone())
        .collect()
}

fn build_report(path: &Path, artifact: &Value) -> Result<Value, String> {
    let outcome = &artifact["outcome"];
    if artifact["replay_verified"] != true {
        return Err("多市场 Bar 产物缺少 ReplayVerifier 成功标记".into());
    }
    let backtest_spec: qx_app::MultiMarketBacktestSpec =
        serde_json::from_value(artifact["spec"].clone())
            .map_err(|error| format!("多市场 Bar spec 无效: {error}"))?;
    backtest_spec
        .validate()
        .map_err(|error| format!("多市场 Bar spec 无效: {error}"))?;
    let quality_records = artifact
        .get("dataset_quality")
        .cloned()
        .unwrap_or_else(|| json!({}));
    qx_app::validate_multi_market_quality_manifest_set_v1(&backtest_spec, &quality_records)
        .map_err(|error| error.to_string())?;
    let qualities = quality_records
        .as_object()
        .ok_or("多市场数据质量记录必须是对象")?;
    let configured_markets = artifact["spec"]["markets"]
        .as_array()
        .ok_or("回测 spec 缺少 markets")?;
    let market_data = artifact["market_data"]
        .as_array()
        .ok_or("产物缺少 market_data")?;
    if market_data.len() != backtest_spec.markets.len() {
        return Err("多市场输入数量与 spec 不一致".into());
    }
    let mut markets = Vec::new();
    let mut typed_market_specs = std::collections::BTreeMap::new();
    let mut final_market_marks = std::collections::BTreeMap::new();
    let mut verified_market_inputs = Vec::with_capacity(backtest_spec.markets.len());
    let mut seen_instruments = std::collections::BTreeSet::new();
    let mut input_artifacts = vec![path.to_path_buf()];
    for market in market_data {
        let instrument = market["instrument"]
            .as_str()
            .ok_or("market_data 缺少 instrument")?;
        if !seen_instruments.insert(instrument.to_owned()) {
            return Err(format!("多市场输入存在重复标的: {instrument}"));
        }
        let configured = backtest_spec
            .markets
            .iter()
            .find(|candidate| candidate.instrument == instrument)
            .ok_or_else(|| format!("回测 spec 缺少市场配置: {instrument}"))?;
        let source = market["path"]
            .as_str()
            .ok_or("market_data 缺少 BarFrame 路径")?;
        let frame_path = resolve_path(source, path);
        input_artifacts.push(frame_path.clone());
        let frame_payload = crate::read_example_json(&frame_path, "BarFrame")?;
        let frame = qx_datastruct::BarFrame::from_json(&frame_payload)
            .map_err(|error| format!("{instrument} BarFrame 无效: {error:?}"))?;
        let quality_record = qualities.get(instrument);
        let quality_path = quality_record
            .map(|record| {
                let source = record
                    .get("manifest_path")
                    .and_then(Value::as_str)
                    .ok_or("质量清单缺少路径")?;
                Ok::<_, String>(resolve_path(source, path))
            })
            .transpose()?;
        let quality_payload = quality_path
            .as_deref()
            .map(|quality_path| {
                input_artifacts.push(quality_path.to_path_buf());
                crate::read_example_json(quality_path, "数据质量清单")
            })
            .transpose()?;
        let verified = qx_app::verify_multi_market_bar_input_v1(
            configured,
            market,
            &frame_payload,
            &frame,
            quality_record,
            quality_payload.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let fingerprint = verified.fingerprint.clone();
        verified_market_inputs.push(qx_app::MultiMarketInputIdentityV1 {
            instrument: verified.instrument.clone(),
            bar_fingerprint: fingerprint.clone(),
            quality_fingerprint: verified.quality_fingerprint.clone(),
        });
        let market_spec = configured
            .market_spec
            .as_ref()
            .map(|snapshot| qx_app::market_spec_from_value(&frame.instrument, snapshot))
            .transpose()
            .map_err(|error| format!("{instrument} market spec 无效: {error}"))?
            .unwrap_or_else(|| {
                qx_core::TradingInstrumentSpec::spot_default(
                    &frame.instrument,
                    &backtest_spec.settlement_currency,
                )
            });
        if typed_market_specs
            .insert(frame.instrument.clone(), market_spec)
            .is_some()
        {
            return Err(format!("多市场输入存在重复标的: {instrument}"));
        }
        final_market_marks.insert(
            frame.instrument.clone(),
            qx_core::Price::from_raw(verified.final_close_raw),
        );
        let quality = if let (Some(record), Some(quality_path)) = (quality_record, quality_path) {
            json!({
                "status": verified.quality_status,
                "manifest_path": quality_path,
                "manifest_fingerprint": verified.quality_fingerprint,
                "quality_report": record["quality_report"],
            })
        } else {
            json!({"status":"not_provided"})
        };
        let spec = configured_markets
            .iter()
            .find(|item| item["instrument"].as_str() == Some(instrument))
            .ok_or_else(|| format!("回测 spec 缺少市场配置: {instrument}"))?;
        markets.push(json!({"instrument":instrument,"path":source,"content_hash":fingerprint,"data_fingerprint":fingerprint,"row_count":verified.row_count,"source_end_ts":verified.source_end_ts,"fills":artifact.pointer(&format!("/per_market_fills/{instrument}")).cloned().unwrap_or(json!(0)),"quality_status":quality["status"],"dataset_quality":quality,"risk_market_spec":spec["market_spec"],"fee_schedule":spec["fee_schedule"],"strategies":spec["strategies"]}));
    }
    if seen_instruments.len() != backtest_spec.markets.len() {
        return Err("spec 中有未进入 market_data 的标的".into());
    }
    let input_identity =
        qx_app::multi_market_input_set_identity_v1(&backtest_spec, &verified_market_inputs)
            .map_err(|error| error.to_string())?;
    let composed = input_identity.data_fingerprint;
    let mut components = input_identity.input_components;
    let manifest_path = artifact["run_manifest_path"]
        .as_str()
        .ok_or("产物缺少 RunManifest")?;
    let manifest_path = resolve_path(manifest_path, path);
    input_artifacts.push(manifest_path.clone());
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("读取 RunManifest 失败: {e}"))?,
    )?;
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact)
        .map_err(|error| format!("多市场 CanonicalRunSpec 复核失败: {error}"))?;
    let instrument_equity_path = artifact
        .get("instrument_equity_contributions_path")
        .and_then(Value::as_str)
        .map(|source| resolve_path(source, path));
    let mut instrument_equity_contributions = Value::Null;
    if let Some(instrument_equity_path) = instrument_equity_path {
        input_artifacts.push(instrument_equity_path.clone());
        let bytes = std::fs::read(&instrument_equity_path)
            .map_err(|error| format!("读取按标的权益贡献失败: {error}"))?;
        instrument_equity_contributions = serde_json::from_slice(&bytes)
            .map_err(|error| format!("按标的权益贡献 JSON 无效: {error}"))?;
        if !instrument_equity_contributions.is_object() {
            return Err("按标的权益贡献必须是按 instrument 索引的 JSON 对象".into());
        }
        components.insert(
            "derived:instrument-equity-contributions".into(),
            qx_data::sha256_hex_bytes(&bytes),
        );
    } else if manifest
        .input_components
        .contains_key("derived:instrument-equity-contributions")
    {
        return Err("RunManifest 声明了按标的权益贡献，但运行产物缺少其文件路径".into());
    }
    let exact_equity_path = artifact
        .get("equity_exact_path")
        .and_then(Value::as_str)
        .map(|source| resolve_path(source, path));
    let mut exact_equity = Value::Null;
    if let Some(exact_equity_path) = exact_equity_path.as_ref() {
        input_artifacts.push(exact_equity_path.clone());
        let bytes = std::fs::read(exact_equity_path)
            .map_err(|error| format!("读取精确权益曲线失败: {error}"))?;
        exact_equity = serde_json::from_slice(&bytes)
            .map_err(|error| format!("精确权益曲线 JSON 无效: {error}"))?;
        if !exact_equity.is_array() {
            return Err("精确权益曲线必须是数组".into());
        }
        components.insert(
            "derived:equity-exact".into(),
            qx_data::sha256_hex_bytes(&bytes),
        );
    } else if manifest
        .input_components
        .contains_key("derived:equity-exact")
    {
        return Err("RunManifest 声明了精确权益曲线，但运行产物缺少其文件路径".into());
    }
    if composed != outcome["data_fingerprint"].as_str().unwrap_or("")
        || manifest.data_fingerprint != composed
        || manifest.input_components != components
        || manifest.result_hash != outcome["shared_result_hash"].as_str().unwrap_or("")
    {
        return Err("多市场 Bar 输入与 RunManifest 身份不一致".into());
    }
    let strategy_markets = backtest_spec
        .markets
        .iter()
        .flat_map(|market| {
            market.strategies.iter().map(move |member| {
                (
                    member.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
            })
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let (events, event_count, event_offset, streamed_fill_identity, ledger) =
        if artifact.get("schema_version").and_then(Value::as_u64) == Some(2) {
            let event_count = artifact["event_count"]
                .as_u64()
                .filter(|count| *count <= MAX_MULTI_MARKET_EVENTS)
                .ok_or("多市场 Bar event_count 无效或超过 500 万行")?;
            let event_trace = resolve_path(
                artifact["event_trace_path"]
                    .as_str()
                    .ok_or("多市场 Bar v2 产物缺少 event_trace_path")?,
                path,
            );
            let page_offset = event_count.saturating_sub(MAX_MULTI_MARKET_EVENT_PAGE as u64);
            let event_report = qx_app::read_multi_market_event_report_v1_with_reader(
                &event_trace.to_string_lossy(),
                event_count,
                &manifest.output_event_hash,
                &artifact["ledger_entries"],
                &strategy_markets,
                Some((page_offset, MAX_MULTI_MARKET_EVENT_PAGE)),
                &qx_app::artifact_store::FileArtifactReader,
            )
            .map_err(|error| format!("多市场 Bar JSONL 事件流核验失败: {error}"))?;
            let ledger = event_report.ledger;
            (
                event_report.rows,
                event_report.total,
                event_report.offset,
                Some((event_report.fill_count, event_report.fill_sequence_sha256)),
                ledger,
            )
        } else {
            let event_rows = artifact["event_log"]
                .as_array()
                .filter(|events| events.len() as u64 <= MAX_MULTI_MARKET_EVENTS)
                .ok_or("多市场 Bar v1 事件流缺失或超过 500 万行")?;
            let event_report = qx_app::read_multi_market_legacy_event_report_v1(
                event_rows,
                &manifest.output_event_hash,
                &artifact["ledger_entries"],
                &strategy_markets,
                None,
            )
            .map_err(|error| format!("多市场 Bar v1 事件流核验失败: {error}"))?;
            // Keep the legacy CLI report's full event projection. Replay, order attribution and
            // ledger facts are still owned by qx-app; the v2 path remains page-bounded.
            let ledger = event_report.ledger;
            (event_rows.clone(), event_report.total, 0, None, ledger)
        };
    let equity_path = resolve_path(
        artifact["equity_path"]
            .as_str()
            .ok_or("产物缺少权益曲线路径")?,
        path,
    );
    let fills_path = resolve_path(
        artifact["fills_path"]
            .as_str()
            .ok_or("产物缺少成交明细路径")?,
        path,
    );
    input_artifacts.push(equity_path.clone());
    input_artifacts.push(fills_path.clone());
    let equity: Value = serde_json::from_slice(
        &std::fs::read(equity_path).map_err(|e| format!("读取权益曲线失败: {e}"))?,
    )
    .map_err(|e| format!("权益曲线 JSON 无效: {e}"))?;
    let fills: Value = serde_json::from_slice(
        &std::fs::read(fills_path).map_err(|e| format!("读取成交明细失败: {e}"))?,
    )
    .map_err(|e| format!("成交明细 JSON 无效: {e}"))?;
    if !instrument_equity_contributions.is_null() {
        let equity_rows = equity.as_array().ok_or("共享权益曲线为空或格式无效")?;
        let exact_rows = exact_equity.as_array().map(Vec::as_slice);
        let verified = qx_app::verify_multi_market_pnl_contributions_with_exact_v1(
            &instrument_equity_contributions,
            equity_rows,
            exact_rows,
            &backtest_spec,
            &ledger,
            &typed_market_specs,
            &final_market_marks,
        )
        .map_err(|error| format!("按标的权益贡献核验失败: {error}"))?;
        instrument_equity_contributions = Value::Object(
            verified
                .curves
                .into_iter()
                .map(|(instrument, samples)| {
                    let rows = samples
                        .into_iter()
                        .map(|sample| {
                            json!({
                                "equity_index": sample.equity_index,
                                "ts": sample.ts,
                                "pnl_contribution_raw": sample.pnl_contribution_raw.to_string(),
                                "pnl_contribution_exact": sample.pnl_contribution_exact.map(|value| value.to_string())
                            })
                        })
                        .collect();
                    (instrument, Value::Array(rows))
                })
                .collect(),
        );
    }
    if let Some((fill_count, fill_sequence_sha256)) = streamed_fill_identity.as_ref() {
        let final_equity_raw = serde_json::from_value::<i128>(outcome["final_equity_raw"].clone())
            .map_err(|_| "多市场 outcome final_equity_raw 非法")?;
        qx_app::verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
            &equity,
            (!exact_equity.is_null()).then_some(&exact_equity),
            &fills,
            *fill_count,
            fill_sequence_sha256,
            &backtest_spec,
            outcome["equity_points"]
                .as_u64()
                .ok_or("多市场 outcome 缺少 equity_points")?,
            final_equity_raw,
            outcome["return_bps"]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or("多市场 outcome return_bps 非法")?,
            outcome["max_drawdown_bps"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("多市场 outcome max_drawdown_bps 非法")?,
        )
        .map_err(|error| format!("多市场 Bar 权益/成交 sidecar 核验失败: {error}"))?;
    } else {
        let final_equity_raw = serde_json::from_value::<i128>(outcome["final_equity_raw"].clone())
            .map_err(|_| "多市场 outcome final_equity_raw 非法")?;
        qx_app::verify_multi_market_bar_sidecars_with_exact_v1(
            &equity,
            (!exact_equity.is_null()).then_some(&exact_equity),
            &fills,
            &events,
            &backtest_spec,
            outcome["equity_points"]
                .as_u64()
                .ok_or("多市场 outcome 缺少 equity_points")?,
            outcome["successful_fills"]
                .as_u64()
                .ok_or("多市场 outcome 缺少 successful_fills")?,
            final_equity_raw,
            outcome["return_bps"]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or("多市场 outcome return_bps 非法")?,
            outcome["max_drawdown_bps"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("多市场 outcome max_drawdown_bps 非法")?,
        )
        .map_err(|error| format!("多市场 Bar 权益/成交 sidecar 核验失败: {error}"))?;
    }
    let event_fills = if streamed_fill_identity.is_none() {
        Some(
            events
                .iter()
                .filter_map(|event| event.pointer("/kind/Filled/fill").cloned())
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    if equity != artifact["equity"]
        || !fills.is_array()
        || fills.as_array().map_or(0, Vec::len) as u64
            != outcome["successful_fills"].as_u64().unwrap_or(u64::MAX)
        || event_fills.is_some_and(|event_fills| fills != Value::Array(event_fills))
    {
        return Err("多市场 Bar 权益/成交产物与摘要不一致".into());
    }
    let events_truncated = event_offset.saturating_add(events.len() as u64) < event_count;
    Ok(
        json!({"run_id":outcome["run_id"],"account_id":outcome["account_id"],"settlement_currency":artifact["spec"]["settlement_currency"],"initial_cash_raw":artifact["spec"]["initial_cash_raw"].to_string(),"result_hash":outcome["shared_result_hash"],"data_fingerprint":composed,"markets_total":outcome["markets_total"],"strategies_total":outcome["strategies_total"],"successful_fills":outcome["successful_fills"],"equity_points":outcome["equity_points"],"final_equity_raw":outcome["final_equity_raw"],"return_bps":outcome["return_bps"],"max_drawdown_bps":outcome["max_drawdown_bps"],"summary_path":path,"input_artifacts":input_artifacts,"strategy_identities":artifact["strategy_identities"],"equity":equity,"equity_exact":exact_equity,"instrument_equity_contributions":instrument_equity_contributions,"fills":fills,"events":events,"event_count":event_count,"event_offset":event_offset,"events_truncated":events_truncated,"event_log_digest":manifest.output_event_hash,"markets":markets}),
    )
}

/// Rebuild bounded causal and startup diagnostics for every Rust built-in strategy in a
/// verified shared-account multi-market Bar run, then optionally attach them to the offline
/// interactive report.
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

fn read_market_bar_diagnostics(path: &Path) -> Result<Value, String> {
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

fn bind_market_bar_diagnostics_value(
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

fn resolve_path(reference: &str, artifact_path: &Path) -> PathBuf {
    let referenced = Path::new(reference);
    if referenced.is_absolute() || referenced.exists() {
        return referenced.to_path_buf();
    }
    artifact_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(referenced)
}

fn render_html(report: &Value, comparison: bool) -> Result<String, String> {
    let mut html_report = report.clone();
    if comparison {
        for run in html_report["runs"].as_array_mut().into_iter().flatten() {
            bound_html_series(run);
        }
    } else {
        bound_html_series(&mut html_report);
    }
    let data = serde_json::to_string(&html_report)
        .map_err(|e| e.to_string())?
        .replace('<', "\\u003c");
    let template = r#"<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>牵星多市场研究报告</title><style>body{font:15px system-ui;margin:2rem;background:#f4f6fa;color:#172033}main{max-width:1320px;margin:auto}section,header{background:#fff;border-radius:12px;padding:1rem 1.4rem;margin:1rem 0;box-shadow:0 2px 12px #10204012}canvas{width:100%;height:360px}table{border-collapse:collapse;width:100%}td,th{padding:.55rem;border-bottom:1px solid #ddd;text-align:left;vertical-align:top}small,.muted{color:#536178}.cards,.legend{display:flex;gap:.65rem;flex-wrap:wrap}.card,.legend label{background:#eef3fb;padding:.65rem .9rem;border-radius:9px}.toolbar{display:flex;gap:1rem;flex-wrap:wrap;margin:.7rem 0}.toolbar input{min-width:15rem;padding:.45rem}pre{white-space:pre-wrap;max-width:38rem;max-height:20rem;overflow:auto}</style><main><header><h1>多市场共享账户回测研究报告</h1><div id="cards" class="cards"></div><p class="muted">输入、质量清单、权益与成交产物已完成身份复核。权益曲线展示共享账户总账；对照时按各轮起点归一化为基点收益。</p></header><section><h2>权益曲线</h2><div id="legend" class="legend"></div><div class="toolbar"><label>显示范围 <input id="range" type="range" min="1" max="100" value="100"></label><span id="hover" class="muted">移动鼠标到曲线上查看时间与收益</span></div><canvas id="equity"></canvas></section><section><h2>市场来源、策略与成交归因</h2><div class="toolbar"><input id="marketSearch" placeholder="筛选标的、数据指纹或来源路径"></div><table id="markets"></table></section><section id="compare"><h2>同输入运行排名</h2><div class="toolbar"><input id="runSearch" placeholder="筛选运行 ID"></div><table id="runs"></table></section><section><h2>成交明细</h2><div class="toolbar"><input id="fillSearch" placeholder="筛选运行、策略、订单或时间"><span id="fillCount" class="muted"></span></div><table id="fills"></table><p class="muted">完整成交数组保留在本地 JSON 报告中；HTML 明细最多显示 2,000 笔以控制浏览器资源。</p></section><section><h2>已验证事件时间线</h2><div class="toolbar"><input id="eventSearch" placeholder="筛选运行、事件、订单或时间"><span id="eventCount" class="muted"></span></div><table id="events"></table><p class="muted">完整事件流和重放摘要保留在本地 JSON 报告中；HTML 每轮最多展示最近 2,000 个事件。</p></section></main><script>const r=__DATA__,cmp=__CMP__,esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));const runs=cmp?r.runs:[r],markets=cmp?r.market_sources:r.markets,colors=['#2463eb','#e4572e','#1b9e77','#8e44ad','#d4a017','#566573'];let visible=runs.map(()=>true),series=[];const eventLinks=new Map();function renderTables(){const mh='<tr><th>标的</th><th>Bar数</th><th>成交数</th><th>数据指纹</th><th>质量</th><th>来源与配置</th></tr>';document.querySelector('#markets').innerHTML=mh+markets.map(m=>`<tr data-search="${esc([m.instrument,m.path,m.content_hash].join(' ')).toLowerCase()}"><td>${esc(m.instrument)}</td><td>${esc(m.row_count)}</td><td>${esc(m.fills)}</td><td><code>${esc(m.content_hash)}</code></td><td>${esc(m.quality_status)}</td><td><code>${esc(m.path)}</code>${m.strategies?`<details><summary>策略、规则与费用</summary><pre>${esc(JSON.stringify({strategies:m.strategies,risk_market_spec:m.risk_market_spec,fee_schedule:m.fee_schedule},null,2))}</pre></details>`:''}</td></tr>`).join('');document.querySelector('#marketSearch').oninput=e=>filterRows('#markets tbody tr',e.target.value);if(cmp){document.querySelector('#compare').style.display='block';document.querySelector('#runs').innerHTML='<tr><th>排名</th><th>运行</th><th>收益 bp</th><th>最大回撤 bp</th><th>成交</th><th>结果哈希</th></tr>'+r.runs.map((x,i)=>`<tr data-search="${esc(x.run_id).toLowerCase()}"><td>${i+1}</td><td>${esc(x.run_id)}</td><td>${esc(x.return_bps)}</td><td>${esc(x.max_drawdown_bps)}</td><td>${esc(x.successful_fills)}</td><td><code>${esc(x.result_hash)}</code></td></tr>`).join('');document.querySelector('#runSearch').oninput=e=>filterRows('#runs tbody tr',e.target.value);}else document.querySelector('#compare').style.display='none';let fills=[];runs.forEach((x,ri)=>(x.fills||[]).forEach(f=>fills.push({run_id:x.run_id,run_index:ri,...f})));const shown=fills.slice(-2000).reverse();document.querySelector('#fillCount').textContent=`显示 ${shown.length} / ${fills.length} 笔（最新优先）`;document.querySelector('#fills').innerHTML='<tr><th>运行</th><th>策略</th><th>订单</th><th>时间戳</th><th>数量 raw</th><th>价格 raw</th><th>费用 raw</th></tr>'+shown.map(f=>`<tr data-search="${esc([f.run_id,f.strategy_id,f.order_id,f.ts].join(' ')).toLowerCase()}"><td>${esc(f.run_id)}</td><td>${esc(f.strategy_id??'—')}</td><td>${eventLinks.has(`${f.run_index}:${f.order_id}`)?`<a href='#${eventLinks.get(`${f.run_index}:${f.order_id}`)}'>${esc(f.order_id)}</a>`:esc(f.order_id)}</td><td>${esc(f.ts)}</td><td>${esc(f.qty?.raw??JSON.stringify(f.qty))}</td><td>${esc(f.price?.raw??JSON.stringify(f.price))}</td><td>${esc(f.fee?.raw??JSON.stringify(f.fee))}</td></tr>`).join('');document.querySelector('#fillSearch').oninput=e=>filterRows('#fills tbody tr',e.target.value);document.querySelector('#cards').innerHTML=cmp?[`同数据对照 ${r.runs.length} 轮`,`标的 ${markets.length} 个`,`Fingerprint ${r.data_fingerprint}`].map(x=>`<div class="card">${esc(x)}</div>`).join(''):[['账户',r.account_id],['策略槽',r.strategies_total],['总成交',r.successful_fills],['收益 bp',r.return_bps],['最大回撤 bp',r.max_drawdown_bps],['结果哈希',r.result_hash]].map(x=>`<div class="card">${esc(x[0])}：${esc(x[1])}</div>`).join('');}function renderEvents(){let all=[];runs.forEach((run,ri)=>(run.events||[]).forEach(ev=>{const kind=typeof ev.kind==='string'?ev.kind:Object.keys(ev.kind||{})[0]||'unknown',payload=typeof ev.kind==='object'?ev.kind[kind]:null,order=payload?.fill?.order_id??payload?.client_order_id??payload?.order?.client_order_id??'',id=`ev-${ri}-${ev.seq}`;all.push({run_id:run.run_id,run_index:ri,kind,order,ts:ev.ts,seq:ev.seq,id})}));const shown=all.slice(-2000).reverse();shown.forEach(ev=>{if(ev.kind==='Filled'&&ev.order!=='')eventLinks.set(`${ev.run_index}:${ev.order}`,ev.id)});document.querySelector('#eventCount').textContent=`显示 ${shown.length} / ${runs.reduce((sum,run)=>sum+Number(run.event_count||0),0)} 个事件（最新优先）`;document.querySelector('#events').innerHTML='<tr><th>运行</th><th>序号</th><th>时间戳</th><th>类型</th><th>订单</th></tr>'+shown.map(ev=>`<tr id="${ev.id}" data-search="${esc([ev.run_id,ev.seq,ev.ts,ev.kind,ev.order].join(' ')).toLowerCase()}"><td>${esc(ev.run_id)}</td><td>${esc(ev.seq)}</td><td>${esc(ev.ts)}</td><td>${esc(ev.kind)}</td><td>${esc(ev.order)}</td></tr>`).join('');document.querySelector('#eventSearch').oninput=e=>filterRows('#events tbody tr',e.target.value)}function filterRows(selector,query){const q=query.trim().toLowerCase();document.querySelectorAll(selector).forEach(row=>row.style.display=row.dataset.search.includes(q)?'':'none')}function draw(){const c=document.querySelector('#equity'),ctx=c.getContext('2d'),dpr=devicePixelRatio||1,w=c.clientWidth,h=360;c.width=w*dpr;c.height=h*dpr;ctx.scale(dpr,dpr);const limit=Math.max(2,Math.ceil((runs[0]?.equity?.length||0)*Number(document.querySelector('#range').value)/100));series=runs.map((run,ri)=>{const a=run.equity||[],end=Math.min(a.length,limit),stride=Math.max(1,Math.ceil(end/2000)),sample=[];for(let i=0;i<end;i+=stride)sample.push(a[i]);if(end&&sample.at(-1)!==a[end-1])sample.push(a[end-1]);const base=Number(a[0]?.[1]||1);return {name:run.run_id,points:sample.map(x=>({ts:x[0],value:(Number(x[1])/base-1)*10000})),color:colors[ri%colors.length]}});const active=series.filter((_,i)=>visible[i]);if(!active.length)return;const vals=active.flatMap(s=>s.points.map(p=>p.value)),lo=Math.min(0,...vals),hi=Math.max(0,...vals),pad=38,plotW=w-pad*2,plotH=h-pad*2;ctx.strokeStyle='#bcc5d2';ctx.beginPath();ctx.moveTo(pad,pad);ctx.lineTo(pad,h-pad);ctx.lineTo(w-pad,h-pad);ctx.stroke();for(const [ri,s] of series.entries()){if(!visible[ri]||!s.points.length)continue;ctx.strokeStyle=s.color;ctx.lineWidth=2;ctx.beginPath();s.points.forEach((p,i)=>{const x=pad+i*plotW/Math.max(1,s.points.length-1),y=h-pad-(p.value-lo)/Math.max(1,hi-lo)*plotH;i?ctx.lineTo(x,y):ctx.moveTo(x,y)});ctx.stroke()}ctx.fillStyle='#536178';ctx.fillText(`${lo.toFixed(1)} bp`,2,pad);ctx.fillText(`${hi.toFixed(1)} bp`,2,h-pad);c.onmousemove=e=>{const rect=c.getBoundingClientRect(),x=e.clientX-rect.left-pad,idx=Math.max(0,Math.min(active[0].points.length-1,Math.round(x/plotW*Math.max(1,active[0].points.length-1)))),p=active[0].points[idx];if(!p)return;document.querySelector('#hover').textContent=`ts ${p.ts} · `+active.map(s=>`${s.name}: ${s.points[idx]?.value.toFixed(2)??'—'} bp`).join(' · ')};c.onmouseleave=()=>document.querySelector('#hover').textContent='移动鼠标到曲线上查看时间与收益';}function renderLegend(){document.querySelector('#legend').innerHTML=series.map((s,i)=>`<label><input type="checkbox" data-series="${i}" ${visible[i]?'checked':''}><span style="color:${s.color}">●</span> ${esc(s.name)}</label>`).join('');document.querySelectorAll('[data-series]').forEach(box=>box.onchange=e=>{visible[Number(e.target.dataset.series)]=e.target.checked;draw()})}renderEvents();renderTables();draw();renderLegend();document.querySelector('#range').oninput=()=>{draw();renderLegend()};window.onresize=draw;</script></html>"#;
    let diagnostic_markup = r#"<section id="strategyDiagnostics" style="display:none"><h2>策略未来数据与预热诊断</h2><p class="muted">诊断结果按策略、标的与数据指纹绑定；报告展示具体计算边界。</p><div class="toolbar"><input id="diagSearch" type="search" placeholder="筛选标的或策略"><select id="diagLookahead"><option value="">所有未来数据状态</option><option value="potential_lookahead_detected">检测到变化</option><option value="no_change_detected">扰动后此前决策未变化</option><option value="causal_event_contract">因果事件接口；未做扰动</option><option value="insufficient_history">历史不足</option><option value="insufficient_work_budget">工作量预算不足</option></select></div><table id="diagnostics"></table></section>"#;
    let template = template.replace(
        "<section id=\"compare\">",
        &format!("{diagnostic_markup}<section id=\"compare\">"),
    );
    let contribution_markup = r#"<section id="marketContributions" style="display:none"><h2>按标的权益贡献</h2><p class="muted">采样轴与共享权益曲线对齐，并已核验初始资金加各标的贡献等于共享权益。金额保留 raw 精确值，曲线按初始资金折算为基点。</p><div class="toolbar"><label>运行 <select id="contributionRun"></select></label><label>标的 <select id="contributionInstrument"></select></label><span id="contributionHover" class="muted">移动鼠标查看时间、贡献 raw 与基点</span></div><canvas id="contributionChart"></canvas></section>"#;
    let template = template.replace(
        "<section><h2>市场来源、策略与成交归因</h2>",
        &format!("{contribution_markup}<section><h2>市场来源、策略与成交归因</h2>"),
    );
    let contribution_script = r#"function renderMarketContributions(){const section=document.querySelector('#marketContributions'),runSelect=document.querySelector('#contributionRun'),instrumentSelect=document.querySelector('#contributionInstrument'),canvas=document.querySelector('#contributionChart'),hover=document.querySelector('#contributionHover'),available=runs.filter(run=>run.instrument_equity_contributions&&typeof run.instrument_equity_contributions==='object'&&Object.keys(run.instrument_equity_contributions).length);if(!available.length)return;section.style.display='block';runSelect.innerHTML=available.map((run,i)=>`<option value="${i}">${esc(run.run_id)}</option>`).join('');const instruments=Object.keys(available[0].instrument_equity_contributions).sort();instrumentSelect.innerHTML=instruments.map(instrument=>`<option value="${esc(instrument)}">${esc(instrument)}</option>`).join('');function draw(){const run=available[Number(runSelect.value)],instrument=instrumentSelect.value,rows=run.instrument_equity_contributions[instrument]||[],initial=BigInt(run.initial_cash_raw||'0');if(!rows.length||initial<=0n){hover.textContent='该运行或标的没有可显示的已验证贡献采样';return}const points=rows.map(row=>({ts:row.ts,raw:row.pnl_contribution_raw,bps:Number(BigInt(row.pnl_contribution_raw)*10000n/initial)})),ctx=canvas.getContext('2d'),dpr=devicePixelRatio||1,w=canvas.clientWidth,h=360;canvas.width=w*dpr;canvas.height=h*dpr;ctx.scale(dpr,dpr);const lo=Math.min(0,...points.map(p=>p.bps)),hi=Math.max(0,...points.map(p=>p.bps)),pad=42,pw=w-pad*2,ph=h-pad*2;ctx.strokeStyle='#bcc5d2';ctx.beginPath();ctx.moveTo(pad,pad);ctx.lineTo(pad,h-pad);ctx.lineTo(w-pad,h-pad);ctx.stroke();ctx.strokeStyle='#1b9e77';ctx.lineWidth=2;ctx.beginPath();points.forEach((p,i)=>{const x=pad+i*pw/Math.max(1,points.length-1),y=h-pad-(p.bps-lo)/Math.max(1,hi-lo)*ph;i?ctx.lineTo(x,y):ctx.moveTo(x,y)});ctx.stroke();ctx.fillStyle='#536178';ctx.fillText(`${lo.toFixed(1)} bp`,2,pad);ctx.fillText(`${hi.toFixed(1)} bp`,2,h-pad);canvas.onmousemove=e=>{const rect=canvas.getBoundingClientRect(),x=e.clientX-rect.left-pad,index=Math.max(0,Math.min(points.length-1,Math.round(x/pw*Math.max(1,points.length-1)))),p=points[index];hover.textContent=`${run.run_id} · ${instrument} · ts ${p.ts} · ${p.raw} ${run.settlement_currency} raw · ${p.bps} bp`};canvas.onmouseleave=()=>{hover.textContent='移动鼠标查看时间、贡献 raw 与基点'}}runSelect.onchange=draw;instrumentSelect.onchange=draw;window.addEventListener('resize',draw);draw()}"#;
    let template = template.replace(
        "function renderEvents(){",
        r#"function renderDiagnostics(){const items=cmp?[]:(r.market_strategy_diagnostics||[]);if(!items.length)return;document.querySelector('#strategyDiagnostics').style.display='block';const rows=items.map(item=>{const d=item.report||{},look=d.lookahead?.status??'—',startup=d.startup?.status??'—',pit=d.data_visibility?.status??'—';return `<tr data-search="${esc([item.instrument,item.strategy_id].join(' ')).toLowerCase()}" data-lookahead="${esc(look)}"><td>${esc(item.instrument)}</td><td><code>${esc(item.strategy_id)}</code></td><td>${esc(d.input_kind??'bar')} · ${esc(d.input_bars??d.input_events??'—')} · ${esc(d.as_of??'—')}</td><td>${esc(look)}</td><td>${esc(startup)}</td><td>${esc(pit)}</td><td><details><summary>查看</summary><pre>${esc(JSON.stringify(d,null,2))}</pre></details></td></tr>`}).join('');document.querySelector('#diagnostics').innerHTML='<tr><th>标的</th><th>策略</th><th>输入覆盖</th><th>未来数据</th><th>预热</th><th>PIT</th><th>明细</th></tr>'+rows;function apply(){const q=document.querySelector('#diagSearch').value.trim().toLowerCase(),s=document.querySelector('#diagLookahead').value;document.querySelectorAll('#diagnostics tr[data-search]').forEach(row=>{row.hidden=!(row.dataset.search.includes(q)&&(!s||row.dataset.lookahead===s))})}document.querySelector('#diagSearch').oninput=apply;document.querySelector('#diagLookahead').onchange=apply}function renderEvents(){"#,
    );
    let template = template.replace(
        "function renderEvents(){",
        &(contribution_script.to_owned() + "function renderEvents(){"),
    );
    let template = template.replace(
        "renderEvents();renderTables();draw();",
        "renderEvents();renderTables();renderDiagnostics();renderMarketContributions();draw();",
    );
    Ok(template
        .replace("__DATA__", &data)
        .replace("__CMP__", if comparison { "true" } else { "false" }))
}

fn bound_html_series(report: &mut Value) {
    if let Some(equity) = report["equity"].as_array_mut() {
        if equity.len() > 2_000 {
            let stride = equity.len().div_ceil(1_999);
            let last = equity.last().cloned();
            let mut sampled = equity
                .iter()
                .step_by(stride)
                .take(1_999)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(last) = last {
                if sampled.last() != Some(&last) {
                    sampled.push(last);
                }
            }
            *equity = sampled;
        }
    }
    if let Some(fills) = report["fills"].as_array_mut() {
        if fills.len() > 2_000 {
            let recent = fills.split_off(fills.len() - 2_000);
            *fills = recent;
        }
    }
    if let Some(events) = report["events"].as_array_mut() {
        if events.len() > 2_000 {
            let recent = events.split_off(events.len() - 2_000);
            *events = recent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bound_html_series, parse_market_bar_comparison_diagnostics, render_comparison_diagnostics,
    };
    use serde_json::{json, Value};

    #[test]
    fn html_projection_is_bounded_and_keeps_equity_endpoints_and_recent_fills() {
        let equity = (0..10_001)
            .map(|value| json!([value, value]))
            .collect::<Vec<_>>();
        let fills = (0..3_001).map(Value::from).collect::<Vec<_>>();
        let events = (0..3_001).map(Value::from).collect::<Vec<_>>();
        let mut report = json!({"equity":equity,"fills":fills,"events":events});
        bound_html_series(&mut report);
        let equity = report["equity"].as_array().unwrap();
        let fills = report["fills"].as_array().unwrap();
        let events = report["events"].as_array().unwrap();
        assert!(equity.len() <= 2_000);
        assert_eq!(equity.first().unwrap()[0], 0);
        assert_eq!(equity.last().unwrap()[0], 10_000);
        assert_eq!(fills.len(), 2_000);
        assert_eq!(fills.first(), Some(&json!(1_001)));
        assert_eq!(fills.last(), Some(&json!(3_000)));
        assert_eq!(events.len(), 2_000);
        assert_eq!(events.first(), Some(&json!(1_001)));
        assert_eq!(events.last(), Some(&json!(3_000)));
    }

    #[test]
    fn multi_market_bar_comparison_diagnostics_require_unique_run_ids() {
        let payload = json!({
            "schema_version": 1,
            "kind": "qianxing-multi-market-bar-diagnostic-comparison",
            "runs": [
                {"run_id":"run-a","diagnostics":[]},
                {"run_id":"run-b","diagnostics":[]}
            ]
        });
        let parsed = parse_market_bar_comparison_diagnostics(&payload).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains_key("run-a"));

        let duplicate = json!({
            "schema_version": 1,
            "kind": "qianxing-multi-market-bar-diagnostic-comparison",
            "runs": [
                {"run_id":"run-a","diagnostics":[]},
                {"run_id":"run-a","diagnostics":[]}
            ]
        });
        assert!(parse_market_bar_comparison_diagnostics(&duplicate)
            .unwrap_err()
            .contains("重复 run_id"));
    }

    #[test]
    fn multi_market_bar_comparison_diagnostics_render_run_identity_and_escape_evidence() {
        let report = json!({
            "runs": [{
                "run_id":"run-a",
                "market_strategy_diagnostics":[{
                    "instrument":"BTCUSDT.BINANCE",
                    "strategy_id":"<strategy>",
                    "report":{
                        "input_kind":"bar",
                        "input_bars":12,
                        "as_of":99,
                        "lookahead":{"status":"no_change_detected"},
                        "startup":{"status":"startup_sensitive"},
                        "data_visibility":{"status":"visibility_unknown"},
                        "evidence":"<img src=x>"
                    }
                }]
            }]
        });
        let html = render_comparison_diagnostics(&report);
        assert!(html.contains("run-a"));
        assert!(html.contains("逐运行策略诊断对照"));
        assert!(html.contains("&lt;strategy&gt;"));
        assert!(html.contains("&lt;img src=x&gt;"));
        assert!(!html.contains("<img src=x>"));
    }
}
