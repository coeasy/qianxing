//! Deterministic assembly of bounded account, market and order facts from EventLog ranges.

use super::*;
use qx_core::{Event, EventKind, InstrumentId};
use qx_runtime::{OutboxRecovery, RuntimeConfig};

pub(in crate::workers::strategy_worker) fn collect_pending(
    config: &RuntimeConfig,
    root: &Path,
    strategy_id: &str,
    state: &CursorState,
) -> Result<PendingExecutionBatch, String> {
    let mut names = BTreeSet::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.role == qx_runtime::WorkerRole::Execution)
    {
        let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        else {
            continue;
        };
        if let Some(name) = qx_runtime::account_event_log_name(account_id, venue_id) {
            names.insert(name);
        }
    }
    let market_data_level = strategy_data_level(config.strategy.python_strategy_spec.as_ref());
    let market_instrument = if matches!(market_data_level, "tick" | "orderbook") {
        Some(
            config
                .strategy
                .instrument
                .as_deref()
                .and_then(InstrumentId::parse)
                .ok_or_else(|| "Runtime 行情事件策略 instrument 非法或未配置".to_string())?,
        )
    } else {
        None
    };
    let market_sources = if let Some(instrument) = market_instrument.as_ref() {
        crate::live_market_event_logs(config, root, instrument)?
            .into_iter()
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    for log_name in market_sources.keys() {
        names.insert(log_name.clone());
    }

    let mut account_update_budget = config
        .strategy
        .python_strategy_spec
        .as_ref()
        .map(|_| MAX_RUNTIME_ACCOUNT_UPDATE_BATCH);
    let mut batch = PendingExecutionBatch::default();
    for log_name in names {
        if account_update_budget == Some(0) {
            break;
        }
        if !crate::event_log_exists(config, root, &log_name)? {
            continue;
        }
        let pipeline = if let Some(currency) = market_sources.get(&log_name) {
            qx_service::pipeline::open_runtime_pipeline(
                config,
                root,
                log_name.clone(),
                currency.clone(),
                OutboxRecovery::ReadOnly,
            )
        } else {
            crate::open_account_pipeline(config, root, &log_name, OutboxRecovery::ReadOnly)
        }
        .map_err(|error| format!("读取 Strategy EventLog 失败 {log_name}: {error}"))?;
        let events = pipeline.log().events();
        let Some(through_seq) = events.last().map(|event| event.seq) else {
            continue;
        };
        let next_after_tail = through_seq
            .checked_add(1)
            .ok_or_else(|| format!("Strategy EventLog 序号溢出: log={log_name}"))?;
        let from_seq = match state.next_seq_by_log.get(&log_name).copied() {
            Some(cursor) => cursor,
            None if market_sources.contains_key(&log_name) => {
                let instrument = market_instrument
                    .as_ref()
                    .ok_or_else(|| "Runtime 行情源缺少 instrument".to_string())?;
                let first_seq = if market_data_level == "tick" {
                    let available =
                        market_ticks_for_range(events, &log_name, instrument, 0, through_seq);
                    available
                        .get(available.len().saturating_sub(MAX_RUNTIME_TICK_BATCH))
                        .map(|tick| tick.event_seq)
                } else {
                    let available =
                        market_order_books_for_range(events, &log_name, instrument, 0, through_seq);
                    available
                        .get(available.len().saturating_sub(MAX_RUNTIME_ORDER_BOOK_BATCH))
                        .map(|book| book.event_seq)
                };
                first_seq.unwrap_or(next_after_tail)
            }
            None => 0,
        };
        if from_seq > next_after_tail {
            return Err(format!(
                "Strategy 执行事实游标超出 EventLog 尾部: log={log_name} cursor={from_seq} tail={through_seq}"
            ));
        }
        // After a successful delivery, next_seq points one past the current tail.
        // Treat that exact boundary as caught up; a larger cursor still means the
        // EventLog was truncated or replaced and must fail closed.
        if from_seq == next_after_tail {
            continue;
        }
        let mut range_through_seq = through_seq;
        if let Some(remaining) = account_update_budget.as_mut() {
            let (bounded_through_seq, included_count) =
                account_update_range(events, config, from_seq, through_seq, *remaining);
            range_through_seq = bounded_through_seq;
            *remaining = remaining.saturating_sub(included_count);
            if range_through_seq < from_seq {
                continue;
            }
        }
        batch.ranges.push(CursorRange {
            log_name: log_name.clone(),
            from_seq,
            through_seq: range_through_seq,
        });
        let replayed =
            replay::replay_execution_updates(events, strategy_id, from_seq, range_through_seq)?;
        batch.updates.extend(replayed.order_updates);
        batch.fill_updates.extend(replayed.fill_updates);
        batch.account_updates.extend(replay_account_updates(
            events,
            config,
            from_seq,
            range_through_seq,
        ));
        if let Some(instrument) = market_instrument.as_ref() {
            if market_sources.contains_key(&log_name) {
                if market_data_level == "tick" {
                    batch.market_ticks.extend(market_ticks_for_range(
                        events,
                        &log_name,
                        instrument,
                        from_seq,
                        range_through_seq,
                    ));
                } else if market_data_level == "orderbook" {
                    batch
                        .market_order_books
                        .extend(market_order_books_for_range(
                            events,
                            &log_name,
                            instrument,
                            from_seq,
                            range_through_seq,
                        ));
                }
            }
        }
    }
    replay::sort_updates(&mut batch.updates);
    replay::sort_fill_updates(&mut batch.fill_updates);
    sort_market_ticks(&mut batch.market_ticks);
    sort_market_order_books(&mut batch.market_order_books);
    sort_account_updates(&mut batch.account_updates);
    if batch.market_ticks.len() > MAX_RUNTIME_TICK_BATCH {
        return Err(format!(
            "Runtime Tick 策略单次待处理行情超过 10000 条（{}）；处理行情积压后重试",
            batch.market_ticks.len()
        ));
    }
    if batch.market_order_books.len() > MAX_RUNTIME_ORDER_BOOK_BATCH {
        return Err(format!(
            "Runtime OrderBook 策略单次待处理行情超过 10000 条（{}）；处理行情积压后重试",
            batch.market_order_books.len()
        ));
    }
    Ok(batch)
}

fn account_update_range(
    events: &[Event],
    config: &RuntimeConfig,
    from_seq: u64,
    through_seq: u64,
    limit: usize,
) -> (u64, usize) {
    if config.strategy.python_strategy_spec.is_none() {
        return (through_seq, 0);
    }
    let (Some(account_id), Some(venue_id)) = (
        config.strategy.account_id.as_deref(),
        config.strategy.venue_id.as_deref(),
    ) else {
        return (through_seq, 0);
    };
    let matches = || {
        events.iter().filter(|event| {
            event.seq >= from_seq
                && event.seq <= through_seq
                && account_update_matches(event, account_id, venue_id)
        })
    };
    let count = matches().count();
    if count <= limit {
        return (through_seq, count);
    }
    let cutoff = matches()
        .nth(limit)
        .map(|event| event.seq.saturating_sub(1))
        .unwrap_or(through_seq);
    (cutoff, limit)
}

fn account_update_matches(event: &Event, account_id: &str, venue_id: &str) -> bool {
    match &event.kind {
        EventKind::AccountBalanceSnapshot {
            account_id: fact_account_id,
            venue_id: fact_venue_id,
            ..
        }
        | EventKind::AccountPositionSnapshot {
            account_id: fact_account_id,
            venue_id: fact_venue_id,
            ..
        } => fact_account_id == account_id && fact_venue_id == venue_id,
        _ => false,
    }
}

pub(in crate::workers::strategy_worker) fn execution_updates_for_ranges(
    config: &RuntimeConfig,
    root: &Path,
    strategy_id: &str,
    ranges: &[CursorRange],
) -> Result<ReplayedExecutionUpdates, String> {
    let mut updates = Vec::new();
    let mut fill_updates = Vec::new();
    let market_data_level = strategy_data_level(config.strategy.python_strategy_spec.as_ref());
    let market_instrument = if matches!(market_data_level, "tick" | "orderbook") {
        Some(
            config
                .strategy
                .instrument
                .as_deref()
                .and_then(InstrumentId::parse)
                .ok_or_else(|| "Runtime 行情事件策略 instrument 非法或未配置".to_string())?,
        )
    } else {
        None
    };
    let market_sources = if let Some(instrument) = market_instrument.as_ref() {
        crate::live_market_event_logs(config, root, instrument)?
            .into_iter()
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let mut market_ticks = Vec::new();
    let mut market_order_books = Vec::new();
    let mut account_updates = Vec::new();
    for range in ranges {
        if range.log_name.trim().is_empty() || range.through_seq < range.from_seq {
            return Err("Strategy 作业中的 execution cursor 范围非法".into());
        }
        if !crate::event_log_exists(config, root, &range.log_name)? {
            return Err(format!(
                "Strategy 作业引用的 EventLog 不存在: {}",
                range.log_name
            ));
        }
        let pipeline = if let Some(currency) = market_sources.get(&range.log_name) {
            qx_service::pipeline::open_runtime_pipeline(
                config,
                root,
                range.log_name.clone(),
                currency.clone(),
                OutboxRecovery::ReadOnly,
            )
        } else {
            crate::open_account_pipeline(config, root, &range.log_name, OutboxRecovery::ReadOnly)
        }
        .map_err(|error| {
            format!(
                "读取 Strategy 作业引用的 EventLog 失败 {}: {error}",
                range.log_name
            )
        })?;
        let events = pipeline.log().events();
        if events
            .last()
            .is_none_or(|event| event.seq < range.through_seq)
        {
            return Err(format!(
                "Strategy 作业引用的 EventLog 尚未到达水位: log={} through={}",
                range.log_name, range.through_seq
            ));
        }
        let replayed = replay::replay_execution_updates(
            events,
            strategy_id,
            range.from_seq,
            range.through_seq,
        )?;
        updates.extend(replayed.order_updates);
        fill_updates.extend(replayed.fill_updates);
        account_updates.extend(replay_account_updates(
            events,
            config,
            range.from_seq,
            range.through_seq,
        ));
        if let Some(instrument) = market_instrument.as_ref() {
            if market_sources.contains_key(&range.log_name) {
                if market_data_level == "tick" {
                    market_ticks.extend(market_ticks_for_range(
                        events,
                        &range.log_name,
                        instrument,
                        range.from_seq,
                        range.through_seq,
                    ));
                } else if market_data_level == "orderbook" {
                    market_order_books.extend(market_order_books_for_range(
                        events,
                        &range.log_name,
                        instrument,
                        range.from_seq,
                        range.through_seq,
                    ));
                }
            }
        }
    }
    replay::sort_updates(&mut updates);
    replay::sort_fill_updates(&mut fill_updates);
    sort_market_ticks(&mut market_ticks);
    sort_market_order_books(&mut market_order_books);
    sort_account_updates(&mut account_updates);
    if market_ticks.len() > MAX_RUNTIME_TICK_BATCH {
        return Err(format!(
            "Strategy 作业引用的 Tick 范围超过 10000 条: {}",
            market_ticks.len()
        ));
    }
    if market_order_books.len() > MAX_RUNTIME_ORDER_BOOK_BATCH {
        return Err(format!(
            "Strategy 作业引用的 OrderBook 范围超过 10000 条: {}",
            market_order_books.len()
        ));
    }
    Ok(ReplayedExecutionUpdates {
        order_updates: updates,
        fill_updates,
        market_ticks,
        market_order_books,
        account_updates,
    })
}

pub(super) fn replay_account_updates(
    events: &[Event],
    config: &RuntimeConfig,
    from_seq: u64,
    through_seq: u64,
) -> Vec<StrategyAccountUpdateV1> {
    let (Some(account_id), Some(venue_id)) = (
        config.strategy.account_id.as_deref(),
        config.strategy.venue_id.as_deref(),
    ) else {
        return Vec::new();
    };
    let mut updates = Vec::new();
    for event in events
        .iter()
        .filter(|event| event.seq >= from_seq && event.seq <= through_seq)
    {
        let update = match &event.kind {
            EventKind::AccountBalanceSnapshot {
                account_id: fact_account_id,
                venue_id: fact_venue_id,
                balances,
            } if fact_account_id == account_id && fact_venue_id == venue_id => {
                Some(StrategyAccountUpdateV1 {
                    schema_version: 1,
                    event: "account_snapshot".into(),
                    event_seq: event.seq,
                    ts: event.ts,
                    account_id: fact_account_id.clone(),
                    venue_id: fact_venue_id.clone(),
                    balances: Some(balances.clone()),
                    positions: None,
                })
            }
            EventKind::AccountPositionSnapshot {
                account_id: fact_account_id,
                venue_id: fact_venue_id,
                positions,
            } if fact_account_id == account_id && fact_venue_id == venue_id => {
                Some(StrategyAccountUpdateV1 {
                    schema_version: 1,
                    event: "position_snapshot".into(),
                    event_seq: event.seq,
                    ts: event.ts,
                    account_id: fact_account_id.clone(),
                    venue_id: fact_venue_id.clone(),
                    balances: None,
                    positions: Some(positions.clone()),
                })
            }
            _ => None,
        };
        if let Some(update) = update {
            updates.push(update);
        }
    }
    updates
}

fn sort_account_updates(updates: &mut [StrategyAccountUpdateV1]) {
    updates.sort_by(|left, right| {
        (
            left.ts,
            left.account_id.as_str(),
            left.venue_id.as_str(),
            left.event_seq,
        )
            .cmp(&(
                right.ts,
                right.account_id.as_str(),
                right.venue_id.as_str(),
                right.event_seq,
            ))
    });
}

fn strategy_data_level(spec: Option<&serde_json::Value>) -> &str {
    spec.and_then(|value| value.get("data_level"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("bar")
}

fn market_ticks_for_range(
    events: &[Event],
    stream_id: &str,
    instrument: &InstrumentId,
    from_seq: u64,
    through_seq: u64,
) -> Vec<StrategyContractMarketTickV1> {
    events
        .iter()
        .filter(|event| event.seq >= from_seq && event.seq <= through_seq)
        .filter_map(|event| {
            let EventKind::MarketQuote {
                instrument: quote_instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
                bid_exact,
                ask_exact,
                bid_qty_exact,
                ask_qty_exact,
            } = &event.kind
            else {
                return None;
            };
            (quote_instrument == instrument).then(|| StrategyContractMarketTickV1 {
                stream_id: stream_id.to_string(),
                event_seq: event.seq,
                source_seq: event.source_seq,
                ts: event.ts,
                instrument: quote_instrument.to_string(),
                bid_raw: bid.raw(),
                ask_raw: ask.raw(),
                bid_qty_raw: bid_qty.raw(),
                ask_qty_raw: ask_qty.raw(),
                bid_exact: bid_exact.map(|value| value.to_string()),
                ask_exact: ask_exact.map(|value| value.to_string()),
                bid_qty_exact: bid_qty_exact.map(|value| value.to_string()),
                ask_qty_exact: ask_qty_exact.map(|value| value.to_string()),
            })
        })
        .collect()
}

fn sort_market_ticks(ticks: &mut [StrategyContractMarketTickV1]) {
    ticks.sort_by(|left, right| {
        (left.ts, &left.stream_id, left.event_seq).cmp(&(
            right.ts,
            &right.stream_id,
            right.event_seq,
        ))
    });
}

fn market_order_books_for_range(
    events: &[Event],
    stream_id: &str,
    instrument: &InstrumentId,
    from_seq: u64,
    through_seq: u64,
) -> Vec<StrategyContractMarketOrderBookV1> {
    events
        .iter()
        .filter(|event| event.seq >= from_seq && event.seq <= through_seq)
        .filter_map(|event| {
            let EventKind::MarketOrderBook {
                instrument: book_instrument,
                sequence,
                bids,
                asks,
            } = &event.kind
            else {
                return None;
            };
            (book_instrument == instrument).then(|| {
                let convert = |levels: &[qx_core::MarketDepthLevel]| {
                    levels
                        .iter()
                        .map(|level| StrategyContractOrderBookLevelV1 {
                            price_raw: level.price.raw(),
                            qty_raw: level.quantity.raw(),
                            price_exact: level.price_exact.map(|value| value.to_string()),
                            qty_exact: level.quantity_exact.map(|value| value.to_string()),
                        })
                        .collect()
                };
                StrategyContractMarketOrderBookV1 {
                    stream_id: stream_id.to_string(),
                    event_seq: event.seq,
                    source_seq: event.source_seq,
                    ts: event.ts,
                    instrument: book_instrument.to_string(),
                    sequence: *sequence,
                    bids: convert(bids),
                    asks: convert(asks),
                }
            })
        })
        .collect()
}

fn sort_market_order_books(books: &mut [StrategyContractMarketOrderBookV1]) {
    books.sort_by(|left, right| {
        (left.ts, &left.stream_id, left.event_seq).cmp(&(
            right.ts,
            &right.stream_id,
            right.event_seq,
        ))
    });
}
