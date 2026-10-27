//! 统一执行层边界。
//!
//! Venue 只返回 `VenueEvent`，执行层负责把这些事件转换成运行时标准事实，
//! 再通过本 crate `application` 子模块的执行端口交给 EventLog、订单状态和 Ledger。Paper、
//! Binance 以及未来连接器都必须复用这里的事实转换，不能在 CLI 中各写一套。
//! 本 crate 不依赖任何具体运行时或存储后端。

pub mod algo_execution;
pub mod application;
mod spread_group_barrier;
pub use algo_execution::{
    AlgoExecutionState, AlgoExecutionStatus, AlgoExecutionStep, AlgoParentOrder,
    ALGO_EXECUTION_STATE_SCHEMA_VERSION,
};
pub use application::*;
use qx_control::{cancel_order_target_from_command, order_from_submit_command, ControlCommand};
use qx_core::{DecimalRounding, ExactDecimal, FeeModel};
use qx_guanxing::QuoteTick;
use qx_risk::OrderRiskPosition;
pub use qx_zhenlu::algo::{
    plan_twap, plan_vwap, AlgoChildSlice, AlgoExecutionError, VwapVolumeBucket, MAX_ALGO_CHILDREN,
};
use qx_zhenlu::{
    PaperVenue, RiskContext, SpreadOrderGroup, SpreadOrderGroupStatus, SpreadOrderGroupStore,
    Venue, VenueEvent,
};

pub fn spread_group_barrier(
    store: Option<&dyn SpreadOrderGroupStore>,
    command: &ControlCommand,
) -> Result<(), String> {
    spread_group_barrier::check_spread_group_barrier(store, command)
}

pub struct RiskExecutionContext<'a> {
    pub risk: &'a RiskContext,
    pub position: &'a OrderRiskPosition,
}

/// Required inputs for the shared order submission boundary. The group store stays
/// explicit so a caller cannot accidentally bypass the multi-leg barrier.
pub struct SubmitOrderWithRiskRequest<'a, V: Venue, P: ExecutionEventPort> {
    pub command: &'a ControlCommand,
    pub venue: &'a mut V,
    pub pipeline: &'a mut P,
    pub worker_id: &'a str,
    pub now: u64,
    pub source_seq: &'a mut u64,
    pub context: &'a RiskExecutionContext<'a>,
    pub spread_store: Option<&'a dyn SpreadOrderGroupStore>,
}

/// Required Paper execution inputs. Every safety and pricing decision is a named,
/// mandatory field; this type intentionally has no `Default` implementation.
pub struct PaperSubmitRequest<'a, P: ExecutionEventPort + application::LedgerProbe> {
    pub command: &'a ControlCommand,
    pub pipeline: &'a mut P,
    pub now: u64,
    pub risk: Option<RiskContext>,
    pub position: Option<OrderRiskPosition>,
    pub market_quote: Option<QuoteTick>,
    pub fee_model: Box<dyn FeeModel + Send>,
    pub allow_synthetic_quote: bool,
    pub spread_store: Option<&'a dyn SpreadOrderGroupStore>,
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
            VenueEvent::FeeAdjustment {
                account_id,
                venue_id,
                order_id,
                instrument,
                fees,
                ..
            } => ExecutionEvent::FeeAdjustment {
                account_id,
                venue_id,
                order_id,
                instrument,
                fees,
            },
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

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        *self.venue.capabilities()
    }

    fn execution_readiness(&self) -> Result<(), String> {
        self.venue
            .execution_readiness()
            .map_err(|error| format!("Venue {} 尚未就绪: {error:?}", self.venue_id()))
    }

    fn submit_order(
        &mut self,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.validate_submit_capabilities(&order)?;
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
        self.capabilities().validate_cancel()?;
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

    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
        *self.venue.capabilities()
    }

    fn execution_readiness(&self) -> Result<(), String> {
        self.venue
            .execution_readiness()
            .map_err(|error| format!("Venue {} 尚未就绪: {error:?}", self.venue_id()))
    }

    fn submit_order(
        &mut self,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.validate_submit_capabilities(&order)?;
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
        self.capabilities().validate_cancel()?;
        self.venue
            .cancel(client_order_id, ts)
            .map(venue_events_to_application)
            .map_err(|error| format!("Venue cancel 失败: {error:?}"))
    }
}

impl<V: Venue + ?Sized> VenueRouterPort for BorrowedVenuePort<'_, V> {
    fn capabilities_for(
        &self,
        venue_id: &str,
    ) -> Option<qx_zhenlu::venue_capabilities::VenueCapabilities> {
        (self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id()))
        .then(|| self.capabilities())
    }

    fn execution_readiness_for(&self, venue_id: &str) -> Result<(), String> {
        if self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id())
        {
            VenuePort::execution_readiness(self)
        } else {
            Err(format!(
                "Venue readiness 路由不匹配: requested={venue_id} configured={}",
                self.venue_id()
            ))
        }
    }

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
    fn capabilities_for(
        &self,
        venue_id: &str,
    ) -> Option<qx_zhenlu::venue_capabilities::VenueCapabilities> {
        (self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id()))
        .then(|| self.capabilities())
    }

    fn execution_readiness_for(&self, venue_id: &str) -> Result<(), String> {
        if self.accept_any_venue
            || venue_id.trim().is_empty()
            || venue_id.eq_ignore_ascii_case(self.venue_id())
        {
            VenuePort::execution_readiness(self)
        } else {
            Err(format!(
                "Venue readiness 路由不匹配: requested={venue_id} configured={}",
                self.venue_id()
            ))
        }
    }

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
    fn evaluate_order(&self, order: &qx_core::Order) -> Result<RiskVerdict, String> {
        let decision = self.risk.evaluate_order(order, self.position);
        if decision.allowed {
            Ok(RiskVerdict::Allow)
        } else {
            Ok(RiskVerdict::Reject {
                reason: format!(
                    "RiskContext {}: {}",
                    decision.rule_set_version,
                    decision.violations.join("; ")
                ),
            })
        }
    }
}

impl RiskPort for CanonicalRiskPort<'_> {
    fn evaluate_order(&self, order: &qx_core::Order) -> Result<RiskVerdict, String> {
        let decision = qx_risk::RiskEngine::evaluate_order(self.context, order);
        if decision.allowed {
            Ok(RiskVerdict::Allow)
        } else {
            Ok(RiskVerdict::Reject {
                reason: format!(
                    "CanonicalRiskEngine {}: {}",
                    decision.rule_set_version,
                    decision.violations.join("; ")
                ),
            })
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
    cancel_operation_id: Option<String>,
    instrument_spec: Option<TradingInstrumentSpec>,
    spread_store: Option<&'a dyn SpreadOrderGroupStore>,
    submission_group_id: Option<String>,
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
            cancel_operation_id: None,
            instrument_spec: None,
            spread_store: None,
            submission_group_id: None,
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
        self.submission_group_id = command.payload.get("spread_group_id").cloned();
        let result = self.submit(order);
        self.registration_correlation = None;
        self.submission_group_id = None;
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
        self.submission_group_id = command.payload.get("spread_group_id").cloned();
        let result = self.submit_with_risk(order, risk);
        self.registration_correlation = None;
        self.submission_group_id = None;
        result
    }

    pub fn submit(&mut self, order: qx_core::Order) -> Result<PortExecutionResult, String> {
        self.validate_order(&order)?;
        self.venue.validate_submit_capabilities(&order)?;
        let existing_order = self
            .events
            .orders()
            .into_iter()
            .find(|existing| existing.client_id == order.client_id);
        if let Some(existing) = existing_order.as_ref() {
            if !existing.same_submission_identity(&order) {
                return Err(format!(
                    "client_order_id {} 已绑定到不同的账户、标的、方向、数量或执行策略，拒绝复用",
                    order.client_id
                ));
            }
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
            // `CancelPending` 必须和 `Submitted`/`Unknown` 同一档：状态迁移表里
            // `CancelPending` 只能走到 `Cancelled`/`Filled`/`Unknown`，`Submitted` 非法，
            // 所以对一条撤单在途的订单补发 `venue.submit_order` 只会用同一个
            // `client_order_id` 再下一单，不是"重试"。
            // 正式的 LiveEventPipeline 会在网络副作用前把新订单持久化为 Submitted。
            // 其它 OrderStore 可能仍保留 PendingSubmit；该状态同样不能证明远端未收到，
            // 因而已有 PendingSubmit/Submitted 都必须先对账，不能把重试当作恢复。
            if matches!(
                existing.status,
                qx_core::OrderStatus::Submitted
                    | qx_core::OrderStatus::PendingSubmit
                    | qx_core::OrderStatus::CancelPending
                    | qx_core::OrderStatus::Unknown
            ) {
                self.append(
                    ExecutionEvent::ReconcileRequired {
                        client_order_id: existing.client_id,
                    },
                    format!("{}:ambiguous-submit:{}", self.worker_id, existing.client_id),
                )?;
                return Err(format!(
                    "订单 {} 状态为 {:?}，先决条件不满足：已有远端事实但未确认，或撤单在途，必须先对账，禁止自动补单",
                    existing.client_id, existing.status
                ));
            }
        }
        if existing_order.is_none() {
            // A known disconnected/snapshotting venue is a safe local refusal. Check it before
            // creating a PendingSubmit record so callers can retry after the connector recovers.
            self.venue.execution_readiness()?;
            if let Some(group_id) = self.submission_group_id.as_deref() {
                let store = self.spread_store.ok_or_else(|| {
                    format!(
                        "FAIL_CLOSED: 多腿订单组 {group_id} 未绑定组状态存储，拒绝提交订单 {}",
                        order.client_id
                    )
                })?;
                store.begin_leg_submission(group_id, order.client_id)?;
            }
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
        // 提交同步回包与用户流回报必须过同一道闸门（形状契约 + 产品规格精度），
        // 否则"越界回报只能转待对账"就只剩半条纪律：同一笔成交走被推下来的路径
        // 会被拦下、不记账，走提交回包却直接入账。
        if let Err(violation) = gate_submit_facts(&order, &facts, self.instrument_spec.as_ref()) {
            self.mark_reconcile(order.client_id, violation.reason_tag())?;
            return Err(violation.to_string());
        }
        let event_count = facts.len();
        let has_reconcile = facts
            .iter()
            .any(|fact| matches!(fact, ExecutionEvent::ReconcileRequired { .. }));
        // 关联号必须含客户单号 **且** 含回报自身身份：EventLog 以
        // `correlation_id:source_seq` 去重，而每条命令都从 0 重新计数，只用
        // `(worker, venue)` 会让后一笔订单的 Accepted/ReconcileRequired 被判为
        // 重放而静默丢弃；只用客户单号又会让同一订单的第二笔成交被吞掉。
        let venue_prefix = format!(
            "{}:venue:{}:{}",
            self.worker_id,
            self.venue.venue_id(),
            order.client_id
        );
        for fact in facts {
            let correlation_id = fact_correlation(&venue_prefix, &fact);
            self.append(fact, correlation_id)?;
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
    /// EventLog 事实追加之前；因此拒单不会留下“已提交但未执行”的伪订单。两条通道
    /// 不混用（V13 §9.12 #172）：端口给出拒绝才播报「拒绝订单」并带违规清单，端口
    /// 给不出裁决才播报「执行失败」。
    pub fn submit_with_risk<R: RiskPort>(
        &mut self,
        order: qx_core::Order,
        risk: &R,
    ) -> Result<PortExecutionResult, String> {
        self.validate_order(&order)?;
        match risk
            .evaluate_order(&order)
            .map_err(|error| format!("账户级 RiskPort 执行失败: {error}"))?
        {
            RiskVerdict::Allow => {}
            RiskVerdict::Reject { reason } => {
                return Err(format!("账户级 RiskPort 拒绝订单: {reason}"))
            }
        }
        self.submit(order)
    }

    /// Fail closed on frozen market-spec identity, lot size, minimum quantity,
    /// and price tick before risk registration or any venue side effect.
    pub fn validate_order(&self, order: &qx_core::Order) -> Result<(), String> {
        order
            .validate()
            .map_err(|error| format!("订单校验失败: {error}"))?;
        if let Some(spec) = &self.instrument_spec {
            if order.instrument != spec.instrument {
                return Err(format!(
                    "订单 market spec 标的与订单不匹配: order={} spec={}",
                    order.instrument, spec.instrument
                ));
            }
            if let Some(amounts) = order.exact_amounts {
                spec.validate_exact_order_precision(amounts.quantity, amounts.limit_price)
                    .map_err(|error| format!("精确订单不满足 market spec: {error}"))?;
            } else {
                spec.validate_order(order.qty.raw(), order.limit.map(|price| price.raw()))
                    .map_err(|error| format!("订单不满足 market spec: {error}"))?;
            }
        }
        Ok(())
    }

    /// Submit with a stable caller correlation id. Algorithm children use
    /// `parent_id/child_index` so an event-log replay can identify the same
    /// child after restart without inventing a second execution path.
    pub fn submit_with_risk_and_correlation<R: RiskPort>(
        &mut self,
        order: qx_core::Order,
        risk: &R,
        correlation_id: impl Into<String>,
    ) -> Result<PortExecutionResult, String> {
        self.registration_correlation = Some(correlation_id.into());
        let result = self.submit_with_risk(order, risk);
        self.registration_correlation = None;
        result
    }

    /// Read the authoritative OMS snapshot for a client order id.
    pub fn order(&self, client_order_id: u64) -> Option<qx_core::Order> {
        self.events
            .orders()
            .into_iter()
            .find(|order| order.client_id == client_order_id)
    }

    pub fn cancel(&mut self, client_order_id: u64) -> Result<PortExecutionResult, String> {
        self.venue.capabilities().validate_cancel()?;
        let order = self
            .order(client_order_id)
            .ok_or_else(|| format!("订单不存在: {client_order_id}"))?;
        match order.status {
            qx_core::OrderStatus::Cancelled => {
                return Ok(PortExecutionResult {
                    message: "ALREADY_CANCELLED".into(),
                    event_count: 0,
                });
            }
            qx_core::OrderStatus::Filled
            | qx_core::OrderStatus::Rejected
            | qx_core::OrderStatus::Expired => {
                return Err(format!(
                    "订单 {} 已处于终态 {:?}，不能撤销",
                    client_order_id, order.status
                ));
            }
            qx_core::OrderStatus::PendingSubmit => {
                return Err(format!(
                    "订单 {} 尚未提交到交易所，不能撤销",
                    client_order_id
                ));
            }
            qx_core::OrderStatus::CancelPending => {
                return Err(format!("订单 {} 已有撤单在途，必须先对账", client_order_id));
            }
            qx_core::OrderStatus::Unknown => {
                return Err(format!(
                    "订单 {} 状态未知，必须先对账后才能撤单",
                    client_order_id
                ));
            }
            qx_core::OrderStatus::Submitted
            | qx_core::OrderStatus::Accepted
            | qx_core::OrderStatus::Working
            | qx_core::OrderStatus::PartiallyFilled => {}
        }
        self.venue.execution_readiness()?;
        let operation_id = self.cancel_operation_id.clone().unwrap_or_else(|| {
            format!("{}:cancel:{}:{}", self.worker_id, client_order_id, self.now)
        });
        let append_outcome = self.append_with_outcome(
            ExecutionEvent::CancelPending { client_order_id },
            format!("{operation_id}:pending:{client_order_id}"),
        )?;
        if append_outcome == EventAppendOutcome::Duplicate {
            return Err(format!(
                "订单 {} 的撤单意图已由同一操作持久化，禁止重复调用 Venue；必须先对账",
                client_order_id
            ));
        }
        let facts = match self.venue.cancel_order(client_order_id, self.now) {
            Ok(facts) if facts.is_empty() => {
                self.mark_reconcile_for_operation(
                    client_order_id,
                    "empty-cancel-response",
                    &operation_id,
                )?;
                return Err("Venue 未返回取消事实，结果未知，必须先对账".into());
            }
            Ok(facts) => facts,
            Err(error) => {
                self.mark_reconcile_for_operation(client_order_id, "cancel-error", &operation_id)?;
                return Err(format!(
                    "Venue cancel 失败/结果未知: {error}；订单已标记为待对账"
                ));
            }
        };
        if let Err(error) = validate_cancel_events(client_order_id, &facts) {
            self.mark_reconcile_for_operation(
                client_order_id,
                "invalid-cancel-response",
                &operation_id,
            )?;
            return Err(format!(
                "Venue 返回非法撤单事实，结果未知，必须先对账: {error}"
            ));
        }
        let event_count = facts.len();
        let has_reconcile = facts
            .iter()
            .any(|fact| matches!(fact, ExecutionEvent::ReconcileRequired { .. }));
        for fact in facts {
            let prefix = format!("{operation_id}:response:{client_order_id}");
            let correlation = fact_correlation(&prefix, &fact);
            self.append(fact, correlation)?;
        }
        if has_reconcile {
            return Err("Venue 返回撤单待对账事实，禁止重复撤单".into());
        }
        Ok(PortExecutionResult {
            message: format!("CANCELLED venue_events={event_count}"),
            event_count,
        })
    }

    /// Apply an audited CancelOrder command only after binding its declared routing scope to the
    /// authoritative OMS order. The venue never receives an account or instrument supplied only
    /// by the caller.
    pub fn cancel_command(
        &mut self,
        command: &ControlCommand,
    ) -> Result<PortExecutionResult, String> {
        let target = cancel_order_target_from_command(command)
            .map_err(|error| format!("CancelOrder 命令无效: {error:?}"))?;
        let order = self
            .order(target.client_order_id)
            .ok_or_else(|| format!("订单不存在: {}", target.client_order_id))?;
        if order.account_id != target.account_id
            || !order
                .instrument
                .venue
                .as_str()
                .eq_ignore_ascii_case(&target.venue_id)
            || !self.venue.venue_id().eq_ignore_ascii_case(&target.venue_id)
        {
            return Err("CancelOrder 路由账户/交易所与 OMS 订单或执行 Venue 不一致".into());
        }
        self.cancel_operation_id = Some(format!(
            "control:cancel:{}:{}",
            command.command_id, target.client_order_id
        ));
        let result = self.cancel(target.client_order_id);
        self.cancel_operation_id = None;
        result
    }

    fn mark_reconcile(&mut self, client_order_id: u64, reason: &str) -> Result<(), String> {
        self.append(
            ExecutionEvent::ReconcileRequired { client_order_id },
            format!("{}:{reason}:{client_order_id}", self.worker_id),
        )
    }

    fn mark_reconcile_for_operation(
        &mut self,
        client_order_id: u64,
        reason: &str,
        operation_id: &str,
    ) -> Result<(), String> {
        self.append(
            ExecutionEvent::ReconcileRequired { client_order_id },
            format!("{operation_id}:reconcile:{reason}:{client_order_id}"),
        )
    }

    fn append(&mut self, event: ExecutionEvent, correlation_id: String) -> Result<(), String> {
        self.append_with_outcome(event, correlation_id).map(|_| ())
    }

    fn append_with_outcome(
        &mut self,
        event: ExecutionEvent,
        correlation_id: String,
    ) -> Result<EventAppendOutcome, String> {
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
        self.events
            .append_execution_event_with_outcome(ExecutionEventEnvelope {
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

    /// 补偿提交必须与正常提交带着同一份冻结产品规格：这个 guard 就是运行时按
    /// `worker.instrument_spec_path` 解析出来的那个来源（见 qx-cli 的 `RecoveryGuard`），
    /// 所以补偿腿既不会被按乘数 1 记账，也不会绕过提交侧精度闸门。
    /// 返回 `Ok(None)` 只表示该 worker 没有配置规格（现货 `contract_size=1` 的既有口径），
    /// **解析失败**（文件缺失、币种与账簿不符等）必须返回 `Err`，由 worker 转待对账。
    fn instrument_spec(&self, _order: &Order) -> Result<Option<TradingInstrumentSpec>, String> {
        Ok(None)
    }
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
    account_venue_scope: Option<(Option<String>, Option<String>)>,
}

const MAX_HEDGE_ATTEMPTS: u32 = 3;

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
            account_venue_scope: None,
        })
    }

    /// Scope a live recovery pass to the exact account and venue served by this worker.
    pub fn for_account_venue(mut self, account_id: &str, venue_id: &str) -> Result<Self, String> {
        let account_id = account_id.trim();
        let venue_id = venue_id.trim();
        if account_id.is_empty() || venue_id.is_empty() {
            return Err("实盘多腿恢复必须绑定 account_id 与 venue_id".into());
        }
        self.account_venue_scope = Some((
            Some(account_id.to_string()),
            Some(venue_id.to_ascii_uppercase()),
        ));
        Ok(self)
    }

    pub fn for_account(mut self, account_id: &str) -> Result<Self, String> {
        let account_id = account_id.trim();
        if account_id.is_empty() {
            return Err("实盘多腿恢复必须绑定 account_id".into());
        }
        self.account_venue_scope = Some((Some(account_id.to_string()), None));
        Ok(self)
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
                        left.leg_id != right.leg_id
                            || left.venue_id != right.venue_id
                            || left.order.account_id != right.order.account_id
                            || left.order.instrument != right.order.instrument
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

        let mut targets = self.group.compensation_targets();
        if let Some((account_id, venue_id)) = &self.account_venue_scope {
            targets.retain(|target| {
                account_id
                    .as_deref()
                    .is_none_or(|account_id| target.account_id == account_id)
                    && venue_id
                        .as_deref()
                        .is_none_or(|venue_id| target.venue_id.eq_ignore_ascii_case(venue_id))
            });
        }
        if targets.is_empty() {
            // A crash can happen after the final compensation fill and progress
            // snapshot are durable, but before the group status is changed to
            // Hedged. On restart there are no outstanding targets, so finish
            // that state transition under the same group claim used by normal
            // recovery. An account-scoped worker must still verify the whole
            // group before it can close the group-wide state.
            if self.group.all_confirmed_exposure_hedged() {
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
                if !self.store.verify_claim(
                    &self.group.group_id,
                    &self.claim_owner,
                    claim_token,
                    self.now,
                )? {
                    return Ok(HedgeRecoveryOutcome {
                        group: self.group,
                        attempted: 0,
                        completed: false,
                        errors: vec!["多腿恢复 claim 已过期或已被其他 owner 接管".into()],
                    });
                }
                self.group
                    .mark_hedged()
                    .map_err(|error| format!("标记多腿订单组已对冲失败: {error:?}"))?;
                self.store
                    .save_claimed(&self.group, &self.claim_owner, claim_token, self.now)?;
                self.store
                    .release_claim(&self.group.group_id, &self.claim_owner, claim_token)?;
                return Ok(HedgeRecoveryOutcome {
                    group: self.group,
                    attempted: 0,
                    completed: true,
                    errors: Vec::new(),
                });
            }
            return Ok(HedgeRecoveryOutcome {
                group: self.group,
                attempted: 0,
                completed: false,
                errors: Vec::new(),
            });
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
            let client_id = compensation_client_id(&self.group, &target.leg_id, target.attempt);
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
                    self.group.require_reconciliation();
                    all_filled = false;
                    continue;
                }
                if existing.status == OrderStatus::Filled {
                    if !existing.filled_decimal().is_positive() {
                        errors.push(format!(
                            "补偿订单 {} 报告 Filled 但成交数量为零，必须对账",
                            client_id
                        ));
                        self.group.require_reconciliation();
                        all_filled = false;
                        continue;
                    }
                    self.group
                        .record_compensation_fill_exact(&target.leg_id, existing.filled_decimal())
                        .map_err(|error| format!("恢复补偿成交进度失败: {error:?}"))?;
                    if existing
                        .filled_decimal()
                        .checked_cmp(target.quantity_decimal())
                        .is_some_and(|ordering| ordering.is_lt())
                    {
                        let next_attempt = self
                            .group
                            .advance_compensation_attempt(&target.leg_id)
                            .map_err(|error| format!("推进补偿尝试序号失败: {error:?}"))?;
                        if next_attempt >= MAX_HEDGE_ATTEMPTS {
                            self.group.require_reconciliation();
                            errors.push(format!(
                                "补偿腿 {} 已达到 {} 次尝试上限，剩余敞口转人工对账",
                                target.leg_id, MAX_HEDGE_ATTEMPTS
                            ));
                        }
                    }
                    continue;
                }
                if matches!(
                    existing.status,
                    OrderStatus::PendingSubmit
                        | OrderStatus::Submitted
                        | OrderStatus::Accepted
                        | OrderStatus::Working
                        | OrderStatus::PartiallyFilled
                        | OrderStatus::CancelPending
                ) {
                    all_filled = false;
                    continue;
                }
                if matches!(
                    existing.status,
                    OrderStatus::Cancelled | OrderStatus::Expired
                ) {
                    if existing.filled_decimal().is_positive() {
                        self.group
                            .record_compensation_fill_exact(
                                &target.leg_id,
                                existing.filled_decimal(),
                            )
                            .map_err(|error| format!("归约终态补偿部分成交失败: {error:?}"))?;
                    }
                    let next_attempt = self
                        .group
                        .advance_compensation_attempt(&target.leg_id)
                        .map_err(|error| format!("推进补偿尝试序号失败: {error:?}"))?;
                    if next_attempt >= MAX_HEDGE_ATTEMPTS {
                        self.group.require_reconciliation();
                        errors.push(format!(
                            "补偿腿 {} 已达到 {} 次尝试上限，剩余敞口转人工对账",
                            target.leg_id, MAX_HEDGE_ATTEMPTS
                        ));
                    }
                    errors.push(format!(
                        "补偿订单 {} 已终止于 {:?}，剩余敞口将在下一恢复轮处理",
                        client_id, existing.status
                    ));
                    all_filled = false;
                    continue;
                }
                errors.push(format!(
                    "补偿订单 {} 状态 {:?} 不能安全重试，必须对账",
                    client_id, existing.status
                ));
                self.group.require_reconciliation();
                all_filled = false;
                continue;
            }

            let mut leg_spec: Option<TradingInstrumentSpec> = None;
            if let Some(validator) = validator {
                if let Err(error) = validator.validate(&order) {
                    errors.push(format!(
                        "补偿腿 {} 未通过账户级风控，拒绝自动对冲: {error}",
                        target.leg_id
                    ));
                    all_filled = false;
                    continue;
                }
                // 规格解析必须先于登记与提交：没有冻结规格就既过不了精度闸门，也
                // 会让衍生腿被按乘数 1 记账。解析失败是配置问题而不是未知结果，
                // 所以留组于 HedgeRequired 等下一轮，而不是留下一条假的待对账事实。
                match validator.instrument_spec(&order) {
                    Ok(spec) => leg_spec = spec,
                    Err(error) => {
                        errors.push(format!(
                            "补偿腿 {} 未解析到产品规格，拒绝自动对冲: {error}",
                            target.leg_id
                        ));
                        all_filled = false;
                        continue;
                    }
                }
            }
            let Some(capabilities) = self.router.capabilities_for(&source_leg.venue_id) else {
                errors.push(format!(
                    "补偿腿 {} 的 Venue {} 未声明能力或路由不存在，拒绝注册",
                    target.leg_id, source_leg.venue_id
                ));
                all_filled = false;
                continue;
            };
            if let Err(error) = capabilities.validate_submit(&order, &source_leg.venue_id) {
                errors.push(format!("补偿腿 {} 能力校验失败: {error}", target.leg_id));
                all_filled = false;
                continue;
            }
            if let Err(error) = self.router.execution_readiness_for(&source_leg.venue_id) {
                errors.push(format!("补偿腿 {} Venue 尚未就绪: {error}", target.leg_id));
                all_filled = false;
                continue;
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
            let facts =
                match self
                    .router
                    .submit_order(&source_leg.venue_id, order.clone(), self.now)
                {
                    Ok(facts) if facts.is_empty() => {
                        errors.push(format!(
                            "补偿腿 {} 未返回事实，结果未知，必须对账",
                            target.leg_id
                        ));
                        self.group.require_reconciliation();
                        all_filled = false;
                        continue;
                    }
                    Ok(facts) => facts,
                    Err(error) => {
                        errors.push(format!(
                            "补偿腿 {} 提交失败/结果未知: {error}",
                            target.leg_id
                        ));
                        self.group.require_reconciliation();
                        all_filled = false;
                        continue;
                    }
                };
            // 补偿提交也是提交：回包必须过与 `PortExecutionService::submit` 同一道
            // 闸门。这里绕过闸门，"越界回报只能转待对账"就只剩半条纪律。
            if let Err(violation) = gate_submit_facts(&order, &facts, leg_spec.as_ref()) {
                errors.push(format!("补偿腿 {} {violation}", target.leg_id));
                self.append_fact(
                    ExecutionEvent::ReconcileRequired {
                        client_order_id: client_id,
                    },
                    format!(
                        "{}:hedge:{}:{}:{}",
                        self.worker_id,
                        self.group.group_id,
                        target.leg_id,
                        violation.reason_tag()
                    ),
                    None,
                )?;
                self.group.require_reconciliation();
                all_filled = false;
                continue;
            }
            let hedge_prefix = format!(
                "{}:hedge:{}:{}",
                self.worker_id, self.group.group_id, target.leg_id
            );
            for fact in facts {
                let correlation_id = fact_correlation(&hedge_prefix, &fact);
                self.append_fact(fact, correlation_id, leg_spec.as_ref())?;
            }
            let persisted_hedge = self
                .events
                .orders()
                .into_iter()
                .find(|candidate| candidate.client_id == client_id);
            match persisted_hedge {
                Some(hedge)
                    if hedge.status == OrderStatus::Filled
                        && hedge.filled_decimal().is_positive() =>
                {
                    self.group
                        .record_compensation_fill_exact(&target.leg_id, hedge.filled_decimal())
                        .map_err(|error| format!("归约补偿成交进度失败: {error:?}"))?;
                    if hedge
                        .filled_decimal()
                        .checked_cmp(target.quantity_decimal())
                        .is_some_and(|ordering| ordering.is_lt())
                    {
                        let next_attempt = self
                            .group
                            .advance_compensation_attempt(&target.leg_id)
                            .map_err(|error| format!("推进补偿尝试序号失败: {error:?}"))?;
                        if next_attempt >= MAX_HEDGE_ATTEMPTS {
                            self.group.require_reconciliation();
                            errors.push(format!(
                                "补偿腿 {} 已达到 {} 次尝试上限，剩余敞口转人工对账",
                                target.leg_id, MAX_HEDGE_ATTEMPTS
                            ));
                        }
                    }
                }
                Some(hedge)
                    if matches!(hedge.status, OrderStatus::Cancelled | OrderStatus::Expired) =>
                {
                    if hedge.filled_decimal().is_positive() {
                        self.group
                            .record_compensation_fill_exact(&target.leg_id, hedge.filled_decimal())
                            .map_err(|error| format!("归约终态补偿部分成交失败: {error:?}"))?;
                    }
                    let next_attempt = self
                        .group
                        .advance_compensation_attempt(&target.leg_id)
                        .map_err(|error| format!("推进补偿尝试序号失败: {error:?}"))?;
                    if next_attempt >= MAX_HEDGE_ATTEMPTS {
                        self.group.require_reconciliation();
                        errors.push(format!(
                            "补偿腿 {} 已达到 {} 次尝试上限，剩余敞口转人工对账",
                            target.leg_id, MAX_HEDGE_ATTEMPTS
                        ));
                    }
                    all_filled = false;
                }
                Some(hedge)
                    if matches!(hedge.status, OrderStatus::Rejected | OrderStatus::Unknown) =>
                {
                    self.group.require_reconciliation();
                    errors.push(format!(
                        "补偿订单 {} 进入 {:?}，必须人工对账",
                        client_id, hedge.status
                    ));
                    all_filled = false;
                }
                _ => all_filled = false,
            }
        }

        if all_filled
            && errors.is_empty()
            && self.group.all_confirmed_exposure_hedged()
            && self.group.status == SpreadOrderGroupStatus::HedgeRequired
        {
            self.group
                .mark_hedged()
                .map_err(|error| format!("标记多腿订单组已对冲失败: {error:?}"))?;
        }
        self.store
            .save_claimed(&self.group, &self.claim_owner, claim_token, self.now)?;
        self.store
            .release_claim(&self.group.group_id, &self.claim_owner, claim_token)?;
        let completed =
            self.group.status == SpreadOrderGroupStatus::Hedged && all_filled && errors.is_empty();
        Ok(HedgeRecoveryOutcome {
            group: self.group,
            attempted,
            completed,
            errors,
        })
    }

    /// 追加一条补偿路径事实。`spec` 与 `PortExecutionService::append` 同源：成交必须
    /// 自带它那份冻结规格落库，否则账簿会退回 `LegacyMultiplier(1)`，衍生腿的仓位与
    /// 保证金按乘数 1 计算，对冲方向对、数量却差一个合约面值。
    fn append_fact(
        &mut self,
        event: ExecutionEvent,
        correlation_id: String,
        spec: Option<&TradingInstrumentSpec>,
    ) -> Result<(), String> {
        *self.source_seq = self.source_seq.saturating_add(1);
        let event_ts = match &event {
            ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => fill.ts,
            _ => self.now,
        };
        let event = match (spec, event) {
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

fn build_compensation_order(
    group: &SpreadOrderGroup,
    source_leg: &qx_zhenlu::SpreadOrderLeg,
    target: &qx_zhenlu::SpreadCompensationTarget,
    client_id: u64,
) -> Result<Order, String> {
    let mut policy = source_leg.order.policy.unwrap_or_default();
    policy.reduce_only = true;
    let exact_amounts = target.qty_exact.map(|quantity| qx_core::ExactOrderAmounts {
        quantity,
        limit_price: None,
        filled: qx_core::ExactDecimal::ZERO,
        quote_budget: None,
        quote_filled: qx_core::ExactDecimal::ZERO,
        market_reference_price: source_leg.order.market_reference_price_decimal(),
    });
    let order = Order {
        client_id,
        instrument: target.instrument.clone(),
        side: target.side,
        qty: if exact_amounts.is_some() {
            qx_core::Quantity::ZERO
        } else {
            target.qty
        },
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
        exact_amounts,
    };
    order.validate()?;
    Ok(order)
}

fn same_compensation_order(left: &Order, right: &Order) -> bool {
    left.same_submission_identity(right)
}

fn compensation_client_id(group: &SpreadOrderGroup, leg_id: &str, attempt: u32) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    let attempt_bytes = attempt.to_le_bytes();
    for byte in group
        .group_id
        .as_bytes()
        .iter()
        .chain([0_u8].iter())
        .chain(leg_id.as_bytes().iter())
        .chain([0_u8].iter())
        .chain(attempt_bytes.iter())
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

/// 提交**同步返回**的事实进入账本前的统一闸门：先形状契约，再产品规格精度。
///
/// 两处提交入口（`PortExecutionService::submit` 与对冲补偿 worker）必须共用它。
/// 各写一份就会重演"越界回报只能转待对账"只剩半条纪律：同一笔成交走用户流被
/// `ingest_venue_events_with_pipeline` 拦下、不记账，走提交回包却直接入账。
/// 规格为 `None` 时只过形状闸门 —— 没有冻结规格就没有可核对的 tick/step，
/// 与归约入口一样绝不凭空造一份。
enum SubmitFactViolation {
    /// 事实与订单不匹配（归属、数量、空回报、混入行情）。
    Shape(String),
    /// 成交落在冻结产品规格的 tick/step 之外。
    Precision(qx_core::QxError),
}

impl SubmitFactViolation {
    /// 写进 `ReconcileRequired` 关联号的原因段，保持两类未知结果可分别检索。
    fn reason_tag(&self) -> &'static str {
        match self {
            Self::Shape(_) => "invalid-submit-response",
            Self::Precision(_) => "submit-fill-out-of-spec",
        }
    }
}

impl std::fmt::Display for SubmitFactViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape(error) => {
                write!(f, "Venue 返回非法下单事实，结果未知，必须先对账: {error}")
            }
            Self::Precision(error) => write!(
                f,
                "Venue 提交返回的成交回报违反产品规格，结果未知，必须先对账: {error:?}"
            ),
        }
    }
}

fn gate_submit_facts(
    order: &Order,
    facts: &[ExecutionEvent],
    spec: Option<&TradingInstrumentSpec>,
) -> Result<(), SubmitFactViolation> {
    validate_submit_events(order, facts).map_err(SubmitFactViolation::Shape)?;
    // 规格优先取回报自带的那份：`append` 落库并交给账簿的就是它，校验另一份是假同源。
    let violation = facts.iter().find_map(|fact| {
        let (fill, embedded) = match fact {
            ExecutionEvent::Fill(fill) => (fill.as_ref(), None),
            ExecutionEvent::FillWithSpec { fill, spec } => (fill.as_ref(), Some(&**spec)),
            _ => return None,
        };
        let spec = embedded.or(spec)?;
        validate_fill_against_spec(fill, spec)
    });
    match violation {
        Some(error) => Err(SubmitFactViolation::Precision(error)),
        None => Ok(()),
    }
}

/// 成交事实的稳定身份段。
///
/// 身份必须取自成交内容本身，不能用进程内的 `source_seq`：一次性 Paper 命令
/// 每个进程都从 1 重新计数，同一订单的第二笔成交会撞上第一笔已落库的事实、被
/// EventLog 静默去重，订单停在 Accepted 而资金不动，调用方却收到 `fills=2`。
/// 字段选择与 `qx-runtime` 的 `fill_key` 同源，因此同一批回报重放仍然幂等。
fn fill_tag(fill: &qx_core::Fill) -> String {
    format!(
        "fill:{}:{}:{}:{}:{}:{}{}",
        fill.order_id,
        fill.ts,
        fill.qty.raw(),
        fill.price.raw(),
        fill.fee.raw(),
        fill.venue_order_id.clone().unwrap_or_default(),
        fill.exact_identity_suffix(),
    )
}

fn validate_fill_against_spec(
    fill: &qx_core::Fill,
    spec: &TradingInstrumentSpec,
) -> Option<qx_core::QxError> {
    match fill.exact_amounts {
        Some(amounts) => spec
            .validate_exact_fill(amounts.quantity, amounts.price)
            .err(),
        None => spec.validate_fill(fill.qty.raw(), fill.price.raw()).err(),
    }
}

/// 一条 Venue 回报在其关联号中的稳定身份段。
fn fact_tag(fact: &ExecutionEvent) -> String {
    match fact {
        ExecutionEvent::Accepted {
            client_order_id, ..
        } => format!("accepted:{client_order_id}"),
        ExecutionEvent::CancelPending { client_order_id } => {
            format!("cancel-pending:{client_order_id}")
        }
        ExecutionEvent::Cancelled { client_order_id } => format!("cancelled:{client_order_id}"),
        ExecutionEvent::FeeAdjustment {
            account_id,
            venue_id,
            order_id,
            instrument,
            fees,
        } => {
            let mut fees = fees.clone();
            fees.sort_by(|left, right| {
                left.currency
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.currency.as_deref().unwrap_or_default())
                    .then_with(|| left.amount.raw().cmp(&right.amount.raw()))
            });
            let identity = fees
                .iter()
                .map(|fee| {
                    format!(
                        "{}={}#{}",
                        fee.currency.as_deref().unwrap_or_default(),
                        fee.amount.raw(),
                        {
                            let mut trade_ids = fee.source_trade_ids.clone();
                            trade_ids.sort();
                            trade_ids.join(";")
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("fee-adjustment:{account_id}:{venue_id}:{order_id}:{instrument}:{identity}")
        }
        ExecutionEvent::ReconcileRequired { client_order_id } => {
            format!("reconcile:{client_order_id}")
        }
        ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => {
            fill_tag(fill.as_ref())
        }
        ExecutionEvent::MarketQuote { instrument, quote } => {
            format!("market:{instrument}:{}:{}", quote.source_seq, quote.ts)
        }
    }
}

/// 在调用方的来源前缀（worker/venue/group/leg）后追加回报身份。
///
/// `RuntimePipeline::ingest` 对 Accepted/Cancelled/ReconcileRequired/
/// FillWithSpec 按 correlation 去重——重启后 `source_seq` 会重新计数，只靠它
/// 挡不住重放。因此前缀必须唯一到"这一批回报属于哪条腿"，再叠加事实自身的
/// 身份；否则第二个订单/第二条腿会撞上第一个已落库的事实而被静默丢弃。
fn fact_correlation(prefix: &str, fact: &ExecutionEvent) -> String {
    format!("{prefix}:{}", fact_tag(fact))
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
                // 成交回报的关联号取自成交内容：`source_seq` 是进程内计数，
                // 重启后重新计数会让同一订单的第二笔成交撞上第一笔已落库事实
                // 而被静默去重（订单停在 Accepted、资金不动）。
                let content_correlation = format!("{worker_id}:{}", fill_tag(&fill));
                // 精度闸门：带冻结产品规格的回报若落在 tick/step 之外，就是与规格冲突的
                // 事实，只能转成待对账（不记账、不改状态），并把原因回传给 worker。
                let violation = spec.and_then(|spec| validate_fill_against_spec(&fill, spec));
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
                    (event, event_ts, content_correlation)
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
            VenueEvent::FeeAdjustment {
                account_id,
                venue_id,
                order_id,
                instrument,
                fees,
                ts,
            } => {
                let fact = ExecutionEvent::FeeAdjustment {
                    account_id,
                    venue_id,
                    order_id,
                    instrument,
                    fees,
                };
                let correlation = fact_correlation(worker_id, &fact);
                (fact, ts, correlation)
            }
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

/// 提交一条已通过控制面审计的 SubmitOrder：框架唯一的订单提交入口。
///
/// 这里不负责账户/Venue 拓扑授权，也不负责 ControlPlane 的 Accepted/终态回写；
/// 它只负责订单事实注册、未知结果保护、Venue submit 和标准回报归约。这样
/// Paper、Binance 和未来执行 worker 可以共享同一副作用边界。
///
/// `spread_store` 是多腿订单组快照来源：带 `spread_group_id` 的命令必须由注入了
/// 组存储的提交路径执行，否则 [`spread_group_barrier`] 以 fail-closed 拒绝。
///
/// 账户级风控走 `context`（快照与市场规格），供 Paper/CCXT/Binance worker 统一使用；
/// Venue 仍只接收通过预检的订单。
pub fn submit_order_with_risk<V: Venue, P: ExecutionEventPort>(
    request: SubmitOrderWithRiskRequest<'_, V, P>,
) -> Result<String, String> {
    let SubmitOrderWithRiskRequest {
        command,
        venue,
        pipeline,
        worker_id,
        now,
        source_seq,
        context,
        spread_store,
    } = request;
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

/// 把“结果未知，必须先对账”写成运行时事实的端口适配器（对账判定唯一口径在 qx-genglu 的 `order_reconcile_verdict`，本端口只落事实、不复制判定）。
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

    /// 把「结果未知，必须先对账」落成运行时事实。私有在 trait 里时编译器不会把它当
    /// 公共面，现在 trait 已删（全仓无 `dyn ReconcilePort`、无 `T: ReconcilePort` 约束，
    /// 三个调用点都拿具体类型 `EventLogReconcilePort` 直接调），所以方法必须显式 `pub`。
    pub fn require_reconcile(&mut self, client_order_id: u64, reason: &str) -> Result<(), String> {
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
/// `fee_model` 同理：成本口径由调用方显式给出，不留"默认零费"的口子（V11 §4.1）。
pub fn execute_paper_submit_effect<P: ExecutionEventPort + application::LedgerProbe>(
    request: PaperSubmitRequest<'_, P>,
) -> Result<String, String> {
    let PaperSubmitRequest {
        command,
        pipeline,
        now,
        risk,
        position,
        market_quote,
        fee_model,
        allow_synthetic_quote,
        spread_store,
    } = request;
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
        let bid = Price::from_raw(ask.raw().saturating_sub(qx_core::SCALE));
        QuoteTick::new(
            now.saturating_add(1),
            bid,
            Quantity::from_i64(1_000),
            ask,
            Quantity::from_i64(1_000),
            1,
        )
    });
    validate_paper_quote(&quote)?;
    let mut venue = PaperVenue::new("paper", fee_model);
    if order.exact_amounts.is_some() {
        let spec = risk_context.instrument_spec.clone().ok_or_else(|| {
            "FAIL_CLOSED: Paper exact order 缺少冻结 market spec 和结算币种".to_string()
        })?;
        venue
            .set_instrument_spec(spec)
            .map_err(|error| format!("Paper 冻结 market spec 配置失败: {error:?}"))?;
    }
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
    let fills = venue
        .try_on_quote(&order.instrument, quote)
        .map_err(|error| format!("Paper 精确撮合失败: {error:?}"))?;
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

fn validate_paper_quote(quote: &QuoteTick) -> Result<(), String> {
    let project = |raw: i128, exact: Option<ExactDecimal>, label: &str| match exact {
        Some(value) => {
            if !value.is_positive() {
                return Err(format!("Paper {label} 精确值必须为正数"));
            }
            let projected = value
                .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                .and_then(|value| value.to_raw(9).ok());
            if projected != Some(raw) {
                return Err(format!("Paper {label} 精确值与兼容投影不一致"));
            }
            Ok(value)
        }
        None if raw > 0 => ExactDecimal::from_parts(raw, 9)
            .map_err(|error| format!("Paper {label} 兼容值非法: {error}")),
        None => Err(format!("Paper {label} 缺少正数行情")),
    };
    let bid = project(quote.bid.raw(), quote.bid_exact, "bid")?;
    let ask = project(quote.ask.raw(), quote.ask_exact, "ask")?;
    if bid.checked_cmp(ask).is_none_or(|ordering| ordering.is_gt()) {
        return Err("Paper 行情买卖价交叉或无法比较".into());
    }
    project(quote.bid_qty.raw(), quote.bid_qty_exact, "bid quantity")?;
    project(quote.ask_qty.raw(), quote.ask_qty_exact, "ask quantity")?;
    Ok(())
}

#[cfg(test)]
mod tests;
