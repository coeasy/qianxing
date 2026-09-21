use super::*;

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
    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
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
