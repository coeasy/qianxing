//! 控制面现读的两条运维读面（`/control/audit` 与 `/metrics` 的退场计数）必须读 store 现值，
//! 而不是装配时装进进程的那份启动期副本。
//!
//! worker 进程经 `control_store.transact(execute)` 落盘的命令终态与随之发生的退场不会回流到
//! API 进程，所以不装 provider 时这两条读面给的是滞后上界而非实时值（V13 R8）。反向验证
//! 依据写在断言里：启动期副本是空的，装上 provider 之后必须当场读得到那条只存在于
//! store 里的命令；store 读不到时必须回 503，而不是拿空副本假装控制面是空的。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use qx_api::{ApiService, ApiState};
use qx_control::{CommandKind, ControlCommand, ControlPlane, Permission};

fn worker_persisted_command() -> ControlCommand {
    ControlCommand {
        command_id: 7,
        request_id: "worker-7".into(),
        operator_id: "qx-worker".into(),
        reason: "worker persisted the command".into(),
        kind: CommandKind::PauseStrategy,
        target: "strategy-7".into(),
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

    let metrics = live.handle("GET", "/metrics", "", 2);
    assert_eq!(metrics.status, 200);
    assert!(
        metrics.body.contains("qx_control_retired_commands_total"),
        "退场计数是 scrape 的唯一运维读面: {}",
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
