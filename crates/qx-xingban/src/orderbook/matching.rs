use super::*;
use crate::ExecutionLiquidityEvidence;
use qx_core::Money;

impl OrderBookMatchingEngine {
    pub fn on_snapshot_with_cancellations(
        &mut self,
        snapshot: &OrderBookSnapshot,
        cancelled_client_ids: &[u64],
    ) -> Result<Vec<Fill>, String> {
        snapshot.validate()?;
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
                        self.record_attempt(
                            &order,
                            snapshot.ts,
                            order.remaining().raw(),
                            0,
                            None,
                            ExecutionAttemptReason::QueueBaselineUnavailable,
                            ExecutionLiquidityEvidence::default(),
                        );
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
                    self.record_attempt(
                        &order,
                        snapshot.ts,
                        requested_qty_raw,
                        executed_qty_raw,
                        executed_price_raw,
                        attempt_reason,
                        attempt_liquidity,
                    );
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
            self.record_attempt(
                &order,
                snapshot.ts,
                requested_qty_raw,
                executed_qty_raw,
                first_price_raw,
                attempt_reason,
                attempt_liquidity,
            );
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
}
