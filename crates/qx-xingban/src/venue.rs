//! 仿真撮合引擎（bar 级）。
//!
//! **结构性防作弊**：在 bar t 提交的订单，在 bar t+1 的 open 成交。
//! 这从结构上杜绝了 cheat-on-close（用本根 bar 的收盘价成交本根 bar 的决策）。

use qx_core::{FeeModel, Fill, Money, Order, OrderStatus, Price, Quantity, SCALE};
use qx_guanxing::Bar;

use crate::cost::{LatencyModel, ZeroLatency};
use crate::fill::{FillContext, FillModel};
use crate::rng::DeterministicRng;
use crate::{ExecutionAttempt, ExecutionAttemptReason};

pub struct MatchingConfig {
    pub fill: Box<dyn FillModel>,
    pub fee: Box<dyn FeeModel>,
    pub latency: Box<dyn LatencyModel>,
}

struct PendingOrder {
    order: Order,
    eligible_ts: u64,
    funding_reservation_raw: i128,
}

pub struct BarMatchingEngine {
    pending: Vec<PendingOrder>,
    pending_funding_reservation_sum_raw: Option<u128>,
    fill_model: Box<dyn FillModel>,
    fee_model: Box<dyn FeeModel>,
    latency_model: Box<dyn LatencyModel>,
    /// 费用模型仍接收 qty/price；这里将合约乘数折算到 fee price，避免
    /// contractSize != 1 时手续费仍按现货名义额计算。
    ///
    /// **必须是 SCALE 标度的定点值**（1.0 倍 = `SCALE`）：`fee_price` 由
    /// `price * fee_price_multiplier / SCALE` 得到。传入普通整数 1 会把基准
    /// 缩小 1e9 倍，使小额手续费在整数除法下截断为 0。该乘数与 `contract_size`
    /// 同为定点 raw 口径，因此调用方不需要再次折算。
    fee_price_multiplier: i128,
    /// 反向（币本位）合约的费用基准不是 `qty × price`，而是
    /// `qty × contract_size / price`，与 `TradingInstrumentSpec::notional` 同源。
    inverse_fee_basis: bool,
    rng: DeterministicRng,
    all_fills: Vec<Fill>,
    execution_attempts: Vec<ExecutionAttempt>,
    halted: bool,
    block_buy: bool,
    block_sell: bool,
    volume_limit_bar_ts: Option<u64>,
    volume_used_raw: i128,
}

impl BarMatchingEngine {
    pub fn new(fill: Box<dyn FillModel>, fee: Box<dyn FeeModel>, seed: u64) -> Self {
        Self {
            pending: Vec::new(),
            pending_funding_reservation_sum_raw: Some(0),
            fill_model: fill,
            fee_model: fee,
            latency_model: Box::new(ZeroLatency),
            fee_price_multiplier: SCALE,
            inverse_fee_basis: false,
            rng: DeterministicRng::new(seed),
            all_fills: Vec::new(),
            execution_attempts: Vec::new(),
            halted: false,
            block_buy: false,
            block_sell: false,
            volume_limit_bar_ts: None,
            volume_used_raw: 0,
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
        self.submit_with_funding_reservation_at(o, submitted_ts, 0);
    }

    /// Submit an order while retaining the account buying power reserved by the
    /// pre-trade gate until this order is fully filled or otherwise removed.
    pub(crate) fn submit_with_funding_reservation_at(
        &mut self,
        o: Order,
        submitted_ts: u64,
        funding_reservation_raw: i128,
    ) {
        let eligible_ts = submitted_ts.saturating_add(self.latency_model.delay_ms());
        let funding_reservation_raw = funding_reservation_raw.max(0);
        self.pending_funding_reservation_sum_raw = self
            .pending_funding_reservation_sum_raw
            .and_then(|total| total.checked_add(funding_reservation_raw as u128));
        self.pending.push(PendingOrder {
            order: o,
            eligible_ts,
            funding_reservation_raw,
        });
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Aggregate reservations conservatively; overflow must never create buying
    /// power that the pending orders already consumed.
    pub(crate) fn pending_funding_reservation_raw(&self) -> i128 {
        if let Some(total) = self.pending_funding_reservation_sum_raw {
            return total.min(i128::MAX as u128) as i128;
        }
        self.pending.iter().fold(0_i128, |total, order| {
            total.saturating_add(order.funding_reservation_raw)
        })
    }

    pub(crate) fn pending_funding_reservation_sum_raw_exact(&self) -> Option<u128> {
        self.pending_funding_reservation_sum_raw
    }

    fn adjust_pending_funding_reservation(&mut self, removed_raw: i128, added_raw: i128) {
        let removed = removed_raw.max(0) as u128;
        let added = added_raw.max(0) as u128;
        self.pending_funding_reservation_sum_raw = self
            .pending_funding_reservation_sum_raw
            .and_then(|total| total.checked_sub(removed))
            .and_then(|total| total.checked_add(added));
    }

    fn recompute_pending_funding_reservation_sum(&self) -> Option<u128> {
        self.pending.iter().try_fold(0_u128, |total, order| {
            total.checked_add(order.funding_reservation_raw.max(0) as u128)
        })
    }

    pub fn fills(&self) -> &[Fill] {
        &self.all_fills
    }

    pub fn execution_attempts(&self) -> &[ExecutionAttempt] {
        &self.execution_attempts
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
        if self.volume_limit_bar_ts != Some(ts) {
            self.volume_limit_bar_ts = Some(ts);
            self.volume_used_raw = 0;
        }
        // 停牌/非交易时段的闸门在引擎里，而不在各 FillModel 里：`FillModel` 是
        // 公开 trait，第三方实现忽略 `ctx.halted` 也不能获得成交。整段挂单原序
        // 退回，既不产生成交也不推进随机数流。
        if self.halted {
            let mut still_pending = Vec::with_capacity(pending.len());
            for pending_order in pending {
                if pending_order.eligible_ts <= ts {
                    self.record_attempt(
                        &pending_order.order,
                        ts,
                        0,
                        None,
                        ExecutionAttemptReason::MarketHalted,
                    );
                }
                still_pending.push(pending_order);
            }
            self.pending = still_pending;
            return Vec::new();
        }
        let mut out = Vec::new();

        for pending_order in pending {
            if pending_order.eligible_ts > ts {
                self.pending.push(pending_order);
                continue;
            }
            let eligible_ts = pending_order.eligible_ts;
            let funding_reservation_raw = pending_order.funding_reservation_raw;
            let mut o = pending_order.order;
            if (o.side == qx_core::Side::Buy && self.block_buy)
                || (o.side == qx_core::Side::Sell && self.block_sell)
            {
                self.record_attempt(&o, ts, 0, None, ExecutionAttemptReason::SideBlocked);
                self.pending.push(PendingOrder {
                    order: o,
                    eligible_ts,
                    funding_reservation_raw,
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

            let remaining_bar_volume =
                self.fill_model
                    .bar_volume_participation_bps()
                    .map(|participation_bps| {
                        let capacity = bar_volume_capacity(bar.volume, participation_bps);
                        capacity.saturating_sub(self.volume_used_raw).max(0)
                    });
            if remaining_bar_volume == Some(0) {
                self.record_attempt(
                    &o,
                    ts,
                    0,
                    None,
                    ExecutionAttemptReason::ParticipationLimitExhausted,
                );
                self.pending.push(PendingOrder {
                    order: o,
                    eligible_ts,
                    funding_reservation_raw,
                });
                continue;
            }

            let requested_qty = o.remaining().raw();
            let evaluation = self.fill_model.evaluate(&ctx, &mut self.rng);
            match evaluation.fill {
                Some((px, q)) => {
                    // 成交量以订单剩余量为上限、成交价必须为正：`FillModel` 是公开
                    // trait，而现货记账路径（`apply_fill_with_multiplier`）既不校验
                    // 超额成交也不校验负价，滑点模型在低价标的上就能报出负的成交价。
                    let q = remaining_bar_volume
                        .map_or(q, |remaining| q.min(remaining))
                        .min(requested_qty);
                    if q <= 0 || px <= 0 {
                        self.record_attempt(
                            &o,
                            ts,
                            0,
                            Some(px),
                            ExecutionAttemptReason::InvalidModelOutput,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw,
                        });
                        continue;
                    }
                    if self.fill_model.bar_volume_participation_bps().is_some() {
                        self.volume_used_raw = self.volume_used_raw.saturating_add(q);
                    }
                    let remaining_before_fill = o.remaining().raw();
                    let attempt_reason = if q < requested_qty {
                        if remaining_bar_volume.is_some_and(|remaining| remaining < requested_qty) {
                            ExecutionAttemptReason::BarParticipationPartial
                        } else {
                            ExecutionAttemptReason::ModelPartialFill
                        }
                    } else {
                        ExecutionAttemptReason::Filled
                    };
                    self.record_attempt(&o, ts, q, Some(px), attempt_reason);
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
                        // Release only the proportional share of the reservation.
                        // If intermediate multiplication overflows, release zero:
                        // that is conservative and cannot manufacture buying power.
                        let consumed_reservation = funding_reservation_raw
                            .checked_mul(q)
                            .and_then(|value| value.checked_div(remaining_before_fill))
                            .unwrap_or(0)
                            .clamp(0, funding_reservation_raw);
                        let remaining_reservation =
                            funding_reservation_raw.saturating_sub(consumed_reservation);
                        self.adjust_pending_funding_reservation(
                            funding_reservation_raw,
                            remaining_reservation,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw: remaining_reservation,
                        });
                    } else {
                        self.adjust_pending_funding_reservation(funding_reservation_raw, 0);
                    }
                }
                None => {
                    // 未成交：继续挂单（真实排队语义）
                    self.record_attempt(&o, ts, 0, None, evaluation.no_fill_reason);
                    self.pending.push(PendingOrder {
                        order: o,
                        eligible_ts,
                        funding_reservation_raw,
                    });
                }
            }
        }

        if self.pending_funding_reservation_sum_raw.is_none() {
            self.pending_funding_reservation_sum_raw =
                self.recompute_pending_funding_reservation_sum();
        }
        out
    }

    fn record_attempt(
        &mut self,
        order: &Order,
        ts: u64,
        filled_qty_raw: i128,
        price_raw: Option<i128>,
        reason: ExecutionAttemptReason,
    ) {
        let requested_qty_raw = order.remaining().raw();
        self.execution_attempts.push(ExecutionAttempt {
            ts,
            order_id: order.client_id,
            instrument: order.instrument.to_string(),
            side: order.side,
            requested_qty_raw,
            filled_qty_raw,
            remaining_qty_raw: requested_qty_raw.saturating_sub(filled_qty_raw),
            price_raw,
            reason,
            displayed_qty_raw: None,
            queue_ahead_before_raw: None,
            queue_ahead_after_raw: None,
            estimated_volume_raw: None,
            displayed_reduction_raw: None,
            queue_consumed_raw: None,
        });
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

/// Compute a fixed-point volume allowance without multiplying the full raw volume by bps.
/// The quotient/remainder form cannot overflow for a validated 0..=10,000 bps input.
fn bar_volume_capacity(volume_raw: i128, participation_bps: u16) -> i128 {
    if volume_raw <= 0 || participation_bps == 0 {
        return 0;
    }
    let bps = i128::from(participation_bps.min(10_000));
    (volume_raw / 10_000) * bps + ((volume_raw % 10_000) * bps) / 10_000
}

#[cfg(test)]
#[path = "venue/tests.rs"]
mod tests;
