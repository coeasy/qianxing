//! Portable evidence packages for multi-exchange joint Bar runs.

mod io;
mod model;
mod pack;
mod portable_paths;
mod recompute;
mod validation;
mod verify;

use self::io::{
    read_run_bundle, reserve_staging_directory, verify_digest, write_new, StagingDirectory,
};
use self::model::*;
use self::portable_paths::{
    account_directory_id, bind_portable_market_paths, build_path_map_reader, checked_package_dir,
    checked_package_file, market_component, parent_artifact_sources, verify_portable_market_paths,
};
use self::validation::{validate_joint_spec_identity, verify_child_bundle_identity};
use std::path::Path;

pub(crate) fn pack_multi_account_joint_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    pack::pack_multi_account_joint_run(result_path, spec_path, output_dir)
}

pub(crate) fn verify_multi_account_joint_run(
    bundle_dir: &Path,
    recompute: bool,
) -> Result<(), String> {
    verify::verify_multi_account_joint_run(bundle_dir, recompute)
}

fn joint_context(run_id: &str) -> qx_app::RunContext {
    qx_app::RunContext::new(qx_app::CallerCapability::Research, run_id.to_string())
}
