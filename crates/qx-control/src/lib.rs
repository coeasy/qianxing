//! 控制面契约。
//!
//! 所有写操作都先变成带权限、原因和请求 ID 的 `ControlCommand`，由上层执行器
//! 再决定如何调用策略运行时、OMS 或 Scheduler。本 crate 不提供绕过风控的快捷写入。

use qx_core::{Fnv1a, Order, OrderStatus, QxError, QxResult};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Permission {
    ReadOnly,
    Research,
    Trading,
    Admin,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommandKind {
    SubmitOrder,
    PauseStrategy,
    ResumeStrategy,
    ChangeRiskLimit,
    CancelOrder,
    ReconcileAccount,
    RetryJob,
    SwitchVenue,
}

impl CommandKind {
    fn required_permission(&self) -> Permission {
        match self {
            Self::RetryJob => Permission::Research,
            Self::SubmitOrder | Self::ReconcileAccount => Permission::Trading,
            Self::PauseStrategy | Self::ResumeStrategy | Self::CancelOrder => Permission::Trading,
            Self::ChangeRiskLimit | Self::SwitchVenue => Permission::Admin,
        }
    }

    /// 这一份构建里**真的有执行者**的命令类型：提交入口按它裁决，其余变体在仓内没有任何
    /// 派发者，接受了只会以 `Accepted` 永远停在审计里（新增变体默认落在"拒绝"这一侧）。
    /// 判定式与执行者文件的对应关系由常驻用例 `control_command_kinds_match_executors` 钉住。
    pub fn executed(&self) -> bool {
        matches!(
            self,
            Self::SubmitOrder | Self::PauseStrategy | Self::ResumeStrategy
        )
    }

    /// 全部 8 颗变体，供用例与文档按序遍历（新增变体必须在这里出现，否则用例会红）。
    pub const ALL: [CommandKind; 8] = [
        Self::SubmitOrder,
        Self::PauseStrategy,
        Self::ResumeStrategy,
        Self::ChangeRiskLimit,
        Self::CancelOrder,
        Self::ReconcileAccount,
        Self::RetryJob,
        Self::SwitchVenue,
    ];
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ControlCommand {
    pub command_id: u64,
    pub request_id: String,
    pub operator_id: String,
    pub reason: String,
    pub kind: CommandKind,
    pub target: String,
    pub payload: BTreeMap<String, String>,
    pub permission: Permission,
    pub dry_run: bool,
}

impl ControlCommand {
    pub fn validate(&self) -> Result<(), ControlError> {
        if self.request_id.trim().is_empty()
            || self.operator_id.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.target.trim().is_empty()
        {
            return Err(ControlError::Invalid("控制命令缺少审计字段".into()));
        }
        if !has_permission(self.permission, self.kind.required_permission()) {
            return Err(ControlError::Forbidden);
        }
        Ok(())
    }

    /// 校验请求方声明的权限是否真的不超过服务端授予的权限。
    ///
    /// `permission` 是审计字段，不是认证凭据；生产入口必须把外部身份解析成
    /// `granted` 后调用本方法，不能只调用无身份的 `validate`。
    pub fn validate_as(&self, granted: Permission) -> Result<(), ControlError> {
        self.validate()?;
        if !has_permission(granted, self.permission)
            || !has_permission(granted, self.kind.required_permission())
        {
            return Err(ControlError::Forbidden);
        }
        Ok(())
    }

    pub fn digest(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u64(self.command_id);
        h.write_text(&self.request_id);
        h.write_text(&self.operator_id);
        h.write_text(&self.reason);
        h.write_text(&format!("{:?}", self.kind));
        h.write_text(&self.target);
        for (key, value) in &self.payload {
            h.write_text(key);
            h.write_text(value);
        }
        h.write_u64(self.dry_run as u64);
        h.finish()
    }
}

/// 控制命令的审计状态。只有三个变体：受理，以及执行后的两种终态。
///
/// 这里**没有** `Rejected`（V13 R1-H 重落 V11 R6-1，那份交付面被合流 c07ad22 覆盖掉了）。
/// 拒绝发生在受理之前——`submit`/`submit_as` 校验不过就返回 `ControlError`，命令与审计记录
/// 都不会落盘，所以"已拒绝"从来不是一个能被写出来的审计状态。留着一个零构造者的变体，
/// 会让每个 `match` 都多一条永不为真的臂，读代码的人得逐个去证它走不到。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommandStatus {
    Accepted,
    Executed,
    Failed,
}

impl CommandStatus {
    /// 表达式位置上的唯一终态判据：待办筛选、`execute` 的 `AlreadyFinal`、恢复校验与
    /// 调用方的"这条命令还动得了吗"共用这一颗。
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Executed | Self::Failed)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AuditRecord {
    pub command_id: u64,
    pub request_id: String,
    pub operator_id: String,
    pub command_digest: u64,
    pub status: CommandStatus,
    pub result_code: String,
    pub ts: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlError {
    Invalid(String),
    Forbidden,
    DuplicateRequest(String),
    DuplicateCommand(u64),
    UnknownCommand(u64),
    AlreadyFinal(u64),
}

/// 从已通过控制面校验的 SubmitOrder 载荷解析订单，并再次校验命令身份边界。
///
/// 这是控制命令与领域订单之间的唯一解析入口，放在控制面契约层，避免
/// Runtime、Execution 和 CLI 各自复制一套 `order_json`/target 校验逻辑。
pub fn order_from_submit_command(command: &ControlCommand) -> QxResult<Order> {
    command.validate().map_err(|error| {
        QxError::BusinessViolation(format!("SubmitOrder 控制命令非法: {error:?}"))
    })?;
    if command.kind != CommandKind::SubmitOrder {
        return Err(QxError::BusinessViolation(
            "控制命令不是 SubmitOrder".into(),
        ));
    }
    let payload = command
        .payload
        .get("order_json")
        .ok_or_else(|| QxError::BusinessViolation("SubmitOrder 缺少 order_json".into()))?;
    let order: Order = serde_json::from_str(payload)
        .map_err(|error| QxError::BusinessViolation(format!("order_json 非法: {error}")))?;
    if command.target != order.client_id.to_string() {
        return Err(QxError::BusinessViolation(
            "SubmitOrder target 与 order.client_id 不一致".into(),
        ));
    }
    order.validate().map_err(QxError::BusinessViolation)?;
    if !matches!(
        order.status,
        OrderStatus::PendingSubmit | OrderStatus::Submitted
    ) {
        return Err(QxError::BusinessViolation(
            "SubmitOrder 只接受 PendingSubmit 或 Submitted 订单".into(),
        ));
    }
    Ok(order)
}

/// 控制面终态命令的保留上界。`commands` / `requests` / `audit` 三格只追加不退场，长跑进程
/// 每受理一条命令就永久留下三处条目；越过上界按「终态时间最旧」退场（V13 R1-H 重落 V11 R5-1，
/// 那份交付面被合流 c07ad22 按上游树整体裁定覆盖掉了）。
///
/// 只退**终态**命令：`pending()` 靠 `finalized_command_ids()` 判待办，退掉一条未终态的命令等于
/// 把一条已受理、还没人执行的命令静默丢掉。三格必须同时删——`from_json` 逐条核对「审计记录
/// 指向的命令存在」与「每条命令至少一条审计记录」，只删一边，重启恢复就会拒读自己写的历史。
pub const FINALIZED_COMMAND_RETENTION: usize = 4096;

/// 退场摘要。控制面的**持久**审计在 `qx-storage`（`AuditFileStore`/`SqliteAuditStore`/
/// `PostgresAuditStore` + 哈希链），这一份只是内存
/// 工作集；退场把它压回上界之内，而压掉了多少必须有个能被读到的数——否则「有界」这句话没有
/// 证据面。读者是 `/metrics` 的 `qx_control_retired_commands_total` 与
/// `qx_control_retired_audit_records_total` 两条，以及持久化 JSON 里的同名字段。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetirementSummary {
    pub retired_commands: u64,
    pub retired_audit_records: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ControlPlane {
    commands: BTreeMap<u64, ControlCommand>,
    requests: BTreeMap<String, u64>,
    audit: Vec<AuditRecord>,
    /// 累计退场量。`#[serde(default)]`：合流之前落盘的控制面 JSON 没有这一格，恢复时必须
    /// 读得回来，否则一次升级就拒读自己的历史。
    #[serde(default)]
    retired: RetirementSummary,
}

impl ControlPlane {
    pub fn submit(
        &mut self,
        command: ControlCommand,
        ts: u64,
    ) -> Result<AuditRecord, ControlError> {
        command.validate()?;
        self.submit_validated(command, ts)
    }

    /// 带服务端授权上下文的提交入口。API/Worker 应优先使用此方法。
    pub fn submit_as(
        &mut self,
        command: ControlCommand,
        granted: Permission,
        ts: u64,
    ) -> Result<AuditRecord, ControlError> {
        command.validate_as(granted)?;
        self.submit_validated(command, ts)
    }

    fn submit_validated(
        &mut self,
        command: ControlCommand,
        ts: u64,
    ) -> Result<AuditRecord, ControlError> {
        // 只有提交入口裁决"有没有执行者"：`ControlCommand::validate` 同时被恢复路径调用，
        // 把这条规则放进 `validate` 会让历史队列里已存在的旧命令在重启时读不回来。
        if !command.kind.executed() {
            return Err(ControlError::Invalid(format!(
                "{:?} 在当前构建里没有派发者，控制面不接受",
                command.kind
            )));
        }
        if self.requests.contains_key(&command.request_id) {
            return Err(ControlError::DuplicateRequest(command.request_id));
        }
        if self.commands.contains_key(&command.command_id) {
            return Err(ControlError::DuplicateCommand(command.command_id));
        }
        let record = AuditRecord {
            command_id: command.command_id,
            request_id: command.request_id.clone(),
            operator_id: command.operator_id.clone(),
            command_digest: command.digest(),
            status: CommandStatus::Accepted,
            result_code: "ACCEPTED_FOR_EXECUTION".into(),
            ts,
        };
        self.requests
            .insert(command.request_id.clone(), command.command_id);
        self.commands.insert(command.command_id, command);
        self.audit.push(record.clone());
        self.retire_overflow();
        Ok(record)
    }

    pub fn command(&self, command_id: u64) -> Option<&ControlCommand> {
        self.commands.get(&command_id)
    }

    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }

    /// 待办命令。判据与 `execute` 的 `AlreadyFinal` 同一口径：终态审计记录一旦落下，
    /// 这条命令就不再是待办。
    ///
    /// 终态集合每次现算一遍：调用方是四个 worker 的轮询循环，而 `audit` 只追加不退场，
    /// 逐条命令回扫整段审计会把每轮轮询打成 O(commands × audit)（V13 R1-D）。
    pub fn pending(&self) -> impl Iterator<Item = &ControlCommand> {
        let finalized = self.finalized_command_ids();
        self.commands
            .values()
            .filter(move |command| !finalized.contains(&command.command_id))
    }

    /// 已落终态审计记录的 command_id 集合。`execute` 对已终态的命令回 `AlreadyFinal`，
    /// 所以每条命令至多一条终态记录——“有没有终态记录”与“最新那条是不是终态”同解。
    fn finalized_command_ids(&self) -> BTreeSet<u64> {
        self.audit
            .iter()
            .filter(|record| record.status.is_final())
            .map(|record| record.command_id)
            .collect()
    }

    /// 累计退场摘要。`/metrics` 读它，运维因此能看见「控制面确实被压回上界之内」。
    pub fn retirement(&self) -> RetirementSummary {
        self.retired
    }

    fn retire_overflow(&mut self) {
        self.retire_overflow_with(FINALIZED_COMMAND_RETENTION)
    }

    /// 上界之外的终态命令按终态时间最旧退场，命令、请求索引、审计记录三格同时删。
    ///
    /// 第一句就是规模判据：`finalized ⊆ commands`，所以命令数没越过上界时终态数也不可能越过，
    /// 直接返回。少了这句，每次 `submit`/`execute` 都要重扫整段审计并重建一个 `BTreeSet`，
    /// 受理 n 条命令的总代价是 O(n²)——那正是 `pending()` 在 R1-D 已经躲开的那种形状。
    fn retire_overflow_with(&mut self, retention: usize) {
        if self.commands.len() <= retention {
            return;
        }
        let finalized = self.finalized_command_ids();
        if finalized.len() <= retention {
            return;
        }
        // 每条终态命令至多一条终态记录（`execute` 对已终态命令回 `AlreadyFinal`），
        // 所以「按终态记录的 ts 升序」就是「按退场优先级升序」，同 ts 按 command_id 定序。
        let mut finalized_at: Vec<(u64, u64)> = self
            .audit
            .iter()
            .filter(|record| record.status.is_final())
            .map(|record| (record.ts, record.command_id))
            .collect();
        finalized_at.sort_unstable();
        let overflow = finalized_at.len() - retention;
        let victims: BTreeSet<u64> = finalized_at
            .into_iter()
            .take(overflow)
            .map(|(_, command_id)| command_id)
            .collect();
        let mut removed_records = 0u64;
        self.audit.retain(|record| {
            if victims.contains(&record.command_id) {
                removed_records += 1;
                false
            } else {
                true
            }
        });
        for command_id in &victims {
            if let Some(command) = self.commands.remove(command_id) {
                self.requests.remove(&command.request_id);
            }
        }
        self.retired.retired_commands += victims.len() as u64;
        self.retired.retired_audit_records += removed_records;
    }

    /// 执行器唯一的状态入口：执行结果必须回写审计记录，不能只返回字符串。
    pub fn execute<F>(
        &mut self,
        command_id: u64,
        ts: u64,
        action: F,
    ) -> Result<AuditRecord, ControlError>
    where
        F: FnOnce(&ControlCommand) -> Result<String, String>,
    {
        let command = self
            .commands
            .get(&command_id)
            .ok_or(ControlError::UnknownCommand(command_id))?;
        let prior = self
            .audit
            .iter()
            .rev()
            .find(|record| record.command_id == command_id)
            .map(|record| record.status)
            .ok_or(ControlError::UnknownCommand(command_id))?;
        if prior.is_final() {
            return Err(ControlError::AlreadyFinal(command_id));
        }
        let (status, result_code) = match action(command) {
            Ok(code) => (CommandStatus::Executed, code),
            Err(code) => (CommandStatus::Failed, code),
        };
        let record = AuditRecord {
            command_id,
            request_id: command.request_id.clone(),
            operator_id: command.operator_id.clone(),
            command_digest: command.digest(),
            status,
            result_code,
            ts,
        };
        self.audit.push(record.clone());
        self.retire_overflow();
        Ok(record)
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let plane: Self = serde_json::from_str(input).map_err(|error| error.to_string())?;
        for (command_id, command) in &plane.commands {
            if *command_id != command.command_id {
                return Err("命令索引与 command_id 不一致".into());
            }
            command
                .validate()
                .map_err(|error| format!("恢复的控制命令非法: {error:?}"))?;
            if plane.requests.get(&command.request_id) != Some(command_id) {
                return Err(format!("命令 {} 缺少一致的 request 索引", command_id));
            }
        }
        for (request_id, command_id) in &plane.requests {
            let command = plane
                .commands
                .get(command_id)
                .ok_or_else(|| format!("请求 {} 指向不存在的命令", request_id))?;
            if &command.request_id != request_id {
                return Err("请求索引与命令不一致".into());
            }
        }
        let mut audit_state = BTreeMap::new();
        for record in &plane.audit {
            let command = plane
                .commands
                .get(&record.command_id)
                .ok_or_else(|| format!("审计记录指向不存在的命令 {}", record.command_id))?;
            if record.request_id != command.request_id
                || record.operator_id != command.operator_id
                || record.command_digest != command.digest()
            {
                return Err(format!("命令 {} 的审计摘要不一致", record.command_id));
            }
            // 状态词表在这里只出现一次：`is_final()`。以前这个 match 把三个终态变体又抄了
            // 两遍（模式位置调不进 `is_final`），加变体就得同步改三处，漏一处恢复校验就松。
            match audit_state.get(&record.command_id).copied() {
                None if !record.status.is_final() => {
                    audit_state.insert(record.command_id, record.status);
                }
                None => {
                    return Err(format!("命令 {} 缺少 Accepted 初始记录", record.command_id));
                }
                Some(prior) if prior.is_final() => {
                    return Err(format!("命令 {} 终态后仍有审计记录", record.command_id));
                }
                Some(_) if record.status.is_final() => {
                    audit_state.insert(record.command_id, record.status);
                }
                Some(_) => {
                    return Err(format!("命令 {} 重复 Accepted", record.command_id));
                }
            }
        }
        // 每条命令都必须留下审计痕迹。`audit_state` 的键只可能来自上面已核对过"命令存在"
        // 的记录，所以这里问"有没有"就够了，不必再抄一遍状态词表。
        for command_id in plane.commands.keys() {
            if !audit_state.contains_key(command_id) {
                return Err(format!("命令 {} 缺少审计记录", command_id));
            }
        }
        Ok(plane)
    }
}

fn has_permission(actual: Permission, required: Permission) -> bool {
    let level = |permission| match permission {
        Permission::ReadOnly => 0,
        Permission::Research => 1,
        Permission::Trading => 2,
        Permission::Admin => 3,
    };
    level(actual) >= level(required)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(permission: Permission) -> ControlCommand {
        ControlCommand {
            command_id: 1,
            request_id: "req-1".into(),
            operator_id: "operator".into(),
            reason: "incident recovery".into(),
            // PauseStrategy 而不是 CancelOrder：提交入口只接受有派发者的命令类型。
            kind: CommandKind::PauseStrategy,
            target: "strategy-1".into(),
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

    fn numbered(index: u64) -> ControlCommand {
        ControlCommand {
            command_id: index,
            request_id: format!("req-{index}"),
            operator_id: "operator".into(),
            reason: "retention probe".into(),
            kind: CommandKind::PauseStrategy,
            target: "strategy-1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        }
    }

    /// 退场只碰终态命令，且命令/请求索引/审计三格同时消失——少删任何一格，
    /// `from_json` 的恢复校验就会拒读，所以末尾那一次往返是这颗用例的牙齿。
    #[test]
    fn retirement_drops_oldest_finalized_commands_whole() {
        let mut plane = ControlPlane::default();
        for index in 1..=4u64 {
            plane.submit(numbered(index), index * 10).unwrap();
        }
        for index in 1..=4u64 {
            plane
                .execute(index, index * 10 + 1, |_| Ok("APPLIED".into()))
                .unwrap();
        }
        assert_eq!(plane.retirement(), RetirementSummary::default());

        plane.retire_overflow_with(2);

        assert_eq!(
            plane.retirement(),
            RetirementSummary {
                retired_commands: 2,
                // 每条命令两条记录（Accepted + 终态），退场必须把两条一起带走。
                retired_audit_records: 4,
            }
        );
        let remaining: Vec<u64> = plane.audit().iter().map(|r| r.command_id).collect();
        // 审计是追加序：四条 Accepted 先全部落盘，四条终态记录再依次跟上，
        // 所以幸存者是 [3, 4, 3, 4] 而不是 [3, 3, 4, 4]。写死这个顺序等于把
        // 「Accepted 与终态是两次独立 push」这条事实也钉住。
        assert_eq!(remaining, vec![3, 4, 3, 4]);
        assert!(plane.command(1).is_none() && plane.command(2).is_none());
        assert!(plane.command(3).is_some() && plane.command(4).is_some());
        assert_eq!(plane.pending().count(), 0);
        // 恢复校验逐条核对索引一致性：三格里漏删任何一格，这一句就红。
        let json = plane.to_json().unwrap();
        let restored = ControlPlane::from_json(&json).unwrap();
        assert_eq!(restored.retirement(), plane.retirement());
        assert_eq!(restored.audit().len(), 4);
    }

    /// 未终态的命令永不退场：把它退掉等于静默丢掉一条已受理、还没人执行的命令。
    #[test]
    fn retirement_never_drops_a_pending_command() {
        let mut plane = ControlPlane::default();
        for index in 1..=6u64 {
            plane.submit(numbered(index), index).unwrap();
        }
        for index in 1..=5u64 {
            plane
                .execute(index, index + 100, |_| Ok("APPLIED".into()))
                .unwrap();
        }
        plane.retire_overflow_with(1);
        assert_eq!(plane.retirement().retired_commands, 4);
        assert!(plane.command(6).is_some(), "待办命令被退场了");
        assert_eq!(plane.pending().count(), 1);
        assert_eq!(plane.audit().len(), 3);
    }

    /// 生产入口真的会触发退场：上界是常量，`submit`/`execute` 每次都过一遍这条判据。
    /// 少了这颗，`retire_overflow` 可以接上却永不被越过，「有界」只是写法不是事实。
    #[test]
    fn production_entries_enforce_the_retention_bound() {
        let mut plane = ControlPlane::default();
        let bound = FINALIZED_COMMAND_RETENTION as u64;
        for index in 1..=bound + 1 {
            plane.submit(numbered(index), index).unwrap();
            plane
                .execute(index, index, |_| Ok("APPLIED".into()))
                .unwrap();
        }
        assert_eq!(plane.retirement().retired_commands, 1);
        assert_eq!(plane.retirement().retired_audit_records, 2);
        assert!(plane.command(1).is_none(), "最旧那条终态命令没退场");
        assert!(plane.command(bound + 1).is_some());
        assert!(ControlPlane::from_json(&plane.to_json().unwrap()).is_ok());
    }

    /// 合流之前落盘的控制面 JSON 没有 `retired` 这一格：恢复必须照样读得回来。
    #[test]
    fn restore_accepts_a_plane_written_before_the_summary_existed() {
        let mut plane = ControlPlane::default();
        plane.submit(numbered(1), 10).unwrap();
        plane.execute(1, 11, |_| Ok("APPLIED".into())).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&plane.to_json().unwrap()).unwrap();
        value
            .as_object_mut()
            .expect("plane serializes as an object")
            .remove("retired");
        let legacy = value.to_string();
        let restored = ControlPlane::from_json(&legacy).unwrap();
        assert_eq!(restored.retirement(), RetirementSummary::default());
        assert_eq!(restored.audit().len(), 2);
    }
}
