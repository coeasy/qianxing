use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct FakeTransport(Mutex<Vec<(String, String)>>);

impl RemoteTransport for FakeTransport {
    fn post(&self, path: &str, body: &str) -> Result<RemoteResponse, String> {
        self.0
            .lock()
            .unwrap()
            .push((path.to_owned(), body.to_owned()));
        Ok(RemoteResponse::new(200, r#"{"ok":true}"#))
    }

    fn get(&self, path: &str) -> Result<RemoteResponse, String> {
        self.0
            .lock()
            .unwrap()
            .push((path.to_owned(), String::new()));
        Ok(RemoteResponse::new(200, "[]"))
    }
}

#[test]
fn paper_algo_lifecycle_uses_server_routes_and_json_contract() {
    let transport = FakeTransport::default();
    let client = RemoteApiClient::new(transport);
    client
        .start_paper_algo_execution(&serde_json::json!({"parent_id":"p-1"}))
        .unwrap();
    client.get_paper_algo_execution("p/2").unwrap();
    client.cancel_paper_algo_execution("p-3").unwrap();
    client.list_paper_algo_executions(Some("p-4"), 17).unwrap();
    client.get_live_events(Some(23)).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        [
            "/app/paper/algo-executions/start",
            "/app/paper/algo-executions/get",
            "/app/paper/algo-executions/cancel",
            "/app/paper/algo-executions/list",
            "/events/live?after=23",
        ]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[1].1).unwrap(),
        serde_json::json!({"parent_id":"p/2"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[3].1).unwrap(),
        serde_json::json!({"after_parent_id":"p-4","limit":17})
    );
}

#[test]
fn builtin_strategy_catalog_uses_the_shared_discovery_route() {
    let transport = FakeTransport::default();
    let client = RemoteApiClient::new(transport);
    assert_eq!(
        client.builtin_strategy_catalog().unwrap(),
        serde_json::json!([])
    );
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        &[("/app/strategies".into(), String::new())]
    );
}

#[test]
fn application_capabilities_use_the_shared_runtime_discovery_route() {
    let client = RemoteApiClient::new(FakeTransport::default());
    assert_eq!(
        client.application_capabilities().unwrap(),
        serde_json::json!([])
    );
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.as_slice(),
        &[("/app/capabilities".into(), String::new())]
    );
}

#[test]
fn research_artifacts_uses_the_replay_verified_detail_route() {
    let client = RemoteApiClient::new(FakeTransport::default());
    let outcome = qx_app::BacktestOutcome {
        run_id: "run-1".into(),
        instrument: "BTCUSDT.BINANCE".into(),
        result_hash: "result".into(),
        data_fingerprint: "data".into(),
        fills: 2,
        equity_points: 3,
        return_bps: 100,
        max_drawdown_bps: 25,
        artifacts: qx_app::BacktestArtifacts {
            run_manifest: "manifest.json".into(),
            summary: "summary.json".into(),
            equity: "equity.csv".into(),
            fills: "fills.csv".into(),
            event_trace: Some("events.jsonl".into()),
        },
        account_id: None,
    };
    assert_eq!(
        client.research_artifacts(&outcome).unwrap(),
        serde_json::json!({"ok":true})
    );
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(calls[0].0, "/app/research-artifacts");
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].1).unwrap(),
        serde_json::to_value(outcome).unwrap()
    );
}

#[test]
fn provider_preflight_uses_shared_route_and_canonical_data_request() {
    let client = RemoteApiClient::new(FakeTransport::default());
    let request = crate::DataRequest::new(
        crate::DataQuery {
            kind: crate::DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: std::collections::BTreeSet::from(["BTCUSDT.BINANCE".into()]),
            field_set: std::collections::BTreeSet::from(["close".into()]),
            frequency: "1m".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 10,
            as_of: Some(10),
        },
        crate::DataRequirements::default(),
    );
    assert_eq!(
        client.provider_preflight(&request).unwrap(),
        serde_json::json!({"ok":true})
    );
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(calls[0].0, "/app/capabilities/providers/preflight");
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].1).unwrap(),
        serde_json::from_str::<Value>(&request.to_json().unwrap()).unwrap()
    );
}

#[test]
fn provider_backtest_uses_shared_server_route_and_preserves_spec_json() {
    let client = RemoteApiClient::new(FakeTransport::default());
    let query = crate::DataQuery {
        kind: crate::DataKind::Bar,
        asset_class: "crypto".into(),
        instrument_set: std::collections::BTreeSet::from(["BTCUSDT.BINANCE".into()]),
        field_set: std::collections::BTreeSet::from(["close".into()]),
        frequency: "1m".into(),
        adjustment: "none".into(),
        quality_policy: "strict".into(),
        start: 1,
        end: 10,
        as_of: Some(10),
    };
    let spec = crate::ProviderBacktestSpec {
        data_request: crate::DataRequest::new(query, crate::DataRequirements::default()),
        backtest: crate::BacktestSpec::new(
            crate::BacktestDatasetInput {
                run_id: "provider-run".into(),
                instrument: "BTCUSDT.BINANCE".into(),
                bars_path: "bars.json".into(),
            },
            "USDT",
            100_000_000_000_000,
            20261013,
            "out",
            crate::BuiltinStrategySpec::new(crate::BuiltinStrategyKind::SmaCross, "provider-sma"),
        ),
        dataset_id: "bars".into(),
        dataset_version: "v1".into(),
        interval_ms: 60_000,
        timezone: "UTC".into(),
        manifest_path: None,
    };
    let _ = client.run_provider_backtest_with_manifest(&spec).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(calls[0].0, "/app/provider-backtest");
    let expected = serde_json::to_value(spec).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].1).unwrap(),
        expected
    );
}

#[test]
fn multi_market_backtest_uses_sync_and_start_server_routes() {
    let transport = FakeTransport::default();
    let client = RemoteApiClient::new(transport);
    let spec: qx_app::MultiMarketBacktestSpec = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "run_id": "remote-portfolio",
        "account_id": "shared",
        "settlement_currency": "USDT",
        "initial_cash_raw": 100,
        "seed": 7,
        "output_dir": "runs",
        "markets": []
    }))
    .expect("spec schema deserializes before engine validation");
    client.run_multi_market_backtest(&spec).unwrap();
    client.start_multi_market_backtest(&spec).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        [
            "/app/multi-market-backtest",
            "/app/multi-market-backtest/start"
        ]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].1).unwrap()["run_id"],
        "remote-portfolio"
    );
}

#[test]
fn multi_account_joint_backtest_uses_sync_and_start_server_routes() {
    let client = RemoteApiClient::new(FakeTransport::default());
    let spec: qx_app::MultiAccountJointBacktestSpec = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "run_id": "remote-joint",
        "settlement_currency": "USDT",
        "seed": 7,
        "output_dir": "runs",
        "accounts": []
    }))
    .expect("spec schema deserializes before engine validation");
    client.run_multi_account_joint_backtest(&spec).unwrap();
    client.start_multi_account_joint_backtest(&spec).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        [
            "/app/multi-account-joint-backtest",
            "/app/multi-account-joint-backtest/start"
        ]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].1).unwrap()["run_id"],
        "remote-joint"
    );
}

#[test]
fn multi_market_depth_backtest_uses_sync_and_start_server_routes() {
    let transport = FakeTransport::default();
    let client = RemoteApiClient::new(transport);
    let spec: qx_app::MultiMarketDepthBacktestSpec = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "run_id": "remote-depth-portfolio",
        "account_id": "shared",
        "settlement_currency": "USDT",
        "initial_cash_raw": 100,
        "output_dir": "runs",
        "markets": []
    }))
    .expect("spec schema deserializes before engine validation");
    client.run_multi_market_depth_backtest(&spec).unwrap();
    client.start_multi_market_depth_backtest(&spec).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        [
            "/app/multi-market-depth-backtest",
            "/app/multi-market-depth-backtest/start"
        ]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].1).unwrap()["run_id"],
        "remote-depth-portfolio"
    );
}

#[test]
fn multi_market_experiment_uses_sync_and_start_server_routes() {
    let transport = FakeTransport::default();
    let client = RemoteApiClient::new(transport);
    let spec: qx_app::RunMultiMarketExperimentSpec = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "experiment_id": "remote-grid",
        "target_strategy_id": "slot-0",
        "base": {
            "schema_version": 1,
            "run_id": "unused",
            "account_id": "shared",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100,
            "seed": 7,
            "output_dir": "runs",
            "markets": []
        },
        "parameter_space": [],
        "strategy_kinds": []
    }))
    .expect("spec schema deserializes before engine validation");
    client.run_multi_market_experiment(&spec).unwrap();
    client.start_multi_market_experiment(&spec).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        [
            "/app/run-multi-market-experiment",
            "/app/run-multi-market-experiment/start"
        ]
    );
}

#[test]
fn multi_market_depth_experiment_uses_sync_and_start_server_routes() {
    let client = RemoteApiClient::new(FakeTransport::default());
    let spec: qx_app::RunMultiMarketDepthExperimentSpec =
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "experiment_id": "remote-depth-grid",
            "target_strategy_id": "slot-0",
            "base": {
                "schema_version": 1,
                "run_id": "unused",
                "account_id": "shared",
                "settlement_currency": "USDT",
                "initial_cash_raw": 100,
                "output_dir": "runs",
                "markets": []
            },
            "parameter_space": [],
            "strategy_kinds": []
        }))
        .expect("spec schema deserializes before engine validation");
    client.run_multi_market_depth_experiment(&spec).unwrap();
    client.start_multi_market_depth_experiment(&spec).unwrap();
    let calls = client.transport.0.lock().unwrap();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        [
            "/app/run-multi-market-depth-experiment",
            "/app/run-multi-market-depth-experiment/start"
        ]
    );
}

#[test]
fn event_reads_preserve_optional_cursor_and_server_recovery_errors() {
    struct CursorTransport(Mutex<Vec<String>>);
    impl RemoteTransport for CursorTransport {
        fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
            unreachable!()
        }
        fn get(&self, path: &str) -> Result<RemoteResponse, String> {
            self.0.lock().unwrap().push(path.into());
            let status = if path.contains("after=99") { 409 } else { 200 };
            Ok(RemoteResponse::new(
                status,
                if status == 409 {
                    r#"{"error":"event_cursor_requires_snapshot"}"#
                } else {
                    "[]"
                },
            ))
        }
    }
    let transport = CursorTransport(Mutex::new(Vec::new()));
    let client = RemoteApiClient::new(transport);
    assert_eq!(client.get_live_events(None).unwrap(), serde_json::json!([]));
    assert!(matches!(
        client.get_live_events(Some(99)),
        Err(RemoteApiError::Http { status: 409, .. })
    ));
    assert_eq!(
        *client.transport.0.lock().unwrap(),
        ["/events/live", "/events/live?after=99"]
    );
}

#[test]
fn run_event_reads_are_scoped_and_use_an_exclusive_cursor() {
    struct RunEventsTransport(Mutex<Vec<String>>);
    impl RemoteTransport for RunEventsTransport {
        fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
            unreachable!()
        }
        fn get(&self, path: &str) -> Result<RemoteResponse, String> {
            self.0.lock().unwrap().push(path.into());
            Ok(RemoteResponse::new(200, "[]"))
        }
    }
    let transport = RunEventsTransport(Mutex::new(Vec::new()));
    let client = RemoteApiClient::new(transport);
    assert_eq!(
        client.get_run_events("run-1", 7).unwrap(),
        serde_json::json!([])
    );
    assert_eq!(
        *client.transport.0.lock().unwrap(),
        ["/app/runs/run-1/events?after_seq=7"]
    );
    assert!(client.get_run_events("../secret", 0).is_err());
}

#[test]
fn remote_client_preserves_http_and_transport_errors() {
    struct ErrorTransport;
    impl RemoteTransport for ErrorTransport {
        fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
            Ok(RemoteResponse::new(
                403,
                r#"{"error":"trading_permission_required"}"#,
            ))
        }
    }
    let client = RemoteApiClient::new(ErrorTransport);
    assert!(matches!(
        client.cancel_paper_algo_execution("p-1"),
        Err(RemoteApiError::Http { status: 403, .. })
    ));

    struct OfflineTransport;
    impl RemoteTransport for OfflineTransport {
        fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
            Err("connection refused".into())
        }
    }
    let offline = RemoteApiClient::new(OfflineTransport);
    assert_eq!(
        offline.get_paper_algo_execution("p-1"),
        Err(RemoteApiError::Transport("connection refused".into()))
    );
}

#[test]
fn remote_client_rejects_invalid_success_json() {
    struct InvalidJsonTransport;
    impl RemoteTransport for InvalidJsonTransport {
        fn post(&self, _: &str, _: &str) -> Result<RemoteResponse, String> {
            Ok(RemoteResponse::new(200, "not-json"))
        }
    }
    let client = RemoteApiClient::new(InvalidJsonTransport);
    assert!(matches!(
        client.get_paper_algo_execution("p-1"),
        Err(RemoteApiError::InvalidJson(_))
    ));
}
