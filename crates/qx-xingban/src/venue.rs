//! 仿真撮合引擎（bar 级）。
//!
//! **结构性防作弊**：在 bar t 提交的订单，在 bar t+1 的 open 成交。
//! 这从结构上杜绝了 cheat-on-close（用本根 bar 的收盘价成交本根 bar 的决策）。

use qx_core::{FeeModel, Fill, Money, Order, OrderStatus, Price, Quantity, SCALE};
use qx_guanxing::Bar;

use crate::cost::{LatencyModel, ZeroLatency};
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
    ///
    /// **必须是 SCALE 标度的定点值**（1.0 倍 = `SCALE`）：`fee_price` 由
    /// `price * fee_price_multiplier / SCALE` 得到。传入普通整数 1 会把基准
    /// 缩小 1e9 倍，使小额手续费在整数除法下截断为 0。
    fee_price_multiplier: i128,
    /// 反向（币本位）合约的费用基准不是 `qty × price`，而是
    /// `qty × contract_size / price`，与 `TradingInstrumentSpec::notional` 同源。
    inverse_fee_basis: bool,
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
            fee_price_multiplier: SCALE,
            inverse_fee_basis: false,
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
        // 停牌/非交易时段的闸门在引擎里，而不在各 FillModel 里：`FillModel` 是
        // 公开 trait，第三方实现忽略 `ctx.halted` 也不能获得成交。整段挂单原序
        // 退回，既不产生成交也不推进随机数流。
        if self.halted {
            self.pending = pending;
            return Vec::new();
        }
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
                    // 成交量以订单剩余量为上限、成交价必须为正：`FillModel` 是公开
                    // trait，而现货记账路径（`apply_fill_with_multiplier`）既不校验
                    // 超额成交也不校验负价，滑点模型在低价标的上就能报出负的成交价。
                    let q = q.min(o.remaining().raw());
                    if q <= 0 || px <= 0 {
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                        });
                        continue;
                    }
                    // maker/taker 由订单意图决定，而非是否有限价：本引擎的 FillModel
                    // 只在成交价已可成交（limit_ok 通过）时返回成交，即限价单此刻是吃单方。
                    // 只有显式 post_only 的挂单才是 maker。
                    let fee = self.estimate_fee(q, px, &o);
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

    /// 用本引擎自己的费率模型、合约乘数折算和 maker/taker 判定估计一笔成交的费用。
    /// 前置资金门禁必须与真实计费同源，否则"校验时以为免费、成交时才扣款"会让
    /// 现金充足的判定失真。`price` 传未折算的行情价。
    pub fn estimate_fee(&self, qty: i128, price: i128, order: &Order) -> i128 {
        let is_maker = order.policy.as_ref().is_some_and(|policy| policy.post_only);
        self.fee_model
            .commission_for_side(qty, self.fee_price(price), order.side, is_maker)
    }

    /// 把行情价折算成费用模型看到的价格。费用模型按 `qty × price / SCALE` 计基，
    /// 所以线性合约用 `price × contract_size`，反向（币本位）合约用
    /// `contract_size × SCALE / price`——后者与 `TradingInstrumentSpec::notional`
    /// 的 `qty × contract_size / price` 口径一致，误用线性口径会让费用偏差 price² 量级。
    fn fee_price(&self, price: i128) -> i128 {
        let basis = if self.inverse_fee_basis {
            self.fee_price_multiplier
                .checked_mul(SCALE)
                .and_then(|value| value.checked_div(price))
        } else {
            price
                .checked_mul(self.fee_price_multiplier)
                .and_then(|value| value.checked_div(SCALE))
        };
        // 溢出只可能出现在非法尺度的输入上（定点价与乘数均为正数）；退化到未折算的
        // 行情价，与历史行为一致，绝不 panic。
        basis.unwrap_or(price)
    }

    /// 反向（币本位）结算的合约必须切换费用基准；现货与线性合约保持默认。
    pub fn set_inverse_fee_basis(&mut self, inverse: bool) {
        self.inverse_fee_basis = inverse;
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

    /// `fee_price_multiplier` 必须是 SCALE 标度的定点值：1.0 倍传 `SCALE`，
    /// 0.001 倍传 `SCALE / 1000`。`new_with_latency` 的默认即 `SCALE`。
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
            Box::new(qx_core::ZeroFeeModel),
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

    /// 真实尺度下手续费不得截断为 0：10 单位 @100.0 = 1000.0 名义，taker 5bp = 0.5。
    /// 回归 `fee_price_multiplier` 被当作普通整数传入、导致基准缩小 1e9 的缺陷。
    #[test]
    fn default_fee_multiplier_charges_real_scale_fees() {
        let mut e = BarMatchingEngine::new(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(qx_core::MakerTakerFeeModel {
                maker_bp: 2,
                taker_bp: 5,
            }),
            1,
        );
        let o = Order {
            client_id: 3,
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
        let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
        let fills = e.on_bar(&bar, 20);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fee.raw(), SCALE / 2, "taker 5bp on 1000 notional");
    }

    /// 小数合约乘数（0.001 张）在定点标度下仍须计费。
    #[test]
    fn fractional_fee_multiplier_stays_in_fixed_scale() {
        let mut e = BarMatchingEngine::new_with_latency_and_fee_multiplier(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(qx_core::MakerTakerFeeModel {
                maker_bp: 0,
                taker_bp: 5,
            }),
            Box::new(crate::cost::ZeroLatency),
            1,
            SCALE / 1000,
        );
        let o = Order {
            client_id: 4,
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
        let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
        let fills = e.on_bar(&bar, 20);
        assert_eq!(fills.len(), 1);
        // 0.001 张/合约 → 名义 1.0 → 5bp = 0.0005
        assert_eq!(fills[0].fee.raw(), SCALE / 2000);
    }

    fn market_order(client_id: u64) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("TEST.V").unwrap(),
            side: qx_core::Side::Buy,
            qty: Quantity::from_i64(10),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        }
    }

    fn limit_order(client_id: u64, post_only: bool) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("TEST.V").unwrap(),
            side: qx_core::Side::Buy,
            qty: Quantity::from_i64(10),
            // 限价高于开盘价：本引擎判定为可成交，因此普通限价单是吃单方。
            limit: Some(Price::from_i64(101)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: post_only.then(|| qx_core::OrderPolicy {
                post_only: true,
                ..Default::default()
            }),
        }
    }

    /// maker 费率只认 post_only 意图；带限价的吃单成交必须按 taker 计费。
    #[test]
    fn limit_order_is_taker_unless_post_only() {
        let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
        let fees = |post_only: bool| {
            let mut e = BarMatchingEngine::new(
                Box::new(crate::fill::NextBarOpenFillModel),
                Box::new(qx_core::MakerTakerFeeModel {
                    maker_bp: 2,
                    taker_bp: 5,
                }),
                1,
            );
            e.submit(limit_order(if post_only { 6 } else { 5 }, post_only));
            e.on_bar(&bar, 20)[0].fee.raw()
        };
        // 名义 1000.0：taker 5bp = 0.5，maker 2bp = 0.2
        assert_eq!(fees(false), SCALE / 2, "marketable limit pays taker");
        assert_eq!(fees(true), SCALE / 5, "post_only pays maker");
    }

    /// A 股三件套必须经撮合引擎按真实尺度计费：佣金有下限、印花税与过户费无下限，
    /// 因此费用基准被错误缩小会让后两者归零、前者退化为固定 5 元。
    #[test]
    fn ashare_fees_charge_commission_stamp_and_transfer() {
        let model = || qx_core::AShareFeeModel {
            commission_bp: 3,
            min_commission: 5 * SCALE,
            stamp_duty_bp: 5,
            transfer_fee_bp: 1,
        };
        let px = 10 * SCALE;
        let bar = Bar::new(20, px, px, px, px, 1_000);
        let charge = |side: qx_core::Side, id: u64| {
            let mut e = BarMatchingEngine::new(
                Box::new(crate::fill::NextBarOpenFillModel),
                Box::new(model()),
                1,
            );
            e.submit(Order {
                client_id: id,
                instrument: InstrumentId::parse("600000.SSE").unwrap(),
                side,
                qty: Quantity::from_i64(100),
                limit: None,
                status: OrderStatus::Submitted,
                filled: Quantity::ZERO,
                account_id: "a".into(),
                trace: None,
                policy: None,
            });
            e.on_bar(&bar, 20)[0].fee.raw()
        };
        // 100 股 @10.0 = 1000 元：佣金 3bp=0.3 → 取 5 元下限，过户费 1bp=0.1
        assert_eq!(charge(qx_core::Side::Buy, 7), 51 * SCALE / 10);
        // 卖出另征印花税 5bp=0.5
        assert_eq!(charge(qx_core::Side::Sell, 8), 56 * SCALE / 10);
    }

    #[test]
    fn latency_delays_order_eligibility() {
        let mut e = BarMatchingEngine::new_with_latency(
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(qx_core::ZeroFeeModel),
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

    /// 停牌闸门位于引擎，而不是各 `FillModel`：除 `NextBarOpen` 外的模型都只读
    /// `ctx.bar`，忽略 `ctx.halted` 的第三方实现同样不得在停牌期间成交。
    #[test]
    fn halted_blocks_every_fill_model() {
        let models: Vec<Box<dyn FillModel>> = vec![
            Box::new(crate::fill::NextBarOpenFillModel),
            Box::new(crate::fill::BestPriceFillModel),
            Box::new(crate::fill::OneTickSlippageFillModel { tick: 1 }),
            Box::new(crate::fill::ProbabilisticFillModel {
                prob_fill_on_limit: SCALE,
                tick: 1,
            }),
            Box::new(crate::fill::VolumeSensitiveFillModel { frac_bp: 1_000 }),
        ];
        for model in models {
            let model_name = model.name();
            let mut e = BarMatchingEngine::new(model, Box::new(qx_core::ZeroFeeModel), 1);
            e.submit(market_order(20));
            e.set_halted(true);
            let bar = Bar::new(20, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 1_000);
            assert!(
                e.on_bar(&bar, 20).is_empty(),
                "{model_name} 不应在停牌期间成交"
            );
            assert_eq!(e.pending_count(), 1, "停牌只推迟成交，不得丢弃挂单");
        }
    }

    /// 越界 `FillModel` 的成交量与价格由引擎兜住：现货记账路径不校验超额成交，
    /// 而滑点模型在低价标的上可以报出非正成交价。
    #[test]
    fn out_of_bounds_fill_model_is_clamped_by_engine() {
        struct OverFill;
        impl FillModel for OverFill {
            fn name(&self) -> &'static str {
                "OverFill"
            }
            fn tier(&self) -> crate::fill::DataTier {
                crate::fill::DataTier::Bar
            }
            fn assumption(&self) -> &'static str {
                "测试用：报出两倍于剩余量的成交"
            }
            fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
                Some((ctx.bar.close, ctx.qty * 2))
            }
        }
        let mut e = BarMatchingEngine::new(Box::new(OverFill), Box::new(qx_core::ZeroFeeModel), 1);
        e.submit(market_order(21));
        let fills = e.on_bar(&Bar::new(20, 100, 100, 100, 100, 1), 20);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].qty.raw(), Quantity::from_i64(10).raw());
        assert_eq!(e.pending_count(), 0, "夹到剩余量后订单应已全成");

        struct BadPrice;
        impl FillModel for BadPrice {
            fn name(&self) -> &'static str {
                "BadPrice"
            }
            fn tier(&self) -> crate::fill::DataTier {
                crate::fill::DataTier::Bar
            }
            fn assumption(&self) -> &'static str {
                "测试用：报出负成交价"
            }
            fn fill(&self, ctx: &FillContext, _rng: &mut DeterministicRng) -> Option<(i128, i128)> {
                Some((-1, ctx.qty))
            }
        }
        let mut e = BarMatchingEngine::new(Box::new(BadPrice), Box::new(qx_core::ZeroFeeModel), 1);
        e.submit(market_order(22));
        assert!(e
            .on_bar(&Bar::new(20, 100, 100, 100, 100, 1), 20)
            .is_empty());
        assert_eq!(e.pending_count(), 1);
    }

    /// 反向（币本位）合约的费用基准是 `qty × contract_size / price`：与线性口径
    /// 相差 price² 量级，且必须与账本 `TradingInstrumentSpec::notional` 同源。
    #[test]
    fn inverse_contract_uses_coin_notional_for_fees() {
        let price = 20_000_i128 * SCALE;
        let qty = SCALE; // 1 张
        let contract_size = SCALE; // 1 张 = 1 币
        let bar = Bar::new(20, price, price, price, price, 1_000);
        let engine = |inverse: bool| {
            let mut e = BarMatchingEngine::new_with_latency_and_fee_multiplier(
                Box::new(crate::fill::NextBarOpenFillModel),
                Box::new(qx_core::MakerTakerFeeModel {
                    maker_bp: 0,
                    taker_bp: 5,
                }),
                Box::new(crate::cost::ZeroLatency),
                1,
                contract_size,
            );
            e.set_inverse_fee_basis(inverse);
            let mut one_contract = market_order(23);
            one_contract.qty = Quantity::from_raw(qty);
            e.submit(one_contract);
            e.on_bar(&bar, 20)[0].fee.raw()
        };
        // 反向：名义 1 × 1 / 20000 币 = 5e-5，5bp = 2.5e-8 → 定点原始值 25。
        assert_eq!(engine(true), 25);
        // 线性：名义 1 × 20000 = 2e4，5bp = 10 → 定点原始值 10 × SCALE。
        assert_eq!(engine(false), 10 * SCALE);
    }
}
