//! Config fingerprint locking contracts exercised through the built CLI process.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_case_dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target/qx-cli-test-tmp")
        .join(format!(
            "qianxing-cli-config-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

#[test]
fn config_lock_writes_and_verifies_published_fingerprint() {
    let root = temp_case_dir();
    let input = root.join("runtime.json");
    let output = root.join("runtime.locked.json");
    let init = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["init", input.to_string_lossy().as_ref()])
        .output()
        .expect("启动 qx-cli init 失败");
    assert!(
        init.status.success(),
        "init 必须先生成配置:\n{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let lock = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "config",
            "lock",
            input.to_string_lossy().as_ref(),
            output.to_string_lossy().as_ref(),
        ])
        .output()
        .expect("启动 qx-cli config lock 失败");
    assert!(
        lock.status.success(),
        "config lock 应成功发布配置指纹:\n{}",
        String::from_utf8_lossy(&lock.stderr)
    );

    let locked: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    let fingerprint = locked["config_fingerprint"].as_str().unwrap();
    assert_eq!(fingerprint.len(), 64, "指纹应是 SHA-256 十六进制串");
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let validate = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["config", "validate", output.to_string_lossy().as_ref()])
        .output()
        .expect("启动 qx-cli config validate 失败");
    assert!(
        validate.status.success(),
        "锁定后的配置必须能由 CLI 校验通过:\n{}",
        String::from_utf8_lossy(&validate.stderr)
    );

    let duplicate = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "config",
            "lock",
            input.to_string_lossy().as_ref(),
            output.to_string_lossy().as_ref(),
        ])
        .output()
        .expect("启动 qx-cli config lock 失败");
    assert!(
        !duplicate.status.success(),
        "config lock 不得静默覆盖已存在的锁定产物"
    );
}
