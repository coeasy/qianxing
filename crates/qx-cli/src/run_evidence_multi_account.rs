//! Portable, batch-level evidence for isolated multi-account Bar runs.

mod io;
mod model;
mod pack;
mod paths;
mod recompute;
mod validation;
mod verify;
mod verify_inputs;

use self::io::{
    read_run_bundle, reserve_staging_directory, verify_digest, write_new, StagingDirectory,
};
use self::paths::{
    account_directory_id, checked_missing_package_file, checked_package_file,
    locate_account_bundle, portable_artifact_path, validate_portable_relative,
    verify_portable_run_paths,
};
use self::validation::{
    validate_successful_outcomes, verify_bundle_summary_account, verify_member_evidence,
};
use self::verify_inputs::verify_packaged_inputs;

use self::model::*;
use std::path::Path;

pub(crate) fn pack_multi_account_run(
    result_path: &Path,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<(), String> {
    pack::pack_multi_account_run(result_path, spec_path, output_dir)
}

pub(crate) fn verify_multi_account_run(bundle_dir: &Path, recompute: bool) -> Result<(), String> {
    verify::verify_multi_account_run(bundle_dir, recompute)
}
