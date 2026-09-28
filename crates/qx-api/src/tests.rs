//! qx-api HTTP 读侧的常驻用例（从 `lib.rs` 外置）。
//!
//! 行数预算只降不升：读侧多一条脉络就得在别处节省出来（同 qx-control 的口径）。

use super::*;
use qx_control::{CommandKind, Permission};
use qx_core::{Event, EventKind, Priority};
use qx_protocol::AccountSnapshot;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::collections::BTreeMap;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
struct NoCertificateResolver;

impl ResolvesServerCert for NoCertificateResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        None
    }
}

#[test]
fn websocket_accept_matches_rfc_example() {
    assert_eq!(
        websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[test]
fn control_and_query_routes_are_audited() {
    let service = ApiService::new(ApiState::default());
    assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
    let metrics = service.handle("GET", "/metrics", "", 1);
    assert_eq!(metrics.status, 200);
    // 行首而不是"文里出现过"：整份文本挤成一行时 `contains` 依旧绿，Prometheus 却一条样本都取不到。
    assert!(metrics
        .body
        .lines()
        .any(|line| line.starts_with("qx_api_requests_total ")));
    let command = ControlCommand {
        command_id: 1,
        request_id: "api-1".into(),
        operator_id: "ops".into(),
        reason: "pause after alert".into(),
        kind: CommandKind::PauseStrategy,
        target: "s1".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    };
    let body = serde_json::to_string(&command).unwrap();
    assert_eq!(
        service.handle("POST", "/control/commands", &body, 2).status,
        202
    );
    assert_eq!(service.state().lock().unwrap().control.audit().len(), 1);
}

#[test]
fn metrics_route_appends_supervised_worker_metrics() {
    let service = ApiService::new(ApiState::default())
        .with_worker_metrics_provider(|| "qx_worker_up{worker=\"relay\"} 1\n".into());
    let metrics = service.handle("GET", "/metrics", "", 1);
    assert_eq!(metrics.status, 200);
    assert!(metrics.body.contains("qx_worker_up{worker=\"relay\"} 1"));
}

/// `/metrics` 必须是**多行** Prometheus 文本：这一版之前每格之间写的是两字符的字面反斜杠加 n，
/// 端点照样回 200、`contains` 照样绿，而 `deploy/prometheus/qianxing-alerts.yml` 的六条告警
/// 一条都取不到样本——对外通告健康、实际链路断开（V11 R7-i）。按行断言，字面 `\n` 一出现就红。
#[test]
fn metrics_exposition_puts_every_metric_on_its_own_line() {
    let service = ApiService::new(ApiState::default());
    assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
    let body = service.handle("GET", "/metrics", "", 2).body;
    assert!(!body.contains("\\n"), "指标之间仍是字面反斜杠加 n：{body}");
    let samples: Vec<&str> = body.lines().filter(|line| !line.starts_with('#')).collect();
    assert_eq!(
        samples,
        [
            "qx_api_requests_total 2",
            "qx_api_rate_limit_rejected_total 0",
            "qx_api_authentication_rejected_total 0",
            "qx_api_connections_rejected_total 0",
        ]
    );
    // 每一格样本都要有自己的一对 HELP/TYPE；行数一少就说明有两格被粘回同一行。
    assert_eq!(body.lines().count(), samples.len() * 3, "{body}");
}

/// 追加侧的同一条判据：worker 块由 `worker_metrics_provider` 拼在 API 摘要之后，上一版 API
/// 摘要的末行没有换行，于是 worker 的第一格被粘成 `..._rejected_total 0qx_worker_up{...}`——
/// 一个谁都不解析的指标名（V11 R7-i）。
#[test]
fn appended_worker_metrics_start_on_a_fresh_line() {
    let service = ApiService::new(ApiState::default())
        .with_worker_metrics_provider(|| "qx_worker_up{worker=\"relay\"} 1\n".into());
    let body = service.handle("GET", "/metrics", "", 1).body;
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        lines.last().copied(),
        Some("qx_worker_up{worker=\"relay\"} 1")
    );
    assert!(
        !lines.iter().any(|line| line.contains("0qx_worker")),
        "worker 块被粘在上一格的尾巴上：{lines:?}"
    );
}

/// 端点侧的"未算 ≠ 零"（V11 Q67，Q70 把权益并进来）：同一份快照在 `/account/balances` 上必须把算出来
/// 正好是零的钱印成 0、把这一层没算过的钱印成 null。两者印成同一个数时，读侧分不清
/// "这个账户没有保证金"和"根本没人替它算过保证金"。
#[test]
fn balances_endpoint_publishes_absent_money_as_null_not_zero() {
    let mut state = ApiState::default();
    let mut snapshot = AccountSnapshot::new(1, "main", "default", "paper", 10);
    snapshot.equity_raw = Some(500);
    snapshot.available_raw = Some(0);
    state.publish_snapshot(snapshot).unwrap();
    let body = ApiService::new(state)
        .handle("GET", "/account/balances", "", 1)
        .body;
    assert!(
        body.contains("\"equity_raw\":500") && body.contains("\"available_raw\":0"),
        "算得出的两个量按实况印: {body}"
    );
    assert!(
        body.contains("\"margin_raw\":null"),
        "没算过的保证金必须印 null，而不是一个合法的 0: {body}"
    );
    // 权益从 Q70 起同样可能是"这一层算不出"：它在端点上必须走同一条 null 口径，
    // 而不是被 `.unwrap_or(0)` 折回一个看起来合法的零。
    let mut absent_state = ApiState::default();
    absent_state
        .publish_snapshot(AccountSnapshot::new(2, "main", "default", "paper", 20))
        .unwrap();
    let absent_body = ApiService::new(absent_state)
        .handle("GET", "/account/balances", "", 2)
        .body;
    assert!(
        absent_body.contains("\"equity_raw\":null"),
        "算不出标记价时权益必须印 null，而不是给账户兜一个 0: {absent_body}"
    );
}

#[test]
fn readiness_separates_liveness_from_dependency_health() {
    let service = ApiService::new(ApiState::default()).with_readiness_provider(|| ApiReadiness {
        ready: false,
        detail: "control_store_unavailable".into(),
    });
    assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
    let ready = service.handle("GET", "/ready", "", 2);
    assert_eq!(ready.status, 503);
    assert!(ready.body.contains("control_store_unavailable"));
}

#[test]
fn query_port_exposes_account_orders_positions_balances_and_audit() {
    let mut state = ApiState::default();
    let mut snapshot = AccountSnapshot::new(10, "main", "default", "paper", 100);
    snapshot.cash_raw.insert("USDT".into(), 123);
    let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    snapshot.orders.insert(
        7,
        qx_protocol::OrderSnapshot {
            order_id: 7,
            client_order_id: 7,
            instrument: instrument.clone(),
            side: qx_core::Side::Buy,
            quantity_raw: 1,
            filled_raw: 0,
            status: qx_core::OrderStatus::Accepted,
        },
    );
    snapshot.positions.insert(
        instrument.clone(),
        qx_protocol::PositionSnapshot {
            instrument,
            quantity_raw: 1,
            ..qx_protocol::PositionSnapshot::default()
        },
    );
    state.job_runs.push(qx_scheduler::JobRun {
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
    });
    state.ledger_entries.push(qx_core::LedgerEntry {
        id: 1,
        account_id: "main".into(),
        currency: "USDT".into(),
        kind: qx_core::LedgerEntryKind::Adjustment,
        amount: qx_core::Money::from_raw(123),
        instrument: None,
        quantity: qx_core::Quantity::ZERO,
        price: None,
        order_id: None,
        ts: 100,
        multiplier: 1,
        position_side: None,
    });
    state.reconcile_reports.insert(
        "reconciler".into(),
        ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: "reconciler".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            observed_ts: 100,
            order_issues: Vec::new(),
            balances_count: 1,
            balance_discrepancies: Vec::new(),
            position_snapshots_count: Some(1),
            funding_rate_snapshots_count: Some(0),
            cashflow_count: Some(0),
        },
    );
    state.publish_snapshot(snapshot).unwrap();
    let service = ApiService::new(state);
    assert_eq!(service.handle("GET", "/account/orders", "", 1).status, 200);
    assert_eq!(
        service.handle("GET", "/account/positions", "", 2).status,
        200
    );
    assert_eq!(
        service.handle("GET", "/account/balances", "", 3).status,
        200
    );
    assert_eq!(service.handle("GET", "/control/audit", "", 4).status, 200);
    assert_eq!(service.handle("GET", "/scheduler/runs", "", 5).status, 200);
    assert_eq!(service.handle("GET", "/account/ledger", "", 6).status, 200);
    assert_eq!(
        service.handle("GET", "/reconcile/reports", "", 7).status,
        200
    );
    assert_eq!(service.query_port().account_cash()["USDT"], 123);
    assert_eq!(service.query_port().account_orders().len(), 1);
    assert_eq!(service.query_port().account_positions().len(), 1);
    assert_eq!(service.query_port().job_runs().len(), 1);
    assert_eq!(service.query_port().ledger_entries().len(), 1);
    assert_eq!(service.query_port().reconcile_reports().len(), 1);
}

#[test]
fn control_submitter_persists_before_api_accepts() {
    let persisted = Arc::new(Mutex::new(ControlPlane::default()));
    let persisted_for_callback = Arc::clone(&persisted);
    let service =
        ApiService::new(ApiState::default()).with_control_submitter(move |command, granted, ts| {
            let mut plane = persisted_for_callback.lock().unwrap();
            let audit = plane
                .submit_as(command, granted, ts)
                .map_err(ControlSubmitError::Rejected)?;
            Ok((plane.clone(), audit))
        });
    let command = ControlCommand {
        command_id: 2,
        request_id: "durable-api-2".into(),
        operator_id: "ops".into(),
        reason: "durable command".into(),
        kind: CommandKind::SubmitOrder,
        target: "2".into(),
        payload: BTreeMap::from([("order_json".into(), "{}".into())]),
        permission: Permission::Trading,
        dry_run: true,
    };
    let body = serde_json::to_string(&command).unwrap();
    let response = service.handle("POST", "/control/commands", &body, 3);
    assert_eq!(response.status, 202);
    assert_eq!(
        service.handle("POST", "/control/commands", &body, 4).status,
        409
    );
    assert_eq!(persisted.lock().unwrap().audit().len(), 1);
    assert_eq!(service.state().lock().unwrap().control.audit().len(), 1);
}

/// 没有执行者的命令种类拿不到 202（V11 P1）：受理即承诺"有人会执行它"，否则它会永远停在
/// `Accepted`，操作者从 `/control/audit` 读到的是"已受控"而不是"没人管"。
///
/// 走 durable submitter 这条真实生产路径，因此判据同时要求落盘的那本 store 一笔审计都没写。
#[test]
fn kinds_without_an_executor_get_400_before_any_audit() {
    let persisted = Arc::new(Mutex::new(ControlPlane::default()));
    let persisted_for_callback = Arc::clone(&persisted);
    let service =
        ApiService::new(ApiState::default()).with_control_submitter(move |command, granted, ts| {
            let mut plane = persisted_for_callback.lock().unwrap();
            let audit = plane
                .submit_as(command, granted, ts)
                .map_err(ControlSubmitError::Rejected)?;
            Ok((plane.clone(), audit))
        });
    // 权限一次给到 Admin：403 不能冒充成这颗判据。
    for (index, kind) in [
        CommandKind::CancelOrder,
        CommandKind::ChangeRiskLimit,
        CommandKind::ReconcileAccount,
        CommandKind::RetryJob,
        CommandKind::SwitchVenue,
    ]
    .into_iter()
    .enumerate()
    {
        let label = format!("{kind:?}");
        let command = ControlCommand {
            command_id: 300 + index as u64,
            request_id: format!("no-executor-{index}"),
            operator_id: "ops".into(),
            reason: "acceptance boundary".into(),
            kind,
            target: "s1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Admin,
            dry_run: true,
        };
        let body = serde_json::to_string(&command).unwrap();
        let response = service.handle("POST", "/control/commands", &body, 10 + index as u64);
        assert_eq!(
            response.status, 400,
            "{label} 没有执行者，不能拿到 202：实际 {}",
            response.status
        );
        assert!(
            response.body.contains("没有执行者"),
            "{label} 的 400 理由不是「没人执行」，而是别的检查顺路顶上了: {}",
            response.body
        );
    }
    assert!(
        persisted.lock().unwrap().audit().is_empty(),
        "被拒的命令仍在落盘 store 里留下了 Accepted 审计"
    );
    // 对照：同一本 store 换成有执行者的种类必须仍然 202，判据不能是"全拒"。
    let accepted = ControlCommand {
        command_id: 900,
        request_id: "with-executor".into(),
        operator_id: "ops".into(),
        reason: "acceptance boundary control".into(),
        kind: CommandKind::PauseStrategy,
        target: "s1".into(),
        payload: BTreeMap::new(),
        permission: Permission::Admin,
        dry_run: true,
    };
    let body = serde_json::to_string(&accepted).unwrap();
    assert_eq!(
        service
            .handle("POST", "/control/commands", &body, 20)
            .status,
        202
    );
    assert_eq!(persisted.lock().unwrap().audit().len(), 1);
}

/// 受理已经落账、入队却失败时，202 就是把"已交给执行者"当成事实（V11 R6-3）。补投由执行
/// worker 每轮扫 `pending()` 负责，所以响应要说的是"这一半没成"，而不是悄悄把缺口留给下一轮。
#[test]
fn a_command_that_cannot_be_queued_answers_503_not_202() {
    let persisted = Arc::new(Mutex::new(ControlPlane::default()));
    let queued = Arc::new(Mutex::new(Vec::<u64>::new()));
    let service = |fail: bool| {
        let plane_for_submit = Arc::clone(&persisted);
        let seen = Arc::clone(&queued);
        ApiService::new(ApiState::default())
            .with_control_submitter(move |command, granted, ts| {
                let mut plane = plane_for_submit.lock().unwrap();
                let audit = plane
                    .submit_as(command, granted, ts)
                    .map_err(ControlSubmitError::Rejected)?;
                Ok((plane.clone(), audit))
            })
            .with_command_enqueuer(move |command, _ts| {
                if fail {
                    return Err("写入控制命令队列失败: StorageBusy".into());
                }
                seen.lock().unwrap().push(command.command_id);
                Ok(())
            })
    };
    let command = ControlCommand {
        command_id: 1201,
        request_id: "queue-broken".into(),
        operator_id: "ops".into(),
        reason: "queue half honesty".into(),
        kind: CommandKind::SubmitOrder,
        target: "1".into(),
        payload: BTreeMap::from([("order_json".into(), "{}".into())]),
        permission: Permission::Trading,
        dry_run: true,
    };
    let response = service(true).handle(
        "POST",
        "/control/commands",
        &serde_json::to_string(&command).unwrap(),
        7,
    );
    assert_eq!(
        response.status, 503,
        "入队失败却仍回 202，等于替一条没人接的命令作保: {}",
        response.body
    );
    assert!(
        response.body.contains("control_command_not_queued"),
        "503 必须点名是队列那一半没成，而不是笼统的提交失败: {}",
        response.body
    );
    assert_eq!(
        persisted.lock().unwrap().audit().len(),
        1,
        "受理那一半仍要落账：503 说的是补投待接，不是把命令一起退回"
    );

    // 对照：同一入口换成一写得进的队列，必须照常 202 且命令真的进了队列——判据不能是"永远 503"。
    let queued_ok = ControlCommand {
        command_id: 1202,
        request_id: "queue-working".into(),
        ..command
    };
    let response = service(false).handle(
        "POST",
        "/control/commands",
        &serde_json::to_string(&queued_ok).unwrap(),
        8,
    );
    assert_eq!(response.status, 202, "可入队的那一半不能顺路也变红");
    assert_eq!(
        queued.lock().unwrap().as_slice(),
        &[1202],
        "202 必须对应一次真实的入队，否则这一格只是把状态码换了个写法"
    );
    assert_eq!(persisted.lock().unwrap().audit().len(), 2);
}

#[test]
fn configured_api_policy_rejects_self_asserted_permission() {
    let service = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new().grant("ops", Permission::ReadOnly),
    );
    assert_eq!(service.handle("GET", "/metrics", "", 1).status, 403);
    let command = ControlCommand {
        command_id: 1,
        request_id: "api-secure-1".into(),
        operator_id: "ops".into(),
        reason: "attempt".into(),
        kind: CommandKind::PauseStrategy,
        target: "s1".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    };
    let body = serde_json::to_string(&command).unwrap();
    assert_eq!(
        service
            .handle_as("ops", "POST", "/control/commands", &body, 2)
            .status,
        403
    );
    assert!(service.state().lock().unwrap().control.audit().is_empty());
}

#[test]
fn protected_api_does_not_accept_operator_from_command_body() {
    let service = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new().grant("ops", Permission::Trading),
    );
    let command = ControlCommand {
        command_id: 1,
        request_id: "api-untrusted-1".into(),
        operator_id: "ops".into(),
        reason: "missing trusted identity".into(),
        kind: CommandKind::PauseStrategy,
        target: "s1".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    };
    let body = serde_json::to_string(&command).unwrap();
    assert_eq!(
        service.handle("POST", "/control/commands", &body, 2).status,
        403
    );
    assert!(service.state().lock().unwrap().control.audit().is_empty());
}

#[test]
fn trusted_api_identity_overrides_command_body_identity() {
    let service = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new().grant("ops", Permission::Trading),
    );
    let command = ControlCommand {
        command_id: 1,
        request_id: "api-trusted-1".into(),
        operator_id: "forged".into(),
        reason: "trusted boundary test".into(),
        kind: CommandKind::PauseStrategy,
        target: "s1".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    };
    let body = serde_json::to_string(&command).unwrap();
    assert_eq!(
        service
            .handle_as("ops", "POST", "/control/commands", &body, 2)
            .status,
        202
    );
    assert_eq!(
        service.state().lock().unwrap().control.audit()[0].operator_id,
        "ops"
    );
}

#[test]
fn api_rate_limit_is_deterministic_for_a_single_process() {
    let service = ApiService::new(ApiState::default()).with_rate_limit(1, 0);
    assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
    assert_eq!(service.handle("GET", "/health", "", 1).status, 429);
    assert_eq!(service.handle("GET", "/health", "", 2).status, 429);
}

#[test]
fn shared_file_rate_limit_is_visible_to_multiple_api_services() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-api-rate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let first = ApiService::new(ApiState::default())
        .with_shared_file_rate_limit(FileTokenBucket::new(&root, "api", 1, 0).unwrap());
    let second = ApiService::new(ApiState::default())
        .with_shared_file_rate_limit(FileTokenBucket::new(&root, "api", 1, 0).unwrap());
    assert_eq!(first.handle("GET", "/health", "", 1).status, 200);
    assert_eq!(second.handle("GET", "/health", "", 1).status, 429);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn event_bus_wakes_waiters_and_reports_retention_gaps() {
    let bus = ApiEventBus::new(1).unwrap();
    bus.publish(Event::new(0, 1, Priority::POST, EventKind::Settle))
        .unwrap();
    let waiter = bus.clone();
    let thread =
        std::thread::spawn(move || waiter.wait_after(Some(0), Duration::from_secs(1)).unwrap());
    bus.publish(Event::new(1, 2, Priority::POST, EventKind::Settle))
        .unwrap();
    assert_eq!(thread.join().unwrap().len(), 1);
    bus.publish(Event::new(2, 3, Priority::POST, EventKind::Settle))
        .unwrap();
    assert!(matches!(
        bus.read_after(Some(0)),
        Err(EventBusError::CursorTooOld { .. })
    ));
}

/// 幂等与漂移判据挂在**生产写路径**上：V11 F1 删掉全局兼容入口后，这条规则只剩
/// `project_account_event_log` 一处实现，用例也只剩一个去处可测。
#[test]
fn api_projection_is_idempotent_and_rejects_gaps_or_drift() {
    fn project(state: &mut ApiState, source: &EventLog) -> Result<usize, String> {
        state.project_account_event_log("account-a", "paper", source)
    }
    let mut source = EventLog::new();
    source
        .append_checked(Event::new(0, 1, Priority::MARKET, EventKind::Settle))
        .unwrap();
    source
        .append_checked(Event::new(1, 2, Priority::POST, EventKind::Settle))
        .unwrap();

    let mut state = ApiState::default();
    let key = ApiProjectionKey::new("account-a", "paper");
    assert_eq!(project(&mut state, &source).unwrap(), 2);
    assert_eq!(project(&mut state, &source).unwrap(), 0);
    assert_eq!(
        state
            .projections
            .get(&key)
            .map_or(0, |projection| projection.events.events().len()),
        2
    );

    let mut gap = EventLog::new();
    gap.append_checked(Event::new(0, 1, Priority::MARKET, EventKind::Settle))
        .unwrap();
    gap.append_checked(Event::new(1, 2, Priority::POST, EventKind::Settle))
        .unwrap();
    let mut changed = gap.clone();
    changed
        .append_checked(Event::new(2, 3, Priority::POST, EventKind::Settle))
        .unwrap();
    assert_eq!(project(&mut state, &changed).unwrap(), 1);

    let mut drift = EventLog::new();
    drift
        .append_checked(Event::new(0, 1, Priority::MARKET, EventKind::Settle))
        .unwrap();
    drift
        .append_checked(Event::new(1, 99, Priority::POST, EventKind::Settle))
        .unwrap();
    assert!(project(&mut state, &drift).is_err());
}

#[test]
fn account_projections_isolate_snapshots_events_and_cursors() {
    let mut state = ApiState::default();
    let mut paper = AccountSnapshot::new(1, "account-a", "portfolio-a", "paper", 10);
    paper.cash_raw.insert("USDT".into(), 100);
    let mut binance = AccountSnapshot::new(2, "account-b", "portfolio-b", "binance", 10);
    binance.cash_raw.insert("USDT".into(), 200);
    state
        .publish_snapshot_for("account-a", "paper", paper)
        .unwrap();
    state
        .publish_snapshot_for("account-b", "binance", binance)
        .unwrap();

    let mut paper_log = EventLog::new();
    paper_log
        .append_checked(Event::new(
            0,
            10,
            Priority::POST,
            EventKind::Timer {
                name: "paper".into(),
            },
        ))
        .unwrap();
    let mut binance_log = EventLog::new();
    binance_log
        .append_checked(Event::new(
            0,
            10,
            Priority::POST,
            EventKind::Timer {
                name: "binance".into(),
            },
        ))
        .unwrap();
    assert_eq!(
        state
            .project_account_event_log("account-a", "paper", &paper_log)
            .unwrap(),
        1
    );
    state
        .project_account_event_log("account-b", "binance", &binance_log)
        .unwrap();

    let service = ApiService::new(state);
    let paper_response = service.handle(
        "GET",
        "/account/snapshot?account_id=account-a&venue_id=paper",
        "",
        1,
    );
    let binance_response = service.handle(
        "GET",
        "/account/snapshot?account_id=account-b&venue_id=binance",
        "",
        1,
    );
    assert_eq!(paper_response.status, 200);
    assert_eq!(binance_response.status, 200);
    assert!(paper_response.body.contains("\"cash_raw\":{\"USDT\":100}"));
    assert!(binance_response
        .body
        .contains("\"cash_raw\":{\"USDT\":200}"));
    let envelope = service.handle(
        "GET",
        "/account/snapshot/envelope?account_id=account-a&venue_id=paper",
        "",
        2,
    );
    assert_eq!(envelope.status, 200);
    assert!(envelope.body.contains("\"kind\":\"account_snapshot\""));
    assert!(envelope.body.contains("\"source\":\"eventlog\""));
    let paper_events = service.handle("GET", "/events?account_id=account-a&venue_id=paper", "", 2);
    assert_eq!(paper_events.status, 200);
    assert!(paper_events.body.contains("paper"));
    assert!(!paper_events.body.contains("binance"));
    assert_eq!(
        service
            .handle("GET", "/account/snapshot?account_id=account-a", "", 3)
            .status,
        400
    );
}

#[test]
fn account_projection_rejects_identity_drift_and_marks_readiness_stale() {
    let mut source = EventLog::new();
    source
        .append_checked(Event::new(
            0,
            1,
            Priority::FEEDBACK,
            EventKind::AccountBalanceSnapshot {
                account_id: "other-account".into(),
                venue_id: "paper".into(),
                balances: Vec::new(),
            },
        ))
        .unwrap();
    let mut state = ApiState::default();
    assert!(state
        .project_account_event_log("account-a", "paper", &source)
        .is_err());
    let health = state
        .projection_health("account-a", "paper")
        .expect("failed projection keeps health state");
    assert!(!health.healthy);
    assert!(health.error.unwrap().contains("身份"));
    let service = ApiService::new(state);
    let ready = service.handle("GET", "/ready", "", 1);
    assert_eq!(ready.status, 503);
    assert!(ready.body.contains("projection_stale"));
}

/// `/ready` 只在"读模型里确实有账户投影、却已经没人刷新"时按刷新者降级（V11 I1）。
///
/// 反向也要钉住：一份没有账户域的拓扑本来就没有第二份写入者，把它念成断链是假告警。
#[test]
fn readiness_blames_the_refresher_only_when_the_read_model_holds_projections() {
    let mut source = EventLog::new();
    source
        .append_checked(Event::new(
            0,
            1,
            Priority::FEEDBACK,
            EventKind::AccountBalanceSnapshot {
                account_id: "account-a".into(),
                venue_id: "paper".into(),
                balances: Vec::new(),
            },
        ))
        .unwrap();
    let mut loaded = ApiState::default();
    loaded
        .project_account_event_log("account-a", "paper", &source)
        .unwrap();
    let reason = ProjectionRefresher::Stopped("测试：EventLog 存储初始化失败".into());
    loaded.projection_refresher = reason.clone();
    let ready = ApiService::new(loaded).handle("GET", "/ready", "", 1);
    assert_eq!(
        ready.status, 503,
        "读模型停在最后一轮却报 Ready：{}",
        ready.body
    );
    assert!(ready.body.contains("projection_refresher_stopped"));
    assert!(
        ready.body.contains("EventLog 存储初始化失败"),
        "原因要跟着一起报出来，否则运维只知道要重启而不知道为什么：{}",
        ready.body
    );

    let bare = ApiState {
        projection_refresher: reason,
        ..Default::default()
    };
    let ready = ApiService::new(bare).handle("GET", "/ready", "", 2);
    assert_eq!(
        ready.status, 200,
        "没有投影可刷新时不作判定：那是没有账户域的拓扑，不是断链。{}",
        ready.body
    );
    // 从没报过状态的服务（进程内装配、只读工具）同样不能被判成停摆。
    let untouched = ApiState::default();
    assert_eq!(
        untouched.projection_refresher,
        ProjectionRefresher::Unreported,
        "默认值必须是「没谈过刷新者」，否则每个不带桥的装配都会一启动就自判 503"
    );
}

#[test]
fn live_event_route_uses_the_realtime_cursor_contract() {
    let mut state = ApiState::default();
    let mut source = EventLog::new();
    source
        .append_checked(Event::new(0, 1, Priority::POST, EventKind::Settle))
        .unwrap();
    state
        .project_account_event_log("account-a", "paper", &source)
        .unwrap();
    let service = ApiService::new(state);
    let scoped = "account_id=account-a&venue_id=paper";
    assert_eq!(
        service
            .handle("GET", &format!("/events/live?{scoped}&after=1"), "", 1)
            .status,
        409
    );
    let response = service.handle("GET", &format!("/events/live?{scoped}"), "", 1);
    assert_eq!(response.status, 200);
    assert!(response.body.contains("\"schema_version\":1"));
    assert!(response.body.contains("\"cursor\":\"0:"));
    assert!(response.body.contains("Settle"));
    // 缺键不再退回"某个默认账户"或那份空的全局事件面（V11 F1）。
    assert_eq!(
        service.handle("GET", "/events/live", "", 1).status,
        400,
        "无键的实时事件读必须说不清，而不是回一条空 200"
    );
}

#[test]
fn mtls_identity_policy_maps_exact_certificate_der() {
    let certificate = CertificateDer::from(vec![1, 2, 3, 4]);
    let policy = MtlsIdentityPolicy::new()
        .grant_certificate(certificate.clone(), "ops")
        .unwrap();
    assert_eq!(
        policy.operator_for(Some(std::slice::from_ref(&certificate))),
        Some("ops")
    );
    let other = CertificateDer::from(vec![1, 2, 3, 5]);
    assert_eq!(
        policy.operator_for(Some(std::slice::from_ref(&other))),
        None
    );
}

#[test]
fn mtls_identity_store_replaces_operator_mapping_atomically() {
    let first_certificate = CertificateDer::from(vec![9, 8, 7]);
    let second_certificate = CertificateDer::from(vec![6, 5, 4]);
    let first = MtlsIdentityPolicy::new()
        .grant_certificate(first_certificate.clone(), "ops-old")
        .unwrap();
    let second = MtlsIdentityPolicy::new()
        .grant_certificate(second_certificate.clone(), "ops-new")
        .unwrap();
    let store = MtlsIdentityStore::new(first);
    assert_eq!(
        store
            .current()
            .operator_for(Some(std::slice::from_ref(&first_certificate))),
        Some("ops-old")
    );
    store.replace(second);
    let current = store.current();
    assert_eq!(
        current.operator_for(Some(std::slice::from_ref(&second_certificate))),
        Some("ops-new")
    );
    assert_eq!(
        current.operator_for(Some(std::slice::from_ref(&first_certificate))),
        None
    );
}

#[test]
fn tls_config_store_exposes_rotation_boundary() {
    let first = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(NoCertificateResolver)),
    );
    let second = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(NoCertificateResolver)),
    );
    let store = TlsConfigStore::new(Arc::clone(&first));
    assert!(Arc::ptr_eq(&store.current(), &first));
    store.replace(Arc::clone(&second));
    assert!(Arc::ptr_eq(&store.current(), &second));
}

#[test]
fn snapshot_diff_and_event_cursor_require_a_valid_base() {
    let mut state = ApiState::default();
    let mut base = AccountSnapshot::new(1, "a", "p", "paper", 1);
    base.cash_raw.insert("USD".into(), 100);
    let base_hash = state.publish_snapshot(base).unwrap();
    let mut target = AccountSnapshot::new(2, "a", "p", "paper", 2);
    target.cash_raw.insert("USD".into(), 120);
    state.publish_snapshot(target).unwrap();
    let mut source = EventLog::new();
    source
        .append_checked(Event::new(0, 2, Priority::POST, EventKind::Settle))
        .unwrap();
    source
        .append_checked(Event::new(1, 3, Priority::POST, EventKind::Settle))
        .unwrap();
    state
        .project_account_event_log("a", "paper", &source)
        .unwrap();
    let service = ApiService::new(state);
    let diff = service.handle(
        "GET",
        &format!("/account/snapshot/diff?base_hash={base_hash}"),
        "",
        3,
    );
    assert_eq!(diff.status, 200);
    assert!(diff.body.contains("base_state_hash"));
    let events = service.handle("GET", "/events?after=0&account_id=a&venue_id=paper", "", 3);
    assert_eq!(events.status, 200);
    assert!(events.body.contains("Settle"));
    // 缺账户键的事件读必须 400：过去它读那份没有生产写入者的全局事件面，永远 200 空表。
    assert_eq!(
        service.handle("GET", "/events?after=0", "", 3).status,
        400,
        "无键事件读必须说不清"
    );
    assert_eq!(
        service
            .handle("GET", "/account/snapshot/diff?base_hash=999", "", 3)
            .status,
        409
    );
}

#[test]
fn http_server_serves_health_route() {
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || service.serve_once(&listener, 1));
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    worker.join().unwrap().unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("{\"status\":\"ok\"}"));
}

#[test]
fn tls_server_rejects_plaintext_before_http_dispatch() {
    let service = ApiService::new(ApiState::default());
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(NoCertificateResolver));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || service.serve_once_tls(&listener, Arc::new(config), 1));
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let result = worker.join().unwrap();
    assert!(result.is_err());
}

/// 端到端咬住 `/stream` 的**生产**数据源：种子与推送都走 `project_account_event_log`。
/// 此前它用零生产调用的 `publish_event` 喂数据，于是"用例绿"与"生产里这条流永不推送"
/// 同时成立（V11 F1）。
#[test]
fn websocket_server_sends_connection_and_event_batches() {
    let mut state = ApiState::default();
    let mut seed = EventLog::new();
    seed.append_checked(Event::new(0, 1, Priority::POST, EventKind::Settle))
        .unwrap();
    state
        .project_account_event_log("account-a", "paper", &seed)
        .unwrap();
    let service = ApiService::new(state);
    let publisher = service.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || service.serve_once(&listener, 1));
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(
            b"GET /stream?account_id=account-a&venue_id=paper HTTP/1.1\r\nHost: localhost\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut response = Vec::new();
    loop {
        let mut chunk = [0_u8; 8192];
        let count = client.read(&mut chunk).unwrap();
        assert!(count > 0);
        response.extend_from_slice(&chunk[..count]);
        if String::from_utf8_lossy(&response).contains("events") {
            break;
        }
    }
    let mut next = EventLog::new();
    next.append_checked(Event::new(0, 1, Priority::POST, EventKind::Settle))
        .unwrap();
    next.append_checked(Event::new(1, 2, Priority::POST, EventKind::Settle))
        .unwrap();
    publisher
        .project_account_event_log("account-a", "paper", &next)
        .unwrap();
    let mut pushed = Vec::new();
    loop {
        let mut chunk = [0_u8; 8192];
        let count = client.read(&mut chunk).unwrap();
        assert!(count > 0);
        pushed.extend_from_slice(&chunk[..count]);
        if String::from_utf8_lossy(&pushed).contains("\"type\":\"event\"") {
            break;
        }
    }
    client.write_all(&[0x88, 0x80, 0, 0, 0, 0]).unwrap();
    client.shutdown(Shutdown::Both).unwrap();
    worker.join().unwrap().unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 101 Switching Protocols"));
    assert!(response.contains("qianxing"));
    assert!(response.contains("events"));
}

/// 连上一条 `/stream` 并读到握手后的第一帧：这一刻服务端的连接线程一定停在事件循环里，
/// 于是这一格长连接名额被实实在在占住——上界的判据要有一条真占着门的连接才数得清。
fn hold_open_websocket(address: std::net::SocketAddr) -> TcpStream {
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(
            b"GET /stream?account_id=account-a&venue_id=paper HTTP/1.1\r\nHost: localhost\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut seen = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let count = client.read(&mut chunk).unwrap();
        assert!(count > 0, "升级之后服务端一帧都没吐");
        seen.extend_from_slice(&chunk[..count]);
        // 读到 `connected` 帧才算把首帧吃干净：后面的"这条流还开着"那半才不被残留字节骗过。
        if String::from_utf8_lossy(&seen).contains("qianxing") {
            break;
        }
    }
    client
}

/// 长连接上界（V11 R7-e）：接一条占一格，占满就当场 503 而不是再开一颗线程；连接收尾时那一格
/// 要能回来，否则上界会变成"一次之后永远 503"。用例把上界调到 1，因为生产的 64 格要 64 颗线程
/// 才撞得到门——那是夹具的代价，不是判据。
#[test]
fn live_connection_ceiling_refuses_and_gives_the_slot_back() {
    let service = ApiService::new(ApiState::default()).with_max_live_connections(1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let server = service.clone();
    let worker = std::thread::spawn(move || {
        server.serve(listener, 1, move || stopping.load(Ordering::Acquire))
    });

    let holder = hold_open_websocket(address);
    let mut refused = TcpStream::connect(address).unwrap();
    refused
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    refused
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut refusal = String::new();
    refused.read_to_string(&mut refusal).unwrap();
    // 首行连原因短语一起钉：503 此前不在状态原因表里，线上印成 "Internal Server Error"。
    assert!(
        refusal.starts_with("HTTP/1.1 503 Service Unavailable"),
        "名额满时没回 503：{refusal}"
    );
    assert_eq!(service.metrics().connections_rejected_total, 1);

    drop(holder);
    let mut accepted = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let mut probe = TcpStream::connect(address).unwrap();
        probe
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        probe
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut head = String::new();
        if probe.read_to_string(&mut head).is_ok() && head.starts_with("HTTP/1.1 200") {
            accepted = Some(head);
            break;
        }
    }
    assert!(accepted.is_some(), "第一条收尾后名额没有回来：{refusal}");
    stop.store(true, Ordering::Release);
    worker.join().unwrap().unwrap();
}

/// 停机令牌对**正在场**的长连接同样生效（V11 R7-e）：过去只有 accept 循环读它，一条静默不关
/// （半开、不发 FIN）的流会一直占到进程退出，并把一格名额一起占死。这里客户端一帧 close 都不发。
#[test]
fn websocket_stream_terminates_on_the_stop_token_without_client_close() {
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let server = service.clone();
    let worker = std::thread::spawn(move || {
        server.serve(listener, 1, move || stopping.load(Ordering::Acquire))
    });
    let mut client = hold_open_websocket(address);
    client
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut idle = [0_u8; 16];
    let before = client.read(&mut idle);
    let idle_kind = before.as_ref().err().map(|error| error.kind());
    assert!(
        matches!(
            idle_kind,
            Some(std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
        ),
        "令牌还没置起这条流就先收了（读到的却是 {before:?}），下面那半就没有判据了"
    );

    stop.store(true, Ordering::Release);
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut closed = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match client.read(&mut idle) {
            // 服务端 return 之后连接线程结束、socket 关闭：读侧看到的是 EOF。
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(_) => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => panic!("读停机后的流失败：{error}"),
        }
    }
    assert!(closed, "停机令牌置起后，服务端仍然没有收掉这条 WebSocket");
    worker.join().unwrap().unwrap();
}

/// 逐字节挤的请求。`gap` 决定每轮 `read` 的耗时，`trailing` 决定"头部之后还愿意发
/// 多少字节而不关闭连接"，因此"没认出头部"与"认出了头部"会走出完全不同的形状。
struct Dribbled {
    bytes: &'static [u8],
    position: usize,
    trailing: usize,
    gap: Duration,
}

impl Dribbled {
    fn new(bytes: &'static [u8], trailing: usize, gap: Duration) -> Self {
        Self {
            bytes,
            position: 0,
            trailing,
            gap,
        }
    }
}

impl Read for Dribbled {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.position == self.bytes.len() && self.trailing == 0 {
            return Ok(0);
        }
        std::thread::sleep(self.gap);
        if self.position < self.bytes.len() {
            buffer[0] = self.bytes[self.position];
            self.position += 1;
        } else {
            self.trailing -= 1;
            buffer[0] = b' ';
        }
        Ok(1)
    }
}

#[test]
fn a_dribbling_client_hits_the_overall_budget_not_the_size_cap() {
    // socket 的 100 ms 读超时只界住**单次** `read`：每个字节都"按时到达"的客户端从不
    // 触发它，而 1 MiB 的体积上限要 29 小时才挡得住。这里每字节 2 ms、整体截止 5 ms，
    // 没有截止就要一路读到 EOF 才返回（V11 R4-9，与 V11 O7 的握手块同一判据）。
    let mut stream = Dribbled::new(b"", 200, Duration::from_millis(2));
    let error = read_request(&mut stream, Duration::from_millis(5))
        .expect_err("永不到来的请求必须在整体截止处收掉，而不是读到 EOF");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::TimedOut,
        "要说是截止挡下的，不是读取失败: {error}"
    );
    assert!(
        error.to_string().contains("budget"),
        "错误要交代清楚是整体截止: {error}"
    );
    // 正向对照：截止之内到达的完整请求照常收下。
    let request = b"GET /health HTTP/1.1\r\n\r\n";
    let mut complete = std::io::Cursor::new(request.to_vec());
    assert_eq!(
        read_request(&mut complete, Duration::from_secs(5))
            .expect("截止之内的完整请求不该被拒")
            .as_slice(),
        request.as_slice()
    );
}

#[test]
fn a_header_split_across_reads_ends_the_request_at_its_own_length() {
    // 扫描改成只看新到字节之后，游标必须回看 3 字节：从新字节起头扫就永远认不出横跨
    // 两轮的 `\r\n\r\n`，于是这条连接会把后续字节一路读到底才收尾。
    let request = b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
    let mut stream = Dribbled::new(request, 4_096, Duration::ZERO);
    let read =
        read_request(&mut stream, Duration::from_secs(5)).expect("逐字节到达的完整请求应当被认出");
    assert_eq!(
        read.as_slice(),
        request.as_slice(),
        "只该收到请求本身，多收一个字节就说明头部没被认出来"
    );
}
