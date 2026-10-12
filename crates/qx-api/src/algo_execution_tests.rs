use super::*;
use qx_control::CommandKind;

#[test]
fn control_and_query_routes_are_audited() {
    let service = ApiService::new(ApiState::default());
    assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
    let metrics = service.handle("GET", "/metrics", "", 1);
    assert_eq!(metrics.status, 200);
    let samples = metrics
        .body
        .lines()
        .filter(|line| !line.starts_with("# "))
        .count();
    // 4 条 `qx_api_*` 自身计数 + 2 条 `qx_control_retired_*` 退场计数：
    // 退场计数少了读者，「内存工作集有界」就只是写法，运维面看不见它有没有真的发生。
    assert_eq!(samples, 6, "自身样本行:\n{}", metrics.body);
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
fn paper_algo_lifecycle_routes_delegate_and_enforce_trading_for_mutations() {
    let service = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new()
            .grant("reader", Permission::ReadOnly)
            .grant("trader", Permission::Trading),
    )
    .with_algo_execution_provider(|operation, body| {
        Ok(serde_json::json!({"operation": operation, "body": body}).to_string())
    });
    assert_eq!(
        service
            .handle_as(
                "reader",
                "POST",
                "/app/paper/algo-executions/start",
                "{}",
                1
            )
            .status,
        403
    );
    let read = service.handle_as("reader", "POST", "/app/paper/algo-executions/get", "{}", 2);
    assert_eq!(read.status, 200);
    assert!(read.body.contains("\"operation\":\"get\""));
    let start = service.handle_as(
        "trader",
        "POST",
        "/app/paper/algo-executions/start",
        "{}",
        3,
    );
    assert_eq!(start.status, 200);
    assert!(start.body.contains("\"operation\":\"start\""));
}
