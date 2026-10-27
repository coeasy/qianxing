//! 仿真撮合引擎（bar 级）。
//!
//! **结构性防作弊**：在 bar t 提交的订单，在 bar t+1 的 open 成交。
//! 这从结构上杜绝了 cheat-on-close（用本根 bar 的收盘价成交本根 bar 的决策）。

use qx_core::{
    DecimalRounding, ExactDecimal, ExactFillAmounts, ExactFillFee, FeeModel, Fill, Money, Order,
    OrderStatus, Price, Quantity, TradingInstrumentSpec, SCALE,
};
use qx_guanxing::Bar;

use crate::cost::{LatencyModel, ZeroLatency};
use crate::fill::{ExactFillContext, FillContext, FillModel};
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
    /// Currency of the fee model's output; exact fills must carry this fact into the ledger.
    exact_fee_currency: Option<String>,
    /// Frozen market rules used for quote-budget sizing and lot-safe partial fills.
    instrument_spec: Option<TradingInstrumentSpec>,
    rng: DeterministicRng,
    all_fills: Vec<Fill>,
    execution_attempts: Vec<ExecutionAttempt>,
    halted: bool,
    block_buy: bool,
    block_sell: bool,
    volume_limit_bar_ts: Option<u64>,
    volume_used_exact: ExactDecimal,
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
            exact_fee_currency: None,
            instrument_spec: None,
            rng: DeterministicRng::new(seed),
            all_fills: Vec::new(),
            execution_attempts: Vec::new(),
            halted: false,
            block_buy: false,
            block_sell: false,
            volume_limit_bar_ts: None,
            volume_used_exact: ExactDecimal::ZERO,
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
        let _ = self.try_submit_at(o, submitted_ts);
    }

    pub fn try_submit_at(&mut self, o: Order, submitted_ts: u64) -> Result<(), String> {
        self.submit_with_funding_reservation_at(o, submitted_ts, 0)
    }

    /// Submit an order while retaining the account buying power reserved by the
    /// pre-trade gate until this order is fully filled or otherwise removed.
    pub(crate) fn submit_with_funding_reservation_at(
        &mut self,
        o: Order,
        submitted_ts: u64,
        funding_reservation_raw: i128,
    ) -> Result<(), String> {
        o.validate()?;
        if let Some(amounts) = o.exact_amounts {
            let remaining = if let Some(budget) = amounts.quote_budget {
                let spec = self.instrument_spec.as_ref().ok_or_else(|| {
                    "Bar quote_budget 撮合缺少冻结 TradingInstrumentSpec".to_string()
                })?;
                if spec.instrument != o.instrument {
                    return Err("Bar quote_budget 撮合规格与订单标的不一致".into());
                }
                spec.validate_exact_quote_budget(budget)
                    .map_err(|error| error.to_string())?;
                if !self.fill_model.supports_exact_amounts() {
                    return Err(format!(
                        "Bar quote_budget 订单要求 exact FillModel，当前模型 {} 未声明支持",
                        self.fill_model.name()
                    ));
                }
                // The actual base amount depends on the fill price and frozen lot step.
                // A positive probe only validates that the model accepts this exact path.
                ExactDecimal::ONE
            } else {
                o.remaining_decimal()
                    .ok_or_else(|| "精确订单剩余数量计算溢出".to_string())?
            };
            if !remaining.is_positive() {
                return Err("精确 Bar 订单没有剩余可成交数量".into());
            }
            let probe_bar = qx_guanxing::Bar::new(1, 1, 1, 1, 1, 0);
            let context = ExactFillContext {
                side: o.side,
                qty: remaining,
                limit: o.limit_price_decimal(),
                bar: &probe_bar,
                halted: false,
            };
            if !self.fill_model.can_evaluate_exact(&context) {
                return Err(format!(
                    "Bar 撮合模型 {} 不支持该精确数量精度",
                    self.fill_model.name()
                ));
            }
        } else {
            o.require_legacy_amounts()?;
        }
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
        Ok(())
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
            self.volume_used_exact = ExactDecimal::ZERO;
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
            let exact_order = o.exact_amounts.is_some();
            let remaining_quote_budget = o.remaining_budget_decimal();
            let requested_qty = if let Some(budget) = remaining_quote_budget {
                let Some(spec) = self.instrument_spec.as_ref() else {
                    self.record_attempt(
                        &o,
                        ts,
                        0,
                        None,
                        ExecutionAttemptReason::InvalidModelOutput,
                    );
                    self.pending.push(PendingOrder {
                        order: o,
                        eligible_ts,
                        funding_reservation_raw,
                    });
                    continue;
                };
                match spec.base_quantity_for_quote_budget(budget, bar.low_exact()) {
                    Ok(quantity) if quantity.is_positive() => quantity,
                    _ => {
                        self.record_attempt(
                            &o,
                            ts,
                            0,
                            None,
                            ExecutionAttemptReason::InvalidModelOutput,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw,
                        });
                        continue;
                    }
                }
            } else {
                o.remaining_decimal()
                    .unwrap_or_else(|| raw_decimal(o.remaining().raw()))
            };
            let exact_ctx = ExactFillContext {
                side: o.side,
                qty: requested_qty,
                limit: o.limit_price_decimal(),
                bar,
                halted: self.halted,
            };
            let legacy_ctx = FillContext {
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
                        exact_bar_volume_capacity(bar.volume_exact(), participation_bps)
                            .and_then(|capacity| capacity.checked_sub(self.volume_used_exact))
                            .filter(|capacity| capacity.is_positive())
                            .unwrap_or(ExactDecimal::ZERO)
                    });
            if remaining_bar_volume.is_some_and(|remaining| !remaining.is_positive()) {
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

            let (model_fill, no_fill_reason) = if exact_order {
                let evaluation = self.fill_model.evaluate_exact(&exact_ctx, &mut self.rng);
                (evaluation.fill, evaluation.no_fill_reason)
            } else {
                let evaluation = self.fill_model.evaluate(&legacy_ctx, &mut self.rng);
                (
                    evaluation
                        .fill
                        .map(|(price, qty)| (raw_decimal(price), raw_decimal(qty))),
                    evaluation.no_fill_reason,
                )
            };
            match model_fill {
                Some((price, model_qty)) => {
                    let fill_target_qty = if let Some(budget) = remaining_quote_budget {
                        self.instrument_spec
                            .as_ref()
                            .and_then(|spec| {
                                spec.base_quantity_for_quote_budget(budget, price).ok()
                            })
                            .unwrap_or(ExactDecimal::ZERO)
                    } else {
                        requested_qty
                    };
                    if !fill_target_qty.is_positive() {
                        self.record_attempt(
                            &o,
                            ts,
                            0,
                            price.to_raw(9).ok(),
                            ExecutionAttemptReason::InvalidModelOutput,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw,
                        });
                        continue;
                    }
                    let mut qty = if exact_order {
                        model_qty
                    } else {
                        // Legacy orders retain their historical 1e-9 volume grid.
                        floor_to_legacy_decimal(model_qty).unwrap_or(ExactDecimal::ZERO)
                    };
                    if qty
                        .checked_cmp(fill_target_qty)
                        .is_some_and(|ordering| ordering.is_gt())
                    {
                        qty = fill_target_qty;
                    }
                    if let Some(capacity) = remaining_bar_volume {
                        let capacity = if exact_order {
                            capacity
                        } else {
                            floor_to_legacy_decimal(capacity).unwrap_or(ExactDecimal::ZERO)
                        };
                        if qty
                            .checked_cmp(capacity)
                            .is_some_and(|ordering| ordering.is_gt())
                        {
                            qty = capacity;
                        }
                    }
                    if qty
                        .checked_cmp(requested_qty)
                        .is_some_and(|ordering| ordering.is_gt())
                    {
                        qty = requested_qty;
                    }
                    if let Some(spec) = self.instrument_spec.as_ref().filter(|_| exact_order) {
                        match spec.floor_exact_quantity_to_step(qty) {
                            Ok(quantity) => qty = quantity,
                            Err(_) => qty = ExactDecimal::ZERO,
                        }
                    }
                    let limit_valid = match (o.side, o.limit_price_decimal()) {
                        (_, None) => true,
                        (qx_core::Side::Buy, Some(limit)) => price
                            .checked_cmp(limit)
                            .is_some_and(|ordering| !ordering.is_gt()),
                        (qx_core::Side::Sell, Some(limit)) => price
                            .checked_cmp(limit)
                            .is_some_and(|ordering| !ordering.is_lt()),
                    };
                    if !qty.is_positive() || !price.is_positive() || !limit_valid {
                        self.record_attempt(
                            &o,
                            ts,
                            0,
                            price.to_raw(9).ok(),
                            ExecutionAttemptReason::InvalidModelOutput,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw,
                        });
                        continue;
                    }
                    let quote_cost = if let Some(remaining_budget) = remaining_quote_budget {
                        self.instrument_spec
                            .as_ref()
                            .and_then(|spec| {
                                spec.quote_cash_exact(qty, price, DecimalRounding::AwayFromZero)
                                    .ok()
                            })
                            .filter(|cost| {
                                cost.is_positive()
                                    && cost
                                        .checked_cmp(remaining_budget)
                                        .is_some_and(|ordering| !ordering.is_gt())
                            })
                    } else {
                        None
                    };
                    if remaining_quote_budget.is_some() && quote_cost.is_none() {
                        self.record_attempt(
                            &o,
                            ts,
                            0,
                            price.to_raw(9).ok(),
                            ExecutionAttemptReason::InvalidModelOutput,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw,
                        });
                        continue;
                    }
                    let used_next = if self.fill_model.bar_volume_participation_bps().is_some() {
                        self.volume_used_exact.checked_add(qty)
                    } else {
                        Some(self.volume_used_exact)
                    };
                    let Some(used_next) = used_next else {
                        self.record_attempt(
                            &o,
                            ts,
                            0,
                            price.to_raw(9).ok(),
                            ExecutionAttemptReason::InvalidModelOutput,
                        );
                        self.pending.push(PendingOrder {
                            order: o,
                            eligible_ts,
                            funding_reservation_raw,
                        });
                        continue;
                    };
                    let filled_raw = qty.to_raw(9).ok().unwrap_or(0);
                    let price_raw = price.to_raw(9).ok();
                    let was_capacity_limited = remaining_bar_volume.is_some_and(|capacity| {
                        capacity
                            .checked_cmp(model_qty)
                            .is_some_and(|ordering| ordering.is_lt())
                            && capacity
                                .checked_cmp(fill_target_qty)
                                .is_some_and(|ordering| ordering.is_lt())
                    });
                    let attempt_reason = if qty
                        .checked_cmp(fill_target_qty)
                        .is_some_and(|ordering| ordering.is_lt())
                    {
                        if was_capacity_limited {
                            ExecutionAttemptReason::BarParticipationPartial
                        } else {
                            ExecutionAttemptReason::ModelPartialFill
                        }
                    } else {
                        ExecutionAttemptReason::Filled
                    };
                    let remaining_before_fill = fill_target_qty;
                    let quote_budget_complete = remaining_quote_budget.is_some()
                        && !qty
                            .checked_cmp(remaining_before_fill)
                            .is_some_and(|ordering| ordering.is_lt());
                    let (fee_raw, exact_fees) = if exact_order {
                        match self.estimate_fee_exact(qty, price, &o) {
                            Ok(fee) => (
                                0,
                                if fee.is_positive() {
                                    vec![ExactFillFee {
                                        amount: fee,
                                        currency: self.exact_fee_currency.clone(),
                                        source_trade_ids: Vec::new(),
                                    }]
                                } else {
                                    Vec::new()
                                },
                            ),
                            Err(_) => {
                                self.record_attempt(
                                    &o,
                                    ts,
                                    0,
                                    price_raw,
                                    ExecutionAttemptReason::InvalidModelOutput,
                                );
                                self.pending.push(PendingOrder {
                                    order: o,
                                    eligible_ts,
                                    funding_reservation_raw,
                                });
                                continue;
                            }
                        }
                    } else {
                        let (Some(qty_raw), Some(price_raw)) =
                            (qty.to_raw(9).ok(), price.to_raw(9).ok())
                        else {
                            self.record_attempt(
                                &o,
                                ts,
                                0,
                                price_raw,
                                ExecutionAttemptReason::InvalidModelOutput,
                            );
                            self.pending.push(PendingOrder {
                                order: o,
                                eligible_ts,
                                funding_reservation_raw,
                            });
                            continue;
                        };
                        match self.estimate_fee(qty_raw, price_raw, &o) {
                            Ok(fee) => (fee, Vec::new()),
                            Err(_) => {
                                self.record_attempt(
                                    &o,
                                    ts,
                                    0,
                                    Some(price_raw),
                                    ExecutionAttemptReason::InvalidModelOutput,
                                );
                                self.pending.push(PendingOrder {
                                    order: o,
                                    eligible_ts,
                                    funding_reservation_raw,
                                });
                                continue;
                            }
                        }
                    };

                    if exact_order {
                        let Some(amounts) = o.exact_amounts.as_mut() else {
                            unreachable!("exact_order checked above")
                        };
                        let Some(filled) = amounts.filled.checked_add(qty) else {
                            self.record_attempt(
                                &o,
                                ts,
                                0,
                                price_raw,
                                ExecutionAttemptReason::InvalidModelOutput,
                            );
                            self.pending.push(PendingOrder {
                                order: o,
                                eligible_ts,
                                funding_reservation_raw,
                            });
                            continue;
                        };
                        let quote_budget_order = amounts.quote_budget.is_some();
                        let next_quote_filled = match (amounts.quote_budget, quote_cost) {
                            (Some(_), Some(cost)) => amounts.quote_filled.checked_add(cost),
                            (Some(_), None) => None,
                            (None, _) => Some(amounts.quote_filled),
                        };
                        let Some(next_quote_filled) = next_quote_filled else {
                            self.record_attempt(
                                &o,
                                ts,
                                0,
                                price_raw,
                                ExecutionAttemptReason::InvalidModelOutput,
                            );
                            self.pending.push(PendingOrder {
                                order: o,
                                eligible_ts,
                                funding_reservation_raw,
                            });
                            continue;
                        };
                        amounts.filled = filled;
                        amounts.quote_filled = next_quote_filled;
                        let complete = if quote_budget_order {
                            quote_budget_complete
                        } else {
                            filled
                                .checked_cmp(amounts.quantity)
                                .is_some_and(|ordering| !ordering.is_lt())
                        };
                        o.status = if complete {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        };
                    } else {
                        let Some(qty_raw) = qty.to_raw(9).ok() else {
                            self.record_attempt(
                                &o,
                                ts,
                                0,
                                price_raw,
                                ExecutionAttemptReason::InvalidModelOutput,
                            );
                            self.pending.push(PendingOrder {
                                order: o,
                                eligible_ts,
                                funding_reservation_raw,
                            });
                            continue;
                        };
                        o.filled = Quantity::from_raw(o.filled.raw().saturating_add(qty_raw));
                        o.status = if o.filled.raw() >= o.qty.raw() {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        };
                    }

                    if self.fill_model.bar_volume_participation_bps().is_some() {
                        self.volume_used_exact = used_next;
                    }
                    if exact_order {
                        self.record_exact_attempt(
                            &o,
                            ts,
                            remaining_before_fill,
                            qty,
                            Some(price),
                            attempt_reason,
                        );
                    } else {
                        self.record_attempt(&o, ts, filled_raw, price_raw, attempt_reason);
                    }
                    if let Some(attempt) = self.execution_attempts.last_mut() {
                        let requested_raw = remaining_before_fill.to_raw(9).unwrap_or(0);
                        attempt.requested_qty_raw = requested_raw;
                        attempt.filled_qty_raw = filled_raw;
                        attempt.remaining_qty_raw = requested_raw.saturating_sub(filled_raw);
                        if exact_order {
                            attempt.requested_qty_exact = Some(remaining_before_fill);
                            attempt.filled_qty_exact = Some(qty);
                            attempt.remaining_qty_exact = remaining_before_fill.checked_sub(qty);
                            attempt.price_exact = Some(price);
                        }
                    }

                    let fill = if exact_order {
                        Fill {
                            order_id: o.client_id,
                            fee: Money::from_raw(0),
                            ts,
                            account_id: o.account_id.clone(),
                            exact_fees,
                            exact_amounts: Some(ExactFillAmounts {
                                quantity: qty,
                                price,
                                quote_cost,
                                quote_budget_complete,
                            }),
                            ..Fill::default()
                        }
                    } else {
                        Fill {
                            order_id: o.client_id,
                            qty: Quantity::from_raw(qty.to_raw(9).expect("legacy fill scale")),
                            price: Price::from_raw(price.to_raw(9).expect("legacy price scale")),
                            fee: Money::from_raw(fee_raw),
                            ts,
                            account_id: o.account_id.clone(),
                            ..Fill::default()
                        }
                    };
                    self.all_fills.push(fill.clone());
                    out.push(fill);

                    if o.status != OrderStatus::Filled {
                        let consumed_reservation = proportional_reservation_raw(
                            funding_reservation_raw,
                            qty,
                            remaining_before_fill,
                        );
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
                    self.record_attempt(&o, ts, 0, None, no_fill_reason);
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
            requested_qty_exact: order
                .exact_amounts
                .map(|_| order.remaining_decimal().unwrap_or(ExactDecimal::ZERO)),
            filled_qty_exact: order.exact_amounts.map(|_| raw_decimal(filled_qty_raw)),
            remaining_qty_exact: order.exact_amounts.map(|_| {
                order
                    .remaining_decimal()
                    .unwrap_or(ExactDecimal::ZERO)
                    .checked_sub(raw_decimal(filled_qty_raw))
                    .unwrap_or(ExactDecimal::ZERO)
            }),
            price_raw,
            price_exact: price_raw.map(raw_decimal),
            reason,
            displayed_qty_raw: None,
            queue_ahead_before_raw: None,
            queue_ahead_after_raw: None,
            estimated_volume_raw: None,
            displayed_reduction_raw: None,
            queue_consumed_raw: None,
        });
    }

    fn record_exact_attempt(
        &mut self,
        order: &Order,
        ts: u64,
        requested_qty: ExactDecimal,
        filled_qty: ExactDecimal,
        price: Option<ExactDecimal>,
        reason: ExecutionAttemptReason,
    ) {
        self.record_attempt(
            order,
            ts,
            filled_qty.to_raw(9).unwrap_or(0),
            price.and_then(|price| price.to_raw(9).ok()),
            reason,
        );
        if let Some(attempt) = self.execution_attempts.last_mut() {
            attempt.requested_qty_exact = Some(requested_qty);
            attempt.filled_qty_exact = Some(filled_qty);
            attempt.remaining_qty_exact = requested_qty.checked_sub(filled_qty);
            attempt.price_exact = price;
        }
    }

    /// 用本引擎自己的费率模型、合约乘数折算和 maker/taker 判定估计一笔成交的费用。
    /// 前置资金门禁必须与真实计费同源，否则"校验时以为免费、成交时才扣款"会让
    /// 现金充足的判定失真。`price` 传未折算的行情价。
    pub fn estimate_fee(
        &self,
        qty: i128,
        price: i128,
        order: &Order,
    ) -> Result<i128, qx_core::QxError> {
        let is_maker = order.policy.as_ref().is_some_and(|policy| policy.post_only);
        self.fee_model
            .commission_for_side(qty, self.fee_price(price)?, order.side, is_maker)
    }

    pub fn estimate_fee_exact(
        &self,
        qty: ExactDecimal,
        price: ExactDecimal,
        order: &Order,
    ) -> Result<ExactDecimal, String> {
        if !qty.is_positive() || !price.is_positive() {
            return Err("精确手续费的成交数量和价格必须为正".into());
        }
        let multiplier = raw_decimal(self.fee_price_multiplier);
        let fee_price = if self.inverse_fee_basis {
            multiplier.checked_div_at_scale(
                price,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
        } else {
            price.checked_mul_at_scale(
                multiplier,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::AwayFromZero,
            )
        }
        .filter(|value| value.is_positive())
        .ok_or_else(|| "精确手续费价格基准计算失败".to_string())?;
        let is_maker = order.policy.as_ref().is_some_and(|policy| policy.post_only);
        let fee = self
            .fee_model
            .commission_exact(qty, fee_price, order.side, is_maker)
            .map_err(|error| format!("精确手续费计算失败: {error}"))?;
        if fee.is_negative() {
            return Err("精确手续费模型不能返回负数".into());
        }
        Ok(fee)
    }

    /// 把行情价折算成费用模型看到的价格。费用模型按 `qty × price / SCALE` 计基，
    /// 所以线性合约用 `price × contract_size`，反向（币本位）合约用
    /// `contract_size × SCALE / price`——后者与 `TradingInstrumentSpec::notional`
    /// 的 `qty × contract_size / price` 口径一致，误用线性口径会让费用偏差 price² 量级。
    fn fee_price(&self, price: i128) -> Result<i128, qx_core::QxError> {
        let basis = if self.inverse_fee_basis {
            self.fee_price_multiplier
                .checked_mul(SCALE)
                .and_then(|value| value.checked_div(price))
        } else {
            price
                .checked_mul(self.fee_price_multiplier)
                .and_then(|value| value.checked_div(SCALE))
        };
        basis
            .filter(|value| *value > 0)
            .ok_or_else(|| qx_core::QxError::Invariant("手续费价格基准计算溢出或非正".into()))
    }

    /// 反向（币本位）结算的合约必须切换费用基准；现货与线性合约保持默认。
    pub fn set_inverse_fee_basis(&mut self, inverse: bool) {
        self.inverse_fee_basis = inverse;
    }

    /// Bind the currency returned by this engine's fee model for scale-aware fills.
    pub fn set_exact_fee_currency(&mut self, currency: impl Into<String>) {
        let currency = currency.into().trim().to_ascii_uppercase();
        self.exact_fee_currency = (!currency.is_empty()).then_some(currency);
    }

    pub fn set_instrument_spec(&mut self, spec: Option<TradingInstrumentSpec>) {
        self.instrument_spec = spec;
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
fn raw_decimal(value: i128) -> ExactDecimal {
    ExactDecimal::from_parts(value, 9).expect("legacy Bar values use supported scale 9")
}

fn floor_to_legacy_decimal(value: ExactDecimal) -> Option<ExactDecimal> {
    value.checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
}

fn exact_bar_volume_capacity(volume: ExactDecimal, participation_bps: u16) -> Option<ExactDecimal> {
    if !volume.is_positive() || participation_bps == 0 {
        return Some(ExactDecimal::ZERO);
    }
    let bps = ExactDecimal::from_parts(i128::from(participation_bps.min(10_000)), 0).ok()?;
    let denominator = ExactDecimal::from_parts(10_000, 0).ok()?;
    volume
        .checked_mul_at_scale(bps, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)?
        .checked_div_at_scale(
            denominator,
            ExactDecimal::MAX_SCALE,
            DecimalRounding::TowardZero,
        )
}

fn proportional_reservation_raw(
    reservation_raw: i128,
    filled: ExactDecimal,
    remaining: ExactDecimal,
) -> i128 {
    if reservation_raw <= 0 || !filled.is_positive() || !remaining.is_positive() {
        return 0;
    }
    if filled
        .checked_cmp(remaining)
        .is_some_and(|ordering| !ordering.is_lt())
    {
        return reservation_raw;
    }
    let Some(ratio) = filled.checked_div_at_scale(
        remaining,
        ExactDecimal::MAX_SCALE,
        DecimalRounding::TowardZero,
    ) else {
        return 0;
    };
    raw_decimal(reservation_raw)
        .checked_mul_at_scale(ratio, 9, DecimalRounding::TowardZero)
        .and_then(|consumed| consumed.to_raw(9).ok())
        .unwrap_or(0)
        .clamp(0, reservation_raw)
}

#[cfg(test)]
#[path = "venue/tests.rs"]
mod tests;
