//! Shared-account spot order-book execution across multiple markets.

mod model;

use model::{reference_price, total_reserved_cash, validate_market_data_tier, MarketRuntime};
pub use model::{
    OrderBookPortfolioBacktestEngine, OrderBookPortfolioBacktestReport, OrderBookPortfolioMarket,
};

use super::funding::{
    self, FillFundingContext, FundingContext, FundingDecision, SpotCashReservations,
};
use super::{append_event, book_notional};
use crate::portfolio_depth_timeline::{PortfolioDepthSeries, PortfolioDepthTimeline};
use crate::OrderBookMatchingEngine;
use qx_core::{
    EventKind, EventLog, Fnv1a, Ledger, Money, OrderStatus, Price, Priority, TradingProduct,
    ValuationContext,
};
use qx_risk::OrderRiskPosition;
use qx_zhenlu::{Oms, RiskGate};
use std::collections::{BTreeMap, BTreeSet};

impl<'a> OrderBookPortfolioBacktestEngine<'a> {
    pub fn new(
        account_id: impl Into<String>,
        currency: impl Into<String>,
        initial_cash: Money,
        risk: RiskGate,
        markets: Vec<OrderBookPortfolioMarket<'a>>,
    ) -> Result<Self, qx_core::QxError> {
        let account_id = account_id.into();
        let currency = currency.into().trim().to_ascii_uppercase();
        if account_id.trim().is_empty() || currency.is_empty() || initial_cash.raw() <= 0 {
            return Err(qx_core::QxError::BusinessViolation(
                "共享订单簿回测账户、结算币和初始资金必须有效".into(),
            ));
        }
        if !(2..=32).contains(&markets.len()) {
            return Err(qx_core::QxError::BusinessViolation(
                "共享订单簿回测市场数量必须在 2..=32".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for market in &markets {
            let spec = &market.instrument_spec;
            spec.validate()?;
            if !seen.insert(spec.instrument.clone()) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享订单簿回测标的重复: {}",
                    spec.instrument
                )));
            }
            if spec.product != TradingProduct::Spot {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享多市场订单簿当前只支持现货；衍生品组合保证金尚未建模".into(),
                ));
            }
            if !spec.quote_currency.eq_ignore_ascii_case(&currency) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享订单簿现货市场 {} 报价币 {} 与账户结算币 {} 不一致",
                    spec.instrument, spec.quote_currency, currency
                )));
            }
            if market.snapshots.len() < 2 {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享订单簿市场 {} 至少需要两份快照",
                    spec.instrument
                )));
            }
            validate_market_data_tier(market)?;
        }
        Ok(Self {
            account_id,
            currency,
            initial_cash,
            risk,
            markets,
        })
    }

    pub fn run(self) -> Result<OrderBookPortfolioBacktestReport, qx_core::QxError> {
        self.run_with_cancel(|| false)?
            .ok_or_else(|| qx_core::QxError::Invariant("共享订单簿回测意外取消".into()))
    }

    pub fn run_with_cancel<F>(
        self,
        mut is_cancelled: F,
    ) -> Result<Option<OrderBookPortfolioBacktestReport>, qx_core::QxError>
    where
        F: FnMut() -> bool,
    {
        let Self {
            account_id,
            currency,
            initial_cash,
            risk,
            mut markets,
        } = self;
        markets.sort_by(|left, right| {
            left.priority.cmp(&right.priority).then_with(|| {
                left.instrument_spec
                    .instrument
                    .cmp(&right.instrument_spec.instrument)
            })
        });
        let series = markets
            .iter()
            .map(|market| PortfolioDepthSeries {
                instrument: market.instrument_spec.instrument.clone(),
                priority: market.priority,
                snapshots: market.snapshots,
            })
            .collect::<Vec<_>>();
        let timeline =
            PortfolioDepthTimeline::new(&series).map_err(qx_core::QxError::BusinessViolation)?;
        let mut input_hash = Fnv1a::new();
        let mut runtime = Vec::with_capacity(markets.len());
        let mut specs = BTreeMap::new();
        let mut index_by_instrument = BTreeMap::new();
        for (index, market) in markets.into_iter().enumerate() {
            let instrument = market.instrument_spec.instrument.clone();
            input_hash.write_text(&instrument.to_string());
            input_hash.write_text(match market.data_tier {
                crate::DataTier::L1 => "l1",
                crate::DataTier::L2L3 => "l2",
                crate::DataTier::Bar => "bar",
            });
            input_hash.write_u64(market.priority as u64);
            input_hash.write_text(&serde_json::to_string(&market.instrument_spec).map_err(
                |error| {
                    qx_core::QxError::Invariant(format!("共享订单簿产品规格序列化失败: {error}"))
                },
            )?);
            input_hash.write_i128(market.execution_model.fee_bps);
            input_hash.write_u64(market.execution_model.latency_snapshots);
            input_hash.write_i128(market.execution_model.queue_position_bps);
            input_hash.write_i128(market.execution_model.market_impact_bps);
            for snapshot in market.snapshots {
                input_hash.write_u64(snapshot.ts);
                input_hash.write_u64(snapshot.sequence);
                input_hash.write_u64(snapshot.bids.len() as u64);
                input_hash.write_u64(snapshot.asks.len() as u64);
                for level in snapshot.bids.iter().chain(snapshot.asks.iter()) {
                    input_hash.write_i128(level.price.raw());
                    input_hash.write_i128(level.qty.raw());
                }
            }
            let matcher = OrderBookMatchingEngine::with_model(market.execution_model)
                .map_err(qx_core::QxError::BusinessViolation)?
                .with_fee_spec(Some(market.instrument_spec.clone()));
            specs.insert(instrument.clone(), market.instrument_spec.clone());
            index_by_instrument.insert(instrument.clone(), index);
            runtime.push(MarketRuntime {
                instrument,
                spec: market.instrument_spec,
                fee_bps: market.execution_model.fee_bps,
                strategy: market.strategy,
                matcher,
                reservations: SpotCashReservations::default(),
                cancellations_for_next_snapshot: Vec::new(),
            });
        }
        let data_hash = input_hash.finish();
        let mut ledger = Ledger::new();
        let mut log = EventLog::new();
        let mut oms = Oms::new();
        let first_ts = timeline.events()[0].snapshot.ts;
        let deposit_id = ledger.deposit(&account_id, &currency, initial_cash, first_ts)?;
        let deposit = ledger
            .entries()
            .iter()
            .find(|entry| entry.id == deposit_id)
            .cloned()
            .ok_or_else(|| qx_core::QxError::Invariant("共享订单簿初始入金缺失".into()))?;
        append_event(
            &mut log,
            deposit.ts,
            Priority::TIMER,
            EventKind::LedgerApplied { entry: deposit },
        );
        let mut owners = BTreeMap::<u64, (usize, u64)>::new();
        let mut local_to_global = BTreeMap::<(usize, u64), u64>::new();
        let mut next_order_id = 1_u64;
        let mut marks = BTreeMap::new();
        let mut fills = Vec::new();
        let mut fills_by_instrument = runtime
            .iter()
            .map(|market| (market.instrument.clone(), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let mut equity = Vec::new();
        let mut fees_raw = 0_i128;
        let mut turnover_raw = 0_i128;
        let mut peak_equity = initial_cash.raw();
        let mut max_drawdown_raw = 0_i128;
        let mut cursor = 0;
        while cursor < timeline.events().len() {
            if is_cancelled() {
                return Ok(None);
            }
            let ts = timeline.events()[cursor].snapshot.ts;
            let start = cursor;
            while cursor < timeline.events().len() && timeline.events()[cursor].snapshot.ts == ts {
                cursor += 1;
            }
            let batch = &timeline.events()[start..cursor];
            let mut cancel_acks = Vec::new();
            for event in batch {
                let market_index = *index_by_instrument
                    .get(&event.instrument)
                    .ok_or_else(|| qx_core::QxError::Invariant("深度事件市场未登记".into()))?;
                let cancellations =
                    std::mem::take(&mut runtime[market_index].cancellations_for_next_snapshot);
                for local_id in cancellations {
                    let global_id = local_to_global
                        .remove(&(market_index, local_id))
                        .ok_or_else(|| {
                            qx_core::QxError::ReconcileRequired(format!(
                                "共享订单簿撤单找不到全局订单映射: {}/{}",
                                runtime[market_index].instrument, local_id
                            ))
                        })?;
                    let order = oms.get(global_id).cloned().ok_or_else(|| {
                        qx_core::QxError::ReconcileRequired("共享订单簿撤单找不到 OMS 订单".into())
                    })?;
                    let market = &mut runtime[market_index];
                    if !market.matcher.cancel(global_id) {
                        return Err(qx_core::QxError::Invariant(format!(
                            "共享订单簿待撤订单 {global_id} 不在撮合器中"
                        )));
                    }
                    oms.cancel(global_id)?;
                    market.reservations.release(global_id);
                    let mut local_order = order;
                    local_order.client_id = local_id;
                    market.strategy.on_order_cancelled(&local_order)?;
                    owners.remove(&global_id);
                    cancel_acks.push(global_id);
                }
                let market_fills = runtime[market_index]
                    .matcher
                    .on_snapshot(event.snapshot)
                    .map_err(qx_core::QxError::Permanent)?;
                for mut fill in market_fills {
                    let mut order = oms.get(fill.order_id).cloned().ok_or_else(|| {
                        qx_core::QxError::ReconcileRequired("共享订单簿成交找不到 OMS 订单".into())
                    })?;
                    let owner = owners.get(&fill.order_id).copied().ok_or_else(|| {
                        qx_core::QxError::ReconcileRequired(
                            "共享订单簿成交找不到策略订单归属".into(),
                        )
                    })?;
                    if owner.0 != market_index {
                        return Err(qx_core::QxError::Invariant(
                            "共享订单簿撮合器返回了其他市场的订单".into(),
                        ));
                    }
                    order.trace_fill(&mut fill, None, None);
                    let reserved_cash = total_reserved_cash(&runtime)?;
                    let market = &mut runtime[market_index];
                    let reservation_update = market.reservations.plan_fill(
                        &order,
                        &fill,
                        FillFundingContext {
                            spec: Some(&market.spec),
                            cash_before_fill: ledger.cash_for(&account_id, &currency),
                            total_reserved_before_fill: reserved_cash,
                            inventory_before_fill: ledger
                                .position_for(&account_id, &market.instrument)
                                .quantity
                                .raw(),
                            total_inventory_reserved_before_fill: market
                                .reservations
                                .total_inventory()?,
                        },
                    )?;
                    let mut local_order = order.clone();
                    local_order.client_id = owner.1;
                    let mut local_fill = fill.clone();
                    local_fill.order_id = owner.1;
                    market.strategy.validate_fill_budget(
                        &local_order,
                        &local_fill,
                        Some(&market.spec),
                    )?;
                    let entry_ids = qx_core::apply_fill_to_books(
                        &mut ledger,
                        &mut oms,
                        &currency,
                        &fill,
                        qx_core::FillTerms::resolve(Some(&market.spec), 1),
                    )?;
                    market.reservations.commit_fill(reservation_update);
                    fees_raw = fees_raw.saturating_add(fill.fee.raw());
                    turnover_raw = turnover_raw.saturating_add(book_notional(
                        Some(&market.spec),
                        fill.qty.raw(),
                        fill.price.raw(),
                    )?);
                    append_event(
                        &mut log,
                        fill.ts,
                        Priority::FEEDBACK,
                        EventKind::Filled { fill: fill.clone() },
                    );
                    for entry_id in entry_ids {
                        let entry = ledger
                            .entries()
                            .iter()
                            .find(|entry| entry.id == entry_id)
                            .cloned()
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("共享订单簿账簿 entry 缺失".into())
                            })?;
                        append_event(
                            &mut log,
                            fill.ts,
                            Priority::FEEDBACK,
                            EventKind::LedgerApplied { entry },
                        );
                    }
                    market.strategy.on_fill(&local_order, &local_fill)?;
                    fills_by_instrument
                        .get_mut(&market.instrument)
                        .expect("registered market projection")
                        .push(fill.clone());
                    fills.push(fill);
                    order = oms.get(order.client_id).cloned().ok_or_else(|| {
                        qx_core::QxError::ReconcileRequired("共享订单簿成交后 OMS 订单消失".into())
                    })?;
                    if order.status == OrderStatus::Filled {
                        owners.remove(&order.client_id);
                        local_to_global.remove(&(owner.0, owner.1));
                    }
                }
            }
            for global_id in cancel_acks {
                append_event(
                    &mut log,
                    ts,
                    Priority::MARKET,
                    EventKind::Cancelled {
                        client_order_id: global_id,
                    },
                );
            }
            let mut accepted_ids = Vec::new();
            for event in batch {
                let market_index = *index_by_instrument
                    .get(&event.instrument)
                    .expect("timeline market was validated");
                let mut total_reserved = total_reserved_cash(&runtime)?;
                let cash = ledger.cash_balances_for(&account_id);
                let free_cash = ledger
                    .cash_for(&account_id, &currency)
                    .checked_sub(total_reserved)
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant("共享订单簿资金预留超过账户现金".into())
                    })?
                    .max(0);
                let market = &mut runtime[market_index];
                market.strategy.update_account_state(&cash, Some(free_cash));
                let position = ledger
                    .position_for(&account_id, &market.instrument)
                    .quantity
                    .raw();
                let open_local = oms
                    .active_order_ids()
                    .into_iter()
                    .filter_map(|global_id| {
                        owners.get(&global_id).and_then(|(owner_market, local_id)| {
                            (*owner_market == market_index).then_some(*local_id)
                        })
                    })
                    .collect::<Vec<_>>();
                let actions = market
                    .strategy
                    .on_order_book_actions_with_margin(
                        event.snapshot,
                        position,
                        &open_local,
                        Some(free_cash),
                    )
                    .map_err(qx_core::QxError::BusinessViolation)?;
                let open_set = open_local.into_iter().collect::<BTreeSet<_>>();
                let mut seen_cancels = BTreeSet::new();
                for local_id in actions.cancel_client_ids {
                    if !seen_cancels.insert(local_id) || !open_set.contains(&local_id) {
                        return Err(qx_core::QxError::BusinessViolation(format!(
                            "共享订单簿策略撤单目标重复或非活动订单: {}/{}",
                            market.instrument, local_id
                        )));
                    }
                    market.cancellations_for_next_snapshot.push(local_id);
                }
                let reference_price = reference_price(event.snapshot);
                let mut seen_order_ids = BTreeSet::new();
                for mut local_order in actions.orders {
                    let local_id = local_order.client_id;
                    let global_id = next_order_id;
                    next_order_id = next_order_id.checked_add(1).ok_or_else(|| {
                        qx_core::QxError::Invariant("共享订单簿全局订单 ID 溢出".into())
                    })?;
                    if local_id == 0
                        || !seen_order_ids.insert(local_id)
                        || local_to_global.contains_key(&(market_index, local_id))
                    {
                        append_event(
                            &mut log,
                            ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: global_id,
                                reason: "策略本地 client_id 必须为正且在本市场唯一".into(),
                            },
                        );
                        continue;
                    }
                    if local_order.account_id.is_empty() {
                        local_order.account_id = account_id.clone();
                    }
                    if local_order.instrument != market.instrument
                        || local_order.account_id != account_id
                    {
                        append_event(
                            &mut log,
                            ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: global_id,
                                reason: "订单账户或标的与共享订单簿市场不一致".into(),
                            },
                        );
                        continue;
                    }
                    let policy = local_order.policy.unwrap_or_default();
                    if let Err(error) = policy.validate_for(&market.spec).and_then(|_| {
                        market.spec.validate_order(
                            local_order.qty.raw(),
                            local_order.limit.map(|price| price.raw()),
                        )
                    }) {
                        append_event(
                            &mut log,
                            ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: global_id,
                                reason: error.to_string(),
                            },
                        );
                        continue;
                    }
                    let position_snapshot = OrderRiskPosition::new_with_multiplier(
                        position,
                        reference_price
                            .map(|price| {
                                market.spec.notional(position.saturating_abs(), price.raw())
                            })
                            .transpose()?
                            .unwrap_or(0),
                        market.spec.contract_size,
                    );
                    if let Err(error) = risk.check_with_spec(
                        &local_order,
                        &position_snapshot,
                        reference_price,
                        Some(&market.spec),
                    ) {
                        append_event(
                            &mut log,
                            ts,
                            Priority::COMMAND,
                            EventKind::Rejected {
                                client_order_id: global_id,
                                reason: error.to_string(),
                            },
                        );
                        continue;
                    }
                    let funding = funding::preflight_order(
                        &local_order,
                        policy,
                        Some(&market.spec),
                        FundingContext {
                            snapshot: event.snapshot,
                            reference_price,
                            ledger: &ledger,
                            account_id: &account_id,
                            currency: &currency,
                            fee_bps: market.fee_bps,
                            reservations: &market.reservations,
                            total_reserved_cash_raw: Some(total_reserved),
                        },
                    )?;
                    let (reserve_cash, reserve_inventory) = match funding {
                        FundingDecision::Accept {
                            reserve_cash_raw,
                            reserve_inventory_raw,
                        } => (reserve_cash_raw, reserve_inventory_raw),
                        FundingDecision::NotRequired => (0, 0),
                        FundingDecision::Reject(reason) => {
                            append_event(
                                &mut log,
                                ts,
                                Priority::COMMAND,
                                EventKind::Rejected {
                                    client_order_id: global_id,
                                    reason,
                                },
                            );
                            continue;
                        }
                    };
                    if let Some(price) = local_order.limit.or(reference_price) {
                        market.strategy.check_order_budget(
                            &local_order,
                            price.raw(),
                            Some(&market.spec),
                        )?;
                    }
                    local_order.client_id = global_id;
                    local_order.status = OrderStatus::Submitted;
                    market
                        .matcher
                        .submit(local_order.clone())
                        .map_err(qx_core::QxError::BusinessViolation)?;
                    oms.submit(local_order.clone())?;
                    oms.accept(global_id)?;
                    if reserve_cash > 0 || reserve_inventory > 0 {
                        market.reservations.reserve(
                            global_id,
                            local_order.qty.raw(),
                            reserve_cash,
                            reserve_inventory,
                        )?;
                    }
                    total_reserved = total_reserved.checked_add(reserve_cash).ok_or_else(|| {
                        qx_core::QxError::Invariant("共享盘口现金预留溢出".into())
                    })?;
                    owners.insert(global_id, (market_index, local_id));
                    local_to_global.insert((market_index, local_id), global_id);
                    let mut callback_order = local_order.clone();
                    callback_order.client_id = local_id;
                    market
                        .strategy
                        .on_order_accepted(&callback_order, reference_price.map(Price::raw))?;
                    append_event(
                        &mut log,
                        ts,
                        Priority::COMMAND,
                        EventKind::OrderSubmitted {
                            order: local_order.clone(),
                        },
                    );
                    accepted_ids.push(global_id);
                }
                if let Some(price) = reference_price {
                    marks.insert(market.instrument.clone(), price);
                }
            }
            for global_id in accepted_ids {
                append_event(
                    &mut log,
                    ts,
                    Priority::MATCH,
                    EventKind::Accepted {
                        client_order_id: global_id,
                        venue_order_id: None,
                    },
                );
            }
            let valuation =
                ValuationContext::spot(&account_id, &marks, &currency).with_market_specs(&specs);
            let marked_equity = ledger.valuate(&valuation)?.equity.raw();
            peak_equity = peak_equity.max(marked_equity);
            max_drawdown_raw = max_drawdown_raw.max(peak_equity.saturating_sub(marked_equity));
            equity.push((ts, marked_equity));
        }
        log.validate()?;
        let final_equity = equity
            .last()
            .map(|(_, value)| *value)
            .unwrap_or(initial_cash.raw());
        let return_bps = final_equity
            .saturating_sub(initial_cash.raw())
            .saturating_mul(10_000)
            .checked_div(initial_cash.raw())
            .unwrap_or(0)
            .clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32;
        let max_drawdown_bps = if peak_equity > 0 {
            max_drawdown_raw
                .saturating_mul(10_000)
                .checked_div(peak_equity)
                .unwrap_or(0)
                .clamp(0, 10_000) as u32
        } else {
            0
        };
        let pending_orders = runtime
            .iter()
            .map(|market| market.matcher.pending_count())
            .sum();
        let report = OrderBookPortfolioBacktestReport {
            fills,
            fills_by_instrument,
            ledger,
            event_log: log,
            equity,
            pending_orders,
            fees_raw,
            turnover_raw,
            return_bps,
            max_drawdown_bps,
            input_data_hash: data_hash,
            clock_start: first_ts,
            clock_end: timeline
                .events()
                .last()
                .expect("non-empty timeline")
                .snapshot
                .ts,
        };
        report.verify()?;
        Ok(Some(report))
    }
}
