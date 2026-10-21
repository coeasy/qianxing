//! Persistence for the combined report of two independently replayed Bar legs.

use crate::artifact_store::{
    persist_run_manifest, persist_verified_run_record, write_backtest_artifact,
    BacktestInputProvenance, BACKTEST_SUMMARY_SCHEMA_VERSION,
};
use crate::multi_leg_combined_equity_metrics;
use qx_datastruct::BarFrame;
use qx_xingban::{BacktestReport, RunManifestIdentity};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Inputs already resolved and verified by the facade for one multi-leg composition report.
pub struct MultiLegCompositionArtifactInput<'a> {
    pub root: &'a Path,
    /// Stable identity fingerprint from the parsed runtime config, when one was supplied.
    pub config_fingerprint: Option<&'a str>,
    pub runtime_config_path: Option<&'a str>,
    pub code_commit: &'a str,
    pub strategy_id: &'a str,
    pub primary_input: &'a BacktestInputProvenance,
    pub reference_input: &'a BacktestInputProvenance,
    pub primary_frame: &'a BarFrame,
    pub reference_frame: &'a BarFrame,
    pub primary_report: &'a BacktestReport,
    pub reference_report: &'a BacktestReport,
    pub leg_summary_paths: [&'a Path; 2],
    pub dataset_quality: [Option<&'a serde_json::Value>; 2],
    pub summary: serde_json::Value,
}

/// Bind two replay-verified leg results and persist their composition summary and CSV projections.
pub fn persist_multi_leg_composition_artifacts(
    input: MultiLegCompositionArtifactInput<'_>,
) -> Result<(PathBuf, serde_json::Value), String> {
    let MultiLegCompositionArtifactInput {
        root,
        config_fingerprint,
        runtime_config_path,
        code_commit,
        strategy_id,
        primary_input,
        reference_input,
        primary_frame,
        reference_frame,
        primary_report,
        reference_report,
        leg_summary_paths,
        dataset_quality,
        mut summary,
    } = input;

    if dataset_quality[0].is_some() != dataset_quality[1].is_some() {
        return Err("组合级数据质量清单必须同时声明主腿和对冲腿".into());
    }

    let data_fingerprint = qx_strategy::sha256_hex(
        format!(
            "primary:{}|reference:{}",
            primary_input.fingerprint, reference_input.fingerprint
        )
        .as_bytes(),
    );
    let result_hash = qx_strategy::sha256_hex(
        format!(
            "primary:{:016x}|reference:{:016x}",
            primary_report.result_hash(),
            reference_report.result_hash()
        )
        .as_bytes(),
    );
    let base_config_hash = config_fingerprint.map(str::to_owned).unwrap_or_else(|| {
        qx_strategy::sha256_hex(
            format!(
                "{strategy_id}|{}|{}",
                primary_input.fingerprint, reference_input.fingerprint
            )
            .as_bytes(),
        )
    });
    let config_hash = crate::multi_leg_quality_fingerprint(dataset_quality).map_or_else(
        || base_config_hash.clone(),
        |quality_fingerprint| {
            qx_strategy::sha256_hex(
                format!("{base_config_hash}|dataset-quality:{quality_fingerprint}").as_bytes(),
            )
        },
    );
    let run_id = format!(
        "multi-composition:{strategy_id}:{}:{}:{}",
        primary_frame.instrument,
        reference_frame.instrument,
        &result_hash[..12]
    );
    let mut manifest = primary_report.run_manifest_with_input_components(
        RunManifestIdentity {
            run_id: &run_id,
            code_commit,
            config_hash: &config_hash,
            strategy_version: &format!("{strategy_id}-v1"),
            instrument_spec_version: "multi-leg-specs-v1",
            runtime_version: "multi-leg-composition-v1",
        },
        &data_fingerprint,
        BTreeMap::from([
            (
                "primary_barframe".to_string(),
                primary_input.fingerprint.clone(),
            ),
            (
                "reference_barframe".to_string(),
                reference_input.fingerprint.clone(),
            ),
            (
                "primary_result".to_string(),
                format!("{:016x}", primary_report.result_hash()),
            ),
            (
                "reference_result".to_string(),
                format!("{:016x}", reference_report.result_hash()),
            ),
        ]),
    )?;
    manifest.result_hash = result_hash.clone();
    manifest.output_event_hash = result_hash.clone();
    manifest.input_event_hash = data_fingerprint.clone();
    let manifest_path =
        persist_run_manifest(root, &manifest).map_err(|error| error.message().to_string())?;
    let manifest_stem = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("multi-composition.run.json")
        .strip_suffix(".run.json")
        .unwrap_or("multi-composition");
    let stem = format!("{manifest_stem}.v{BACKTEST_SUMMARY_SCHEMA_VERSION}");
    let runs = root.join("runs");
    let summary_path = runs.join(format!("{stem}.summary.json"));
    let equity_path = runs.join(format!("{stem}.equity.csv"));
    let fills_path = runs.join(format!("{stem}.fills.csv"));
    let record_path = runs.join(format!("{stem}.record.json"));
    let input_json = serde_json::json!({
        "kind": "multi-barframe",
        "path": format!("{}|{}", primary_input.path, reference_input.path),
        "dataset_id": format!("{}+{}", primary_input.dataset_id, reference_input.dataset_id),
        "dataset_version": qx_data::BARFRAME_DATASET_VERSION,
        "fingerprint": data_fingerprint,
        "legs": {
            "primary": {
                "path": primary_input.path,
                "dataset_id": primary_input.dataset_id,
                "dataset_version": primary_input.dataset_version,
                "fingerprint": primary_input.fingerprint
            },
            "reference": {
                "path": reference_input.path,
                "dataset_id": reference_input.dataset_id,
                "dataset_version": reference_input.dataset_version,
                "fingerprint": reference_input.fingerprint
            }
        }
    });
    summary["schema_version"] = serde_json::json!(BACKTEST_SUMMARY_SCHEMA_VERSION);
    summary["instrument"] = serde_json::json!(format!(
        "{}+{}",
        primary_frame.instrument, reference_frame.instrument
    ));
    summary["bars"] = serde_json::json!(primary_frame.ts.len());
    summary["fills"] = serde_json::json!(primary_report.fills.len() + reference_report.fills.len());
    let execution_attempts = primary_report
        .execution_attempts
        .iter()
        .chain(&reference_report.execution_attempts)
        .collect::<Vec<_>>();
    summary["execution_attempt_count"] = serde_json::json!(execution_attempts.len());
    summary["execution_attempts"] = serde_json::to_value(execution_attempts)
        .map_err(|error| format!("编码组合逐单执行记录失败: {error}"))?;
    summary["input"] = input_json;
    if dataset_quality.iter().any(Option::is_some) {
        let quality_report =
            crate::aggregate_multi_leg_quality_report(dataset_quality[0], dataset_quality[1]);
        let manifest_fingerprint = crate::multi_leg_quality_fingerprint(dataset_quality)
            .ok_or("组合级质量指纹缺少任一腿清单")?;
        summary["dataset_quality"] = serde_json::json!({
            "manifest_fingerprint": manifest_fingerprint,
            "quality_report": quality_report,
            "legs": {
                "primary": dataset_quality[0],
                "reference": dataset_quality[1],
            }
        });
    }
    summary["runtime_config"] = runtime_config_path
        .map(|path| serde_json::Value::String(path.to_string()))
        .unwrap_or(serde_json::Value::Null);
    summary["leg_summaries"] = serde_json::json!([
        leg_summary_paths[0].to_string_lossy(),
        leg_summary_paths[1].to_string_lossy()
    ]);
    summary["input_data_hash"] = serde_json::json!(data_fingerprint);
    summary["result_hash"] = serde_json::json!(result_hash);
    summary["replay_contract"] = serde_json::json!(
        "both-leg ledgers are replay-verified independently; composition hash binds both result hashes; no synthetic consolidated ledger"
    );
    summary["run_manifest"] = serde_json::json!(manifest_path.to_string_lossy());
    summary["run_record"] = serde_json::json!(record_path.to_string_lossy());

    let primary_initial_cash = summary["accounts"]["primary_initial_cash"]
        .as_str()
        .ok_or("组合摘要缺少主腿初始权益")?
        .parse::<i128>()
        .map_err(|error| format!("主腿初始权益无法解析: {error}"))?;
    let reference_initial_cash = summary["accounts"]["reference_initial_cash"]
        .as_str()
        .ok_or("组合摘要缺少对冲腿初始权益")?
        .parse::<i128>()
        .map_err(|error| format!("对冲腿初始权益无法解析: {error}"))?;
    let combined_equity = multi_leg_combined_equity_metrics(
        &primary_report.equity,
        &reference_report.equity,
        primary_initial_cash,
        reference_initial_cash,
        primary_frame.ts.len(),
    )?;
    let combined_return_bps = i32::try_from(
        summary["totals"]["combined_return_bps"]
            .as_i64()
            .ok_or("组合摘要缺少 combined_return_bps")?,
    )
    .map_err(|_| "组合收益基点超出 i32 范围")?;
    let risk = qx_core::backtest_risk_metrics(
        &combined_equity.equity_raw,
        combined_return_bps,
        combined_equity.max_drawdown_bps,
    );
    summary["metrics"] = serde_json::json!({
        "return_bps": combined_return_bps,
        "max_drawdown_bps": combined_equity.max_drawdown_bps,
        "fees_raw": summary["totals"]["fees_raw"],
        "turnover_raw": summary["totals"]["turnover_raw"],
        "final_equity_raw": combined_equity.final_equity_raw.to_string(),
        "sharpe": risk.sharpe,
        "sortino": risk.sortino,
        "calmar": risk.calmar,
        "win_rate": risk.win_rate,
        "profit_factor": risk.profit_factor
    });
    let summary_payload = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("编码组合级回测摘要失败: {error}"))?;
    let persisted_summary: serde_json::Value = serde_json::from_str(&summary_payload)
        .map_err(|error| format!("组合级回测摘要序列化后读回失败: {error}"))?;
    write_backtest_artifact(&summary_path, &summary_payload, "组合级回测摘要")?;

    let mut equity_payload = String::from("index,ts,primary_equity_raw,reference_equity_raw\n");
    for (index, (primary, reference)) in primary_report
        .equity
        .iter()
        .zip(&reference_report.equity)
        .enumerate()
    {
        let ts = primary_frame.ts.get(index).copied().unwrap_or_default();
        equity_payload.push_str(&format!("{index},{ts},{primary},{reference}\n"));
    }
    write_backtest_artifact(&equity_path, &equity_payload, "组合级权益曲线")?;

    let mut fills_payload = String::from(
        "leg,order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id\n",
    );
    for (label, fills) in [
        ("primary", &primary_report.fills),
        ("reference", &reference_report.fills),
    ] {
        for fill in fills {
            fills_payload.push_str(&format!(
                "{label},{},{},{},{},{},{},{},{},{},{},{}\n",
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
                fill.venue_order_id.as_deref().unwrap_or_default()
            ));
        }
    }
    write_backtest_artifact(&fills_path, &fills_payload, "组合级成交明细")?;
    persist_verified_run_record(&manifest_path, &summary_path, &equity_path, &fills_path)?;
    Ok((summary_path, persisted_summary))
}
