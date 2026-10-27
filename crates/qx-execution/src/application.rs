//! 应用层端口。
//!
//! 原 `qx-application` crate，V10 P2a 按"端口即执行事实契约，归执行网关"并入本 crate。
//! 这里不拥有 EventLog、数据库或具体 Venue；只定义执行编排需要的稳定事实和
//! 端口。Runtime、Storage、Paper、CCXT 和 Binance 通过 adapter 实现这些端口，
//! 避免应用服务反向依赖某个运行时具体类型。

pub use qx_core::{
    Fill, InstrumentId, Order, OrderPolicy, OrderStatus, OrderTrace, Price, Quantity, Side,
    TradingInstrumentSpec, TradingProduct,
};
use qx_guanxing::QuoteTick;
pub use qx_zhenlu::venue_capabilities::{
    VenueCapabilities, VenueMarketScope, VenueOrderType, PAPER_VENUE_CAPABILITIES,
};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ExecutionEvent {
    Accepted {
        client_order_id: u64,
        venue_order_id: String,
    },
    Fill(Box<Fill>),
    FeeAdjustment {
        account_id: String,
        venue_id: String,
        order_id: u64,
        instrument: InstrumentId,
        fees: Vec<qx_core::FillFee>,
    },
    FillWithSpec {
        fill: Box<Fill>,
        spec: Box<TradingInstrumentSpec>,
    },
    /// Persisted before the cancel side effect; a restart must reconcile this operation.
    CancelPending {
        client_order_id: u64,
    },
    Cancelled {
        client_order_id: u64,
    },
    ReconcileRequired {
        client_order_id: u64,
    },
    /// 行情事实。撮合前必须先落行情事件，使 Paper 与 Live 共享同一道行情门禁。
    MarketQuote {
        instrument: InstrumentId,
        quote: QuoteTick,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExecutionEventEnvelope {
    pub event: ExecutionEvent,
    pub event_ts: u64,
    pub receive_ts: u64,
    pub source_seq: u64,
    pub correlation_id: String,
}

/// Result of appending an execution fact to a store with durable deduplication.
/// `Duplicate` means the same correlation was already committed and the caller
/// must not repeat the external side effect associated with that fact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventAppendOutcome {
    Appended,
    Duplicate,
}

pub trait EventAppender {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String>;

    /// Append a fact and report whether this call committed it. Persistent
    /// implementations that support idempotency should override this method.
    /// The default preserves compatibility for append-only adapters without a
    /// duplicate receipt; production cancel paths use the Runtime override.
    fn append_execution_event_with_outcome(
        &mut self,
        envelope: ExecutionEventEnvelope,
    ) -> Result<EventAppendOutcome, String> {
        self.append_execution_event(envelope)?;
        Ok(EventAppendOutcome::Appended)
    }
}

pub trait OrderStore {
    fn orders(&self) -> Vec<Order>;

    fn register_order(
        &mut self,
        order: Order,
        ts: u64,
        correlation_id: Option<String>,
    ) -> Result<(), String>;
}

pub trait ExecutionEventPort: EventAppender + OrderStore {}

impl<T: EventAppender + OrderStore> ExecutionEventPort for T {}

/// 只读的账本条目计数，用于执行结果摘要；实现者不必暴露 Ledger 结构。
pub trait LedgerProbe {
    fn ledger_entry_count(&self) -> usize;
}

/// 风控端口对单笔订单的裁决。
///
/// 两条通道各有其义，V13 §9.12 #172 之前它们被混用过：
/// * `Ok(Allow)` / `Ok(Reject { .. })` —— 端口**给出了**裁决；拒绝是正常业务结果，
///   订单不该出门，但不代表链路坏了。
/// * `Err(..)` —— 端口给不出裁决（读不到账户状态、上游链路故障等）。
///
/// 原形状是 `struct { accepted: bool, reason_code: &'static str }`：两个生产实现
/// 只在通过时写 `accepted: true`，拒绝改走 `Err`，于是 `accepted: false` 与
/// `if !decision.accepted { "风控拒绝订单" }` 那条播报只有测试假件能触发，真实
/// 拒绝被上层报成「RiskPort 执行失败」。改名同时消掉与 `qx_risk::RiskDecision`
/// 的同名异物（V13 §5 R2-1 预登记的口径）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiskVerdict {
    Allow,
    Reject { reason: String },
}

pub trait RiskPort {
    fn evaluate_order(&self, order: &Order) -> Result<RiskVerdict, String>;
}

pub trait VenuePort {
    fn venue_id(&self) -> &str;
    fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities;
    /// Dynamic connector/reconciliation gate, checked before the gateway registers an order.
    fn execution_readiness(&self) -> Result<(), String>;
    fn validate_submit_capabilities(&self, order: &Order) -> Result<(), String> {
        self.capabilities().validate_submit(order, self.venue_id())
    }
    fn submit_order(&mut self, order: Order, ts: u64) -> Result<Vec<ExecutionEvent>, String>;
    fn cancel_order(
        &mut self,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String>;
}

/// 多 Venue 路由端口。它刻意只返回标准事实，不承诺跨 Venue 原子性；
/// 应用层必须在每条腿事实落盘后更新 SpreadOrderGroup，失败进入补偿/对账。
pub trait VenueRouterPort {
    fn capabilities_for(
        &self,
        venue_id: &str,
    ) -> Option<qx_zhenlu::venue_capabilities::VenueCapabilities>;

    fn execution_readiness_for(&self, venue_id: &str) -> Result<(), String>;

    fn submit_order(
        &mut self,
        venue_id: &str,
        order: Order,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String>;

    fn cancel_order(
        &mut self,
        venue_id: &str,
        client_order_id: u64,
        ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String>;
}

/// 校验 Venue 返回的单腿提交事实。
///
/// 该校验位于应用端口层，所有 Paper、CCXT、Binance 以及未来券商适配器
/// 只要接入 `VenuePort` 就必须遵守同一份契约。异常响应不能直接写入事件
/// 账本；调用方应将订单置为待对账，避免把错误的成交或其他订单事实归属
/// 到当前订单。
pub fn validate_submit_events(order: &Order, events: &[ExecutionEvent]) -> Result<(), String> {
    if events.is_empty() {
        return Err("Venue submit 未返回执行事实".into());
    }
    let mut quote_spent = order.quote_filled_decimal();
    for event in events {
        match event {
            ExecutionEvent::Accepted {
                client_order_id,
                venue_order_id,
            } => {
                if *client_order_id != order.client_id {
                    return Err(format!(
                        "Accepted client_order_id 不匹配: expected={} actual={}",
                        order.client_id, client_order_id
                    ));
                }
                if venue_order_id.trim().is_empty() {
                    return Err("Accepted 缺少 venue_order_id".into());
                }
            }
            ExecutionEvent::Fill(fill) | ExecutionEvent::FillWithSpec { fill, .. } => {
                validate_fill_for_order(order, fill)?;
                if let Some(budget) = order.quote_budget_decimal() {
                    let cost = fill
                        .exact_amounts
                        .and_then(|amounts| amounts.quote_cost)
                        .ok_or_else(|| "计价币预算订单成交缺少精确花费".to_string())?;
                    quote_spent = quote_spent
                        .checked_add(cost)
                        .ok_or_else(|| "提交返回成交的计价币花费累计溢出".to_string())?;
                    if quote_spent
                        .checked_cmp(budget)
                        .is_none_or(|ordering| ordering.is_gt())
                    {
                        return Err("提交返回的成交总花费超过计价币预算".into());
                    }
                }
            }
            ExecutionEvent::FeeAdjustment { .. } => {
                return Err("订单提交接口不能返回独立手续费调整事实".into());
            }
            ExecutionEvent::Cancelled { client_order_id }
            | ExecutionEvent::ReconcileRequired { client_order_id } => {
                if *client_order_id != order.client_id {
                    return Err(format!(
                        "执行事实 client_order_id 不匹配: expected={} actual={}",
                        order.client_id, client_order_id
                    ));
                }
            }
            ExecutionEvent::CancelPending { .. } => {
                return Err("订单提交接口不能返回本地撤单发起事实".into());
            }
            ExecutionEvent::MarketQuote { instrument, .. } => {
                return Err(format!("提交接口返回了行情事实: instrument={instrument}"));
            }
        }
    }
    Ok(())
}

/// 校验 Venue 返回的撤单事实。撤单接口不能把其他订单的成交、接收或撤单
/// 事实写入当前订单；结果未知时只能返回 `ReconcileRequired`。
pub fn validate_cancel_events(
    client_order_id: u64,
    events: &[ExecutionEvent],
) -> Result<(), String> {
    if events.is_empty() {
        return Err("Venue cancel 未返回执行事实".into());
    }
    for event in events {
        match event {
            ExecutionEvent::Cancelled {
                client_order_id: actual,
            }
            | ExecutionEvent::ReconcileRequired {
                client_order_id: actual,
            } if *actual == client_order_id => {}
            ExecutionEvent::Cancelled {
                client_order_id: actual,
            }
            | ExecutionEvent::ReconcileRequired {
                client_order_id: actual,
            } => {
                return Err(format!(
                    "撤单事实 client_order_id 不匹配: expected={} actual={}",
                    client_order_id, actual
                ));
            }
            _ => return Err("撤单接口返回了非撤单事实".into()),
        }
    }
    Ok(())
}

fn validate_fill_for_order(order: &Order, fill: &Fill) -> Result<(), String> {
    if fill.order_id != order.client_id {
        return Err(format!(
            "Fill order_id 不匹配: expected={} actual={}",
            order.client_id, fill.order_id
        ));
    }
    fill.validate_exact_amounts()?;
    if order.exact_amounts.is_some() != fill.exact_amounts.is_some() {
        return Err(if order.exact_amounts.is_some() {
            "精确订单收到缺少精确值的成交".into()
        } else {
            "精确成交不能应用到旧精度订单".into()
        });
    }
    let fill_quantity = fill.quantity_decimal();
    if !fill_quantity.is_positive() {
        return Err("Fill 数量必须为正".into());
    }
    let fill_price = fill.price_decimal();
    if !fill_price.is_positive() {
        return Err("Fill 价格必须为正".into());
    }
    if let Some(budget) = order.quote_budget_decimal() {
        let cost = fill
            .exact_amounts
            .and_then(|amounts| amounts.quote_cost)
            .ok_or_else(|| "计价币预算订单成交缺少精确花费".to_string())?;
        let remaining_budget = budget
            .checked_sub(order.quote_filled_decimal())
            .ok_or_else(|| "计价币预算剩余量计算溢出".to_string())?;
        if cost
            .checked_cmp(remaining_budget)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err("Fill 花费超过订单剩余计价币预算".into());
        }
    } else {
        let remaining = order
            .remaining_decimal()
            .ok_or_else(|| "订单剩余数量计算溢出".to_string())?;
        if fill_quantity
            .checked_cmp(remaining)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err("Fill 数量超过订单剩余数量".into());
        }
    }
    if let Some(limit_price) = order.limit_price_decimal() {
        let outside_limit = match order.side {
            qx_core::Side::Buy => fill_price
                .checked_cmp(limit_price)
                .is_none_or(|ordering| ordering.is_gt()),
            qx_core::Side::Sell => fill_price
                .checked_cmp(limit_price)
                .is_none_or(|ordering| ordering.is_lt()),
        };
        if outside_limit {
            return Err("Fill 成交价格违反订单限价".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Price, Quantity};

    fn test_order() -> Order {
        Order {
            client_id: 7,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: qx_core::Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: qx_core::OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "test-account".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }
    }

    #[test]
    fn fill_validation_preserves_exact_precision_and_enforces_limit_side() {
        use qx_core::{ExactDecimal, ExactFillAmounts, ExactOrderAmounts};

        let mut order = test_order();
        order.qty = Quantity::ZERO;
        order.limit = None;
        order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000002").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.0000000100000001").unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        let fill = Fill {
            order_id: order.client_id,
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::parse("0.0000000001").unwrap(),
                price: ExactDecimal::parse("0.0000000100000002").unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            ..Fill::default()
        };
        assert_eq!(
            validate_fill_for_order(&order, &fill).unwrap_err(),
            "Fill 成交价格违反订单限价"
        );

        let mut within_limit = fill;
        within_limit.exact_amounts.as_mut().unwrap().price =
            ExactDecimal::parse("0.0000000100000000").unwrap();
        assert!(validate_fill_for_order(&order, &within_limit).is_ok());
    }

    #[test]
    fn execution_contract_rejects_cross_order_or_invalid_fill() {
        let order = test_order();
        let wrong_order = ExecutionEvent::Accepted {
            client_order_id: 8,
            venue_order_id: "remote-8".into(),
        };
        assert!(validate_submit_events(&order, &[wrong_order]).is_err());

        let invalid_fill = ExecutionEvent::Fill(Box::new(Fill {
            order_id: order.client_id,
            qty: Quantity::from_i64(3),
            price: Price::from_i64(100),
            ..Fill::default()
        }));
        assert!(validate_submit_events(&order, &[invalid_fill]).is_err());
    }

    #[test]
    fn execution_contract_accepts_immediate_fill_and_valid_cancel() {
        let order = test_order();
        let fill = ExecutionEvent::Fill(Box::new(Fill {
            order_id: order.client_id,
            qty: Quantity::from_i64(2),
            price: Price::from_i64(100),
            ..Fill::default()
        }));
        assert!(validate_submit_events(&order, &[fill]).is_ok());
        assert!(validate_cancel_events(
            order.client_id,
            &[ExecutionEvent::Cancelled {
                client_order_id: order.client_id,
            }]
        )
        .is_ok());
        assert!(validate_cancel_events(
            order.client_id,
            &[ExecutionEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: "remote-7".into(),
            }]
        )
        .is_err());
    }
}
