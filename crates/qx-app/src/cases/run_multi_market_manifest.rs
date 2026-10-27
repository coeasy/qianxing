use super::run_multi_market_backtest::MultiMarketExecutionWindow;
use crate::{AppError, AppErrorCategory, MultiMarketBacktestSpec, RunContext};
use qx_core::{InstrumentId, MakerTakerFeeModel, RunManifest, TradingInstrumentSpec};
use qx_guanxing::Bar;
use qx_xingban::{MarginRule, NextBarOpenFillModel, PortfolioBarMarket, ZeroLatency};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::path::Path;

#[derive(Serialize, serde::Deserialize)]
pub(crate) struct DataIdentity {
    pub(crate) instrument: String,
    pub(crate) path: String,
    pub(crate) content_hash: String,
    pub(crate) row_count: usize,
    pub(crate) start_ts_ms: u64,
    pub(crate) end_ts_ms: u64,
    #[serde(skip)]
    pub(crate) quality_fingerprint: Option<String>,
}

pub(super) struct PreparedMarket {
    pub(super) instrument: InstrumentId,
    pub(super) bars: Vec<Bar>,
    pub(super) priority: u32,
    pub(super) contract: TradingInstrumentSpec,
    pub(super) fee: Option<MakerTakerFeeModel>,
    pub(super) margin: Option<Box<dyn MarginRule>>,
    pub(super) seed: u64,
    pub(super) identity: DataIdentity,
    pub(super) dataset_quality: Option<serde_json::Value>,
}

pub(super) fn take_runtime_market(
    market: &mut PreparedMarket,
) -> Result<PortfolioBarMarket<'_>, AppError> {
    Ok(PortfolioBarMarket {
        instrument: market.instrument.clone(),
        priority: market.priority,
        bars: market.bars.as_slice(),
        spec: market.contract.clone(),
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(market.fee.take().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "market fee was not prepared",
            )
        })?),
        latency: Box::new(ZeroLatency),
        margin: market.margin.take().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "market margin was not prepared",
            )
        })?,
        seed: market.seed,
    })
}

pub(super) fn dataset_manifest_fingerprint(quality: &serde_json::Value) -> Result<&str, AppError> {
    quality
        .get("manifest_fingerprint")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "dataset quality verifier omitted manifest_fingerprint",
            )
        })
}

pub(super) struct MultiMarketManifestInput<'a> {
    pub(super) spec: &'a MultiMarketBacktestSpec,
    pub(super) context: &'a RunContext,
    pub(super) data_fingerprint: &'a str,
    pub(super) input_components: BTreeMap<String, String>,
    pub(super) clock_start: u64,
    pub(super) clock_end: u64,
    pub(super) spec_fingerprint: &'a str,
    pub(super) execution_window: Option<MultiMarketExecutionWindow>,
    pub(super) experiment_parameters: Option<&'a BTreeMap<String, i128>>,
    pub(super) result_hash: &'a str,
    pub(super) event_hash: &'a str,
    pub(super) strategy_identities: &'a [crate::StrategyArtifactIdentity],
    pub(super) data_identities: &'a [DataIdentity],
}

pub(super) fn build_run_manifest(input: MultiMarketManifestInput<'_>) -> Result<String, AppError> {
    let MultiMarketManifestInput {
        spec,
        context,
        data_fingerprint,
        input_components,
        clock_start,
        clock_end,
        spec_fingerprint,
        execution_window,
        experiment_parameters,
        result_hash,
        event_hash,
        strategy_identities,
        data_identities,
    } = input;
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.bars_path.clear();
        market.dataset_manifest_path = None;
    }
    let identity_json = match (execution_window, experiment_parameters) {
        (Some(window), Some(parameters)) => {
            serde_json::to_vec(&(identity_spec, window, parameters))
        }
        (Some(window), None) => serde_json::to_vec(&(identity_spec, window)),
        (None, Some(parameters)) => serde_json::to_vec(&(identity_spec, parameters)),
        (None, None) => serde_json::to_vec(&identity_spec),
    }
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let canonical_spec =
        super::run_multi_market_backtest::canonical_spec::build_multi_market_canonical_run_spec(
            spec,
            strategy_identities,
            data_identities,
            context.code_commit(),
            &runtime_version,
            clock_start,
            clock_end,
            execution_window,
            experiment_parameters,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("构建多市场 CanonicalRunSpec 失败: {error}"),
            )
        })?;
    let manifest = RunManifest {
        run_id: spec.run_id.clone(),
        code_commit: context.code_commit().to_string(),
        config_hash: qx_strategy::sha256_hex(&identity_json),
        data_fingerprint: data_fingerprint.to_string(),
        input_components,
        clock_start,
        clock_end,
        global_seed: spec.seed,
        determinism_mode: true,
        result_hash: result_hash.to_string(),
        strategy_version: format!("multi-market-strategy-set:{spec_fingerprint}"),
        instrument_spec_version: "multi-market-spec-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(
            b"PortfolioBarEngine/NextBarOpenFillModel/ZeroLatency/v1",
        ),
        input_event_hash: data_fingerprint.to_string(),
        output_event_hash: event_hash.to_string(),
        runtime_version,
        canonical_run_spec_json: Some(canonical_spec.canonical_json().map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("多市场 CanonicalRunSpec 规范化失败: {error}"),
            )
        })?),
        canonical_run_spec_fingerprint: Some(canonical_spec.fingerprint().map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("多市场 CanonicalRunSpec 指纹失败: {error}"),
            )
        })?),
    };
    manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场 RunManifest 构建失败: {error}"),
        )
    })
}

pub(super) fn verify_dataset_quality(
    manifest_path: &str,
    frame_path: &str,
    frame: &qx_datastruct::BarFrame,
    frame_fingerprint: &str,
) -> Result<serde_json::Value, AppError> {
    let path = Path::new(manifest_path);
    let manifest_bytes =
        std::fs::read(path).map_err(|error| AppError::from_io("读取多市场数据质量清单", &error))?;
    let frame_bytes = std::fs::read(frame_path)
        .map_err(|error| AppError::from_io("读取多市场 BarFrame 内容摘要", &error))?;
    verify_dataset_quality_bytes(
        &manifest_bytes,
        &path.display().to_string(),
        &frame_bytes,
        frame_path,
        frame,
        frame_fingerprint,
    )
}

fn verify_dataset_quality_bytes(
    manifest_bytes: &[u8],
    manifest_label: &str,
    frame_bytes: &[u8],
    frame_label: &str,
    frame: &qx_datastruct::BarFrame,
    frame_fingerprint: &str,
) -> Result<serde_json::Value, AppError> {
    let payload = std::str::from_utf8(manifest_bytes).map_err(|error| {
        invalid(format!(
            "多市场数据质量清单不是 UTF-8 {manifest_label}: {error}"
        ))
    })?;
    let manifest = qx_data::DatasetManifestV2::from_json(payload).map_err(invalid)?;
    let frame_sha256 = qx_data::sha256_hex_bytes(&frame_bytes);
    let row_count = u64::try_from(frame.ts.len()).map_err(|_| invalid("BarFrame 行数超出 u64"))?;
    let first_ts = frame
        .ts
        .first()
        .copied()
        .ok_or_else(|| invalid("BarFrame 不能为空"))?;
    let last_ts = frame
        .ts
        .last()
        .copied()
        .ok_or_else(|| invalid("BarFrame 不能为空"))?;
    let actual_instrument = frame.instrument.to_string();
    let declared = &manifest.dataset;
    for (field, actual, expected) in [
        (
            "dataset.fingerprint",
            declared.fingerprint.as_str(),
            frame_fingerprint,
        ),
        (
            "content_hash",
            manifest.content_hash.as_str(),
            frame_sha256.as_str(),
        ),
        (
            "instrument",
            manifest.instrument.as_str(),
            actual_instrument.as_str(),
        ),
        (
            "provider_version",
            manifest.provider_version.as_str(),
            qx_data::BARFRAME_DATASET_VERSION,
        ),
    ] {
        if actual != expected {
            return Err(invalid(format!(
                "多市场数据质量清单 {field} 与输入不一致: 清单={actual} 实际={expected} ({manifest_label}; BarFrame {frame_label})"
            )));
        }
    }
    if manifest.tier != qx_data::DatasetTier::Bar
        || manifest.quality_report.row_count != row_count
        || declared.start_timestamp != first_ts
        || declared.end_timestamp != last_ts
    {
        return Err(invalid(format!(
            "多市场数据质量清单样本数/范围/档位与 {manifest_label} 不一致"
        )));
    }
    let interval_ms = manifest
        .quality_report
        .interval_ms
        .filter(|interval| *interval > 0)
        .ok_or_else(|| {
            invalid(format!(
                "多市场数据质量清单缺少正 interval_ms: {manifest_label}"
            ))
        })?;
    let missing = frame
        .ts
        .windows(2)
        .try_fold(0_u64, |total, pair| {
            total.checked_add((pair[1] - pair[0]).div_ceil(interval_ms).saturating_sub(1))
        })
        .ok_or_else(|| invalid("多市场数据缺口数复算溢出"))?;
    if manifest.quality_report.missing_intervals != missing
        || manifest.quality_report.duplicate_rows != 0
        || manifest.quality_report.out_of_order_rows != 0
    {
        return Err(invalid(format!(
            "多市场质量报告与 BarFrame 实况不一致: missing_intervals 清单={} 重算={} duplicate_rows={} out_of_order_rows={} ({})",
            manifest.quality_report.missing_intervals,
            missing,
            manifest.quality_report.duplicate_rows,
            manifest.quality_report.out_of_order_rows,
            manifest_label
        )));
    }
    let manifest_fingerprint = manifest.fingerprint().map_err(invalid)?;
    Ok(serde_json::json!({
        "manifest_path": manifest_label,
        "manifest_sha256": qx_data::sha256_hex_bytes(&manifest_bytes),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": manifest.content_hash,
        "quality_report": manifest.quality_report,
        "manifest": manifest,
    }))
}

/// Verify a persisted multi-leg report's quality evidence with the same application contract
/// used by its producer. The caller is responsible for resolving both paths inside its grant.
pub fn verify_multi_leg_report_quality_evidence_v1(
    manifest_path: &str,
    frame_path: &str,
    frame: &qx_datastruct::BarFrame,
    frame_fingerprint: &str,
    declared_manifest_path: &str,
    evidence: &serde_json::Value,
) -> Result<String, AppError> {
    let manifest_bytes = std::fs::read(manifest_path)
        .map_err(|error| AppError::from_io("读取多腿数据质量清单", &error))?;
    let frame_bytes = std::fs::read(frame_path)
        .map_err(|error| AppError::from_io("读取多腿 BarFrame 内容摘要", &error))?;
    verify_multi_leg_report_quality_evidence_bytes_v1(
        &manifest_bytes,
        manifest_path,
        &frame_bytes,
        frame_path,
        frame,
        frame_fingerprint,
        declared_manifest_path,
        evidence,
    )
}

/// Verify multi-leg quality evidence when the storage adapter already resolved the source bytes.
pub fn verify_multi_leg_report_quality_evidence_bytes_v1(
    manifest_bytes: &[u8],
    manifest_label: &str,
    frame_bytes: &[u8],
    frame_label: &str,
    frame: &qx_datastruct::BarFrame,
    frame_fingerprint: &str,
    declared_manifest_path: &str,
    evidence: &serde_json::Value,
) -> Result<String, AppError> {
    if evidence
        .get("manifest_path")
        .and_then(serde_json::Value::as_str)
        != Some(declared_manifest_path)
    {
        return Err(invalid("多腿质量证据中的 manifest_path 与 spec 不一致"));
    }
    let verified = verify_dataset_quality_bytes(
        manifest_bytes,
        manifest_label,
        frame_bytes,
        frame_label,
        frame,
        frame_fingerprint,
    )?;
    for field in [
        "manifest_sha256",
        "manifest_fingerprint",
        "content_hash",
        "quality_report",
    ] {
        if evidence.get(field) != verified.get(field) {
            return Err(invalid(format!(
                "多腿质量证据的 {field} 与已复核 BarFrame/质量清单不一致"
            )));
        }
    }
    verified["manifest_fingerprint"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("已复核的多腿质量清单缺少 manifest_fingerprint"))
}

pub(super) fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

pub(super) fn existing_identical(
    path: &std::path::Path,
    payload: &str,
    label: &str,
) -> Result<bool, AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == payload => Ok(true),
        Ok(_) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场{label}产物已存在且内容不同: {}", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::from_io(&format!("读取多市场{label}产物"), &error)),
    }
}

/// Publish an EventLog as one JSON object per line without building a second full-size buffer.
/// Existing artifacts are compared line by line to preserve deterministic rerun semantics.
pub(super) fn ensure_event_trace_jsonl(
    path: &Path,
    events: &[qx_core::Event],
    label: &str,
) -> Result<(), AppError> {
    match std::fs::File::open(path) {
        Ok(file) => {
            if event_trace_matches(BufReader::new(file), path, events)? {
                return Ok(());
            }
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场{label}产物已存在且内容不同: {}", path.display()),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io(&format!("读取多市场{label}产物"), &error)),
    }

    let publish = crate::artifact_store::sink::publish_stream(
        path,
        crate::artifact_store::sink::PublishMode::CreateNew,
        |file| {
            for event in events {
                serde_json::to_writer(&mut *file, event).map_err(|error| {
                    crate::artifact_store::sink::PublishError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("序列化多市场事件失败: {error}"),
                    ))
                })?;
                file.write_all(b"\n")
                    .map_err(crate::artifact_store::sink::PublishError::Io)?;
            }
            Ok(())
        },
    );
    match publish {
        Ok(()) => Ok(()),
        Err(crate::artifact_store::sink::PublishError::Conflict(_)) => {
            let file = std::fs::File::open(path)
                .map_err(|error| AppError::from_io("复核并发发布的多市场事件轨迹", &error))?;
            if event_trace_matches(BufReader::new(file), path, events)? {
                Ok(())
            } else {
                Err(AppError::new(
                    AppErrorCategory::Conflict,
                    format!("多市场{label}产物已存在且内容不同: {}", path.display()),
                ))
            }
        }
        Err(crate::artifact_store::sink::PublishError::Io(error)) => {
            Err(AppError::from_io("发布多市场事件轨迹", &error))
        }
    }
}

fn event_trace_matches(
    mut reader: impl std::io::BufRead,
    path: &Path,
    events: &[qx_core::Event],
) -> Result<bool, AppError> {
    let reference = path.to_string_lossy();
    for event in events {
        let expected = serde_json::to_vec(event).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("序列化多市场事件失败: {error}"),
            )
        })?;
        let Some((actual, _)) = crate::artifact_store::read_bounded_line(&mut reader, &reference)?
        else {
            return Ok(false);
        };
        if actual.len() != expected.len() + 1
            || !actual.as_bytes().starts_with(&expected)
            || actual.as_bytes().last() != Some(&b'\n')
        {
            return Ok(false);
        }
    }
    Ok(crate::artifact_store::read_bounded_line(&mut reader, &reference)?.is_none())
}
