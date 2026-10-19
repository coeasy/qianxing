use super::*;

#[test]
fn paper_e2e_entrypoint_runs_every_strategy_worker_and_ledger() {
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
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                workspace_root
                    .join("deploy")
                    .join("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    config.strategy.target_qty = 0;

    // A multi-strategy Paper topology must exercise every worker and each
    // scheduled job. Previously the E2E entry point silently selected only
    // the first Strategy worker and still reported a successful pipeline.
    let primary_worker = config
        .workers
        .iter()
        .find(|worker| worker.id == "strategy-paper")
        .unwrap()
        .clone();
    let mut secondary_worker = primary_worker;
    secondary_worker.id = "strategy-paper-2".into();
    config.workers.push(secondary_worker);
    let mut primary_strategy = config.strategy.clone();
    primary_strategy.id = Some("strategy-paper".into());
    let mut secondary_strategy = primary_strategy.clone();
    secondary_strategy.id = Some("strategy-paper-2".into());
    secondary_strategy.version = "paper-signal-secondary-v1".into();
    let secondary_snapshot_path = root.join("secondary-target.json");
    let mut secondary_snapshot: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            secondary_strategy
                .target_snapshot_path
                .as_ref()
                .expect("primary snapshot path"),
        )
        .expect("read primary target snapshot"),
    )
    .expect("parse target snapshot");
    secondary_snapshot["strategy_version"] =
        serde_json::Value::String(secondary_strategy.version.clone());
    std::fs::write(
        &secondary_snapshot_path,
        serde_json::to_vec_pretty(&secondary_snapshot).unwrap(),
    )
    .unwrap();
    secondary_strategy.target_snapshot_path =
        Some(secondary_snapshot_path.to_string_lossy().into_owned());
    config.strategies = vec![primary_strategy, secondary_strategy];

    let original_jobs_path = workspace_root
        .join("deploy")
        .join("qianxing.scheduler.paper-order-smoke.json");
    let mut jobs: serde_json::Value =
        serde_json::from_slice(&std::fs::read(original_jobs_path).expect("read Paper smoke jobs"))
            .expect("parse Paper smoke jobs");
    let mut secondary_job = jobs[0].clone();
    secondary_job["job_id"] = serde_json::Value::String("paper-strategy-order-2".into());
    secondary_job["owner"] = serde_json::Value::String("strategy-paper-2".into());
    secondary_job["concurrency_key"] = serde_json::Value::String("paper-strategy-order-2".into());
    secondary_job["idempotency_key"] =
        serde_json::Value::String("paper-strategy-order-2-v1".into());
    jobs.as_array_mut().expect("jobs array").push(secondary_job);
    let jobs_path = root.join("paper-jobs.json");
    std::fs::write(&jobs_path, serde_json::to_vec_pretty(&jobs).unwrap()).unwrap();
    config.scheduler.jobs_path = jobs_path.to_string_lossy().into_owned();

    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    run_paper_pipeline_once(&config_path).unwrap();
    run_paper_pipeline_once(&config_path).unwrap();
    let pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDT").unwrap();
    assert_eq!(pipeline.orders().len(), 2);
    assert!(pipeline
        .orders()
        .iter()
        .all(|order| order.status == OrderStatus::Filled));
    let strategy_ids: std::collections::BTreeSet<_> = pipeline
        .orders()
        .iter()
        .map(|order| {
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.strategy_id.clone())
                .unwrap()
        })
        .collect();
    assert_eq!(
        strategy_ids,
        ["strategy-paper".to_string(), "strategy-paper-2".to_string()]
            .into_iter()
            .collect()
    );
    assert!(pipeline.ledger().entries().len() >= 4);
    let _ = std::fs::remove_dir_all(root);
}
