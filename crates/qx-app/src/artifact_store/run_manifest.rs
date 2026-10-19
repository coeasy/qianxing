//! Read-side verification for the RunManifest reference stored in a backtest summary.

/// Verify that a summary's RunManifest pointer resolves and agrees with its declared hashes.
pub fn verify_declared_run_manifest(summary: &serde_json::Value) -> Result<(), String> {
    let pointer = summary
        .get("run_manifest")
        .and_then(serde_json::Value::as_str)
        .ok_or("回测摘要没写 run_manifest 指针，产物身份没有第二份证据可对")?;
    let payload = std::fs::read_to_string(pointer)
        .map_err(|error| format!("读取回测 RunManifest 失败 {pointer}: {error}"))?;
    let manifest = qx_core::RunManifest::from_json(&payload)
        .map_err(|error| format!("回测 RunManifest 无效 {pointer}: {error}"))?;
    let declared = |name: &str| -> Result<String, String> {
        summary
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(|value| value.to_string())
            .ok_or_else(|| format!("回测摘要的 {name} 缺失或不是字符串，无法与 RunManifest 对账"))
    };
    for (field, manifest_value) in [
        ("result_hash", manifest.result_hash.as_str()),
        ("input_data_hash", manifest.input_event_hash.as_str()),
    ] {
        let summary_value = declared(field)?;
        if summary_value != manifest_value {
            return Err(format!(
                "回测摘要与自己的 RunManifest 不一致: {field} 摘要={summary_value} 清单={manifest_value}（{pointer}）"
            ));
        }
    }
    Ok(())
}
