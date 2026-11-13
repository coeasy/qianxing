use std::path::{Path, PathBuf};

#[path = "run_evidence_multi_market_pack.rs"]
mod run_evidence_multi_market_pack;
pub(crate) use run_evidence_multi_market_pack::write_multi_market_run_evidence;

#[path = "run_evidence_multi_market_depth.rs"]
mod run_evidence_multi_market_depth;

pub(crate) fn write_multi_market_depth_run_evidence(
    artifact_path: &Path,
    diagnostics: Option<(&Path, &serde_json::Value)>,
) -> Result<PathBuf, String> {
    run_evidence_multi_market_depth::write_multi_market_depth_run_evidence(
        artifact_path,
        diagnostics,
    )
}

#[path = "run_evidence_multi_market_depth_recompute.rs"]
mod run_evidence_multi_market_depth_recompute;
pub(crate) use run_evidence_multi_market_depth_recompute::{
    recompute_multi_market_depth_identity, rerun_multi_market_depth,
};

#[path = "run_evidence_multi_market_recompute.rs"]
mod run_evidence_multi_market_recompute;
pub(crate) use run_evidence_multi_market_recompute::{
    recompute_multi_market_barframe_identity, rerun_multi_market_barframe,
};
