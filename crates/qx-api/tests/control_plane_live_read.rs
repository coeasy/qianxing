//! 控制面现读的两条运维读面（`/control/audit` 与 `/metrics` 的控制面指标）必须读 store 现值，
//! 而不是装配时装进进程的那份启动期副本。
//!
//! worker 进程经 `control_store.transact(execute)` 落盘的命令终态与随之发生的退场不会回流到
//! API 进程，所以不装 provider 时这两条读面给的是滞后上界而非实时值（V13 R8）。反向验证
//! 依据写在断言里：启动期副本是空的，装上 provider 之后必须当场读得到那条只存在于
//! store 里的命令；store 读不到时必须回 503，而不是拿空副本假装控制面是空的。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use qx_api::{ApiService, ApiState};
use qx_control::{
    CommandKind, ControlCommand, ControlPlane, Permission, PENDING_CONTROL_COMMAND_LIMIT,
};

fn audit_records(body: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["record"].clone())
        .collect()
}

fn worker_persisted_command() -> ControlCommand {
    ControlCommand {
        command_id: 7,
        request_id: "worker +/7".into(),
        operator_id: "qx-worker".into(),
        reason: "worker persisted the command".into(),
        kind: CommandKind::PauseStrategy,
        target: "strategy-7".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    }
}

fn numbered_command(id: u64) -> ControlCommand {
    ControlCommand {
        command_id: id,
        request_id: format!("request-{id}"),
        operator_id: "qx-worker".into(),
        reason: "page contract probe".into(),
        kind: CommandKind::PauseStrategy,
        target: format!("strategy-{id}"),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    }
}

#[test]
fn control_plane_live_read_wins_over_the_startup_copy() {
    let store = Arc::new(Mutex::new(ControlPlane::default()));
    {
        let mut plane = store.lock().unwrap();
        plane.submit(worker_persisted_command(), 900).unwrap();
    }

    // 启动期副本是空的：不装 provider 时只能读到这份空。
    let boot_only = ApiService::new(ApiState::default());
    let boot_audit = boot_only.handle("GET", "/control/audit", "", 1);
    assert_eq!(boot_audit.status, 200);
    assert!(
        !boot_audit.body.contains("\"command_id\":7"),
        "启动期副本里没有 worker 落盘的那条命令: {}",
        boot_audit.body
    );

    let live_store = Arc::clone(&store);
    let live = ApiService::new(ApiState::default())
        .with_control_plane_provider(move || Ok(live_store.lock().unwrap().clone()));
    let audit = live.handle("GET", "/control/audit", "", 1);
    assert_eq!(audit.status, 200);
    assert!(
        audit.body.contains("\"command_id\":7"),
        "/control/audit 必须读 store 现值，而不是启动期那份空副本: {}",
        audit.body
    );
    let filtered = live.handle("GET", "/control/audit?request_id=worker+%2B%2F7", "", 1);
    let filtered = audit_records(&filtered.body);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0]["request_id"], "worker +/7");
    let missing = live.handle("GET", "/control/audit?request_id=absent", "", 1);
    assert!(audit_records(&missing.body).is_empty());

    let metrics = live.handle("GET", "/metrics", "", 2);
    assert_eq!(metrics.status, 200);
    assert!(
        metrics.body.contains("qx_control_retired_commands_total"),
        "控制面计数是 scrape 的运维读面: {}",
        metrics.body
    );
}

#[test]
fn control_plane_read_failure_reports_unavailable_not_an_empty_control_plane() {
    let failing = ApiService::new(ApiState::default())
        .with_control_plane_provider(|| Err("control store unavailable".into()));

    let audit = failing.handle("GET", "/control/audit", "", 1);
    assert_eq!(audit.status, 503);
    assert!(
        audit.body.contains("control store unavailable"),
        "读失败要印出原因，不能只给一个码: {}",
        audit.body
    );

    let metrics = failing.handle("GET", "/metrics", "", 2);
    assert_eq!(metrics.status, 503);
    assert!(
        metrics.body.contains("control store unavailable"),
        "scrape 同样要明确报 503: {}",
        metrics.body
    );
}

#[test]
fn control_command_capacity_is_rejected_and_reported() {
    let mut state = ApiState::default();
    for id in 1..=PENDING_CONTROL_COMMAND_LIMIT as u64 {
        state.control.submit(numbered_command(id), id).unwrap();
    }
    let service = ApiService::new(state);
    let rejected = service.handle(
        "POST",
        "/control/commands",
        &serde_json::to_string(&numbered_command(PENDING_CONTROL_COMMAND_LIMIT as u64 + 1))
            .unwrap(),
        10,
    );
    assert_eq!(rejected.status, 503);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rejected.body).unwrap()["error"],
        "control_command_capacity_exceeded"
    );

    let metrics = service.handle("GET", "/metrics", "", 11);
    assert_eq!(metrics.status, 200);
    assert!(metrics.body.contains("qx_control_pending_commands 4096\n"));
    assert!(metrics
        .body
        .contains("qx_control_pending_commands_limit 4096\n"));
    assert!(metrics
        .body
        .contains("qx_api_control_capacity_rejections_total 1\n"));
}

#[test]
fn control_audit_http_pages_are_bounded_snapshot_consistent_and_restart_safe() {
    let store = Arc::new(Mutex::new(ControlPlane::default()));
    {
        let mut plane = store.lock().unwrap();
        for id in 1..=3 {
            plane.submit(numbered_command(id), id * 10).unwrap();
        }
    }
    let live_store = Arc::clone(&store);
    let service = ApiService::new(ApiState::default())
        .with_control_plane_provider(move || Ok(live_store.lock().unwrap().clone()));

    let first = service.handle("GET", "/control/audit?limit=2", "", 1);
    assert_eq!(first.status, 200, "{}", first.body);
    let first: qx_control::ControlAuditPageV1 = serde_json::from_str(&first.body).unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    assert!(first.has_more);
    let cursor = first.next_cursor.unwrap();

    // A new API store image after the first request keeps the old upper bound in its cursor.
    {
        let mut plane = store.lock().unwrap();
        plane.submit(numbered_command(4), 40).unwrap();
        *plane = ControlPlane::from_json(&plane.to_json().unwrap()).unwrap();
    }
    let continued = service.handle(
        "GET",
        &format!("/control/audit?limit=2&cursor={cursor}"),
        "",
        2,
    );
    assert_eq!(continued.status, 200, "{}", continued.body);
    let continued: qx_control::ControlAuditPageV1 = serde_json::from_str(&continued.body).unwrap();
    assert_eq!(
        continued
            .entries
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(continued.snapshot_through_sequence, 3);
    assert!(!continued.has_more);

    let expired = service.handle("GET", "/control/audit?cursor=v1.1.3", "", 3);
    assert_eq!(expired.status, 409, "{}", expired.body);
    assert!(expired.body.contains("control_audit_cursor_expired"));

    let filtered = service.handle("GET", "/control/audit?limit=2&request_id=request-2", "", 4);
    let filtered: qx_control::ControlAuditPageV1 = serde_json::from_str(&filtered.body).unwrap();
    assert_eq!(filtered.entries.len(), 1);
    assert_eq!(filtered.entries[0].record.request_id, "request-2");

    let mut huge_identity = numbered_command(5);
    huge_identity.request_id = "x".repeat(qx_control::CONTROL_AUDIT_MAX_PAGE_BYTES + 1);
    store.lock().unwrap().submit(huge_identity, 50).unwrap();
    let oversized = service.handle("GET", "/control/audit?limit=2", "", 5);
    assert_eq!(oversized.status, 413, "{}", oversized.body);
    assert!(oversized
        .body
        .contains("control_audit_record_exceeds_response_budget"));
}
