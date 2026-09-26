//! 控制面事务必须把审计尾部喂给"只追加"的哈希链（V12 §16 断链修复）。
//!
//! 修复前的实况是：`sync_control` 只有用例读者，生产命令一律只整体覆盖
//! `control-plane.json`（或 `qx_control_state` 那一行），而 `deploy/README.md` 与
//! 后端能力段落宣称的"审计链"文件在真实运行里永远是空的。本文件把这条
//! 因果钉住：事务提交 ⇒ 链变长；篡改 ⇒ 事务失败；链落后 ⇒ 自愈。

use qx_control::{AuditRecord, CommandKind, CommandStatus, ControlCommand, Permission};
use qx_storage::{AuditFileStore, JsonStateStore};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn command(id: u64) -> ControlCommand {
    ControlCommand {
        command_id: id,
        request_id: format!("req-{id}"),
        operator_id: "operator".into(),
        reason: "audit chain wiring".into(),
        kind: CommandKind::CancelOrder,
        target: format!("order-{id}"),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    }
}

fn temp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-audit-wired-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn submit(store: &JsonStateStore, id: u64) {
    let (_, result) = store
        .transact_control(|plane| plane.submit(command(id), id).map(|record| record.status))
        .unwrap();
    assert_eq!(
        result.unwrap(),
        CommandStatus::Accepted,
        "命令 {id} 必须被受理"
    );
}

#[test]
fn every_committed_control_transaction_extends_the_durable_audit_chain() {
    let root = temp_root("grow");
    let store = JsonStateStore::new(&root);
    for id in 1..=3 {
        submit(&store, id);
    }
    let entries = AuditFileStore::new(&root).read().unwrap();
    assert_eq!(
        entries.len(),
        3,
        "三次成功事务必须在持久化链上留下三条记录，而不是只留一份可覆盖的快照"
    );
    assert_eq!(entries[0].previous_hash, 0);
    for pair in entries.windows(2) {
        assert_eq!(
            pair[1].previous_hash, pair[0].entry_hash,
            "链必须逐条相接，否则第 {} 条之后的历史可以被整段替换",
            pair[1].sequence
        );
    }
    assert_eq!(
        entries[2].record.command_id, 3,
        "链尾必须是最后一次事务的那条命令"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_business_rejected_transaction_grows_neither_the_snapshot_nor_the_chain() {
    let root = temp_root("reject");
    let store = JsonStateStore::new(&root);
    submit(&store, 1);
    // 同一 request_id 再投一次：这是业务拒绝，不能被包装成存储故障，
    // 也不允许把半条命令留在快照或链上。
    let (_, result) = store
        .transact_control(|plane| plane.submit(command(1), 2).map(|record| record.status))
        .unwrap();
    assert!(result.is_err(), "重复请求必须被拒绝");
    let entries = AuditFileStore::new(&root).read().unwrap();
    assert_eq!(entries.len(), 1, "被拒绝的事务不得给链添记录");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_chain_ahead_of_the_snapshot_fails_the_transaction_inst_of_rewriting_history() {
    let root = temp_root("ahead");
    let store = JsonStateStore::new(&root);
    submit(&store, 1);
    // 模拟有人绕过事务边界直接往链上追加了一条快照里不存在的记录。
    let audit = AuditFileStore::new(&root);
    audit
        .append(AuditRecord {
            command_id: 99,
            request_id: "req-99".into(),
            operator_id: "attacker".into(),
            command_digest: 7,
            status: CommandStatus::Accepted,
            result_code: "FORGED".into(),
            ts: 99,
        })
        .unwrap();
    let error = match store.transact_control(|plane| plane.submit(command(2), 2).map(|_| ())) {
        Ok((_, result)) => panic!("链比快照长说明历史分叉，必须拒绝而不是按快照重算链: {result:?}"),
        Err(error) => error,
    };
    assert!(
        matches!(error, qx_storage::StorageError::Conflict(_)),
        "分叉的口径必须是 Conflict，实际 {error:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_chain_trailing_the_snapshot_reheals_on_the_next_transaction() {
    let root = temp_root("lag");
    let store = JsonStateStore::new(&root);
    submit(&store, 1);
    submit(&store, 2);
    // 链落后（例如上一次同步在写完快照后崩溃）要能自愈，而不是从此卡住。
    let path = root.join("audit.json");
    let entries: Vec<qx_storage::AuditEntry> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&entries[..1]).unwrap()).unwrap();
    submit(&store, 3);
    let entries = AuditFileStore::new(&root).read().unwrap();
    assert_eq!(
        entries.len(),
        3,
        "落后的链必须由下一次事务补齐，而不是留下永久缺口"
    );
    assert_eq!(entries[1].record.command_id, 2);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_control_transaction_feeds_the_same_audit_table() {
    let path = temp_root("sqlite").join("state.sqlite3");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let store = qx_storage::SqliteControlStore::new(&path).unwrap();
    let (_, result) = store
        .transact_control(|plane| plane.submit(command(1), 1).map(|_| ()))
        .unwrap();
    result.unwrap();
    let audit = qx_storage::SqliteAuditStore::new(&path).unwrap();
    let entries = audit.read().unwrap();
    assert_eq!(
        entries.len(),
        1,
        "SQLite 后端的事务同样必须把审计尾部写进 qx_audit_entries"
    );
    assert_eq!(entries[0].record.command_id, 1);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
