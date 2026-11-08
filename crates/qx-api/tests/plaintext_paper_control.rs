use qx_api::{ApiService, ApiState};
use qx_control::{AuditRecord, CommandKind, ControlCommand, Permission};
use std::collections::BTreeMap;

fn command(id: u64, permission: Permission) -> ControlCommand {
    ControlCommand {
        command_id: id,
        request_id: format!("local-paper-{id}"),
        operator_id: "body-operator-must-not-be-trusted".into(),
        reason: "paper control boundary test".into(),
        kind: CommandKind::PauseStrategy,
        target: "strategy-1".into(),
        payload: BTreeMap::new(),
        permission,
        dry_run: false,
    }
}

fn workflow_body(workflow: &str, command: ControlCommand) -> String {
    serde_json::json!({
        "schema_version": 1,
        "workflow": workflow,
        "command": command,
    })
    .to_string()
}

#[test]
fn local_paper_control_uses_server_grant_and_identity() {
    let service = ApiService::new(ApiState::default()).with_local_paper_control();
    let elevated = workflow_body("paper", command(1, Permission::Admin));
    assert_eq!(
        service
            .handle("POST", "/app/paper/commands", &elevated, 1)
            .status,
        403,
        "the request body cannot grant Admin"
    );

    let paper = workflow_body("paper", command(2, Permission::Trading));
    let response = service.handle("POST", "/app/paper/commands", &paper, 2);
    assert_eq!(response.status, 202, "Paper workflow remains available");
    let receipt: AuditRecord = serde_json::from_str(&response.body).unwrap();
    assert_eq!(receipt.operator_id, "local-paper");
}

#[test]
fn local_paper_control_rejects_generic_control_and_live_workflow() {
    let service = ApiService::new(ApiState::default()).with_local_paper_control();
    let raw = serde_json::to_string(&command(3, Permission::Admin)).unwrap();
    assert_eq!(
        service.handle("POST", "/control/commands", &raw, 3).status,
        403
    );

    let live = workflow_body("live", command(4, Permission::Trading));
    assert_eq!(
        service
            .handle("POST", "/app/live/commands", &live, 4)
            .status,
        403
    );
    assert!(service.state().lock().unwrap().control.audit().is_empty());
}
