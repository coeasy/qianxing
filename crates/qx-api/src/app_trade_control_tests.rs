use crate::*;
use qx_control::{CommandKind, ControlAuditRecordV1, PENDING_CONTROL_COMMAND_LIMIT};
use std::collections::BTreeMap;

#[path = "app_multi_leg_tests.rs"]
mod app_multi_leg_tests;
#[path = "app_multi_market_tests.rs"]
mod app_multi_market_tests;

#[path = "app_strategy_selection_tests.rs"]
mod app_strategy_selection_tests;

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
        let receipt: ControlAuditRecordV1 = serde_json::from_str(&response.body).unwrap();
        assert_eq!(receipt.schema_version, 1);
        assert_eq!(receipt.operator_id, "ops");
        assert_eq!(receipt.status, qx_control::CommandStatus::Accepted);
    }
    let audit = service.handle_as("ops", "GET", "/control/audit", "", 703);
    assert_eq!(audit.status, 200);
    let audit: qx_control::ControlAuditPageV1 = serde_json::from_str(&audit.body).unwrap();
    assert_eq!(audit.entries.len(), 2);
    assert!(audit
        .entries
        .iter()
        .all(|entry| entry.record.schema_version == 1));
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

#[test]
fn paper_and_live_app_routes_return_capacity_error_when_control_plane_is_full() {
    let service = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new().grant("ops", Permission::Trading),
    );
    let state = service.state();
    {
        let mut state = state.lock().unwrap();
        for id in 1..=PENDING_CONTROL_COMMAND_LIMIT as u64 {
            state
                .control
                .submit(
                    ControlCommand {
                        command_id: id,
                        request_id: format!("capacity-fill-{id}"),
                        operator_id: "ops".into(),
                        reason: "fill pending control capacity".into(),
                        kind: CommandKind::PauseStrategy,
                        target: format!("strategy-{id}"),
                        payload: BTreeMap::new(),
                        permission: Permission::Trading,
                        dry_run: false,
                    },
                    id,
                )
                .unwrap();
        }
    }

    for (workflow, route, command_id) in [
        ("paper", "/app/paper/commands", 10_001),
        ("live", "/app/live/commands", 10_002),
    ] {
        let command = ControlCommand {
            command_id,
            request_id: format!("capacity-{workflow}-{command_id}"),
            operator_id: "forged-body-operator".into(),
            reason: "capacity rejection integration".into(),
            kind: CommandKind::PauseStrategy,
            target: "strategy-capacity".into(),
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
        assert_eq!(response.status, 503, "{workflow}: {}", response.body);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response.body).unwrap()["error"],
            "control_command_capacity_exceeded",
            "{workflow}: {}",
            response.body
        );
    }
}
