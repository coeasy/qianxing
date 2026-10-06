//! 地基规格对象（qx-spec）的进程内冒烟：七类声明式文档在本构建里必须既能读入、又能拒绝坏载荷。
//!
//! 规划（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2 / §7）把
//! ProjectManifest / DatasetManifest v2 / ExperimentSpec / RunRecord / CapabilityManifest /
//! EvidenceBundle / SchemaRegistry 定为「统一身份」地基。这里用一份最小合法夹具与一份坏夹具，
//! 证明 `qx_spec::describe` 这个唯一读入漏斗在**随包发布的二进制里**真的接上了：合法夹具必须过，
//! 坏夹具必须被拒——否则地基对象只是躺在源码里的类型。由 `ecosystem` / `all` 自检驱动。

use crate::plan_args::PlanArgs;
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

/// `plan` 命令的唯一实现：读一份地基规格对象，按严格 schema 解析、自洽校验、算出稳定指纹。
///
/// 这是七类「统一身份」文档对使用者开放的唯一读入口——`qx_spec::describe` 是同一份漏斗，
/// 这里只负责取文件、印摘要（`--json` 时改印规范化正文）。告警一律走 stderr，不污染 stdout。
pub(crate) fn plan_readout(args: &PlanArgs) -> Result<(), String> {
    let payload = std::fs::read_to_string(&args.file)
        .map_err(|error| format!("读取规格文件失败 {}: {error}", args.file.display()))?;
    let readout = describe(&args.kind, &payload)?;
    if args.json {
        println!("{}", readout.canonical_json);
    } else {
        println!(
            "[Plan · {}] identity={} schema_version={} fingerprint={}",
            readout.kind, readout.identity, readout.schema_version, readout.fingerprint
        );
    }
    for warning in &readout.warnings {
        eprintln!("[Plan · 警告] {warning}");
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_args::PlanArgs;

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "qianxing-plan-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("doc.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn args(kind: &str, file: std::path::PathBuf) -> PlanArgs {
        PlanArgs {
            kind: kind.to_string(),
            file,
            json: false,
        }
    }

    /// 七类地基对象都必须能被 `plan` 读入：这是「统一身份」对使用者开放的唯一入口。
    #[test]
    fn plan_reads_every_foundation_kind() {
        for kind in FoundationKind::ALL {
            let path = write_temp(kind.as_str(), &fixture(kind).unwrap());
            plan_readout(&args(kind.as_str(), path.clone())).unwrap_or_else(|error| {
                panic!("{} 的合法夹具必须被 plan 读入: {error}", kind.as_str())
            });
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[test]
    fn plan_rejects_unknown_kind_missing_file_and_bad_payload() {
        let path = write_temp("bad", "{\"not\":\"a manifest\"}");
        assert!(
            plan_readout(&args("nonsense", path.clone())).is_err(),
            "未知 kind 必须被拒"
        );
        assert!(
            plan_readout(&args("project", path.clone())).is_err(),
            "缺版本号必须被拒"
        );
        let missing = path.with_file_name("nope.json");
        assert!(
            plan_readout(&args("project", missing)).is_err(),
            "缺文件必须被拒"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let ok = write_temp("ok", &fixture(FoundationKind::Project).unwrap());
        assert!(plan_readout(&args("project", ok.clone())).is_ok());
        let _ = std::fs::remove_dir_all(ok.parent().unwrap());
    }
}
