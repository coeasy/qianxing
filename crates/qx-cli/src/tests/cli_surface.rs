use super::*;

const QX_CLI_SURFACE_SOURCES: [&str; 3] =
    ["src/cli.rs", "src/cli_help.rs", "src/config_commands.rs"];

/// 被测 binary 是否不早于被测源码：`cargo test --bin` 只编译测试壳、不会重链
/// `target/debug/qx-cli.exe`，放任过期 binary 会让子进程断言对着旧行为"绿"。
fn assert_binary_fresh(binary: &Path) {
    let built = binary
        .metadata()
        .and_then(|m| m.modified())
        .expect("读取被测 binary 修改时间失败");
    for source in QX_CLI_SURFACE_SOURCES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(source);
        let edited = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| panic!("找不到被测源码 {}", path.display()));
        assert!(
            built >= edited,
            "被测 binary {} 比 {} 旧，请先 cargo build -p qx-cli 再跑本用例",
            binary.display(),
            path.display()
        );
    }
}

fn qx_cli_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_qx-cli") {
        let binary = PathBuf::from(path);
        assert_binary_fresh(&binary);
        return binary;
    }
    let profile_dir = std::env::current_exe()
        .expect("读取测试可执行文件路径失败")
        .parent()
        .and_then(|deps| deps.parent())
        .expect("测试可执行文件应位于 target/<profile>/deps")
        .to_path_buf();
    let binary = profile_dir.join(if cfg!(windows) {
        "qx-cli.exe"
    } else {
        "qx-cli"
    });
    assert!(binary.is_file(), "未找到被测 binary {}", binary.display());
    assert_binary_fresh(&binary);
    binary
}

/// V10 §4.3 第 3 项：`reconcile` 无参曾经打印两份手写向量的"差异"，看起来像真对账能力。
#[test]
fn reconcile_without_sources_is_a_usage_error_not_a_demo() {
    let output = Command::new(qx_cli_binary())
        .arg("reconcile")
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(
        output.status.code(),
        Some(2),
        "reconcile 缺少本地/远端来源必须是用法错误并退出 2:\n{stdout}{stderr}"
    );
    assert!(
        stderr.contains("本地") && stderr.contains("远端"),
        "用法错误必须点名两个必需输入: {stderr}"
    );
    assert!(
        !stdout.contains("差异数="),
        "不得再打印硬编码假订单的演示差异:\n{stdout}"
    );
}

/// V10 §4.3 第 4 项 + §7.2 新不变量：help 宣称的 `run` 入口必须真能被派发。
#[test]
fn run_help_entry_list_equals_dispatched_entries() {
    let output = Command::new(qx_cli_binary())
        .arg("help")
        .output()
        .expect("启动 qx-cli 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("run <"))
        .expect("help 里没有 run 入口行");
    let advertised: BTreeSet<String> = line
        .trim_start()
        .trim_start_matches("run <")
        .split('>')
        .next()
        .expect("run 入口行缺少闭合尖括号")
        .split('|')
        .map(str::to_string)
        .collect();
    let listed: BTreeSet<String> = RUN_ENTRY_POINTS
        .iter()
        .map(|entry| (*entry).to_string())
        .collect();
    assert_eq!(
        advertised, listed,
        "help 的 run 入口表必须与 RUN_ENTRY_POINTS 集合相等"
    );
    let missing = temp_cli_case_dir("run-entry").join("missing-runtime.json");
    for entry in RUN_ENTRY_POINTS {
        let error =
            run_unified_command(&[entry.to_string(), missing.to_string_lossy().into_owned()])
                .expect_err("缺配置文件时每条入口都必须失败，而不是静默成功");
        assert!(
            !error.contains("不支持"),
            "help 宣称 run {entry} 可用，但 run_unified_command 没有该分支: {error}"
        );
    }
    let unknown = run_unified_command(&["definitely-not-an-entry".to_string()]).unwrap_err();
    for entry in RUN_ENTRY_POINTS {
        assert!(
            unknown.contains(entry),
            "未知入口的提示要列出全部可用入口，缺 {entry}: {unknown}"
        );
    }
}

#[test]
fn init_creates_self_contained_project_assets_and_builtin_strategy() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-init-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = root.join("qianxing.runtime.json");
    run_init_with_profile(&runtime, false, Some("macd"), None).unwrap();

    let config = read_runtime_config(&runtime).unwrap();
    assert_eq!(config.strategy.builtin_strategy.as_deref(), Some("macd"));
    assert_eq!(
        config.strategy.bars_snapshot_path.as_deref(),
        Some("qianxing.bar-frame.example.json")
    );
    for asset in [
        "qianxing.scheduler.jobs.example.json",
        "qianxing.scheduler.paper-order-smoke.json",
        "qianxing.bar-frame.example.json",
        "qianxing.dataset-bundle.bar-frame.example.json",
        "qianxing.dataset-component.arrow.example.json",
        "qianxing.binance.spot.spec.json",
        "qianxing.strategy-target.paper.json",
        "README.qianxing.md",
    ] {
        assert!(root.join(asset).is_file(), "missing init asset {asset}");
    }
    let (failures, _) = validate_runtime_references(&runtime, &config);
    assert!(failures.is_empty(), "init references invalid: {failures:?}");
    assert!(run_init_with_profile(&runtime, false, Some("macd"), None).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn init_profiles_rewrite_deploy_paths_and_validate_references() {
    for profile in ["paper", "ccxt", "ashare", "multi-venue", "backtest"] {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-profile-{profile}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let runtime = root.join("qianxing.runtime.json");
        run_init_with_profile(&runtime, false, None, Some(profile)).unwrap();
        let config = read_runtime_config(&runtime).unwrap();
        let (failures, _) = validate_runtime_references(&runtime, &config);
        assert!(
            failures.is_empty(),
            "profile={profile} failures={failures:?}"
        );
        assert!(!std::fs::read_to_string(&runtime)
            .unwrap()
            .contains("deploy/"));
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn doctor_report_is_machine_readable_and_never_claims_network_or_orders() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-doctor-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = root.join("qianxing.runtime.json");
    run_init_with_profile(&runtime, false, Some("macd"), None).unwrap();

    let report = collect_doctor_report(&runtime).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], true);
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| { check["name"] == "config" && check["status"] == "pass" }));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn report_resolves_explicit_summary_and_runtime_latest_paths() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-report-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = root.join("qianxing.runtime.json");
    run_init_with_profile(&runtime, false, Some("macd"), None).unwrap();
    let runs = root.join("data/qianxing/runs");
    std::fs::create_dir_all(&runs).unwrap();
    let summary = runs.join("example.summary.json");
    std::fs::write(&summary, "{\"schema_version\":1}").unwrap();

    assert_eq!(resolve_backtest_summary_path(&summary).unwrap(), summary);
    assert_eq!(resolve_backtest_summary_path(&runtime).unwrap(), summary);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_lock_writes_and_verifies_published_fingerprint() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-config-lock-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let input = root.join("runtime.json");
    let output = root.join("runtime.locked.json");
    run_init_with_profile(&input, false, None, None).unwrap();
    run_config_lock(&input, &output, false).unwrap();
    let locked = read_runtime_config(&output).unwrap();
    assert_eq!(
        locked.config_fingerprint,
        Some(locked.fingerprint().unwrap())
    );
    assert!(locked.verify_fingerprint().is_ok());
    assert!(run_config_lock(&input, &output, false).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn runtime_relative_paths_resolve_from_runtime_config_directory() {
    assert_eq!(
        resolve_runtime_relative_path(
            Path::new("deploy/qianxing.runtime.production.example.json"),
            "qianxing.binance.spot.spec.json",
        ),
        PathBuf::from("deploy/qianxing.binance.spot.spec.json")
    );
    assert_eq!(
        resolve_runtime_relative_path(
            Path::new("deploy/qianxing.runtime.ccxt.example.json"),
            "deploy/qianxing.ccxt.okx.perpetual.spec.json",
        ),
        PathBuf::from("deploy/qianxing.ccxt.okx.perpetual.spec.json")
    );
    assert_eq!(
        resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "C:/absolute/spec.json",),
        PathBuf::from("C:/absolute/spec.json")
    );
    let mut strategy = StrategyRuntimeConfig {
        external_executable: Some("../strategy/bin/strategy.exe".into()),
        python_module: Some("../python/strategy.py".into()),
        target_snapshot_path: Some("../research/target.json".into()),
        research_snapshot_path: Some("../research/bundle.json".into()),
        dataset_bundle_path: Some("../research/dataset.bundle.json".into()),
        ..StrategyRuntimeConfig::default()
    };
    resolve_strategy_runtime_paths(&mut strategy, Path::new("deploy/runtime.json"));
    assert_eq!(
        PathBuf::from(strategy.external_executable.unwrap()),
        resolve_runtime_relative_path(
            Path::new("deploy/runtime.json"),
            "../strategy/bin/strategy.exe"
        )
    );
    assert_eq!(
        PathBuf::from(strategy.python_module.unwrap()),
        resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../python/strategy.py")
    );
    assert_eq!(
        PathBuf::from(strategy.target_snapshot_path.unwrap()),
        resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../research/target.json")
    );
    assert_eq!(
        PathBuf::from(strategy.research_snapshot_path.unwrap()),
        resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../research/bundle.json")
    );
    assert_eq!(
        PathBuf::from(strategy.dataset_bundle_path.unwrap()),
        resolve_runtime_relative_path(
            Path::new("deploy/runtime.json"),
            "../research/dataset.bundle.json"
        )
    );
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-runtime-asset-path-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_dir = root.join("deploy");
    let storage_dir = root.join("data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&storage_dir).unwrap();
    let runtime_path = config_dir.join("runtime.json");
    let configured = "research/bundle.json";
    let config_asset = config_dir.join(configured);
    let storage_asset = storage_dir.join(configured);
    std::fs::create_dir_all(config_asset.parent().unwrap()).unwrap();
    std::fs::create_dir_all(storage_asset.parent().unwrap()).unwrap();
    std::fs::write(&storage_asset, "legacy").unwrap();
    assert_eq!(
        resolve_runtime_asset_path(&runtime_path, &storage_dir, configured),
        storage_asset
    );
    std::fs::write(&config_asset, "current").unwrap();
    assert_eq!(
        resolve_runtime_asset_path(&runtime_path, &storage_dir, configured),
        config_asset
    );
    let _ = std::fs::remove_dir_all(root);
    let mut worker = WorkerConfig {
        id: "execution".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("binance".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: Some(qx_runtime::CredentialFiles {
            api_key: "../secrets/key".into(),
            secret: "../secrets/secret".into(),
        }),
        instrument_spec_path: Some("market.json".into()),
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    };
    resolve_worker_runtime_paths(&mut worker, Path::new("deploy/runtime.json"));
    let files = worker.credential_files.unwrap();
    assert_eq!(
        PathBuf::from(files.api_key),
        resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../secrets/key")
    );
    assert_eq!(
        PathBuf::from(files.secret),
        resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../secrets/secret")
    );
}
