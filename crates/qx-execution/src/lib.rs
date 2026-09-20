//! 统一执行层边界。
//!
//! Venue 只返回 `VenueEvent`，执行层负责把这些事件转换成运行时标准事实，
//! 再通过本 crate `application` 子模块的执行端口交给 EventLog、订单状态和 Ledger。Paper、
//! Binance 以及未来连接器都必须复用这里的事实转换，不能在 CLI 中各写一套。
//! 本 crate 不依赖任何具体运行时或存储后端。

pub mod application;
pub use application::*;
use qx_control::{order_from_submit_command, ControlCommand};
use qx_core::{Order, OrderStatus, OrderTrace, Price, Quantity, TradingInstrumentSpec, SCALE};
use qx_guanxing::QuoteTick;
use qx_risk::OrderRiskPosition;
use qx_zhenlu::{
    PaperVenue, RiskContext, SpreadOrderGroup, SpreadOrderGroupStatus, SpreadOrderGroupStore,
    Venue, VenueEvent,
};

pub struct RiskExecutionContext<'a> {
    pub risk: &'a RiskContext,
    pub position: &'a OrderRiskPosition,
}

/// 把当前账户快照适配为应用层 RiskPort，便于回测、Paper 和实盘执行器
/// 替换风控实现而不改动 `ExecutionGateway` 的订单副作用边界。
pub struct RiskContextPort<'a> {
    pub risk: &'a RiskContext,
    pub position: &'a OrderRiskPosition,
}

/// 直接适配 `qx-risk::OrderRiskContext` 的应用层端口。
///
/// `RiskContextPort` 保留 qx-zhenlu 的兼容快照入口；新执行器可以直接注入
/// canonical context，避免回测、Paper 和实盘再各自复制一份规格/保证金/
/// 投影持仓转换逻辑。
pub struct CanonicalRiskPort<'a> {
    pub context: &'a qx_risk::OrderRiskContext,
}

/// 将现有 Venue 事实适配为应用层 VenuePort。它只做事实类型转换，不写
/// EventLog/OMS；真正的副作用仍由 `ExecutionGateway`（`PortExecutionService`）统一编排。
pub struct VenuePortAdapter<V: Venue> {
    venue: V,
    accept_any_venue: bool,
}

/// 把仍使用旧版 `Venue` trait 的连接器借用到统一 `VenuePort`，使只持有可变
/// 借用的执行入口（Paper worker、多腿编排）也走同一个 `ExecutionGateway`。
pub struct BorrowedVenuePort<'a, V: Venue + ?Sized> {
    venue: &'a mut V,
    accept_any_venue: bool,
}

impl<'a, V: Venue + ?Sized> BorrowedVenuePort<'a, V> {
    pub fn new(venue: &'a mut V) -> Self {
        Self {
            venue,
            accept_any_venue: false,
        }
    }

    pub fn new_for_any_venue(venue: &'a mut V) -> Self {
        Self {
            venue,
            accept_any_venue: true,
        }
    }
}

impl<V: Venue> VenuePortAdapter<V> {
    pub fn new(venue: V) -> Self {
        Self {
            venue,
            accept_any_venue: false,
        }
    }

    /// Paper/模拟路由可以把多个底层 Instrument Venue 映射到同一个虚拟
    /// Venue；实盘适配器必须继续使用 `new`，保持严格路由校验。
    pub fn new_for_any_venue(venue: V) -> Self {
        Self {
            venue,
            accept_any_venue: true,
        }
    }

    pub fn into_inner(self) -> V {
        self.venue
    }

    pub fn venue(&self) -> &V {
        &self.venue
    }

    pub fn venue_mut(&mut self) -> &mut V {
        &mut self.venue
    }
}

fn venue_events_to_application(events: Vec<VenueEvent>) -> Vec<ExecutionEvent> {
    events
        .into_iter()
        .map(|event| match event {
            VenueEvent::Accepted {
                client_order_id,
                venue_order_id,
                ..
            } => ExecutionEvent::Accepted {
                client_order_id,
                venue_order_id,
            },
            VenueEvent::Fill(fill) => ExecutionEvent::Fill(Box::new(fill)),
            VenueEvent::Cancelled {
                client_order_id, ..
            } => ExecutionEvent::Cancelled { client_order_id },
        })
        .collect()
}

impl<V: Venue> VenuePort for VenuePortAdapter<V> {
    fn venue_id(&self) -> &str {
        self.venue.id()
    }

    fn submit_order(
        &mut self,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.venue
            .submit(order, ts)
            .map(venue_events_to_application)
            .map_err(|error| format!("Venue submit 失败: {error:?}"))
    }

    fn cancel_order(
        &mut self,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.venue
            .cancel(client_order_id, ts)
            .map(venue_events_to_application)
            .map_err(|error| format!("Venue cancel 失败: {error:?}"))
    }
}

impl<V: Venue + ?Sized> VenuePort for BorrowedVenuePort<'_, V> {
    fn venue_id(&self) -> &str {
        self.venue.id()
    }

    fn submit_order(
        &mut self,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.venue
            .submit(order, ts)
            .map(venue_events_to_application)
            .map_err(|error| format!("Venue submit 失败: {error:?}"))
    }

    fn cancel_order(
        &mut self,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.venue
            .cancel(client_order_id, ts)
            .map(venue_events_to_application)
            .map_err(|error| format!("Venue cancel 失败: {error:?}"))
    }
}

impl<V: Venue + ?Sized> VenueRouterPort for BorrowedVenuePort<'_, V> {
    fn submit_order(
        &mut self,
        venue_id: &str,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        if self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id())
        {
            VenuePort::submit_order(self, order, ts)
        } else {
            Err(format!(
                "Venue 路由不匹配: requested={venue_id} configured={}",
                self.venue_id()
            ))
        }
    }

    fn cancel_order(
        &mut self,
        venue_id: &str,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        if self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id())
        {
            VenuePort::cancel_order(self, client_order_id, ts)
        } else {
            Err(format!(
                "Venue 路由不匹配: requested={venue_id} configured={}",
                self.venue_id()
            ))
        }
    }
}

impl<V: Venue> VenueRouterPort for VenuePortAdapter<V> {
    fn submit_order(
        &mut self,
        venue_id: &str,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        if self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id())
        {
            VenuePort::submit_order(self, order, ts)
        } else {
            Err(format!(
                "Venue 路由不匹配: requested={venue_id} configured={}",
                self.venue_id()
            ))
        }
    }

    fn cancel_order(
        &mut self,
        venue_id: &str,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        if self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id())
        {
            VenuePort::cancel_order(self, client_order_id, ts)
        } else {
            Err(format!(
                "Venue 路由不匹配: requested={venue_id} configured={}",
                self.venue_id()
            ))
        }
    }
}

impl RiskPort for RiskContextPort<'_> {
    fn evaluate_order(&self, order: &qx_core::Order) -> Result<RiskDecision, String> {
        let decision = self.risk.evaluate_order(order, self.position);
        if decision.allowed {
            Ok(RiskDecision {
                accepted: true,
                reason_code: "accepted",
            })
        } else {
            Err(format!(
                "RiskContext {}: {}",
                decision.rule_set_version,
                decision.violations.join("; ")
            ))
        }
    }
}

impl RiskPort for CanonicalRiskPort<'_> {
    fn evaluate_order(&self, order: &qx_core::Order) -> Result<RiskDecision, String> {
        let decision = qx_risk::RiskEngine::evaluate_order(self.context, order);
        if decision.allowed {
            Ok(RiskDecision {
                accepted: true,
                reason_code: "accepted",
            })
        } else {
            Err(format!(
                "CanonicalRiskEngine {}: {}",
                decision.rule_set_version,
                decision.violations.join("; ")
            ))
        }
    }
}

/// 完全基于 Application Ports 的单腿执行器。
///
/// 这是回测、Paper、CCXT 及未来券商适配器共用的最小副作用边界：订单先
/// 注册到 `OrderStore`，再调用 `VenuePort`，最后把标准执行事实写入
/// `ExecutionEventPort`。它不依赖 `LiveEventPipeline` 或 `Venue`，便于在
/// 不同运行时、进程和语言绑定中复用同一套未知结果保护。
pub struct PortExecutionService<'a, V: VenuePort, P: ExecutionEventPort> {
    venue: &'a mut V,
    events: &'a mut P,
    worker_id: &'a str,
    now: u64,
    source_seq: &'a mut u64,
    registration_correlation: Option<String>,
    instrument_spec: Option<TradingInstrumentSpec>,
    spread_store: Option<&'a dyn SpreadOrderGroupStore>,
}

/// 多腿订单组提交屏障的**唯一实现**，由执行网关在写入任何事实之前调用。
///
/// 组一旦进入 `ReconcileRequired`（已有腿结果未知）或 `HedgeRequired`（已有确认敞口
/// 等待补偿），同组其余腿必须停止自动提交：未知敞口不能靠再加一条腿掩盖，必须先由
/// 对账/补偿链路给出确定事实。判定口径由 `SpreadOrderGroup::blocks_new_leg_submission`
/// 唯一持有。
///
/// 三条 fail-closed 纪律（V10 §4.10 把屏障从 CLI 下沉到这里的原因）：
/// 1. 命令声明了 `spread_group_id` 而提交路径没注入组存储 —— 无法证明组是干净的，拒绝；
/// 2. 注入了组存储但快照读不到 —— 拒绝；
/// 3. 快照处于待对账/待补偿态 —— 拒绝并列出未知腿。
///
/// 不带 `spread_group_id` 的单腿命令不受影响。
pub fn spread_group_barrier(
    store: Option<&dyn SpreadOrderGroupStore>,
    command: &ControlCommand,
) -> Result<(), String> {
    let Some(group_id) = command.payload.get("spread_group_id") else {
        return Ok(());
    };
    let Some(store) = store else {
        return Err(format!(
            "FAIL_CLOSED: 命令 {} 声明属于多腿订单组 {group_id}，但提交路径未注入组快照存储，禁止提交该腿",
            command.command_id
        ));
    };
    let Some(group) = store.load(group_id).map_err(|error| {
        format!("FAIL_CLOSED: 读取多腿订单组 {group_id} 快照失败，禁止提交腿: {error}",)
    })?
    else {
        return Err(format!(
            "FAIL_CLOSED: 命令 {} 属于多腿订单组 {group_id}，但快照不存在，禁止提交该腿",
            command.command_id
        ));
    };
    if group.blocks_new_leg_submission() {
        let unknown_legs = group
            .legs
            .iter()
            .filter(|leg| leg.order.status == OrderStatus::Unknown)
            .map(|leg| leg.leg_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        return Err(format!(
            "FAIL_CLOSED: 多腿订单组 {group_id} 状态为 {:?}，禁止提交剩余腿（未知腿: {unknown_legs}），须先完成对账/补偿",
            group.status
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortExecutionResult {
    pub message: String,
    pub event_count: usize,
}

/// 统一执行网关名称。新运行时、Paper、回测和语言绑定应依赖这个端口化
/// 入口；保留 `PortExecutionService` 作为兼容的具体类型名，避免已有适配器
/// 在迁移期间被迫同时修改。
pub type ExecutionGateway<'a, V, P> = PortExecutionService<'a, V, P>;

impl<'a, V: VenuePort, P: ExecutionEventPort> PortExecutionService<'a, V, P> {
    pub fn new(
        venue: &'a mut V,
        events: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> Self {
        Self {
            venue,
            events,
            worker_id,
            now,
            source_seq,
            registration_correlation: None,
            instrument_spec: None,
            spread_store: None,
        }
    }

    /// 绑定多腿订单组快照存储：只有注入组存储的提交路径才可能提交带
    /// `spread_group_id` 的腿，其余路径对这类命令一律 fail-closed（见
    /// [`spread_group_barrier`]）。屏障判定住在网关而不是 CLI，是为了让任何
    /// 绕过 CLI 的调用方（回测、语言绑定、未来执行器）也无法跳过它。
    pub fn with_spread_group_store(mut self, store: &'a dyn SpreadOrderGroupStore) -> Self {
        self.spread_store = Some(store);
        self
    }

    /// 为本次提交绑定控制面/策略意图关联号。它只影响首次订单登记，重复
    /// 提交仍由 EventLog 中已有的 client_order_id 状态决定。
    pub fn with_registration_correlation(mut self, correlation_id: impl Into<String>) -> Self {
        self.registration_correlation = Some(correlation_id.into());
        self
    }

    pub fn with_instrument_spec(mut self, spec: TradingInstrumentSpec) -> Self {
        self.instrument_spec = Some(spec);
        self
    }

    /// 统一的控制命令提交入口。命令解析、订单校验、风险（如调用方继续使用
    /// `submit_with_risk`）和 Venue 副作用均在同一个端口化服务中完成。
    pub fn submit_command(
        &mut self,
        command: &ControlCommand,
    ) -> Result<PortExecutionResult, String> {
        spread_group_barrier(self.spread_store, command)?;
        let order = order_from_submit_command(command)
            .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
        self.registration_correlation = Some(format!("control:{}", command.command_id));
        let result = self.submit(order);
        self.registration_correlation = None;
        result
    }

    pub fn submit_command_with_risk<R: RiskPort>(
        &mut self,
        command: &ControlCommand,
        risk: &R,
    ) -> Result<PortExecutionResult, String> {
        spread_group_barrier(self.spread_store, command)?;
        let order = order_from_submit_command(command)
            .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
        self.registration_correlation = Some(format!("control:{}", command.command_id));
        let result = self.submit_with_risk(order, risk);
        self.registration_correlation = None;
        result
    }

    pub fn submit(&mut self, order: qx_core::Order) -> Result<PortExecutionResult, String> {
        order
            .validate()
            .map_err(|error| format!("订单校验失败: {error}"))?;
        if let Some(existing) = self
            .events
            .orders()
            .into_iter()
            .find(|existing| existing.client_id == order.client_id)
        {
            if existing.status.is_terminal()
                || matches!(
                    existing.status,
                    qx_core::OrderStatus::Accepted
                        | qx_core::OrderStatus::Working
                        | qx_core::OrderStatus::PartiallyFilled
                        | qx_core::OrderStatus::Filled
                )
            {
                return Ok(PortExecutionResult {
                    message: format!(
                        "ALREADY_APPLIED_FROM_EVENT_LOG status={:?}",
                        existing.status
                    ),
                    event_count: 0,
                });
            }
            if matches!(
                existing.status,
                qx_core::OrderStatus::Submitted | qx_core::OrderStatus::Unknown
            ) {
                self.append(
                    ExecutionEvent::ReconcileRequired {
                        client_order_id: existing.client_id,
                    },
                    format!("{}:ambiguous-submit:{}", self.worker_id, existing.client_id),
                )?;
                return Err(format!(
                    "订单 {} 已离开本地但未有 Accepted 事实，必须先对账，禁止自动补单",
                    existing.client_id
                ));
            }
        } else {
            self.events.register_order(
                order.clone(),
                self.now,
                Some(
                    self.registration_correlation.clone().unwrap_or_else(|| {
                        format!("{}:submit:{}", self.worker_id, order.client_id)
                    }),
                ),
            )?;
        }

        let registered = self
            .events
            .orders()
            .into_iter()
            .find(|existing| existing.client_id == order.client_id)
            .ok_or_else(|| "订单注册后无法读取订单".to_string())?;
        let facts = match self.venue.submit_order(registered, self.now) {
            Ok(facts) if facts.is_empty() => {
                self.mark_reconcile(order.client_id, "empty-venue-response")?;
                return Err("Venue 未返回任何订单事实，结果未知，必须先对账".into());
            }
            Ok(facts) => facts,
            Err(error) => {
                self.mark_reconcile(order.client_id, "submit-error")?;
                return Err(format!(
                    "Venue submit 失败/结果未知: {error}；订单已标记为待对账"
                ));
            }
        };
        if let Err(error) = validate_submit_events(&order, &facts) {
            self.mark_reconcile(order.client_id, "invalid-submit-response")?;
            return Err(format!(
                "Venue 返回非法下单事实，结果未知，必须先对账: {error}"
            ));
        }
        let event_count = facts.len();
        let has_reconcile = facts
            .iter()
            .any(|fact| matches!(fact, ExecutionEvent::ReconcileRequired { .. }));
        // 关联号必须含客户单号：EventLog 以 `correlation_id:source_seq` 去重，而每条
        // 命令都从 0 重新计数，只用 `(worker, venue)` 会让后一笔订单的 Accepted/
        // ReconcileRequired 被判为重放而静默丢弃。
        let venue_correlation = format!(
            "{}:venue:{}:{}",
            self.worker_id,
            self.venue.venue_id(),
            order.client_id
        );
        for fact in facts {
            self.append(fact, venue_correlation.clone())?;
        }
        if has_reconcile {
            return Err("Venue 返回待对账事实，禁止继续自动提交".into());
        }
        Ok(PortExecutionResult {
            message: format!("SUBMITTED venue_events={event_count}"),
            event_count,
        })
    }

    /// 风控前置的统一提交入口。RiskPort 拒绝发生在订单登记、Venue 副作用和
    /// EventLog 事实追加之前；因此拒单不会留下“已提交但未执行”的伪订单。
    pub fn submit_with_risk<R: RiskPort>(
        &mut self,
        order: qx_core::Order,
        risk: &R,
    ) -> Result<PortExecutionResult, String> {
        order
            .validate()
            .map_err(|error| format!("订单校验失败: {error}"))?;
        let decision = risk
            .evaluate_order(&order)
            .map_err(|error| format!("账户级 RiskPort 执行失败: {error}"))?;
        if !decision.accepted {
            return Err(format!(
                "账户级 RiskPort 拒绝订单: {}",
                decision.reason_code
            ));
        }
        self.submit(order)
    }

    pub fn cancel(&mut self, client_order_id: u64) -> Result<PortExecutionResult, String> {
        if !self
            .events
            .orders()
            .into_iter()
            .any(|order| order.client_id == client_order_id)
        {
            return Err(format!("订单不存在: {client_order_id}"));
        }
        let facts = match self.venue.cancel_order(client_order_id, self.now) {
            Ok(facts) if facts.is_empty() => {
                self.mark_reconcile(client_order_id, "empty-cancel-response")?;
                return Err("Venue 未返回取消事实，结果未知，必须先对账".into());
            }
            Ok(facts) => facts,
            Err(error) => {
                self.mark_reconcile(client_order_id, "cancel-error")?;
                return Err(format!(
                    "Venue cancel 失败/结果未知: {error}；订单已标记为待对账"
                ));
            }
        };
        if let Err(error) = validate_cancel_events(client_order_id, &facts) {
            self.mark_reconcile(client_order_id, "invalid-cancel-response")?;
            return Err(format!(
                "Venue 返回非法撤单事实，结果未知，必须先对账: {error}"
            ));
        }
        let event_count = facts.len();
        for fact in facts {
            self.append(
                fact,
                format!("{}:cancel:{}", self.worker_id, client_order_id),
            )?;
        }
        Ok(PortExecutionResult {
            message: format!("CANCELLED venue_events={event_count}"),
            event_count,
        })
    }

    fn mark_reconcile(&mut self, client_order_id: u64, reason: &str) -> Result<(), String> {
        self.append(
            ExecutionEvent::ReconcileRequired { client_order_id },
            format!("{}:{reason}:{client_order_id}", self.worker_id),
        )
    }

    fn append(&mut self, event: ExecutionEvent, correlation_id: String) -> Result<(), String> {
        *self.source_seq = self.source_seq.saturating_add(1);
        let event_ts = match &event {
            ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => fill.ts,
            _ => self.now,
        };
        let event = match (self.instrument_spec.as_ref(), event) {
            (Some(spec), ExecutionEvent::Fill(fill)) => ExecutionEvent::FillWithSpec {
                fill,
                spec: Box::new(spec.clone()),
            },
            (_, event) => event,
        };
        self.events.append_execution_event(ExecutionEventEnvelope {
            event,
            event_ts,
            receive_ts: self.now,
            source_seq: *self.source_seq,
            correlation_id,
        })
    }
}

/// Explicit recovery result for a spread group whose confirmed exposure needs
/// to be offset. The worker never submits a hedge for an unknown original
/// leg; that case remains `ReconcileRequired` and must be resolved by the
/// venue reconciliation flow first.
#[derive(Clone, Debug)]
pub struct HedgeRecoveryOutcome {
    pub group: SpreadOrderGroup,
    pub attempted: usize,
    pub completed: bool,
    pub errors: Vec<String>,
}

const HEDGE_RECOVERY_LEASE_MS: u64 = 30_000;

pub trait HedgeOrderValidator {
    fn validate(&self, order: &Order) -> Result<(), String>;
}

impl<F> HedgeOrderValidator for F
where
    F: Fn(&Order) -> Result<(), String>,
{
    fn validate(&self, order: &Order) -> Result<(), String> {
        self(order)
    }
}

/// Restart-safe single-node hedge recovery worker.
///
/// Compensation client ids are deterministic for `(group_id, leg_id)`, so a
/// retry after a process restart reuses the same order identity. The event
/// port remains the source of truth for the compensation order status; the
/// group snapshot only records that all confirmed exposure has been hedged.
pub struct HedgeRecoveryWorker<'a, R: VenueRouterPort, P: ExecutionEventPort> {
    router: &'a mut R,
    events: &'a mut P,
    store: &'a mut dyn SpreadOrderGroupStore,
    group: SpreadOrderGroup,
    worker_id: &'a str,
    claim_owner: String,
    now: u64,
    source_seq: &'a mut u64,
}

impl<'a, R: VenueRouterPort, P: ExecutionEventPort> HedgeRecoveryWorker<'a, R, P> {
    pub fn new(
        group: SpreadOrderGroup,
        store: &'a mut dyn SpreadOrderGroupStore,
        router: &'a mut R,
        events: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> qx_core::QxResult<Self> {
        group.validate_persisted()?;
        Ok(Self {
            router,
            events,
            store,
            group,
            worker_id,
            claim_owner: format!("{}:{}:{}", worker_id, std::process::id(), now),
            now,
            source_seq,
        })
    }

    fn load_group(&mut self) -> Result<(), String> {
        if let Some(saved) = self.store.load(&self.group.group_id)? {
            if saved.strategy_id != self.group.strategy_id
                || saved.legs.len() != self.group.legs.len()
                || saved
                    .legs
                    .iter()
                    .zip(&self.group.legs)
                    .any(|(left, right)| {
                        left.leg_id != right.leg_id || left.venue_id != right.venue_id
                    })
            {
                return Err("Hedge 恢复快照与请求的策略、腿或 Venue 路由不一致".into());
            }
            self.group = saved;
        }
        Ok(())
    }

    /// Execute all confirmed compensation targets once, then persist Hedged
    /// only when the event port confirms every deterministic hedge order is
    /// Filled. Active orders are left pending and are never submitted twice.
    pub fn execute(self) -> Result<HedgeRecoveryOutcome, String> {
        self.execute_with_validator(None)
    }

    /// 在实际补偿单提交前注入账户级风控校验。恢复 worker 本身不依赖
    /// `qx-risk` 的具体快照来源，由运行时按当前 Venue/账户提供校验器；
    /// 未提供时仍保留库级 `Order::validate` 和 reduce-only 约束。
    pub fn execute_with_validator(
        mut self,
        validator: Option<&dyn HedgeOrderValidator>,
    ) -> Result<HedgeRecoveryOutcome, String> {
        self.load_group()?;
        match self.group.status {
            SpreadOrderGroupStatus::HedgeRequired => {}
            SpreadOrderGroupStatus::Hedged => {
                return Ok(HedgeRecoveryOutcome {
                    group: self.group,
                    attempted: 0,
                    completed: true,
                    errors: Vec::new(),
                });
            }
            SpreadOrderGroupStatus::ReconcileRequired => {
                return Ok(HedgeRecoveryOutcome {
                    group: self.group,
                    attempted: 0,
                    completed: false,
                    errors: vec![
                        "原始多腿订单组存在未知 Venue 状态，必须先完成对账，禁止自动对冲".into(),
                    ],
                });
            }
            status => {
                return Ok(HedgeRecoveryOutcome {
                    group: self.group,
                    attempted: 0,
                    completed: false,
                    errors: vec![format!(
                        "订单组当前为 {status:?}，只有 HedgeRequired 才能执行恢复"
                    )],
                });
            }
        }

        let targets = self.group.compensation_targets();
        if targets.is_empty() {
            return Err("HedgeRequired 订单组没有可确认的补偿目标".into());
        }
        let Some(claim_token) = self.store.try_claim(
            &self.group.group_id,
            &self.claim_owner,
            self.now,
            HEDGE_RECOVERY_LEASE_MS,
        )?
        else {
            return Ok(HedgeRecoveryOutcome {
                group: self.group,
                attempted: 0,
                completed: false,
                errors: vec!["订单组正在由其他恢复 owner 处理，跳过本轮".into()],
            });
        };
        let mut attempted = 0;
        let mut errors = Vec::new();
        let mut all_filled = true;
        for target in targets {
            if !self.store.verify_claim(
                &self.group.group_id,
                &self.claim_owner,
                claim_token,
                self.now,
            )? {
                errors.push("多腿恢复 claim 已过期或已被其他 owner 接管".into());
                all_filled = false;
                break;
            }
            let source_leg = self
                .group
                .leg(&target.leg_id)
                .map_err(|error| format!("读取补偿来源腿失败: {error:?}"))?;
            if source_leg.venue_id.trim().is_empty() {
                errors.push(format!(
                    "补偿腿 {} 缺少 Venue 路由，拒绝使用默认交易所",
                    target.leg_id
                ));
                all_filled = false;
                continue;
            }
            let client_id = compensation_client_id(&self.group, &target.leg_id);
            let existing = self
                .events
                .orders()
                .into_iter()
                .find(|order| order.client_id == client_id);
            let order = build_compensation_order(&self.group, source_leg, &target, client_id)?;
            if let Some(existing) = existing {
                if !same_compensation_order(&existing, &order) {
                    errors.push(format!(
                        "补偿 client_order_id={} 已被其他订单占用，拒绝覆盖",
                        client_id
                    ));
                    all_filled = false;
                    continue;
                }
                if existing.status == OrderStatus::Filled {
                    continue;
                }
                if matches!(existing.status, OrderStatus::Unknown) {
                    errors.push(format!("补偿订单 {} 状态未知，必须先对账", client_id));
                    all_filled = false;
                    continue;
                }
                if !matches!(
                    existing.status,
                    OrderStatus::PendingSubmit | OrderStatus::Submitted
                ) {
                    errors.push(format!(
                        "补偿订单 {} 当前状态 {:?}，未达到 Filled",
                        client_id, existing.status
                    ));
                    all_filled = false;
                    continue;
                }
                all_filled = false;
                continue;
            }

            if let Some(validator) = validator {
                if let Err(error) = validator.validate(&order) {
                    errors.push(format!(
                        "补偿腿 {} 未通过账户级风控，拒绝自动对冲: {error}",
                        target.leg_id
                    ));
                    all_filled = false;
                    continue;
                }
            }
            self.events
                .register_order(
                    order.clone(),
                    self.now,
                    Some(format!(
                        "{}:hedge:{}:{}:register",
                        self.worker_id, self.group.group_id, target.leg_id
                    )),
                )
                .map_err(|error| format!("注册补偿订单 {} 失败: {error}", client_id))?;
            attempted += 1;
            let facts = match self
                .router
                .submit_order(&source_leg.venue_id, order, self.now)
            {
                Ok(facts) if facts.is_empty() => {
                    errors.push(format!(
                        "补偿腿 {} 未返回事实，结果未知，必须对账",
                        target.leg_id
                    ));
                    all_filled = false;
                    continue;
                }
                Ok(facts) => facts,
                Err(error) => {
                    errors.push(format!(
                        "补偿腿 {} 提交失败/结果未知: {error}",
                        target.leg_id
                    ));
                    all_filled = false;
                    continue;
                }
            };
            for fact in facts {
                self.append_fact(
                    fact,
                    format!(
                        "{}:hedge:{}:{}",
                        self.worker_id, self.group.group_id, target.leg_id
                    ),
                )?;
            }
            let filled = self
                .events
                .orders()
                .into_iter()
                .find(|candidate| candidate.client_id == client_id)
                .is_some_and(|candidate| candidate.status == OrderStatus::Filled);
            if !filled {
                all_filled = false;
            }
        }

        if all_filled && errors.is_empty() {
            self.group
                .mark_hedged()
                .map_err(|error| format!("标记多腿订单组已对冲失败: {error:?}"))?;
        }
        self.store
            .save_claimed(&self.group, &self.claim_owner, claim_token, self.now)?;
        self.store
            .release_claim(&self.group.group_id, &self.claim_owner, claim_token)?;
        Ok(HedgeRecoveryOutcome {
            group: self.group,
            attempted,
            completed: all_filled && errors.is_empty(),
            errors,
        })
    }

    fn append_fact(&mut self, event: ExecutionEvent, correlation_id: String) -> Result<(), String> {
        *self.source_seq = self.source_seq.saturating_add(1);
        let event_ts = match &event {
            ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => fill.ts,
            _ => self.now,
        };
        self.events.append_execution_event(ExecutionEventEnvelope {
            event,
            event_ts,
            receive_ts: self.now,
            source_seq: *self.source_seq,
            correlation_id,
        })
    }
}

fn build_compensation_order(
    group: &SpreadOrderGroup,
    source_leg: &qx_zhenlu::SpreadOrderLeg,
    target: &qx_zhenlu::SpreadCompensationTarget,
    client_id: u64,
) -> Result<Order, String> {
    let mut policy = source_leg.order.policy.unwrap_or_default();
    policy.reduce_only = true;
    let order = Order {
        client_id,
        instrument: target.instrument.clone(),
        side: target.side,
        qty: target.qty,
        limit: None,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: target.account_id.clone(),
        trace: Some(OrderTrace {
            strategy_id: Some(group.strategy_id.clone()),
            signal_id: None,
            intent_id: None,
            rule_version: Some("spread-hedge-v1".into()),
        }),
        policy: Some(policy),
    };
    order.validate()?;
    Ok(order)
}

fn same_compensation_order(left: &Order, right: &Order) -> bool {
    left.instrument == right.instrument
        && left.side == right.side
        && left.qty == right.qty
        && left.account_id == right.account_id
        && left.policy == right.policy
}

fn compensation_client_id(group: &SpreadOrderGroup, leg_id: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in group
        .group_id
        .as_bytes()
        .iter()
        .chain([0_u8].iter())
        .chain(leg_id.as_bytes().iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3_u64);
    }
    let mut candidate = hash | (1_u64 << 63);
    if candidate == 0 {
        candidate = 1_u64 << 63;
    }
    while group
        .legs
        .iter()
        .any(|leg| leg.order.client_id == candidate)
    {
        candidate = candidate.wrapping_add(1);
    }
    candidate
}

/// 将 Venue 返回的订单事实统一归约到执行事件端口（EventLog/Ledger 管线）。
///
/// `source_seq` 由调用方持有；重复回报仍由端口的幂等语义去重。Paper、Live 与
/// 任何自定义运行时共用这一条实现，不允许再按后端分叉。
pub fn ingest_venue_events<P: ExecutionEventPort>(
    pipeline: &mut P,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    ingest_venue_events_with_pipeline(pipeline, events, worker_id, receive_ts, source_seq, None)
        .map_err(|error| format!("Venue 执行回报事实归约失败: {error}"))
}

/// 将含有冻结产品规格的 Venue 成交归约为衍生品记账事实。
///
/// 规格不会被写进 `Filled` 事件本身；完整的衍生品 LedgerEntry 会随同一批
/// `LedgerApplied` 事实写入 EventLog，因此重启后的 ReplayVerifier 仍可准确重建。
pub fn ingest_venue_events_with_spec<P: ExecutionEventPort>(
    pipeline: &mut P,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
    spec: &TradingInstrumentSpec,
) -> Result<usize, String> {
    ingest_venue_events_with_pipeline(
        pipeline,
        events,
        worker_id,
        receive_ts,
        source_seq,
        Some(spec),
    )
    .map_err(|error| format!("带产品规格的 Venue 成交事实归约失败: {error}"))
}

fn ingest_venue_events_with_pipeline<P: ExecutionEventPort>(
    pipeline: &mut P,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
    spec: Option<&TradingInstrumentSpec>,
) -> Result<usize, String> {
    let mut event_count = 0_usize;
    let mut rejections = Vec::new();
    for event in events {
        event_count += 1;
        *source_seq = source_seq.saturating_add(1);
        let (event, event_ts, correlation_id) = match event {
            VenueEvent::Accepted {
                client_order_id,
                venue_order_id,
                ts,
            } => (
                ExecutionEvent::Accepted {
                    client_order_id,
                    venue_order_id,
                },
                ts,
                format!("{worker_id}:accepted:{client_order_id}"),
            ),
            VenueEvent::Fill(fill) => {
                let event_ts = fill.ts;
                let fill_order_id = fill.order_id;
                let seq = *source_seq;
                // 精度闸门：带冻结产品规格的回报若落在 tick/step 之外，就是与规格冲突的
                // 事实，只能转成待对账（不记账、不改状态），并把原因回传给 worker。
                let violation = spec
                    .and_then(|spec| spec.validate_fill(fill.qty.raw(), fill.price.raw()).err());
                if let Some(error) = violation {
                    rejections.push(format!(
                        "订单 {fill_order_id} 成交回报违反产品规格: {error:?}"
                    ));
                    (
                        ExecutionEvent::ReconcileRequired {
                            client_order_id: fill_order_id,
                        },
                        event_ts,
                        format!("{worker_id}:precision:{fill_order_id}:{seq}"),
                    )
                } else {
                    let event = match spec {
                        Some(spec) => ExecutionEvent::FillWithSpec {
                            fill: Box::new(fill),
                            spec: Box::new(spec.clone()),
                        },
                        None => ExecutionEvent::Fill(Box::new(fill)),
                    };
                    (
                        event,
                        event_ts,
                        format!("{worker_id}:fill:{fill_order_id}:{seq}"),
                    )
                }
            }
            VenueEvent::Cancelled {
                client_order_id,
                ts,
            } => (
                ExecutionEvent::Cancelled { client_order_id },
                ts,
                format!("{worker_id}:cancelled:{client_order_id}"),
            ),
        };
        pipeline.append_execution_event(ExecutionEventEnvelope {
            event,
            event_ts,
            receive_ts,
            source_seq: *source_seq,
            correlation_id,
        })?;
    }
    if !rejections.is_empty() {
        return Err(format!(
            "成交回报精度越界，已转为待对账事实: {}",
            rejections.join("; ")
        ));
    }
    Ok(event_count)
}

/// 执行一条已经通过控制面审计的 SubmitOrder。
///
/// 这里不负责账户/Venue 拓扑授权，也不负责 ControlPlane 的 Accepted/终态回写；
/// 它只负责订单事实注册、未知结果保护、Venue submit 和标准回报归约。这样
/// Paper、Binance 和未来执行 worker 可以共享同一副作用边界。
///
/// `spread_store` 是多腿订单组快照来源：带 `spread_group_id` 的命令必须由注入了
/// 组存储的提交路径执行，否则 [`spread_group_barrier`] 以 fail-closed 拒绝。
#[allow(clippy::too_many_arguments)]
pub fn submit_order_via_gateway<V: VenuePort, P: ExecutionEventPort>(
    command: &ControlCommand,
    venue: &mut V,
    events: &mut P,
    worker_id: &str,
    now: u64,
    source_seq: &mut u64,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<PortExecutionResult, String> {
    spread_group_barrier(spread_store, command)?;
    let mut gateway = ExecutionGateway::new(venue, events, worker_id, now, source_seq);
    if let Some(store) = spread_store {
        gateway = gateway.with_spread_group_store(store);
    }
    gateway.submit_command(command)
}

#[allow(clippy::too_many_arguments)]
pub fn submit_order_via_gateway_with_risk<V: VenuePort, P: ExecutionEventPort, R: RiskPort>(
    command: &ControlCommand,
    venue: &mut V,
    events: &mut P,
    worker_id: &str,
    now: u64,
    source_seq: &mut u64,
    risk: &R,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<PortExecutionResult, String> {
    spread_group_barrier(spread_store, command)?;
    let mut gateway = ExecutionGateway::new(venue, events, worker_id, now, source_seq);
    if let Some(store) = spread_store {
        gateway = gateway.with_spread_group_store(store);
    }
    gateway.submit_command_with_risk(command, risk)
}

#[allow(clippy::too_many_arguments)]
pub fn submit_order<V: Venue, P: ExecutionEventPort>(
    command: &ControlCommand,
    venue: &mut V,
    pipeline: &mut P,
    worker_id: &str,
    now: u64,
    source_seq: &mut u64,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<String, String> {
    spread_group_barrier(spread_store, command)?;
    let mut venue = BorrowedVenuePort::new(venue);
    let mut gateway = ExecutionGateway::new(&mut venue, pipeline, worker_id, now, source_seq);
    if let Some(store) = spread_store {
        gateway = gateway.with_spread_group_store(store);
    }
    let result = gateway.submit_command(command)?;
    Ok(result.message)
}

/// `submit_order` 的账户级风控版本，供 Paper/CCXT/Binance worker 在已有账户
/// 快照和市场规格时统一使用；Venue 仍只接收通过预检的订单。
#[allow(clippy::too_many_arguments)]
pub fn submit_order_with_risk<V: Venue, P: ExecutionEventPort>(
    command: &ControlCommand,
    venue: &mut V,
    pipeline: &mut P,
    worker_id: &str,
    now: u64,
    source_seq: &mut u64,
    context: &RiskExecutionContext<'_>,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<String, String> {
    spread_group_barrier(spread_store, command)?;
    let mut venue = BorrowedVenuePort::new(venue);
    let mut gateway = ExecutionGateway::new(&mut venue, pipeline, worker_id, now, source_seq);
    if let Some(store) = spread_store {
        gateway = gateway.with_spread_group_store(store);
    }
    let result = if let Some(spec) = context.risk.instrument_spec.clone() {
        gateway
            .with_instrument_spec(spec)
            .submit_command_with_risk(
                command,
                &RiskContextPort {
                    risk: context.risk,
                    position: context.position,
                },
            )?
    } else {
        gateway.submit_command_with_risk(
            command,
            &RiskContextPort {
                risk: context.risk,
                position: context.position,
            },
        )?
    };
    Ok(result.message)
}

/// 把“结果未知，必须先对账”写成运行时事实的 `ReconcilePort` 适配器（对账判定唯一口径在 qx-genglu 的 `order_reconcile_verdict`，本端口只落事实、不复制判定）。
///
/// Paper、CCXT 与 Binance worker 共用同一条写入路径：`source_seq` 由端口独占推进，
/// correlation id 形如 `<worker>:<tag>:<client_order_id>`（`tag` 默认 `reconcile`，
/// 各 worker 用自己的分类标签以保持既有审计口径）。`reason` 不会进入事件载荷
/// （EventLog schema 不变），但必须非空，并在写入失败时随错误一起返回，避免
/// “未知结果”连原因都留不下。
pub struct EventLogReconcilePort<'a, P: EventAppender> {
    pipeline: &'a mut P,
    worker_id: &'a str,
    now: u64,
    source_seq: &'a mut u64,
    event_ts: u64,
    tag: &'static str,
}

impl<'a, P: EventAppender> EventLogReconcilePort<'a, P> {
    pub fn new(pipeline: &'a mut P, worker_id: &'a str, now: u64, source_seq: &'a mut u64) -> Self {
        Self {
            pipeline,
            worker_id,
            now,
            source_seq,
            event_ts: now,
            tag: "reconcile",
        }
    }

    /// 事件时间取远端回报时间，`now` 仍是本地接收时间。
    pub fn at_event_ts(mut self, event_ts: u64) -> Self {
        self.event_ts = event_ts;
        self
    }

    /// 覆盖 correlation id 的分类标签，保持各 worker 既有口径不变。
    pub fn with_tag(mut self, tag: &'static str) -> Self {
        self.tag = tag;
        self
    }
}

impl<P: EventAppender> ReconcilePort for EventLogReconcilePort<'_, P> {
    fn require_reconcile(&mut self, client_order_id: u64, reason: &str) -> Result<(), String> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(format!(
                "标记订单 {client_order_id} 待对账失败: 缺少未知结果的原因"
            ));
        }
        *self.source_seq = self.source_seq.saturating_add(1);
        let source_seq = *self.source_seq;
        self.pipeline
            .append_execution_event(ExecutionEventEnvelope {
                event: ExecutionEvent::ReconcileRequired { client_order_id },
                event_ts: self.event_ts,
                receive_ts: self.now,
                source_seq,
                correlation_id: format!("{}:{}:{client_order_id}", self.worker_id, self.tag),
            })
            .map_err(|error| format!("{error}；待对账原因: {reason}"))
    }
}

/// Paper 执行的统一单轨入口：风控预检、行情门禁与事实写入语义只由执行端口决定。
///
/// 具体 EventLog 后端（文件/分段、SQLite、PostgreSQL）由调用方按运行时配置打开并
/// 注入，本 crate 不再感知任何后端与特性开关，因此 Paper、Live 与自定义运行时共用
/// 同一条判定链。`allow_synthetic_quote` 是**唯一**的伪造价开关，只允许离线 smoke
/// fixture 显式传 `true`；生产 worker 必须保持 `false` 并提供 `Some(market_quote)`。
#[allow(clippy::too_many_arguments)] // V10 P1b：组存储作为最后一个形参是刻意的，让编译器逐个点名提交入口是否接了屏障。
pub fn execute_paper_submit_effect<P: ExecutionEventPort + application::LedgerProbe>(
    command: &ControlCommand,
    pipeline: &mut P,
    now: u64,
    risk: Option<RiskContext>,
    position: Option<OrderRiskPosition>,
    market_quote: Option<QuoteTick>,
    allow_synthetic_quote: bool,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<String, String> {
    // 多腿屏障由网关统一裁决：带 `spread_group_id` 的腿若提交路径没有注入组存储，
    // 无法证明组干净，直接 fail-closed（见 [`spread_group_barrier`]）。
    spread_group_barrier(spread_store, command)?;
    let order = order_from_submit_command(command)
        .map_err(|error| format!("Paper SubmitOrder 订单载荷非法: {error:?}"))?;
    // fail-closed：风控上下文与持仓快照必须成对提供。`(None, None)` 过去会跳过
    // 账户级预检直接下单，等价于风控缺失时 fail-open，现已禁止。消息保留
    // `FAIL_CLOSED:` 前缀，供 CLI 与监控按前缀分类拒绝原因。
    let (risk_context, position_snapshot) = match (risk.as_ref(), position.as_ref()) {
        (Some(risk), Some(position)) => (risk, position),
        _ => {
            return Err(
                "FAIL_CLOSED: risk context missing: RiskContext and OrderRiskPosition required"
                    .into(),
            )
        }
    };
    // fail-closed：撮合行情必须来自 EventLog 的真实行情事实；缺行情时不再伪造
    // `100` 盘口价。`allow_synthetic_quote` 只允许离线 smoke fixture 显式开启。
    if market_quote.is_none() && !allow_synthetic_quote {
        return Err("FAIL_CLOSED: market quote missing: EventLog quote is required".into());
    }
    let quote = market_quote.unwrap_or_else(|| {
        // WARN: 合成盘口仅供 Paper smoke fixture 使用（显式 opt-in）。生产路径
        // 必须传入最新行情事实，否则上面的分支已经以 fail-closed 拒绝撮合。
        let ask = order.limit.unwrap_or_else(|| Price::from_i64(100));
        let bid = Price::from_raw(ask.raw().saturating_sub(SCALE));
        QuoteTick::new(
            now.saturating_add(1),
            bid,
            Quantity::from_i64(1_000),
            ask,
            Quantity::from_i64(1_000),
            1,
        )
    });
    let mut venue = PaperVenue::new("paper");
    let mut source_seq = 0_u64;
    // Paper 与 worker/实盘共用同一副作用编排（`ExecutionGateway` = `PortExecutionService`）：
    // 空的 Venue 回报同样要转成"未知结果 → 待对账"，不允许第二套实现各自解释。
    let risk_port = RiskContextPort {
        risk: risk_context,
        position: position_snapshot,
    };
    let submit_result = {
        let mut venue_port = BorrowedVenuePort::new(&mut venue);
        let mut gateway = ExecutionGateway::new(
            &mut venue_port,
            pipeline,
            "paper-execution",
            now,
            &mut source_seq,
        );
        if let Some(store) = spread_store {
            gateway = gateway.with_spread_group_store(store);
        }
        let message = match risk_context.instrument_spec.clone() {
            Some(spec) => {
                gateway
                    .with_instrument_spec(spec)
                    .submit_command_with_risk(command, &risk_port)?
                    .message
            }
            None => {
                gateway
                    .submit_command_with_risk(command, &risk_port)?
                    .message
            }
        };
        message
    };
    if submit_result.starts_with("ALREADY_APPLIED_FROM_EVENT_LOG") {
        return Ok(submit_result);
    }
    if quote.bid.raw() <= 0
        || quote.ask.raw() <= 0
        || quote.bid.raw() > quote.ask.raw()
        || quote.bid_qty.raw() <= 0
        || quote.ask_qty.raw() <= 0
    {
        return Err("Paper 行情报价非法，拒绝撮合".into());
    }
    let quote_seq = if quote.source_seq == 0 {
        1
    } else {
        quote.source_seq
    };
    pipeline.append_execution_event(ExecutionEventEnvelope {
        event: ExecutionEvent::MarketQuote {
            instrument: order.instrument.clone(),
            quote,
        },
        event_ts: now.saturating_add(1),
        receive_ts: now.saturating_add(1),
        source_seq: quote_seq,
        correlation_id: format!("paper-execution:market:{}:{quote_seq}", order.instrument),
    })?;
    let fills = venue.on_quote(&order.instrument, quote);
    let fill_count = fills.len();
    if let Some(spec) = risk_context.instrument_spec.as_ref() {
        ingest_venue_events_with_spec(
            pipeline,
            fills,
            "paper-execution",
            quote.ts,
            &mut source_seq,
            spec,
        )?;
    } else {
        ingest_venue_events(
            pipeline,
            fills,
            "paper-execution",
            quote.ts,
            &mut source_seq,
        )?;
    }
    Ok(format!(
        "PAPER_EXECUTED fills={} ledger_entries={}",
        fill_count,
        pipeline.ledger_entry_count()
    ))
}

#[cfg(test)]
mod tests;
