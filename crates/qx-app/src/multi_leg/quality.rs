//! 双腿数据质量身份与报告投影。

/// 基于每条腿的数据集清单与可选 snapshot digest 生成质量身份。
///
/// 只有两侧都提供质量信息时才返回指纹，保持 CLI 产物的既有身份规则。
pub fn multi_leg_dataset_quality_identity(quality: &serde_json::Value) -> Option<String> {
    let manifest_fingerprint = quality
        .get("manifest_fingerprint")
        .and_then(serde_json::Value::as_str)?;
    let snapshot_sha256 = quality
        .get("dataset_snapshot_sha256")
        .and_then(serde_json::Value::as_str);
    Some(
        crate::multi_market_report::dataset_quality_identity_fingerprint(
            manifest_fingerprint,
            snapshot_sha256,
        ),
    )
}

/// 基于两条腿的数据集质量身份生成组合级数据质量指纹。
pub fn multi_leg_quality_fingerprint(
    dataset_quality: [Option<&serde_json::Value>; 2],
) -> Option<String> {
    let [Some(primary), Some(reference)] = dataset_quality else {
        return None;
    };
    Some(qx_strategy::sha256_hex(
        format!(
            "primary:{}|reference:{}",
            multi_leg_dataset_quality_identity(primary).unwrap_or_default(),
            multi_leg_dataset_quality_identity(reference).unwrap_or_default()
        )
        .as_bytes(),
    ))
}

/// 合并两腿数据质量报告；组合报告不隐去任何一侧的缺失与重复统计。
pub fn aggregate_multi_leg_quality_report(
    primary: Option<&serde_json::Value>,
    reference: Option<&serde_json::Value>,
) -> serde_json::Value {
    let reports = [primary, reference]
        .into_iter()
        .flatten()
        .filter_map(|quality| quality.get("quality_report"))
        .collect::<Vec<_>>();
    let sum = |field: &str| {
        reports
            .iter()
            .filter_map(|report| report.get(field).and_then(serde_json::Value::as_u64))
            .sum::<u64>()
    };
    let all_tiers = reports
        .iter()
        .filter_map(|report| {
            report
                .get("usable_tiers")
                .and_then(serde_json::Value::as_array)
        })
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    serde_json::json!({
        "row_count": sum("row_count"),
        "duplicate_rows": sum("duplicate_rows"),
        "out_of_order_rows": sum("out_of_order_rows"),
        "missing_intervals": sum("missing_intervals"),
        "interval_ms": serde_json::Value::Null,
        "timezone": "mixed",
        "corporate_action_coverage": reports.iter().all(|report| {
            report.get("corporate_action_coverage").and_then(serde_json::Value::as_bool) == Some(true)
        }),
        "usable_tiers": all_tiers,
    })
}
