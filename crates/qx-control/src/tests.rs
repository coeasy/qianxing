//! 控制面的常驻用例（V11 R5-1 的退场形状与 R5-3 的幂等身份）。
//!
//! 从 `lib.rs` 外置：行数预算只降不升，退场多一颗判据就得在别处节省出来（同 qx-datastruct 的口径）。

use super::*;

fn command(permission: Permission) -> ControlCommand {
    ControlCommand {
        command_id: 1,
        request_id: "req-1".into(),
        operator_id: "operator".into(),
        reason: "incident recovery".into(),
        kind: CommandKind::PauseStrategy,
        target: "order-1".into(),
        payload: BTreeMap::new(),
        permission,
        dry_run: true,
    }
}

#[test]
fn control_command_is_audited_and_idempotent() {
    let mut plane = ControlPlane::default();
    let record = plane.submit(command(Permission::Trading), 10).unwrap();
    assert_eq!(record.status, CommandStatus::Accepted);
    assert_eq!(plane.audit().len(), 1);
    assert_eq!(
        plane.submit(command(Permission::Trading), 11),
        Err(ControlError::DuplicateRequest("req-1".into()))
    );
    let mut duplicate_id = command(Permission::Trading);
    duplicate_id.request_id = "req-2".into();
    assert_eq!(
        plane.submit(duplicate_id, 12),
        Err(ControlError::DuplicateCommand(1))
    );
}

#[test]
fn insufficient_permission_is_rejected_before_queueing() {
    let mut plane = ControlPlane::default();
    assert_eq!(
        plane.submit(command(Permission::ReadOnly), 10),
        Err(ControlError::Forbidden)
    );
    assert!(plane.audit().is_empty());
}

#[test]
fn declared_permission_cannot_exceed_server_grant() {
    let mut plane = ControlPlane::default();
    assert_eq!(
        plane.submit_as(command(Permission::Trading), Permission::ReadOnly, 10),
        Err(ControlError::Forbidden)
    );
    assert!(plane.audit().is_empty());
    assert!(plane
        .submit_as(command(Permission::Trading), Permission::Admin, 11)
        .is_ok());
}

/// 没有执行者的种类必须在受理处当场拒，且拒绝发生在写入 `Accepted` 之前。
///
/// 逐格过一遍五颗，而不是只挑一颗代言：`has_executor` 少列一种就会让那种命令重新变成
/// "202 受理 + 永远停在 pending"，而那种形状正是这颗缺陷本身。
#[test]
fn kinds_without_an_executor_are_refused_at_acceptance() {
    for kind in [
        CommandKind::CancelOrder,
        CommandKind::ChangeRiskLimit,
        CommandKind::ReconcileAccount,
        CommandKind::RetryJob,
        CommandKind::SwitchVenue,
    ] {
        // 权限一次给到 Admin：判据只能是"没人执行"，不能是权限不够顺路挡下的。
        let mut command = command(Permission::Admin);
        command.kind = kind.clone();
        let mut plane = ControlPlane::default();
        let error = plane
            .submit_as(command.clone(), Permission::Admin, 10)
            .expect_err("没有执行者的命令必须在受理处被拒");
        match error {
            ControlError::Invalid(reason) => assert!(
                reason.contains("没有执行者"),
                "{kind:?} 的拒绝理由不是「没人执行」，而是别的检查顺路顶上了: {reason}"
            ),
            other => panic!("{kind:?} 应以 Invalid 被拒，实际 {other:?}"),
        }
        assert!(
            plane.audit().is_empty(),
            "{kind:?} 已被拒，却仍留下了 Accepted 审计"
        );
        assert_eq!(
            plane.pending().count(),
            0,
            "{kind:?} 已被拒，命令体却进了主表"
        );
    }
}

/// 三格可执行的形状是反面对照：新判据不能退化成"什么都不受理"。
#[test]
fn kinds_with_an_executor_stay_acceptable() {
    for kind in [
        CommandKind::SubmitOrder,
        CommandKind::PauseStrategy,
        CommandKind::ResumeStrategy,
    ] {
        let label = format!("{kind:?}");
        assert!(kind.has_executor());
        let mut command = command(Permission::Trading);
        command.kind = kind;
        let mut plane = ControlPlane::default();
        plane
            .submit(command, 10)
            .unwrap_or_else(|error| panic!("{label} 有执行者却被受理处拒绝: {error:?}"));
        assert_eq!(plane.audit().len(), 1);
    }
}

/// 可执行性只在受理处生效：修复之前已经落盘的历史命令仍要能读回来并处置成终态。
///
/// 把判据搬进结构性 `validate` 会让这份数据在恢复时直接拒启——那比原来的缺陷更糟。
#[test]
fn persisted_commands_without_an_executor_still_restore() {
    let mut legacy = command(Permission::Trading);
    legacy.kind = CommandKind::CancelOrder;
    let mut plane = ControlPlane::default();
    plane
        .requests
        .insert(legacy.request_id.clone(), legacy.command_id);
    plane.commands.insert(legacy.command_id, legacy.clone());
    plane.audit.push(AuditRecord {
        command_id: legacy.command_id,
        request_id: legacy.request_id.clone(),
        operator_id: legacy.operator_id.clone(),
        command_digest: legacy.digest(),
        status: CommandStatus::Accepted,
        result_code: "ACCEPTED_FOR_EXECUTION".into(),
        ts: 10,
    });
    let mut restored =
        ControlPlane::from_json(&persisted(&plane)).expect("历史受理记录必须仍能恢复");
    // 恢复后它仍是那条"停在 pending"的旧命令：读得回，也还能被写成终态。
    assert_eq!(restored.pending().count(), 1);
    let record = restored
        .execute(legacy.command_id, 11, |_| Ok("LEGACY_DISPOSED".into()))
        .expect("旧命令必须仍可回写终态");
    assert_eq!(record.status, CommandStatus::Executed);
}

#[test]
fn execution_is_audited_and_idempotent_after_completion() {
    let mut plane = ControlPlane::default();
    plane.submit(command(Permission::Trading), 10).unwrap();
    assert_eq!(plane.pending().count(), 1);
    let record = plane.execute(1, 11, |_| Ok("APPLIED".into())).unwrap();
    assert_eq!(record.status, CommandStatus::Executed);
    assert_eq!(record.result_code, "APPLIED");
    assert_eq!(plane.pending().count(), 0);
    assert_eq!(
        plane.execute(1, 12, |_| Ok("DUPLICATE".into())),
        Err(ControlError::AlreadyFinal(1))
    );
}

#[test]
fn restore_rejects_terminal_audit_without_acceptance() {
    let command = command(Permission::Trading);
    let mut plane = ControlPlane::default();
    plane
        .requests
        .insert(command.request_id.clone(), command.command_id);
    plane.commands.insert(command.command_id, command.clone());
    let command_digest = command.digest();
    plane.audit.push(AuditRecord {
        command_id: command.command_id,
        request_id: command.request_id,
        operator_id: command.operator_id,
        command_digest,
        status: CommandStatus::Executed,
        result_code: "APPLIED".into(),
        ts: 10,
    });
    let json = plane.to_json().unwrap();
    assert!(ControlPlane::from_json(&json).is_err());
}

/// 逐颗换身份的同形状命令：上面那颗 `command()` 夹具 id 固定，跑不了窗口。
/// 序号按定宽写，让"命令数翻倍而窗口有界"这条判据能落成一次逐字相等而不是一个百分比。
fn numbered_command(command_id: u64) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("req-{command_id:04}"),
        ..command(Permission::Trading)
    }
}

/// 交出一份"落过盘"的状态文本。接链是存储层在事务里做的（`audit_seq` 由它推进），
/// 这里的夹具只在内存里造流水，所以补上"窗口这些记录已经在链上"这一格——
/// 让每个用例真正要审的那件事成为唯一的失败原因，而不是新加的链接入守卫。
fn persisted(plane: &ControlPlane) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&plane.to_json().unwrap()).unwrap();
    value["audit_seq"] = (plane.audit().len() as u64).into();
    value.to_string()
}

/// 链接入之前写下的状态文件（或绕过事务边界的整份保存）就是这个形状：窗口里摆着
/// 流水，接链计数却是 0。那种文件必须当场拒启——链不能追溯补写，读侧也不能念一段
/// 没有任何摘要背书的审计历史。
#[test]
fn restore_refuses_window_records_that_were_never_chained() {
    let mut plane = ControlPlane::default();
    plane.submit(numbered_command(41), 10).unwrap();
    assert_eq!(
        plane.audit_chain().seq,
        0,
        "夹具要靠「没接链」这一格成立，先把它钉住"
    );
    let Err(error) = ControlPlane::from_json(&plane.to_json().unwrap()) else {
        panic!("未接链的审计窗口必须被恢复拒收，让它启动就是念假历史");
    };
    assert!(
        error.contains("没接进哈希链"),
        "未接链的窗口应以检查点不足被拒，实际: {error}"
    );
}

/// 退场的形状：命令体离开主表，`Accepted` 与终态那一审计对留下，计数前进，
/// 并且这份状态还要能原样读回来——否则"保留摘要"只是一句口号。
#[test]
fn a_final_command_leaves_the_main_table_with_its_audit_pair() {
    let mut plane = ControlPlane::default();
    plane.submit(numbered_command(7), 10).unwrap();
    assert_eq!(
        plane.retirement().retired_total,
        0,
        "在途命令不能被算成退场"
    );
    plane.execute(7, 11, |_| Ok("APPLIED".into())).unwrap();
    assert_eq!(plane.pending().count(), 0, "终态命令必须离开主表");
    let records: Vec<_> = plane
        .audit()
        .iter()
        .filter(|record| record.command_id == 7)
        .collect();
    assert_eq!(records.len(), 2, "退场只带走命令体，不带走那一审计对");
    assert_eq!(records[0].status, CommandStatus::Accepted);
    assert_eq!(records[1].status, CommandStatus::Executed);
    let summary = plane.retirement();
    assert_eq!(
        (
            summary.retired_total,
            summary.executed,
            summary.last_retired_ts
        ),
        (1, 1, 11),
        "退场摘要必须数得出总量、终态种类与最后时点"
    );
    let restored = ControlPlane::from_json(&persisted(&plane)).unwrap();
    assert_eq!(restored.audit(), plane.audit());
    assert_eq!(restored.retirement(), summary);
    assert!(restored.pending().next().is_none());
    assert_eq!(
        restored.latest_audit(7).map(|record| record.status),
        Some(CommandStatus::Executed),
        "命令体已退场，窗口里那一对仍要答得出终态"
    );
}

/// 摘要的每格都要有人写：退场总数必须等于各终态格之和。
///
/// `rejected` 那一格当初就是这么来的——词表里有一颗没人产出的状态，计数永远为 0，
/// 却照样随 `/control/audit` 外销（V11 R6-1）。加一颗新终态而忘记给它计数，这里先红。
#[test]
fn every_retired_command_is_counted_by_its_terminal_status() {
    let mut plane = ControlPlane::default();
    for command_id in 1..=6_u64 {
        plane
            .submit(numbered_command(command_id), 10 + command_id)
            .unwrap();
    }
    for command_id in 1..=3_u64 {
        plane
            .execute(command_id, 20 + command_id, |_| Ok("APPLIED".into()))
            .unwrap();
    }
    for command_id in 4..=6_u64 {
        plane
            .execute(command_id, 24 + command_id, |_| {
                Err("EXECUTION_FAILED".into())
            })
            .unwrap();
    }
    let summary = plane.retirement();
    assert_eq!(
        (summary.retired_total, summary.executed, summary.failed),
        (6, 3, 3),
        "退场总数必须等于各终态格之和：多出一颗没人计的终态，这里就先红"
    );
    assert_eq!(
        ControlPlane::from_json(&persisted(&plane))
            .unwrap()
            .retirement(),
        summary,
        "摘要必须原样读回，否则恢复之后计数会从头再来"
    );
}

/// 退场不等于失忆：窗口还留着这条身份时，重复受理仍按同一种拒绝回答。
#[test]
fn a_retired_identity_is_still_refused_within_the_window() {
    let mut plane = ControlPlane::default();
    let command = numbered_command(9);
    plane.submit(command.clone(), 10).unwrap();
    plane.execute(9, 11, |_| Ok("APPLIED".into())).unwrap();
    assert_eq!(
        plane.execute(9, 12, |_| Ok("TWICE".into())),
        Err(ControlError::AlreadyFinal(9)),
        "退场后的重复执行不能落回 UnknownCommand"
    );
    // 请求号先答、命令号后答：两条都是拒绝，但命令号那一支才是"命令体已经不在了"的形状。
    assert_eq!(
        plane.submit(command.clone(), 13),
        Err(ControlError::DuplicateRequest("req-0009".into()))
    );
    let mut same_id_other_request = command.clone();
    same_id_other_request.request_id = "req-0009-beside".into();
    assert_eq!(
        plane.submit(same_id_other_request, 14),
        Err(ControlError::DuplicateCommand(9)),
        "command_id 只离开了主表，没有离开审计窗口"
    );
    let mut same_request = command;
    same_request.command_id = 909;
    assert_eq!(
        plane.submit(same_request, 15),
        Err(ControlError::DuplicateRequest("req-0009".into()))
    );
    // 换一个身份就是新命令：窗口挡的是"同一身份重复受理"，不是"这条链再跑一次"。
    assert!(plane.submit(numbered_command(10), 16).is_ok());
}

/// 只删主表不裁流水，`to_json` 照样线性涨——窗口才是把"退场"变成"有界"的那一半。
#[test]
fn the_audit_window_stays_bounded_across_a_long_run() {
    let rounds = AUDIT_WINDOW_RECORDS * 4;
    let mut plane = ControlPlane::default();
    let mut doc_bytes = Vec::new();
    for command_id in 0..rounds as u64 {
        plane
            .submit(numbered_command(command_id), command_id)
            .unwrap();
        plane
            .execute(command_id, command_id + 1, |_| {
                Ok(format!("APPLIED-{command_id:04}"))
            })
            .unwrap();
        // 两个采样点各自窗口里那 500 对都同宽（id、ts、result_code 都是四位），剩下的
        // 字节差只来自 `command_digest` 的位数漂移，与命令数无关。
        if [rounds as u64 / 2, rounds as u64 - 1].contains(&command_id) {
            doc_bytes.push(plane.to_json().unwrap().len());
        }
    }
    assert!(plane.pending().next().is_none());
    assert_eq!(
        plane.audit().len(),
        AUDIT_WINDOW_RECORDS,
        "流水必须裁回窗口容量"
    );
    assert_eq!(plane.retirement().retired_total, rounds as u64);
    // 裁的是最旧那一对，不是"装着而已"。
    assert!(
        plane.latest_audit(0).is_none(),
        "最旧的退场对应滚出窗口，否则容量只是纸面上的"
    );
    assert_eq!(
        plane
            .latest_audit(rounds as u64 - 1)
            .map(|record| record.status),
        Some(CommandStatus::Executed)
    );
    // 窗口外不再有身份证据：重复执行落回 UnknownCommand，而不是假装还认得。
    assert_eq!(
        plane.execute(0, rounds as u64 + 1, |_| Ok("X".into())),
        Err(ControlError::UnknownCommand(0))
    );
    let [later, last] = [doc_bytes[0], doc_bytes[1]];
    assert!(
        last * 100 <= later * 101,
        "命令数从 {} 涨到 {}，状态文档规模必须仍锁在窗口容量上（实测 {later} B → {last} B）",
        rounds / 2,
        rounds
    );
}

/// 「只追加」时代写下的状态文件里，终态命令体还留在主表：加载即退场，而不是拒启。
#[test]
fn a_legacy_append_only_state_file_retires_on_load() {
    let command = numbered_command(21);
    let accepted = AuditRecord {
        command_id: command.command_id,
        request_id: command.request_id.clone(),
        operator_id: command.operator_id.clone(),
        command_digest: command.digest(),
        status: CommandStatus::Accepted,
        result_code: "ACCEPTED_FOR_EXECUTION".into(),
        ts: 10,
    };
    let executed = AuditRecord {
        status: CommandStatus::Executed,
        result_code: "APPLIED".into(),
        ts: 11,
        ..accepted.clone()
    };
    let mut legacy = ControlPlane::default();
    legacy
        .requests
        .insert(command.request_id.clone(), command.command_id);
    legacy.commands.insert(command.command_id, command.clone());
    legacy.audit.extend([accepted, executed]);
    let mut plane = ControlPlane::from_json(&persisted(&legacy)).unwrap();
    assert!(
        plane.pending().next().is_none(),
        "旧文件里的终态命令必须在加载时离开主表"
    );
    assert_eq!(plane.audit().len(), 2, "退场不带走审计对，历史仍读得回来");
    assert_eq!(plane.retirement().retired_total, 1);
    assert_eq!(plane.retirement().last_retired_ts, 11);
    assert_eq!(
        plane.latest_audit(21).map(|record| record.status),
        Some(CommandStatus::Executed)
    );
    assert_eq!(
        plane.submit(command.clone(), 12),
        Err(ControlError::DuplicateRequest("req-0021".into())),
        "旧文件退场后，请求号那半身份仍要认得"
    );
    let mut same_id = command;
    same_id.request_id = "req-0021-beside".into();
    assert_eq!(
        plane.submit(same_id, 13),
        Err(ControlError::DuplicateCommand(21)),
        "命令体已经不在主表了，身份只能由窗口里那一对作证"
    );
    // 再落一次盘就是当前形状：第二次加载不得再多退场一条。
    let again = ControlPlane::from_json(&plane.to_json().unwrap()).unwrap();
    assert_eq!(again.retirement(), plane.retirement());
    assert_eq!(again.audit(), plane.audit());
}

/// 窗口里只剩一条 `Accepted`、命令体与终态都不在：这条命令到底跑没跑过，答不出来。
#[test]
fn restore_rejects_an_accepted_record_without_body_or_terminal() {
    let command = numbered_command(31);
    let broken = ControlPlane {
        audit: vec![AuditRecord {
            command_id: command.command_id,
            request_id: command.request_id.clone(),
            operator_id: command.operator_id.clone(),
            command_digest: command.digest(),
            status: CommandStatus::Accepted,
            result_code: "ACCEPTED_FOR_EXECUTION".into(),
            ts: 10,
        }],
        ..ControlPlane::default()
    };
    let Err(error) = ControlPlane::from_json(&persisted(&broken)) else {
        panic!("孤立 Accepted 必须被恢复拒收，否则这条命令跑没跑过无人能答");
    };
    assert!(
        error.contains("没有在途命令体"),
        "孤立 Accepted 应以身份不可答被拒，实际: {error}"
    );
}
