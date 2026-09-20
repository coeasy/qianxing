//! CLI 自校验：确定性重放、配置门禁与 worker 契约的集成测试。
//!
//! 这些用例既覆盖纯函数，也用 `spawn` 启动本二进制跑真实进程边界，
//! 所以必须与 crate 根共用同一套名字（`use super::*`），不能改成 `tests/` 集成测试。

use super::*;
use qx_control::{CommandKind, CommandStatus};
use qx_execution::execute_paper_submit_effect;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[test]
fn paper_market_bridge_filters_symbols_and_accepts_account_wildcard() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let other = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    let worker = WorkerConfig {
        id: "paper-btc".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("paper-main".into()),
        venue_id: Some("paper".into()),
        endpoint: None,
        symbols: vec![instrument.to_string()],
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    };
    assert!(paper_market_worker_matches_instrument(&worker, &instrument));
    assert!(!paper_market_worker_matches_instrument(&worker, &other));

    let mut wildcard = worker.clone();
    wildcard.symbols.clear();
    assert!(paper_market_worker_matches_instrument(&wildcard, &other));

    let mut disabled = wildcard.clone();
    disabled.enabled = false;
    assert!(!paper_market_worker_matches_instrument(
        &disabled,
        &instrument
    ));
    let mut live = wildcard;
    live.venue_id = Some("binance".into());
    assert!(!paper_market_worker_matches_instrument(&live, &instrument));
}

#[test]
fn paper_market_bridge_mirrors_quote_into_account_event_log() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-market-bridge-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let worker = WorkerConfig {
        id: "paper-btc".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("paper-main".into()),
        venue_id: Some("paper".into()),
        endpoint: None,
        symbols: vec![instrument.to_string()],
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    };
    let pipeline = LiveEventPipeline::open_configured(
        root.clone(),
        "paper-paper-main-paper-events",
        "USDT",
        None,
    )
    .unwrap();
    let mut bridge = PaperMarketBridge {
        workers: vec![worker],
        log_name: "paper-paper-main-paper-events".into(),
        pipeline,
    };
    let mirrored = bridge_market_quote_to_paper(
        std::slice::from_mut(&mut bridge),
        PaperMarketQuote {
            source_worker_id: "ccxt-btc",
            instrument: &instrument,
            bid: Price::from_raw(100),
            bid_qty: Quantity::from_i64(2),
            ask: Price::from_raw(101),
            ask_qty: Quantity::from_i64(3),
            event_ts: 1_000,
            receive_ts: 1_001,
            source_seq: 7,
        },
    )
    .unwrap();
    assert_eq!(mirrored, 1);
    assert_eq!(
        bridge.pipeline.latest_quote(&instrument),
        Some((Price::from_raw(100), Price::from_raw(101), 1_000))
    );
    let restored_quote = bridge
        .pipeline
        .latest_quote_with_depth(&instrument)
        .unwrap();
    assert_eq!(restored_quote.bid_qty, Quantity::from_i64(2));
    assert_eq!(restored_quote.ask_qty, Quantity::from_i64(3));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_backtest_rejects_dataset_bundle_bar_mismatch() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-bundle-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let bundle_path = root.join("bundle.json");
    let mut bundle = qx_data::DatasetBundleManifest::new("test", "v1", "fixture");
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "bars".into(),
            format: qx_data::DatasetComponentFormat::Json,
            dataset: qx_data::DatasetManifest {
                dataset_id: "bars".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint: "bundle-bars".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 2,
        })
        .unwrap();
    std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    let expected = qx_data::DatasetManifest {
        dataset_id: "bars".into(),
        version: "v1".into(),
        source: "fixture".into(),
        fingerprint: "input-bars".into(),
        schema_version: 1,
        start_timestamp: 1,
        end_timestamp: 2,
    };
    let error = verify_dataset_bundle_binding(&bundle_path, &expected, 2).unwrap_err();
    assert!(error.contains("fingerprint 不匹配"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_bundle_binds_non_bar_component_content() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-component-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let actions_path = root.join("actions.json");
    let actions = serde_json::json!([{"instrument":"000001.SZSE","action_type":"cash_dividend"}]);
    let actions_bytes = serde_json::to_vec(&actions).unwrap();
    std::fs::write(&actions_path, &actions_bytes).unwrap();
    let mut bundle = qx_data::DatasetBundleManifest::new("test", "v1", "fixture");
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "bars".into(),
            format: qx_data::DatasetComponentFormat::Json,
            dataset: qx_data::DatasetManifest {
                dataset_id: "bars".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint: "bars".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 2,
        })
        .unwrap();
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "corporate_actions".into(),
            format: qx_data::DatasetComponentFormat::Json,
            dataset: qx_data::DatasetManifest {
                dataset_id: "actions".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint: qx_strategy::sha256_hex(&actions_bytes),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 1,
        })
        .unwrap();
    let strategy = StrategyRuntimeConfig {
        ashare_actions_path: Some(actions_path.to_string_lossy().into_owned()),
        ..StrategyRuntimeConfig::default()
    };
    verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").unwrap();
    std::fs::write(&actions_path, br"[]").unwrap();
    assert!(verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_bundle_binds_explicit_generic_component_path() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-generic-component-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let factors_path = root.join("factors.json");
    std::fs::write(
        &factors_path,
        br#"{"schema":1,"rows":[{"beta":2,"alpha":1}]}"#,
    )
    .unwrap();
    let (fingerprint, row_count) =
        dataset_commands::dataset_component_file_fingerprint(&factors_path, "factors").unwrap();
    let mut bundle = qx_data::DatasetBundleManifest::new("test", "v1", "fixture");
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "bars".into(),
            format: qx_data::DatasetComponentFormat::Json,
            dataset: qx_data::DatasetManifest {
                dataset_id: "bars".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint: "bars".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 2,
        })
        .unwrap();
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "factors".into(),
            format: qx_data::DatasetComponentFormat::Json,
            dataset: qx_data::DatasetManifest {
                dataset_id: "factors".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint,
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count,
        })
        .unwrap();
    let strategy = StrategyRuntimeConfig {
        dataset_component_paths: BTreeMap::from([(
            "factors".into(),
            factors_path.to_string_lossy().into_owned(),
        )]),
        ..StrategyRuntimeConfig::default()
    };
    verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").unwrap();
    // JSON 字段顺序变化不应改变通用组件 fingerprint。
    std::fs::write(
        &factors_path,
        br#"{"rows":[{"alpha":1,"beta":2}],"schema":1}"#,
    )
    .unwrap();
    verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").unwrap();
    std::fs::write(
        &factors_path,
        br#"{"rows":[{"alpha":1,"beta":3}],"schema":1}"#,
    )
    .unwrap();
    assert!(verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_bundle_binds_arrow_component_manifest() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-arrow-component-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let arrow_path = root.join("factors.arrow.manifest.json");
    let arrow_manifest = qx_data::ArrowDatasetManifest {
        format: "arrow".into(),
        kind: "factors".into(),
        dataset: qx_data::DatasetManifest {
            dataset_id: "factors".into(),
            version: "arrow-v1".into(),
            source: "fixture".into(),
            fingerprint: "arrow-factors".into(),
            schema_version: 1,
            start_timestamp: 1,
            end_timestamp: 2,
        },
        row_count: 2,
        schema: vec![qx_data::ArrowFieldManifest {
            name: "value_raw".into(),
            format: "decimal128(38,9)".into(),
        }],
    };
    std::fs::write(
        &arrow_path,
        serde_json::to_vec_pretty(&arrow_manifest).unwrap(),
    )
    .unwrap();
    let mut bundle = qx_data::DatasetBundleManifest::new("test", "arrow-v1", "fixture");
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "bars".into(),
            format: qx_data::DatasetComponentFormat::Json,
            dataset: qx_data::DatasetManifest {
                dataset_id: "bars".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint: "bars".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            row_count: 2,
        })
        .unwrap();
    bundle
        .add_component(qx_data::DatasetComponentManifest {
            kind: "factors".into(),
            format: qx_data::DatasetComponentFormat::Arrow,
            dataset: arrow_manifest.dataset.clone(),
            row_count: arrow_manifest.row_count,
        })
        .unwrap();
    let strategy = StrategyRuntimeConfig {
        dataset_component_paths: BTreeMap::from([(
            "factors".into(),
            arrow_path.to_string_lossy().into_owned(),
        )]),
        ..StrategyRuntimeConfig::default()
    };
    verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").unwrap();
    let mut invalid = arrow_manifest;
    invalid.dataset.fingerprint = "changed".into();
    std::fs::write(&arrow_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(verify_dataset_bundle_component_bindings(&bundle, &strategy, "test").is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_artifact_sha256_is_checked_before_spawn() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-strategy-artifact-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("strategy.bin");
    std::fs::write(&path, "artifact").unwrap();
    let mut strategy = StrategyRuntimeConfig {
        external_executable: Some(path.to_string_lossy().into_owned()),
        strategy_artifact_sha256: Some(
            "c7c5c1d70c5dec4416ab6158afd0b223ef40c29b1dc1f97ed9428b94d4cadb1c".into(),
        ),
        ..StrategyRuntimeConfig::default()
    };
    assert!(verify_strategy_artifact(&strategy).is_ok());
    std::fs::write(&path, "tampered").unwrap();
    assert!(verify_strategy_artifact(&strategy).is_err());
    strategy.strategy_artifact_sha256 = Some("not-a-digest".into());
    assert!(verify_strategy_artifact(&strategy).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "nats")]
#[test]
fn worker_metrics_are_atomic_and_aggregated_deterministically() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-worker-metrics-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let first = root.join("worker-metrics").join("a.prom");
    let second = root.join("worker-metrics").join("b.prom");
    write_worker_metrics(
        &second,
        "qx_worker_up{worker=\"b\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"b\"} 2\n",
    )
    .unwrap();
    write_worker_metrics(
        &first,
        "qx_worker_up{worker=\"a\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"a\"} 2\n",
    )
    .unwrap();
    let body = read_worker_metrics(&root.join("worker-metrics"), 2_000, 30_000);
    assert_eq!(
            body,
            "qx_worker_up{worker=\"a\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"a\"} 2\nqx_worker_up{worker=\"b\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"b\"} 2\n"
        );
    assert!(!root.join("worker-metrics").join("a.prom.tmp").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "nats")]
#[test]
fn stale_worker_metrics_are_exposed_as_down() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-worker-metrics-stale-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = root.join("worker-metrics").join("relay.prom");
    write_worker_metrics(
            &path,
            "qx_worker_up{worker=\"relay\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"relay\"} 1\n",
        )
        .unwrap();
    let body = read_worker_metrics(&root.join("worker-metrics"), 40_000, 30_000);
    assert!(body.contains("qx_worker_up{worker=\"relay\"} 0"));
    assert!(body.contains("qx_worker_metrics_stale{worker=\"relay\"} 1"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unhealthy_worker_metrics_block_readiness() {
    let healthy =
            "qx_worker_up{worker=\"relay\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"relay\"} 2\n";
    let down =
            "qx_worker_up{worker=\"relay\"} 0\nqx_worker_heartbeat_timestamp_seconds{worker=\"relay\"} 2\n";
    assert!(!worker_metrics_unhealthy(healthy, 2_000, 30_000));
    assert!(worker_metrics_unhealthy(down, 2_000, 30_000));
    assert!(worker_metrics_unhealthy(healthy, 40_000, 30_000));
}

#[test]
fn production_readiness_requires_private_worker_assets() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-readiness-assets-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let key = root.join("key");
    let secret = root.join("secret");
    let spec = root.join("spec.json");
    std::fs::write(&key, "key").unwrap();
    std::fs::write(&secret, "secret").unwrap();
    std::fs::write(&spec, "{}").unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    for worker in &mut config.workers {
        if matches!(worker.role, WorkerRole::UserStream | WorkerRole::Reconciler) {
            worker.enabled = false;
        }
    }
    let execution = config
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Execution)
        .unwrap();
    execution.credential_env = None;
    execution.credential_files = Some(qx_runtime::CredentialFiles {
        api_key: key.to_string_lossy().into_owned(),
        secret: secret.to_string_lossy().into_owned(),
    });
    execution.instrument_spec_path = Some(spec.to_string_lossy().into_owned());
    let config_path = root.join("runtime.json");
    assert!(production_trading_assets_ready(&config, &config_path));
    std::fs::remove_file(&secret).unwrap();
    assert!(!production_trading_assets_ready(&config, &config_path));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "nats")]
#[test]
fn prometheus_labels_escape_control_characters() {
    assert_eq!(prometheus_label("a\\b\"c\nd\re"), "a\\\\b\\\"c\\nd\\re");
}

#[test]
fn strategy_child_environment_rejects_credentials_and_keeps_runtime_allowlist() {
    let safe = BTreeMap::from([("QX_MODE".to_string(), "paper".to_string())]);
    let child = strategy_child_environment(&safe).unwrap();
    assert_eq!(child.get("QX_MODE"), Some(&"paper".to_string()));
    assert!(!child.contains_key("QX_API_KEY"));
    let secret = BTreeMap::from([("EXCHANGE_SECRET".to_string(), "x".to_string())]);
    assert!(strategy_child_environment(&secret).is_err());
}

#[test]
fn runtime_check_report_is_machine_readable_and_safe() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let report = collect_runtime_check_report(&path).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], true);
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    assert!(!report["health"]["services"].as_array().unwrap().is_empty());
    assert!(report["config_fingerprint"].as_str().unwrap().len() >= 32);
}

#[test]
fn live_check_report_is_structured_and_fail_closed_for_template() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let report = collect_live_check_report(&path).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], false);
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    assert!(!report["checks"].as_array().unwrap().is_empty());
    assert!(!report["failures"].as_array().unwrap().is_empty());
}

#[test]
fn ccxt_worker_rejects_exchange_config_mismatch() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-ccxt-binding-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config_path = root.join("ccxt.json");
    std::fs::write(&config_path, r#"{"exchange_id":"okx"}"#).unwrap();
    let worker = WorkerConfig {
        id: "ccxt-binance".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("binance".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    };
    let error = validate_ccxt_worker_binding(&worker, &config_path).unwrap_err();
    assert!(error.contains("不一致"));
    std::fs::write(
        &config_path,
        r#"{"exchange_id":"binance","credential_env":{"api_key":1,"secret":"QX_SECRET"}}"#,
    )
    .unwrap();
    let error = validate_ccxt_worker_binding(&worker, &config_path).unwrap_err();
    assert!(error.contains("credential_env.api_key"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ccxt_private_worker_reads_credentials_from_endpoint_config() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-ccxt-credentials-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config_path = root.join("runtime.json");
    let ccxt_path = root.join("ccxt.json");
    std::fs::write(
            &ccxt_path,
            r#"{"exchange_id":"okx","credential_env":{"api_key":"QX_TEST_MISSING_KEY","secret":"QX_TEST_MISSING_SECRET"}}"#,
        )
        .unwrap();
    let worker = WorkerConfig {
        id: "ccxt-private".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("okx".into()),
        endpoint: Some("ccxt.json".into()),
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    };
    assert!(!worker_credentials_ready(&config_path, &worker).unwrap());
    assert!(validate_ccxt_worker_binding(&worker, &ccxt_path).is_ok());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_initial_cash_is_idempotent_and_replayed_into_ledger() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-cash-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut pipeline = LiveEventPipeline::open(&root, "paper-events", "USDT").unwrap();
    let worker = WorkerConfig {
        id: "paper-execution".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("paper".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: Some(1_000),
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    };
    seed_paper_initial_cash(&mut pipeline, &worker, 10).unwrap();
    seed_paper_initial_cash(&mut pipeline, &worker, 11).unwrap();
    assert_eq!(pipeline.ledger().cash_for("main", "USDT"), 1_000);
    assert_eq!(pipeline.log().events().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ccxt_cashflow_facts_map_signed_bills_and_stable_external_ids() {
    let value = serde_json::json!({
        "cashflows": [
            {
                "external_id": "fund-1",
                "currency": "usdt",
                "kind": "funding",
                "amount_raw": -120000000_i64,
                "timestamp_ms": 5000
            },
            {
                "external_id": "interest-1",
                "currency": "USDT",
                "kind": "interest",
                "amount_raw": 30000000_i64,
                "timestamp_ms": 5100
            }
        ]
    });
    let facts = ccxt_cashflow_facts(&value, "main", "OKX").unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].0.kind, CashflowKind::Funding);
    assert_eq!(facts[0].0.currency, "USDT");
    assert_eq!(facts[0].0.amount, Money::from_raw(-120000000));
    assert_eq!(facts[1].0.kind, CashflowKind::Interest);
    assert_eq!(facts[1].1, 5100);
}

#[test]
fn ccxt_open_orders_report_unknown_and_unmapped_remote_risk() {
    let instrument = InstrumentId::parse("BTC/USDT.OKX").unwrap();
    let mut terminal = mk_order(7, &instrument, Side::Buy, 1);
    terminal.status = OrderStatus::Filled;
    let local_orders = vec![terminal, mk_order(8, &instrument, Side::Sell, 1)];
    let known_remote_orders = BTreeMap::from([("remote-known".into(), (7, OrderStatus::Filled))]);
    let value = serde_json::json!({
        "orders": [
            {"order_id": "remote-known", "client_order_id": "7", "symbol": "BTC/USDT", "status": "open"},
            {"order_id": "remote-unmapped", "client_order_id": "8", "symbol": "BTC/USDT", "status": "open"},
            {"order_id": "remote-unknown", "client_order_id": "", "symbol": "ETH/USDT", "status": "open"}
        ]
    });
    let issues =
        ccxt_open_order_issues(&value, &local_orders, &known_remote_orders, "okx").unwrap();
    assert_eq!(issues.len(), 3);
    assert_eq!(issues[0]["kind"], "remote_open_local_terminal");
    assert_eq!(issues[1]["kind"], "remote_open_unmapped_local_order");
    assert_eq!(issues[2]["kind"], "unknown_remote_open_order");
}

#[test]
fn ccxt_market_tiers_drive_backtest_margin_rule() {
    let market = serde_json::json!({
        "leverage_tiers": [{
            "max_notional_raw": 100000000000000_i64,
            "initial_margin_bps": 2000,
            "maintenance_margin_bps": 1000,
            "max_leverage": 5
        }]
    });
    let rule = ccxt_margin_rule_from_market(&market);
    assert_eq!(rule.initial_margin(100_000), 20_000);
    assert_eq!(rule.maintain_margin(100_000), 10_000);
}

#[test]
fn supervisor_maps_paper_topology_and_rejects_unknown_venue_without_opt_in() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let config = read_runtime_config(&template).unwrap();
    let launches = managed_worker_args(&config, Path::new("runtime.json"), false).unwrap();
    assert_eq!(launches.len(), 4);
    assert!(launches
        .iter()
        .any(|(id, args)| id == "paper-execution" && args[0] == "paper-worker"));

    let mut unsupported = config;
    unsupported
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .unwrap()
        .venue_id = Some("unmanaged-venue".into());
    assert!(managed_worker_args(&unsupported, Path::new("runtime.json"), false).is_err());
    assert!(managed_worker_args(&unsupported, Path::new("runtime.json"), true).is_ok());
}

#[test]
fn supervisor_routes_endpoint_backed_execution_to_public_ccxt() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.ccxt.example.json");
    let config = read_runtime_config(&template).unwrap();
    let launches = managed_worker_args(&config, &template, false).unwrap();
    assert_eq!(launches.len(), 6);
    assert!(launches
        .iter()
        .any(|(id, args)| id == "ccxt-market-main" && args[0] == "ccxt-worker"));
    assert!(launches
        .iter()
        .any(|(id, args)| id == "ccxt-reconciler-main" && args[0] == "ccxt-worker"));
    let (_, args) = launches
        .iter()
        .find(|(id, _)| id == "ccxt-execution-main")
        .unwrap();
    assert_eq!(args[0], "ccxt-worker");
    assert_eq!(args[1], template.to_string_lossy());
    assert_eq!(args[2], "ccxt-execution-main");
    assert_eq!(
        args[3],
        template
            .parent()
            .unwrap()
            .join("qianxing.ccxt.exchange.example.json")
            .to_string_lossy()
    );
}

#[test]
fn ccxt_barframe_snapshot_runs_through_rust_backtest_engine() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-ccxt-backtest-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let frame = BarFrame {
        instrument: InstrumentId::parse("BTC/USDT.OKX").unwrap(),
        source: DataSourceId::new("ccxt:test"),
        ts: (1..=30).collect(),
        open_raw: (1..=30).map(|value| value * 1_000_000_000).collect(),
        high_raw: (1..=30).map(|value| value * 1_000_000_000 + 1).collect(),
        low_raw: (1..=30).map(|value| value * 1_000_000_000 - 1).collect(),
        close_raw: (1..=30).map(|value| value * 1_000_000_000).collect(),
        volume_raw: vec![1_000_000_000; 30],
    };
    let path = root.join("bars.json");
    std::fs::write(&path, frame.to_json()).unwrap();
    run_ccxt_backtest(&path, 5, 20, None, None).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn configured_cost_rules_override_defaults_and_are_validated() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-costs-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("costs.json");
    std::fs::write(&path, r#"{"maker_bp":0,"taker_bp":1000}"#).unwrap();
    assert_eq!(
        load_cost_rules(Some(&path))
            .unwrap()
            .fee_model()
            .descriptor(),
        "MakerTaker@v1[params=maker_bp=0;taker_bp=1000]"
    );
    assert_eq!(
        load_cost_rules(None).unwrap().fee_model().descriptor(),
        "MakerTaker@v1[params=maker_bp=2;taker_bp=5]"
    );
    std::fs::write(&path, r#"{"taker_bp":10001}"#).unwrap();
    assert!(load_cost_rules(Some(&path)).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn live_strategy_rejects_future_or_stale_bar_snapshots() {
    let frame = BarFrame {
        instrument: InstrumentId::parse("BTC/USDT.OKX").unwrap(),
        source: DataSourceId::new("ccxt:test"),
        ts: vec![0, 60_000, 120_000],
        open_raw: vec![1_000_000_000; 3],
        high_raw: vec![1_000_000_001; 3],
        low_raw: vec![999_999_999; 3],
        close_raw: vec![1_000_000_000; 3],
        volume_raw: vec![1_000_000_000; 3],
    };
    assert!(live_frame_is_fresh(&frame, 60_000, true, 120_000, 180_000));
    assert!(!live_frame_is_fresh(&frame, 60_000, true, 120_000, 300_001));
    assert!(!live_frame_is_fresh(&frame, 60_000, true, 120_000, 119_999));
    assert!(live_frame_is_fresh(&frame, 60_000, false, 120_000, 240_000));
}

#[test]
fn ccxt_derivative_snapshots_map_to_signed_positions_and_funding_facts() {
    let response = serde_json::json!({
        "positions": [{
            "symbol": "BTC/USDT:USDT",
            "side": "short",
            "contracts_raw": "2000000000",
            "entry_price_raw": 100000000000_i64,
            "mark_price_raw": 99000000000_i64,
            "unrealized_pnl_raw": 2000000000,
            "initial_margin_raw": 10000000000_i64,
            "maintenance_margin_raw": 5000000000_i64,
            "leverage": 10,
            "margin_mode": "isolated"
        }]
    });
    let positions = ccxt_position_facts(&response, "okx").unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].quantity.raw(), -2_000_000_000);
    assert_eq!(positions[0].leverage, Some(10));

    let funding = serde_json::json!({
        "funding": {
            "symbol": "BTC/USDT:USDT",
            "timestamp_ms": 100,
            "funding_rate_bps": 3,
            "next_funding_timestamp_ms": 200
        }
    });
    let (snapshot, ts) = ccxt_funding_fact(&funding, "okx").unwrap();
    assert_eq!(snapshot.funding_rate_bps, 3);
    assert_eq!(snapshot.next_funding_timestamp_ms, Some(200));
    assert_eq!(ts, 100);
}

#[test]
fn dry_run_submit_order_is_audited_without_credentials_or_network() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-submit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.config_fingerprint = None;
    config.environment = "test".into();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.event_log_segment_events = Some(2);
    config.storage.backend = StorageBackend::Files;
    config.storage.consistency = qx_runtime::StorageConsistency::LocalDurable;
    config.storage.sqlite_path = None;
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let order = mk_order(
        7001,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let mut payload = BTreeMap::new();
    payload.insert("order_json".into(), serde_json::to_string(&order).unwrap());
    let command = ControlCommand {
        command_id: 7001,
        request_id: "submit-7001".into(),
        operator_id: "ops".into(),
        reason: "dry run integration".into(),
        kind: CommandKind::SubmitOrder,
        target: "7001".into(),
        payload,
        permission: Permission::Trading,
        dry_run: true,
    };
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

    run_binance_submit_order(&config_path, "binance-user-main", &command_path).unwrap();
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 2);
    assert_eq!(state.audit()[0].status, CommandStatus::Accepted);
    assert_eq!(state.audit()[1].status, CommandStatus::Executed);
    assert!(state.pending().next().is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_multileg_group_snapshot_is_idempotent_and_reconciles_eventlog_state() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-spread-group-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let first = mk_order(
        9101,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let second = mk_order(
        9102,
        &InstrumentId::parse("ETHUSDT.OKX").unwrap(),
        Side::Sell,
        1,
    );
    let group_id = spread_group_id("arb", 7, 11);
    persist_strategy_spread_group(&root, &group_id, "arb", &[first.clone(), second.clone()])
        .unwrap();
    // 策略重试或 worker 重启不能重复创建不同快照。
    persist_strategy_spread_group(&root, &group_id, "arb", &[first.clone(), second.clone()])
        .unwrap();
    let store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
    let group = store.load(&group_id).unwrap().unwrap();
    assert_eq!(group.status, qx_zhenlu::SpreadOrderGroupStatus::Planned);
    assert_eq!(group.strategy_id, "arb");

    let command = strategy_submit_command("arb", &first, false, Some(&group_id)).unwrap();
    let mut pipeline = LiveEventPipeline::open(&root, "spread-events", "USDT").unwrap();
    pipeline.register_order(first.clone(), 100).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: first.client_id,
                venue_order_id: Some("binance-9101".into()),
            },
            101,
            101,
            1,
            "binance:accepted:9101",
        ))
        .unwrap();
    sync_spread_group_after_order(&root, &pipeline, &command).unwrap();
    let group = store.load(&group_id).unwrap().unwrap();
    assert_eq!(
        group.leg("leg-9101").unwrap().order.status,
        OrderStatus::Accepted
    );
    assert_eq!(group.status, qx_zhenlu::SpreadOrderGroupStatus::Submitting);

    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::ReconcileRequired {
                client_order_id: first.client_id,
            },
            102,
            102,
            2,
            "binance:reconcile:9101",
        ))
        .unwrap();
    sync_spread_group_after_order(&root, &pipeline, &command).unwrap();
    let group = store.load(&group_id).unwrap().unwrap();
    assert_eq!(
        group.leg("leg-9101").unwrap().order.status,
        OrderStatus::Unknown
    );
    assert_eq!(
        group.status,
        qx_zhenlu::SpreadOrderGroupStatus::ReconcileRequired
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 订单组快照的成交必须由 EventLog 的逐笔事实驱动：重复同步不能重复计入，
/// 部分成交必须按真实的每一笔记入（而不是由订单聚合状态反推一笔合成成交）。
#[test]
fn spread_group_sync_books_each_event_log_fill_once() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-spread-sync-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut leg_order = mk_order(9301, &instrument, Side::Buy, 2);
    leg_order.limit = Some(Price::from_i64(101));
    let other = mk_order(
        9302,
        &InstrumentId::parse("ETHUSDT.BINANCE").unwrap(),
        Side::Sell,
        2,
    );
    let group = SpreadOrderGroup::new(
        "sync-fills-1",
        "arb",
        vec![
            SpreadOrderLeg {
                leg_id: "leg-9301".into(),
                venue_id: "BINANCE".into(),
                order: leg_order.clone(),
            },
            SpreadOrderLeg {
                leg_id: "leg-9302".into(),
                venue_id: "BINANCE".into(),
                order: other,
            },
        ],
    )
    .unwrap();
    let mut store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
    store.save(&group).unwrap();

    let mut pipeline = LiveEventPipeline::open(&root, "sync-events", "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "binance".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "sync-cash".into(),
                },
            },
            1,
            1,
            1,
            "sync:cash",
        ))
        .unwrap();
    pipeline.register_order(leg_order.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: leg_order.client_id,
                venue_order_id: Some("binance-9301".into()),
            },
            3,
            3,
            2,
            "sync:accepted:9301",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                4,
                Price::from_i64(100),
                Quantity::from_i64(10),
                Price::from_i64(101),
                Quantity::from_i64(10),
                3,
            ),
            4,
            3,
            "sync:quote:btc",
        ))
        .unwrap();
    let command = strategy_submit_command("arb", &leg_order, false, Some("sync-fills-1")).unwrap();
    let ingest_fill =
        |pipeline: &mut LiveEventPipeline, qty: i64, price: i64, ts: u64, seq: u64| {
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Fill {
                        fill: qx_core::Fill {
                            order_id: 9301,
                            qty: Quantity::from_i64(qty),
                            price: Price::from_i64(price),
                            fee: Money::ZERO,
                            ts,
                            account_id: "main".into(),
                            venue_id: Some("binance".into()),
                            venue_order_id: Some("binance-9301".into()),
                            ..qx_core::Fill::default()
                        },
                    },
                    ts,
                    ts,
                    seq,
                    format!("sync:fill:{seq}"),
                ))
                .unwrap();
        };

    ingest_fill(&mut pipeline, 1, 100, 5, 4);
    sync_spread_group_after_order(&root, &pipeline, &command).unwrap();
    let booked = store.load("sync-fills-1").unwrap().unwrap();
    assert_eq!(booked.leg("leg-9301").unwrap().order.filled.raw(), SCALE);

    ingest_fill(&mut pipeline, 1, 101, 6, 5);
    sync_spread_group_after_order(&root, &pipeline, &command).unwrap();
    let booked = store.load("sync-fills-1").unwrap().unwrap();
    assert_eq!(
        booked.leg("leg-9301").unwrap().order.filled.raw(),
        2 * SCALE
    );
    assert_eq!(
        booked.leg("leg-9301").unwrap().order.status,
        OrderStatus::Filled
    );
    // 没有新事实时再次同步不能把已计入的成交重复登记。
    sync_spread_group_after_order(&root, &pipeline, &command).unwrap();
    let booked = store.load("sync-fills-1").unwrap().unwrap();
    assert_eq!(
        booked.leg("leg-9301").unwrap().order.filled.raw(),
        2 * SCALE
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_hedge_recovery_replays_partial_fill_to_hedged() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-hedge-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut first = mk_order(9201, &instrument, Side::Buy, 2);
    first.limit = Some(Price::from_i64(100));
    let mut second = mk_order(
        9202,
        &InstrumentId::parse("ETHUSDT.BINANCE").unwrap(),
        Side::Sell,
        2,
    );
    second.limit = Some(Price::from_i64(100));
    let mut group = SpreadOrderGroup::new(
        "paper-hedge-1",
        "basis-arbitrage",
        vec![
            SpreadOrderLeg {
                leg_id: "spot".into(),
                venue_id: "BINANCE".into(),
                order: first.clone(),
            },
            SpreadOrderLeg {
                leg_id: "future".into(),
                venue_id: "BINANCE".into(),
                order: second,
            },
        ],
    )
    .unwrap();
    group.begin_submission().unwrap();
    group.record_accepted("spot").unwrap();
    group
        .record_fill(
            "spot",
            &qx_core::Fill {
                order_id: first.client_id,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ..qx_core::Fill::default()
            },
        )
        .unwrap();
    group.record_cancelled("future").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
    let mut group_store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
    group_store.save(&group).unwrap();

    let mut pipeline = LiveEventPipeline::open(&root, "paper-events", "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "paper-hedge-cash".into(),
                },
            },
            1,
            1,
            1,
            "paper:cash",
        ))
        .unwrap();
    pipeline.register_order(first.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: first.client_id,
                venue_order_id: Some("paper-9201".into()),
            },
            3,
            3,
            2,
            "paper:accepted:9201",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                4,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                3,
            ),
            4,
            3,
            "paper:quote:btc",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Fill {
                fill: qx_core::Fill {
                    order_id: first.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    fee: Money::ZERO,
                    ts: 5,
                    account_id: "main".into(),
                    venue_id: Some("paper".into()),
                    venue_order_id: Some("paper-9201".into()),
                    ..qx_core::Fill::default()
                },
            },
            5,
            5,
            4,
            "paper:fill:9201",
        ))
        .unwrap();

    let diagnostics = recover_paper_spread_groups(
        &root,
        &mut pipeline,
        "paper-hedge",
        6,
        None,
        ExecutionCostRules::default().fee_model(),
    )
    .unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("hedge=completed")),
        "{diagnostics:?}"
    );
    let restored = group_store.load("paper-hedge-1").unwrap().unwrap();
    assert_eq!(restored.status, SpreadOrderGroupStatus::Hedged);
    let hedge = pipeline
        .orders()
        .into_iter()
        .find(|order| {
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.rule_version.as_deref())
                == Some("spread-hedge-v1")
        })
        .unwrap();
    assert_eq!(hedge.status, OrderStatus::Filled);
    assert!(hedge.policy.unwrap().reduce_only);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_submit_order_runs_queue_pipeline_ledger_and_ack() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-submit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let order = mk_order(
        8001,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let command = ControlCommand {
        command_id: 8001,
        request_id: "paper-submit-8001".into(),
        operator_id: "paper".into(),
        reason: "paper execution integration".into(),
        kind: CommandKind::SubmitOrder,
        target: "8001".into(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();
    run_paper_submit_order(&config_path, &command_path).unwrap();
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 2);
    let pipeline = LiveEventPipeline::open(&data_dir, "paper-events", "USDT").unwrap();
    let entries = pipeline.ledger().entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.kind == qx_core::LedgerEntryKind::Fee)
            .map(|entry| entry.amount.raw())
            .collect::<Vec<_>>(),
        // 吃单 100 USDT 名义额 × taker 5bp = 0.05 USDT，负号表示现金流出。
        vec![-50_000_000], // 定点原始值
        "Paper 必须按配置费率扣手续费，不能比回测乐观"
    );
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert!(
        qx_storage::ControlCommandQueue::new(data_dir.join("control-queue"))
            .pending()
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_worker_executes_pause_command_through_persistent_queue() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-strategy-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let command = ControlCommand {
        command_id: 9001,
        request_id: "pause-strategy-9001".into(),
        operator_id: "ops".into(),
        reason: "strategy control integration".into(),
        kind: CommandKind::PauseStrategy,
        target: "strategy-paper".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: false,
    };
    let store = JsonStateStore::new(&data_dir);
    let (_, accepted) = store
        .transact_control(|plane| plane.submit_as(command.clone(), Permission::Trading, 1))
        .unwrap();
    assert!(accepted.is_ok());
    ControlCommandQueue::new(data_dir.join("control-queue"))
        .enqueue(command, 1)
        .unwrap();

    run_strategy_worker(&config_path, "strategy-paper", true).unwrap();
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 2);
    assert_eq!(state.audit()[1].status, CommandStatus::Executed);
    assert_eq!(state.audit()[1].result_code, "STRATEGY_PAUSED");
    assert!(ControlCommandQueue::new(data_dir.join("control-queue"))
        .pending()
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_signal_portfolio_risk_emits_idempotent_submit_order() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-strategy-order-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("binance-testnet".into());
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.target_qty = 1;
    let order = build_strategy_order(
        &config,
        "strategy-paper",
        9101,
        10,
        0,
        config.strategy.target_qty,
    )
    .unwrap()
    .unwrap();
    let command = strategy_submit_command("strategy-paper", &order, false, None).unwrap();
    let store = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
    let result = persist_strategy_submit(&store, &queue, &command, 10).unwrap();
    assert_eq!(result, "ORDER_INTENT_ACCEPTED");
    let retry = persist_strategy_submit(&store, &queue, &command, 11).unwrap();
    assert_eq!(retry, "ORDER_INTENT_ALREADY_ACCEPTED");
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 1);
    assert_eq!(queue.pending().unwrap().len(), 1);
    let queued = queue.pending().unwrap().pop().unwrap();
    assert_eq!(queued.command.command_id, 9101);
    assert!(queued.command.payload.contains_key("order_json"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_target_zero_emits_close_order_for_existing_position() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("binance-testnet".into());
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.target_qty = 0;

    let order = build_strategy_order(&config, "strategy-close", 9102, 10, 2, 0)
        .unwrap()
        .expect("target zero must close an existing position");
    assert_eq!(order.side, Side::Sell);
    assert_eq!(order.qty.raw(), 2);
}

#[test]
fn strategy_worker_reads_candidate_factor_bundle_as_context() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-research-context-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.strategy.target_qty = 0;
    config.strategy.target_snapshot_path = None;
    config.strategy.research_snapshot_path =
        Some(root.join("research.json").to_string_lossy().into_owned());
    config.strategy.research_data_fingerprint = Some("bars-1".into());
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut catalog = FactorCatalog::default();
    catalog
        .register_definition(FeatureDefinition {
            name: "momentum".into(),
            version: "v1".into(),
            formula: "close / close[-20] - 1".into(),
            input_fields: vec!["close".into()],
            dependencies: Vec::new(),
            point_in_time: true,
        })
        .unwrap();
    let artifact = FeatureArtifact {
        feature_key: "momentum@v1".into(),
        input_fingerprint: "bars-1".into(),
        as_of: 20,
        coverage_bps: 10_000,
        values: [(instrument.clone(), 100)].into_iter().collect(),
    };
    let report = qx_factor::FactorReport {
        feature_key: "momentum@v1".into(),
        input_fingerprint: "bars-1".into(),
        observation_hash: 1,
        analysis_start: 1,
        analysis_end: 20,
        sample_count: 1,
        coverage_bps: 10_000,
        ic_bps: 1,
        rank_ic_bps: 1,
        turnover_bps: 1,
        transform: None,
        missing_policy: "reject".into(),
        decay_bps: 0,
        capacity_raw: 1,
        exposures: BTreeMap::new(),
    };
    catalog.publish_artifact(artifact.clone()).unwrap();
    catalog.publish_report(report.clone()).unwrap();
    let candidate = catalog
        .bind_candidate(CandidateRequest {
            strategy_version: config.strategy.version.clone(),
            universe_version: "universe-v1".into(),
            parameters: qx_guanxing::ParameterSet::default(),
            data_fingerprint: "bars-1".into(),
            factor_keys: vec!["momentum@v1".into()],
            cost_bps: 1,
            train_start: 1,
            train_end: 10,
            validation_start: 11,
            validation_end: 20,
            intended_exposure: [(instrument.clone(), 2)].into_iter().collect(),
            constraints: BTreeMap::new(),
            execution_model: "event-backtest@v1".into(),
            risk_model: "default-risk@v1".into(),
        })
        .unwrap();
    let research = StrategyResearchSnapshot {
        schema_version: StrategyResearchSnapshot::SCHEMA_VERSION,
        candidate,
        artifacts: vec![artifact],
        reports: vec![report],
        as_of: 20,
    };
    std::fs::write(
        config.strategy.research_snapshot_path.as_ref().unwrap(),
        research.to_json().unwrap(),
    )
    .unwrap();
    assert_eq!(
        strategy_target_qty(&data_dir, &config, &instrument, 21).unwrap(),
        2
    );
    config.strategy.research_data_fingerprint = Some("wrong-fingerprint".into());
    assert!(strategy_target_qty(&data_dir, &config, &instrument, 21).is_err());
    config.strategy.research_data_fingerprint = Some("bars-1".into());
    config.strategy.version = "wrong-version".into();
    assert!(strategy_target_qty(&data_dir, &config, &instrument, 21).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn derivative_strategy_emits_leveraged_short_policy_without_no_short_rule() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.product = Some(TradingProduct::Perpetual);
    config.strategy.margin_mode = Some(MarginMode::Isolated);
    config.strategy.position_mode = Some(PositionMode::OneWay);
    config.strategy.leverage = Some(5);
    config.strategy.allow_short = Some(true);
    config.strategy.target_qty = -1;
    config.validate().unwrap();
    let order = build_strategy_order(&config, "strategy-perp", 9301, 10, 0, -1)
        .unwrap()
        .unwrap();
    let policy = order.policy.unwrap();
    assert_eq!(policy.leverage, 5);
    assert_eq!(policy.margin_mode, MarginMode::Isolated);
    assert_eq!(policy.position_mode, PositionMode::OneWay);
    assert_eq!(policy.position_side, PositionSide::Net);
}

#[test]
fn strategy_backtest_accepts_builtin_runtime_config() {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let runtime = deploy.join("qianxing.runtime.builtin-strategy.example.json");
    let frame = deploy.join("qianxing.bar-frame.example.json");
    run_strategy_backtest(&runtime, &frame, None).unwrap();
}

#[test]
fn builtin_strategy_worker_path_reads_bar_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-builtin-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.builtin-strategy.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    config.storage.data_dir = root.to_string_lossy().into_owned();
    resolve_strategy_runtime_paths(&mut config.strategy, &runtime);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let output =
        invoke_builtin_strategy(&root, &config, &instrument, "builtin-worker-test", u64::MAX)
            .unwrap();
    assert_eq!(output.request_id, "builtin-worker-test");
    assert_eq!(output.strategy_id, "strategy-builtin");
    assert_eq!(output.instrument, "BTCUSDT.BINANCE");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dedicated_paper_spread_recovery_worker_runs_one_scan_without_orders() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-recovery-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = root.join("data").to_string_lossy().into_owned();
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper sample must include disabled spread recovery")
        .enabled = true;
    let runtime = root.join("runtime.json");
    std::fs::write(&runtime, config.to_json().unwrap()).unwrap();
    run_paper_spread_recovery_worker(&runtime, "paper-spread-recovery", true).unwrap();
    assert!(root.join("data").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dedicated_spread_recovery_disables_legacy_execution_scan_only_for_same_account_and_venue() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    let execution = config
        .workers
        .iter()
        .find(|worker| worker.role == WorkerRole::Execution)
        .cloned()
        .unwrap();
    assert!(!dedicated_spread_recovery_configured(&config, &execution));
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper sample must include disabled spread recovery")
        .enabled = true;
    assert!(dedicated_spread_recovery_configured(&config, &execution));
    config.workers.last_mut().unwrap().venue_id = Some("other".into());
    assert!(!dedicated_spread_recovery_configured(&config, &execution));
}

#[test]
fn api_snapshot_is_rebuilt_from_persisted_account_eventlog() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-api-read-model-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = root.to_string_lossy().into_owned();
    config.storage.event_log_segment_events = Some(2);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let order = mk_order(9401, &instrument, Side::Buy, 1);
    let mut pipeline = LiveEventPipeline::open_configured(
        &root,
        "paper-main-paper-events",
        "USDT",
        config.storage.event_log_segment_events,
    )
    .unwrap();
    pipeline.register_order(order, 1).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: 9401,
                venue_order_id: Some("paper-9401".into()),
            },
            2,
            2,
            1,
            "api-read-model:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Fill {
                fill: qx_core::Fill {
                    order_id: 9401,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 3,
                    account_id: "main".into(),
                    ..qx_core::Fill::default()
                },
            },
            3,
            3,
            2,
            "api-read-model:fill",
        ))
        .unwrap();
    let snapshot = load_api_account_snapshot(&config).unwrap().unwrap();
    assert_eq!(snapshot.header.account_id, "main");
    assert_eq!(snapshot.orders.len(), 1);
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.positions[&instrument].quantity_raw, SCALE);
    assert_eq!(snapshot.reconcile.recovery_state, "eventlog-replayed");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_strategy_reads_filled_position_before_emitting_next_order() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-position-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.event_log_segment_events = Some(2);

    let order = build_strategy_order(
        &config,
        "strategy-paper",
        9201,
        10,
        0,
        config.strategy.target_qty,
    )
    .unwrap()
    .unwrap();
    let command = strategy_submit_command("strategy-paper", &order, false, None).unwrap();
    let log_name = "paper-main-paper-events";
    let result = execute_paper_submit_effect_with_storage(
        &command,
        &data_dir,
        log_name,
        10,
        config.storage.event_log_segment_events,
        None,
        None,
    )
    .unwrap();
    assert!(result.starts_with("PAPER_EXECUTED fills=1"));

    let current_qty = strategy_current_qty(&data_dir, &config).unwrap();
    assert_eq!(current_qty, config.strategy.target_qty);
    assert!(build_strategy_order(
        &config,
        "strategy-paper",
        9202,
        11,
        current_qty,
        config.strategy.target_qty,
    )
    .unwrap()
    .is_none());

    let pipeline = LiveEventPipeline::open_configured(
        &data_dir,
        log_name,
        "USDT",
        config.storage.event_log_segment_events,
    )
    .unwrap();
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert_eq!(pipeline.ledger().entries().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// 账户级风控要求 Paper 执行 worker 带交易规格；把示例拓扑里的相对规格路径
/// 指向仓库 `deploy/` 下的副本，这样测试把 runtime.json 写到临时目录也能解析。
fn override_paper_execution_spec_path(config: &mut RuntimeConfig) {
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .expect("paper 拓扑缺少 paper-execution worker")
        .instrument_spec_path = Some(
        repository_deploy_path("qianxing.binance.spot.spec.json")
            .to_string_lossy()
            .into_owned(),
    );
}

#[test]
fn paper_worker_cleans_stale_queue_after_terminal_commit() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    override_paper_execution_spec_path(&mut config);
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let order = build_strategy_order(
        &config,
        "strategy-paper",
        9301,
        10,
        0,
        config.strategy.target_qty,
    )
    .unwrap()
    .unwrap();
    let command = strategy_submit_command("strategy-paper", &order, false, None).unwrap();
    let store = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
    store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
        .unwrap()
        .1
        .unwrap();
    queue.enqueue(command.clone(), 10).unwrap();

    let log_name = "paper-main-paper-events";
    execute_paper_submit_effect(&command, &data_dir, log_name, 10).unwrap();
    store
        .transact(|plane| plane.execute(command.command_id, 11, |_| Ok("PAPER_EXECUTED".into())))
        .unwrap()
        .1
        .unwrap();

    run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();
    assert!(queue.pending().unwrap().is_empty());
    let pipeline = LiveEventPipeline::open(&data_dir, log_name, "USDT").unwrap();
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert_eq!(pipeline.ledger().entries().len(), 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-e2e-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    config.scheduler.jobs_path = workspace_root
        .join("deploy")
        .join("qianxing.scheduler.paper-order-smoke.json")
        .to_string_lossy()
        .into_owned();
    config.strategy.target_snapshot_path = Some(
        workspace_root
            .join("deploy")
            .join("qianxing.strategy-target.paper.json")
            .to_string_lossy()
            .into_owned(),
    );
    config.strategy.target_qty = 0;
    // Paper 执行 worker 必须带交易规格才能通过账户级风控；规格路径按运行时
    // 配置目录解析，而这里把配置写到临时目录，所以指向仓库 deploy/ 的副本。
    override_paper_execution_spec_path(&mut config);
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    run_paper_pipeline_once(&config_path).unwrap();
    run_paper_pipeline_once(&config_path).unwrap();
    let pipeline = LiveEventPipeline::open(&data_dir, "paper-main-paper-events", "USDT").unwrap();
    assert_eq!(pipeline.orders().len(), 1);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    // 初始资金、成交现金、持仓、手续费四条 Ledger 事实：账户级风控要求订单数量
    // 落在规格步长上，成交因此真实计费，不再退化成零成本的名义成交。
    assert_eq!(pipeline.ledger().entries().len(), 4);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reconcile_report_persists_structured_balance_discrepancy() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-reconcile-report-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let discrepancy = RuntimeBalanceDiscrepancy {
        account_id: "main".into(),
        venue_id: "binance".into(),
        asset: "USDT".into(),
        ledger_raw: 0,
        venue_raw: Money::from_i64(10).raw(),
    };
    persist_reconcile_report(ReconcileReportInput {
        pipeline_root: &root,
        worker_id: "reconciler-main",
        account_id: "main",
        venue_id: "binance",
        observed_ts: 42,
        issues: &[],
        additional_order_issues: &[],
        balances_count: 1,
        balance_discrepancies: &[discrepancy],
        position_snapshots_count: 0,
        funding_rate_snapshots_count: 0,
        cashflow_count: 0,
    })
    .unwrap();
    let report: serde_json::Value = JsonStateStore::new(&root)
        .load_json_at("reconcile/reconciler-main.json")
        .unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["balance_discrepancies"][0]["asset"], "USDT");
    assert_eq!(
        report["balance_discrepancies"][0]["venue_raw"],
        10_000_000_000_i64
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn rust_invokes_python_strategy_jsonl_worker_through_versioned_contract() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_target.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-1".into(),
        strategy_id: "strategy-python".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
        bars: None,
    };
    let output = invoke_python_strategy(module.to_string_lossy().as_ref(), &input).unwrap();
    assert_eq!(output.target_qty, 3);
    assert_eq!(output.signal_id, 7);
    assert_eq!(output.confidence, 800);
    assert_eq!(output.priority, 2);
    assert_eq!(output.request_id, input.request_id);
    let artifact_sha256 = qx_strategy::sha256_hex(&std::fs::read(&module).unwrap());
    let mut client = PythonStrategyClient::start_with_transport_config(
        module.to_string_lossy().as_ref(),
        PYTHON_STRATEGY_TIMEOUT_MS,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        Some(&artifact_sha256),
    )
    .unwrap();
    let first = client.request(&input).unwrap();
    let second = client.request(&input).unwrap();
    assert_eq!(first.signal_id, second.signal_id);
    assert_eq!(first.target_qty, 3);
}

#[test]
fn rust_invokes_python_multi_intent_strategy_contract() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_multi.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-multi".into(),
        strategy_id: "strategy-python-multi".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::new(),
        bars: None,
    };
    let output = invoke_python_strategy(module.to_string_lossy().as_ref(), &input).unwrap();
    assert_eq!(output.intents.len(), 2);
    assert_eq!(output.intents[0].side, "buy");
    assert_eq!(output.intents[1].intent_id, 802);
}

#[test]
fn command_registry_is_the_only_cli_dispatch_surface() {
    let mut seen = BTreeSet::new();
    for command in COMMANDS {
        assert!(seen.insert(command.name), "命令名重复: {}", command.name);
        for alias in command.aliases {
            assert!(seen.insert(alias), "命令别名与其他名字冲突: {alias}");
            assert_eq!(
                find_command(alias).map(|found| found.name),
                Some(command.name),
                "别名 {alias} 没有派发到 {name}",
                name = command.name
            );
        }
        // 帮助文本直接取用分组、用法与摘要，空串会打印成残缺行。
        assert!(!command.category.is_empty() && !command.summary.is_empty());
        assert!(
            command.usage.starts_with(command.name),
            "用法必须挂在命令名上: {}",
            command.usage
        );
        for (usage, summary) in command.variants {
            assert!(
                usage.starts_with(command.name) && !summary.is_empty(),
                "{} 的用法变体不成立: {usage}",
                command.name
            );
        }
        if command.machine_readable {
            let documented = command.usage.contains("--json")
                || command
                    .variants
                    .iter()
                    .any(|(usage, _)| usage.contains("--json"));
            assert!(
                documented,
                "{} 声明机器可读，却没有一条用法提到 --json",
                command.name
            );
        }
    }
    // 无参数走演示链路；未注册命令必须显式失败，不能像旧实现那样静默落回演示。
    assert_eq!(find_command("all").map(|command| command.name), Some("all"));
    assert!(find_command("definitely-not-a-command").is_none());
}
