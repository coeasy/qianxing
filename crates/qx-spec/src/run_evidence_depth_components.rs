use super::RunEvidenceDataset;

pub(super) fn validate(dataset: &RunEvidenceDataset) -> Result<(), String> {
    if dataset.components.len() < 2 {
        return Err("multi-market-depthframe 至少要登记两个 market 组成输入".into());
    }
    let mut roles = std::collections::BTreeSet::new();
    let mut total_rows = 0_u64;
    for component in &dataset.components {
        if !component.role.starts_with("market:")
            || component.role["market:".len()..].trim().is_empty()
            || !roles.insert(component.role.as_str())
        {
            return Err(format!(
                "multi-market-depthframe role 必须唯一且为 market:<instrument>: {}",
                component.role
            ));
        }
        if component.dataset_id.trim().is_empty()
            || component.version.trim().is_empty()
            || component.content_fingerprint.trim().is_empty()
            || component.path.trim().is_empty()
            || component.row_count == 0
        {
            return Err(format!(
                "运行证据包 dataset.components.{} 必须有身份、路径和正快照数",
                component.role
            ));
        }
        total_rows = total_rows
            .checked_add(component.row_count)
            .ok_or("multi-market-depthframe row_count 总和溢出")?;
        let quality_fields = [
            component.manifest_fingerprint.is_some(),
            component.quality_report.is_some(),
            component.quality_manifest_path.is_some(),
            component.quality_manifest_sha256.is_some(),
        ];
        if quality_fields.iter().any(|present| *present)
            && !quality_fields.iter().all(|present| *present)
        {
            return Err(format!(
                "运行证据包 dataset.components.{} 的质量清单字段必须成套出现",
                component.role
            ));
        }
    }
    if dataset.row_count != total_rows {
        return Err("multi-market-depthframe row_count 必须等于各市场快照数总和".into());
    }
    Ok(())
}
