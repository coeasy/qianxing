//! Shared control-plane lifecycle for explicit one-shot SubmitOrder commands.

use crate::configured_backends::ControlStateBackend;
use qx_control::{AuditRecord, CommandStatus, ControlCommand, Permission};

#[derive(Clone, Debug)]
pub struct OneShotSubmitOutcome {
    pub accepted: AuditRecord,
    pub terminal: AuditRecord,
}

/// Persist Accepted, run the supplied venue action, then persist exactly one terminal decision.
///
/// A failed venue action is data for the terminal audit record, not an early return. This keeps
/// command parsing and connector construction at the facade while the service owns the shared
/// state transition used by Paper, Binance, and CCXT one-shot entrypoints.
pub fn run_one_shot_submit<F>(
    store: &ControlStateBackend,
    command: ControlCommand,
    now: u64,
    execute_action: F,
) -> Result<OneShotSubmitOutcome, String>
where
    F: FnOnce() -> Result<String, String>,
{
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 SubmitOrder Accepted 失败: {error}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("SubmitOrder 未通过控制面权限/幂等校验: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".to_string())
    } else {
        execute_action()
    };

    let (_, terminal_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 SubmitOrder 终态失败: {error}"))?;
    let terminal =
        terminal_result.map_err(|error| format!("执行 SubmitOrder 控制命令失败: {error:?}"))?;

    Ok(OneShotSubmitOutcome { accepted, terminal })
}

impl OneShotSubmitOutcome {
    pub fn into_result_code(self) -> Result<String, String> {
        if self.terminal.status == CommandStatus::Failed {
            Err(self.terminal.result_code)
        } else {
            Ok(self.terminal.result_code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run_one_shot_submit;
    use crate::configured_backends::ControlStateBackend;
    use qx_control::{CommandKind, CommandStatus, ControlCommand, Permission};
    use qx_storage::JsonStateStore;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn store() -> (ControlStateBackend, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        // Keep test scratch under the workspace target tree. Some Windows hosts deny direct
        // writes beneath the configured system Temp directory while permitting repository I/O.
        let root = std::env::current_dir()
            .expect("workspace current directory")
            .join("target")
            .join("qx-service-test-tmp")
            .join(format!("qx-one-shot-submit-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create isolated control root");
        (
            ControlStateBackend::Files(JsonStateStore::new(root.join("control"))),
            root,
        )
    }

    fn command(dry_run: bool) -> ControlCommand {
        ControlCommand {
            command_id: 17,
            request_id: "request-17".into(),
            operator_id: "operator".into(),
            reason: "test one-shot terminal state".into(),
            kind: CommandKind::SubmitOrder,
            target: "account-a".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run,
        }
    }

    #[test]
    fn failed_action_is_persisted_as_terminal_and_not_left_pending() {
        let (store, root) = store();
        let outcome = run_one_shot_submit(&store, command(false), 100, || {
            Err("venue rejected order".into())
        })
        .expect("failed action still returns its terminal audit record");

        assert_eq!(outcome.accepted.status, CommandStatus::Accepted);
        assert_eq!(outcome.terminal.status, CommandStatus::Failed);
        assert_eq!(outcome.terminal.result_code, "venue rejected order");
        assert_eq!(
            store.load().expect("load control state").pending().count(),
            0
        );
        std::fs::remove_dir_all(root).expect("remove isolated control root");
    }

    #[test]
    fn dry_run_is_terminalized_without_calling_the_venue_action() {
        let (store, root) = store();
        let outcome = run_one_shot_submit(&store, command(true), 100, || {
            panic!("dry-run must not call the venue")
        })
        .expect("dry-run control transition");

        assert_eq!(outcome.accepted.status, CommandStatus::Accepted);
        assert_eq!(outcome.terminal.status, CommandStatus::Executed);
        assert_eq!(outcome.terminal.result_code, "DRY_RUN_VALIDATED");
        std::fs::remove_dir_all(root).expect("remove isolated control root");
    }
}
