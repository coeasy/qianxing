//! 只读运维端点必须跟着磁盘走，而不是停在启动那一刻（V11 S3）。
//!
//! `/reconcile/reports`、`/account/ledger`、`/scheduler/runs` 此前在 `serve` 启动时读一次
//! 就装进 `ApiState`，之后进程活着就一直念那一份：对账 worker 每轮覆写
//! `reconcile/<worker-id>.json`，API 却永远看不见；账户日志续写新成交，账簿端点仍是旧的那本。

use super::*;

fn live_api_service(root: &Path) -> ApiService {
    let mut config = paper_runtime_config(root);
    config.config_fingerprint = None;
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    build_configured_api_service(&config, &config_path).expect("paper 拓扑装配得出 API 服务")
}

/// 文件后端部署也要拿到跨进程共享的限流桶（V12 §16）：此前只有 SQLite 部署装配共享桶，
/// 同一 `data_dir` 下并起的两个 API 进程各算各的额度，而它们用的是同一份配置。
#[test]
fn file_backend_services_share_one_rate_limit_bucket() {
    let root = temp_cli_case_dir("api-shared-rate-limit");
    let first = live_api_service(&root);
    for attempt in 0..qx_api::DEFAULT_RATE_LIMIT_CAPACITY {
        assert_eq!(
            first.handle("GET", "/health", "", 1).status,
            200,
            "第 {attempt} 次请求仍在额度内"
        );
    }
    let second = live_api_service(&root);
    assert_eq!(
        second.handle("GET", "/health", "", 1).status,
        429,
        "同一 data_dir 的第二个 API 进程必须读同一份令牌桶，而不是重新起算"
    );
}

/// 启动之后落盘的对账报告必须读得到：这条链的唯一出口就是那个文件。
#[test]
fn reconcile_report_endpoint_follows_the_worker_written_file() {
    let root = temp_cli_case_dir("api-reconcile-reports-live");
    let service = live_api_service(&root);
    let at_boot: Vec<serde_json::Value> =
        serde_json::from_str(&service.handle("GET", "/reconcile/reports", "", 1).body).unwrap();
    assert!(at_boot.is_empty(), "启动时没有对账报告");

    let report = ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: "ccxt-reconciler-live".into(),
        account_id: "main".into(),
        venue_id: "PAPER".into(),
        observed_ts: 1_700_000_000_000,
        order_issues: Vec::new(),
        balances_count: 1,
        balance_discrepancies: Vec::new(),
        position_snapshots_count: Some(0),
        funding_rate_snapshots_count: None,
        cashflow_count: None,
    };
    let report_dir = root.join("reconcile");
    std::fs::create_dir_all(&report_dir).unwrap();
    std::fs::write(
        report_dir.join("ccxt-reconciler-live.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();

    let after: Vec<serde_json::Value> =
        serde_json::from_str(&service.handle("GET", "/reconcile/reports", "", 2).body).unwrap();
    assert_eq!(
        after.len(),
        1,
        "对账 worker 落的报告必须被读到，启动即冻结的读模型看不见它"
    );
    assert_eq!(after[0]["worker_id"], "ccxt-reconciler-live");
}

/// 同一份现读纪律也要覆盖账簿：账簿来自账户 EventLog 的重放，日志续写之后端点不能
/// 还停在启动时的那本（那时日志还不存在，账簿是空的）。
#[test]
fn ledger_endpoint_sees_fills_appended_after_boot() {
    let root = temp_cli_case_dir("api-ledger-live");
    let service = live_api_service(&root);
    let at_boot: Vec<serde_json::Value> =
        serde_json::from_str(&service.handle("GET", "/account/ledger", "", 1).body).unwrap();
    assert!(
        at_boot.is_empty(),
        "用例开始时账户日志还不存在，账簿必须是空的"
    );

    let mut config = paper_runtime_config(&root);
    config.config_fingerprint = None;
    seed_paper_fill_with_fee(&root, &config);

    let after: Vec<serde_json::Value> =
        serde_json::from_str(&service.handle("GET", "/account/ledger", "", 2).body).unwrap();
    assert!(
        !after.is_empty(),
        "启动后落进账户日志的成交必须出现在账簿读模型里"
    );
    for entry in &after {
        assert_eq!(entry["account_id"], "main");
    }
}
