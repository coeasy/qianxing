//! Verified projection of multi-market artifacts into the CLI report model.

use super::*;

pub(super) fn market_ids(report: &Value) -> Vec<Value> {
    report["markets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| m["instrument"].clone())
        .collect()
}

pub(super) fn build_report(path: &Path, artifact: &Value) -> Result<Value, String> {
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
                "manifest_fingerprint": record["manifest_fingerprint"],
                "dataset_quality_fingerprint": verified.quality_fingerprint,
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
    qx_app::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, artifact)
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
    let fees_by_currency = verified_ledger_fees_by_currency(&ledger)?;
    let events_truncated = event_offset.saturating_add(events.len() as u64) < event_count;
    Ok(
        json!({"run_id":outcome["run_id"],"account_id":outcome["account_id"],"settlement_currency":artifact["spec"]["settlement_currency"],"initial_cash_raw":artifact["spec"]["initial_cash_raw"].to_string(),"result_hash":outcome["shared_result_hash"],"data_fingerprint":composed,"markets_total":outcome["markets_total"],"strategies_total":outcome["strategies_total"],"successful_fills":outcome["successful_fills"],"equity_points":outcome["equity_points"],"final_equity_raw":outcome["final_equity_raw"],"return_bps":outcome["return_bps"],"max_drawdown_bps":outcome["max_drawdown_bps"],"metrics_definition":qx_app::artifact_store::backtest_metrics_definition_json(),"metrics_exact":{"fees_by_currency":fees_by_currency},"summary_path":path,"input_artifacts":input_artifacts,"strategy_identities":artifact["strategy_identities"],"equity":equity,"equity_exact":exact_equity,"instrument_equity_contributions":instrument_equity_contributions,"fills":fills,"events":events,"event_count":event_count,"event_offset":event_offset,"events_truncated":events_truncated,"event_log_digest":manifest.output_event_hash,"markets":markets}),
    )
}

fn verified_ledger_fees_by_currency(ledger: &qx_core::Ledger) -> Result<Value, String> {
    let mut totals = std::collections::BTreeMap::<String, qx_core::ExactDecimal>::new();
    for entry in ledger
        .entries()
        .iter()
        .filter(|entry| entry.kind == qx_core::LedgerEntryKind::Fee)
    {
        let currency = entry.currency.trim().to_ascii_uppercase();
        if currency.is_empty() {
            return Err("已重放账簿中的手续费缺少币种".into());
        }
        let amount = match entry.exact_amounts {
            Some(exact) => exact.amount,
            None => qx_core::ExactDecimal::from_parts(entry.amount.raw(), 9)
                .map_err(|error| format!("已重放账簿手续费无法转为精确金额: {error}"))?,
        };
        let total = totals
            .entry(currency.clone())
            .or_insert(qx_core::ExactDecimal::ZERO);
        *total = total
            .checked_add(amount)
            .ok_or_else(|| format!("{currency} 精确手续费累计溢出"))?;
    }
    Ok(Value::Object(
        totals
            .into_iter()
            .map(|(currency, amount)| (currency, Value::String(amount.to_string())))
            .collect(),
    ))
}

/// Rebuild bounded causal and startup diagnostics for every Rust built-in strategy in a
/// verified shared-account multi-market Bar run, then optionally attach them to the offline
/// interactive report.
pub(super) fn resolve_path(reference: &str, artifact_path: &Path) -> PathBuf {
    let referenced = Path::new(reference);
    if referenced.is_absolute() || referenced.exists() {
        return referenced.to_path_buf();
    }
    artifact_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(referenced)
}
