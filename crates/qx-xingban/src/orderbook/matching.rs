use super::*;
use crate::ExecutionLiquidityEvidence;
use qx_core::{
    DecimalRounding, ExactDecimal, ExactFillAmounts, ExactFillFee, Fill, Money, OrderStatus,
    TradingInstrumentSpec,
};

impl OrderBookMatchingEngine {
    pub fn on_snapshot_with_cancellations(
        &mut self,
        snapshot: &OrderBookSnapshot,
        cancelled_client_ids: &[u64],
    ) -> Result<Vec<Fill>, String> {
        snapshot.validate()?;
        if snapshot.has_exact_levels() {
            return self.on_exact_snapshot(snapshot, cancelled_client_ids);
        }
        if self.pending.iter().any(|pending| {
            pending.order.exact_amounts.is_some()
                && !cancelled_client_ids.contains(&pending.order.client_id)
        }) {
            return Err("精确订单不能在旧精度盘口上撮合，拒绝使用 1e-9 投影".into());
        }
        if self
            .last_snapshot_sequence
            .is_some_and(|sequence| snapshot.sequence <= sequence)
            || self.last_snapshot_ts.is_some_and(|ts| snapshot.ts <= ts)
        {
            return Err("订单簿快照 sequence/ts 必须严格递增".into());
        }
        self.last_snapshot_sequence = Some(snapshot.sequence);
        self.last_snapshot_ts = Some(snapshot.ts);
        self.cancel_many(cancelled_client_ids);
        self.snapshot_index = self.snapshot_index.saturating_add(1);
        let mut pending = std::mem::take(&mut self.pending);
        // 单遍按提交顺序处理挂单：到期即撮合、未到期或仍存活者原位留在队列末尾。
        // 先分组再拼接（未到期 + 部分成交）会把早提交、已部分成交的订单排到晚提交、
        // 延迟更长的订单之后，在 latency_snapshots > 0 且发生部分成交时颠倒
        // 价格-时间优先级，让后单抢走前单本应先吃到的流动性。
        let mut next = std::mem::take(&mut self.scratch_pending);
        next.clear();
        next.reserve(pending.len());
        let mut fills = Vec::new();
        let mut remaining_bids = std::mem::take(&mut self.scratch_remaining_bids);
        remaining_bids.clear();
        remaining_bids.extend(snapshot.bids.iter().map(|level| level.qty.raw()));
        let mut remaining_asks = std::mem::take(&mut self.scratch_remaining_asks);
        remaining_asks.clear();
        remaining_asks.extend(snapshot.asks.iter().map(|level| level.qty.raw()));
        let mut queue_bids = std::mem::take(&mut self.scratch_queue_bids);
        queue_bids.clear();
        queue_bids.extend(
            snapshot
                .bids
                .iter()
                .map(|level| self.queue_ahead(level.qty.raw())),
        );
        let mut queue_asks = std::mem::take(&mut self.scratch_queue_asks);
        queue_asks.clear();
        queue_asks.extend(
            snapshot
                .asks
                .iter()
                .map(|level| self.queue_ahead(level.qty.raw())),
        );
        // 每个同侧同价位的显示量减少只能作为一次估算成交量分配，避免多张本地挂单重复
        // 消费同一段盘口变化。按挂单提交顺序分配，维持确定性的 FIFO 近似。
        let mut estimated_volume_budget = std::mem::take(&mut self.scratch_estimated_volume_budget);
        estimated_volume_budget.clear();
        for pending_order in pending.drain(..) {
            if pending_order.ready_at_snapshot > self.snapshot_index {
                next.push(pending_order);
                continue;
            }
            let ready_at_snapshot = pending_order.ready_at_snapshot;
            let mut order = pending_order.order;
            if order.instrument != snapshot.instrument {
                return Err("订单簿订单 instrument 与快照不一致".into());
            }
            let levels = if order.side == Side::Buy {
                &snapshot.asks
            } else {
                &snapshot.bids
            };
            if let (Some(limit), QueuePositionModel::FifoVolumeEstimate { volume_advance_bps }) =
                (order.limit, self.model.queue_position_model)
            {
                let is_passive = match order.side {
                    Side::Buy => snapshot
                        .asks
                        .first()
                        .is_none_or(|best| best.price.raw() > limit.raw()),
                    Side::Sell => snapshot
                        .bids
                        .first()
                        .is_none_or(|best| best.price.raw() < limit.raw()),
                };
                if is_passive {
                    let requested_qty_raw = order.remaining().raw();
                    let Some(displayed_qty_raw) =
                        level_lookup::displayed_qty_at_limit(snapshot, limit, order.side)
                    else {
                        // A missing price level in a finite-depth snapshot is not proof of zero
                        // queue depth. Discard any stale baseline and wait for an exact-level
                        // observation before estimating reductions or fills.
                        self.record_attempt(ExecutionAttemptDraft {
                            order: &order,
                            ts: snapshot.ts,
                            requested_qty_raw: order.remaining().raw(),
                            filled_qty_raw: 0,
                            price_raw: None,
                            reason: ExecutionAttemptReason::QueueBaselineUnavailable,
                            liquidity: ExecutionLiquidityEvidence::default(),
                        });
                        next.push(PendingBookOrder {
                            order,
                            ready_at_snapshot: pending_order.ready_at_snapshot,
                            queue_state: None,
                        });
                        continue;
                    };
                    let has_queue_baseline = pending_order
                        .queue_state
                        .as_ref()
                        .is_some_and(|state| state.price_raw == limit.raw());
                    let mut state = pending_order.queue_state.unwrap_or(PendingQueueState {
                        price_raw: limit.raw(),
                        queue_ahead_raw: self.queue_ahead(displayed_qty_raw),
                        displayed_qty_raw,
                    });
                    if state.price_raw != limit.raw() {
                        state = PendingQueueState {
                            price_raw: limit.raw(),
                            queue_ahead_raw: self.queue_ahead(displayed_qty_raw),
                            displayed_qty_raw,
                        };
                    }
                    let displayed_reduction = has_queue_baseline.then(|| {
                        state
                            .displayed_qty_raw
                            .saturating_sub(displayed_qty_raw)
                            .max(0)
                    });
                    let observed_estimated_volume = displayed_reduction
                        .map(|reduction| {
                            reduction
                                .checked_mul(i128::from(volume_advance_bps))
                                .and_then(|value| value.checked_div(10_000))
                                .ok_or_else(|| "FIFO 队列估算成交量溢出".to_string())
                        })
                        .transpose()?;
                    let estimated_volume = if let Some(observed) = observed_estimated_volume {
                        *estimated_volume_budget
                            .entry((order.side == Side::Buy, limit.raw()))
                            .or_insert(observed)
                    } else {
                        0
                    };
                    let queue_ahead_before_raw = state.queue_ahead_raw;
                    let queue_consumed = estimated_volume.min(state.queue_ahead_raw);
                    state.queue_ahead_raw = state.queue_ahead_raw.saturating_sub(queue_consumed);
                    let queue_ahead_after_raw = state.queue_ahead_raw;
                    let fill_qty = order
                        .remaining()
                        .raw()
                        .min(estimated_volume.saturating_sub(queue_consumed));
                    if observed_estimated_volume.is_some() {
                        if let Some(budget) =
                            estimated_volume_budget.get_mut(&(order.side == Side::Buy, limit.raw()))
                        {
                            *budget = budget
                                .saturating_sub(queue_consumed)
                                .saturating_sub(fill_qty);
                        }
                    }
                    state.displayed_qty_raw = displayed_qty_raw;
                    let mut executed_price_raw = None;
                    let mut executed_qty_raw = 0;
                    let attempt_liquidity = ExecutionLiquidityEvidence {
                        displayed_qty_raw: Some(displayed_qty_raw),
                        queue_ahead_before_raw: Some(queue_ahead_before_raw),
                        queue_ahead_after_raw: Some(queue_ahead_after_raw),
                        estimated_volume_raw: observed_estimated_volume.map(|_| estimated_volume),
                        displayed_reduction_raw: displayed_reduction,
                        queue_consumed_raw: Some(queue_consumed),
                    };
                    let attempt_reason;
                    if fill_qty > 0 {
                        let execution_price_raw = self
                            .execution_price(order.side, limit.raw())
                            .ok_or_else(|| "订单簿冲击价格计算溢出或非正".to_string())?;
                        if Self::limit_ok(&order, Price::from_raw(execution_price_raw)) {
                            let notional_raw = match self.fee_spec.as_ref() {
                                Some(spec) => {
                                    spec.notional(fill_qty, execution_price_raw).map_err(
                                        |error| format!("订单簿成交名义额计算失败: {error}"),
                                    )?
                                }
                                None => fill_qty
                                    .checked_mul(execution_price_raw)
                                    .and_then(|value| value.checked_div(SCALE))
                                    .ok_or_else(|| "订单簿成交名义额溢出".to_string())?,
                            };
                            let fee_raw =
                                qx_core::bp_amount_checked(notional_raw, self.model.fee_bps)
                                    .map_err(|error| {
                                        format!("订单簿成交手续费计算失败: {error}")
                                    })?;
                            fills.push(Fill {
                                order_id: order.client_id,
                                qty: Quantity::from_raw(fill_qty),
                                price: Price::from_raw(execution_price_raw),
                                fee: Money::from_raw(fee_raw),
                                ts: snapshot.ts,
                                account_id: order.account_id.clone(),
                                strategy_id: order
                                    .trace
                                    .as_ref()
                                    .and_then(|trace| trace.strategy_id.clone()),
                                signal_id: order.trace.as_ref().and_then(|trace| trace.signal_id),
                                intent_id: order.trace.as_ref().and_then(|trace| trace.intent_id),
                                venue_id: None,
                                venue_order_id: None,
                                rule_version: order
                                    .trace
                                    .as_ref()
                                    .and_then(|trace| trace.rule_version.clone()),
                                fee_currency: None,
                                fees: vec![],
                                exact_fees: vec![],
                                exact_amounts: None,
                            });
                            order.filled = Quantity::from_raw(
                                order
                                    .filled
                                    .raw()
                                    .checked_add(fill_qty)
                                    .ok_or_else(|| "订单簿订单成交数量溢出".to_string())?,
                            );
                            executed_price_raw = Some(execution_price_raw);
                            executed_qty_raw = fill_qty;
                            attempt_reason = if fill_qty >= requested_qty_raw {
                                ExecutionAttemptReason::Filled
                            } else {
                                ExecutionAttemptReason::PartiallyFilled
                            };
                        } else {
                            attempt_reason = ExecutionAttemptReason::LimitNotMarketable;
                        }
                    } else if !has_queue_baseline {
                        attempt_reason = ExecutionAttemptReason::QueueBaselineUnavailable;
                    } else if state.queue_ahead_raw > 0 {
                        attempt_reason = ExecutionAttemptReason::QueueAheadRemains;
                    } else {
                        attempt_reason = ExecutionAttemptReason::NoVisibleLiquidity;
                    }
                    self.record_attempt(ExecutionAttemptDraft {
                        order: &order,
                        ts: snapshot.ts,
                        requested_qty_raw,
                        filled_qty_raw: executed_qty_raw,
                        price_raw: executed_price_raw,
                        reason: attempt_reason,
                        liquidity: attempt_liquidity,
                    });
                    order.status = if order.filled.raw() >= order.qty.raw() {
                        OrderStatus::Filled
                    } else if order.filled.raw() > 0 {
                        OrderStatus::PartiallyFilled
                    } else {
                        order.status
                    };
                    if order.status != OrderStatus::Filled {
                        next.push(PendingBookOrder {
                            order,
                            ready_at_snapshot,
                            queue_state: Some(state),
                        });
                    }
                    continue;
                }
            }
            let mut remaining_qty = order.remaining().raw();
            let requested_qty_raw = remaining_qty;
            let mut executed_qty_raw = 0_i128;
            let mut first_price_raw = None;
            let mut blocked_by_limit = false;
            let mut blocked_by_queue = false;
            let mut attempt_liquidity = ExecutionLiquidityEvidence::default();
            for (level_index, level) in levels.iter().enumerate() {
                if remaining_qty <= 0 {
                    break;
                }
                if !Self::limit_ok(&order, level.price) {
                    blocked_by_limit = true;
                    break;
                }
                let raw_available = if order.side == Side::Buy {
                    &mut remaining_asks[level_index]
                } else {
                    &mut remaining_bids[level_index]
                };
                let queue_ahead = if order.limit.is_some() {
                    if order.side == Side::Buy {
                        queue_asks[level_index]
                    } else {
                        queue_bids[level_index]
                    }
                } else {
                    0
                };
                if attempt_liquidity.displayed_qty_raw.is_none() {
                    attempt_liquidity.displayed_qty_raw = Some(level.qty.raw());
                    attempt_liquidity.queue_ahead_before_raw = Some(queue_ahead.max(0));
                    attempt_liquidity.queue_ahead_after_raw = Some(queue_ahead.max(0));
                }
                let available_qty = (*raw_available).saturating_sub(queue_ahead);
                let fill_qty = remaining_qty.min(available_qty);
                if fill_qty <= 0 {
                    blocked_by_queue |= queue_ahead > 0;
                    continue;
                }
                let execution_price_raw = self
                    .execution_price(order.side, level.price.raw())
                    .ok_or_else(|| "订单簿冲击价格计算溢出或非正".to_string())?;
                if !Self::limit_ok(&order, Price::from_raw(execution_price_raw)) {
                    blocked_by_limit = true;
                    break;
                }
                let notional_raw = match self.fee_spec.as_ref() {
                    Some(spec) => spec
                        .notional(fill_qty, execution_price_raw)
                        .map_err(|error| format!("订单簿成交名义额计算失败: {error}"))?,
                    None => fill_qty
                        .checked_mul(execution_price_raw)
                        .and_then(|value| value.checked_div(SCALE))
                        .ok_or_else(|| "订单簿成交名义额溢出".to_string())?,
                };
                let fee_raw = qx_core::bp_amount_checked(notional_raw, self.model.fee_bps)
                    .map_err(|error| format!("订单簿成交手续费计算失败: {error}"))?;
                fills.push(Fill {
                    order_id: order.client_id,
                    qty: Quantity::from_raw(fill_qty),
                    price: Price::from_raw(execution_price_raw),
                    fee: Money::from_raw(fee_raw),
                    ts: snapshot.ts,
                    account_id: order.account_id.clone(),
                    strategy_id: order
                        .trace
                        .as_ref()
                        .and_then(|trace| trace.strategy_id.clone()),
                    signal_id: order.trace.as_ref().and_then(|trace| trace.signal_id),
                    intent_id: order.trace.as_ref().and_then(|trace| trace.intent_id),
                    venue_id: None,
                    venue_order_id: None,
                    rule_version: order
                        .trace
                        .as_ref()
                        .and_then(|trace| trace.rule_version.clone()),
                    fee_currency: None,
                    fees: vec![],
                    exact_fees: vec![],
                    exact_amounts: None,
                });
                first_price_raw.get_or_insert(execution_price_raw);
                remaining_qty -= fill_qty;
                executed_qty_raw = executed_qty_raw.saturating_add(fill_qty);
                *raw_available -= fill_qty;
                if order.limit.is_some() {
                    // 前置队列只能被消耗到 0：不夹住的话 `available = raw - queue` 会因为
                    // 负的 queue 重新变大，把本快照已吃掉的流动性按成交数量凭空复制给
                    // 后续限价单——`queue_position_bps = 0` 时同样成立，因为扣减照样执行。
                    if order.side == Side::Buy {
                        queue_asks[level_index] =
                            queue_asks[level_index].saturating_sub(fill_qty).max(0);
                        attempt_liquidity.queue_ahead_after_raw = Some(queue_asks[level_index]);
                    } else {
                        queue_bids[level_index] =
                            queue_bids[level_index].saturating_sub(fill_qty).max(0);
                        attempt_liquidity.queue_ahead_after_raw = Some(queue_bids[level_index]);
                    }
                }
                order.filled = Quantity::from_raw(
                    order
                        .filled
                        .raw()
                        .checked_add(fill_qty)
                        .ok_or_else(|| "订单簿订单成交数量溢出".to_string())?,
                );
            }
            // 被动限价单没有触及对手盘档位；仍把本方同价位可见量和配置推导的队列前量
            // 写入本次观察，便于区分“队列仍在”与“盘口没有可见数量”。
            if attempt_liquidity.displayed_qty_raw.is_none() {
                if let Some(limit) = order.limit {
                    let displayed_qty_raw =
                        level_lookup::displayed_qty_at_limit(snapshot, limit, order.side)
                            .unwrap_or(0);
                    if displayed_qty_raw > 0 {
                        let queue_ahead_raw = self.queue_ahead(displayed_qty_raw);
                        attempt_liquidity.displayed_qty_raw = Some(displayed_qty_raw);
                        attempt_liquidity.queue_ahead_before_raw = Some(queue_ahead_raw);
                        attempt_liquidity.queue_ahead_after_raw = Some(queue_ahead_raw);
                    }
                }
            }
            let attempt_reason = if executed_qty_raw > 0 {
                if remaining_qty == 0 {
                    ExecutionAttemptReason::Filled
                } else {
                    ExecutionAttemptReason::PartiallyFilled
                }
            } else if order.limit.is_some_and(|limit| {
                level_lookup::displayed_qty_at_limit(snapshot, limit, order.side)
                    .is_some_and(|quantity| quantity > 0)
            }) {
                // Disabled queue-position estimation still reports the visible queue ahead;
                // it simply does not turn quote changes into estimated fills.
                ExecutionAttemptReason::QueueAheadRemains
            } else if blocked_by_queue {
                ExecutionAttemptReason::QueueAheadRemains
            } else if blocked_by_limit {
                ExecutionAttemptReason::LimitNotMarketable
            } else {
                ExecutionAttemptReason::NoVisibleLiquidity
            };
            self.record_attempt(ExecutionAttemptDraft {
                order: &order,
                ts: snapshot.ts,
                requested_qty_raw,
                filled_qty_raw: executed_qty_raw,
                price_raw: first_price_raw,
                reason: attempt_reason,
                liquidity: attempt_liquidity,
            });
            order.status = if order.filled.raw() >= order.qty.raw() {
                OrderStatus::Filled
            } else if order.filled.raw() > 0 {
                OrderStatus::PartiallyFilled
            } else {
                order.status
            };
            if order.status != OrderStatus::Filled {
                next.push(PendingBookOrder {
                    order,
                    ready_at_snapshot,
                    // 可立即成交路径消费了对手盘；下一次回到被动状态时重新观测排队位置。
                    queue_state: None,
                });
            }
        }
        self.pending = next;
        self.scratch_pending = pending;
        self.scratch_remaining_bids = remaining_bids;
        self.scratch_remaining_asks = remaining_asks;
        self.scratch_queue_bids = queue_bids;
        self.scratch_queue_asks = queue_asks;
        self.scratch_estimated_volume_budget = estimated_volume_budget;
        Ok(fills)
    }

    /// Exact spot path. Queue inference is deliberately disabled because it cannot be
    /// reconstructed from the legacy raw liquidity fields.
    fn on_exact_snapshot(
        &mut self,
        snapshot: &OrderBookSnapshot,
        cancelled_client_ids: &[u64],
    ) -> Result<Vec<Fill>, String> {
        if self
            .last_snapshot_sequence
            .is_some_and(|sequence| snapshot.sequence <= sequence)
            || self.last_snapshot_ts.is_some_and(|ts| snapshot.ts <= ts)
        {
            return Err("订单簿快照 sequence/ts 必须严格递增".into());
        }

        let mut pending = self.pending.clone();
        pending.retain(|item| !cancelled_client_ids.contains(&item.order.client_id));
        if pending.is_empty() {
            self.pending.clear();
            self.snapshot_index = self.snapshot_index.saturating_add(1);
            self.last_snapshot_sequence = Some(snapshot.sequence);
            self.last_snapshot_ts = Some(snapshot.ts);
            return Ok(Vec::new());
        }
        if pending
            .iter()
            .any(|pending| pending.order.exact_amounts.is_none())
        {
            return Err("旧精度订单不能在精确盘口上撮合，拒绝使用 1e-9 投影".into());
        }
        if self.model.queue_position_bps != 0
            || self.model.queue_position_model != QueuePositionModel::Disabled
        {
            return Err("精确盘口撮合暂不支持队列位置估算".into());
        }
        let spec = self
            .fee_spec
            .as_ref()
            .ok_or_else(|| "精确盘口撮合需要冻结 TradingInstrumentSpec".to_string())?;
        if spec.exact_rules.is_none() {
            return Err("精确盘口撮合需要带精确交易规则的产品规格".into());
        }
        let mut next = Vec::with_capacity(pending.len());
        let mut fills = Vec::new();
        let mut attempts = Vec::new();
        let mut remaining_bids = snapshot
            .bids
            .iter()
            .map(|level| level.qty_decimal())
            .collect::<Vec<_>>();
        let mut remaining_asks = snapshot
            .asks
            .iter()
            .map(|level| level.qty_decimal())
            .collect::<Vec<_>>();
        let step = spec
            .exact_rules
            .as_ref()
            .expect("exact rules were checked")
            .quantity_step;

        for mut pending_order in pending.drain(..) {
            if pending_order.ready_at_snapshot > self.snapshot_index.saturating_add(1) {
                next.push(pending_order);
                continue;
            }
            let mut order = pending_order.order;
            if order.instrument != snapshot.instrument {
                return Err("订单簿订单 instrument 与快照不一致".into());
            }
            let amounts = order
                .exact_amounts
                .ok_or_else(|| "精确盘口订单缺少 exact_amounts".to_string())?;
            let quote_budget = amounts.quote_budget;
            let target_quantity = if let Some(budget) = quote_budget {
                spec.validate_exact_quote_budget(budget)
                    .map_err(|error| error.to_string())?;
                if order.side != qx_core::Side::Buy {
                    return Err("计价币预算只能用于精确现货买单".into());
                }
                let reference_price = amounts
                    .market_reference_price
                    .or_else(|| snapshot.asks.first().map(|level| level.price_decimal()))
                    .ok_or_else(|| "计价币预算订单缺少冻结参考价".to_string())?;
                let target = spec
                    .base_quantity_for_quote_budget(budget, reference_price)
                    .map_err(|error| error.to_string())?;
                spec.validate_exact_order_with_reference(target, None, Some(reference_price))
                    .map_err(|error| error.to_string())?;
                Some(target)
            } else {
                None
            };
            let requested = match target_quantity {
                Some(target) => target
                    .checked_sub(amounts.filled)
                    .ok_or_else(|| "计价币预算订单剩余基础币数量溢出".to_string())?,
                None => order
                    .remaining_decimal()
                    .ok_or_else(|| "精确盘口订单剩余数量溢出".to_string())?,
            };
            let levels = if order.side == qx_core::Side::Buy {
                &snapshot.asks
            } else {
                &snapshot.bids
            };
            let available = if order.side == qx_core::Side::Buy {
                &mut remaining_asks
            } else {
                &mut remaining_bids
            };
            let mut remaining = requested;
            let mut filled = ExactDecimal::ZERO;
            let mut first_price = None;
            let mut last_price = None;
            let mut blocked_by_limit = false;
            let fill_start = fills.len();
            let mut remaining_budget = quote_budget
                .map(|budget| {
                    budget
                        .checked_sub(amounts.quote_filled)
                        .filter(|remaining| !remaining.is_negative())
                        .ok_or_else(|| "计价币预算剩余金额非法".to_string())
                })
                .transpose()?;

            for (level_index, level) in levels.iter().enumerate() {
                if !remaining.is_positive() {
                    break;
                }
                let execution_price =
                    self.exact_execution_price(spec, order.side, level.price_decimal())?;
                if let Some(limit) = amounts.limit_price {
                    let comparison = execution_price
                        .checked_cmp(limit)
                        .ok_or_else(|| "精确限价比较失败".to_string())?;
                    if (order.side == qx_core::Side::Buy && comparison.is_gt())
                        || (order.side == qx_core::Side::Sell && comparison.is_lt())
                    {
                        blocked_by_limit = true;
                        break;
                    }
                }

                let available_at_level = available[level_index];
                let mut maximum = if remaining
                    .checked_cmp(available_at_level)
                    .is_some_and(|ordering| ordering.is_gt())
                {
                    available_at_level
                } else {
                    remaining
                };
                if let Some(budget) = remaining_budget {
                    let affordable = spec
                        .base_quantity_for_quote_budget(budget, execution_price)
                        .map_err(|error| error.to_string())?;
                    if affordable
                        .checked_cmp(maximum)
                        .is_some_and(|ordering| ordering.is_lt())
                    {
                        maximum = affordable;
                    }
                }
                let fill_quantity = floor_to_step(maximum, step)?;
                if !fill_quantity.is_positive() {
                    continue;
                }
                spec.validate_exact_fill(fill_quantity, execution_price)
                    .map_err(|error| format!("精确盘口成交不符合冻结交易规则: {error}"))?;

                let quote_rounding = if order.side == qx_core::Side::Buy {
                    DecimalRounding::AwayFromZero
                } else {
                    DecimalRounding::TowardZero
                };
                let quote_cost = if spec.product == qx_core::TradingProduct::Spot {
                    Some(
                        spec.quote_cash_exact(fill_quantity, execution_price, quote_rounding)
                            .map_err(|error| format!("精确盘口成交现金额计算失败: {error}"))?,
                    )
                } else {
                    None
                };
                let quote_cost_value = quote_cost.unwrap_or(ExactDecimal::ZERO);
                if remaining_budget.is_some_and(|budget| {
                    quote_cost.is_none_or(|cost| {
                        cost.checked_cmp(budget)
                            .is_none_or(|ordering| ordering.is_gt())
                    })
                }) {
                    return Err("精确盘口成交金额超过计价币预算".into());
                }
                let fee = exact_fee(spec, fill_quantity, execution_price, self.model.fee_bps)?;
                let mut exact_fees = Vec::new();
                if fee.is_positive() {
                    exact_fees.push(ExactFillFee {
                        amount: fee,
                        currency: Some(
                            if spec.product == qx_core::TradingProduct::Spot {
                                spec.quote_currency.trim()
                            } else {
                                spec.settlement_currency.trim()
                            }
                            .to_ascii_uppercase(),
                        ),
                        source_trade_ids: Vec::new(),
                    });
                }
                let next_filled = amounts
                    .filled
                    .checked_add(filled)
                    .and_then(|value| value.checked_add(fill_quantity))
                    .ok_or_else(|| "精确盘口订单累计成交数量溢出".to_string())?;
                let current_quote_filled = order
                    .exact_amounts
                    .ok_or_else(|| "精确盘口订单 exact_amounts 丢失".to_string())?
                    .quote_filled;
                let next_quote_filled = current_quote_filled
                    .checked_add(quote_budget.map_or(ExactDecimal::ZERO, |_| quote_cost_value))
                    .ok_or_else(|| "精确盘口订单累计计价币金额溢出".to_string())?;
                let complete = quote_budget.is_some_and(|budget| {
                    target_quantity.is_some_and(|target| {
                        next_filled
                            .checked_cmp(target)
                            .is_some_and(|ordering| !ordering.is_lt())
                    }) || next_quote_filled
                        .checked_cmp(budget)
                        .is_some_and(|ordering| !ordering.is_lt())
                });
                fills.push(Fill {
                    order_id: order.client_id,
                    qty: qx_core::Quantity::ZERO,
                    price: qx_core::Price::ZERO,
                    fee: Money::ZERO,
                    ts: snapshot.ts,
                    account_id: order.account_id.clone(),
                    strategy_id: order
                        .trace
                        .as_ref()
                        .and_then(|trace| trace.strategy_id.clone()),
                    signal_id: order.trace.as_ref().and_then(|trace| trace.signal_id),
                    intent_id: order.trace.as_ref().and_then(|trace| trace.intent_id),
                    venue_id: None,
                    venue_order_id: None,
                    rule_version: order
                        .trace
                        .as_ref()
                        .and_then(|trace| trace.rule_version.clone()),
                    fee_currency: None,
                    fees: Vec::new(),
                    exact_fees,
                    exact_amounts: Some(ExactFillAmounts {
                        quantity: fill_quantity,
                        price: execution_price,
                        quote_cost,
                        quote_budget_complete: complete,
                    }),
                });

                let exact = order
                    .exact_amounts
                    .as_mut()
                    .ok_or_else(|| "精确盘口订单 exact_amounts 丢失".to_string())?;
                exact.filled = exact
                    .filled
                    .checked_add(fill_quantity)
                    .ok_or_else(|| "精确盘口订单累计成交数量溢出".to_string())?;
                if quote_budget.is_some() {
                    exact.quote_filled = exact
                        .quote_filled
                        .checked_add(quote_cost_value)
                        .ok_or_else(|| "精确盘口订单累计计价币金额溢出".to_string())?;
                    remaining_budget = remaining_budget
                        .and_then(|budget| budget.checked_sub(quote_cost_value))
                        .filter(|budget| !budget.is_negative());
                    if remaining_budget.is_none() {
                        return Err("精确盘口计价币预算扣减失败".into());
                    }
                }
                remaining = remaining
                    .checked_sub(fill_quantity)
                    .ok_or_else(|| "精确盘口订单剩余数量计算溢出".to_string())?;
                available[level_index] = available[level_index]
                    .checked_sub(fill_quantity)
                    .ok_or_else(|| "精确盘口档位剩余数量计算溢出".to_string())?;
                filled = filled
                    .checked_add(fill_quantity)
                    .ok_or_else(|| "精确盘口本次成交数量溢出".to_string())?;
                first_price.get_or_insert(execution_price);
                last_price = Some(execution_price);
                if complete {
                    remaining = ExactDecimal::ZERO;
                    break;
                }
            }

            // If the residual budget cannot buy one quantity step at the last executable
            // price, the budget order is complete even though decimal dust remains.
            if remaining.is_positive() && filled.is_positive() {
                if let (Some(budget), Some(price)) = (remaining_budget, last_price) {
                    let affordable = spec
                        .base_quantity_for_quote_budget(budget, price)
                        .map_err(|error| error.to_string())?;
                    if affordable.is_zero() {
                        remaining = ExactDecimal::ZERO;
                        if let Some(fill) = fills[fill_start..].last_mut() {
                            if let Some(exact) = fill.exact_amounts.as_mut() {
                                exact.quote_budget_complete = true;
                            }
                        }
                    }
                }
            }

            let reason = if remaining.is_zero() {
                ExecutionAttemptReason::Filled
            } else if filled.is_positive() {
                ExecutionAttemptReason::PartiallyFilled
            } else if blocked_by_limit {
                ExecutionAttemptReason::LimitNotMarketable
            } else {
                ExecutionAttemptReason::NoVisibleLiquidity
            };
            attempts.push(exact_attempt(
                &order,
                snapshot.ts,
                requested,
                filled,
                first_price,
                reason,
            ));
            order.status = if remaining.is_zero() {
                OrderStatus::Filled
            } else if filled.is_positive() {
                OrderStatus::PartiallyFilled
            } else {
                order.status
            };
            if order.status != OrderStatus::Filled {
                pending_order.order = order;
                pending_order.queue_state = None;
                next.push(pending_order);
            }
        }

        self.pending = next;
        self.snapshot_index = self.snapshot_index.saturating_add(1);
        self.last_snapshot_sequence = Some(snapshot.sequence);
        self.last_snapshot_ts = Some(snapshot.ts);
        self.execution_attempts.extend(attempts);
        Ok(fills)
    }

    fn exact_execution_price(
        &self,
        spec: &TradingInstrumentSpec,
        side: qx_core::Side,
        book_price: ExactDecimal,
    ) -> Result<ExactDecimal, String> {
        let rules = spec
            .exact_rules
            .as_ref()
            .ok_or_else(|| "精确盘口缺少冻结交易规则".to_string())?;
        if self.model.market_impact_bps == 0 {
            return Ok(book_price);
        }
        let bps = ExactDecimal::from_parts(self.model.market_impact_bps, 0)
            .map_err(|error| error.to_string())?;
        let denominator = ExactDecimal::from_parts(10_000, 0).map_err(|error| error.to_string())?;
        let impact = book_price
            .checked_mul_at_scale(bps, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
            .and_then(|value| {
                value.checked_div_at_scale(
                    denominator,
                    ExactDecimal::MAX_SCALE,
                    DecimalRounding::TowardZero,
                )
            })
            .ok_or_else(|| "精确盘口冲击价格计算溢出".to_string())?;
        let impacted = match side {
            qx_core::Side::Buy => book_price.checked_add(impact),
            qx_core::Side::Sell => book_price.checked_sub(impact),
        }
        .ok_or_else(|| "精确盘口冲击价格计算溢出".to_string())?;
        let rounding = match side {
            qx_core::Side::Buy => DecimalRounding::Ceiling,
            qx_core::Side::Sell => DecimalRounding::Floor,
        };
        let ticks = impacted
            .checked_div_at_scale(rules.price_tick, 0, rounding)
            .ok_or_else(|| "精确盘口价格 tick 取整失败".to_string())?;
        rules
            .price_tick
            .checked_mul(ticks)
            .filter(|price| price.is_positive())
            .ok_or_else(|| "精确盘口冲击价格非正或溢出".to_string())
    }
}

fn floor_to_step(value: ExactDecimal, step: ExactDecimal) -> Result<ExactDecimal, String> {
    let units = value
        .checked_div_at_scale(step, 0, DecimalRounding::TowardZero)
        .ok_or_else(|| "精确盘口数量步长计算溢出".to_string())?;
    step.checked_mul(units)
        .ok_or_else(|| "精确盘口数量步进计算溢出".to_string())
}

fn exact_fee(
    spec: &TradingInstrumentSpec,
    quantity: ExactDecimal,
    price: ExactDecimal,
    fee_bps: i128,
) -> Result<ExactDecimal, String> {
    let notional = spec
        .notional_exact(quantity, price)
        .map_err(|error| format!("精确盘口手续费名义额计算失败: {error}"))?;
    let bps = ExactDecimal::from_parts(fee_bps, 0).map_err(|error| error.to_string())?;
    let denominator = ExactDecimal::from_parts(10_000, 0).map_err(|error| error.to_string())?;
    notional
        .checked_mul_at_scale(bps, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
        .and_then(|value| {
            value.checked_div_at_scale(
                denominator,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            )
        })
        .ok_or_else(|| "精确盘口手续费计算溢出".to_string())
}

fn exact_raw_projection(value: ExactDecimal) -> i128 {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .unwrap_or(0)
}

fn exact_attempt(
    order: &qx_core::Order,
    ts: u64,
    requested: ExactDecimal,
    filled: ExactDecimal,
    price: Option<ExactDecimal>,
    reason: ExecutionAttemptReason,
) -> crate::ExecutionAttempt {
    let remaining = requested.checked_sub(filled).unwrap_or(ExactDecimal::ZERO);
    crate::ExecutionAttempt {
        ts,
        order_id: order.client_id,
        instrument: order.instrument.to_string(),
        side: order.side,
        requested_qty_raw: exact_raw_projection(requested),
        filled_qty_raw: exact_raw_projection(filled),
        remaining_qty_raw: exact_raw_projection(remaining),
        requested_qty_exact: Some(requested),
        filled_qty_exact: Some(filled),
        remaining_qty_exact: Some(remaining),
        price_raw: price.map(exact_raw_projection),
        price_exact: price,
        reason,
        displayed_qty_raw: None,
        queue_ahead_before_raw: None,
        queue_ahead_after_raw: None,
        estimated_volume_raw: None,
        displayed_reduction_raw: None,
        queue_consumed_raw: None,
    }
}
