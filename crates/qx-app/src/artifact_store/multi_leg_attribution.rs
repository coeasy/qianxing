use std::path::{Path, PathBuf};

use super::write_backtest_artifact;

/// Persist the canonical attribution projection for a pair of independently replayed legs.
///
/// The output name is bound to both replay result hashes, so a facade cannot accidentally
/// persist a projection under an unrelated run identity.
pub fn persist_multi_leg_attribution_artifact(
    root: &Path,
    strategy_name: &str,
    primary_result_hash: u64,
    reference_result_hash: u64,
    attribution: &serde_json::Value,
) -> Result<PathBuf, String> {
    let payload = serde_json::to_string_pretty(attribution)
        .map_err(|error| format!("编码多腿归因产物失败: {error}"))?;
    let fingerprint = primary_result_hash
        .wrapping_mul(31)
        .wrapping_add(reference_result_hash);
    let path = root.join("runs").join(format!(
        "multi-leg-{strategy_name}-{fingerprint:016x}.spread-attribution.json"
    ));
    write_backtest_artifact(&path, &payload, "多腿归因产物")?;
    Ok(path)
}
