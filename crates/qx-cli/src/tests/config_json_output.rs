use super::*;

/// V11 E4：`config validate` / `fingerprint` / `lock` 曾经声明了 `--json` 却只用它抑制横幅，
///  stdout 里一个 JSON 字符都没有。下面三条用例各自跑真 binary，判的是"机读分支真的能解析、
///  且人读分支不会混进同一份 stdout"——横幅行同样会破坏解析，所以这一条断言顺带守住了它。
fn qx_json_stdout(args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let output = Command::new(qx_cli_binary())
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!(
            "stdout 不是单个 JSON 文档: {error}\n--- stdout ---\n{stdout}--- stderr ---\n{stderr}"
        )
    });
    (output.status.code(), parsed)
}

/// 把一份可用配置改写成"引用了一个不存在的文件"，validate 必须当场判失败。
fn runtime_with_missing_reference(root: &Path) -> PathBuf {
    let runtime = root.join("runtime.json");
    run_init_with_profile(&runtime, false, None, None).unwrap();
    let mut config = read_runtime_config(&runtime).unwrap();
    config.strategy.target_snapshot_path = Some("missing-research-snapshot.json".into());
    std::fs::write(&runtime, config.to_json().unwrap()).unwrap();
    runtime
}

#[test]
fn config_validate_json_carries_the_verdict_and_keeps_exit_code_two() {
    let root = temp_cli_case_dir("config-validate-json");
    let broken = runtime_with_missing_reference(&root);

    let (code, report) =
        qx_json_stdout(&["config", "validate", &broken.to_string_lossy(), "--json"]);
    assert_eq!(
        code,
        Some(2),
        "校验失败必须仍然退出 2，JSON 不能把失败洗成成功"
    );
    assert_eq!(report["command"], "config validate");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], serde_json::json!(false));
    let failures = report["failures"].as_array().expect("failures 必须是数组");
    assert_eq!(
        failures.len(),
        1,
        "缺失引用要逐条进 JSON，而不是只给一个总数: {failures:?}"
    );
    assert!(failures[0]
        .as_str()
        .unwrap_or_default()
        .contains("missing-research-snapshot.json"));

    // 同一份配置的机读与人读必须给同一个结论，且两条通道各说各的话。
    let human = Command::new(qx_cli_binary())
        .args(["config", "validate", &broken.to_string_lossy()])
        .output()
        .expect("启动 qx-cli 失败");
    let human_stdout = String::from_utf8_lossy(&human.stdout).to_string();
    let human_stderr = String::from_utf8_lossy(&human.stderr).to_string();
    assert_eq!(human.status.code(), Some(2));
    assert!(
        !human_stdout.contains("[PASS]") && human_stderr.contains("[FAIL]"),
        "人读分支仍要打印 [FAIL]:\n{human_stdout}{human_stderr}"
    );
    assert!(
        !human_stdout.contains(r#""ok""#),
        "机读字段不得漏进人读输出:\n{human_stdout}"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_validate_json_reports_a_clean_project_as_ok() {
    let root = temp_cli_case_dir("config-validate-clean");
    let runtime = root.join("runtime.json");
    run_init_with_profile(&runtime, false, None, None).unwrap();

    let (code, report) =
        qx_json_stdout(&["config", "validate", &runtime.to_string_lossy(), "--json"]);
    assert_eq!(code, Some(0), "init 生成的项目必须自带可校验的引用");
    assert_eq!(report["ok"], serde_json::json!(true));
    assert_eq!(report["failures"], serde_json::json!([]));
    assert_eq!(report["warnings"], serde_json::json!([]));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_fingerprint_and_lock_json_agree_with_the_locked_file_they_write() {
    let root = temp_cli_case_dir("config-lock-json");
    let runtime = root.join("runtime.json");
    run_init_with_profile(&runtime, false, None, None).unwrap();
    let output = root.join("runtime.locked.json");
    let expected = read_runtime_config(&runtime)
        .unwrap()
        .fingerprint()
        .unwrap();

    let (code, before) = qx_json_stdout(&[
        "config",
        "fingerprint",
        &runtime.to_string_lossy(),
        "--json",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(before["fingerprint"], expected.clone());
    assert_eq!(before["locked"], serde_json::json!(false));
    assert!(
        before["config_fingerprint"].is_null(),
        "未锁定的配置要把这一格留成 null，而不是省掉键: {before}"
    );

    let (code, locked) = qx_json_stdout(&[
        "config",
        "lock",
        &runtime.to_string_lossy(),
        &output.to_string_lossy(),
        "--json",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(locked["command"], "config lock");
    assert_eq!(locked["fingerprint"], expected.clone());
    assert_eq!(locked["locked"], serde_json::json!(true));
    assert_eq!(
        locked["output_path"],
        output.display().to_string(),
        "机读分支要点名真正落盘的那份文件"
    );
    assert!(output.is_file());
    let written = read_runtime_config(&output).unwrap();
    assert_eq!(
        written.config_fingerprint.as_deref(),
        Some(expected.as_str())
    );
    assert!(written.verify_fingerprint().is_ok());

    let (code, after) =
        qx_json_stdout(&["config", "fingerprint", &output.to_string_lossy(), "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(after["locked"], serde_json::json!(true));
    assert_eq!(after["config_fingerprint"], expected.clone());
    assert_eq!(after["fingerprint"], expected);

    let _ = std::fs::remove_dir_all(root);
}
