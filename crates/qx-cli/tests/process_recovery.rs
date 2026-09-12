use qx_control::{CommandKind, ControlCommand, Permission};
use qx_storage::ControlCommandQueue;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn run_child(executable: &str, args: &[String]) -> String {
    let output = Command::new(executable)
        .args(args)
        .output()
        .expect("启动 recovery-child 失败");
    assert!(
        output.status.success(),
        "recovery-child 失败: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[test]
fn process_restart_recovers_expired_execution_lease_and_rejects_stale_ack() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-process-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let queue = ControlCommandQueue::new(&root);
    queue
        .enqueue(
            ControlCommand {
                command_id: 99001,
                request_id: "recovery-99001".into(),
                operator_id: "recovery-test".into(),
                reason: "cross process recovery matrix".into(),
                kind: CommandKind::ReconcileAccount,
                target: "main".into(),
                payload: BTreeMap::new(),
                permission: Permission::Trading,
                dry_run: false,
            },
            100,
        )
        .unwrap();
    let executable = env!("CARGO_BIN_EXE_qx-cli");
    let root_arg = root.to_string_lossy().to_string();
    let first_token = run_child(
        executable,
        &[
            "recovery-child".into(),
            root_arg.clone(),
            "claim".into(),
            "worker-a".into(),
            "100".into(),
            "10".into(),
            "99001".into(),
        ],
    );
    assert_eq!(first_token, "1");
    assert!(queue.available(105).unwrap().is_empty());
    assert_eq!(queue.available(111).unwrap().len(), 1);

    let stale = run_child(
        executable,
        &[
            "recovery-child".into(),
            root_arg.clone(),
            "stale-ack".into(),
            "worker-a".into(),
            "111".into(),
            first_token.clone(),
            "99001".into(),
        ],
    );
    assert_eq!(stale, "rejected");
    let takeover_token = run_child(
        executable,
        &[
            "recovery-child".into(),
            root_arg.clone(),
            "takeover".into(),
            "worker-b".into(),
            "111".into(),
            "10".into(),
            "99001".into(),
        ],
    );
    assert_eq!(takeover_token, "2");
    let acked = run_child(
        executable,
        &[
            "recovery-child".into(),
            root_arg,
            "ack".into(),
            "worker-b".into(),
            "112".into(),
            takeover_token,
            "99001".into(),
        ],
    );
    assert_eq!(acked, "acked");
    assert!(queue.pending().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}
