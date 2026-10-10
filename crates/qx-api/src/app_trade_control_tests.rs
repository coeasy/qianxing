use crate::*;
use qx_control::CommandKind;
use std::collections::BTreeMap;

#[path = "app_multi_leg_tests.rs"]
mod app_multi_leg_tests;

#[test]
fn paper_and_live_app_routes_validate_then_enqueue_through_the_existing_control_plane() {
    let service = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new().grant("ops", Permission::Trading),
    );
    for (workflow, route, command_id) in [
        ("paper", "/app/paper/commands", 701),
        ("live", "/app/live/commands", 702),
    ] {
        let command = ControlCommand {
            command_id,
            request_id: format!("app-{workflow}-{command_id}"),
            operator_id: "forged-body-operator".into(),
            reason: "workflow API integration".into(),
            kind: CommandKind::PauseStrategy,
            target: "strategy-1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: false,
        };
        let body = serde_json::json!({
            "schema_version": 1,
            "workflow": workflow,
            "command": command,
        })
        .to_string();
        let response = service.handle_as("ops", "POST", route, &body, command_id);
        assert_eq!(response.status, 202, "{workflow}: {}", response.body);
        let receipt: AuditRecord = serde_json::from_str(&response.body).unwrap();
        assert_eq!(receipt.operator_id, "ops");
        assert_eq!(receipt.status, qx_control::CommandStatus::Accepted);
    }
    let state = service.state();
    let state = state.lock().unwrap();
    assert_eq!(state.control.audit().len(), 2);
    assert_eq!(state.control.pending().count(), 2);
}

#[test]
fn live_app_control_fails_closed_without_authenticated_operator_policy() {
    let service = ApiService::new(ApiState::default());
    let command = ControlCommand {
        command_id: 703,
        request_id: "app-live-unprotected".into(),
        operator_id: "body-operator".into(),
        reason: "must fail closed".into(),
        kind: CommandKind::PauseStrategy,
        target: "strategy-1".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: false,
    };
    let body = serde_json::json!({
        "schema_version": 1,
        "workflow": "live",
        "command": command,
    })
    .to_string();
    assert_eq!(
        service
            .handle("POST", "/app/live/commands", &body, 1)
            .status,
        403
    );
    assert!(service.state().lock().unwrap().control.audit().is_empty());
}
