//! 应用层端口。
//!
//! 这里不拥有 EventLog、数据库或具体 Venue；只定义执行编排需要的稳定事实和
//! 端口。Runtime、Storage、Paper、CCXT 和 Binance 通过 adapter 实现这些端口，
//! 避免应用服务反向依赖某个运行时具体类型。

use qx_core::{Fill, InstrumentId, Order, TradingInstrumentSpec};
use qx_guanxing::QuoteTick;
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ExecutionEvent {
    Accepted {
        client_order_id: u64,
        venue_order_id: String,
    },
    Fill(Box<Fill>),
    FillWithSpec {
        fill: Box<Fill>,
        spec: Box<TradingInstrumentSpec>,
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

pub trait EventAppender {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String>;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RiskDecision {
    pub accepted: bool,
    pub reason_code: &'static str,
}

pub trait RiskPort {
    fn evaluate_order(&self, order: &Order) -> Result<RiskDecision, String>;
}

pub trait MarketDataPort {
    fn latest_quote(&self, instrument: &InstrumentId) -> Option<QuoteTick>;
}

/// 单机 Paper/回测可直接使用的最新报价簿。
///
/// 它只保存已经通过 `validate_quote` 的 L1 报价，不承担行情持久化；生产
/// worker 应在写入 EventLog 后再更新此端口，避免内存报价成为唯一事实来源。
#[derive(Clone, Default, Debug)]
pub struct QuoteBook {
    quotes: BTreeMap<InstrumentId, QuoteTick>,
}

impl QuoteBook {
    pub fn upsert(&mut self, instrument: InstrumentId, quote: QuoteTick) -> Result<(), String> {
        self.quotes.insert(instrument, validate_quote(quote)?);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.quotes.clear();
    }

    pub fn len(&self) -> usize {
        self.quotes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.quotes.is_empty()
    }
}

impl MarketDataPort for QuoteBook {
    fn latest_quote(&self, instrument: &InstrumentId) -> Option<QuoteTick> {
        self.quotes.get(instrument).copied()
    }
}

pub trait ReconcilePort {
    fn require_reconcile(&mut self, client_order_id: u64, reason: &str) -> Result<(), String>;
}

pub trait VenuePort {
    fn venue_id(&self) -> &str;
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
    if fill.qty.raw() <= 0 {
        return Err("Fill 数量必须为正".into());
    }
    if fill.price.raw() <= 0 {
        return Err("Fill 价格必须为正".into());
    }
    if fill.qty.raw() > order.remaining().raw() {
        return Err(format!(
            "Fill 数量超过订单剩余数量: remaining={} actual={}",
            order.remaining().raw(),
            fill.qty.raw()
        ));
    }
    Ok(())
}

/// 端口层的最小报价校验，Paper/Live 共用，避免应用层接受 crossed 或无流动性报价。
pub fn validate_quote(quote: QuoteTick) -> Result<QuoteTick, String> {
    if quote.bid.raw() <= 0
        || quote.ask.raw() <= 0
        || quote.bid.raw() > quote.ask.raw()
        || quote.bid_qty.raw() <= 0
        || quote.ask_qty.raw() <= 0
    {
        return Err("行情报价非法".into());
    }
    Ok(quote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{Price, Quantity};

    #[test]
    fn quote_book_rejects_invalid_quotes_and_keeps_latest_valid_quote() {
        let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
        let mut book = QuoteBook::default();
        let invalid = QuoteTick::new(
            1,
            Price::from_i64(101),
            Quantity::from_i64(1),
            Price::from_i64(100),
            Quantity::from_i64(1),
            1,
        );
        assert!(book.upsert(instrument.clone(), invalid).is_err());
        assert!(book.is_empty());

        let valid = QuoteTick::new(
            2,
            Price::from_i64(100),
            Quantity::from_i64(2),
            Price::from_i64(101),
            Quantity::from_i64(3),
            2,
        );
        book.upsert(instrument.clone(), valid).unwrap();
        assert_eq!(book.len(), 1);
        assert_eq!(book.latest_quote(&instrument), Some(valid));
    }

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
        }
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
