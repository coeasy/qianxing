//! 延迟单位契约：`latency_base_ns`/`latency_insert_ns` 按纳秒配置，撮合时间轴
//! （`Bar.ts` 与全仓事件时间戳）按毫秒走。

use qx_core::{InstrumentId, Order, OrderStatus, Quantity, Side, ZeroFeeModel};
use qx_guanxing::Bar;
use qx_xingban::{BarMatchingEngine, NextBarOpenFillModel, StaticLatency};

const BAR_MS: u64 = 60_000;

fn buy(client_id: u64) -> Order {
    Order {
        client_id,
        instrument: InstrumentId::parse("TEST.V").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "a".into(),
        trace: None,
        policy: None,
    }
}

fn engine(base_ns: u64) -> BarMatchingEngine {
    BarMatchingEngine::new_with_latency(
        Box::new(NextBarOpenFillModel),
        Box::new(ZeroFeeModel),
        Box::new(StaticLatency {
            base_ns,
            insert_ns: 0,
        }),
        1,
    )
}

/// 纳秒直接加到毫秒时间轴上会把 1ms 放大成 1e6 ms（约 16.7 分钟）：一分钟 bar 上
/// 订单静默永不成交，而回测仍以 `fills=0` 正常退出。
#[test]
fn millisecond_latency_eligibilizes_on_the_next_bar() {
    let mut engine = engine(1_000_000);
    engine.submit_at(buy(1), BAR_MS);
    let fills = engine.on_bar(&Bar::new(2 * BAR_MS, 101, 101, 101, 101, 1), 2 * BAR_MS);
    assert_eq!(fills.len(), 1, "1ms 延迟不能把成交推到几十分钟之后");
}

/// 250ms 是真实网络往返的量级，远小于一根 bar：它不能推迟任何一根可成交的 bar。
#[test]
fn realistic_latency_stays_inside_the_bar_cadence() {
    let mut engine = engine(250_000_000);
    engine.submit_at(buy(2), BAR_MS);
    let fills = engine.on_bar(&Bar::new(2 * BAR_MS, 101, 101, 101, 101, 1), 2 * BAR_MS);
    assert_eq!(fills.len(), 1, "250ms 延迟在 60s bar 上必须照样成交");
}
