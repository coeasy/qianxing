//! 仿真撮合引擎（bar 级）。
//!
//! **结构性防作弊**：策略只基于已结束的可见 bar 做决策，订单随后进入下一可执行
//! bar 的撮合。Bar 级数据无法证明真实队列位置或 maker 身份，因此费用按未知流动性
//! 处理，并采用费用模型的保守规则。

use qx_core::{Fill, Money, Order, OrderStatus, Price, Quantity};
use qx_guanxing::Bar;

use crate::cost::{FeeContext, FeeModel, LatencyModel, LiquidityRole, ZeroLatency};
use crate::fill::{FillContext, FillModel};
use crate::rng::DeterministicRng;

pub struct MatchingConfig {
    pub fill: Box<dyn FillModel>,
    pub fee: Box<dyn FeeModel>,
    pub latency: Box<dyn LatencyModel>,
}

struct PendingOrder {
    order: Order,
    eligible_ts: u64,
}

pub struct BarMatchingEngine {
    pending: Vec<PendingOrder>,
    fill_model: Box<dyn FillModel>,
    fee_model: Box<dyn FeeModel>,
    latency_model: Box<dyn LatencyModel>,
    /// 费用模型仍接收 qty/price；这里将合约乘数折算到 fee price，避免
    /// contractSize != 1 时手续费仍按现货名义额计算。
    fee_price_multiplier: i128,
    rng: DeterministicRng,
    all_fills: Vec<Fill>,
}

impl BarMatchingEngine {
    pub fn new(fill: Box<dyn FillModel>, fee: Box<dyn FeeModel>, seed: u64) -> Self {
        Self {
            pending: Vec::new(),
            fill_model: fill,
            fee_model: fee,
            latency_model: Box::new(ZeroLatency),
            fee_price_multiplier: 1,
            rng: DeterministicRng::new(seed),
            all_fills: Vec::new(),
        }
    }

    /// 由配置构造：撮合模型与费率模型成对装配，避免二者档位不匹配。
    pub fn from_config(cfg: MatchingConfig, seed: u64) -> Self {
        Self::new_with_latency(cfg.fill, cfg.fee, cfg.latency, seed)
    }

    pub fn submit(&mut self, o: Order) {
        self.submit_at(o, 0);
    }

    pub fn submit_at(&mut self, o: Order, submitted_ts: u64) {
        let eligible_ts = submitted_ts.saturating_add(self.latency_model.delay_ns());
        self.pending.push(PendingOrder {
            order: o,
            eligible_ts,
        });
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn fills(&self) -> &[Fill] {
        &self.all_fills
    }

    /// 用当前可执行 bar 撮合已进入队列的订单，返回本轮成交。
    pub fn on_bar(&mut self, bar: &Bar, ts: u64) -> Vec<Fill> {
        let pending = std::mem::take(&mut self.pending);
        let mut out = Vec::new();

        for pending_order in pending {
            if pending_order.eligible_ts > ts {
                self.pending.push(pending_order);
                continue;
            }

            // OHLCV 的 volume=0 只能说明当前 bar 没有可观测成交容量。无论具体
            // FillModel 是否读取 halted，都不能在这种 bar 上凭空制造成交。
            if bar.volume <= 0 {
                self.pending.push(pending_order);
                continue;
            }

            let eligible_ts = pending_order.eligible_ts;
            let mut o = pending_order.order;
            let ctx = FillContext {
                side: o.side,
                qty: o.remaining().raw(),
                limit: o.limit.map(|p| p.raw()),
                bar,
                halted: false,
            };

            match self.fill_model.fill(&ctx, &mut self.rng) {
                Some((px, q)) => {
                    let fee_price = px
                        .checked_mul(self.fee_price_multiplier)
                        .and_then(|value| value.checked_div(qx_core::SCALE))
                        .unwrap_or(px);
                    // Bar 数据无法证明限价单是否真实挂在簿上并成为 maker。
                    // 市价单确定是主动成交；限价单标为 Unknown，由费率模型保守处理。
                    let liquidity = if o.limit.is_some() {
                        LiquidityRole::Unknown
                    } else {
                        LiquidityRole::Taker
                    };
                    let fee = self.fee_model.commission_for(&FeeContext {
                        side: o.side,
                        qty: q,
                        price: fee_price,
                        liquidity,
                    });
                    o.filled = Quantity::from_raw(o.filled.raw() + q);
                    o.status = if o.filled.raw() >= o.qty.raw() {
                        OrderStatus::Filled
                    } else {
                        OrderStatus::PartiallyFilled
                    };

                    let f = Fill {
                        order_id: o.client_id,
                        qty: Quantity::from_raw(q),
                        price: Price::from_raw(px),
                        fee: Money::from_raw(fee),
                        ts,
                        account_id: o.account_id.clone(),
                        ..Fill::default()
                    };
                    self.all_fills.push(f.clone());
                    out.push(f);

                    // 部分成交的订单继续挂单。
                    if o.status != OrderStatus::Filled {
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                        });
                    }
                }
                None => {
                    // 未成交：继续挂单（真实排队语义）。
                    self.pending.push(PendingOrder {
                        order: o,
                        eligible_ts,
                    });
                }
            }
        }

        out
    }

    pub fn new_with_latency(
        fill: Box<dyn FillModel>,
        fee: Box<dyn FeeModel>,
        latency: Box<dyn LatencyModel>,
        seed: u64,
    ) -> Self {
        let mut engine = Self::new(fill, fee, seed);
        engine.latency_model = latency;
        engine
    }

    pub fn new_with_latency_and_fee_multiplier(
        fill: Box<dyn FillModel>,
        fee: Box<dyn FeeModel>,
        latency: Box<dyn LatencyModel>,
        seed: u64,
        fee_price_multiplier: i128,
    ) -> Self {
        let mut engine = Self::new_with_latency(fill, fee, latency, seed);
        engine.fee_price_multiplier = fee_price_multiplier.max(1);
        engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Side, SCALE};

    fn order(client_id: u64, limit: Option<Price>) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("TEST.V").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        }
    }

    #[test]
    fn order_fills_on_next_visible_bar_open() {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(crate::cost::ZeroFeeModel),
            1,
        );
        e.submit(order(1, None));
        let bar = Bar::new(20, 100, 110, 90, 105, 1000);
        let fills = e.on_bar(&bar, 20);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price.raw(), 100); // open 而非 close(105)
    }

    #[test]
    fn latency_delays_order_eligibility() {
        let mut e = BarMatchingEngine::new_with_latency(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(crate::cost::ZeroFeeModel),
            Box::new(crate::cost::StaticLatency {
                base_ns: 5,
                insert_ns: 0,
            }),
            1,
        );
        e.submit_at(order(2, None), 0);
        assert!(e.on_bar(&Bar::new(4, 100, 100, 100, 100, 1), 4).is_empty());
        assert_eq!(e.on_bar(&Bar::new(5, 101, 101, 101, 101, 1), 5).len(), 1);
    }

    #[test]
    fn zero_volume_bar_never_creates_a_fill() {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::BestPriceFillModel),
            Box::new(crate::cost::ZeroFeeModel),
            1,
        );
        e.submit(order(3, None));
        assert!(e
            .on_bar(&Bar::new(10, 100, 101, 99, 100, 0), 10)
            .is_empty());
        assert_eq!(e.pending_count(), 1);
        assert_eq!(
            e.on_bar(&Bar::new(11, 101, 102, 100, 101, 1), 11)
                .len(),
            1
        );
    }

    #[test]
    fn bar_limit_order_does_not_get_unproven_maker_discount() {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(crate::cost::MakerTakerFeeModel {
                maker_bp: 1,
                taker_bp: 10,
            }),
            1,
        );
        e.submit(order(4, Some(Price::from_i64(110))));
        let fills = e.on_bar(
            &Bar::new(
                10,
                100 * SCALE,
                101 * SCALE,
                99 * SCALE,
                100 * SCALE,
                SCALE,
            ),
            10,
        );
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fee.raw(), crate::cost::bp_amount(100 * SCALE, 10));
    }
}

#[allow(unused_imports)]
mod qx_xingban_deps {
    pub use qx_core::Side;
}
