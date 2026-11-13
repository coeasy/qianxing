//! Idempotent persistence of a verified multi-market report and its evidence artifacts.

use super::super::artifacts::write_text;
use super::super::run_multi_market_manifest::{
    build_run_manifest, ensure_event_trace_jsonl, existing_identical, invalid, DataIdentity,
    MultiMarketManifestInput, PreparedMarket,
};
use super::MultiMarketExecutionWindow;
use crate::{
    AppError, AppErrorCategory, MultiMarketBacktestOutcome, MultiMarketBacktestSpec, RunContext,
    StrategyArtifactIdentity,
};
use qx_core::ExactDecimal;
use qx_xingban::PortfolioBarSessionReport;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub(super) fn persist_multi_market_report(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
    report: PortfolioBarSessionReport,
    prepared: Vec<PreparedMarket>,
    strategy_identities: Vec<StrategyArtifactIdentity>,
    data_identities: Vec<DataIdentity>,
    data_fingerprint: String,
    spec_fingerprint: String,
    strategy_count: usize,
    execution_window: Option<MultiMarketExecutionWindow>,
    experiment_parameters: Option<&BTreeMap<String, i128>>,
    mut input_components: BTreeMap<String, String>,
    initial_cash_exact: ExactDecimal,
) -> Result<Option<MultiMarketBacktestOutcome>, AppError> {
    let final_equity_raw = report
        .equity
        .last()
        .map(|(_, value)| *value)
        .unwrap_or(spec.initial_cash_raw);
    let initial_equity_exact = initial_cash_exact;
    let final_equity_exact = report
        .equity_exact
        .last()
        .map(|(_, _, value)| *value)
        .unwrap_or(initial_equity_exact);
    let return_bps = crate::report_analysis::exact_equity_total_return_bps_v1(
        initial_equity_exact,
        final_equity_exact,
    )
    .map_err(invalid)?;
    let exact_equity = report
        .equity_exact
        .iter()
        .map(|(_, _, equity)| *equity)
        .collect::<Vec<_>>();
    let max_drawdown_bps =
        crate::report_analysis::exact_equity_max_drawdown_bps_v1(&exact_equity).map_err(invalid)?;
    let successful_fills =
        u64::try_from(report.fills.len()).map_err(|_| invalid("多市场成交数超出 u64 范围"))?;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多市场回测产物目录", &error))?;
    let output_root = PathBuf::from(&spec.output_dir)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场回测产物目录", &error))?;
    let output_path = output_root.join(format!("{}.multi-market.json", spec.run_id));
    let artifact_path = output_path.to_string_lossy().into_owned();
    let manifest_path = output_root.join(format!("{}.multi-market.run.json", spec.run_id));
    let run_manifest_path = manifest_path.to_string_lossy().into_owned();
    let equity_path = output_root.join(format!("{}.multi-market.equity.json", spec.run_id));
    let equity_artifact_path = equity_path.to_string_lossy().into_owned();
    let equity_exact_path =
        output_root.join(format!("{}.multi-market.equity-exact.json", spec.run_id));
    let equity_exact_artifact_path = equity_exact_path.to_string_lossy().into_owned();
    let fills_path = output_root.join(format!("{}.multi-market.fills.json", spec.run_id));
    let fills_artifact_path = fills_path.to_string_lossy().into_owned();
    let event_trace_path = output_root.join(format!("{}.multi-market.events.jsonl", spec.run_id));
    let event_trace_artifact_path = event_trace_path.to_string_lossy().into_owned();
    let instrument_equity_path = output_root.join(format!(
        "{}.multi-market.instrument-equity.json",
        spec.run_id
    ));
    let instrument_equity_artifact_path = instrument_equity_path.to_string_lossy().into_owned();
    let instrument_equity_payload = report
        .instrument_equity_contributions
        .iter()
        .map(|(instrument, rows)| {
            let exact_rows = report
                .instrument_equity_contributions_exact
                .get(instrument)
                .ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        format!("{instrument} exact contribution curve is missing"),
                    )
                })?;
            if rows.len() != exact_rows.len()
                || rows.iter().zip(exact_rows).any(
                    |((index, ts, _), (exact_index, exact_ts, _))| {
                        index != exact_index || ts != exact_ts
                    },
                )
            {
                return Err(AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("{instrument} raw and exact contribution axes differ"),
                ));
            }
            let payload = rows
                .iter()
                .zip(exact_rows)
                .map(|((equity_index, ts, value), (_, _, exact_value))| {
                    serde_json::json!({
                        "equity_index": equity_index,
                        "ts": ts,
                        "pnl_contribution_raw": value.to_string(),
                        "pnl_contribution_exact": exact_value.to_string()
                    })
                })
                .collect::<Vec<_>>();
            Ok((instrument.to_string(), payload))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    let instrument_equity_json = serde_json::to_string(&instrument_equity_payload)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let equity_exact_json = serde_json::to_string(
        &report
            .equity_exact
            .iter()
            .map(|(equity_index, ts, value)| {
                serde_json::json!({
                    "equity_index": equity_index,
                    "ts": ts,
                    "equity_exact": value.to_string()
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    input_components.insert(
        "derived:instrument-equity-contributions".into(),
        qx_data::sha256_hex_bytes(instrument_equity_json.as_bytes()),
    );
    input_components.insert(
        "derived:equity-exact".into(),
        qx_data::sha256_hex_bytes(equity_exact_json.as_bytes()),
    );
    let result_hash = format!("{:016x}", report.result_hash());
    let event_hash = format!("{:016x}", report.event_log.digest());
    let run_manifest_json = build_run_manifest(MultiMarketManifestInput {
        spec,
        context,
        data_fingerprint: &data_fingerprint,
        input_components,
        clock_start: report.clock_start,
        clock_end: report.clock_end,
        spec_fingerprint: &spec_fingerprint,
        execution_window,
        experiment_parameters,
        result_hash: &result_hash,
        event_hash: &event_hash,
        strategy_identities: &strategy_identities,
        data_identities: &data_identities,
    })?;
    let equity_json = serde_json::to_string(&report.equity)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let fills_json = serde_json::to_string(&report.fills)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let outcome = MultiMarketBacktestOutcome {
        run_id: spec.run_id.clone(),
        account_id: spec.account_id.clone(),
        spec_fingerprint,
        data_fingerprint,
        shared_result_hash: format!("{:016x}", report.result_hash()),
        markets_total: spec.markets.len(),
        strategies_total: strategy_count,
        successful_fills,
        equity_points: u64::try_from(report.equity.len())
            .map_err(|_| invalid("多市场权益点数超出 u64 范围"))?,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
        artifact_path: artifact_path.clone(),
    };
    let per_market_fills = report
        .fills_by_instrument
        .iter()
        .map(|(instrument, fills)| (instrument.to_string(), fills.len()))
        .collect::<BTreeMap<_, _>>();
    let mut artifact = serde_json::json!({
        "schema_version": 2,
        "spec": spec,
        "outcome": outcome,
        "market_data": data_identities,
        "dataset_quality": prepared.iter().filter_map(|market| market.dataset_quality.as_ref().map(|quality| (market.identity.instrument.clone(), quality.clone()))).collect::<BTreeMap<_, _>>(),
        "strategy_identities": strategy_identities,
        "per_market_fills": per_market_fills,
        "funding_by_instrument_raw": report.funding_by_instrument_raw,
        "funding_events_by_instrument": report.funding_events_by_instrument,
        "funding_rate_input": spec.markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "funding_bps": market.funding_bps,
            "funding_events": market.funding_events,
        })).collect::<Vec<_>>(),
        "equity": report.equity,
        "event_count": report.event_log.events().len(),
        "event_trace_path": event_trace_artifact_path,
        "ledger_entries": report.ledger.entries(),
        "replay_verified": true,
        "run_manifest_path": run_manifest_path,
        "equity_path": equity_artifact_path,
        "equity_exact_path": equity_exact_artifact_path,
        "fills_path": fills_artifact_path,
        "instrument_equity_contributions_path": instrument_equity_artifact_path,
    });
    if let Some(window) = execution_window {
        artifact["execution_window"] = serde_json::json!(window);
    }
    if let Some(parameters) = experiment_parameters {
        artifact["experiment_parameters"] = serde_json::json!(parameters);
    }
    let artifact_json = serde_json::to_string(&artifact)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let manifest_exists = match std::fs::read_to_string(&manifest_path) {
        Ok(existing) if existing == run_manifest_json => true,
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场运行 RunManifest 已存在且内容不同: {run_manifest_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(AppError::from_io("读取多市场 RunManifest", &error)),
    };
    let equity_exists = existing_identical(&equity_path, &equity_json, "权益曲线")?;
    let equity_exact_exists =
        existing_identical(&equity_exact_path, &equity_exact_json, "精确权益曲线")?;
    let fills_exists = existing_identical(&fills_path, &fills_json, "成交明细")?;
    let instrument_equity_exists = existing_identical(
        &instrument_equity_path,
        &instrument_equity_json,
        "逐标的权益贡献曲线",
    )?;
    match std::fs::read_to_string(&output_path) {
        Ok(existing) if existing == artifact_json => {
            ensure_event_trace_jsonl(&event_trace_path, report.event_log.events(), "事件轨迹")?;
            if !equity_exists {
                write_text(&equity_artifact_path, &equity_json)?;
            }
            if !equity_exact_exists {
                write_text(&equity_exact_artifact_path, &equity_exact_json)?;
            }
            if !fills_exists {
                write_text(&fills_artifact_path, &fills_json)?;
            }
            if !instrument_equity_exists {
                write_text(&instrument_equity_artifact_path, &instrument_equity_json)?;
            }
            if !manifest_exists {
                write_text(&run_manifest_path, &run_manifest_json)?;
            }
            return Ok(Some(outcome));
        }
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场运行产物已存在且内容不同: {artifact_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取多市场回测产物", &error)),
    }
    if !equity_exists {
        write_text(&equity_artifact_path, &equity_json)?;
    }
    if !equity_exact_exists {
        write_text(&equity_exact_artifact_path, &equity_exact_json)?;
    }
    if !fills_exists {
        write_text(&fills_artifact_path, &fills_json)?;
    }
    if !instrument_equity_exists {
        write_text(&instrument_equity_artifact_path, &instrument_equity_json)?;
    }
    if !manifest_exists {
        write_text(&run_manifest_path, &run_manifest_json)?;
    }
    ensure_event_trace_jsonl(&event_trace_path, report.event_log.events(), "事件轨迹")?;
    write_text(&outcome.artifact_path, &artifact_json)?;
    Ok(Some(outcome))
}
