//! Stable CLI facade for multi-market Bar Evidence assembly.

#[path = "run_evidence_multi_market_pack/assembly.rs"]
mod assembly;

use std::path::{Path, PathBuf};

pub(crate) fn write_multi_market_run_evidence(
    artifact_path: &Path,
    strategy_diagnostics: Option<(&Path, &serde_json::Value)>,
) -> Result<PathBuf, String> {
    assembly::write_multi_market_run_evidence(artifact_path, strategy_diagnostics)
}
