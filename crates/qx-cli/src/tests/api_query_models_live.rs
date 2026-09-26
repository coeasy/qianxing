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

/// 对账报告的落盘形状：对账 worker 每轮覆写 `reconcile/<worker-id>.json`。
fn reconcile_report(worker_id: &str) -> ReconcileReportSnapshot {
    ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: worker_id.into(),
        account_id: "main".into(),
        venue_id: "PAPER".into(),
        observed_ts: 1_700_000_000_000,
        order_issues: Vec::new(),
        balances_count: 1,
        balance_discrepancies: Vec::new(),
        position_snapshots_count: Some(0),
        funding_rate_snapshots_count: None,
        cashflow_count: None,
    }
}

fn write_reconcile_report(root: &Path, report: &ReconcileReportSnapshot) {
    let report_dir = root.join("reconcile");
    std::fs::create_dir_all(&report_dir).unwrap();
    std::fs::write(
        report_dir.join(format!("{}.json", report.worker_id)),
        serde_json::to_vec(report).unwrap(),
    )
    .unwrap();
}

/// 启动之后落盘的对账报告必须读得到：这条链的唯一出口就是那个文件。
#[test]
fn reconcile_report_endpoint_follows_the_worker_written_file() {
    let root = temp_cli_case_dir("api-reconcile-reports-live");
    let service = live_api_service(&root);
    let at_boot: Vec<serde_json::Value> =
        serde_json::from_str(&service.handle("GET", "/reconcile/reports", "", 1).body).unwrap();
    assert!(at_boot.is_empty(), "启动时没有对账报告");

    let report = reconcile_report("ccxt-reconciler-live");
    write_reconcile_report(&root, &report);

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

/// `QueryPort` 的三格读点要问同一个现读出口，不能等 HTTP 侧先读过（V11 I2）。
///
/// S3 把回填留在了 `query_models()` 里，于是 trait 读点读的那份副本只在"有人走过端点"之后
/// 才前进：装配完到第一个请求之间，trait 念的仍是 boot 那一份——正是端点侧修掉的那个形状，
/// 只是换了读侧。H1 已经按同样方式收过控制面的那一格。
///
/// 这台服务只给"第一格"证据：现读成功会回填同一份副本，先读的 `reconcile_reports()` 会把
/// `ledger_entries()` 替它答对（变异 I2-M1 就是这样在这条用例上活下来的）。逐格现读由下面
/// `each_query_port_slot_reads_the_live_provider_before_any_backfill` 三台服务钉住。
#[test]
fn query_port_trait_read_points_load_without_an_http_request() {
    let root = temp_cli_case_dir("api-query-port-live");
    let service = live_api_service(&root);
    assert!(
        service.query_port().reconcile_reports().is_empty(),
        "boot 时既没有对账报告也没有成交"
    );

    write_reconcile_report(&root, &reconcile_report("ccxt-reconciler-trait"));
    let mut config = paper_runtime_config(&root);
    config.config_fingerprint = None;
    seed_paper_fill_with_fee(&root, &config);

    let reports = service.query_port().reconcile_reports();
    assert_eq!(
        reports.len(),
        1,
        "trait 读点必须现读，而不是念装配那一刻的副本"
    );
    assert_eq!(reports[0].worker_id, "ccxt-reconciler-trait");
    assert!(
        !service.query_port().ledger_entries().is_empty(),
        "账簿那格同理：boot 之后落进账户日志的成交要不经任何请求就读得到"
    );
    let via_http: Vec<ReconcileReportSnapshot> =
        serde_json::from_str(&service.handle("GET", "/reconcile/reports", "", 3).body).unwrap();
    assert_eq!(via_http, reports, "端点与 trait 读点说的是同一份报告");
}

/// 一份"现读拿得到、boot 副本里没有"的三格样例，供逐格现读与失败兜底两条用例共用。
fn live_query_models() -> ApiQueryModels {
    ApiQueryModels {
        job_runs: vec![qx_scheduler::JobRun {
            run_id: 99,
            job_id: "strategy".into(),
            trading_day: "20260911".into(),
            attempt: 1,
            status: qx_scheduler::JobStatus::Running,
            manifest_digest: Some(7),
            error_code: None,
            next_retry_ts: None,
            started_ts: 1,
            deadline_ts: 2,
        }],
        ledger_entries: vec![qx_core::LedgerEntry {
            id: 1,
            account_id: "main".into(),
            currency: "USDT".into(),
            kind: qx_core::LedgerEntryKind::Adjustment,
            amount: Money::from_raw(123),
            instrument: None,
            quantity: Quantity::ZERO,
            price: None,
            order_id: None,
            ts: 100,
            multiplier: 1,
            position_side: None,
        }],
        reconcile_reports: vec![reconcile_report("ccxt-reconciler-provider")],
    }
}

/// 装了现读 provider 的服务：副本按 `ApiState::default()` 是空的，第一格读什么就该是现读那份。
fn service_with_live_models() -> ApiService {
    ApiService::new(ApiState::default()).with_query_models_provider(|| Ok(live_query_models()))
}

/// `QueryPort` 的每一格都要在"本机第一次现读"的位置上被断言（V11 I2）。
///
/// 现读成功会把结果回填进同一份副本，于是同一台服务上先读的那格会把后读的格替它答对：
/// 单独把 `ledger_entries()` 退回副本，端到端那条用例照样绿（变异 I2-M1 就是这样活下来的）。
/// 逐格现读只能由三台各读一格的服务钉；HTTP 侧每请求现读，没有这个问题。
#[test]
fn each_query_port_slot_reads_the_live_provider_before_any_backfill() {
    assert_eq!(
        service_with_live_models().query_port().job_runs().len(),
        1,
        "作业那格：boot 副本里没有它，读到 0 就是退回了副本"
    );
    assert_eq!(
        service_with_live_models()
            .query_port()
            .ledger_entries()
            .len(),
        1,
        "账簿那格同理，且它必须是自己这台服务上的第一次现读"
    );
    assert_eq!(
        service_with_live_models()
            .query_port()
            .reconcile_reports()
            .len(),
        1,
        "对账报告那格同理"
    );
}

/// provider 现读失败时，trait 退回**最后已知副本**而不是空表（V11 I2）。
///
/// trait 签名没有 `Result`：把"这一次没读到"与"磁盘上确实没有"塌成同一个空值，调用方就再也
/// 分不开两者——R10 在端点侧收过一次这个口径，这里是换到 trait 读侧的同一件事。
#[test]
fn query_port_keeps_the_last_known_models_when_the_provider_fails() {
    let online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probe = std::sync::Arc::clone(&online);
    let service = ApiService::new(ApiState::default()).with_query_models_provider(move || {
        match probe.load(std::sync::atomic::Ordering::Relaxed) {
            true => Ok(live_query_models()),
            false => Err("对账报告目录读失败".into()),
        }
    });
    let port = service.query_port();
    assert_eq!(
        port.reconcile_reports().len(),
        1,
        "先按成功那一次把副本填上，才有『最后已知』可退"
    );
    online.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        port.reconcile_reports().len(),
        1,
        "现读失败要退回最后已知副本，而不是把『这次没读到』念成『没有报告』"
    );
}
