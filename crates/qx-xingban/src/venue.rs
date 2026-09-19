//! 仿真撮合引擎（bar 级）。
//!
//! **结构性防作弊**：在 bar t 提交的订单，在 bar t+1 的 open 成交。
//! 这从结构上杜绝了 cheat-on-close（用本根 bar 的收盘价成交本根 bar 的决策）。

use qx_core::{Fill, Money, Order, OrderStatus, Price, Quantity};
use qx_guanxing::Bar;

use crate::cost::{FeeModel, LatencyModel, ZeroLatency};
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
    /// contractSize != 1 时手续费仍按现货名义额计算。该乘数与 `contract_size`
    /// 同为定点 raw 口径（`SCALE` 表示 1.0），因此 1 会把费用压到 1e-9 倍。
    fee_price_multiplier: i128,
    rng: DeterministicRng,
    all_fills: Vec<Fill>,
    halted: bool,
    block_buy: bool,
    block_sell: bool,
}

impl BarMatchingEngine {
    pub fn new(fill: Box<dyn FillModel>, fee: Box<dyn FeeModel>, seed: u64) -> Self {
        Self {
            pending: Vec::new(),
            fill_model: fill,
            fee_model: fee,
            latency_model: Box::new(ZeroLatency),
            fee_price_multiplier: qx_core::SCALE,
            rng: DeterministicRng::new(seed),
            all_fills: Vec::new(),
            halted: false,
            block_buy: false,
            block_sell: false,
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

    pub fn set_halted(&mut self, halted: bool) {
        self.halted = halted;
    }

    pub fn set_side_blocks(&mut self, block_buy: bool, block_sell: bool) {
        self.block_buy = block_buy;
        self.block_sell = block_sell;
    }

    /// 用本根 bar 撮合上一根 bar 提交的挂单，返回本轮成交。
    pub fn on_bar(&mut self, bar: &Bar, ts: u64) -> Vec<Fill> {
        let pending = std::mem::take(&mut self.pending);
        let mut out = Vec::new();

        for pending_order in pending {
            if pending_order.eligible_ts > ts {
                self.pending.push(pending_order);
                continue;
            }
            let eligible_ts = pending_order.eligible_ts;
            let mut o = pending_order.order;
            if (o.side == qx_core::Side::Buy && self.block_buy)
                || (o.side == qx_core::Side::Sell && self.block_sell)
            {
                self.pending.push(PendingOrder {
                    order: o,
                    eligible_ts,
                });
                continue;
            }
            let ctx = FillContext {
                side: o.side,
                qty: o.remaining().raw(),
                limit: o.limit.map(|p| p.raw()),
                bar,
                halted: self.halted,
            };

            match self.fill_model.fill(&ctx, &mut self.rng) {
                Some((px, q)) => {
                    let fee_price = px
                        .checked_mul(self.fee_price_multiplier)
                        .and_then(|value| value.checked_div(qx_core::SCALE))
                        .unwrap_or(px);
                    let fee =
                        self.fee_model
                            .commission_for_side(q, fee_price, o.side, o.limit.is_some());
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

                    // 部分成交的订单继续挂单
                    if o.status != OrderStatus::Filled {
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                        });
                    }
                }
                None => {
                    // 未成交：继续挂单（真实排队语义）
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
    use qx_core::InstrumentId;

    #[test]
    fn order_fills_on_next_bar_open() {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(crate::cost::ZeroFeeModel),
            1,
        );
        let o = Order {
            client_id: 1,
            instrument: InstrumentId::parse("TEST.V").unwrap(),
            side: qx_core::Side::Buy,
            qty: Quantity::from_i64(10),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        };
        e.submit(o);
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
        let o = Order {
            client_id: 2,
            instrument: InstrumentId::parse("TEST.V").unwrap(),
            side: qx_core::Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        };
        e.submit_at(o, 0);
        assert!(e.on_bar(&Bar::new(4, 100, 100, 100, 100, 1), 4).is_empty());
        assert_eq!(e.on_bar(&Bar::new(5, 101, 101, 101, 101, 1), 5).len(), 1);
    }
}

#[allow(unused_imports)]
mod qx_xingban_deps {
    pub use qx_core::Side;
}
