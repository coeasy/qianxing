//! 控制面契约。
//!
//! 所有写操作都先变成带权限、原因和请求 ID 的 `ControlCommand`，由上层执行器
//! 再决定如何调用策略运行时、OMS 或 Scheduler。本 crate 不提供绕过风控的快捷写入。

use qx_core::{Fnv1a, Order, OrderStatus, QxError, QxResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommandStatus {
    Accepted,
    Rejected,
    Executed,
    Failed,
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

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ControlPlane {
    commands: BTreeMap<u64, ControlCommand>,
    requests: BTreeMap<String, u64>,
    audit: Vec<AuditRecord>,
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
        Ok(record)
    }

    pub fn command(&self, command_id: u64) -> Option<&ControlCommand> {
        self.commands.get(&command_id)
    }

    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }

    pub fn pending(&self) -> impl Iterator<Item = &ControlCommand> {
        self.commands.values().filter(|command| {
            !self.audit.iter().rev().any(|record| {
                record.command_id == command.command_id
                    && matches!(
                        record.status,
                        CommandStatus::Rejected | CommandStatus::Executed | CommandStatus::Failed
                    )
            })
        })
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
        if matches!(
            prior,
            CommandStatus::Rejected | CommandStatus::Executed | CommandStatus::Failed
        ) {
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
            match (audit_state.get(&record.command_id).copied(), record.status) {
                (None, CommandStatus::Accepted) => {
                    audit_state.insert(record.command_id, CommandStatus::Accepted);
                }
                (None, _) => {
                    return Err(format!("命令 {} 缺少 Accepted 初始记录", record.command_id));
                }
                (
                    Some(CommandStatus::Accepted),
                    CommandStatus::Rejected | CommandStatus::Executed | CommandStatus::Failed,
                ) => {
                    audit_state.insert(record.command_id, record.status);
                }
                (Some(CommandStatus::Accepted), CommandStatus::Accepted) => {
                    return Err(format!("命令 {} 重复 Accepted", record.command_id));
                }
                (
                    Some(CommandStatus::Rejected | CommandStatus::Executed | CommandStatus::Failed),
                    _,
                ) => {
                    return Err(format!("命令 {} 终态后仍有审计记录", record.command_id));
                }
            }
        }
        for command_id in plane.commands.keys() {
            if !matches!(
                audit_state.get(command_id),
                Some(
                    CommandStatus::Accepted
                        | CommandStatus::Rejected
                        | CommandStatus::Executed
                        | CommandStatus::Failed,
                )
            ) {
                return Err(format!("命令 {} 缺少审计记录", command_id));
            }
        }
        Ok(plane)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SubscriptionCursor {
    pub stream: String,
    pub last_seq: u64,
    pub state_hash: u64,
}

impl SubscriptionCursor {
    pub fn requires_snapshot(&self, next_seq: u64) -> bool {
        next_seq != self.last_seq.saturating_add(1)
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
            kind: CommandKind::CancelOrder,
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

    #[test]
    fn cursor_detects_lost_events() {
        let cursor = SubscriptionCursor {
            stream: "orders".into(),
            last_seq: 3,
            state_hash: 42,
        };
        assert!(!cursor.requires_snapshot(4));
        assert!(cursor.requires_snapshot(6));
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
}
