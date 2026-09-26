//! 控制面契约。
//!
//! 所有写操作都先变成带权限、原因和请求 ID 的 `ControlCommand`，由上层执行器
//! 再决定如何调用策略运行时、OMS 或 Scheduler。本 crate 不提供绕过风控的快捷写入。
//!
//! 受理即承诺执行：只有 [`CommandKind::has_executor`] 为真的种类会被写进 `Accepted`
//! 审计，其余种类在受理处当场拒（V11 P1）。
//!
//! 状态规模由"还有多少条命令没执行完"决定，不由这个进程跑过多少条决定：命令一进终态就离开
//! 主表，它的 `Accepted` 与终态记录成对留在有界审计窗口（[`AUDIT_WINDOW_RECORDS`]）里，窗口
//! 滚过之后只剩 [`ControlPlane::retirement`] 的累计计数（V11 R5-1）。

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

    /// 这类命令在仓内有没有执行者：即是否存在一个 worker 会领取它并回写终态。
    ///
    /// SubmitOrder 由 venue 执行 worker（Binance / CCXT / Paper）领取，Pause 与 Resume 由
    /// Strategy worker 领取。其余种类只有契约形状、没有领取方，受理它们等于留下一条永远停
    /// 在 `Accepted` 的审计记录（V11 P1）。变体本身保留，历史审计记录仍要能反序列化。
    pub fn has_executor(&self) -> bool {
        matches!(
            self,
            Self::SubmitOrder | Self::PauseStrategy | Self::ResumeStrategy
        )
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

/// 命令状态词表：只列得出生产者的那一半——`Rejected` 曾在这里，但受理期的拒绝在写下
/// 第一颗审计记录之前就 `return Err`，那颗状态永远没人写（V11 R6-1）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommandStatus {
    Accepted,
    Executed,
    Failed,
}

impl CommandStatus {
    /// 终态：命令已经离场，之后不得再出现任何审计记录。
    ///
    /// 退场判据、窗口裁剪、幂等回查与恢复期的配对检查共用这一颗，避免"什么算终态"在各处各长一遍。
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

/// 审计窗口容量：`audit` 最多留下多少条记录（记录数，不是命令数）。
///
/// 裁剪只动"已退场命令"的成对记录；在途命令的 `Accepted` 永不裁掉，所以 `audit.len()` 的上界是
/// "窗口容量 + 当前在途命令数"。后者停在 `Accepted` 本身就是告警形状，把它一起裁掉等于把缺陷
/// 藏进保留策略里（V11 R5-1）。
pub const AUDIT_WINDOW_RECORDS: usize = 1_000;

/// 终态命令退场之后累计留下的摘要。审计窗口会滚掉细节，这几个数不会。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetirementSummary {
    /// 已经离开主表（`commands` / `requests`）的命令总数。
    pub retired_total: u64,
    pub executed: u64,
    pub failed: u64,
    pub last_retired_ts: u64,
}

/// 控制面状态。
///
/// `commands` 与 `requests` 只含**在途**命令：命令一进终态就退场，主表规模由"还有多少条没执行"
/// 决定，而不是由这个进程跑过多少条决定。退场命令的 `Accepted` 与终态记录成对留在容量为
/// [`AUDIT_WINDOW_RECORDS`] 的审计窗口里，窗口之外只剩 [`RetirementSummary`] 的计数（V11 R5-1）。
///
/// 窗口滚过一条命令之后，控制面不再认得它的身份，同一份载荷会被当作新命令受理；兜住重复下单的
/// 是执行面的持久事实（`qx-runtime` 的 EventLog 重建 OMS，重投落 `ALREADY_APPLIED_FROM_EVENT_LOG`），
/// 而那条事实要求订单身份按轮稳定——见 `schemas/strategy_api_v1.md` 的身份推导。
///
/// 窗口与摘要之外还有第三本：`audit_seq` / `audit_head_hash` 是哈希审计链的检查点。落盘时链与
/// 状态在同一次事务里一起走（文件后端链先落、数据库后端同一笔事务），所以状态永远知道链到哪一条
/// 为止（V11 R5-2）。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ControlPlane {
    commands: BTreeMap<u64, ControlCommand>,
    requests: BTreeMap<String, u64>,
    audit: Vec<AuditRecord>,
    // 旧状态文件没有这一格：它按"只追加"写就，退场计数从零开始，命令体在恢复时当场退场。
    #[serde(default)]
    retirement: RetirementSummary,
    // 本轮事务产出、还没接进哈希链的流水。它不进状态文件——链才是它的持久处，状态文件
    // 只留下面那两个检查点格（V11 R5-2）。
    #[serde(skip, default)]
    unchained_audit: Vec<AuditRecord>,
    // 已经接进哈希链的记录条数与链尾摘要。存储层在落状态之前按这两格核对链尾，
    // 对不上就是链被旁路写过或整段截过，当场失败而不是继续追加。
    #[serde(default)]
    audit_seq: u64,
    #[serde(default)]
    audit_head_hash: u64,
}

/// 一次事务边界交给存储层接链的三格：链的检查点，与本轮尚未接链的流水。
#[derive(Clone, Copy, Debug)]
pub struct AuditChainState<'a> {
    pub seq: u64,
    pub head_hash: u64,
    pub unchained: &'a [AuditRecord],
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
        // 受理即承诺"有人会执行这条命令"：没有执行者的种类在写入 Accepted 审计之前当场拒，
        // 而不是回 202 之后永远停在 pending。判据只长在这一处，`submit` 与 `submit_as`
        // 共用；恢复读侧仍走结构性 `validate`，已落盘的历史命令要能被读回来处置（V11 P1）。
        if !command.kind.has_executor() {
            return Err(ControlError::Invalid(format!(
                "{:?} 没有执行者，控制面不予受理：可受理的命令种类只有 SubmitOrder / PauseStrategy / ResumeStrategy",
                command.kind
            )));
        }
        // 幂等判据有两层：在途命令在主表里，已退场命令只残留着窗口里的终态记录。
        // 两层都答同一个错误，`persist_strategy_submit` 的分支形状不因退场而变。
        if self.requests.contains_key(&command.request_id)
            || self.final_audit_for_request(&command.request_id).is_some()
        {
            return Err(ControlError::DuplicateRequest(command.request_id));
        }
        if self.commands.contains_key(&command.command_id)
            || self.final_audit(command.command_id).is_some()
        {
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
        self.push_audit(record.clone());
        Ok(record)
    }

    /// 该命令在审计窗口里的最后一条记录：在途命令是它的 `Accepted`，已退场命令是它的终态。
    /// 窗口已经滚过这条命令时返回 `None`——命令体在退场时一并离开主表，这里不再有第二条读路。
    pub fn latest_audit(&self, command_id: u64) -> Option<&AuditRecord> {
        self.audit
            .iter()
            .rev()
            .find(|record| record.command_id == command_id)
    }

    /// 窗口内属于这条命令的终态记录。命令体退场之后，它是"这个身份已经被用过"的唯一残留证据，
    /// 也是幂等分支唯一还能核对 `command_digest` 的地方。
    fn final_audit(&self, command_id: u64) -> Option<&AuditRecord> {
        self.audit
            .iter()
            .rev()
            .find(|record| record.command_id == command_id && record.status.is_final())
    }

    fn final_audit_for_request(&self, request_id: &str) -> Option<&AuditRecord> {
        self.audit
            .iter()
            .rev()
            .find(|record| record.request_id == request_id && record.status.is_final())
    }

    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }

    /// 本轮事务产出、尚未接进哈希链的流水与链的检查点（V11 R5-2）。
    ///
    /// 只有存储层的事务边界会用到它：链必须与状态在同一次落盘里接上，所以"哪些记录
    /// 还没接链"是事务状态，不是查询状态。恢复期从状态文件重放出来的历史记录不走这里
    /// ——它们早在写出它们的那一笔事务里接过链了。
    pub fn audit_chain(&self) -> AuditChainState<'_> {
        AuditChainState {
            seq: self.audit_seq,
            head_hash: self.audit_head_hash,
            unchained: &self.unchained_audit,
        }
    }

    /// 存储层把 `chained` 条接上链之后回填检查点。少接一条都不该调用这个口：状态与链
    /// 各走各的，正是这条链要防的那件事。
    pub fn note_audit_chained(&mut self, chained: usize, head_hash: u64) {
        self.unchained_audit.drain(..chained);
        self.audit_seq = self.audit_seq.saturating_add(chained as u64);
        self.audit_head_hash = head_hash;
    }

    /// 已退场命令的累计摘要：窗口滚掉细节之后，操作者仍然要看得见总量与失败数。
    pub fn retirement(&self) -> RetirementSummary {
        self.retirement
    }

    /// 在途命令。终态命令不会出现在这里（见 [`ControlPlane`] 的退场口径），所以这一格不再需要
    /// 扫一遍审计流水去重——那条扫描正是 §52.10 里 3 万条命令 ≈7.09 s 的开销来源。
    pub fn pending(&self) -> impl Iterator<Item = &ControlCommand> {
        self.commands.values()
    }

    /// 审计流水进窗口的唯一出口：一份留在有界窗口里供当场判据读，一份等着在同一次
    /// 事务里被接进哈希链。恢复期重放历史记录不走这里——那些记录在写出它们的那一笔
    /// 事务里已经接过链了（V11 R5-2）。
    fn push_audit(&mut self, record: AuditRecord) {
        self.audit.push(record.clone());
        self.unchained_audit.push(record);
    }

    /// 命令进入终态：离开主表，写进累计摘要，并把审计窗口裁回容量内。
    fn retire(&mut self, command: ControlCommand, status: CommandStatus, ts: u64) {
        self.requests.remove(&command.request_id);
        self.commands.remove(&command.command_id);
        self.note_retirement(status, ts);
        self.trim_audit_window();
    }

    /// 计数只在"确实离开主表"时前进：窗口里那一对记录会随裁剪滚掉，计数不会。
    fn note_retirement(&mut self, status: CommandStatus, ts: u64) {
        self.retirement.retired_total = self.retirement.retired_total.saturating_add(1);
        match status {
            CommandStatus::Executed => self.retirement.executed += 1,
            CommandStatus::Failed => self.retirement.failed += 1,
            CommandStatus::Accepted => {}
        }
        self.retirement.last_retired_ts = self.retirement.last_retired_ts.max(ts);
    }

    /// 成对裁掉最旧的已退场命令：`Accepted` 与终态记录同进同出，留下半对就等于
    /// [`ControlPlane::from_json`] 会拒收的"念一份读不回来的历史"。
    fn trim_audit_window(&mut self) {
        if self.audit.len() <= AUDIT_WINDOW_RECORDS {
            return;
        }
        // 一次裁到配额的一半，让 O(n) 的整表 retain 摊在多条命令上，而不是每条都触发一次。
        let budget = AUDIT_WINDOW_RECORDS / 2;
        let mut keep: BTreeSet<u64> = self.commands.keys().copied().collect();
        let mut retained = 0_usize;
        for record in self.audit.iter().rev() {
            if retained >= budget {
                break;
            }
            if record.status.is_final() {
                keep.insert(record.command_id);
                retained += 1;
            }
        }
        self.audit
            .retain(|record| keep.contains(&record.command_id));
    }

    /// 执行器唯一的状态入口：执行结果必须回写审计记录，不能只返回字符串。
    ///
    /// 命令一进终态就退场，所以重复执行读到的不再是主表里的命令体，而是窗口里那条终态记录
    /// （[`ControlError::AlreadyFinal`]）；窗口也滚过去时才落回
    /// [`ControlError::UnknownCommand`]——那时"这条命令跑过"已经不由控制面作证了。
    pub fn execute<F>(
        &mut self,
        command_id: u64,
        ts: u64,
        action: F,
    ) -> Result<AuditRecord, ControlError>
    where
        F: FnOnce(&ControlCommand) -> Result<String, String>,
    {
        let command = match self.commands.get(&command_id) {
            Some(command) => command.clone(),
            None => {
                return Err(if self.final_audit(command_id).is_some() {
                    ControlError::AlreadyFinal(command_id)
                } else {
                    ControlError::UnknownCommand(command_id)
                })
            }
        };
        let (status, result_code) = match action(&command) {
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
        self.push_audit(record.clone());
        self.retire(command, status, ts);
        Ok(record)
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let mut plane: Self = serde_json::from_str(input).map_err(|error| error.to_string())?;
        // 审计流水按命令身份重放配对：非终态（词表里只有 `Accepted`）恰一条，终态至多一条且必须
        // 落在同一条摘要上。"哪颗算终态"仍然只问 `is_final`，不在这里第二处自己认状态。
        let mut accepted: BTreeMap<u64, &AuditRecord> = BTreeMap::new();
        let mut finalised: BTreeMap<u64, (CommandStatus, u64)> = BTreeMap::new();
        for record in &plane.audit {
            let command_id = record.command_id;
            if !record.status.is_final() {
                if accepted.insert(command_id, record).is_some() {
                    return Err(format!("命令 {} 重复 Accepted", command_id));
                }
                continue;
            }
            let Some(acceptance) = accepted.get(&command_id) else {
                return Err(format!("命令 {} 缺少 Accepted 初始记录", command_id));
            };
            if finalised
                .insert(command_id, (record.status, record.ts))
                .is_some()
            {
                return Err(format!("命令 {} 终态后仍有审计记录", command_id));
            }
            if record.request_id != acceptance.request_id
                || record.operator_id != acceptance.operator_id
                || record.command_digest != acceptance.command_digest
            {
                return Err(format!("命令 {} 的审计摘要不一致", command_id));
            }
        }
        // 窗口只能是哈希链的一段后缀：`audit_seq` 数的是已经接进链的记录，窗口里却摆着
        // 比它更多的流水，说明有记录写出时没进链（绕过事务边界的整份保存留下的）。那种
        // 流水当场拒收——事后既补不回链，也不能让读侧念一段没有摘要背书的审计历史。
        if plane.audit_seq < plane.audit.len() as u64 {
            return Err(format!(
                "审计检查点 {} 少于窗口里的 {} 条流水：这些记录没接进哈希链。最可能是本文件写于\
                 链接入（V11 R5-2）之前——链不能追溯补写，请归档这份状态后重新起栈",
                plane.audit_seq,
                plane.audit.len()
            ));
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
            let record = accepted
                .get(command_id)
                .ok_or_else(|| format!("命令 {} 缺少审计记录", command_id))?;
            if record.request_id != command.request_id
                || record.operator_id != command.operator_id
                || record.command_digest != command.digest()
            {
                return Err(format!("命令 {} 的审计摘要不一致", command_id));
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
        // 主表里没有命令体的身份只剩窗口里那一对：只留下孤零零一条 `Accepted`，就是
        // "念一份读不回来的历史"——那条命令到底跑没跑过，这份状态回答不了。
        for command_id in accepted
            .keys()
            .filter(|command_id| !plane.commands.contains_key(command_id))
        {
            if !finalised.contains_key(command_id) {
                return Err(format!(
                    "命令 {} 的 Accepted 记录既没有在途命令体，也没有终态配对",
                    command_id
                ));
            }
        }
        // 「只追加」时代写下的状态文件里，终态命令体还留在主表：加载即退场，一次性收敛到
        // 当前口径。计数只数真正离开主表的命令，已在窗口里的退场不重复计。
        let mut normalised = false;
        for (command_id, status, ts) in finalised
            .into_iter()
            .map(|(command_id, (status, ts))| (command_id, status, ts))
        {
            if let Some(command) = plane.commands.remove(&command_id) {
                plane.requests.remove(&command.request_id);
                plane.note_retirement(status, ts);
                normalised = true;
            }
        }
        if normalised {
            plane.trim_audit_window();
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
mod tests;
