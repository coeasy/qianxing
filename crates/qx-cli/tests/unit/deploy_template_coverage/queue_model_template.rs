use super::*;

pub(super) fn read(path: &Path) -> Result<String, String> {
    let spec = qx_app::DepthBacktestSpec::from_json(&read_text(path)?)
        .map_err(|error| format!("DepthBacktestSpec JSON 无效: {error}"))?;
    spec.validate()
        .map_err(|error| format!("DepthBacktestSpec 非法: {error}"))?;
    Ok(format!(
        "tier={} queue_position_model={:?} queue_position_bps={}",
        spec.tier, spec.queue_position_model, spec.queue_position_bps
    ))
}
