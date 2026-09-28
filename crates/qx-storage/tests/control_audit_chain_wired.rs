//! 控制面事务必须把审计尾部喂给哈希链（V11 R5-2 / V12 §16 断链修复）。
//!
//! 修复前的实况是：`sync_control` 只有用例读者，生产命令一律只整体覆盖
//! `control-plane.json`，而 `deploy/README.md` 与后端能力段落宣称的"审计链"文件在真实
//! 运行里永远是空的。本文件把这条因果钉住，并且只从公开面走：写用 `transact_control`，
//! 读用 [`qx_storage::AuditStore::read_entries`]——链的冷读契约在集成层一个读者都没有的
//! 话，"能读回来"这句话就只有单元测试里那条 `pub(crate)` 通路在替它作保。
//!
//! 上游同名的另外两颗用例（链比快照长要报 Conflict、链比快照短要自愈）方向与
//! V11 R5-2 相反，没带过来：链比检查点长的那一段从没被任何已提交状态引用过，是崩溃
//! 残尾，由下一笔事务截掉；链比检查点短（被人剪过）的必须失败关闭，静默补齐等于把
//! "剪掉中间一段历史"读成一次自愈。两半各自长在：残尾与检查点退链在 `src/tests.rs` 的
//! `audit_file_chain_truncates_uncommitted_residue_before_appending` 与
//! `audit_file_chain_rewinds_to_the_state_checkpoint`，剪短那一半是本文件的
//! [`a_chain_trimmed_below_its_checkpoint_fails_closed_on_both_sides`]。

use qx_control::{CommandStatus, ControlCommand, Permission};
use qx_storage::{AuditFileStore, AuditStore, JsonStateStore};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn command(id: u64) -> ControlCommand {
    ControlCommand {
        command_id: id,
        request_id: format!("req-{id}"),
        operator_id: "operator".into(),
        reason: "audit chain wiring".into(),
        // 必须用有执行者的种类：没有执行者的种类在受理处当场拒（V11 P1），链上什么都不会留。
        kind: qx_control::CommandKind::SubmitOrder,
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
    let entries = AuditFileStore::new(&root).read_entries().unwrap();
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
    let entries = AuditFileStore::new(&root).read_entries().unwrap();
    assert_eq!(entries.len(), 1, "被拒绝的事务不得给链添记录");
    let _ = std::fs::remove_dir_all(root);
}

/// 链被从尾部剪短（抹掉一段已提交历史）时，读侧与写侧都必须说话。
///
/// 上游同名位置写的是另一侧期望："链落后就由下一次事务补齐"。V11 R5-2 反过来定：
/// 静默补齐等于把"剪掉中间一段历史"读成一次自愈，所以这里钉的是失败关闭。
#[test]
fn a_chain_trimmed_below_its_checkpoint_fails_closed_on_both_sides() {
    let root = temp_root("trimmed");
    let store = JsonStateStore::new(&root);
    submit(&store, 1);
    submit(&store, 2);
    let path = root.join("audit.jsonl");
    let written = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{}\n", written.lines().next().unwrap())).unwrap();

    // 读侧（doctor 的 `audit_chain` 用的就是这颗公开判据）：只剩一环的链自身仍然连续，
    // 只有拿检查点比对才看得见"少了一条"。
    let plane = store.load_control_if_exists().unwrap().unwrap();
    let chain = AuditFileStore::new(&root).read_entries().unwrap();
    assert_eq!(chain.len(), 1, "剪短后的链读回来还是一环");
    assert!(
        matches!(
            qx_storage::verify_audit_chain(&plane, &chain),
            Err(qx_storage::StorageError::Conflict(_))
        ),
        "检查点引用两条、链上只剩一条，必须报错而不是继续: {:?}",
        qx_storage::verify_audit_chain(&plane, &chain)
    );

    // 写侧：下一笔事务不能把那一条当成链尾接着写，否则被剪掉的那一环永远补不回来。
    let outcome = store
        .transact_control(|plane| plane.submit(command(3), 3).map(|_| ()))
        .err();
    assert!(
        matches!(outcome, Some(qx_storage::StorageError::Conflict(_))),
        "短链上追加必须失败关闭，实际 {outcome:?}"
    );
    assert_eq!(
        AuditFileStore::new(&root).read_entries().unwrap().len(),
        1,
        "被拒的事务不得在不连续的链尾留记录"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_control_transaction_feeds_the_same_audit_table() {
    let root = temp_root("sqlite");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.sqlite3");
    let store = qx_storage::SqliteControlStore::new(&path).unwrap();
    let (_, result) = store
        .transact_control(|plane| plane.submit(command(1), 1).map(|_| ()))
        .unwrap();
    result.unwrap();
    let audit = qx_storage::SqliteAuditStore::new(&path).unwrap();
    let entries = audit.read_entries().unwrap();
    assert_eq!(
        entries.len(),
        1,
        "SQLite 后端的事务同样必须把审计尾部写进 qx_audit_entries"
    );
    assert_eq!(entries[0].record.command_id, 1);
    let _ = std::fs::remove_dir_all(root);
}
