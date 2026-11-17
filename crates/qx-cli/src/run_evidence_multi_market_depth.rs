//! Stable CLI facade for multi-market depth Evidence assembly.

#[path = "run_evidence_multi_market_depth/assembly.rs"]
mod assembly;

use std::path::{Path, PathBuf};

pub(crate) fn write_multi_market_depth_run_evidence(
    path: &Path,
    diagnostics: Option<(&Path, &serde_json::Value)>,
) -> Result<PathBuf, String> {
    assembly::write_multi_market_depth_run_evidence(path, diagnostics)
}
