//! #188 判据：控制面只受理真的有执行者的命令类型，且"有没有执行者"这条判定不漂移。
//!
//! 改前实况：`CommandKind` 八颗变体里有五颗（`ChangeRiskLimit` / `CancelOrder` /
//! `ReconcileAccount` / `RetryJob` / `SwitchVenue`）在全仓没有任何派发者。提交入口照样
//! 返回 `Accepted` 并把命令写进审计与队列，然后它永远停在那里 —— 运维读到"已受理"，
//! 交易所、调度器那两侧什么都没有。现在提交入口按 `CommandKind::executed()` 裁决，
//! 但判定式本身是一行手写的 `matches!`，它会腐坏，所以本文件钉住两侧：
//!
//! - **行为侧**：八颗逐个过真实提交入口 —— 三颗受理、五颗以 `Invalid` 拒绝，且拒绝时
//!   既不写审计也不留待办（"拒绝"必须是真的没受理，而不是受理后再标失败）。
//! - **源码侧**：`executed()` 为真的名字必须出现在生产派发者文件里；为假的名字在**任何**
//!   生产源码里都不许以 `CommandKind::<名字>` 的形式出现。于是"加了派发者却忘了改判定式"
//!   与"改了判定式却没有派发者"两个方向都会红。
//!
//! 口径边界：本文件只认限定形式 `CommandKind::X`。契约 crate 自己在 `required_permission`
//! 与 `ALL` 里用的是 `Self::X`，那是声明处而非派发处，不在覆盖面内（也不该在）。

use super::*;
use qx_control::ControlError;

/// 生产派发者名单（V13 R2 第十一遍实测：这 5 个文件之外没有第二处 `CommandKind::` 生产读者）。
const EXECUTOR_SOURCES: [&str; 5] = [
    "crates/qx-cli/src/api_service.rs",
    "crates/qx-cli/src/workers.rs",
    "crates/qx-cli/src/venue_runtime/paper_worker.rs",
    "crates/qx-cli/src/venue_runtime/binance_submit.rs",
    "crates/qx-cli/src/venue_runtime/ccxt_execution.rs",
];

/// 一份源码里属于生产代码的那一段：切到文件末尾追加的测试模块之前。
///
/// 不切的话，`crates/qx-api/src/lib.rs` 那两个 `CommandKind::CancelOrder` 用例会读成
/// "取消订单有派发者"，反方向断言当场失去牙齿。
fn production_region(source: &str) -> String {
    let lines = source.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        if line.trim_end() != "#[cfg(test)]" {
            continue;
        }
        let following = lines[index + 1..]
            .iter()
            .find(|next| !next.trim().is_empty() && !next.trim_start().starts_with('#'));
        if following.is_some_and(|next| next.trim_start().starts_with("mod ")) {
            return lines[..index].join("\n");
        }
    }
    source.to_string()
}

fn production_text(rel: &str) -> String {
    production_region(&workspace_source(rel))
}

/// 全仓生产源码里以限定形式提到某个命令类型的文件（相对仓库根，便于报错时点名）。
fn dispatchers_of(kind_name: &str) -> Vec<String> {
    let token = format!("CommandKind::{kind_name}");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    all_crate_production_sources()
        .iter()
        .filter(|path| {
            let source = std::fs::read_to_string(path).expect("生产源码读取失败");
            production_region(&source).contains(&token)
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

fn command(kind: CommandKind, id: u64) -> ControlCommand {
    ControlCommand {
        command_id: id,
        request_id: format!("req-{id}"),
        operator_id: "ops".into(),
        reason: "executor coverage".into(),
        // Admin 能盖住任何一颗变体所需的权限，于是这里必然走到"有没有执行者"这一道裁决，
        // 而不是先被权限闸门判掉 —— 那样五颗被拒的变体里有三颗根本到不了判定式。
        kind,
        target: format!("target-{id}"),
        payload: BTreeMap::new(),
        permission: Permission::Admin,
        dry_run: true,
    }
}

#[test]
fn control_plane_accepts_only_the_kinds_that_have_an_executor() {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for (index, kind) in CommandKind::ALL.iter().cloned().enumerate() {
        let mut plane = ControlPlane::default();
        let id = 4_800 + index as u64;
        let name = format!("{kind:?}");
        match plane.submit(command(kind, id), 10) {
            Ok(record) => {
                assert_eq!(record.status, CommandStatus::Accepted);
                assert_eq!(plane.pending().count(), 1, "{name} 受理后应有一条待办");
                accepted.push(name);
            }
            Err(ControlError::Invalid(reason)) => {
                assert!(
                    reason.contains("没有派发者") && reason.contains(&name),
                    "拒绝理由必须点名是哪颗变体没有执行者: {reason}"
                );
                assert!(plane.audit().is_empty(), "{name} 被拒后不该留下审计记录");
                assert_eq!(plane.pending().count(), 0, "{name} 被拒后不该留下待办");
                rejected.push(name);
            }
            Err(other) => panic!("{name} 应以 Invalid 拒绝，实际得到 {other:?}"),
        }
    }
    assert_eq!(
        accepted,
        ["SubmitOrder", "PauseStrategy", "ResumeStrategy"],
        "受理名单变了却没同步执行者：{accepted:?}"
    );
    assert_eq!(
        rejected,
        [
            "ChangeRiskLimit",
            "CancelOrder",
            "ReconcileAccount",
            "RetryJob",
            "SwitchVenue"
        ],
        "拒绝名单变了：{rejected:?}"
    );
}

#[test]
fn control_command_kinds_match_executors() {
    for rel in EXECUTOR_SOURCES {
        assert!(
            production_text(rel).contains("CommandKind::"),
            "{rel} 已经不派发任何控制命令了，把它从派发者名单里删掉"
        );
    }
    let named = EXECUTOR_SOURCES
        .iter()
        .filter(|rel| production_text(rel).contains("CommandKind::"))
        .count();
    assert!(
        named >= 3,
        "派发者名单里只剩 {named} 份文件还提到命令类型，名单本身已经腐坏"
    );
    for kind in CommandKind::ALL {
        let found = dispatchers_of(&format!("{kind:?}"));
        if kind.executed() {
            assert!(
                !found.is_empty(),
                "{kind:?} 声称有执行者，但生产源码里没有任何地方派发它: {found:?}"
            );
        } else {
            assert!(
                found.is_empty(),
                "{kind:?} 被判定式排除在执行者之外，生产源码里却有人派发它，判定式落后了: {found:?}"
            );
        }
    }
}

#[test]
fn command_kind_all_enumerates_every_variant_in_the_source() {
    let source = workspace_source("crates/qx-control/src/lib.rs");
    let start = source
        .find("pub enum CommandKind {")
        .expect("找不到 CommandKind 声明");
    let body = &source[start..];
    let end = body.find("\n}").expect("CommandKind 没有列首收尾花括号");
    let variants = body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("pub enum")
                && !line.starts_with('#')
                && !line.starts_with("//")
        })
        .map(|line| line.trim_end_matches(','))
        .collect::<Vec<_>>();
    assert_eq!(
        variants.len(),
        CommandKind::ALL.len(),
        "源码里的变体数与 ALL 名单不平（源码 {variants:?}），新增变体要同时进 ALL"
    );
    for (index, name) in variants.iter().enumerate() {
        assert_eq!(
            format!("{:?}", CommandKind::ALL[index]),
            *name,
            "ALL 的顺序与源码不一致"
        );
    }
}

/// 接口文档那两行名单必须与 `executed()` 说的同一件事。
///
/// 判据按行取，不按全文子串：八颗的名字在文档里到处出现（`SubmitOrder` 更是贯穿全篇），
/// 数子串等于不判。只认这两行前缀，且要求八颗里被点到的那些**恰好**等于该档的名单，
/// 于是"给一颗改了档位而文档没跟着改"与"同一颗写进两行"都会红在那颗名字上。
#[test]
fn control_plane_acceptance_kinds_are_listed_in_the_ops_doc() {
    let doc = workspace_source("deploy/README.md");
    let listed = |prefix: &str| -> Vec<String> {
        let line = doc
            .lines()
            .find(|row| row.starts_with(prefix))
            .unwrap_or_else(|| panic!("运维文档里找不到以 {prefix} 开头的那一行名单"));
        CommandKind::ALL
            .iter()
            .map(|kind| format!("{kind:?}"))
            .filter(|name| line.contains(&format!("`{name}`")))
            .collect()
    };
    let accepted = listed("- 有派发者、能被受理的：");
    let rejected = listed("- 没有派发者、提交即以 400 拒绝的：");
    let expected = |want: bool| -> Vec<String> {
        CommandKind::ALL
            .iter()
            .filter(|kind| kind.executed() == want)
            .map(|kind| format!("{kind:?}"))
            .collect()
    };
    assert_eq!(
        accepted,
        expected(true),
        "运维文档的受理名单与 `executed()` 不再同口径（文档 {accepted:?}）"
    );
    assert_eq!(
        rejected,
        expected(false),
        "运维文档的拒绝名单与 `executed()` 不再同口径（文档 {rejected:?}）"
    );
    let overlap = accepted
        .iter()
        .filter(|name| rejected.contains(name))
        .collect::<Vec<_>>();
    assert!(
        overlap.is_empty(),
        "同一颗命令被同时列进受理与拒绝两档: {overlap:?}"
    );
}
