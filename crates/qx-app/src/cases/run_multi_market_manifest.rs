use crate::{AppError, AppErrorCategory, MultiMarketBacktestSpec, RunContext};
use qx_core::{InstrumentId, MakerTakerFeeModel, RunManifest, TradingInstrumentSpec};
use qx_guanxing::Bar;
use qx_xingban::{MarginRule, NextBarOpenFillModel, PortfolioBarMarket, ZeroLatency};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize)]
pub(super) struct DataIdentity {
    pub(super) instrument: String,
    pub(super) path: String,
    pub(super) content_hash: String,
    pub(super) row_count: usize,
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
    pub(super) result_hash: &'a str,
    pub(super) event_hash: &'a str,
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
        result_hash,
        event_hash,
    } = input;
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.bars_path.clear();
        market.dataset_manifest_path = None;
    }
    let identity_json = serde_json::to_vec(&identity_spec)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
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
        runtime_version: format!("qx-app-{}", env!("CARGO_PKG_VERSION")),
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
    let payload = std::str::from_utf8(&manifest_bytes).map_err(|error| {
        invalid(format!(
            "多市场数据质量清单不是 UTF-8 {}: {error}",
            path.display()
        ))
    })?;
    let manifest = qx_data::DatasetManifestV2::from_json(payload).map_err(invalid)?;
    let frame_bytes = std::fs::read(frame_path)
        .map_err(|error| AppError::from_io("读取多市场 BarFrame 内容摘要", &error))?;
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
                "多市场数据质量清单 {field} 与输入不一致: 清单={actual} 实际={expected} ({})",
                path.display()
            )));
        }
    }
    if manifest.tier != qx_data::DatasetTier::Bar
        || manifest.quality_report.row_count != row_count
        || declared.start_timestamp != first_ts
        || declared.end_timestamp != last_ts
    {
        return Err(invalid(format!(
            "多市场数据质量清单样本数/范围/档位与 {} 不一致",
            path.display()
        )));
    }
    let interval_ms = manifest
        .quality_report
        .interval_ms
        .filter(|interval| *interval > 0)
        .ok_or_else(|| {
            invalid(format!(
                "多市场数据质量清单缺少正 interval_ms: {}",
                path.display()
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
            path.display()
        )));
    }
    let manifest_fingerprint = manifest.fingerprint().map_err(invalid)?;
    Ok(serde_json::json!({
        "manifest_path": path.to_string_lossy(),
        "manifest_sha256": qx_data::sha256_hex_bytes(&manifest_bytes),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": manifest.content_hash,
        "quality_report": manifest.quality_report,
        "manifest": manifest,
    }))
}

pub(super) fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

pub(super) fn max_drawdown_bps(equity: &[(u64, i128)]) -> Result<u32, AppError> {
    let mut peak = 0_i128;
    let mut maximum = 0_u32;
    for (_, value) in equity {
        peak = peak.max(*value);
        if peak <= 0 || *value >= peak {
            continue;
        }
        let drawdown = peak
            .checked_sub(*value)
            .and_then(|loss| loss.checked_mul(10_000))
            .and_then(|loss| loss.checked_div(peak))
            .unwrap_or(10_000)
            .clamp(0, 10_000) as u32;
        maximum = maximum.max(drawdown);
    }
    Ok(maximum)
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
