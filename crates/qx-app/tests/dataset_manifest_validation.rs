use qx_app::{
    validate_dataset_manifest, AppErrorCategory, CallerCapability, DatasetManifestValidationSpec,
    RunContext,
};
use qx_data::{DatasetManifestV2, DatasetTier};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const CLEAN: &str = r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,3000],"open_raw":[10,20,30],"high_raw":[11,21,31],"low_raw":[9,19,29],"close_raw":[10,20,30],"volume_raw":[1,2,3]}"#;
const DIRTY: &str = r#"{"schema_version":1,"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,2000,3000,2500,5500],"open_raw":[10,20,20,30,25,55],"high_raw":[11,21,21,31,26,56],"low_raw":[9,19,19,29,24,54],"close_raw":[10,20,20,30,25,55],"volume_raw":[1,2,2,3,2,5]}"#;

fn case_dir(label: &str) -> PathBuf {
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        ".task-tmp/dataset-manifest-{}-{serial}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create dataset validation temp dir");
    path
}

fn spec(frame: &std::path::Path, output: &std::path::Path) -> DatasetManifestValidationSpec {
    DatasetManifestValidationSpec::new(
        frame.to_string_lossy(),
        "ashare.000001",
        "snapshot-1",
        1_000,
        "Asia/Shanghai",
        output.to_string_lossy(),
    )
}

fn run(
    spec: &DatasetManifestValidationSpec,
) -> Result<qx_app::DatasetManifestValidationResult, qx_app::AppError> {
    validate_dataset_manifest(
        spec,
        &RunContext::new(CallerCapability::Research, &spec.dataset_id),
    )
}

#[test]
fn dirty_series_is_diagnosed_and_manifest_keeps_source_bytes_untouched() {
    let root = case_dir("dirty");
    let frame = root.join("bars.json");
    let output = root.join("manifest.json");
    std::fs::write(&frame, DIRTY).unwrap();

    let result = run(&spec(&frame, &output)).unwrap();
    let manifest = &result.manifest;
    assert_eq!(manifest.identity(), "ashare.000001@snapshot-1");
    assert_eq!(manifest.instrument, "000001.SZSE");
    assert_eq!(manifest.tier, DatasetTier::Bar);
    assert_eq!(manifest.quality_report.row_count, 6);
    assert_eq!(manifest.quality_report.duplicate_rows, 1);
    assert_eq!(manifest.quality_report.out_of_order_rows, 1);
    assert_eq!(manifest.quality_report.missing_intervals, 2);
    assert_eq!(manifest.dataset.start_timestamp, 1_000);
    assert_eq!(manifest.dataset.end_timestamp, 5_500);
    assert!(!manifest.quality_report.corporate_action_coverage);
    assert_eq!(manifest.source_lineage.primary_source, "akshare");
    assert!(!result.verified_existing);
    assert_eq!(std::fs::read_to_string(&frame).unwrap(), DIRTY);
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        result.manifest_json
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn clean_series_uses_canonical_fingerprint_and_rechecks_idempotently() {
    let root = case_dir("clean");
    let frame = root.join("bars.json");
    let output = root.join("manifest.json");
    std::fs::write(&frame, CLEAN).unwrap();

    let first = run(&spec(&frame, &output)).unwrap();
    assert!(first.degraded_fingerprint.is_none());
    assert!(!first.verified_existing);
    let first_bytes = std::fs::read(&output).unwrap();
    let parsed = DatasetManifestV2::from_json(std::str::from_utf8(&first_bytes).unwrap()).unwrap();
    assert_eq!(
        parsed.dataset.fingerprint,
        first.manifest.dataset.fingerprint
    );
    assert_eq!(parsed.quality_report.duplicate_rows, 0);
    assert_eq!(parsed.quality_report.out_of_order_rows, 0);
    assert_eq!(parsed.quality_report.missing_intervals, 0);

    let second = run(&spec(&frame, &output)).unwrap();
    assert!(second.verified_existing);
    assert_eq!(std::fs::read(&output).unwrap(), first_bytes);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn changed_content_with_same_identity_conflicts_without_rewriting_manifest() {
    let root = case_dir("drift");
    let frame = root.join("bars.json");
    let output = root.join("manifest.json");
    std::fs::write(&frame, CLEAN).unwrap();
    run(&spec(&frame, &output)).unwrap();
    let original_manifest = std::fs::read(&output).unwrap();

    std::fs::write(
        &frame,
        r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,4000],"open_raw":[10,20,40],"high_raw":[11,21,41],"low_raw":[9,19,39],"close_raw":[10,20,40],"volume_raw":[1,2,4]}"#,
    )
    .unwrap();
    let error = run(&spec(&frame, &output)).unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::Conflict);
    assert!(error.message().contains("拒绝覆盖"));
    assert!(error.message().contains("missing_intervals"));
    assert_eq!(std::fs::read(&output).unwrap(), original_manifest);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn occupied_non_manifest_output_is_never_clobbered() {
    let root = case_dir("occupied");
    let frame = root.join("bars.json");
    let output = root.join("manifest.json");
    std::fs::write(&frame, CLEAN).unwrap();
    std::fs::write(&output, "{\"not\":\"a manifest\"}").unwrap();

    let error = run(&spec(&frame, &output)).unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::Conflict);
    assert!(error.message().contains("拒绝覆盖"));
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "{\"not\":\"a manifest\"}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ragged_columns_and_unsupported_schema_are_rejected() {
    let root = case_dir("invalid");
    let frame = root.join("bars.json");
    let output = root.join("manifest.json");
    std::fs::write(
        &frame,
        r#"{"instrument":"x","ts":[1000,2000],"open_raw":[10],"high_raw":[11,21],"low_raw":[9,19],"close_raw":[10,20],"volume_raw":[1,2]}"#,
    )
    .unwrap();
    let error = run(&spec(&frame, &output)).unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);

    let newer_schema = json!({
        "schema_version": 99,
        "instrument": "x",
        "ts": [1000],
        "open_raw": [10],
        "high_raw": [11],
        "low_raw": [9],
        "close_raw": [10],
        "volume_raw": [1],
    });
    std::fs::write(&frame, newer_schema.to_string()).unwrap();
    let error = run(&spec(&frame, &output)).unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.message().contains("schema_version=99"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dirty_timestamps_use_content_hash_and_report_the_fallback() {
    let root = case_dir("fingerprint-fallback");
    let frame = root.join("bars.json");
    let output = root.join("manifest.json");
    let duplicate = r#"{"instrument":"x","ts":[1000,1000],"open_raw":[10,10],"high_raw":[11,11],"low_raw":[9,9],"close_raw":[10,10],"volume_raw":[1,1]}"#;
    std::fs::write(&frame, duplicate).unwrap();

    let result = run(&spec(&frame, &output)).unwrap();
    assert_eq!(
        result.manifest.dataset.fingerprint,
        qx_strategy::sha256_hex(duplicate.as_bytes())
    );
    assert!(result
        .degraded_fingerprint
        .as_deref()
        .is_some_and(|reason| reason.contains("退化为文件内容 sha256")));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn zero_interval_fails_before_reading_input() {
    let mut invalid = DatasetManifestValidationSpec::new(
        "missing.json",
        "dataset",
        "v1",
        0,
        "UTC",
        "manifest.json",
    );
    invalid.schema_version = 1;
    let error = run(&invalid).unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.message().contains("interval-ms"));
}
