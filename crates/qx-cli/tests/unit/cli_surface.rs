use super::*;

/// clap 的位置参数规矩是「必填位置参数之前不得出现可选位置参数」。带
/// `default_value` 的位置参数在 clap 眼里是可选，于是 `paper-worker
/// [runtime.json] <worker-id>` 这一族签名只能在 release 下侥幸工作：debug
/// 构建里 `clap` 的自检直接 panic，开发者照 README 跑 `cargo run -p qx-cli`
/// 拿到的是 clap 内部堆栈而不是我们的退出码 2。`debug_assert()` 在 release
/// 下是空操作，所以这条门禁只在它真正有效的构建里说话。
#[test]
fn clap_command_table_passes_debug_asserts() {
    use clap::CommandFactory;
    crate::cli_args::Cli::command().debug_assert();
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
