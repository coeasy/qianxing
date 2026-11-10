use super::*;

fn workspace_deploy() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
}

fn runtime_fixture(name: &str) -> (PathBuf, qx_runtime::RuntimeConfig) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
    let deploy = workspace_deploy();
    let source = deploy.join(name);
    let mut config = crate::read_runtime_config(&source).expect("load runtime fixture");
    config.scheduler.jobs_path = workspace_deploy()
        .join("qianxing.scheduler.jobs.example.json")
        .to_string_lossy()
        .into_owned();
    let root = temp_cli_case_dir(&format!(
        "runtime-capability-matrix-{}",
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    config.storage.data_dir = root.join("data").to_string_lossy().into_owned();
    let runtime_path = root.join("runtime.json");
    std::fs::write(
        &runtime_path,
        serde_json::to_vec(&config).expect("serialize runtime config"),
    )
    .expect("write runtime config");
    (runtime_path, config)
}

#[test]
fn runtime_check_lists_executable_and_unimplemented_control_commands() {
    let (path, _) = runtime_fixture("qianxing.runtime.example.json");
    let report = crate::collect_runtime_check_report(&path).expect("runtime-check report");
    let capabilities = report["capabilities"]
        .as_array()
        .expect("capability matrix");

    for (id, status) in [
        ("control.command.submit_order", "supported"),
        ("control.command.cancel_order", "supported"),
        ("control.command.pause_strategy", "supported"),
        ("control.command.resume_strategy", "supported"),
        ("control.command.change_risk_limit", "unsupported"),
        ("control.command.reconcile_account", "unsupported"),
        ("control.command.retry_job", "unsupported"),
        ("control.command.switch_venue", "unsupported"),
    ] {
        let item = capabilities
            .iter()
            .find(|item| item["id"] == id)
            .unwrap_or_else(|| panic!("missing capability {id}: {capabilities:?}"));
        assert_eq!(item["status"], status, "capability {id}");
        assert!(item["reason"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert!(item["required_evidence"].as_array().is_some());
    }
    assert_eq!(
        report["ok"], true,
        "unrequested command kinds are informational"
    );
    for item in report["capability_failures"].as_array().unwrap() {
        assert!(item["capability"]
            .as_str()
            .is_some_and(|id| id.starts_with("control.command.")));
        assert_eq!(item["blocking"], false);
        assert!(item["required_evidence"].as_array().is_some());
    }
}

#[test]
fn runtime_check_blocks_enabled_scheduler_jobs_without_a_dispatcher() {
    let (path, mut config) = runtime_fixture("qianxing.runtime.example.json");
    let root = path.parent().expect("fixture directory");
    let jobs_path = root.join("jobs.json");
    std::fs::write(
        &jobs_path,
        r#"[{"job_id":"manual-job","job_version":"v1","owner":"*","enabled":true,"trigger":"Manual","window":"Any","depends_on":[],"timeout_seconds":30,"retry_policy":{"max_attempts":1,"backoff_seconds":0,"retryable_codes":[]},"concurrency_key":"manual-job","idempotency_key":"manual-job","dry_run":true}]"#,
    )
    .expect("write unsupported scheduler job");
    config.scheduler.jobs_path = jobs_path.to_string_lossy().into_owned();
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();

    let report = crate::collect_runtime_check_report(&path).expect("runtime-check report");
    let item = report["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "scheduler.job.manual-job")
        .unwrap_or_else(|| panic!("scheduler capability entry missing: {report:#}"));
    assert_eq!(item["status"], "unsupported");
    assert_eq!(item["requested"], true);
    assert!(!item["required_evidence"].as_array().unwrap().is_empty());
    assert_eq!(report["ok"], false);
    assert_eq!(report["readiness_layers"]["C"]["status"], "blocked");
    assert!(report["capability_failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|failure| {
            failure["capability"] == "scheduler.job.manual-job"
                && failure["blocking"] == true
                && failure["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("不会派发"))
        }));
}

#[test]
fn runtime_check_reports_paper_ashare_submit_refusal_as_structured_failure() {
    let (path, mut config) = runtime_fixture("qianxing.runtime.paper-strategy.example.json");
    let rules_path = path.parent().unwrap().join("ashare-rules.json");
    std::fs::write(&rules_path, "{}").unwrap();
    config.strategy.ashare_rules_path = Some(rules_path.to_string_lossy().into_owned());
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();

    let report = crate::collect_runtime_check_report(&path).expect("runtime-check report");
    let item = report["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "paper.ashare_submission")
        .expect("A-share Paper capability entry");
    assert_eq!(item["status"], "unsupported");
    assert_eq!(item["requested"], true);
    assert!(item["reason"].as_str().unwrap().contains("T+1"));
    assert_eq!(report["ok"], false);
    assert!(report["capability_failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|failure| {
            failure["capability"] == "paper.ashare_submission" && failure["blocking"] == true
        }));
}

#[test]
fn runtime_check_does_not_claim_a_strategy_capability_without_host_support() {
    let (path, mut config) = runtime_fixture("qianxing.runtime.example.json");
    config.strategy.required_capabilities =
        vec![qx_strategy::capabilities::StrategyCapability::Timer];
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();

    let report = crate::collect_runtime_check_report(&path).expect("runtime-check report");
    let item = report["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "strategy.strategy.timer")
        .expect("strategy capability entry");
    assert_eq!(item["status"], "unsupported");
    assert_eq!(item["requested"], true);
    assert!(item["reason"]
        .as_str()
        .unwrap()
        .contains("does not support"));
    assert_eq!(report["readiness_layers"]["C"]["status"], "blocked");
    assert_eq!(report["ok"], false);
}

#[test]
fn runtime_check_includes_implicit_builtin_strategy_requirements() {
    let (path, mut config) = runtime_fixture("qianxing.runtime.example.json");
    config.strategy.builtin_strategy = Some("pairs_arbitrage".into());
    config.strategy.bars_snapshot_path = Some("bars.json".into());
    config.strategy.builtin_reference_instrument = Some("ETHUSDT.BINANCE".into());
    config.strategy.builtin_reference_bars_snapshot_path = Some("reference-bars.json".into());
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();

    let report = crate::collect_runtime_check_report(&path).expect("runtime-check report");
    let item = report["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "strategy.strategy.multi_instrument")
        .expect("implicit built-in capability entry");
    assert_eq!(item["status"], "unsupported");
    assert_eq!(item["requested"], true);
    assert_eq!(report["readiness_layers"]["C"]["status"], "blocked");
    assert_eq!(report["ok"], false);
}

#[test]
fn unknown_required_strategy_capability_is_rejected_before_readiness() {
    let (path, _) = runtime_fixture("qianxing.runtime.example.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["strategy"]["required_capabilities"] = serde_json::json!(["future_capability"]);
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let error = crate::collect_runtime_check_report(&path).unwrap_err();
    assert!(
        error.contains("unknown variant") || error.contains("unknown value"),
        "{error}"
    );
}
