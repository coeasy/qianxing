use super::*;

fn storage_dir_checks(
    runtime_path: &Path,
    configured: &str,
) -> (Vec<serde_json::Value>, Vec<String>, Vec<String>) {
    let mut checks = Vec::new();
    let mut warnings = Vec::new();
    let mut failures = Vec::new();
    check_storage_data_dir(
        runtime_path,
        configured,
        &mut checks,
        &mut warnings,
        &mut failures,
    );
    (checks, warnings, failures)
}

fn named<'a>(checks: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    assert!(
        has_check(checks, name),
        "缺少检查项 {name}，实际 {checks:?}"
    );
    checks
        .iter()
        .find(|check| check["name"] == serde_json::Value::String(name.into()))
        .unwrap()
}

fn has_check(checks: &[serde_json::Value], name: &str) -> bool {
    checks.iter().any(|check| check["name"] == name)
}

#[test]
fn storage_root_report_follows_the_directory_actually_in_use() {
    let root = temp_cli_case_dir("storage-root");
    let runtime_path = root.join("conf").join("runtime.json");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let configured = "qianxing-state";
    assert!(!Path::new(configured).exists());
    // 两处都还没有状态：报告可写运行态实际打开的进程目录口径。
    assert_eq!(
        effective_storage_root(&runtime_path, configured),
        PathBuf::from(configured)
    );
    // 只有 runtime.json 同级存在状态（回测产物口径）：报告它，而不是提示"首次运行时创建"。
    std::fs::create_dir_all(root.join("conf").join(configured)).unwrap();
    assert_eq!(
        effective_storage_root(&runtime_path, configured),
        root.join("conf").join(configured)
    );
    let (checks, warnings, failures) = storage_dir_checks(&runtime_path, configured);
    assert!(failures.is_empty(), "回测落点存在时不能判失败");
    assert!(warnings.is_empty());
    assert_eq!(named(&checks, "storage.data_dir")["status"], "pass");
    let _ = std::fs::remove_dir_all(root);
}

/// 同一份配置换了启动目录：账本落在进程目录口径、回测产物落在配置目录口径，两边互不
/// 可见。doctor 必须并列提示，而不是静默挑一个说"一切正常"。
#[test]
fn doctor_warns_when_the_two_storage_landing_points_diverged() {
    let root = temp_cli_case_dir("storage-root-split");
    let runtime_path = root.join("conf").join("runtime.json");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    // "." 在两个口径下都必然存在，且分别指向进程当前目录与 runtime.json 同级目录。
    let (checks, warnings, failures) = storage_dir_checks(&runtime_path, ".");
    assert!(failures.is_empty());
    assert!(
        has_check(&checks, "storage.data_dir.split"),
        "两个落点都已存在时必须提示分家，实际 {checks:?}"
    );
    assert_eq!(warnings.len(), 1, "分家提示只能出现一次");
    assert!(warnings[0].contains("进程目录口径") && warnings[0].contains("配置目录口径"));
    assert_eq!(named(&checks, "storage.data_dir")["status"], "pass");
    assert_eq!(
        named(&checks, "storage.data_dir")["message"],
        Path::new(".").display().to_string(),
        "报告的是可写运行态真正使用的落点"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn doctor_reports_the_landing_point_where_data_dir_will_be_created() {
    let root = temp_cli_case_dir("storage-root-create");
    let runtime_path = root.join("conf").join("runtime.json");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let configured = root.join("state").to_string_lossy().into_owned();
    let (checks, warnings, failures) = storage_dir_checks(&runtime_path, &configured);
    assert!(failures.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].starts_with("storage.data_dir 尚不存在，将在首次运行时创建"));
    assert!(!has_check(&checks, "storage.data_dir.split"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn doctor_fails_when_no_landing_point_can_be_created() {
    let root = temp_cli_case_dir("storage-root-blocked");
    let runtime_path = root.join("conf").join("runtime.json");
    let configured = root
        .join("absent")
        .join("state")
        .to_string_lossy()
        .into_owned();
    let (checks, warnings, failures) = storage_dir_checks(&runtime_path, &configured);
    assert!(warnings.is_empty());
    assert_eq!(failures.len(), 1);
    assert!(failures[0].starts_with("storage.data_dir 的父目录不存在"));
    assert_eq!(named(&checks, "storage.data_dir")["status"], "fail");
    let _ = std::fs::remove_dir_all(root);
}
