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

fn run_evidence_json() -> String {
    serde_json::json!({
        "schema_version": 1,
        "run": {
            "run_id": "run-1",
            "code_commit": "abc123",
            "runtime_version": "0.1.0",
            "config_hash": "cfg",
            "data_fingerprint": "fp",
            "input_components": {"bars": "bars-fp"},
            "clock_start": 100,
            "clock_end": 200,
            "global_seed": 7,
            "determinism_mode": true,
            "result_hash": "res",
            "strategy_version": "macd@1",
            "instrument_spec_version": "spot@1",
            "model_fingerprint": "model",
            "input_event_hash": "in",
            "output_event_hash": "out"
        },
        "artifacts": [
            {"name": "run_manifest", "path": "run-1.run.json", "digest": "d1"},
            {"name": "summary", "path": "run-1.summary.json", "digest": "d2"},
            {"name": "equity", "path": "run-1.equity.csv", "digest": "d3"},
            {"name": "fills", "path": "run-1.fills.csv", "digest": "d4"}
        ],
        "dataset": {
            "kind": "barframe",
            "dataset_id": "crypto.btcusdt",
            "version": "v1",
            "content_fingerprint": "fp",
            "composed_fingerprint": "fp",
            "row_count": 10,
            "path": "crypto.bar-frame.json",
            "quality_report": null
        },
        "identity": {
            "strategy_id": "macd",
            "strategy_version": "macd@1",
            "instrument": "BINANCE.BTCUSDT",
            "config_digest": "cfg"
        },
        "assumptions": [
            {"name": "fill_model", "value": "risk-averse-fifo", "source": "backtest spec"}
        ],
        "build": {
            "runtime_version": "0.1.0",
            "code_commit": "abc123",
            "profile": "release",
            "target_triple": "x86_64-pc-windows-msvc"
        },
        "verification": {
            "capability_level": "L1",
            "replay_verdict": "verified",
            "artifact_digests_verified": true,
            "verified_artifact_count": 4
        },
        "unverified": ["实盘 venue 未参与本次运行"],
        "recompute": {
            "steps": ["按 run.data_fingerprint 取回同一份数据集", "重跑同 config_hash 的回测"],
            "inputs": ["run-1.run.json", "run-1.summary.json"],
            "expected_result_hash": "res"
        },
        "operator": "ci-bot"
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
        ("run-evidence", run_evidence_json()),
        ("capability", capability_json()),
        ("evidence", evidence_json()),
        ("schema-registry", registry_json()),
    ];
    // 这张表是手抄的，而用例名说的是「every foundation kind」：不在这里逐一对齐，下一次
    // 往 `FoundationKind::ALL` 里加一类时这份用例会静默漏掉它（名字比内容诚实）。
    let covered: Vec<&str> = cases.iter().map(|(kind, _)| *kind).collect();
    let declared: Vec<&str> = FoundationKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect();
    assert_eq!(
        covered, declared,
        "用例表必须与 FoundationKind::ALL 逐条同名同序：新增一类地基对象要在这里补一份夹具"
    );
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

/// T1-2：版本不匹配必须给出**具名**拒绝——文案里同时点名「读到的版本」与「本构建只认的版本」。
///
/// 只断言 `is_err()` 是不够的：那样把文案换成一句泛泛的「解析失败」也照样绿，而下游拿到的
/// 是一句没有定位信息的错误（产物是旧版本还是文件坏了？）。这正是 T1-2 的立案点。
fn assert_version_refused(error: String, bad: &str, good: &str) {
    assert!(
        error.contains(bad) && error.contains(good),
        "版本拒绝文案必须同时点名读到的版本（{bad}）与只认的版本（{good}）；实际 {error}"
    );
}

#[test]
fn project_manifest_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&project_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = ProjectManifest::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

#[test]
fn experiment_spec_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&experiment_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = ExperimentSpec::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

#[test]
fn run_record_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&run_record_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = RunRecord::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

#[test]
fn capability_manifest_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&capability_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = CapabilityManifest::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

#[test]
fn evidence_bundle_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&evidence_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = EvidenceBundle::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

#[test]
fn schema_registry_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&registry_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = SchemaRegistry::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

#[test]
fn run_evidence_bundle_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = RunEvidenceBundle::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 1");
}

/// T1-1 三条对象层纪律各一发：空洞的未验证清单 / 未核摘要 / 本地档冒充沙盒档。
///
/// 这三格都是「改错了也不红」那一类：删掉 `unverified` 的 `is_empty` 分支、把
/// `artifact_digests_verified` 放宽成「有产物就算核过」、把档位上限从 L2 提到 L4，
/// 全都会让证据包照常通过解析，而它声称的东西一件都没被核过。
#[test]
fn run_evidence_refuses_an_empty_unverified_list() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["unverified"] = serde_json::json!([]);
    let error = RunEvidenceBundle::from_json(&value.to_string())
        .expect_err("空的未验证清单等于盖单一 verified 标签，必须被拒");
    assert!(
        error.contains("unverified 必须非空"),
        "拒绝文案要点名 unverified：实际 {error}"
    );
}

#[test]
fn run_evidence_refuses_a_sandbox_level_claim() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["verification"]["capability_level"] = serde_json::json!("L3");
    let error = RunEvidenceBundle::from_json(&value.to_string())
        .expect_err("L3 属于沙盒档，必须由带真实 venue 窗口的 EvidenceBundle 支撑");
    assert!(
        error.contains("L3") && error.contains("L2"),
        "拒绝文案要同时点名读到的档（L3）与上限档（L2）：实际 {error}"
    );
}

#[test]
fn run_evidence_refuses_unverified_artifact_digests() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["verification"]["artifact_digests_verified"] = serde_json::json!(false);
    let error = RunEvidenceBundle::from_json(&value.to_string())
        .expect_err("没重算过产物摘要的证据包只是引用了一堆路径");
    assert!(
        error.contains("artifact_digests_verified"),
        "拒绝文案要点名那一格：实际 {error}"
    );
}

#[test]
fn run_evidence_refuses_a_cross_reference_mismatch() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["dataset"]["composed_fingerprint"] = serde_json::json!("another-fp");
    let error = RunEvidenceBundle::from_json(&value.to_string())
        .expect_err("合成数据指纹与 run 块各说各话必须被拒");
    assert!(
        error.contains("dataset.composed_fingerprint") && error.contains("run 块"),
        "拒绝文案要点名是哪一处交叉引用对不上：实际 {error}"
    );
}

/// 质量报告缺席是合法的（这一档输入没有报告），但**带一份空报告**不合法。
///
/// 这两种形态在 JSON 里只差一个 `[]`，含义却相反：`null` 是「没有这份报告」，
/// 空 `usable_tiers` 是「核过了，没有任何一档可用」。后者是一句该被拒的断言。
#[test]
fn run_evidence_refuses_an_empty_quality_report() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["dataset"]["quality_report"] = serde_json::json!({
        "duplicate_rows": 0,
        "out_of_order_rows": 0,
        "missing_intervals": 0,
        "timezone": "UTC",
        "corporate_action_coverage": false,
        "usable_tiers": []
    });
    let error = RunEvidenceBundle::from_json(&value.to_string())
        .expect_err("空 usable_tiers 的质量报告必须被拒");
    assert!(
        error.contains("quality_report"),
        "拒绝文案要点名那一格：实际 {error}"
    );
    // 对照组：同一份证据包把质量报告换成「真的带一档」时必须通过——否则上面那条拒的
    // 可能根本不是空 tiers，而是「只要带报告就拒」。
    value["dataset"]["quality_report"]["usable_tiers"] = serde_json::json!(["bar"]);
    RunEvidenceBundle::from_json(&value.to_string()).expect("带一档可用档位的质量报告必须被接受");
}

#[test]
fn run_evidence_refuses_a_missing_required_artifact() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    // 去掉 fills：少一份就复算不出成交腿，因此「由 RunManifest 离线复算」这句话落不了地。
    let kept: Vec<serde_json::Value> = value["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|artifact| artifact["name"] != "fills")
        .cloned()
        .collect();
    value["artifacts"] = serde_json::json!(kept);
    value["verification"]["verified_artifact_count"] = serde_json::json!(3);
    let error =
        RunEvidenceBundle::from_json(&value.to_string()).expect_err("缺离线复算必需的产物必须被拒");
    assert!(
        error.contains("fills"),
        "拒绝文案要点名缺的是哪一份：实际 {error}"
    );
}

#[test]
fn run_evidence_refuses_a_recompute_guide_for_another_run() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["recompute"]["expected_result_hash"] = serde_json::json!("some-other-run");
    let error =
        RunEvidenceBundle::from_json(&value.to_string()).expect_err("复算指引指的必须是这次运行");
    assert!(
        error.contains("expected_result_hash"),
        "拒绝文案要点名那一格：实际 {error}"
    );
}

#[test]
fn run_evidence_refuses_an_assumption_without_a_source() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["assumptions"][0]["source"] = serde_json::json!("   ");
    let error = RunEvidenceBundle::from_json(&value.to_string())
        .expect_err("空 source 等于把假设写成事实，必须被拒");
    assert!(
        error.contains("source"),
        "拒绝文案要点名那一格：实际 {error}"
    );
}
