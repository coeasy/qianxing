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
    SubmitOrderGroup,
    AmendOrder,
    PauseStrategy,
    ResumeStrategy,
    ChangeRiskLimit,
    CancelOrder,
    ReconcileAccount,
    RetryJob,
    SwitchVenue,
    CancelOrderGroup,
}

impl CommandKind {
    fn required_permission(&self) -> Permission {
        match self {
            Self::RetryJob => Permission::Research,
            Self::SubmitOrder
            | Self::SubmitOrderGroup
            | Self::AmendOrder
            | Self::ReconcileAccount => Permission::Trading,
            Self::PauseStrategy
            | Self::ResumeStrategy
            | Self::CancelOrder
            | Self::CancelOrderGroup => Permission::Trading,
            Self::ChangeRiskLimit | Self::SwitchVenue => Permission::Admin,
        }
    }

    /// 这一份构建里**真的有执行者**的命令类型：提交/改单/撤单 worker 按它裁决，其余变体在仓内没有任何
    /// 派发者，接受了只会以 `Accepted` 永远停在审计里（新增变体默认落在"拒绝"这一侧）。
    /// 判定式与执行者文件的对应关系由常驻用例 `control_command_kinds_match_executors` 钉住。
    pub fn executed(&self) -> bool {
        matches!(
            self,
            Self::SubmitOrder
                | Self::SubmitOrderGroup
                | Self::AmendOrder
                | Self::CancelOrder
                | Self::CancelOrderGroup
                | Self::PauseStrategy
                | Self::ResumeStrategy
        )
    }

    /// 全部变体，供用例与文档按序遍历（新增变体必须在这里出现，否则用例会红）。
    pub const ALL: [CommandKind; 11] = [
        Self::SubmitOrder,
        Self::SubmitOrderGroup,
        Self::AmendOrder,
        Self::PauseStrategy,
        Self::ResumeStrategy,
        Self::ChangeRiskLimit,
        Self::CancelOrder,
        Self::ReconcileAccount,
        Self::RetryJob,
        Self::SwitchVenue,
        Self::CancelOrderGroup,
    ];
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// CancelOrder 的路由声明。账户和 venue 必须由实时 worker 再与 EventLog 中的权威订单核对。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelOrderTarget {
    pub client_order_id: u64,
    pub account_id: String,
    pub venue_id: String,
}

/// CancelOrderGroup resolves its group ID against the worker's durable order-group store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelOrderGroupTarget {
    pub group_id: String,
    pub account_id: String,
    pub venue_id: String,
}

fn parse_cancel_order_group_target(
    command: &ControlCommand,
) -> Result<CancelOrderGroupTarget, ControlError> {
    if command.kind != CommandKind::CancelOrderGroup {
        return Err(ControlError::Invalid(
            "控制命令不是 CancelOrderGroup".into(),
        ));
    }
    let group_id = command.target.trim();
    if group_id.is_empty() || group_id.len() > 128 {
        return Err(ControlError::Invalid(
            "CancelOrderGroup target 必须是 1 至 128 字符的 group_id".into(),
        ));
    }
    if command.payload.len() != 2
        || !command.payload.contains_key("account_id")
        || !command.payload.contains_key("venue_id")
    {
        return Err(ControlError::Invalid(
            "CancelOrderGroup payload 必须且只能包含 account_id 和 venue_id".into(),
        ));
    }
    let account_id = command.payload["account_id"].trim();
    let venue_id = command.payload["venue_id"].trim();
    if account_id.is_empty() || venue_id.is_empty() {
        return Err(ControlError::Invalid(
            "CancelOrderGroup account_id/venue_id 不能为空".into(),
        ));
    }
    Ok(CancelOrderGroupTarget {
        group_id: group_id.to_owned(),
        account_id: account_id.to_owned(),
        venue_id: venue_id.to_owned(),
    })
}

pub fn cancel_order_group_target_from_command(
    command: &ControlCommand,
) -> Result<CancelOrderGroupTarget, ControlError> {
    command.validate()?;
    parse_cancel_order_group_target(command)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmendOrderTarget {
    pub client_order_id: u64,
    pub account_id: String,
    pub venue_id: String,
    pub order: Order,
}

fn parse_cancel_order_target(command: &ControlCommand) -> Result<CancelOrderTarget, ControlError> {
    if command.kind != CommandKind::CancelOrder {
        return Err(ControlError::Invalid("控制命令不是 CancelOrder".into()));
    }
    let client_order_id = command
        .target
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            ControlError::Invalid("CancelOrder target 必须是正整数 client_order_id".into())
        })?;
    if command.payload.len() != 2
        || !command.payload.contains_key("account_id")
        || !command.payload.contains_key("venue_id")
    {
        return Err(ControlError::Invalid(
            "CancelOrder payload 必须且只能包含 account_id 和 venue_id".into(),
        ));
    }
    let account_id = command.payload["account_id"].trim();
    let venue_id = command.payload["venue_id"].trim();
    if account_id.is_empty() || venue_id.is_empty() {
        return Err(ControlError::Invalid(
            "CancelOrder account_id/venue_id 不能为空".into(),
        ));
    }
    Ok(CancelOrderTarget {
        client_order_id,
        account_id: account_id.to_owned(),
        venue_id: venue_id.to_owned(),
    })
}

/// Parse the explicit routing identity for a cancellation command. The execution worker must
/// independently bind this identity to the authoritative order snapshot before contacting a venue.
pub fn cancel_order_target_from_command(
    command: &ControlCommand,
) -> Result<CancelOrderTarget, ControlError> {
    command.validate()?;
    parse_cancel_order_target(command)
}

fn parse_amend_order_target(command: &ControlCommand) -> Result<AmendOrderTarget, ControlError> {
    if command.kind != CommandKind::AmendOrder {
        return Err(ControlError::Invalid("控制命令不是 AmendOrder".into()));
    }
    let client_order_id = command
        .target
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| ControlError::Invalid("AmendOrder target 必须是正整数订单 ID".into()))?;
    if command.payload.len() != 3
        || !command.payload.contains_key("account_id")
        || !command.payload.contains_key("venue_id")
        || !command.payload.contains_key("order_json")
    {
        return Err(ControlError::Invalid(
            "AmendOrder payload 必须且只能包含 account_id、venue_id、order_json".into(),
        ));
    }
    let account_id = command.payload["account_id"].trim();
    let venue_id = command.payload["venue_id"].trim();
    if account_id.is_empty() || venue_id.is_empty() {
        return Err(ControlError::Invalid(
            "AmendOrder account_id/venue_id 不能为空".into(),
        ));
    }
    let order: Order = serde_json::from_str(&command.payload["order_json"])
        .map_err(|error| ControlError::Invalid(format!("AmendOrder order_json 非法: {error}")))?;
    order.validate().map_err(|error| {
        ControlError::Invalid(format!("AmendOrder order_json 校验失败: {error}"))
    })?;
    if order.client_id != client_order_id
        || order.account_id != account_id
        || !order
            .instrument
            .venue
            .as_str()
            .eq_ignore_ascii_case(venue_id)
        || !matches!(
            order.status,
            OrderStatus::Accepted | OrderStatus::Working | OrderStatus::PartiallyFilled
        )
    {
        return Err(ControlError::Invalid(
            "AmendOrder target、账户、交易所或活动订单状态不匹配".into(),
        ));
    }
    Ok(AmendOrderTarget {
        client_order_id,
        account_id: account_id.to_owned(),
        venue_id: venue_id.to_owned(),
        order,
    })
}

pub fn amend_order_target_from_command(
    command: &ControlCommand,
) -> Result<AmendOrderTarget, ControlError> {
    command.validate()?;
    parse_amend_order_target(command)
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
        if self.kind == CommandKind::CancelOrder {
            parse_cancel_order_target(self)?;
        }
        if self.kind == CommandKind::CancelOrderGroup {
            parse_cancel_order_group_target(self)?;
        }
        if self.kind == CommandKind::AmendOrder {
            parse_amend_order_target(self)?;
        }
        Ok(())
    }

    /// 校验请求方声明的权限是否真的不超过服务端授予的权限。
    ///
    /// `permission` 是审计字段，不是认证凭据；生产入口必须把外部身份解析成
    /// `granted` 后调用本方法，不能只调用无身份的 `validate`。
    pub fn validate_as(&self, granted: Permission) -> Result<(), ControlError> {
        if !has_permission(granted, self.permission)
            || !has_permission(granted, self.kind.required_permission())
        {
            return Err(ControlError::Forbidden);
        }
        self.validate()?;
        self.validate_submission_target()
    }

    fn validate_submission_target(&self) -> Result<(), ControlError> {
        if matches!(
            &self.kind,
            CommandKind::PauseStrategy | CommandKind::ResumeStrategy
        ) && self.target == "*"
        {
            return Err(ControlError::Invalid(
                "strategy lifecycle commands require an explicit worker target".into(),
            ));
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

pub const CONTROL_AUDIT_RECORD_SCHEMA_VERSION_V1: u32 = 1;
pub const CONTROL_AUDIT_PAGE_SCHEMA_VERSION_V1: u32 = 1;
pub const CONTROL_AUDIT_DEFAULT_PAGE_SIZE: usize = 100;
pub const CONTROL_AUDIT_MAX_PAGE_SIZE: usize = 1_000;
pub const CONTROL_AUDIT_MAX_PAGE_BYTES: usize = 1_048_576;

/// Versioned public response used by Paper/Live command submission and audit recovery routes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(try_from = "ControlAuditRecordV1Raw")]
pub struct ControlAuditRecordV1 {
    pub schema_version: u32,
    pub command_id: u64,
    pub request_id: String,
    pub operator_id: String,
    pub command_digest: u64,
    pub status: CommandStatus,
    pub result_code: String,
    pub ts: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlAuditRecordV1Raw {
    schema_version: u32,
    command_id: u64,
    request_id: String,
    operator_id: String,
    command_digest: u64,
    status: CommandStatus,
    result_code: String,
    ts: u64,
}

impl TryFrom<ControlAuditRecordV1Raw> for ControlAuditRecordV1 {
    type Error = &'static str;

    fn try_from(raw: ControlAuditRecordV1Raw) -> Result<Self, Self::Error> {
        if raw.schema_version != CONTROL_AUDIT_RECORD_SCHEMA_VERSION_V1 {
            return Err("unsupported control audit record schema version");
        }
        if raw.request_id.trim().is_empty() || raw.operator_id.trim().is_empty() {
            return Err("control audit record identities must not be empty");
        }
        Ok(Self {
            schema_version: raw.schema_version,
            command_id: raw.command_id,
            request_id: raw.request_id,
            operator_id: raw.operator_id,
            command_digest: raw.command_digest,
            status: raw.status,
            result_code: raw.result_code,
            ts: raw.ts,
        })
    }
}

impl From<&AuditRecord> for ControlAuditRecordV1 {
    fn from(record: &AuditRecord) -> Self {
        Self {
            schema_version: CONTROL_AUDIT_RECORD_SCHEMA_VERSION_V1,
            command_id: record.command_id,
            request_id: record.request_id.clone(),
            operator_id: record.operator_id.clone(),
            command_digest: record.command_digest,
            status: record.status,
            result_code: record.result_code.clone(),
            ts: record.ts,
        }
    }
}

/// One audit row with its durable append sequence. The sequence is separate from the legacy
/// persisted `AuditRecord` so the existing storage/audit-chain wire format remains unchanged.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlAuditEntryV1 {
    pub sequence: u64,
    pub record: ControlAuditRecordV1,
}

/// Bounded, snapshot-consistent page returned by `GET /control/audit`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(try_from = "ControlAuditPageV1Raw")]
pub struct ControlAuditPageV1 {
    pub schema_version: u32,
    pub entries: Vec<ControlAuditEntryV1>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub snapshot_through_sequence: u64,
    pub oldest_available_sequence: Option<u64>,
    pub retired_records: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlAuditPageV1Raw {
    schema_version: u32,
    entries: Vec<ControlAuditEntryV1>,
    next_cursor: Option<String>,
    has_more: bool,
    snapshot_through_sequence: u64,
    oldest_available_sequence: Option<u64>,
    retired_records: u64,
}

impl TryFrom<ControlAuditPageV1Raw> for ControlAuditPageV1 {
    type Error = &'static str;

    fn try_from(raw: ControlAuditPageV1Raw) -> Result<Self, Self::Error> {
        if raw.schema_version != CONTROL_AUDIT_PAGE_SCHEMA_VERSION_V1 {
            return Err("unsupported control audit page schema version");
        }
        if raw.entries.len() > CONTROL_AUDIT_MAX_PAGE_SIZE {
            return Err("control audit page exceeds the maximum entry count");
        }
        if raw.has_more != raw.next_cursor.is_some() {
            return Err("control audit page has_more and next_cursor disagree");
        }
        if raw.has_more && raw.entries.is_empty() {
            return Err("control audit page cannot continue without an entry");
        }
        if raw
            .entries
            .windows(2)
            .any(|pair| pair[0].sequence <= pair[1].sequence)
        {
            return Err("control audit page sequences must be strictly decreasing");
        }
        if raw
            .entries
            .iter()
            .any(|entry| entry.sequence == 0 || entry.sequence > raw.snapshot_through_sequence)
        {
            return Err("control audit entry sequence is outside the page snapshot");
        }
        if raw
            .oldest_available_sequence
            .is_some_and(|sequence| sequence == 0)
        {
            return Err("oldest available control audit sequence must be non-zero");
        }
        if raw
            .oldest_available_sequence
            .is_some_and(|sequence| sequence > raw.snapshot_through_sequence)
        {
            return Err("oldest available sequence is outside the page snapshot");
        }
        if let Some(cursor) = raw.next_cursor.as_deref() {
            let decoded = ControlAuditCursorV1::decode(cursor)?;
            if decoded.through_sequence != raw.snapshot_through_sequence
                || raw.entries.last().map(|entry| entry.sequence) != Some(decoded.before_sequence)
            {
                return Err("control audit next_cursor does not match its final entry");
            }
        }
        let page = Self {
            schema_version: raw.schema_version,
            entries: raw.entries,
            next_cursor: raw.next_cursor,
            has_more: raw.has_more,
            snapshot_through_sequence: raw.snapshot_through_sequence,
            oldest_available_sequence: raw.oldest_available_sequence,
            retired_records: raw.retired_records,
        };
        if serde_json::to_vec(&page)
            .map_err(|_| "control audit page serialization failed")?
            .len()
            > CONTROL_AUDIT_MAX_PAGE_BYTES
        {
            return Err("control audit page exceeds the maximum response byte budget");
        }
        Ok(page)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ControlAuditCursorV1 {
    before_sequence: u64,
    through_sequence: u64,
}

impl ControlAuditCursorV1 {
    fn encode(self) -> String {
        format!("v1.{}.{}", self.before_sequence, self.through_sequence)
    }

    fn decode(value: &str) -> Result<Self, &'static str> {
        let mut parts = value.split('.');
        if parts.next() != Some("v1") {
            return Err("unsupported control audit cursor version");
        }
        let before_sequence = parts
            .next()
            .ok_or("control audit cursor is incomplete")?
            .parse::<u64>()
            .map_err(|_| "control audit cursor has an invalid after sequence")?;
        let through_sequence = parts
            .next()
            .ok_or("control audit cursor is incomplete")?
            .parse::<u64>()
            .map_err(|_| "control audit cursor has an invalid snapshot sequence")?;
        if parts.next().is_some() || before_sequence > through_sequence {
            return Err("control audit cursor is invalid");
        }
        Ok(Self {
            before_sequence,
            through_sequence,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlError {
    Invalid(String),
    Forbidden,
    PendingLimitReached,
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

/// Parse and bind an atomic OCO/Bracket intent to an audited control command.
pub fn order_group_from_submit_command(
    command: &ControlCommand,
) -> QxResult<qx_core::OrderGroupIntent> {
    command.validate().map_err(|error| {
        QxError::BusinessViolation(format!("SubmitOrderGroup 控制命令非法: {error:?}"))
    })?;
    if command.kind != CommandKind::SubmitOrderGroup {
        return Err(QxError::BusinessViolation(
            "控制命令不是 SubmitOrderGroup".into(),
        ));
    }
    if command.payload.len() != 1 || !command.payload.contains_key("order_group_json") {
        return Err(QxError::BusinessViolation(
            "SubmitOrderGroup payload 必须且只能包含 order_group_json".into(),
        ));
    }
    let group: qx_core::OrderGroupIntent =
        serde_json::from_str(&command.payload["order_group_json"]).map_err(|error| {
            QxError::BusinessViolation(format!("order_group_json 非法: {error}"))
        })?;
    group.validate().map_err(QxError::BusinessViolation)?;
    if command.target != group.group_id {
        return Err(QxError::BusinessViolation(
            "SubmitOrderGroup target 与 group_id 不一致".into(),
        ));
    }
    Ok(group)
}

/// 控制面终态命令的保留上界。`commands` / `requests` / `audit` 三格只追加不退场，长跑进程
/// 每受理一条命令就永久留下三处条目；越过上界按「终态审计写入顺序最早」退场（V13 R1-H 重落 V11 R5-1，
/// 那份交付面被合流 c07ad22 按上游树整体裁定覆盖掉了）。
///
/// 只退**终态**命令：`pending()` 靠 `finalized_command_ids()` 判待办，退掉一条未终态的命令等于
/// 把一条已受理、还没人执行的命令静默丢掉。三格必须同时删——`from_json` 逐条核对「审计记录
/// 指向的命令存在」与「每条命令至少一条审计记录」，只删一边，重启恢复就会拒读自己写的历史。
pub const FINALIZED_COMMAND_RETENTION: usize = 4096;
/// Maximum number of accepted commands that have not yet reached a terminal audit status.
/// Pending work is never retired silently; new submissions fail closed until workers make room.
pub const PENDING_CONTROL_COMMAND_LIMIT: usize = 4096;

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
    /// Append sequence stored beside legacy audit rows so the existing audit-chain record shape
    /// remains unchanged. These fields are validated and migrated together on restore.
    #[serde(default)]
    audit_sequences: Vec<u64>,
    /// Per-command sequence lookup keeps request-id recovery bounded without changing the
    /// legacy audit rows. It is validated against `audit` and rebuilt for older snapshots.
    #[serde(default)]
    audit_command_sequences: BTreeMap<u64, Vec<u64>>,
    /// Last sequence ever assigned. Retiring rows never rewinds this durable watermark.
    #[serde(default)]
    audit_high_watermark: u64,
    /// Durable O(1) admission and metrics count. Older snapshots reconstruct it from audit state.
    #[serde(default)]
    pending_commands_count: u64,
    #[serde(default)]
    strategy_lifecycle: BTreeMap<String, StrategyLifecycleRecord>,
    #[serde(default)]
    strategy_lifecycle_legacy_incomplete: bool,
    /// 累计退场量。`#[serde(default)]`：合流之前落盘的控制面 JSON 没有这一格，恢复时必须
    /// 读得回来，否则一次升级就拒读自己的历史。
    #[serde(default)]
    retired: RetirementSummary,
}

/// Latest successful lifecycle command, retained independently of bounded audit history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyLifecycleRecord {
    pub paused: bool,
    pub command_id: u64,
    pub ts: u64,
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
        // Reject new ambiguous commands without making legacy stored commands unreadable.
        command.validate_submission_target()?;
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
        if self.pending_commands_count >= PENDING_CONTROL_COMMAND_LIMIT as u64 {
            return Err(ControlError::PendingLimitReached);
        }
        let sequence = self.next_audit_sequence()?;
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
        self.append_audit(sequence, record.clone());
        self.retire_overflow();
        Ok(record)
    }

    pub fn command(&self, command_id: u64) -> Option<&ControlCommand> {
        self.commands.get(&command_id)
    }

    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }

    /// Return a bounded newest-first page from a stable append-sequence snapshot. The cursor
    /// freezes the upper sequence on the first request, so new commands cannot extend a scan.
    pub fn audit_page(
        &self,
        cursor: Option<&str>,
        limit: usize,
        request_id: Option<&str>,
    ) -> Result<ControlAuditPageV1, &'static str> {
        if !(1..=CONTROL_AUDIT_MAX_PAGE_SIZE).contains(&limit) {
            return Err("control audit page limit is outside the allowed range");
        }
        if request_id.is_some() && limit < 2 {
            return Err("control audit request_id lookup requires a page limit of at least 2");
        }
        if cursor.is_some() && request_id.is_some() {
            return Err("control audit cursor cannot be combined with request_id");
        }
        let parsed_cursor = cursor.map(ControlAuditCursorV1::decode).transpose()?;
        let before_sequence = parsed_cursor.map(|value| value.before_sequence);
        let through_sequence =
            parsed_cursor.map_or(self.audit_high_watermark, |value| value.through_sequence);
        if through_sequence > self.audit_high_watermark {
            return Err("control audit cursor is ahead of the current durable watermark");
        }
        if parsed_cursor.is_some() {
            let oldest = self.audit_sequences.first().copied();
            let expired = match oldest {
                Some(oldest) => oldest >= before_sequence.unwrap_or(0),
                None => true,
            };
            if expired {
                return Err("control audit cursor has expired because older rows were retired");
            }
        }

        let mut rows = if let Some(request_id) = request_id {
            match self.requests.get(request_id).copied() {
                None => Vec::new(),
                Some(command_id) => {
                    self.audit_command_sequences
                        .get(&command_id)
                        .map(|sequences| {
                            sequences
                                .iter()
                                .copied()
                                .filter(|sequence| {
                                    *sequence <= through_sequence
                                        && before_sequence.is_none_or(|before| *sequence < before)
                                })
                                .filter_map(|sequence| {
                                    self.audit_sequences.binary_search(&sequence).ok().map(
                                        |index| (&self.audit[index], &self.audit_sequences[index]),
                                    )
                                })
                                .rev()
                                .take(limit + 1)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_else(|| {
                            // Direct Serde deserialization of legacy state may bypass `from_json`
                            // migration. Preserve correctness there; supported storage readers rebuild
                            // the index.
                            self.audit
                                .iter()
                                .zip(&self.audit_sequences)
                                .filter(|(record, sequence)| {
                                    record.request_id == request_id
                                        && **sequence <= through_sequence
                                        && before_sequence.is_none_or(|before| **sequence < before)
                                })
                                .rev()
                                .take(limit + 1)
                                .collect::<Vec<_>>()
                        })
                }
            }
        } else {
            let snapshot_end = self
                .audit_sequences
                .partition_point(|sequence| *sequence <= through_sequence);
            let page_end = before_sequence.map_or(snapshot_end, |before| {
                snapshot_end.min(
                    self.audit_sequences
                        .partition_point(|sequence| *sequence < before),
                )
            });
            let page_start = page_end.saturating_sub(limit + 1);
            (page_start..page_end)
                .rev()
                .map(|index| (&self.audit[index], &self.audit_sequences[index]))
                .collect::<Vec<_>>()
        };
        let mut has_more = rows.len() > limit;
        rows.truncate(limit);
        let entries = rows
            .iter()
            .map(|(record, sequence)| ControlAuditEntryV1 {
                sequence: **sequence,
                record: ControlAuditRecordV1::from(*record),
            })
            .collect::<Vec<_>>();
        let entry_sizes = entries
            .iter()
            .map(|entry| serde_json::to_vec(entry).map(|bytes| bytes.len()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "control audit page serialization failed")?;
        let mut count = entries.len();
        loop {
            has_more |= count < entries.len();
            let next_cursor = has_more.then(|| {
                ControlAuditCursorV1 {
                    before_sequence: entries
                        .get(count.saturating_sub(1))
                        .map_or(0, |entry| entry.sequence),
                    through_sequence,
                }
                .encode()
            });
            let empty_page = ControlAuditPageV1 {
                schema_version: CONTROL_AUDIT_PAGE_SCHEMA_VERSION_V1,
                entries: Vec::new(),
                next_cursor,
                has_more,
                snapshot_through_sequence: through_sequence,
                oldest_available_sequence: self
                    .audit_sequences
                    .iter()
                    .copied()
                    .find(|sequence| *sequence <= through_sequence),
                retired_records: self.retired.retired_audit_records,
            };
            let base_size = serde_json::to_vec(&empty_page)
                .map_err(|_| "control audit page serialization failed")?
                .len()
                .saturating_sub(2);
            let entries_size = entry_sizes[..count].iter().sum::<usize>() + count.saturating_sub(1);
            if base_size.saturating_add(entries_size) <= CONTROL_AUDIT_MAX_PAGE_BYTES {
                return Ok(ControlAuditPageV1 {
                    entries: entries[..count].to_vec(),
                    ..empty_page
                });
            }
            if count <= 1 {
                return Err("control audit record exceeds the maximum response byte budget");
            }
            count -= 1;
        }
    }

    fn next_audit_sequence(&self) -> Result<u64, ControlError> {
        self.audit_high_watermark
            .checked_add(1)
            .ok_or_else(|| ControlError::Invalid("control audit sequence exhausted".into()))
    }

    fn append_audit(&mut self, sequence: u64, record: AuditRecord) {
        debug_assert_eq!(sequence, self.audit_high_watermark + 1);
        self.audit_high_watermark = sequence;
        if record.status.is_final() {
            self.pending_commands_count = self.pending_commands_count.saturating_sub(1);
        } else {
            self.pending_commands_count = self.pending_commands_count.saturating_add(1);
        }
        self.audit_command_sequences
            .entry(record.command_id)
            .or_default()
            .push(sequence);
        self.audit_sequences.push(sequence);
        self.audit.push(record);
    }

    pub fn strategy_lifecycle(&self, worker_id: &str) -> Option<&StrategyLifecycleRecord> {
        self.strategy_lifecycle.get(worker_id)
    }

    /// Legacy snapshots with retired audit cannot prove the last lifecycle state.
    /// Start unrecorded workers paused until an explicit resume establishes it.
    pub fn strategy_pause_state(&self, worker_id: &str) -> Option<bool> {
        self.strategy_lifecycle(worker_id)
            .map(|state| state.paused)
            .or_else(|| self.strategy_lifecycle_legacy_incomplete.then_some(true))
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

    /// Number of accepted commands awaiting a terminal outcome.
    pub fn pending_commands_count(&self) -> u64 {
        self.pending_commands_count
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

    /// 上界之外的终态命令按终态审计记录的追加顺序最早退场，命令、请求索引、审计记录三格同时删。
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
        // 系统时钟可能回拨或节点间有偏移，`ts` 不是追加顺序。审计向量保留了
        // ControlPlane 接受记录的先后；按终态记录在此向量中的顺序退场，避免时钟漂移
        // 让较新的命令先于较旧命令淘汰。`execute` 保证每条命令至多一条终态记录。
        let finalized_in_order: Vec<u64> = self
            .audit
            .iter()
            .filter(|record| record.status.is_final())
            .map(|record| record.command_id)
            .collect();
        let overflow = finalized_in_order.len() - retention;
        let victims: BTreeSet<u64> = finalized_in_order.into_iter().take(overflow).collect();
        let mut removed_records = 0u64;
        let mut retained_audit = Vec::with_capacity(self.audit.len());
        let mut retained_sequences = Vec::with_capacity(self.audit_sequences.len());
        for (record, sequence) in self.audit.drain(..).zip(self.audit_sequences.drain(..)) {
            if victims.contains(&record.command_id) {
                removed_records += 1;
            } else {
                retained_audit.push(record);
                retained_sequences.push(sequence);
            }
        }
        self.audit = retained_audit;
        self.audit_sequences = retained_sequences;
        for command_id in &victims {
            if let Some(command) = self.commands.remove(command_id) {
                self.requests.remove(&command.request_id);
            }
            self.audit_command_sequences.remove(command_id);
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
        // Reserve the sequence before invoking a potentially side-effecting executor. Sequence
        // exhaustion must fail before any external action can succeed without an audit record.
        let sequence = self.next_audit_sequence()?;
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
        if status == CommandStatus::Executed && !command.dry_run && command.target != "*" {
            let paused = match command.kind {
                CommandKind::PauseStrategy => Some(true),
                CommandKind::ResumeStrategy => Some(false),
                _ => None,
            };
            if let Some(paused) = paused {
                self.strategy_lifecycle.insert(
                    command.target.clone(),
                    StrategyLifecycleRecord {
                        paused,
                        command_id,
                        ts,
                    },
                );
            }
        }
        self.append_audit(sequence, record.clone());
        self.retire_overflow();
        Ok(record)
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|error| error.to_string())?;
        let legacy_lifecycle = value.get("strategy_lifecycle").is_none();
        let has_audit_sequences = value.get("audit_sequences").is_some();
        let has_audit_watermark = value.get("audit_high_watermark").is_some();
        let has_audit_command_sequences = value.get("audit_command_sequences").is_some();
        let has_pending_commands_count = value.get("pending_commands_count").is_some();
        if has_audit_sequences != has_audit_watermark {
            return Err("控制面审计序号与水位必须同时存在".into());
        }
        let legacy_audit_sequences = !has_audit_sequences;
        let mut plane: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if legacy_audit_sequences {
            plane.audit_sequences = (1..=plane.audit.len() as u64).collect();
            plane.audit_high_watermark = plane.audit_sequences.last().copied().unwrap_or(0);
        }
        if plane.audit_sequences.len() != plane.audit.len()
            || plane.audit_sequences.contains(&0)
            || plane
                .audit_sequences
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || plane
                .audit_sequences
                .last()
                .is_some_and(|last| *last > plane.audit_high_watermark)
        {
            return Err("控制面审计追加序号与记录不一致".into());
        }
        let mut expected_command_sequences = BTreeMap::<u64, Vec<u64>>::new();
        for (record, sequence) in plane.audit.iter().zip(&plane.audit_sequences) {
            expected_command_sequences
                .entry(record.command_id)
                .or_default()
                .push(*sequence);
        }
        if legacy_audit_sequences || !has_audit_command_sequences {
            plane.audit_command_sequences = expected_command_sequences;
        } else if plane.audit_command_sequences != expected_command_sequences {
            return Err("控制面按命令审计序号索引与记录不一致".into());
        }
        if legacy_lifecycle && plane.retired.retired_commands > 0 {
            plane.strategy_lifecycle_legacy_incomplete = true;
        }
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
        let expected_pending_count = audit_state
            .values()
            .filter(|status| !status.is_final())
            .count() as u64;
        if has_pending_commands_count && plane.pending_commands_count != expected_pending_count {
            return Err("控制面待执行命令计数与终态审计不一致".into());
        }
        plane.pending_commands_count = expected_pending_count;
        // Migrate legacy documents from retained successful commands in execution order.
        let mut latest = BTreeMap::new();
        for record in &plane.audit {
            let command = &plane.commands[&record.command_id];
            if record.status != CommandStatus::Executed || command.dry_run || command.target == "*"
            {
                continue;
            }
            let paused = match command.kind {
                CommandKind::PauseStrategy => true,
                CommandKind::ResumeStrategy => false,
                _ => continue,
            };
            latest.insert(
                command.target.clone(),
                StrategyLifecycleRecord {
                    paused,
                    command_id: record.command_id,
                    ts: record.ts,
                },
            );
        }
        for (worker_id, record) in latest {
            if let Some(stored) = plane.strategy_lifecycle.get(&worker_id) {
                if plane.commands.contains_key(&stored.command_id) && stored != &record {
                    return Err(format!(
                        "strategy lifecycle state disagrees with audit for {worker_id}"
                    ));
                }
            } else if legacy_lifecycle {
                plane.strategy_lifecycle.insert(worker_id, record);
            } else {
                return Err(format!(
                    "strategy lifecycle state is missing for {worker_id}"
                ));
            }
        }
        if plane
            .strategy_lifecycle
            .keys()
            .any(|id| id.trim().is_empty() || id == "*")
        {
            return Err("strategy lifecycle state requires explicit worker IDs".into());
        }
        for (worker_id, state) in &plane.strategy_lifecycle {
            if let Some(command) = plane.commands.get(&state.command_id) {
                let kind_matches = if state.paused {
                    command.kind == CommandKind::PauseStrategy
                } else {
                    command.kind == CommandKind::ResumeStrategy
                };
                if !kind_matches
                    || command.dry_run
                    || command.target != *worker_id
                    || !plane.audit.iter().any(|record| {
                        record.command_id == state.command_id
                            && record.status == CommandStatus::Executed
                            && record.ts == state.ts
                    })
                {
                    return Err(format!(
                        "invalid strategy lifecycle binding for {worker_id}"
                    ));
                }
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

    fn protection_order(
        client_id: u64,
        kind: qx_core::ConditionalOrderKind,
        trigger: &str,
    ) -> Order {
        Order {
            client_id,
            instrument: qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: qx_core::Side::Sell,
            qty: qx_core::Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: qx_core::Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(qx_core::ExactOrderAmounts {
                quantity: qx_core::ExactDecimal::parse("0.01").unwrap(),
                limit_price: None,
                filled: qx_core::ExactDecimal::ZERO,
                quote_budget: None,
                quote_filled: qx_core::ExactDecimal::ZERO,
                market_reference_price: None,
                conditional: Some(qx_core::ConditionalOrderIntent {
                    kind,
                    trigger_price: Some(qx_core::ExactDecimal::parse(trigger).unwrap()),
                    trigger_price_type: Some(qx_core::TriggerPriceType::Last),
                    trailing_percent: None,
                    trailing_amount: None,
                    trailing_trigger_price: None,
                }),
            }),
        }
    }

    #[test]
    fn submit_order_group_command_binds_audited_target_and_group_payload() {
        let group = qx_core::OrderGroupIntent::new(
            "main-btc-protection",
            qx_core::OrderGroupKind::Oco,
            qx_core::OrderGroupExecution::NativeRequired,
            vec![
                qx_core::OrderGroupLegIntent {
                    role: qx_core::OrderGroupLegRole::TakeProfit,
                    order: protection_order(71, qx_core::ConditionalOrderKind::TakeProfit, "70000"),
                },
                qx_core::OrderGroupLegIntent {
                    role: qx_core::OrderGroupLegRole::StopLoss,
                    order: protection_order(72, qx_core::ConditionalOrderKind::StopLoss, "60000"),
                },
            ],
        )
        .unwrap();
        let mut command = command(Permission::Trading);
        command.kind = CommandKind::SubmitOrderGroup;
        command.target = group.group_id.clone();
        command.payload = BTreeMap::from([(
            "order_group_json".into(),
            serde_json::to_string(&group).unwrap(),
        )]);

        assert_eq!(order_group_from_submit_command(&command).unwrap(), group);
        let mut mismatched = command;
        mismatched.target = "another-group".into();
        assert!(order_group_from_submit_command(&mismatched).is_err());
    }

    #[test]
    fn cancel_order_group_command_binds_group_and_route_without_extra_payload() {
        let mut command = command(Permission::Trading);
        command.kind = CommandKind::CancelOrderGroup;
        command.target = "main-btc-protection".into();
        command.payload = BTreeMap::from([
            ("account_id".into(), "main".into()),
            ("venue_id".into(), "BINANCE".into()),
        ]);
        let target = cancel_order_group_target_from_command(&command).unwrap();
        assert_eq!(target.group_id, "main-btc-protection");
        assert_eq!(target.account_id, "main");
        assert_eq!(target.venue_id, "BINANCE");

        let mut extra = command.clone();
        extra.payload.insert("order_id".into(), "71".into());
        assert!(cancel_order_group_target_from_command(&extra).is_err());
        let mut empty_group = command;
        empty_group.target = "  ".into();
        assert!(cancel_order_group_target_from_command(&empty_group).is_err());
    }

    fn amend_command(order: &Order) -> ControlCommand {
        let mut value = command(Permission::Trading);
        value.kind = CommandKind::AmendOrder;
        value.target = order.client_id.to_string();
        value.payload = BTreeMap::from([
            ("account_id".into(), order.account_id.clone()),
            (
                "venue_id".into(),
                order.instrument.venue.as_str().to_string(),
            ),
            ("order_json".into(), serde_json::to_string(order).unwrap()),
        ]);
        value
    }

    #[test]
    fn amend_order_command_binds_target_route_and_active_order_snapshot() {
        let order = Order {
            client_id: 7,
            instrument: qx_core::InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_i64(3),
            limit: Some(qx_core::Price::from_i64(101)),
            status: OrderStatus::PartiallyFilled,
            filled: qx_core::Quantity::from_i64(1),
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let command = amend_command(&order);
        let target = amend_order_target_from_command(&command).unwrap();
        assert_eq!(target.client_order_id, 7);
        assert_eq!(target.account_id, "main");
        assert_eq!(target.venue_id, "BINANCE");
        assert_eq!(target.order, order);

        let mut mismatch = command.clone();
        mismatch.target = "8".into();
        assert!(amend_order_target_from_command(&mismatch).is_err());
        let mut wrong_route = command.clone();
        wrong_route.payload.insert("venue_id".into(), "OKX".into());
        assert!(amend_order_target_from_command(&wrong_route).is_err());
        let mut terminal = order;
        terminal.status = OrderStatus::Filled;
        assert!(amend_order_target_from_command(&amend_command(&terminal)).is_err());
    }

    fn command(permission: Permission) -> ControlCommand {
        ControlCommand {
            command_id: 1,
            request_id: "req-1".into(),
            operator_id: "operator".into(),
            reason: "incident recovery".into(),
            // PauseStrategy 有策略 worker 派发者；CancelOrder 也已由交易所执行 worker 派发。
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
    fn control_command_wire_rejects_unknown_fields() {
        let mut value = serde_json::to_value(command(Permission::Trading)).unwrap();
        value["dry_rnu"] = serde_json::Value::Bool(false);
        assert!(serde_json::from_value::<ControlCommand>(value).is_err());
    }

    #[test]
    fn versioned_audit_record_matches_the_shared_wire_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../python/tests/fixtures/control-audit-record-v1.json"
        ))
        .unwrap();
        let domain_record: AuditRecord = serde_json::from_value(fixture.clone()).unwrap();
        let record = ControlAuditRecordV1::from(&domain_record);
        assert_eq!(serde_json::to_value(&record).unwrap(), fixture);
        let decoded: ControlAuditRecordV1 =
            serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
        assert_eq!(decoded, record);

        let mut unknown = fixture.clone();
        unknown["unregistered"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ControlAuditRecordV1>(unknown).is_err());
        let mut old_version = fixture;
        old_version["schema_version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<ControlAuditRecordV1>(old_version).is_err());
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
        plane.audit_sequences.push(1);
        plane
            .audit_command_sequences
            .insert(command.command_id, vec![1]);
        plane.audit_high_watermark = 1;
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

    #[test]
    fn audit_pages_use_durable_sequences_and_freeze_the_snapshot() {
        let mut plane = ControlPlane::default();
        for index in 1..=3u64 {
            plane.submit(numbered(index), index * 10).unwrap();
        }

        let first = plane.audit_page(None, 2, None).unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            [3, 2]
        );
        assert!(first.has_more);
        assert_eq!(first.snapshot_through_sequence, 3);
        let cursor = first.next_cursor.clone().unwrap();
        assert_eq!(cursor, "v1.2.3");

        // New rows after page one do not extend this scan's upper bound.
        plane.submit(numbered(4), 40).unwrap();
        let restored = ControlPlane::from_json(&plane.to_json().unwrap()).unwrap();
        let second = restored.audit_page(Some(&cursor), 2, None).unwrap();
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            [1]
        );
        assert!(!second.has_more);
        assert_eq!(second.snapshot_through_sequence, 3);

        let request_page = restored.audit_page(None, 2, Some("req-2")).unwrap();
        assert_eq!(request_page.entries.len(), 1);
        assert!(request_page.next_cursor.is_none());
    }

    #[test]
    fn audit_pages_enforce_a_serialized_byte_budget() {
        let mut plane = ControlPlane::default();
        let mut large = numbered(1);
        large.request_id = "r".repeat(CONTROL_AUDIT_MAX_PAGE_BYTES / 2 + 32);
        plane.submit(large, 10).unwrap();
        plane.execute(1, 11, |_| Ok("APPLIED".into())).unwrap();

        let first = plane.audit_page(None, 2, None).unwrap();
        assert_eq!(first.entries.len(), 1);
        assert!(first.has_more);
        assert!(serde_json::to_vec(&first).unwrap().len() <= CONTROL_AUDIT_MAX_PAGE_BYTES);
        let second = plane
            .audit_page(first.next_cursor.as_deref(), 2, None)
            .unwrap();
        assert_eq!(second.entries.len(), 1);
        assert!(!second.has_more);
        assert!(serde_json::to_vec(&second).unwrap().len() <= CONTROL_AUDIT_MAX_PAGE_BYTES);

        let mut too_large = numbered(2);
        too_large.request_id = "r".repeat(CONTROL_AUDIT_MAX_PAGE_BYTES + 1);
        plane.submit(too_large, 20).unwrap();
        assert!(plane
            .audit_page(None, 2, None)
            .unwrap_err()
            .contains("maximum response byte budget"));
    }

    #[test]
    fn audit_cursor_expires_when_retirement_removes_its_unread_window() {
        let mut plane = ControlPlane::default();
        plane.submit(numbered(1), 10).unwrap();
        plane.submit(numbered(2), 20).unwrap();
        let first = plane.audit_page(None, 1, None).unwrap();
        let cursor = first.next_cursor.unwrap();

        plane.execute(1, 30, |_| Ok("APPLIED".into())).unwrap();
        plane.execute(2, 40, |_| Ok("APPLIED".into())).unwrap();
        plane.retire_overflow_with(0);

        assert!(plane.audit_page(Some(&cursor), 1, None).is_err());
    }

    #[test]
    fn audit_sequence_migration_keeps_legacy_control_plane_readable() {
        let mut plane = ControlPlane::default();
        plane.submit(numbered(1), 10).unwrap();
        plane.execute(1, 11, |_| Ok("APPLIED".into())).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&plane.to_json().unwrap()).unwrap();
        let object = value
            .as_object_mut()
            .expect("plane serializes as an object");
        object.remove("audit_sequences");
        object.remove("audit_high_watermark");
        object.remove("pending_commands_count");

        let restored = ControlPlane::from_json(&value.to_string()).unwrap();
        let page = restored.audit_page(None, 10, None).unwrap();
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(page.snapshot_through_sequence, 2);
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

    #[test]
    fn retirement_uses_audit_append_order_when_timestamps_move_backwards() {
        let mut plane = ControlPlane::default();
        for index in 1..=4u64 {
            plane.submit(numbered(index), 100 + index).unwrap();
        }
        for (command_id, ts) in [(1, 400), (2, 300), (3, 200), (4, 100)] {
            plane
                .execute(command_id, ts, |_| Ok("APPLIED".into()))
                .unwrap();
        }

        plane.retire_overflow_with(2);

        assert!(
            plane.command(1).is_none(),
            "oldest finalized command must retire first"
        );
        assert!(
            plane.command(2).is_none(),
            "second-oldest finalized command must retire"
        );
        assert!(plane.command(3).is_some());
        assert!(plane.command(4).is_some());
        assert_eq!(plane.retirement().retired_commands, 2);
        assert!(ControlPlane::from_json(&plane.to_json().unwrap()).is_ok());
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

    #[test]
    fn pending_command_admission_is_bounded_and_recovers_capacity() {
        let mut plane = ControlPlane::default();
        for id in 1..=PENDING_CONTROL_COMMAND_LIMIT as u64 {
            plane.submit(numbered(id), id).unwrap();
        }
        assert_eq!(
            plane.pending_commands_count(),
            PENDING_CONTROL_COMMAND_LIMIT as u64
        );
        assert_eq!(
            plane.submit(numbered(PENDING_CONTROL_COMMAND_LIMIT as u64 + 1), 5000),
            Err(ControlError::PendingLimitReached)
        );
        assert_eq!(plane.audit().len(), PENDING_CONTROL_COMMAND_LIMIT);

        plane.execute(1, 5001, |_| Ok("APPLIED".into())).unwrap();
        assert_eq!(
            plane.pending_commands_count(),
            PENDING_CONTROL_COMMAND_LIMIT as u64 - 1
        );
        plane
            .submit(numbered(PENDING_CONTROL_COMMAND_LIMIT as u64 + 1), 5002)
            .unwrap();
        assert_eq!(
            plane.pending_commands_count(),
            PENDING_CONTROL_COMMAND_LIMIT as u64
        );

        let restored = ControlPlane::from_json(&plane.to_json().unwrap()).unwrap();
        assert_eq!(
            restored.pending_commands_count(),
            PENDING_CONTROL_COMMAND_LIMIT as u64
        );
        let mut legacy: serde_json::Value =
            serde_json::from_str(&plane.to_json().unwrap()).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("pending_commands_count");
        assert_eq!(
            ControlPlane::from_json(&legacy.to_string())
                .unwrap()
                .pending_commands_count(),
            PENDING_CONTROL_COMMAND_LIMIT as u64
        );
        legacy["pending_commands_count"] = 0.into();
        match ControlPlane::from_json(&legacy.to_string()) {
            Ok(_) => panic!("持久计数与未终结命令不一致时必须拒绝恢复"),
            Err(error) => assert!(error.contains("待执行命令计数")),
        }
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

#[cfg(test)]
mod durable_lifecycle_tests {
    use super::*;

    fn command(id: u64, kind: CommandKind, dry_run: bool) -> ControlCommand {
        ControlCommand {
            command_id: id,
            request_id: format!("lifecycle-{id}"),
            operator_id: "ops".into(),
            reason: "lifecycle recovery".into(),
            kind,
            target: "worker-a".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run,
        }
    }

    fn execute(plane: &mut ControlPlane, id: u64, kind: CommandKind, dry_run: bool, success: bool) {
        plane.submit(command(id, kind, dry_run), id).unwrap();
        plane
            .execute(id, id, |_| {
                if success {
                    Ok("ok".into())
                } else {
                    Err("failed".into())
                }
            })
            .unwrap();
    }

    #[test]
    fn durable_lifecycle_survives_audit_retirement_and_restart() {
        let mut plane = ControlPlane::default();
        execute(&mut plane, 1, CommandKind::PauseStrategy, false, true);
        plane.retire_overflow_with(0);
        assert!(plane.audit().is_empty());
        let restored = ControlPlane::from_json(&plane.to_json().unwrap()).unwrap();
        assert!(restored.strategy_lifecycle("worker-a").unwrap().paused);
        assert!(restored.strategy_lifecycle("worker-b").is_none());
    }

    #[test]
    fn durable_lifecycle_legacy_retirement_requires_explicit_resume() {
        let mut plane = ControlPlane::default();
        execute(&mut plane, 1, CommandKind::PauseStrategy, false, true);
        plane.retire_overflow_with(0);
        let mut value = serde_json::to_value(&plane).unwrap();
        value.as_object_mut().unwrap().remove("strategy_lifecycle");
        value
            .as_object_mut()
            .unwrap()
            .remove("strategy_lifecycle_legacy_incomplete");
        let mut migrated = ControlPlane::from_json(&value.to_string()).unwrap();
        assert_eq!(migrated.strategy_pause_state("worker-a"), Some(true));
        execute(&mut migrated, 2, CommandKind::ResumeStrategy, false, true);
        let restored = ControlPlane::from_json(&migrated.to_json().unwrap()).unwrap();
        assert_eq!(restored.strategy_pause_state("worker-a"), Some(false));
        assert_eq!(restored.strategy_pause_state("worker-b"), Some(true));
    }

    #[test]
    fn durable_lifecycle_ignores_failed_and_dry_run_commands() {
        let mut plane = ControlPlane::default();
        execute(&mut plane, 1, CommandKind::PauseStrategy, false, true);
        execute(&mut plane, 2, CommandKind::ResumeStrategy, true, true);
        execute(&mut plane, 3, CommandKind::ResumeStrategy, false, false);
        assert_eq!(plane.strategy_lifecycle("worker-a").unwrap().command_id, 1);
        execute(&mut plane, 4, CommandKind::ResumeStrategy, false, true);
        let restored = ControlPlane::from_json(&plane.to_json().unwrap()).unwrap();
        assert!(!restored.strategy_lifecycle("worker-a").unwrap().paused);
    }

    #[test]
    fn durable_lifecycle_migrates_legacy_and_rejects_conflicting_projection() {
        let mut plane = ControlPlane::default();
        execute(&mut plane, 1, CommandKind::PauseStrategy, false, true);
        let mut value = serde_json::to_value(&plane).unwrap();
        value.as_object_mut().unwrap().remove("strategy_lifecycle");
        let migrated = ControlPlane::from_json(&value.to_string()).unwrap();
        assert!(migrated.strategy_lifecycle("worker-a").unwrap().paused);
        let mut value = serde_json::to_value(&migrated).unwrap();
        value["strategy_lifecycle"]["worker-a"]["paused"] = false.into();
        assert!(ControlPlane::from_json(&value.to_string()).is_err());
    }

    #[test]
    fn durable_lifecycle_rejects_new_wildcard_without_breaking_legacy_reads() {
        let mut plane = ControlPlane::default();
        let mut wildcard = command(1, CommandKind::PauseStrategy, false);
        wildcard.target = "*".into();
        assert!(wildcard.validate().is_ok());
        assert!(wildcard.validate_as(Permission::Trading).is_err());
        assert!(plane.submit(wildcard.clone(), 1).is_err());
        // A previously accepted wildcard remains readable but does not invent per-worker state.
        let mut legacy = ControlPlane::default();
        let accepted = AuditRecord {
            command_id: 1,
            request_id: wildcard.request_id.clone(),
            operator_id: wildcard.operator_id.clone(),
            command_digest: wildcard.digest(),
            status: CommandStatus::Accepted,
            result_code: "accepted".into(),
            ts: 1,
        };
        legacy.requests.insert(wildcard.request_id.clone(), 1);
        legacy.commands.insert(1, wildcard);
        legacy.audit.push(accepted);
        legacy.audit_sequences.push(1);
        legacy.audit_command_sequences.insert(1, vec![1]);
        legacy.pending_commands_count = 1;
        legacy.audit_high_watermark = 1;
        assert!(ControlPlane::from_json(&legacy.to_json().unwrap()).is_ok());
    }
}
