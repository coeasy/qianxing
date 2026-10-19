use super::{artifact_files, binary, temp_dir};
use std::process::Command;

#[test]
fn config_lock_conflict_keeps_the_existing_artifact_unchanged() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let runtime = root.join("deploy/qianxing.runtime.example.json");
    let cwd = temp_dir("config-lock-conflict", 1);
    let invoke = || {
        Command::new(binary())
            .current_dir(&cwd)
            .arg("config")
            .arg("lock")
            .arg(&runtime)
            .arg("locked.json")
            .output()
            .expect("run config lock CLI process")
    };
    let created = invoke();
    assert_eq!(created.status.code(), Some(0));
    let original = std::fs::read(cwd.join("locked.json")).expect("read locked artifact");
    let rejected = invoke();
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_eq!(rejected.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("目标发布配置已存在"), "{stderr}");
    assert_eq!(
        std::fs::read(cwd.join("locked.json")).expect("reread locked artifact"),
        original,
        "failed overwrite must preserve the existing locked config"
    );
    assert_eq!(artifact_files(&cwd), ["locked.json"]);
    let _ = std::fs::remove_dir_all(cwd);
}
