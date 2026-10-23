//! File-backed references used by run EvidenceBundle adapters.

use qx_core::RunManifest;
use qx_spec::{
    ArtifactRef, FoundationDocument, RunEvidenceDatasetComponent, RunEvidenceReportAnalysis,
    RunEvidenceStrategyDiagnostics, RunRecord,
};
use std::path::Path;

/// Run identity documents loaded from paths declared by a verified backtest summary.
pub struct RunEvidenceRunFiles {
    pub manifest_path: String,
    pub manifest: RunManifest,
    pub record_path: String,
    pub record: RunRecord,
}

/// Resolve a run EvidenceBundle input while retaining its caller-facing label in errors.
pub fn canonical_run_evidence_path(path: &str, label: &str) -> Result<String, String> {
    Path::new(path)
        .canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| format!("解析运行证据包 {label} 路径失败 {path}: {error}"))
}

/// Load the RunManifest and RunRecord referenced by a backtest summary.
pub fn load_run_evidence_identity_files(
    summary: &serde_json::Value,
) -> Result<RunEvidenceRunFiles, String> {
    let manifest_path = canonical_run_evidence_path(
        &crate::artifact_store::run_evidence_summary_str(summary, &["run_manifest"])?,
        "RunManifest",
    )?;
    let manifest_payload = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读取运行证据包的 RunManifest 失败 {manifest_path}: {error}"))?;
    let manifest = RunManifest::from_json(&manifest_payload)?;

    let record_path = canonical_run_evidence_path(
        &crate::artifact_store::run_evidence_summary_str(summary, &["run_record"])?,
        "RunRecord",
    )?;
    let record_payload = std::fs::read_to_string(&record_path)
        .map_err(|error| format!("读取运行证据包的 RunRecord 失败 {record_path}: {error}"))?;
    let record = RunRecord::from_json(&record_payload)?;

    Ok(RunEvidenceRunFiles {
        manifest_path,
        manifest,
        record_path,
        record,
    })
}

/// Return the optional quality-manifest location and the digest of its exact bytes.
pub fn run_evidence_quality_manifest_reference(
    summary: &serde_json::Value,
) -> Result<(Option<String>, Option<String>), String> {
    let path = summary
        .pointer("/dataset_quality/manifest_path")
        .and_then(serde_json::Value::as_str)
        .map(|path| canonical_run_evidence_path(path, "数据质量清单"))
        .transpose()?;
    let sha256 = path
        .as_deref()
        .map(qx_strategy::file_digest::sha256_file_hex)
        .transpose()?;
    Ok((path, sha256))
}

/// Canonicalize and hash all summary-declared artifacts needed by a run EvidenceBundle.
pub fn run_evidence_artifact_references(
    summary: &serde_json::Value,
    record: &RunRecord,
) -> Result<Vec<ArtifactRef>, String> {
    let mut artifacts = record.artifact_refs.clone();
    for artifact in &mut artifacts {
        artifact.path = canonical_run_evidence_path(&artifact.path, &artifact.name)?;
    }
    for (name, pointer) in [
        ("runtime_config", summary.get("runtime_config")),
        ("market_spec", summary.get("market_spec")),
        (
            "market_spec_primary",
            summary
                .get("market_specs")
                .and_then(|specs| specs.get("primary")),
        ),
        (
            "market_spec_reference",
            summary
                .get("market_specs")
                .and_then(|specs| specs.get("reference")),
        ),
        (
            "dataset_manifest_primary",
            summary.pointer("/dataset_quality/legs/primary/manifest_path"),
        ),
        (
            "dataset_manifest_reference",
            summary.pointer("/dataset_quality/legs/reference/manifest_path"),
        ),
    ] {
        let Some(source) = pointer.and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = canonical_run_evidence_path(source, name)?;
        let digest = qx_strategy::file_digest::sha256_file_hex(&path)?;
        artifacts.push(ArtifactRef {
            name: name.to_string(),
            path,
            digest,
        });
    }
    Ok(artifacts)
}

/// Resolve benchmark-analysis inputs and bind their byte digests into the run payload.
pub fn run_evidence_report_analysis(
    analysis: Option<&serde_json::Value>,
) -> Result<Option<RunEvidenceReportAnalysis>, String> {
    analysis
        .map(|analysis| {
            let input_path = analysis
                .pointer("/benchmark/path")
                .and_then(serde_json::Value::as_str)
                .ok_or("基准报告缺少 benchmark.path")?;
            let input_path = Path::new(input_path)
                .canonicalize()
                .map_err(|error| format!("解析基准输入路径失败 {input_path}: {error}"))?;
            let input_sha256 = qx_strategy::file_digest::sha256_file_hex(&input_path)?;
            let calendar_input = analysis
                .pointer("/calendar/path")
                .and_then(serde_json::Value::as_str)
                .map(|path| {
                    let path = Path::new(path)
                        .canonicalize()
                        .map_err(|error| format!("解析基准交易日历路径失败 {path}: {error}"))?;
                    let sha256 = qx_strategy::file_digest::sha256_file_hex(&path)?;
                    Ok::<_, String>((path.to_string_lossy().to_string(), sha256))
                })
                .transpose()?;
            Ok::<_, String>(RunEvidenceReportAnalysis {
                kind: "benchmark".into(),
                input_path: input_path.to_string_lossy().to_string(),
                input_sha256,
                input_fingerprint: analysis
                    .pointer("/benchmark/fingerprint")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("基准报告缺少 benchmark.fingerprint")?
                    .to_string(),
                instrument: analysis
                    .pointer("/benchmark/instrument")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("基准报告缺少 benchmark.instrument")?
                    .to_string(),
                calendar_input_path: calendar_input.as_ref().map(|(path, _)| path.clone()),
                calendar_input_sha256: calendar_input.map(|(_, sha256)| sha256),
                analysis: serde_json::to_string(analysis)
                    .map_err(|error| format!("编码基准分析结果失败: {error}"))?,
            })
        })
        .transpose()
}

/// Resolve and hash the exact strategy-diagnostic snapshot after the CLI has validated its
/// strategy, instrument, data fingerprint, and diagnostic contract.
pub fn run_evidence_strategy_diagnostics(
    summary: &serde_json::Value,
    diagnostics: Option<(&Path, &serde_json::Value)>,
) -> Result<Option<RunEvidenceStrategyDiagnostics>, String> {
    diagnostics
        .map(|(path, report)| {
            let path = path
                .canonicalize()
                .map_err(|error| format!("解析策略诊断路径失败 {}: {error}", path.display()))?;
            let metadata = std::fs::metadata(&path)
                .map_err(|error| format!("读取策略诊断文件属性失败 {}: {error}", path.display()))?;
            if metadata.len() > 20 * 1024 * 1024 {
                return Err("策略诊断文件在打包前超过 20 MiB 上限".into());
            }
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("读取策略诊断文件失败 {}: {error}", path.display()))?;
            let persisted_report: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|error| format!("策略诊断文件在打包前变成无效 JSON: {error}"))?;
            if &persisted_report != report {
                return Err("策略诊断文件在报告校验后发生变化；拒绝写入不一致证据包".into());
            }
            let strategy_id =
                crate::artifact_store::run_evidence_summary_str(summary, &["strategy_id"])?;
            let instrument =
                crate::artifact_store::run_evidence_summary_str(summary, &["instrument"])?;
            let fingerprint = crate::artifact_store::run_evidence_summary_str(
                summary,
                &["input", "fingerprint"],
            )?;
            for (field, expected) in [
                ("strategy_id", strategy_id),
                ("instrument", instrument),
                ("data_fingerprint", fingerprint),
            ] {
                if report.get(field).and_then(serde_json::Value::as_str) != Some(expected.as_str())
                {
                    return Err(format!("策略诊断 {field} 与已验证回测摘要不一致"));
                }
            }
            Ok(RunEvidenceStrategyDiagnostics {
                kind: "strategy-bias-diagnostics".into(),
                input_path: path.to_string_lossy().into_owned(),
                input_sha256: qx_strategy::sha256_hex(&bytes),
            })
        })
        .transpose()
}

/// Bind a multi-market diagnostic array to its exact source bytes after the report adapter has
/// validated every strategy slot, instrument, component fingerprint, row count and end timestamp.
pub fn run_evidence_strategy_diagnostics_array(
    path: &Path,
    diagnostics: &serde_json::Value,
) -> Result<RunEvidenceStrategyDiagnostics, String> {
    run_evidence_strategy_diagnostics_array_with_kind(
        path,
        diagnostics,
        "strategy-bias-diagnostics-array",
    )
}

/// Bind a multi-market depth diagnostic array after the report adapter has validated each
/// strategy slot against its DepthFrame identity and event coverage.
pub fn run_evidence_depth_strategy_diagnostics_array(
    path: &Path,
    diagnostics: &serde_json::Value,
) -> Result<RunEvidenceStrategyDiagnostics, String> {
    run_evidence_strategy_diagnostics_array_with_kind(
        path,
        diagnostics,
        "strategy-bias-diagnostics-depth-array",
    )
}

fn run_evidence_strategy_diagnostics_array_with_kind(
    path: &Path,
    diagnostics: &serde_json::Value,
    kind: &str,
) -> Result<RunEvidenceStrategyDiagnostics, String> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("解析多市场策略诊断路径失败 {}: {error}", path.display()))?;
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("读取多市场策略诊断文件属性失败 {}: {error}", path.display()))?;
    if metadata.len() > 1024 * 1024 {
        return Err("多市场策略诊断集合在打包前超过 1 MiB 上限".into());
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("读取多市场策略诊断文件失败 {}: {error}", path.display()))?;
    let persisted: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("多市场策略诊断文件在打包前变成无效 JSON: {error}"))?;
    if &persisted != diagnostics {
        return Err("多市场策略诊断文件在报告校验后发生变化；拒绝写入不一致证据包".into());
    }
    Ok(RunEvidenceStrategyDiagnostics {
        kind: kind.into(),
        input_path: path.to_string_lossy().into_owned(),
        input_sha256: qx_strategy::sha256_hex(&bytes),
    })
}

/// Project one multi-input dataset component after the adapter has decoded its BarFrame.
pub fn run_evidence_dataset_component(
    summary: &serde_json::Value,
    role: &str,
    row_count: u64,
    path: String,
) -> Result<RunEvidenceDatasetComponent, String> {
    let quality_pointer = format!("/dataset_quality/legs/{role}/manifest_path");
    let quality_manifest_path = summary
        .pointer(&quality_pointer)
        .and_then(serde_json::Value::as_str)
        .map(|path| canonical_run_evidence_path(path, &format!("{role} 数据质量清单")))
        .transpose()?;
    let quality_manifest_sha256 = quality_manifest_path
        .as_deref()
        .map(qx_strategy::file_digest::sha256_file_hex)
        .transpose()?;
    let quality_pointer = format!("/dataset_quality/legs/{role}/quality_report");
    let quality_report = summary
        .pointer(&quality_pointer)
        .cloned()
        .map(|mut report| {
            if let Some(fields) = report.as_object_mut() {
                fields.remove("row_count");
            }
            serde_json::from_value(report)
        })
        .transpose()
        .map_err(|error| format!("回测摘要的 {role} 腿数据质量报告无效: {error}"))?;

    let read_string = |field| {
        crate::artifact_store::run_evidence_summary_str(summary, &["input", "legs", role, field])
    };
    let manifest_fingerprint = summary
        .pointer(&format!(
            "/dataset_quality/legs/{role}/manifest_fingerprint"
        ))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok(RunEvidenceDatasetComponent {
        role: role.to_string(),
        dataset_id: read_string("dataset_id")?,
        version: read_string("dataset_version")?,
        content_fingerprint: read_string("fingerprint")?,
        row_count,
        path,
        manifest_fingerprint,
        quality_report,
        quality_manifest_path,
        quality_manifest_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::run_evidence_report_analysis;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn run_evidence_binds_calendar_path_digest_and_daily_analysis() {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest
            .parent()
            .and_then(|path| path.parent())
            .expect("qx-app manifest is nested in the workspace")
            .to_path_buf();
        let root = workspace.join("target").join(format!(
            "qx-app-run-evidence-benchmark-calendar-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create isolated EvidenceBundle fixture");
        let benchmark_path = root.join("benchmark-bars.json");
        let calendar_path = root.join("calendar.json");
        let benchmark_bytes = br#"{"instrument":"000300.SSE","ts":[1,2]}"#;
        let calendar_bytes = br#"{"calendar_id":"cn-test","trading_days":["1970-01-01"]}"#;
        std::fs::write(&benchmark_path, benchmark_bytes).expect("write benchmark fixture");
        std::fs::write(&calendar_path, calendar_bytes).expect("write calendar fixture");
        let analysis = json!({
            "schema_version": 1,
            "alignment": "exact_timestamp_and_trading_calendar_coverage",
            "period_basis": "per_trading_day_no_annualization",
            "benchmark": {
                "path": benchmark_path,
                "fingerprint": "benchmark-fingerprint-v1",
                "instrument": "000300.SSE"
            },
            "calendar": {
                "path": calendar_path,
                "calendar_id": "cn-test",
                "sha256": "analysis-side-digest"
            },
            "metrics": {
                "mean_active_return_bps_per_trading_day": 1.0
            }
        });

        let projected = run_evidence_report_analysis(Some(&analysis))
            .expect("project EvidenceBundle benchmark inputs")
            .expect("benchmark analysis exists");
        let resolved_benchmark = benchmark_path.canonicalize().unwrap();
        let resolved_calendar = calendar_path.canonicalize().unwrap();
        assert_eq!(projected.kind, "benchmark");
        assert_eq!(projected.input_path, resolved_benchmark.to_string_lossy());
        assert_eq!(
            projected.input_sha256,
            qx_strategy::file_digest::sha256_file_hex(&resolved_benchmark).unwrap()
        );
        assert_eq!(
            projected.calendar_input_path.as_deref(),
            Some(resolved_calendar.to_string_lossy().as_ref())
        );
        assert_eq!(
            projected.calendar_input_sha256.as_deref(),
            Some(
                qx_strategy::file_digest::sha256_file_hex(&resolved_calendar)
                    .unwrap()
                    .as_str()
            )
        );
        let embedded: serde_json::Value = serde_json::from_str(&projected.analysis).unwrap();
        assert_eq!(
            embedded["alignment"],
            "exact_timestamp_and_trading_calendar_coverage"
        );
        assert_eq!(embedded["calendar"]["calendar_id"], "cn-test");
        assert_eq!(
            embedded["metrics"]["mean_active_return_bps_per_trading_day"],
            1.0
        );

        let without_calendar = run_evidence_report_analysis(Some(&json!({
            "benchmark": {
                "path": resolved_benchmark,
                "fingerprint": "benchmark-fingerprint-v1",
                "instrument": "000300.SSE"
            }
        })))
        .expect("project benchmark without optional calendar")
        .unwrap();
        assert_eq!(without_calendar.calendar_input_path, None);
        assert_eq!(without_calendar.calendar_input_sha256, None);
        let _ = std::fs::remove_dir_all(root);
    }
}
