//! 统一执行层边界。
//!
//! Venue 只返回 `VenueEvent`，执行层负责把这些事件转换成运行时标准事实，
//! 再交给 `LiveEventPipeline` 归约到 EventLog、订单状态和 Ledger。Paper、
//! Binance 以及未来连接器都必须复用这里的事实转换，不能在 CLI 中各写一套。

use qx_application::{
    validate_cancel_events, validate_submit_events, ExecutionEvent, ExecutionEventEnvelope,
    ExecutionEventPort, RiskDecision, RiskPort, VenuePort, VenueRouterPort,
};
use qx_control::{order_from_submit_command, ControlCommand};
use qx_core::{Order, OrderStatus, OrderTrace, Price, Quantity, TradingInstrumentSpec, SCALE};
use qx_guanxing::QuoteTick;
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
use qx_zhenlu::{
    PaperVenue, PositionSnapshot, RiskContext, SpreadOrderGroup, SpreadOrderGroupStatus,
    SpreadOrderGroupStore, Venue, VenueEvent,
};
use std::collections::BTreeMap;

/// Venue 无关的执行编排服务。
///
/// 该服务持有一次执行所需的运行时上下文，但不持有 ControlPlane、策略或
/// 具体交易所类型。调用方负责在服务外完成权限、路由和命令 Accepted 审计，
/// 服务只保证“注册订单 → 调 Venue → 归约标准回报”的副作用边界一致。
pub struct ExecutionService<'a, V: Venue, P: ExecutionEventPort> {
    venue: &'a mut V,
    pipeline: &'a mut P,
    worker_id: &'a str,
    now: u64,
    source_seq: &'a mut u64,
    instrument_spec: Option<TradingInstrumentSpec>,
}

pub struct RiskExecutionContext<'a> {
    pub risk: &'a RiskContext,
    pub position: &'a PositionSnapshot,
}

/// 把当前账户快照适配为应用层 RiskPort，便于回测、Paper 和实盘执行器
/// 替换风控实现而不改动 ExecutionService 的订单副作用边界。
pub struct RiskContextPort<'a> {
    pub risk: &'a RiskContext,
    pub position: &'a PositionSnapshot,
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
/// EventLog/OMS；真正的副作用仍由 `ExecutionService` 统一编排。
pub struct VenuePortAdapter<V: Venue> {
    venue: V,
    accept_any_venue: bool,
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

/// 单进程多 Venue 路由表。Venue 适配器仍由调用方创建并注入，路由表只负责
/// 以稳定的 `venue_id` 选择端口，不包含交易所签名、重试或订单状态逻辑。
pub struct VenueRouterMap<'a> {
    venues: BTreeMap<String, Box<dyn VenuePort + 'a>>,
}

impl<'a> Default for VenueRouterMap<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> VenueRouterMap<'a> {
    pub fn new() -> Self {
        Self {
            venues: BTreeMap::new(),
        }
    }

    pub fn insert<V: VenuePort + 'a>(
        &mut self,
        venue_id: impl Into<String>,
        venue: V,
    ) -> Result<(), String> {
        let venue_id = venue_id.into();
        if venue_id.trim().is_empty() || venue.venue_id() != venue_id {
            return Err(format!(
                "VenueRouterMap id 不匹配: key={venue_id} actual={}",
                venue.venue_id()
            ));
        }
        if self.venues.contains_key(&venue_id) {
            return Err(format!("VenueRouterMap 重复注册 Venue: {venue_id}"));
        }
        self.venues.insert(venue_id, Box::new(venue));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.venues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.venues.is_empty()
    }
}

impl VenueRouterPort for VenueRouterMap<'_> {
    fn submit_order(
        &mut self,
        venue_id: &str,
        order: qx_core::Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.venues
            .get_mut(venue_id)
            .ok_or_else(|| format!("未注册 Venue: {venue_id}"))?
            .submit_order(order, ts)
    }

    fn cancel_order(
        &mut self,
        venue_id: &str,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        self.venues
            .get_mut(venue_id)
            .ok_or_else(|| format!("未注册 Venue: {venue_id}"))?
            .cancel_order(client_order_id, ts)
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
        }
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
                Some(format!("{}:submit:{}", self.worker_id, order.client_id)),
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
        for fact in facts {
            self.append(
                fact,
                format!("{}:venue:{}", self.worker_id, self.venue.venue_id()),
            )?;
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
        self.events.append_execution_event(ExecutionEventEnvelope {
            event,
            event_ts,
            receive_ts: self.now,
            source_seq: *self.source_seq,
            correlation_id,
        })
    }
}

impl<'a, V: Venue, P: ExecutionEventPort> ExecutionService<'a, V, P> {
    pub fn new(
        venue: &'a mut V,
        pipeline: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> Self {
        Self {
            venue,
            pipeline,
            worker_id,
            now,
            source_seq,
            instrument_spec: None,
        }
    }

    pub fn new_with_spec(
        venue: &'a mut V,
        pipeline: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
        instrument_spec: TradingInstrumentSpec,
    ) -> Self {
        Self {
            venue,
            pipeline,
            worker_id,
            now,
            source_seq,
            instrument_spec: Some(instrument_spec),
        }
    }

    pub fn execute(&mut self, command: &ControlCommand) -> Result<String, String> {
        let requested_order = order_from_submit_command(command)
            .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
        self.execute_order_inner(&requested_order, Some(command))
            .map(|(result, _)| result)
    }

    /// 执行已经通过控制面校验的领域订单，并返回供应商事实副本。
    ///
    /// 该入口供多腿编排使用：标准单腿路径仍由 `execute` 调用它，
    /// SpreadOrderGroup 可以在同一事实批次下更新每条腿状态和补偿计划。
    pub fn execute_order(
        &mut self,
        requested_order: &qx_core::Order,
    ) -> Result<(String, Vec<VenueEvent>), String> {
        self.execute_order_inner(requested_order, None)
    }

    fn execute_order_inner(
        &mut self,
        requested_order: &qx_core::Order,
        control_command: Option<&ControlCommand>,
    ) -> Result<(String, Vec<VenueEvent>), String> {
        if let Some(existing) = self
            .pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == requested_order.client_id)
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
                return Ok((
                    format!(
                        "ALREADY_APPLIED_FROM_EVENT_LOG status={:?}",
                        existing.status
                    ),
                    Vec::new(),
                ));
            }
            if matches!(
                existing.status,
                qx_core::OrderStatus::Submitted | qx_core::OrderStatus::Unknown
            ) {
                *self.source_seq = self.source_seq.saturating_add(1);
                self.pipeline
                    .append_execution_event(ExecutionEventEnvelope {
                        event: ExecutionEvent::ReconcileRequired {
                            client_order_id: existing.client_id,
                        },
                        event_ts: self.now,
                        receive_ts: self.now,
                        source_seq: *self.source_seq,
                        correlation_id: format!(
                            "{}:ambiguous-submit:{}",
                            self.worker_id, existing.client_id
                        ),
                    })
                    .map_err(|error| format!("标记未知下单结果失败: {error}"))?;
                return Err(format!(
                    "订单 {} 已离开本地但未有 Accepted 事实，必须先对账，禁止自动补单",
                    existing.client_id
                ));
            }
        } else {
            if let Some(command) = control_command {
                self.pipeline
                    .register_order(
                        requested_order.clone(),
                        self.now,
                        Some(format!("control:{}", command.command_id)),
                    )
                    .map_err(|error| format!("写入 OrderSubmitted 失败: {error}"))?;
            } else {
                self.pipeline
                    .register_order(requested_order.clone(), self.now, None)
                    .map_err(|error| format!("写入 OrderSubmitted 失败: {error}"))?;
            }
        }
        let order = self
            .pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == requested_order.client_id)
            .ok_or_else(|| "OrderSubmitted 后找不到订单".to_string())?;
        let events = match self.venue.submit(order.clone(), self.now) {
            Ok(events) => events,
            Err(error) => {
                // Once the order has been persisted, a Venue error cannot prove
                // that no remote order exists. Fail closed by moving the local
                // order into reconcile-required state before returning.
                *self.source_seq = self.source_seq.saturating_add(1);
                let reconcile = self
                    .pipeline
                    .append_execution_event(ExecutionEventEnvelope {
                        event: ExecutionEvent::ReconcileRequired {
                            client_order_id: requested_order.client_id,
                        },
                        event_ts: self.now,
                        receive_ts: self.now,
                        source_seq: *self.source_seq,
                        correlation_id: format!(
                            "{}:submit-error:{}",
                            self.worker_id, requested_order.client_id
                        ),
                    });
                return match reconcile {
                    Ok(_) => Err(format!(
                        "Venue submit 失败/结果未知: {error:?}；订单已标记为待对账"
                    )),
                    Err(reconcile_error) => Err(format!(
                        "Venue submit 失败/结果未知: {error:?}；标记待对账也失败: {reconcile_error}"
                    )),
                };
            }
        };
        let application_events = venue_events_to_application(events.clone());
        if let Err(error) = validate_submit_events(&order, &application_events) {
            *self.source_seq = self.source_seq.saturating_add(1);
            let reconcile = self
                .pipeline
                .append_execution_event(ExecutionEventEnvelope {
                    event: ExecutionEvent::ReconcileRequired {
                        client_order_id: order.client_id,
                    },
                    event_ts: self.now,
                    receive_ts: self.now,
                    source_seq: *self.source_seq,
                    correlation_id: format!(
                        "{}:invalid-submit-response:{}",
                        self.worker_id, order.client_id
                    ),
                });
            return match reconcile {
                Ok(_) => Err(format!(
                    "Venue 返回非法下单事实，结果未知，必须先对账: {error}"
                )),
                Err(reconcile_error) => Err(format!(
                    "Venue 返回非法下单事实: {error}；标记待对账也失败: {reconcile_error}"
                )),
            };
        }
        let count = if let Some(spec) = self.instrument_spec.as_ref() {
            ingest_venue_events_with_spec_port(
                self.pipeline,
                events.clone(),
                self.worker_id,
                self.now,
                self.source_seq,
                spec,
            )?
        } else {
            ingest_venue_events_port(
                self.pipeline,
                events.clone(),
                self.worker_id,
                self.now,
                self.source_seq,
            )?
        };
        Ok((format!("SUBMITTED venue_events={count}"), events))
    }

    /// 带账户级 RiskContext 的执行入口。风控失败发生在写入 OrderSubmitted 和
    /// 调用 Venue 之前；调用方必须传入同一时点的账户快照、产品规格和市场参考价。
    pub fn execute_with_risk(
        &mut self,
        command: &ControlCommand,
        risk: &RiskContext,
        position: &PositionSnapshot,
    ) -> Result<String, String> {
        self.execute_with_risk_port(command, &RiskContextPort { risk, position })
    }

    /// 面向应用层 RiskPort 的统一执行入口。风控拒绝发生在 OrderSubmitted
    /// 和 Venue submit 之前；通过 `control:<command_id>` 保留原有审计关联。
    pub fn execute_with_risk_port<R: RiskPort>(
        &mut self,
        command: &ControlCommand,
        risk: &R,
    ) -> Result<String, String> {
        let order = order_from_submit_command(command)
            .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
        let decision = risk
            .evaluate_order(&order)
            .map_err(|error| format!("账户级 RiskPort 执行失败: {error}"))?;
        if !decision.accepted {
            return Err(format!(
                "账户级 RiskPort 拒绝订单: {}",
                decision.reason_code
            ));
        }
        self.execute_order_inner(&order, Some(command))
            .map(|(result, _)| result)
    }
}

#[derive(Clone, Debug)]
pub struct SpreadExecutionOutcome {
    pub group: SpreadOrderGroup,
    pub errors: Vec<String>,
}

impl SpreadExecutionOutcome {
    pub fn is_complete(&self) -> bool {
        self.errors.is_empty() && self.group.status == SpreadOrderGroupStatus::Filled
    }

    pub fn compensation_targets(&self) -> Vec<qx_zhenlu::SpreadCompensationTarget> {
        self.group.compensation_targets()
    }
}

/// 有序多腿执行编排。
///
/// 它不宣称跨 Venue 原子提交：每条腿仍由注入的 Venue 顺序提交；一旦出现
/// 未知结果、部分成交或后续腿失败，结果会保留 `SpreadOrderGroup` 状态和反向
/// 补偿目标，调用方必须走 reduce-only/人工对账恢复。该边界把策略信号升级为
/// 可重放的执行状态，而不是把两条独立 `OrderIntent` 当成套利完成。
pub struct SpreadExecutionService<'a, V: Venue, P: ExecutionEventPort> {
    execution: ExecutionService<'a, V, P>,
    group: SpreadOrderGroup,
    state_store: Option<&'a mut dyn SpreadOrderGroupStore>,
}

impl<'a, V: Venue, P: ExecutionEventPort> SpreadExecutionService<'a, V, P> {
    pub fn new(
        group: SpreadOrderGroup,
        venue: &'a mut V,
        pipeline: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> qx_core::QxResult<Self> {
        group.validate()?;
        Ok(Self {
            execution: ExecutionService::new(venue, pipeline, worker_id, now, source_seq),
            group,
            state_store: None,
        })
    }

    /// 带恢复快照的多腿执行入口。若状态文件中已存在同一 group_id，
    /// 将以持久化生命周期继续执行，避免重启后重新提交已经确认的腿。
    pub fn new_with_store(
        group: SpreadOrderGroup,
        store: &'a mut dyn SpreadOrderGroupStore,
        venue: &'a mut V,
        pipeline: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> qx_core::QxResult<Self> {
        group.validate()?;
        Ok(Self {
            execution: ExecutionService::new(venue, pipeline, worker_id, now, source_seq),
            group,
            state_store: Some(store),
        })
    }

    fn persist_group(&mut self) -> Result<(), String> {
        if let Some(store) = self.state_store.as_deref_mut() {
            store.save(&self.group)?;
        }
        Ok(())
    }

    pub fn execute(mut self) -> Result<SpreadExecutionOutcome, String> {
        if let Some(store) = self.state_store.as_deref_mut() {
            if let Some(saved) = store.load(&self.group.group_id)? {
                if saved.strategy_id != self.group.strategy_id
                    || saved.legs.len() != self.group.legs.len()
                    || saved
                        .legs
                        .iter()
                        .zip(&self.group.legs)
                        .any(|(left, right)| left.leg_id != right.leg_id)
                {
                    return Err("多腿恢复快照与请求的策略或腿结构不一致".into());
                }
                self.group = saved;
            }
        }
        match self.group.status {
            SpreadOrderGroupStatus::Planned => {
                self.group
                    .begin_submission()
                    .map_err(|error| format!("多腿订单组开始提交失败: {error:?}"))?;
                self.persist_group()?;
            }
            SpreadOrderGroupStatus::Submitting
            | SpreadOrderGroupStatus::PartiallyFilled
            | SpreadOrderGroupStatus::HedgeRequired
            | SpreadOrderGroupStatus::ReconcileRequired
            | SpreadOrderGroupStatus::Hedged => {}
            status => {
                return Ok(SpreadExecutionOutcome {
                    group: self.group,
                    errors: vec![format!("多腿订单组已处于终态 {status:?}，跳过重复提交")],
                })
            }
        }
        let leg_ids = self
            .group
            .legs
            .iter()
            .map(|leg| leg.leg_id.clone())
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for leg_id in leg_ids {
            let order = self
                .group
                .leg(&leg_id)
                .map_err(|error| format!("读取多腿订单失败: {error:?}"))?
                .order
                .clone();
            match self.execution.execute_order(&order) {
                Ok((result, events)) if events.is_empty() => {
                    let error = format!("腿 {leg_id} 未产生新事实: {result}");
                    let _ = self.group.record_unknown(&leg_id);
                    self.persist_group()?;
                    errors.push(error);
                    break;
                }
                Ok((_result, events)) => {
                    if let Err(error) = apply_spread_venue_events(&mut self.group, &leg_id, events)
                    {
                        errors.push(format!("腿 {leg_id} 回报归约失败: {error:?}"));
                        let _ = self.group.record_unknown(&leg_id);
                        self.persist_group()?;
                        break;
                    }
                    self.persist_group()?;
                }
                Err(error) => {
                    let _ = self.group.record_unknown(&leg_id);
                    self.persist_group()?;
                    errors.push(format!("腿 {leg_id} 提交失败: {error}"));
                    break;
                }
            }
        }
        Ok(SpreadExecutionOutcome {
            group: self.group,
            errors,
        })
    }
}

/// 多 Venue 多腿执行器。
///
/// 每条腿通过 `VenueRouterPort` 选择交易所，事件先写入统一
/// `ExecutionEventPort`，再归约到 `SpreadOrderGroup`。这解决了“策略支持多交易所
/// 但执行器只能持有一个 Venue”的结构性缺口；跨 Venue 仍然不是原子事务，任一
/// 腿出现空回报、异常或未知结果都会停止后续提交并生成补偿/对账状态。
pub struct MultiVenueSpreadExecutionService<'a, R: VenueRouterPort, P: ExecutionEventPort> {
    router: &'a mut R,
    events: &'a mut P,
    group: SpreadOrderGroup,
    state_store: Option<&'a mut dyn SpreadOrderGroupStore>,
    worker_id: &'a str,
    now: u64,
    source_seq: &'a mut u64,
}

impl<'a, R: VenueRouterPort, P: ExecutionEventPort> MultiVenueSpreadExecutionService<'a, R, P> {
    pub fn new(
        group: SpreadOrderGroup,
        router: &'a mut R,
        events: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> qx_core::QxResult<Self> {
        group.validate()?;
        Self::validate_routes(&group)?;
        Ok(Self {
            router,
            events,
            group,
            state_store: None,
            worker_id,
            now,
            source_seq,
        })
    }

    pub fn new_with_store(
        group: SpreadOrderGroup,
        store: &'a mut dyn SpreadOrderGroupStore,
        router: &'a mut R,
        events: &'a mut P,
        worker_id: &'a str,
        now: u64,
        source_seq: &'a mut u64,
    ) -> qx_core::QxResult<Self> {
        group.validate()?;
        Self::validate_routes(&group)?;
        Ok(Self {
            router,
            events,
            group,
            state_store: Some(store),
            worker_id,
            now,
            source_seq,
        })
    }

    fn validate_routes(group: &SpreadOrderGroup) -> qx_core::QxResult<()> {
        if group.legs.iter().any(|leg| leg.venue_id.trim().is_empty()) {
            return Err(qx_core::QxError::BusinessViolation(
                "多 Venue SpreadOrderGroup 每条腿必须配置 venue_id".into(),
            ));
        }
        Ok(())
    }

    fn persist_group(&mut self) -> Result<(), String> {
        if let Some(store) = self.state_store.as_deref_mut() {
            store.save(&self.group)?;
        }
        Ok(())
    }

    pub fn execute(mut self) -> Result<SpreadExecutionOutcome, String> {
        if let Some(store) = self.state_store.as_deref_mut() {
            if let Some(saved) = store.load(&self.group.group_id)? {
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
                    return Err("多 Venue 恢复快照与请求的策略、腿或 Venue 路由不一致".into());
                }
                self.group = saved;
            }
        }
        match self.group.status {
            SpreadOrderGroupStatus::Planned => {
                self.group
                    .begin_submission()
                    .map_err(|error| format!("多 Venue 订单组开始提交失败: {error:?}"))?;
                self.persist_group()?;
            }
            SpreadOrderGroupStatus::Submitting
            | SpreadOrderGroupStatus::PartiallyFilled
            | SpreadOrderGroupStatus::HedgeRequired
            | SpreadOrderGroupStatus::ReconcileRequired
            | SpreadOrderGroupStatus::Hedged => {}
            status => {
                return Ok(SpreadExecutionOutcome {
                    group: self.group,
                    errors: vec![format!(
                        "多 Venue 订单组已处于终态 {status:?}，跳过重复提交"
                    )],
                })
            }
        }

        let leg_ids = self
            .group
            .legs
            .iter()
            .map(|leg| leg.leg_id.clone())
            .collect::<Vec<_>>();
        let mut errors = Vec::new();
        for leg_id in leg_ids {
            let leg = self
                .group
                .leg(&leg_id)
                .map_err(|error| format!("读取多 Venue 订单腿失败: {error:?}"))?;
            if matches!(
                leg.order.status,
                qx_core::OrderStatus::Accepted
                    | qx_core::OrderStatus::Working
                    | qx_core::OrderStatus::PartiallyFilled
                    | qx_core::OrderStatus::Filled
                    | qx_core::OrderStatus::Cancelled
                    | qx_core::OrderStatus::Rejected
                    | qx_core::OrderStatus::Expired
            ) {
                continue;
            }
            if matches!(
                leg.order.status,
                qx_core::OrderStatus::Submitted | qx_core::OrderStatus::Unknown
            ) {
                let _ = self.group.record_unknown(&leg_id);
                self.persist_group()?;
                errors.push(format!(
                    "腿 {leg_id} 已离开本地但状态不明确，禁止跨 Venue 自动补单"
                ));
                break;
            }
            let venue_id = leg.venue_id.clone();
            let order = leg.order.clone();
            if !self
                .events
                .orders()
                .into_iter()
                .any(|existing| existing.client_id == order.client_id)
            {
                self.events
                    .register_order(
                        order.clone(),
                        self.now,
                        Some(format!(
                            "{}:spread:{}:{}:register",
                            self.worker_id, self.group.group_id, leg_id
                        )),
                    )
                    .map_err(|error| format!("注册多 Venue 订单腿失败: {error}"))?;
            }
            let facts = match self.router.submit_order(&venue_id, order, self.now) {
                Ok(facts) if facts.is_empty() => {
                    let _ = self.group.record_unknown(&leg_id);
                    self.persist_group()?;
                    errors.push(format!("腿 {leg_id} Venue={venue_id} 未返回事实，必须对账"));
                    break;
                }
                Ok(facts) => facts,
                Err(error) => {
                    let _ = self.group.record_unknown(&leg_id);
                    self.persist_group()?;
                    errors.push(format!(
                        "腿 {leg_id} Venue={venue_id} 提交失败/结果未知: {error}"
                    ));
                    break;
                }
            };
            let mut route_error = None;
            for fact in facts {
                let event = fact.clone();
                if let Err(error) = self.append_fact(
                    fact,
                    format!(
                        "{}:spread:{}:{}",
                        self.worker_id, self.group.group_id, leg_id
                    ),
                ) {
                    route_error = Some(error);
                    break;
                }
                if let Err(error) = apply_spread_application_event(&mut self.group, &leg_id, event)
                {
                    route_error = Some(format!("腿 {leg_id} 回报归约失败: {error:?}"));
                    break;
                }
            }
            if let Some(error) = route_error {
                let _ = self.group.record_unknown(&leg_id);
                self.persist_group()?;
                errors.push(error);
                break;
            }
            self.persist_group()?;
            if matches!(
                self.group.status,
                SpreadOrderGroupStatus::HedgeRequired | SpreadOrderGroupStatus::ReconcileRequired
            ) {
                break;
            }
        }
        Ok(SpreadExecutionOutcome {
            group: self.group,
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

fn apply_spread_application_event(
    group: &mut SpreadOrderGroup,
    leg_id: &str,
    event: ExecutionEvent,
) -> qx_core::QxResult<()> {
    match event {
        ExecutionEvent::Accepted { .. } => group.record_accepted(leg_id),
        ExecutionEvent::Fill(fill) => group.record_fill(leg_id, &fill),
        ExecutionEvent::FillWithSpec { fill, .. } => group.record_fill(leg_id, &fill),
        ExecutionEvent::Cancelled { .. } => group.record_cancelled(leg_id),
        ExecutionEvent::ReconcileRequired { .. } => group.record_unknown(leg_id),
    }
}

fn apply_spread_venue_events(
    group: &mut SpreadOrderGroup,
    leg_id: &str,
    events: Vec<VenueEvent>,
) -> qx_core::QxResult<()> {
    for event in events {
        match event {
            VenueEvent::Accepted { .. } => group.record_accepted(leg_id)?,
            VenueEvent::Fill(fill) => group.record_fill(leg_id, &fill)?,
            VenueEvent::Cancelled { .. } => group.record_cancelled(leg_id)?,
        }
    }
    Ok(())
}

/// 将 Venue 返回的订单事实统一归约到 Runtime/EventLog/Ledger 管线。
///
/// `source_seq` 由调用方持有；重复回报仍由 LiveEventPipeline 去重。
fn ingest_venue_events_port<P: ExecutionEventPort>(
    pipeline: &mut P,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    ingest_venue_events_port_with_spec(pipeline, events, worker_id, receive_ts, source_seq, None)
}

fn ingest_venue_events_with_spec_port<P: ExecutionEventPort>(
    pipeline: &mut P,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
    spec: &TradingInstrumentSpec,
) -> Result<usize, String> {
    ingest_venue_events_port_with_spec(
        pipeline,
        events,
        worker_id,
        receive_ts,
        source_seq,
        Some(spec),
    )
}

fn ingest_venue_events_port_with_spec<P: ExecutionEventPort>(
    pipeline: &mut P,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
    spec: Option<&TradingInstrumentSpec>,
) -> Result<usize, String> {
    let mut event_count = 0_usize;
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
                let event = match spec {
                    Some(spec) => ExecutionEvent::FillWithSpec {
                        fill: Box::new(fill),
                        spec: Box::new(spec.clone()),
                    },
                    None => ExecutionEvent::Fill(Box::new(fill)),
                };
                (event, event_ts, format!("{worker_id}:fill:{}", *source_seq))
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
    Ok(event_count)
}

pub fn ingest_venue_events(
    pipeline: &mut LiveEventPipeline,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    let mut event_count = 0_usize;
    for event in events {
        event_count += 1;
        *source_seq = source_seq.saturating_add(1);
        let (event, event_ts, correlation_id) = match event {
            VenueEvent::Accepted {
                client_order_id,
                venue_order_id,
                ts,
            } => (
                RuntimeExternalEvent::Accepted {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                },
                ts,
                format!("{worker_id}:accepted:{client_order_id}"),
            ),
            VenueEvent::Fill(fill) => {
                let event_ts = fill.ts;
                (
                    RuntimeExternalEvent::Fill { fill },
                    event_ts,
                    format!("{worker_id}:fill:{}", *source_seq),
                )
            }
            VenueEvent::Cancelled {
                client_order_id,
                ts,
            } => (
                RuntimeExternalEvent::Cancelled { client_order_id },
                ts,
                format!("{worker_id}:cancelled:{client_order_id}"),
            ),
        };
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                event,
                event_ts,
                receive_ts,
                *source_seq,
                correlation_id,
            ))
            .map_err(|error| format!("Venue 执行回报事实归约失败: {error:?}"))?;
    }
    Ok(event_count)
}

/// 将含有冻结产品规格的 Venue 成交归约为衍生品记账事实。
///
/// 规格不会被写进 `Filled` 事件本身；完整的衍生品 LedgerEntry 会随同一批
/// `LedgerApplied` 事实写入 EventLog，因此重启后的 ReplayVerifier 仍可准确重建。
pub fn ingest_venue_events_with_spec(
    pipeline: &mut LiveEventPipeline,
    events: impl IntoIterator<Item = VenueEvent>,
    worker_id: &str,
    receive_ts: u64,
    source_seq: &mut u64,
    spec: &TradingInstrumentSpec,
) -> Result<usize, String> {
    let mut event_count = 0_usize;
    for event in events {
        event_count += 1;
        *source_seq = source_seq.saturating_add(1);
        let (event, event_ts, correlation_id) = match event {
            VenueEvent::Accepted {
                client_order_id,
                venue_order_id,
                ts,
            } => (
                RuntimeExternalEvent::Accepted {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                },
                ts,
                format!("{worker_id}:accepted:{client_order_id}"),
            ),
            VenueEvent::Fill(fill) => {
                let event_ts = fill.ts;
                (
                    RuntimeExternalEvent::FillWithSpec {
                        fill: Box::new(fill),
                        spec: Box::new(spec.clone()),
                    },
                    event_ts,
                    format!("{worker_id}:fill:{}", *source_seq),
                )
            }
            VenueEvent::Cancelled {
                client_order_id,
                ts,
            } => (
                RuntimeExternalEvent::Cancelled { client_order_id },
                ts,
                format!("{worker_id}:cancelled:{client_order_id}"),
            ),
        };
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                event,
                event_ts,
                receive_ts,
                *source_seq,
                correlation_id,
            ))
            .map_err(|error| format!("带产品规格的 Venue 成交事实归约失败: {error:?}"))?;
    }
    Ok(event_count)
}

/// 执行一条已经通过控制面审计的 SubmitOrder。
///
/// 这里不负责账户/Venue 拓扑授权，也不负责 ControlPlane 的 Accepted/终态回写；
/// 它只负责订单事实注册、未知结果保护、Venue submit 和标准回报归约。这样
/// Paper、Binance 和未来执行 worker 可以共享同一副作用边界。
pub fn submit_order<V: Venue>(
    command: &ControlCommand,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    now: u64,
    source_seq: &mut u64,
) -> Result<String, String> {
    ExecutionService::new(venue, pipeline, worker_id, now, source_seq).execute(command)
}

/// `submit_order` 的账户级风控版本，供 Paper/CCXT/Binance worker 在已有账户
/// 快照和市场规格时统一使用；Venue 仍只接收通过预检的订单。
pub fn submit_order_with_risk<V: Venue>(
    command: &ControlCommand,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    worker_id: &str,
    now: u64,
    source_seq: &mut u64,
    context: &RiskExecutionContext<'_>,
) -> Result<String, String> {
    if let Some(spec) = context.risk.instrument_spec.clone() {
        ExecutionService::new_with_spec(venue, pipeline, worker_id, now, source_seq, spec)
            .execute_with_risk(command, context.risk, context.position)
    } else {
        ExecutionService::new(venue, pipeline, worker_id, now, source_seq).execute_with_risk(
            command,
            context.risk,
            context.position,
        )
    }
}

/// 完全本地的 Paper SubmitOrder 副作用：提交、行情、成交和 Ledger 归约共用
/// 与真实执行相同的 EventLog 事实边界。
pub fn execute_paper_submit_effect(
    command: &ControlCommand,
    root: &std::path::Path,
    log_name: &str,
    now: u64,
) -> Result<String, String> {
    execute_paper_submit_effect_with_risk(command, root, log_name, now, None, None)
}

/// Paper 执行的账户级风控版本。风险快照由调用方从同一 EventLog/行情快照
/// 构造并以值传入，避免 Paper 路径因为内部重新打开管线而退化成绕过账户预检。
pub fn execute_paper_submit_effect_with_risk(
    command: &ControlCommand,
    root: &std::path::Path,
    log_name: &str,
    now: u64,
    risk: Option<RiskContext>,
    position: Option<PositionSnapshot>,
) -> Result<String, String> {
    execute_paper_submit_effect_with_storage(command, root, log_name, now, None, risk, position)
}

/// Paper 执行的可配置存储版本。`segment_events` 与运行时配置保持一致，
/// 使 Paper 的提交、行情和成交归约不会在分段/单文件两种 EventLog 后端间
/// 意外分叉。
pub fn execute_paper_submit_effect_with_storage(
    command: &ControlCommand,
    root: &std::path::Path,
    log_name: &str,
    now: u64,
    segment_events: Option<usize>,
    risk: Option<RiskContext>,
    position: Option<PositionSnapshot>,
) -> Result<String, String> {
    execute_paper_submit_effect_with_storage_backend(
        command,
        root,
        log_name,
        now,
        segment_events,
        None,
        risk,
        position,
    )
}

/// Paper 执行的统一存储版本。`postgres_dsn` 非空时，订单事实、成交、行情和
/// Ledger 归约全部进入 PostgreSQL EventLog；为空时保持文件/分段后端兼容。
#[allow(clippy::too_many_arguments)]
pub fn execute_paper_submit_effect_with_storage_backend(
    command: &ControlCommand,
    root: &std::path::Path,
    log_name: &str,
    now: u64,
    segment_events: Option<usize>,
    postgres_dsn: Option<&str>,
    risk: Option<RiskContext>,
    position: Option<PositionSnapshot>,
) -> Result<String, String> {
    execute_paper_submit_effect_with_storage_backend_and_pool(
        command,
        root,
        log_name,
        now,
        segment_events,
        postgres_dsn,
        1,
        risk,
        position,
    )
}

/// Paper 执行的统一存储版本，并允许 PostgreSQL EventLog 使用运行时连接池。
#[allow(clippy::too_many_arguments)]
pub fn execute_paper_submit_effect_with_storage_backend_and_pool(
    command: &ControlCommand,
    root: &std::path::Path,
    log_name: &str,
    now: u64,
    segment_events: Option<usize>,
    postgres_dsn: Option<&str>,
    postgres_pool_size: usize,
    risk: Option<RiskContext>,
    position: Option<PositionSnapshot>,
) -> Result<String, String> {
    execute_paper_submit_effect_with_storage_backend_and_pool_with_quote(
        command,
        root,
        log_name,
        now,
        segment_events,
        postgres_dsn,
        postgres_pool_size,
        risk,
        position,
        None,
    )
}

/// Paper 执行的市场驱动版本。`quote` 必须来自同一 EventLog/行情快照；生产
/// Paper worker 不传 `None`，没有最近行情时拒绝成交，避免固定测试价格伪装成
/// 可交易市场。`None` 仅保留给离线兼容 smoke fixture。
#[allow(clippy::too_many_arguments)]
pub fn execute_paper_submit_effect_with_storage_backend_and_pool_with_quote(
    command: &ControlCommand,
    root: &std::path::Path,
    log_name: &str,
    now: u64,
    segment_events: Option<usize>,
    postgres_dsn: Option<&str>,
    postgres_pool_size: usize,
    risk: Option<RiskContext>,
    position: Option<PositionSnapshot>,
    market_quote: Option<QuoteTick>,
) -> Result<String, String> {
    let order = order_from_submit_command(command)
        .map_err(|error| format!("Paper SubmitOrder 订单载荷非法: {error:?}"))?;
    let mut pipeline = match postgres_dsn {
        Some(dsn) => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = dsn;
                let _ = postgres_pool_size;
                return Err(
                    "qx-execution 未启用 postgres feature，无法写入 PostgreSQL EventLog".into(),
                );
            }
            #[cfg(feature = "postgres")]
            {
                LiveEventPipeline::open_postgres_with_pool_size(
                    dsn,
                    postgres_pool_size,
                    log_name,
                    "USDT",
                )
            }
        }
        None => LiveEventPipeline::open_configured(root, log_name, "USDT", segment_events),
    }
    .map_err(|error| format!("打开 Paper EventLog 失败: {error:?}"))?;
    let mut venue = PaperVenue::new("paper");
    let mut source_seq = 0_u64;
    let mut execution = if let Some(spec) = risk
        .as_ref()
        .and_then(|context| context.instrument_spec.clone())
    {
        ExecutionService::new_with_spec(
            &mut venue,
            &mut pipeline,
            "paper-execution",
            now,
            &mut source_seq,
            spec,
        )
    } else {
        ExecutionService::new(
            &mut venue,
            &mut pipeline,
            "paper-execution",
            now,
            &mut source_seq,
        )
    };
    let submit_result = match (risk.as_ref(), position.as_ref()) {
        (Some(risk), Some(position)) => execution.execute_with_risk(command, risk, position)?,
        (None, None) => execution.execute(command)?,
        _ => return Err("Paper 风控快照必须同时提供 RiskContext 和 PositionSnapshot".into()),
    };
    if submit_result.starts_with("ALREADY_APPLIED_FROM_EVENT_LOG") {
        return Ok(submit_result);
    }
    let quote = market_quote.unwrap_or_else(|| {
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
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            order.instrument.clone(),
            quote,
            now.saturating_add(1),
            quote_seq,
            format!("{log_name}:market:{quote_seq}"),
        ))
        .map_err(|error| format!("Paper 行情事实归约失败: {error:?}"))?;
    let fills = venue.on_quote(&order.instrument, quote);
    let fill_count = fills.len();
    if let Some(spec) = risk
        .as_ref()
        .and_then(|context| context.instrument_spec.as_ref())
    {
        ingest_venue_events_with_spec(
            &mut pipeline,
            fills,
            "paper-execution",
            quote.ts,
            &mut source_seq,
            spec,
        )?;
    } else {
        ingest_venue_events(
            &mut pipeline,
            fills,
            "paper-execution",
            quote.ts,
            &mut source_seq,
        )?;
    }
    Ok(format!(
        "PAPER_EXECUTED fills={} ledger_entries={}",
        fill_count,
        pipeline.ledger().entries().len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_adapter::{
        BinanceSpotAuth, BinanceSpotVenue, CcxtProcessVenue, CcxtRpc, HttpRequest, HttpResponse,
        HttpTransport,
    };
    use qx_application::{
        EventAppender, ExecutionEventEnvelope, OrderStore, VenuePort, VenueRouterPort,
    };
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_core::{
        InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, PositionMode, Price, Quantity,
        Side, TradingInstrumentSpec, TradingProduct, SCALE,
    };
    use qx_zhenlu::{FileSpreadOrderGroupStore, SpreadOrderGroupStore, SpreadOrderLeg};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct PortState {
        orders: Vec<Order>,
        events: Vec<ExecutionEventEnvelope>,
    }

    impl OrderStore for PortState {
        fn orders(&self) -> Vec<Order> {
            self.orders.clone()
        }

        fn register_order(
            &mut self,
            order: Order,
            _ts: u64,
            _correlation_id: Option<String>,
        ) -> Result<(), String> {
            if self
                .orders
                .iter()
                .any(|existing| existing.client_id == order.client_id)
            {
                return Err("duplicate order".into());
            }
            self.orders.push(order);
            Ok(())
        }
    }

    impl EventAppender for PortState {
        fn append_execution_event(
            &mut self,
            envelope: ExecutionEventEnvelope,
        ) -> Result<(), String> {
            match &envelope.event {
                ExecutionEvent::Accepted {
                    client_order_id, ..
                } => {
                    if let Some(order) = self
                        .orders
                        .iter_mut()
                        .find(|order| order.client_id == *client_order_id)
                    {
                        order.status = OrderStatus::Accepted;
                    }
                }
                ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => {
                    if let Some(order) = self
                        .orders
                        .iter_mut()
                        .find(|order| order.client_id == fill.order_id)
                    {
                        order.filled = Quantity::from_raw(
                            order
                                .filled
                                .raw()
                                .saturating_add(fill.qty.raw())
                                .min(order.qty.raw()),
                        );
                        order.status = if order.filled == order.qty {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        };
                    }
                }
                ExecutionEvent::Cancelled { client_order_id } => {
                    if let Some(order) = self
                        .orders
                        .iter_mut()
                        .find(|order| order.client_id == *client_order_id)
                    {
                        order.status = OrderStatus::Cancelled;
                    }
                }
                ExecutionEvent::ReconcileRequired { client_order_id } => {
                    if let Some(order) = self
                        .orders
                        .iter_mut()
                        .find(|order| order.client_id == *client_order_id)
                    {
                        order.status = OrderStatus::Unknown;
                    }
                }
            }
            self.events.push(envelope);
            Ok(())
        }
    }

    struct PortVenue {
        result: Result<Vec<ExecutionEvent>, String>,
    }

    struct NeverCalledVenue;

    impl VenuePort for NeverCalledVenue {
        fn venue_id(&self) -> &str {
            "never-called"
        }

        fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
            panic!("risk rejection must happen before Venue submit")
        }

        fn cancel_order(
            &mut self,
            _client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            panic!("risk rejection test must not cancel")
        }
    }

    struct RejectingRisk;

    impl RiskPort for RejectingRisk {
        fn evaluate_order(&self, _order: &Order) -> Result<RiskDecision, String> {
            Ok(RiskDecision {
                accepted: false,
                reason_code: "max_notional",
            })
        }
    }

    #[derive(Default)]
    struct PortRouter {
        calls: Vec<String>,
    }

    impl VenueRouterPort for PortRouter {
        fn submit_order(
            &mut self,
            venue_id: &str,
            order: Order,
            ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            self.calls.push(venue_id.to_string());
            Ok(vec![
                ExecutionEvent::Accepted {
                    client_order_id: order.client_id,
                    venue_order_id: format!("{venue_id}-{}", order.client_id),
                },
                ExecutionEvent::Fill(Box::new(qx_core::Fill {
                    order_id: order.client_id,
                    qty: order.qty,
                    price: Price::from_i64(100),
                    ts,
                    ..qx_core::Fill::default()
                })),
            ])
        }

        fn cancel_order(
            &mut self,
            venue_id: &str,
            client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            self.calls.push(format!("cancel:{venue_id}"));
            Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
        }
    }

    impl VenuePort for PortVenue {
        fn venue_id(&self) -> &str {
            "port-test"
        }

        fn submit_order(&mut self, _order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
            self.result.clone()
        }

        fn cancel_order(
            &mut self,
            _client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            Ok(vec![ExecutionEvent::Cancelled { client_order_id: 7 }])
        }
    }

    fn port_order(client_id: u64) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "port-main".into(),
            trace: None,
            policy: None,
        }
    }

    #[test]
    fn port_execution_service_registers_and_appends_standard_facts() {
        let mut state = PortState::default();
        let mut venue = PortVenue {
            result: Ok(vec![ExecutionEvent::Accepted {
                client_order_id: 7,
                venue_order_id: "remote-7".into(),
            }]),
        };
        let mut source_seq = 0;
        let result =
            PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
                .submit(port_order(7))
                .unwrap();
        assert_eq!(result.event_count, 1);
        assert_eq!(state.orders.len(), 1);
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].source_seq, 1);
        assert!(matches!(
            state.events[0].event,
            ExecutionEvent::Accepted { .. }
        ));
    }

    #[test]
    fn port_execution_service_fails_closed_on_empty_venue_response() {
        let mut state = PortState::default();
        let mut venue = PortVenue {
            result: Ok(Vec::new()),
        };
        let mut source_seq = 0;
        let result =
            PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
                .submit(port_order(8));
        assert!(result.is_err());
        assert!(matches!(
            state.events.last().map(|event| &event.event),
            Some(ExecutionEvent::ReconcileRequired { client_order_id: 8 })
        ));
    }

    #[test]
    fn port_execution_service_fails_closed_on_invalid_venue_fact() {
        let mut state = PortState::default();
        let mut venue = PortVenue {
            result: Ok(vec![ExecutionEvent::Accepted {
                client_order_id: 999,
                venue_order_id: "wrong-order".into(),
            }]),
        };
        let mut source_seq = 0;
        let result =
            PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
                .submit(port_order(9));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("非法下单事实"));
        assert!(matches!(
            state.events.last().map(|event| &event.event),
            Some(ExecutionEvent::ReconcileRequired { client_order_id: 9 })
        ));
    }

    #[test]
    fn port_execution_service_rejects_before_registration_or_venue_side_effect() {
        let mut state = PortState::default();
        let mut venue = NeverCalledVenue;
        let mut source_seq = 0;
        let result =
            PortExecutionService::new(&mut venue, &mut state, "risk-worker", 10, &mut source_seq)
                .submit_with_risk(port_order(12), &RejectingRisk);

        assert_eq!(
            result.unwrap_err().to_string(),
            "账户级 RiskPort 拒绝订单: max_notional"
        );
        assert!(state.orders.is_empty());
        assert!(state.events.is_empty());
        assert_eq!(source_seq, 0);
    }

    #[test]
    fn canonical_risk_port_is_available_without_zhenlu_context_conversion() {
        let accepted_context = qx_risk::OrderRiskContext::default();
        let accepted = CanonicalRiskPort {
            context: &accepted_context,
        }
        .evaluate_order(&port_order(10))
        .unwrap();
        assert!(accepted.accepted);
        assert_eq!(accepted.reason_code, "accepted");

        let rejected_context = qx_risk::OrderRiskContext {
            max_order_notional_raw: Some(1),
            ..qx_risk::OrderRiskContext::default()
        };
        let rejected = CanonicalRiskPort {
            context: &rejected_context,
        }
        .evaluate_order(&port_order(11));
        assert!(rejected.is_err());
    }

    fn assert_submit_cancel_port_contract<V: VenuePort>(
        venue: V,
        order: Order,
        expected_venue_id: &str,
    ) {
        let mut venue = venue;
        assert_eq!(venue.venue_id(), expected_venue_id);
        let mut state = PortState::default();
        let mut source_seq = 0;
        {
            let mut execution = PortExecutionService::new(
                &mut venue,
                &mut state,
                "contract-worker",
                100,
                &mut source_seq,
            );
            let submitted = execution.submit(order.clone()).unwrap();
            assert!(submitted.event_count >= 1);
        }
        assert!(state.events.iter().any(|event| matches!(
            &event.event,
            ExecutionEvent::Accepted { client_order_id, .. } if *client_order_id == order.client_id
        )));
        let cancelled = {
            let mut execution = PortExecutionService::new(
                &mut venue,
                &mut state,
                "contract-worker",
                100,
                &mut source_seq,
            );
            execution.cancel(order.client_id).unwrap()
        };
        assert_eq!(cancelled.event_count, 1);
        assert!(matches!(
            state.events.last().map(|event| &event.event),
            Some(ExecutionEvent::Cancelled { client_order_id }) if *client_order_id == order.client_id
        ));
    }

    struct ContractCcxtRpc;

    impl CcxtRpc for ContractCcxtRpc {
        fn call(&mut self, request: serde_json::Value) -> Result<serde_json::Value, String> {
            match request.get("op").and_then(serde_json::Value::as_str) {
                Some("create_order") => Ok(serde_json::json!({
                    "order": {"order_id": "ccxt-contract-1", "status": "open", "filled_raw": 0}
                })),
                _ => Ok(serde_json::json!({})),
            }
        }
    }

    struct ContractBinanceTransport {
        responses: Mutex<Vec<HttpResponse>>,
    }

    impl HttpTransport for ContractBinanceTransport {
        fn send(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
            self.responses
                .lock()
                .map_err(|_| "contract transport lock poisoned".to_string())?
                .pop()
                .ok_or_else(|| "contract response exhausted".into())
        }
    }

    #[test]
    fn paper_ccxt_and_binance_share_submit_cancel_port_contract() {
        let order = port_order(40);
        assert_submit_cancel_port_contract(
            VenuePortAdapter::new_for_any_venue(PaperVenue::new("paper")),
            order.clone(),
            "paper",
        );

        assert_submit_cancel_port_contract(
            VenuePortAdapter::new(CcxtProcessVenue::new(
                "ccxt-binance",
                Box::new(ContractCcxtRpc),
            )),
            order.clone(),
            "ccxt-binance",
        );

        let transport = Arc::new(ContractBinanceTransport {
            responses: Mutex::new(vec![
                HttpResponse {
                    status: 200,
                    body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-40","status":"CANCELED","transactTime":100,"fills":[]}"#.into(),
                },
                HttpResponse {
                    status: 200,
                    body: r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-40","status":"NEW","transactTime":100,"fills":[]}"#.into(),
                },
            ]),
        });
        let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
        let binance =
            BinanceSpotVenue::with_endpoint("binance", auth, transport, "mock.binance", 443);
        assert_submit_cancel_port_contract(VenuePortAdapter::new(binance), order, "binance");
    }

    #[test]
    fn venue_router_map_rejects_duplicate_or_mismatched_routes() {
        let mut router = VenueRouterMap::new();
        router
            .insert(
                "port-test",
                PortVenue {
                    result: Ok(vec![ExecutionEvent::Accepted {
                        client_order_id: 9,
                        venue_order_id: "remote-9".into(),
                    }]),
                },
            )
            .unwrap();
        assert_eq!(router.len(), 1);
        assert!(router
            .insert(
                "port-test",
                PortVenue {
                    result: Ok(Vec::new()),
                },
            )
            .is_err());
        assert!(router
            .insert(
                "other",
                PortVenue {
                    result: Ok(Vec::new()),
                },
            )
            .is_err());
    }

    #[test]
    fn multi_venue_spread_routes_each_leg_and_persists_complete_group() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-multi-venue-spread-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut first = port_order(201);
        first.side = Side::Buy;
        let mut second = port_order(202);
        second.side = Side::Sell;
        let group = SpreadOrderGroup::new(
            "multi-venue-1",
            "basis-arbitrage",
            vec![
                SpreadOrderLeg {
                    leg_id: "spot".into(),
                    venue_id: "binance".into(),
                    order: first,
                },
                SpreadOrderLeg {
                    leg_id: "future".into(),
                    venue_id: "okx".into(),
                    order: second,
                },
            ],
        )
        .unwrap();
        let mut state = PortState::default();
        let mut router = PortRouter::default();
        let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
        let mut source_seq = 0;
        let outcome = MultiVenueSpreadExecutionService::new_with_store(
            group,
            &mut store,
            &mut router,
            &mut state,
            "multi-venue-worker",
            100,
            &mut source_seq,
        )
        .unwrap()
        .execute()
        .unwrap();
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Filled);
        assert_eq!(router.calls, vec!["binance", "okx"]);
        assert_eq!(state.orders.len(), 2);
        assert_eq!(state.events.len(), 4);
        assert_eq!(
            store.load("multi-venue-1").unwrap().unwrap().status,
            SpreadOrderGroupStatus::Filled
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hedge_recovery_worker_is_idempotent_and_fails_closed_on_unknown_state() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-hedge-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut first = port_order(401);
        first.side = Side::Buy;
        let mut second = port_order(402);
        second.side = Side::Sell;
        let mut group = SpreadOrderGroup::new(
            "hedge-recovery-1",
            "basis-recovery",
            vec![
                SpreadOrderLeg {
                    leg_id: "spot".into(),
                    venue_id: "binance".into(),
                    order: first,
                },
                SpreadOrderLeg {
                    leg_id: "future".into(),
                    venue_id: "okx".into(),
                    order: second,
                },
            ],
        )
        .unwrap();
        group.begin_submission().unwrap();
        group.record_accepted("spot").unwrap();
        group
            .record_fill(
                "spot",
                &qx_core::Fill {
                    order_id: 401,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ..qx_core::Fill::default()
                },
            )
            .unwrap();
        group.record_cancelled("future").unwrap();
        assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
        let mut risk_blocked_group = group.clone();
        risk_blocked_group.group_id = "hedge-recovery-risk-blocked".into();

        let mut state = PortState::default();
        let mut router = PortRouter::default();
        let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
        let other_token = store
            .try_claim("hedge-recovery-1", "other-worker", 500, 30_000)
            .unwrap()
            .expect("other worker should acquire claim");
        let mut blocked_state = PortState::default();
        let mut blocked_router = PortRouter::default();
        let mut blocked_seq = 0;
        let blocked = HedgeRecoveryWorker::new(
            group.clone(),
            &mut store,
            &mut blocked_router,
            &mut blocked_state,
            "hedge-worker",
            501,
            &mut blocked_seq,
        )
        .unwrap()
        .execute()
        .unwrap();
        assert!(!blocked.completed);
        assert!(blocked.errors[0].contains("其他恢复 owner"));
        store
            .release_claim("hedge-recovery-1", "other-worker", other_token)
            .unwrap();
        let mut source_seq = 0;
        let outcome = HedgeRecoveryWorker::new(
            group,
            &mut store,
            &mut router,
            &mut state,
            "hedge-worker",
            500,
            &mut source_seq,
        )
        .unwrap()
        .execute()
        .unwrap();
        assert!(outcome.completed);
        assert_eq!(outcome.attempted, 1);
        assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Hedged);
        assert_eq!(router.calls, vec!["binance"]);
        assert_eq!(state.orders.len(), 1);
        assert!(state.orders[0].policy.unwrap().reduce_only);
        let persisted = store.load("hedge-recovery-1").unwrap().unwrap();
        assert_eq!(persisted.status, SpreadOrderGroupStatus::Hedged);

        let mut guarded_state = PortState::default();
        let mut guarded_router = PortRouter::default();
        let mut guarded_store =
            FileSpreadOrderGroupStore::new(root.join("guarded-groups")).unwrap();
        let guarded_validator =
            |_order: &Order| -> Result<(), String> { Err("risk snapshot unavailable".into()) };
        let guarded = HedgeRecoveryWorker::new(
            risk_blocked_group,
            &mut guarded_store,
            &mut guarded_router,
            &mut guarded_state,
            "hedge-worker",
            500,
            &mut source_seq,
        )
        .unwrap()
        .execute_with_validator(Some(&guarded_validator))
        .unwrap();
        assert!(!guarded.completed);
        assert!(guarded.errors[0].contains("风控"));
        assert!(guarded_router.calls.is_empty());
        assert_eq!(guarded.attempted, 0);

        let mut retry_state = PortState::default();
        let mut retry_router = PortRouter::default();
        let mut retry_seq = source_seq;
        let retry = HedgeRecoveryWorker::new(
            outcome.group.clone(),
            &mut store,
            &mut retry_router,
            &mut retry_state,
            "hedge-worker",
            501,
            &mut retry_seq,
        )
        .unwrap()
        .execute()
        .unwrap();
        assert!(retry.completed);
        assert!(retry_router.calls.is_empty());

        let mut unknown = outcome.group;
        unknown.group_id = "hedge-recovery-unknown".into();
        unknown.status = SpreadOrderGroupStatus::ReconcileRequired;
        let mut unknown_state = PortState::default();
        let mut unknown_router = PortRouter::default();
        let mut unknown_seq = retry_seq;
        let blocked = HedgeRecoveryWorker::new(
            unknown,
            &mut store,
            &mut unknown_router,
            &mut unknown_state,
            "hedge-worker",
            502,
            &mut unknown_seq,
        )
        .unwrap()
        .execute()
        .unwrap();
        assert!(!blocked.completed);
        assert!(blocked.errors[0].contains("对账"));
        assert!(unknown_router.calls.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn accepted_events_replay_by_correlation_even_when_local_sequence_changes() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-execution-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
        pipeline
            .register_order(
                Order {
                    client_id: 1,
                    instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
                    side: Side::Buy,
                    qty: Quantity::from_i64(1),
                    limit: None,
                    status: OrderStatus::PendingSubmit,
                    filled: Quantity::ZERO,
                    account_id: "main".into(),
                    trace: None,
                    policy: None,
                },
                1,
            )
            .unwrap();
        let event = VenueEvent::Accepted {
            client_order_id: 1,
            venue_order_id: "venue-1".into(),
            ts: 2,
        };
        let mut first_seq = 0;
        let first = ingest_venue_events(
            &mut pipeline,
            vec![event.clone()],
            "execution",
            2,
            &mut first_seq,
        )
        .unwrap();
        let mut second_seq = 10;
        let second =
            ingest_venue_events(&mut pipeline, vec![event], "execution", 3, &mut second_seq)
                .unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 1);
        assert_eq!(pipeline.log().len(), 2);
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Accepted);
        assert_eq!(pipeline.venue_order_id(1).as_deref(), Some("venue-1"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn risk_preflight_rejects_before_event_log_or_venue_side_effect() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-execution-risk-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
        let order = Order {
            client_id: 2,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                leverage: 10,
                ..OrderPolicy::default()
            }),
        };
        let command = ControlCommand {
            command_id: 2,
            request_id: "risk-preflight".into(),
            operator_id: "test".into(),
            reason: "risk preflight".into(),
            kind: CommandKind::SubmitOrder,
            target: "2".into(),
            payload: BTreeMap::from([(
                "order_json".into(),
                serde_json::to_string(&order).unwrap(),
            )]),
            permission: Permission::Trading,
            dry_run: false,
        };
        let spec = TradingInstrumentSpec {
            instrument,
            product: TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 20,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let required = spec
            .initial_margin(order.qty.raw(), order.limit.unwrap().raw(), 10)
            .unwrap();
        let risk = RiskContext {
            available_margin_raw: Some(required - 1),
            reference_price: order.limit,
            instrument_spec: Some(spec),
            ..RiskContext::default()
        };
        let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
        let mut venue = PaperVenue::new("paper");
        let mut source_seq = 0;
        let risk_context = RiskExecutionContext {
            risk: &risk,
            position: &PositionSnapshot::default(),
        };
        let result = submit_order_with_risk(
            &command,
            &mut venue,
            &mut pipeline,
            "execution",
            1,
            &mut source_seq,
            &risk_context,
        );
        assert!(result.is_err());
        assert!(pipeline.orders().is_empty());
        assert!(venue.snapshot().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_derivative_fill_uses_spec_pnl_accounting_instead_of_spot_cash() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-execution-paper-derivative-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
        let order = Order {
            client_id: 3,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                leverage: 5,
                ..OrderPolicy::default()
            }),
        };
        let command = ControlCommand {
            command_id: order.client_id,
            request_id: "paper-derivative".into(),
            operator_id: "test".into(),
            reason: "paper derivative accounting".into(),
            kind: CommandKind::SubmitOrder,
            target: order.client_id.to_string(),
            payload: BTreeMap::from([(
                "order_json".into(),
                serde_json::to_string(&order).unwrap(),
            )]),
            permission: Permission::Trading,
            dry_run: false,
        };
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 20,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let risk = RiskContext {
            available_margin_raw: Some(10_000 * SCALE),
            reference_price: order.limit,
            instrument_spec: Some(spec),
            ..RiskContext::default()
        };
        let result = execute_paper_submit_effect_with_risk(
            &command,
            &root,
            "events",
            1,
            Some(risk),
            Some(PositionSnapshot::default()),
        )
        .unwrap();
        assert!(result.contains("fills=1"));
        let pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
        assert_eq!(
            pipeline
                .ledger()
                .position_for("main", &instrument)
                .quantity
                .raw(),
            Quantity::from_i64(1).raw()
        );
        assert_eq!(pipeline.ledger().cash_for("main", "USDT"), 0);
        assert!(pipeline
            .ledger()
            .entries()
            .iter()
            .all(|entry| entry.kind != qx_core::LedgerEntryKind::TradeCash));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spread_execution_submits_all_legs_and_keeps_group_lifecycle() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-execution-spread-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let make_order = |client_id: u64, instrument: &str| Order {
            client_id,
            instrument: InstrumentId::parse(instrument).unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "spread-main".into(),
            trace: None,
            policy: None,
        };
        let group = SpreadOrderGroup::new(
            "spread-1",
            "basis-arbitrage",
            vec![
                SpreadOrderLeg {
                    leg_id: "spot".into(),
                    venue_id: "binance".into(),
                    order: make_order(101, "BTCUSDT.BINANCE"),
                },
                SpreadOrderLeg {
                    leg_id: "future".into(),
                    venue_id: "okx".into(),
                    order: make_order(102, "BTCUSDT.OKX"),
                },
            ],
        )
        .unwrap();
        let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
        let mut venue = PaperVenue::new("paper");
        let mut group_store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
        let mut source_seq = 0;
        let outcome = SpreadExecutionService::new_with_store(
            group,
            &mut group_store,
            &mut venue,
            &mut pipeline,
            "spread-execution",
            1,
            &mut source_seq,
        )
        .unwrap()
        .execute()
        .unwrap();
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Submitting);
        assert!(outcome.compensation_targets().is_empty());
        assert_eq!(pipeline.orders().len(), 2);
        let persisted = group_store.load("spread-1").unwrap().unwrap();
        assert_eq!(persisted.status, SpreadOrderGroupStatus::Submitting);
        let _ = std::fs::remove_dir_all(root);
    }
}
