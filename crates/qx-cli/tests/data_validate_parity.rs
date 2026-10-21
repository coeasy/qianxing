use qx_data::DatasetManifestV2;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const DIRTY: &str = r#"{"schema_version":1,"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,2000,4000],"open_raw":[10,20,20,40],"high_raw":[11,21,21,41],"low_raw":[9,19,19,39],"close_raw":[10,20,20,40],"volume_raw":[1,2,2,4]}"#;

fn case_dir() -> PathBuf {
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests live two levels below workspace root")
        .join("target")
        .join(format!(
            "qx-cli-data-validate-{}-{serial}",
            std::process::id()
        ));
    std::fs::create_dir_all(&path).expect("create data-validate fixture directory");
    path
}

fn invoke(frame: &Path, output: &Path, json: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qx-cli"));
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["data-validate"])
        .arg(frame)
        .args(["ashare.000001", "snapshot-1", "1000"])
        .arg(output);
    if json {
        command.arg("--json");
    }
    command.output().expect("run data-validate CLI")
}

fn normalize_case_path(stdout: &str, root: &Path) -> String {
    let root = root.to_string_lossy();
    stdout
        .replace(root.as_ref(), "<case>")
        .replace(&root.replace('\\', "/"), "<case>")
}

fn relative_files(root: &Path) -> Vec<String> {
    let mut files = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[test]
fn data_validate_cli_preserves_manifest_stdout_json_and_conflict_contract() {
    let root = case_dir();
    let frame = root.join("bars.json");
    let manifest_path = root.join("manifest.json");
    std::fs::write(&frame, DIRTY).unwrap();

    let first = invoke(&frame, &manifest_path, false);
    assert_eq!(
        first.status.code(),
        Some(0),
        "first validation must succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("[Data · Validate]"), "{first_stdout}");
    assert!(
        first_stdout.contains("rows=4 duplicate=1 out_of_order=0 missing=1"),
        "{first_stdout}"
    );
    assert!(first_stdout.contains("verdict=已写出"), "{first_stdout}");
    let first_bytes = std::fs::read(&manifest_path).unwrap();
    let manifest =
        DatasetManifestV2::from_json(std::str::from_utf8(&first_bytes).unwrap()).unwrap();
    assert_eq!(manifest.identity(), "ashare.000001@snapshot-1");
    assert_eq!(manifest.quality_report.duplicate_rows, 1);
    assert_eq!(manifest.quality_report.missing_intervals, 1);

    let replay_root = case_dir();
    let replay_frame = replay_root.join("bars.json");
    let replay_manifest = replay_root.join("manifest.json");
    std::fs::write(&replay_frame, DIRTY).unwrap();
    let replay = invoke(&replay_frame, &replay_manifest, false);
    assert_eq!(replay.status.code(), Some(0));
    assert_eq!(
        normalize_case_path(&first_stdout, &root),
        normalize_case_path(&String::from_utf8_lossy(&replay.stdout), &replay_root),
        "fixed input must replay the same supported stdout fields"
    );
    assert_eq!(std::fs::read(&replay_manifest).unwrap(), first_bytes);
    assert_eq!(relative_files(&root), relative_files(&replay_root));
    assert_eq!(
        std::fs::read(&replay_frame).unwrap(),
        std::fs::read(&frame).unwrap()
    );

    let second = invoke(&frame, &manifest_path, true);
    assert_eq!(
        second.status.code(),
        Some(0),
        "identical manifest verification must succeed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8(second.stdout).unwrap();
    let expected_json = String::from_utf8(first_bytes.clone()).unwrap();
    assert!(
        second_stdout.trim_end().ends_with(expected_json.trim_end()),
        "JSON mode must end with the same manifest payload, including the CLI's normal banner: {second_stdout}"
    );
    assert_eq!(std::fs::read(&manifest_path).unwrap(), first_bytes);

    std::fs::write(
        &frame,
        r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,4000],"open_raw":[10,20,40],"high_raw":[11,21,41],"low_raw":[9,19,39],"close_raw":[10,20,40],"volume_raw":[1,2,4]}"#,
    )
    .unwrap();
    let drift = invoke(&frame, &manifest_path, false);
    assert_eq!(drift.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&drift.stderr);
    assert!(stderr.contains("CONFLICT"), "{stderr}");
    assert!(stderr.contains("拒绝覆盖"), "{stderr}");
    assert_eq!(std::fs::read(&manifest_path).unwrap(), first_bytes);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(replay_root);
}
