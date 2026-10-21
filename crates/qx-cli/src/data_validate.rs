//! `data-validate` CLI adapter: bind flags to the qx-app manifest-validation use case.

use crate::data_validate_args::{DatasetValidateArgs, DepthDatasetValidateArgs};
use std::fmt::Write as _;

pub(crate) fn validate_dataset(args: &DatasetValidateArgs) -> Result<(), String> {
    let spec = qx_app::DatasetManifestValidationSpec::new(
        args.frame.to_string_lossy(),
        args.dataset_id.clone(),
        args.version.clone(),
        args.interval_ms,
        args.timezone.clone(),
        args.output.to_string_lossy(),
    );
    let context = qx_app::RunContext::new(qx_app::CallerCapability::Research, &args.dataset_id);
    let result =
        qx_app::validate_dataset_manifest(&spec, &context).map_err(|error| error.to_string())?;
    if args.json {
        println!("{}", result.manifest_json);
    } else {
        let report = &result.manifest.quality_report;
        let mut line = String::new();
        let _ = write!(
            line,
            "[Data · Validate] dataset={} instrument={} rows={} duplicate={} out_of_order={} missing={} verdict={} dataset_fingerprint={} output={}",
            result.manifest.identity(),
            result.manifest.instrument,
            report.row_count,
            report.duplicate_rows,
            report.out_of_order_rows,
            report.missing_intervals,
            if result.verified_existing {
                "已复核一致"
            } else {
                "已写出"
            },
            result.manifest.dataset.fingerprint,
            args.output.display()
        );
        println!("{line}");
    }
    if let Some(reason) = &result.degraded_fingerprint {
        eprintln!("[Data · Validate · 警告] {reason}");
    }
    for warning in result.manifest.warnings() {
        eprintln!("[Data · Validate · 警告] {warning}");
    }
    Ok(())
}

pub(crate) fn validate_depth_dataset(args: &DepthDatasetValidateArgs) -> Result<(), String> {
    let tier = match args.tier.as_str() {
        "l1" => qx_data::DatasetTier::L1,
        "l2" => qx_data::DatasetTier::L2,
        other => return Err(format!("深度数据清单 --tier 仅支持 l1 或 l2，实际 {other}")),
    };
    let spec = qx_app::DepthDatasetManifestValidationSpec::new(
        args.frame.to_string_lossy(),
        tier,
        args.interval_ms,
        args.timezone.clone(),
        args.output.to_string_lossy(),
    );
    let context =
        qx_app::RunContext::new(qx_app::CallerCapability::Research, "depth-data-validate");
    let result = qx_app::validate_depth_dataset_manifest(&spec, &context)
        .map_err(|error| error.to_string())?;
    let quality = &result.manifest.quality_report;
    println!(
        "[Data · Validate · Depth] dataset={} tier={} instrument={} rows={} duplicate={} out_of_order={} missing={} interval_ms={} content_hash={} verdict={} output={}",
        result.manifest.identity(),
        result.manifest.tier.as_str(),
        result.manifest.instrument,
        quality.row_count,
        quality.duplicate_rows,
        quality.out_of_order_rows,
        quality.missing_intervals,
        quality.interval_ms.unwrap_or_default(),
        result.manifest.content_hash,
        if result.verified_existing {
            "已复核一致"
        } else {
            "已写出"
        },
        args.output.display(),
    );
    for warning in result.manifest.warnings() {
        eprintln!("[Data · Validate · Depth · 警告] {warning}");
    }
    Ok(())
}
