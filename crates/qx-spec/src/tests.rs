use super::*;

fn project_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "project_id": "demo",
        "profile": "crypto-paper",
        "runtime": "deploy/qianxing.runtime.example.json",
        "artifact_root": "runs",
        "datasets": [{"dataset_id": "crypto.btcusdt", "version": "v1"}],
        "strategies": [{"strategy_id": "macd", "language": "python", "source": "strategy.py"}]
    })
    .to_string()
}

fn experiment_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "experiment_id": "exp-1",
        "strategy_ref": "macd",
        "dataset_id": "crypto.btcusdt@v1",
        "seed": 7,
        "initial_capital_raw": 1000000000i64,
        "parameter_space": [{"name": "fast", "values": ["5", "10"]}],
        "split_plan": {"train_bars": 100, "test_bars": 20, "walk_forward": true, "step_bars": 20},
        "baselines": ["buy-and-hold"],
        "cost_model_ref": "costs.json",
        "risk_rule_set": "qx-order-risk-v1",
        "model_version": "none"
    })
    .to_string()
}

fn run_record_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "run_id": "run-1",
        "status": "completed",
        "input_digest": "in",
        "code_identity": "abc123",
        "config_fingerprint": "cfg",
        "started_at_ms": 100,
        "finished_at_ms": 200,
        "artifact_refs": [{"name": "summary", "path": "summary.json", "digest": "d"}],
        "replay_verdict": "verified",
        "capability_level": "L1"
    })
    .to_string()
}

fn capability_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "venue_id": "binance",
        "provider_id": "binance",
        "level": "L2",
        "supported_products": ["spot"],
        "order_types": ["limit", "market"],
        "data_tiers": ["l1"],
        "reconcile": true,
        "code_tested": true,
        "paper_tested": true,
        "sandbox_tested": false,
        "production_approved": false,
        "strategy_api_version": 1,
        "account_snapshot_version": 1
    })
    .to_string()
}

fn evidence_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "claim": {"summary": "dry-run accepted", "capability_level": "L3"},
        "venue_id": "binance",
        "window": {"start_ms": 100, "end_ms": 200},
        "environment": {"host": "ci", "runtime_version": "0.1.0", "network": "testnet"},
        "redacted_config_digest": "cfg-digest",
        "logs": ["logs/run.txt"],
        "fact_count": 3,
        "result_digest": "res-digest",
        "operator": "ci-bot"
    })
    .to_string()
}

fn registry_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "registry_version": 1,
        "entries": [{
            "schema_id": "strategy-api-v1",
            "version": 1,
            "path": "schemas/strategy_api_v1.schema.json",
            "producer": "qx-runtime",
            "consumers": ["qx-python"],
            "compatibility": "backward",
            "golden_fixtures": ["crates/qx-strategy/tests/fixture.json"]
        }]
    })
    .to_string()
}

fn dataset_json() -> String {
    serde_json::json!({
        "manifest_version": 1,
        "dataset": {
            "dataset_id": "crypto.btcusdt",
            "version": "v1",
            "source": "binance",
            "fingerprint": "fp",
            "schema_version": 1,
            "start_timestamp": 1,
            "end_timestamp": 2
        },
        "instrument": "BINANCE.BTCUSDT",
        "timezone": "UTC",
        "tier": "bar",
        "provider_version": "binance-1.0",
        "content_hash": "content",
        "quality_report": {
            "row_count": 10,
            "duplicate_rows": 0,
            "out_of_order_rows": 0,
            "missing_intervals": 0,
            "timezone": "UTC",
            "corporate_action_coverage": false,
            "usable_tiers": ["bar"]
        },
        "source_lineage": {"primary_source": "binance", "backup_sources": []}
    })
    .to_string()
}

#[test]
fn describe_reads_every_foundation_kind() {
    let cases = [
        ("project", project_json()),
        ("dataset", dataset_json()),
        ("experiment", experiment_json()),
        ("run-record", run_record_json()),
        ("capability", capability_json()),
        ("evidence", evidence_json()),
        ("schema-registry", registry_json()),
    ];
    for (kind, payload) in cases {
        let readout = describe(kind, &payload).unwrap_or_else(|error| panic!("{kind}: {error}"));
        assert_eq!(readout.kind, kind);
        assert!(!readout.fingerprint.is_empty());
        assert!(!readout.canonical_json.is_empty());
        assert!(readout.schema_version >= 1);
    }
}

#[test]
fn describe_fingerprint_is_stable_and_rejects_unknown_kind() {
    let first = describe("project", &project_json()).unwrap();
    let second = describe("project", &project_json()).unwrap();
    assert_eq!(first.fingerprint, second.fingerprint);
    let error = describe("nope", &project_json()).unwrap_err();
    assert!(error.contains("未知规格类型"));
}

#[test]
fn foundation_kind_parse_and_all_round_trip() {
    for kind in FoundationKind::ALL {
        assert_eq!(FoundationKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(FoundationKind::parse(""), None);
}

#[test]
fn run_record_refuses_completion_without_verified_replay() {
    let mut value: serde_json::Value = serde_json::from_str(&run_record_json()).unwrap();
    value["replay_verdict"] = serde_json::json!("not_run");
    assert!(RunRecord::from_json(&value.to_string()).is_err());
}

#[test]
fn capability_refuses_sandbox_claim_without_evidence() {
    let mut value: serde_json::Value = serde_json::from_str(&capability_json()).unwrap();
    value["sandbox_tested"] = serde_json::json!(true);
    value["level"] = serde_json::json!("L3");
    assert!(CapabilityManifest::from_json(&value.to_string()).is_err());
}

#[test]
fn evidence_refuses_l4_claim_from_testnet() {
    let mut value: serde_json::Value = serde_json::from_str(&evidence_json()).unwrap();
    value["claim"]["capability_level"] = serde_json::json!("L4");
    assert!(EvidenceBundle::from_json(&value.to_string()).is_err());
}

#[test]
fn project_refuses_unknown_profile_and_duplicate_free_ok() {
    let mut value: serde_json::Value = serde_json::from_str(&project_json()).unwrap();
    value["profile"] = serde_json::json!("not-a-profile");
    assert!(ProjectManifest::from_json(&value.to_string()).is_err());
    let manifest = ProjectManifest::from_json(&project_json()).unwrap();
    assert_eq!(manifest.identity(), "demo");
}

#[test]
fn experiment_refuses_walk_forward_without_step() {
    let mut value: serde_json::Value = serde_json::from_str(&experiment_json()).unwrap();
    value["split_plan"]["step_bars"] = serde_json::json!(0);
    assert!(ExperimentSpec::from_json(&value.to_string()).is_err());
}

#[test]
fn schema_registry_refuses_duplicate_ids() {
    let mut value: serde_json::Value = serde_json::from_str(&registry_json()).unwrap();
    let entry = value["entries"][0].clone();
    value["entries"] = serde_json::json!([entry.clone(), entry]);
    assert!(SchemaRegistry::from_json(&value.to_string()).is_err());
}

#[test]
fn unknown_fields_are_rejected() {
    let mut value: serde_json::Value = serde_json::from_str(&project_json()).unwrap();
    value["extra"] = serde_json::json!(1);
    assert!(ProjectManifest::from_json(&value.to_string()).is_err());
}
