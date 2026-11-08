//! Portable, batch-level evidence for independent multi-account Tick/OrderBook runs.

mod io;
mod model;
mod paths;
mod recompute;
mod validation;
mod verify;
mod verify_inputs;

use self::io::{
    read_run_bundle, reserve_staging_directory, verify_digest, write_new, StagingDirectory,
};
use self::model::*;
use self::paths::{
    account_directory_id, checked_missing_package_file, checked_package_file,
    locate_account_bundle, validate_portable_relative, verify_portable_run_paths,
};
pub(super) use self::validation::{
    stable_depth_spec_fingerprint, validate_depth_batch_identity, validate_successful_outcomes,
    verify_bundle_summary_account, verify_member_evidence,
};
use self::verify_inputs::verify_packaged_inputs;

use std::path::Path;

mod pack;

pub(crate) fn pack_multi_account_depth_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    pack::pack_multi_account_depth_run(result_path, spec_path, output_dir)
}

pub(crate) fn verify_multi_account_depth_run(
    bundle_dir: &Path,
    recompute: bool,
) -> Result<(), String> {
    verify::verify_multi_account_depth_run(bundle_dir, recompute)
}
