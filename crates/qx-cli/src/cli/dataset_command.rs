//! CLI adapters for dataset ingestion, validation, snapshots, bundles, and plan readout.

use super::*;

pub(super) fn dispatch_ingest(
    frame: PathBuf,
    dataset_id: String,
    version: String,
    data_dir: PathBuf,
    snapshot_context: Option<PathBuf>,
    timezone: Option<String>,
) {
    usage_errors::exit_on_failure(
        run_dataset_ingest(
            &frame,
            &dataset_id,
            &version,
            &data_dir,
            snapshot_context.as_deref(),
            timezone.as_deref(),
        ),
        "数据集摄取失败",
    );
}

pub(super) fn dispatch_bundle(
    bundle: PathBuf,
    data_dir: PathBuf,
    bars: Option<PathBuf>,
    snapshot: Option<PathBuf>,
) {
    usage_errors::exit_on_failure(
        run_dataset_bundle(&bundle, &data_dir, bars.as_deref(), snapshot.as_deref()),
        "数据集 Bundle 保存失败",
    );
}

pub(super) fn dispatch_validate(args: crate::data_validate_args::DatasetValidateArgs) {
    usage_errors::exit_on_failure(validate_dataset(&args), "数据集校验失败");
}

pub(super) fn dispatch_validate_depth(args: crate::data_validate_args::DepthDatasetValidateArgs) {
    usage_errors::exit_on_failure(validate_depth_dataset(&args), "深度数据集校验失败");
}

pub(super) fn dispatch_snapshot(args: crate::data_validate_args::DatasetSnapshotArgs) {
    usage_errors::exit_on_failure(publish_dataset_snapshot(&args), "数据集快照发布失败");
}

pub(super) fn dispatch_plan(args: crate::plan_args::PlanArgs) {
    usage_errors::exit_on_failure(plan_readout(&args), "规格读入失败");
}
