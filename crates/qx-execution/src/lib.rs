//! 统一执行层边界。
//!
//! Venue 只返回 `VenueEvent`，执行层负责把这些事件转换成运行时标准事实，
//! 再交给 `LiveEventPipeline` 归约到 EventLog、订单状态和 Ledger。Paper、
//! Binance 以及未来连接器都必须复用这里的事实转换，不能在 CLI 中各写一套。

use qx_control::ControlCommand;
use qx_core::{Price, Quantity, TradingInstrumentSpec, SCALE};
use qx_guanxing::QuoteTick;
use qx_runtime::{
    order_from_submit_command, LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent,
};
use qx_zhenlu::{PaperVenue, PositionSnapshot, RiskContext, Venue, VenueEvent};

/// Venue 无关的执行编排服务。
///
/// 该服务持有一次执行所需的运行时上下文，但不持有 ControlPlane、策略或
/// 具体交易所类型。调用方负责在服务外完成权限、路由和命令 Accepted 审计，
/// 服务只保证“注册订单 → 调 Venue → 归约标准回报”的副作用边界一致。
pub struct ExecutionService<'a, V: Venue> {
    venue: &'a mut V,
    pipeline: &'a mut LiveEventPipeline,
    worker_id: &'a str,
    now: u64,
    source_seq: &'a mut u64,
    instrument_spec: Option<TradingInstrumentSpec>,
}

pub struct RiskExecutionContext<'a> {
    pub risk: &'a RiskContext,
    pub position: &'a PositionSnapshot,
}

impl<'a, V: Venue> ExecutionService<'a, V> {
    pub fn new(
        venue: &'a mut V,
        pipeline: &'a mut LiveEventPipeline,
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
        pipeline: &'a mut LiveEventPipeline,
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
                return Ok(format!(
                    "ALREADY_APPLIED_FROM_EVENT_LOG status={:?}",
                    existing.status
                ));
            }
            if matches!(
                existing.status,
                qx_core::OrderStatus::Submitted | qx_core::OrderStatus::Unknown
            ) {
                *self.source_seq = self.source_seq.saturating_add(1);
                self.pipeline
                    .ingest(RuntimeEventEnvelope::venue(
                        RuntimeExternalEvent::ReconcileRequired {
                            client_order_id: existing.client_id,
                        },
                        self.now,
                        self.now,
                        *self.source_seq,
                        format!("{}:ambiguous-submit:{}", self.worker_id, existing.client_id),
                    ))
                    .map_err(|error| format!("标记未知下单结果失败: {error:?}"))?;
                return Err(format!(
                    "订单 {} 已离开本地但未有 Accepted 事实，必须先对账，禁止自动补单",
                    existing.client_id
                ));
            }
        } else {
            self.pipeline
                .register_control_order(command, self.now)
                .map_err(|error| format!("写入 OrderSubmitted 失败: {error:?}"))?;
        }
        let order = self
            .pipeline
            .orders()
            .into_iter()
            .find(|order| order.client_id == requested_order.client_id)
            .ok_or_else(|| "OrderSubmitted 后找不到订单".to_string())?;
        let events = self
            .venue
            .submit(order, self.now)
            .map_err(|error| format!("Venue submit 失败/结果未知: {error:?}"))?;
        let count = if let Some(spec) = self.instrument_spec.as_ref() {
            ingest_venue_events_with_spec(
                self.pipeline,
                events,
                self.worker_id,
                self.now,
                self.source_seq,
                spec,
            )?
        } else {
            ingest_venue_events(
                self.pipeline,
                events,
                self.worker_id,
                self.now,
                self.source_seq,
            )?
        };
        Ok(format!("SUBMITTED venue_events={count}"))
    }

    /// 带账户级 RiskContext 的执行入口。风控失败发生在写入 OrderSubmitted 和
    /// 调用 Venue 之前；调用方必须传入同一时点的账户快照、产品规格和市场参考价。
    pub fn execute_with_risk(
        &mut self,
        command: &ControlCommand,
        risk: &RiskContext,
        position: &PositionSnapshot,
    ) -> Result<String, String> {
        let order = order_from_submit_command(command)
            .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
        risk.validate_order(&order, position)
            .map_err(|error| format!("账户级 RiskContext 拒绝订单: {error:?}"))?;
        self.execute(command)
    }
}

/// 将 Venue 返回的订单事实统一归约到 Runtime/EventLog/Ledger 管线。
///
/// `source_seq` 由调用方持有；重复回报仍由 LiveEventPipeline 去重。
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
    let ask = order.limit.unwrap_or_else(|| Price::from_i64(100));
    let bid = Price::from_raw(ask.raw().saturating_sub(SCALE));
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            order.instrument.clone(),
            bid,
            ask,
            now.saturating_add(1),
            now.saturating_add(1),
            1,
            format!("{log_name}:market:1"),
        ))
        .map_err(|error| format!("Paper 行情事实归约失败: {error:?}"))?;
    let fills = venue.on_quote(
        &order.instrument,
        QuoteTick::new(
            now.saturating_add(1),
            bid,
            Quantity::from_i64(1_000),
            ask,
            Quantity::from_i64(1_000),
            1,
        ),
    );
    let fill_count = fills.len();
    if let Some(spec) = risk
        .as_ref()
        .and_then(|context| context.instrument_spec.as_ref())
    {
        ingest_venue_events_with_spec(
            &mut pipeline,
            fills,
            "paper-execution",
            now.saturating_add(1),
            &mut source_seq,
            spec,
        )?;
    } else {
        ingest_venue_events(
            &mut pipeline,
            fills,
            "paper-execution",
            now.saturating_add(1),
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
    use qx_control::{CommandKind, ControlCommand, Permission};
    use qx_core::{
        InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, PositionMode, Price, Quantity,
        Side, TradingInstrumentSpec, TradingProduct, SCALE,
    };
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

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
}
