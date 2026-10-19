use super::RunEvidenceDataset;

pub(super) fn validate_dataset(dataset: &RunEvidenceDataset) -> Result<(), String> {
    if let Some(report) = &dataset.quality_report {
        if report.timezone.trim().is_empty()
            || report.usable_tiers.is_empty()
            || report.interval_ms == Some(0)
        {
            return Err(
                "运行证据包 dataset.quality_report 若在场就必须写明 timezone、正 interval_ms（若在场）与非空 usable_tiers".into(),
            );
        }
    }
    if dataset.quality_manifest_path.is_some() != dataset.quality_manifest_sha256.is_some() {
        return Err(
            "运行证据包 dataset.quality_manifest_path 与 quality_manifest_sha256 必须成对出现"
                .into(),
        );
    }
    if dataset
        .quality_manifest_path
        .as_ref()
        .is_some_and(|path| path.trim().is_empty())
        || dataset
            .quality_manifest_sha256
            .as_ref()
            .is_some_and(|digest| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    {
        return Err("运行证据包 dataset 质量清单路径或 SHA-256 无效".into());
    }
    if dataset.quality_manifest_path.is_some()
        && (dataset.manifest_fingerprint.is_none() || dataset.quality_report.is_none())
    {
        return Err(
            "运行证据包 dataset 绑定质量清单时必须同时声明 fingerprint 与 quality_report".into(),
        );
    }
    Ok(())
}
