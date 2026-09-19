use super::*;

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
