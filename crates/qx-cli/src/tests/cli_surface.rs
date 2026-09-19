use super::*;

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
