//! 存储核心的常驻用例：审计链的生产写入与逐环校验、原子投影、outbox 中继。
//!
//! 从 `lib.rs` 外置：行数预算只降不升，链上多一颗形状（V11 R5-2）就得在别处节省出来（同 qx-datastruct 的口径）。

use super::*;
use qx_control::{AuditRecord, CommandKind, CommandStatus, ControlCommand, Permission};
use qx_core::{Event, EventKind, Priority};
use qx_scheduler::{JobSpec, JobWindow, RetryPolicy, Trigger};

#[test]
fn file_store_round_trips_and_rejects_path_escape() {
    let root = std::env::temp_dir().join(format!("qianxing-storage-{}", std::process::id()));
    let store = EventLogFileStore::new(&root);
    let mut log = EventLog::new();
    let seq = log.alloc_seq();
    log.append(Event::new(seq, 1, Priority::POST, EventKind::Settle));
    store.write("run-1", &log).unwrap();
    let restored = store.read("run-1").unwrap();
    assert_eq!(restored.digest(), log.digest());
    let mut extended = log.clone();
    let seq = extended.alloc_seq();
    extended.append(Event::new(seq, 2, Priority::POST, EventKind::Settle));
    store.write("run-1", &extended).unwrap();
    assert!(matches!(
        store.write("run-1", &log),
        Err(StorageError::NonAppendOnly(_))
    ));
    assert!(matches!(
        store.read("../escape"),
        Err(StorageError::InvalidName(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn segmented_event_store_is_append_only_and_manifest_verified() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-segmented-storage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut log = EventLog::new();
    for ts in 1..=3 {
        let seq = log.alloc_seq();
        log.append(Event::new(seq, ts, Priority::POST, EventKind::Settle));
    }
    store.write("run", &log).unwrap();
    assert_eq!(store.read("run").unwrap().digest(), log.digest());

    let mut extended = log.clone();
    let seq = extended.alloc_seq();
    extended.append(Event::new(seq, 4, Priority::POST, EventKind::Settle));
    store.write("run", &extended).unwrap();
    assert_eq!(store.read("run").unwrap().len(), 4);
    assert!(matches!(
        store.write("run", &log),
        Err(StorageError::NonAppendOnly(_))
    ));

    std::fs::write(
        root.join("segments").join("run-0000000000000000.jsonl"),
        "{}\n",
    )
    .unwrap();
    let corrupted = store.read("run");
    assert!(corrupted.is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn event_log_store_does_not_silently_overwrite_concurrent_extensions() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-event-concurrent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = EventLogFileStore::new(&root);
    let mut base = EventLog::new();
    let seq = base.alloc_seq();
    base.append(Event::new(seq, 1, Priority::POST, EventKind::Settle));
    store.write("run", &base).unwrap();

    let mut left = base.clone();
    let left_seq = left.alloc_seq();
    left.append(Event::new(left_seq, 2, Priority::POST, EventKind::Settle));
    let mut right = base;
    let right_seq = right.alloc_seq();
    right.append(Event::new(right_seq, 3, Priority::POST, EventKind::Settle));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_store = store.clone();
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_store.write("run", &left)
    });
    let second_store = store.clone();
    let second_barrier = barrier.clone();
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_store.write("run", &right)
    });
    let outcomes = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(StorageError::NonAppendOnly(_))))
            .count(),
        1
    );
    assert_eq!(store.read("run").unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn control_state_round_trips_after_restart() {
    let root = std::env::temp_dir().join(format!("qianxing-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = JsonStateStore::new(&root);
    let command = queued_control(1);
    store
        .update_control(|plane| {
            plane
                .submit(command.clone(), 1)
                .map_err(|error| format!("{error:?}"))
        })
        .unwrap();
    let restored = store
        .load_control_if_exists()
        .unwrap()
        .expect("控制面状态应当已经落盘");
    // 在途命令体仍读得回来：它没有终态记录，所以不会被退场口径收走。
    assert_eq!(
        restored
            .pending()
            .find(|command| command.command_id == 1)
            .map(|command| command.request_id.as_str()),
        Some(command.request_id.as_str())
    );
    // 检查点与哈希链一起恢复：窗口里那条流水在链上有一份带摘要的对应物。
    let chain = AuditFileStore::new(&root).read().unwrap();
    assert_eq!(restored.audit_chain().seq, chain.len() as u64);
    assert_eq!(
        chain
            .iter()
            .map(|entry| entry.entry_hash)
            .collect::<Vec<_>>(),
        vec![restored.audit_chain().head_hash]
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn json_state_store_round_trips_nested_report_atomically() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-json-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = JsonStateStore::new(&root);
    let value = serde_json::json!({"status":"degraded","issues":[{"asset":"USDT"}]});
    store.save_json_at("reconcile/main.json", &value).unwrap();
    let restored: serde_json::Value = store.load_json_at("reconcile/main.json").unwrap();
    assert_eq!(restored, value);
    assert!(matches!(
        store.load_json_at::<serde_json::Value>("../escape.json"),
        Err(StorageError::InvalidName(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn scheduler_state_round_trips_after_restart() {
    let root =
        std::env::temp_dir().join(format!("qianxing-scheduler-state-{}", std::process::id()));
    let store = JsonStateStore::new(&root);
    let mut scheduler = Scheduler::default();
    scheduler
        .register(JobSpec {
            job_id: "bars".into(),
            job_version: "v1".into(),
            owner: "research".into(),
            enabled: true,
            trigger: Trigger::Cron("0 9 * * 1-5".into()),
            window: JobWindow::Session,
            depends_on: Vec::new(),
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: "bars".into(),
            idempotency_key: "bars-daily".into(),
            audit_reason: "scheduler persistence test".into(),
            dry_run: true,
        })
        .unwrap();
    store.save_scheduler(&scheduler).unwrap();
    let restored = store.load_scheduler().unwrap();
    assert_eq!(restored.job("bars").unwrap().job_version, "v1");
    let _ = std::fs::remove_dir_all(root);
}

fn queued_job() -> (JobSpec, JobRun) {
    let job = JobSpec {
        job_id: "queue-job".into(),
        job_version: "v1".into(),
        owner: "research".into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: "queue-job".into(),
        idempotency_key: "queue-job-daily".into(),
        audit_reason: "queue test".into(),
        dry_run: true,
    };
    let run = JobRun {
        run_id: job.stable_key("20260910"),
        job_id: job.job_id.clone(),
        trading_day: "20260910".into(),
        attempt: 1,
        status: JobStatus::Running,
        manifest_digest: Some(7),
        error_code: None,
        next_retry_ts: None,
        started_ts: 10,
        deadline_ts: 70,
    };
    (job, run)
}

#[test]
fn file_job_queue_is_idempotent_and_recoverable() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-job-queue-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let queue = FileJobQueue::new(&root);
    let (job, run) = queued_job();
    let run_id = run.run_id;
    let first = queue.enqueue(job.clone(), run.clone(), 10).unwrap();
    assert_eq!(queue.enqueue(job.clone(), run.clone(), 10).unwrap(), first);
    assert_eq!(queue.pending().unwrap().len(), 1);
    assert_eq!(queue.enqueue(job, run.clone(), 11).unwrap(), first);

    let lease = queue.claim(run_id, "worker-a", 10, 10).unwrap();
    assert_eq!(lease.expires_ts, 20);
    assert_eq!(lease.fencing_token, 1);
    assert!(queue.available(11).unwrap().is_empty());
    assert!(matches!(
        queue.ack_at(run_id, "worker-a", lease.fencing_token, 20),
        Err(StorageError::LeaseExpired { .. })
    ));
    assert!(matches!(
        queue.claim(run_id, "worker-b", 11, 10),
        Err(StorageError::LeaseHeld { .. })
    ));
    assert!(queue.recover_expired(19).unwrap().is_empty());
    assert_eq!(queue.recover_expired(20).unwrap(), vec![run_id]);
    assert_eq!(queue.available(20).unwrap().len(), 1);
    let takeover = queue.claim(run_id, "worker-b", 21, 10).unwrap();
    assert_eq!(takeover.fencing_token, 2);
    assert!(matches!(
        queue.ack(run_id, "worker-a"),
        Err(StorageError::Unauthorized(_))
    ));
    assert!(matches!(
        queue.ack_at(run_id, "worker-a", lease.fencing_token, 22),
        Err(StorageError::Unauthorized(_))
    ));
    let done = queue
        .ack_at(run_id, "worker-b", takeover.fencing_token, 21)
        .unwrap();
    assert!(done.exists());
    assert!(queue.pending().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

fn queued_control(command_id: u64) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("queue-{command_id}"),
        operator_id: "ops".into(),
        reason: "queue integration test".into(),
        kind: CommandKind::SubmitOrder,
        target: format!("{command_id}"),
        payload: std::collections::BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: true,
    }
}

/// 一颗独立的临时根目录：审计链的用例每次都要一条新链，复用会串起上一颗的链尾。
fn audit_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qianxing-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// 走生产写入侧的一笔流水：控制面记下流水，`chain_audit` 把它接上文件后端的链。
/// 链上没有第二个入口可走（V11 R5-2 删掉了独立追加），用例要造链尾只能从这条真路进。
fn chained_submit(root: &Path, plane: &mut ControlPlane, command_id: u64) {
    plane
        .submit(queued_control(command_id), command_id)
        .map(|_| ())
        .map_err(|error| StorageError::Io(format!("{error:?}")))
        .unwrap();
    chain_audit(plane, &mut file::FileChainWriter::lock(root).unwrap()).unwrap();
}

#[test]
fn control_command_queue_is_idempotent_and_fenced() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-control-queue-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let queue = ControlCommandQueue::new(&root);
    let command = queued_control(501);
    let path = queue.enqueue(command.clone(), 10).unwrap();
    assert_eq!(queue.enqueue(command, 10).unwrap(), path);
    assert_eq!(queue.pending().unwrap().len(), 1);
    let lease = queue.claim(501, "execution-a", 10, 10).unwrap();
    assert!(queue.available(11).unwrap().is_empty());
    assert!(matches!(
        queue.claim(501, "execution-b", 11, 10),
        Err(StorageError::LeaseHeld { .. })
    ));
    assert!(matches!(
        queue.ack_at(501, "execution-b", lease.fencing_token, 11),
        Err(StorageError::Unauthorized(_))
    ));
    assert_eq!(queue.available(20).unwrap().len(), 1);
    let takeover = queue.claim(501, "execution-b", 20, 10).unwrap();
    assert_eq!(takeover.fencing_token, 2);
    queue
        .ack_at(501, "execution-b", takeover.fencing_token, 21)
        .unwrap();
    assert!(queue.pending().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn control_state_update_is_atomic_and_restores() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-control-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = JsonStateStore::new(&root);
    let (_, accepted) = store
        .update_control(|plane| {
            plane
                .submit(queued_control(601), 10)
                .map_err(|error| format!("{error:?}"))
        })
        .unwrap();
    assert_eq!(accepted.status, qx_control::CommandStatus::Accepted);
    let (_, executed) = store
        .update_control(|plane| {
            plane
                .execute(601, 11, |_| Ok("DRY_RUN_VALIDATED".into()))
                .map_err(|error| format!("{error:?}"))
        })
        .unwrap();
    assert_eq!(executed.status, qx_control::CommandStatus::Executed);
    let restored = store
        .load_control_if_exists()
        .unwrap()
        .expect("控制面状态应当已经落盘");
    assert_eq!(restored.audit().len(), 2);
    assert!(restored.pending().next().is_none());
    // 受理与终态两笔流水在链上各有一条，检查点数到链尾。
    let chain = AuditFileStore::new(&root).read().unwrap();
    assert_eq!(chain.len(), 2);
    assert_eq!(restored.audit_chain().seq, 2);
    assert_eq!(restored.audit_chain().head_hash, chain[1].entry_hash);
    let _ = std::fs::remove_dir_all(root);
}

/// 链的篡改可见性。整条链只由控制面事务写（V11 R5-2 删掉了链上的第二个写入者），
/// 所以用例从 `update_control` 进去造一条两环链，再从落盘文件里改掉第一环的摘要：
/// 冷读与尾部读必须同时抓住——写入侧只回看尾部窗口，尾部篡改逃过这里就等于下一次
/// 追加会把一条被动过的链接下去。
#[test]
fn audit_file_chain_is_written_by_the_transaction_and_tamper_evident() {
    let root = audit_root("audit");
    let store = JsonStateStore::new(&root);
    store
        .update_control(|plane| {
            plane
                .submit(queued_control(77), 10)
                .map_err(|error| format!("{error:?}"))
        })
        .unwrap();
    store
        .update_control(|plane| {
            plane
                .execute(77, 11, |_| Ok("DONE".into()))
                .map_err(|error| format!("{error:?}"))
        })
        .unwrap();
    let audit = AuditFileStore::new(&root);
    let chain = audit.read().unwrap();
    assert_eq!(chain.len(), 2, "受理与终态在链上各写一环");
    assert!(
        chain.iter().all(|entry| entry.record.command_id == 77),
        "两环都该属于同一颗命令，实际: {chain:?}"
    );
    assert_eq!(
        chain
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<u64>>(),
        vec![0, 1],
        "序号从 0 起连续"
    );
    // 检查点与链尾互相指认：doctor 的 `audit_chain` 检查用的就是这同一颗判据。
    let restored = store.load_control_if_exists().unwrap().unwrap();
    verify_audit_chain(&restored, &chain).unwrap();

    let path = root.join("audit.jsonl");
    let written = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines.len(), 2, "落盘形状是一行一条已提交记录");
    let mut tampered: AuditEntry = serde_json::from_str(lines[0]).unwrap();
    tampered.entry_hash ^= 1;
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&tampered).unwrap(),
            lines[1]
        ),
    )
    .unwrap();
    assert!(matches!(
        audit.read(),
        Err(StorageError::Conflict(message)) if message.contains("摘要不一致")
    ));
    assert!(matches!(
        audit.tail(),
        Err(StorageError::Conflict(message)) if message.contains("摘要不一致")
    ));
    let _ = std::fs::remove_dir_all(root);
}

/// 链摘要把状态念成 `{:?}`（`audit_entry_hash`，V11 R7-7 登记的那处依赖），所以这几颗
/// 变体的 **Debug 名字本身就是摘要输入**：改名不动序列化的那一半，却会让每一条已经落盘的
/// 链在冷读时逐环对不上——doctor 报 `fail`、`/control/audit` 503，而旧链补不回来。
/// 因此这里把词表钉成字面量，让改名的人在用例里看到代价，而不是在别人的机器上看到断链。
#[test]
fn audit_chain_status_vocabulary_is_pinned_by_literal_words() {
    let statuses = [
        CommandStatus::Accepted,
        CommandStatus::Executed,
        CommandStatus::Failed,
    ];
    let hashed: Vec<String> = statuses
        .iter()
        .map(|status| format!("{status:?}"))
        .collect();
    assert_eq!(
        hashed,
        ["Accepted", "Executed", "Failed"],
        "改这些名字就是改链摘要的输入：已落盘的每一环会当场对不上"
    );
    // Debug 与 serde 两半必须是同一串：只动其中一半（`#[serde(rename)]`）会让"落盘的形状"
    // 与"摘要认的形状"分叉，读侧一半报解析失败、一半报摘要不一致。
    let persisted: Vec<String> = statuses
        .iter()
        .map(|status| {
            serde_json::to_value(status)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        persisted, hashed,
        "状态词表的两种写法要逐颗相同：一份链上不能有两种身份"
    );
}

/// 名字之外，整张摘要输入的摆法也是契约：`audit_entry_hash` 按字段顺序喂 Fnv1a，任何一处
/// 改动——包括把 `{:?}` 换成看着更稳的编码——都会作废已经落盘的每一条款存链，而链不能追溯
/// 补写。这颗字面量把"今天算出来的那个数"钉住，改输入形状的人必须在这里改口（V11 R7-7）。
#[test]
fn audit_chain_digest_inputs_are_pinned_by_a_golden_record() {
    let golden = AuditRecord {
        command_id: 9801,
        request_id: "control-audit-live:9801".into(),
        operator_id: "ops".into(),
        command_digest: 12345678901234567890,
        status: CommandStatus::Executed,
        result_code: "H1_EXECUTED".into(),
        ts: 1700000000000,
    };
    assert_eq!(
        audit_entry_hash(7, 0, &golden),
        14_440_628_196_099_998_690,
        "摘要输入的形状变了：已落盘的每一条款存链会在冷读时逐环对不上，而链补不回来"
    );
}

/// 追加式的链要求写入是 O(1)：崩溃留在文件末尾的半行从未被任何状态引用过，读取只认
/// 完整行、下一次追加按已提交前缀把它截掉（V11 R5-2a）。
#[test]
fn audit_file_chain_truncates_uncommitted_residue_before_appending() {
    let root = audit_root("audit-tail");
    let store = AuditFileStore::new(&root);
    let mut plane = ControlPlane::default();
    for command_id in 1..=3 {
        chained_submit(&root, &mut plane, command_id);
    }
    let path = root.join("audit.jsonl");
    let committed = std::fs::read_to_string(&path).unwrap();
    assert_eq!(committed.matches('\n').count(), 3);
    assert_eq!(
        store.tail().unwrap().map(|entry| entry.sequence),
        Some(2),
        "链尾读交出的是最后一条已提交记录"
    );

    std::fs::write(&path, format!("{}{{\"sequence\":3", committed)).unwrap();
    assert_eq!(
        store.read().unwrap().len(),
        3,
        "末尾没有换行的一段不算已提交记录"
    );
    chained_submit(&root, &mut plane, 4);
    let entries = store.read().unwrap();
    assert_eq!(entries.len(), 4, "残尾被截掉后链继续长一条，不是长了两条");
    assert_eq!(entries.last().unwrap().record.command_id, 4);
    assert_eq!(entries.last().unwrap().sequence, 3);
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .matches('\n')
            .count(),
        4,
        "截断是真的把字节收回去，不是在残料后面接着写"
    );
    verify_audit_chain(&plane, &entries).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// 「链先落、状态后落」留下的洞由检查点补：拿着崩溃前那份状态回来续链时，链上多出来
/// 的行整段退掉，再从同一个检查点接上去；退到 0 就是整条链重来（V11 R5-2 的 WAL 顺序）。
#[test]
fn audit_file_chain_rewinds_to_the_state_checkpoint() {
    let root = audit_root("audit-rewind");
    let store = AuditFileStore::new(&root);
    let mut plane = ControlPlane::default();
    chained_submit(&root, &mut plane, 1);
    chained_submit(&root, &mut plane, 2);
    // 这一份就是"链已经写到 4 行、状态只落到了 2 行"时崩溃后会读回来的那个时点。
    let crashed = plane.clone();
    chained_submit(&root, &mut plane, 3);
    chained_submit(&root, &mut plane, 4);
    assert_eq!(store.read().unwrap().len(), 4);
    // 续链之前先要看得见这个时点：链上 4 行、检查点还停在 2 行。读侧只查"链自身逐环连续"
    // 会在这一刻通过，而它恰恰是 doctor 会放过的洞（V11 R5-2 的两半判据）。
    assert!(
        matches!(
            verify_audit_chain(&crashed, &store.read().unwrap()),
            Err(StorageError::Conflict(message)) if message.contains("不指向审计链尾")
        ),
        "检查点落后链尾时必须报错，而不是等下一笔事务去截残尾"
    );

    let mut recovered = crashed;
    chained_submit(&root, &mut recovered, 5);
    let entries = store.read().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<u64>>(),
        vec![0, 1, 2],
        "检查点之后的两行不算已提交，续链从第 2 环接上去"
    );
    assert_eq!(entries.last().unwrap().record.command_id, 5);
    verify_audit_chain(&recovered, &entries).unwrap();

    let mut wiped = ControlPlane::default();
    chained_submit(&root, &mut wiped, 9);
    let entries = store.read().unwrap();
    assert!(
        entries.iter().all(|entry| entry.record.command_id == 9),
        "检查点 0 意味着链上一条都不算提交，实际: {entries:?}"
    );
    assert_eq!(entries.len(), 1);
    verify_audit_chain(&wiped, &entries).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// 两个执行者同时提交：一笔事务把"读状态 → 改 → 接链 → 落状态"整段锁在一起，所以
/// 并发留下的是一条连续的链，而不是两条各自从 0 起数的半成品。
#[test]
fn audit_file_chain_serializes_concurrent_transactions() {
    let root = audit_root("audit-concurrent");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = [1u64, 2].map(|command_id| {
        let store = JsonStateStore::new(root.clone());
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store
                .update_control(|plane| {
                    plane
                        .submit(queued_control(command_id), command_id)
                        .map_err(|error| format!("{error:?}"))
                })
                .map(|(_, record)| record)
        })
    });
    let records: Vec<AuditRecord> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records.len(), 2, "两笔事务都必须落定，不能有一侧被锁挤掉");
    let entries = AuditFileStore::new(&root).read().unwrap();
    assert_eq!(entries.len(), 2, "两笔流水各占一环");
    assert_eq!(entries[0].previous_hash, 0);
    assert_eq!(entries[1].previous_hash, entries[0].entry_hash);
    assert_ne!(
        entries[0].record.command_id, entries[1].record.command_id,
        "两个执行者的流水不能被写成同一条命令"
    );
    let restored = JsonStateStore::new(&root)
        .load_control_if_exists()
        .unwrap()
        .unwrap();
    verify_audit_chain(&restored, &entries).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_job_queue_revalidates_forged_payload_before_claim() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-job-forge-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let queue = FileJobQueue::new(&root);
    let (job, mut run) = queued_job();
    let run_id = run.run_id;
    queue.enqueue(job.clone(), run.clone(), 10).unwrap();
    run.status = JobStatus::Succeeded;
    std::fs::write(
        queue.queue_path(run_id),
        serde_json::to_string(&QueuedJob {
            job,
            run,
            enqueued_ts: 10,
        })
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        queue.claim(run_id, "worker", 10, 10),
        Err(StorageError::Conflict(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_job_queue_serializes_concurrent_claims_on_shared_filesystem() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-job-concurrent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let queue = FileJobQueue::new(&root);
    let (job, run) = queued_job();
    let run_id = run.run_id;
    queue.enqueue(job, run, 10).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_queue = queue.clone();
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_queue.claim(run_id, "worker-a", 10, 10)
    });
    let second_queue = queue.clone();
    let second_barrier = barrier;
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_queue.claim(run_id, "worker-b", 10, 10)
    });
    let outcomes = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(StorageError::LeaseHeld { .. })))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_outbox_is_idempotent_fenced_and_retryable() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-outbox-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileOutboxStore::new(&root);
    let event = OutboxEvent {
        event_id: "order-1".into(),
        topic: "order.events".into(),
        partition_key: "account-1".into(),
        sequence: 1,
        schema_version: 1,
        trace_id: "trace-1".into(),
        payload: "{\"status\":\"filled\"}".into(),
        created_ts: 10,
        attempts: 0,
    };
    store.append(event.clone()).unwrap();
    store.append(event).unwrap();
    let first = store.claim("order-1", "relay-a", 10, 5).unwrap();
    assert!(matches!(
        store.claim("order-1", "relay-b", 11, 5),
        Err(StorageError::LeaseHeld { .. })
    ));
    assert!(matches!(
        store.ack("order-1", "relay-a", first.fencing_token, 16),
        Err(StorageError::LeaseExpired { .. })
    ));
    let second = store.claim("order-1", "relay-b", 16, 5).unwrap();
    assert_eq!(second.fencing_token, first.fencing_token + 1);
    store
        .retry("order-1", "relay-b", second.fencing_token, 17)
        .unwrap();
    assert_eq!(store.available(17).unwrap()[0].attempts, 1);
    let third = store.claim("order-1", "relay-a", 17, 5).unwrap();
    store
        .ack("order-1", "relay-a", third.fencing_token, 18)
        .unwrap();
    assert!(store.available(18).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn outbox_relay_publishes_then_acknowledges_and_retries_failures() {
    struct Publisher {
        fail: bool,
        published: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl OutboxPublisher for Publisher {
        fn publish(&self, event: &OutboxEvent) -> Result<(), String> {
            if self.fail {
                return Err("test publisher unavailable".into());
            }
            self.published.lock().unwrap().push(event.event_id.clone());
            Ok(())
        }
    }

    let root = std::env::temp_dir().join(format!(
        "qianxing-outbox-relay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileOutboxStore::new(&root);
    store
        .append(OutboxEvent {
            event_id: "relay-event".into(),
            topic: "qx.events".into(),
            partition_key: "account".into(),
            sequence: 1,
            schema_version: 1,
            trace_id: String::new(),
            payload: "{}".into(),
            created_ts: 1,
            attempts: 0,
        })
        .unwrap();
    let published = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let failing = OutboxRelay::new(
        store.clone(),
        Publisher {
            fail: true,
            published: published.clone(),
        },
        "relay",
        10,
    )
    .unwrap();
    assert_eq!(failing.pump_once(1, 10).unwrap().retried, 1);
    assert_eq!(store.available(1).unwrap()[0].attempts, 1);
    let working = OutboxRelay::new(
        store.clone(),
        Publisher {
            fail: false,
            published,
        },
        "relay",
        10,
    )
    .unwrap();
    let report = working.pump_once(2, 10).unwrap();
    assert_eq!(report.published, 1);
    assert!(store.available(2).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_consumer_checkpoint_is_idempotent_and_dead_letters_after_retries() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-consumer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileConsumerStateStore::new(&root);
    let engine = ConsumerEngine::new(store.clone(), "ledger-reducer", 2).unwrap();
    let event = OutboxEvent {
        event_id: "consumer-event-1".into(),
        topic: "qx.eventlog".into(),
        partition_key: "account-1".into(),
        sequence: 1,
        schema_version: 1,
        trace_id: String::new(),
        payload: "{}".into(),
        created_ts: 10,
        attempts: 0,
    };
    assert_eq!(
        engine.consume(&event, 5, 1, 10, |_| Ok(())).unwrap(),
        ConsumerOutcome::Applied
    );
    assert_eq!(
        engine
            .consume(&event, 5, 1, 11, |_| panic!(
                "duplicate must not invoke handler"
            ))
            .unwrap(),
        ConsumerOutcome::Duplicate
    );
    let failed = OutboxEvent {
        event_id: "consumer-event-2".into(),
        sequence: 2,
        ..event
    };
    assert!(matches!(
        engine
            .consume(&failed, 6, 1, 12, |_| Err("temporary".into()))
            .unwrap(),
        ConsumerOutcome::Retried { .. }
    ));
    assert_eq!(
        engine
            .consume(&failed, 6, 2, 13, |_| Err("permanent".into()))
            .unwrap(),
        ConsumerOutcome::DeadLettered
    );
    let dead = store
        .dead_letter("ledger-reducer", "consumer-event-2")
        .unwrap()
        .expect("死信必须能按 event_id 点查命中");
    assert_eq!(
        (
            dead.attempts,
            dead.offset,
            dead.failed_ts,
            dead.error.as_str()
        ),
        (2, 6, 13, "permanent")
    );
    assert_eq!(dead.event.event_id, "consumer-event-2");
    assert!(store
        .dead_letter("ledger-reducer", "consumer-event-1")
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .load_checkpoint("ledger-reducer", "qx.eventlog", "account-1")
            .unwrap()
            .unwrap()
            .offset,
        6
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn file_token_bucket_is_persistent_and_serializes_concurrent_consumers() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-rate-limit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bucket = FileTokenBucket::new(&root, "api", 1, 0).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_bucket = bucket.clone();
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_bucket.try_acquire(10, 1).unwrap()
    });
    let second_bucket = bucket.clone();
    let second_barrier = barrier.clone();
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second_bucket.try_acquire(10, 1).unwrap()
    });
    let granted = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(granted.iter().filter(|value| **value).count(), 1);
    assert!(!bucket.try_acquire(10, 1).unwrap());
    let replenished = FileTokenBucket::new(&root, "api", 1, 1).unwrap();
    assert!(replenished.try_acquire(11, 1).unwrap());
    let _ = std::fs::remove_dir_all(root);
}
