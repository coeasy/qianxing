//! 地基规格对象（qx-spec）的进程内冒烟：七类声明式文档在本构建里必须既能读入、又能拒绝坏载荷。
//!
//! 规划（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2 / §7）把
//! ProjectManifest / DatasetManifest v2 / ExperimentSpec / RunRecord / CapabilityManifest /
//! EvidenceBundle / SchemaRegistry 定为「统一身份」地基。这里用一份最小合法夹具与一份坏夹具，
//! 证明 `qx_spec::describe` 这个唯一读入漏斗在**随包发布的二进制里**真的接上了：合法夹具必须过，
//! 坏夹具必须被拒——否则地基对象只是躺在源码里的类型。由 `ecosystem` / `all` 自检驱动。

use qx_spec::{describe, FoundationKind};

/// 逐类跑「合法夹具通过 + 坏夹具被拒」，返回一行摘要。
pub(crate) fn verify_foundation_specs() -> Result<String, String> {
    let mut verified = Vec::new();
    for kind in FoundationKind::ALL {
        let good = fixture(kind).ok_or_else(|| format!("{} 缺合法夹具", kind.as_str()))?;
        let readout = describe(kind.as_str(), &good)
            .map_err(|error| format!("{} 合法夹具被拒: {error}", kind.as_str()))?;
        let bad = corrupt(kind, &good);
        if describe(kind.as_str(), &bad).is_ok() {
            return Err(format!(
                "{} 的坏夹具竟然通过校验（缺版本号必须当场拒绝）",
                kind.as_str()
            ));
        }
        verified.push(format!("{}={}", kind.as_str(), readout.identity));
    }
    Ok(format!(
        "{} 类规格读入/拒绝通过（{}）",
        verified.len(),
        verified.join(" ")
    ))
}

/// 把一份合法夹具破坏成必须被拒的载荷：删掉版本号键，让严格解析当场报缺字段。
fn corrupt(kind: FoundationKind, payload: &str) -> String {
    let mut value: serde_json::Value =
        serde_json::from_str(payload).expect("夹具必须是合法 JSON 对象");
    let key = if kind == FoundationKind::Dataset {
        "manifest_version"
    } else {
        "schema_version"
    };
    value
        .as_object_mut()
        .expect("夹具必须是 JSON 对象")
        .remove(key);
    value.to_string()
}

/// 七类对象各一份最小合法夹具。
fn fixture(kind: FoundationKind) -> Option<String> {
    let value = match kind {
        FoundationKind::Project => serde_json::json!({
            "schema_version": 1,
            "project_id": "smoke",
            "profile": "crypto-paper",
            "runtime": "deploy/qianxing.runtime.example.json",
            "artifact_root": "runs",
            "datasets": [{"dataset_id": "demo.bars", "version": "v1"}],
            "strategies": [{"strategy_id": "macd", "language": "rust", "source": "macd"}]
        }),
        FoundationKind::Dataset => serde_json::json!({
            "manifest_version": 1,
            "dataset": {
                "dataset_id": "demo.bars",
                "version": "v1",
                "source": "synthetic",
                "fingerprint": "fp",
                "schema_version": 1,
                "start_timestamp": 1,
                "end_timestamp": 2
            },
            "instrument": "DEMO.SIM",
            "timezone": "UTC",
            "tier": "bar",
            "provider_version": "synthetic-1.0",
            "content_hash": "content",
            "quality_report": {
                "row_count": 3,
                "duplicate_rows": 0,
                "out_of_order_rows": 0,
                "missing_intervals": 0,
                "timezone": "UTC",
                "corporate_action_coverage": false,
                "usable_tiers": ["bar"]
            },
            "source_lineage": {"primary_source": "synthetic", "backup_sources": []}
        }),
        FoundationKind::Experiment => serde_json::json!({
            "schema_version": 1,
            "experiment_id": "smoke-exp",
            "strategy_ref": "macd",
            "dataset_id": "demo.bars@v1",
            "seed": 1,
            "initial_capital_raw": 1000000000i64,
            "parameter_space": [{"name": "fast", "values": ["5", "10"]}],
            "split_plan": {"train_bars": 20, "test_bars": 5, "walk_forward": false, "step_bars": 0},
            "baselines": ["none"],
            "cost_model_ref": "default",
            "risk_rule_set": "qx-order-risk-v1",
            "model_version": "none"
        }),
        FoundationKind::RunRecord => serde_json::json!({
            "schema_version": 1,
            "run_id": "smoke-run",
            "status": "completed",
            "input_digest": "in",
            "code_identity": "0000000",
            "config_fingerprint": "cfg",
            "started_at_ms": 1,
            "finished_at_ms": 2,
            "artifact_refs": [{"name": "summary", "path": "summary.json", "digest": "d"}],
            "replay_verdict": "verified",
            "capability_level": "L1"
        }),
        FoundationKind::Capability => serde_json::json!({
            "schema_version": 1,
            "venue_id": "demo",
            "provider_id": "demo",
            "level": "L1",
            "supported_products": ["spot"],
            "order_types": ["limit", "market"],
            "data_tiers": ["bar"],
            "reconcile": true,
            "code_tested": true,
            "paper_tested": false,
            "sandbox_tested": false,
            "production_approved": false,
            "strategy_api_version": 1,
            "account_snapshot_version": 1
        }),
        FoundationKind::Evidence => serde_json::json!({
            "schema_version": 1,
            "claim": {"summary": "smoke", "capability_level": "L3"},
            "venue_id": "demo",
            "window": {"start_ms": 1, "end_ms": 2},
            "environment": {"host": "ci", "runtime_version": "0.1.0", "network": "testnet"},
            "redacted_config_digest": "cfg-digest",
            "logs": ["logs/smoke.txt"],
            "fact_count": 1,
            "result_digest": "res-digest",
            "operator": "ci-bot"
        }),
        FoundationKind::SchemaRegistry => serde_json::json!({
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
        }),
    };
    Some(value.to_string())
}
