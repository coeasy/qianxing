use super::*;

#[test]
fn evidence_bundle_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&evidence_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = EvidenceBundle::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 3");
}

#[test]
fn evidence_bundle_refuses_schema_versions_that_overflow_u32_without_truncating() {
    let mut value: serde_json::Value = serde_json::from_str(&evidence_json()).unwrap();
    value["schema_version"] = serde_json::json!(4_294_967_297_u64);
    let error = EvidenceBundle::from_json(&value.to_string()).expect_err("超范围版本必须被拒");
    assert_version_refused(error, "schema_version=4294967297", "只认 3");
}

#[test]
fn legacy_venue_evidence_v1_migrates_to_the_unified_bundle_generation() {
    let bundle = EvidenceBundle::from_json(&evidence_json()).expect("venue v1 应能显式迁入 v3");
    assert_eq!(bundle.schema_version, EVIDENCE_BUNDLE_SCHEMA_VERSION);
    assert!(matches!(bundle.generation, EvidenceGeneration::Venue(_)));
    let canonical = bundle.to_json().expect("统一证据包可序列化");
    assert!(canonical.contains("\"venue\""));
    assert!(canonical.contains("\"schema_version\": 3"));
}

#[test]
fn run_evidence_bundle_refuses_an_unsupported_schema_version() {
    let mut value: serde_json::Value = serde_json::from_str(&run_evidence_json()).unwrap();
    value["schema_version"] = serde_json::json!(7);
    let error = EvidenceBundle::from_json(&value.to_string()).expect_err("未来版本必须被拒");
    assert_version_refused(error, "schema_version=7", "只认 3");
}

#[test]
fn legacy_run_evidence_v1_migrates_to_the_unified_bundle_generation() {
    let bundle = EvidenceBundle::from_json(&run_evidence_json())
        .expect("已有 L1/L2 run evidence v1 应能显式迁入统一 v3");
    assert_eq!(bundle.schema_version, EVIDENCE_BUNDLE_SCHEMA_VERSION);
    let EvidenceGeneration::Run(run) = &bundle.generation else {
        panic!("run evidence v1 必须迁入 run 世代")
    };
    assert_eq!(run.run.run_id, "run-1");
    assert_eq!(run.verification.capability_level, CapabilityLevel::L1);
    assert_eq!(run.recompute.expected_result_hash, run.run.result_hash);
    let canonical = bundle.to_json().expect("统一证据包可序列化");
    assert!(canonical.contains("\"run\""));
    assert!(canonical.contains("\"schema_version\": 3"));
}

#[test]
fn legacy_evidence_migration_refuses_ambiguous_or_missing_generation_fields() {
    for payload in [
        r#"{"schema_version":1}"#,
        r#"{"schema_version":1,"run":{},"claim":{}}"#,
    ] {
        let error = EvidenceBundle::from_json(payload).expect_err("世代身份必须唯一");
        assert!(error.contains("世代无法唯一判定"), "{error}");
    }
}
