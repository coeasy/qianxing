//! Durable, account-scoped EventLog cursor and typed execution facts for live strategy jobs.

use qx_core::{Event, EventKind, InstrumentId, Order, OrderStatus};
use qx_runtime::{OutboxRecovery, RuntimeConfig, StrategyRuntimeConfig};
use qx_scheduler::JobSpec;
use qx_strategy::contract::{
    StrategyAccountUpdateV1, StrategyCheckpointV1, StrategyContractMarketOrderBookV1,
    StrategyContractMarketTickV1, StrategyContractOrderBookLevelV1,
};
use qx_strategy::{StrategyFillUpdateV2, StrategyOrderStatusUpdate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CURSOR_SCHEMA_VERSION: u32 = 1;
const JOB_INPUT_REF_PREFIX: &str = "strategy-execution-cursors-v1:";
const MAX_RUNTIME_TICK_BATCH: usize = 10_000;
const MAX_RUNTIME_ORDER_BOOK_BATCH: usize = 10_000;
const MAX_RUNTIME_ACCOUNT_UPDATE_BATCH: usize = 10_000;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorState {
    schema_version: u32,
    /// The next EventLog sequence to inspect for each account/venue log.
    next_seq_by_log: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    strategy_checkpoint: Option<StrategyCheckpointV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    strategy_binding_sha256: Option<String>,
    /// If the process stops after checkpoint commit but before scheduler completion,
    /// recovery finalizes this run without invoking the strategy a second time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committed_run_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorRange {
    log_name: String,
    from_seq: u64,
    through_seq: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PendingExecutionBatch {
    pub(super) ranges: Vec<CursorRange>,
    pub(super) updates: Vec<StrategyOrderStatusUpdate>,
    pub(super) fill_updates: Vec<StrategyFillUpdateV2>,
    pub(super) market_ticks: Vec<StrategyContractMarketTickV1>,
    pub(super) market_order_books: Vec<StrategyContractMarketOrderBookV1>,
    pub(super) account_updates: Vec<StrategyAccountUpdateV1>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ReplayedExecutionUpdates {
    pub(super) order_updates: Vec<StrategyOrderStatusUpdate>,
    pub(super) fill_updates: Vec<StrategyFillUpdateV2>,
    pub(super) market_ticks: Vec<StrategyContractMarketTickV1>,
    pub(super) market_order_books: Vec<StrategyContractMarketOrderBookV1>,
    pub(super) account_updates: Vec<StrategyAccountUpdateV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorReference {
    schema_version: u32,
    ranges: Vec<CursorRange>,
}

pub(super) fn cursor_path(root: &Path, worker_id: &str) -> PathBuf {
    root.join("strategy-live-state")
        .join(format!("{worker_id}.execution-cursor.json"))
}

pub(super) fn load_cursor(path: &Path) -> Result<CursorState, String> {
    let backup = backup_path(path);
    let payload = match std::fs::read_to_string(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::read_to_string(&backup) {
                Ok(payload) => payload,
                Err(backup_error) if backup_error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(CursorState {
                        schema_version: CURSOR_SCHEMA_VERSION,
                        next_seq_by_log: BTreeMap::new(),
                        strategy_checkpoint: None,
                        strategy_binding_sha256: None,
                        committed_run_id: None,
                    });
                }
                Err(backup_error) => {
                    return Err(format!(
                        "读取策略执行事实游标和恢复副本失败 {} / {}: {backup_error}",
                        path.display(),
                        backup.display()
                    ));
                }
            }
        }
        Err(error) => {
            return Err(format!(
                "读取策略执行事实游标失败 {}: {error}",
                path.display()
            ));
        }
    };
    let state: CursorState = serde_json::from_str(&payload)
        .map_err(|error| format!("策略执行事实游标 JSON 无效 {}: {error}", path.display()))?;
    if state.schema_version != CURSOR_SCHEMA_VERSION
        || state
            .next_seq_by_log
            .keys()
            .any(|name| name.trim().is_empty())
    {
        return Err(format!(
            "策略执行事实游标版本或 EventLog 名称非法: {}",
            path.display()
        ));
    }
    if state.strategy_checkpoint.is_some() != state.strategy_binding_sha256.is_some()
        || state
            .strategy_binding_sha256
            .as_deref()
            .is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    {
        return Err(format!(
            "策略 checkpoint 与绑定指纹状态不一致: {}",
            path.display()
        ));
    }
    Ok(state)
}

pub(super) fn save_cursor(path: &Path, state: &CursorState) -> Result<(), String> {
    if state.schema_version != CURSOR_SCHEMA_VERSION
        || state
            .next_seq_by_log
            .keys()
            .any(|name| name.trim().is_empty())
    {
        return Err("拒绝保存版本或 EventLog 名称非法的策略执行事实游标".into());
    }
    if state.strategy_checkpoint.is_some() != state.strategy_binding_sha256.is_some()
        || state
            .strategy_binding_sha256
            .as_deref()
            .is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    {
        return Err("拒绝保存与策略绑定指纹不一致的 checkpoint".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("策略执行事实游标路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建策略执行事实游标目录失败: {error}"))?;
    let temporary = path.with_extension(format!("cursor.tmp.{}", std::process::id()));
    let _temporary_guard = TemporaryCursorFile(temporary.clone());
    let backup = backup_path(path);
    let payload =
        serde_json::to_vec(state).map_err(|error| format!("编码策略执行事实游标失败: {error}"))?;
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入策略执行事实游标失败: {error}"))?;
    if backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|error| format!("清理策略执行事实游标旧恢复副本失败: {error}"))?;
    }
    if path.exists() {
        std::fs::rename(path, &backup)
            .map_err(|error| format!("备份旧策略执行事实游标失败: {error}"))?;
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        if backup.exists() {
            std::fs::rename(&backup, path).map_err(|restore| {
                format!("提交策略执行事实游标失败: {error}; 恢复旧游标失败: {restore}")
            })?;
        }
        return Err(format!("提交策略执行事实游标失败: {error}"));
    }
    // The primary cursor is already committed at this point. A stale recovery
    // copy is safe because load_cursor prefers the primary file; reporting a
    // cleanup failure as a failed commit would leave the caller's in-memory
    // state behind the durable state on disk.
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }
    Ok(())
}

struct TemporaryCursorFile(PathBuf);

impl Drop for TemporaryCursorFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Read every configured execution account/venue log and build a deterministic snapshot.
/// Cursor ranges include unrelated events too, so a successful strategy job can advance past
/// them without repeatedly scanning the same log tail.
pub(super) fn collect_pending(
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
        let replayed = replay_execution_updates(events, strategy_id, from_seq, range_through_seq)?;
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
    sort_updates(&mut batch.updates);
    sort_fill_updates(&mut batch.fill_updates);
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

pub(super) fn execution_updates_for_ranges(
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
        let replayed =
            replay_execution_updates(events, strategy_id, range.from_seq, range.through_seq)?;
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
    sort_updates(&mut updates);
    sort_fill_updates(&mut fill_updates);
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

fn replay_account_updates(
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

pub(super) fn advance_cursor(
    state: &mut CursorState,
    ranges: &[CursorRange],
) -> Result<(), String> {
    for range in ranges {
        let next_seq = range
            .through_seq
            .checked_add(1)
            .ok_or_else(|| "Strategy EventLog 游标溢出".to_string())?;
        let current = state
            .next_seq_by_log
            .entry(range.log_name.clone())
            .or_default();
        *current = (*current).max(next_seq);
    }
    Ok(())
}

pub(super) fn strategy_checkpoint(state: &CursorState) -> Option<&StrategyCheckpointV1> {
    state.strategy_checkpoint.as_ref()
}

pub(super) fn is_committed_run(state: &CursorState, run_id: u64) -> bool {
    state.committed_run_id == Some(run_id)
}

#[cfg(test)]
fn validate_checkpoint_binding(
    state: &CursorState,
    strategy: &StrategyRuntimeConfig,
) -> Result<(), String> {
    validate_checkpoint_binding_for_mode(state, strategy, strategy.python_module.is_some())
}

pub(super) fn validate_checkpoint_binding_for_mode(
    state: &CursorState,
    strategy: &StrategyRuntimeConfig,
    checkpoint_enabled: bool,
) -> Result<(), String> {
    if !checkpoint_enabled {
        if state.strategy_checkpoint.is_some() {
            return Err(
                "当前 Strategy runtime 不支持恢复已提交 checkpoint；拒绝继续推进执行游标".into(),
            );
        }
        return Ok(());
    }
    let expected = strategy_binding_sha256(strategy)?;
    match state.strategy_binding_sha256.as_deref() {
        Some(actual) if actual != expected => Err(
            "Strategy durable checkpoint 与当前 strategy_id/version/artifact/spec 不匹配；拒绝从旧状态继续"
                .into(),
        ),
        Some(_) => {
            let expected_id = strategy.id.as_deref().unwrap_or(&strategy.version);
            if state.strategy_checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.strategy_id != expected_id
                    || checkpoint.strategy_version != strategy.version
            }) {
                return Err(
                    "Strategy checkpoint 内的 strategy_id/version 与当前配置不匹配；拒绝恢复"
                        .into(),
                );
            }
            Ok(())
        }
        None
            if state.strategy_checkpoint.is_some()
                || state.next_seq_by_log.values().any(|next_seq| *next_seq > 0) =>
        {
            Err("Strategy 执行游标已推进但没有可恢复 checkpoint；拒绝静默重启".into())
        }
        None => Ok(()),
    }
}

pub(super) fn commit_strategy_checkpoint(
    state: &mut CursorState,
    path: &Path,
    strategy: &StrategyRuntimeConfig,
    run_id: u64,
    ranges: &[CursorRange],
    checkpoint: StrategyCheckpointV1,
) -> Result<(), String> {
    if strategy.python_module.is_none()
        && strategy.external_executable.is_none()
        && strategy.c_abi_library.is_none()
    {
        return Err("拒绝为不支持 durable checkpoint 的 Strategy worker 保存 checkpoint".into());
    }
    let expected_id = strategy
        .id
        .clone()
        .unwrap_or_else(|| strategy.version.clone());
    if checkpoint.strategy_id != expected_id || checkpoint.strategy_version != strategy.version {
        return Err("Strategy checkpoint 与 worker strategy_id/version 不匹配".into());
    }
    let fingerprint = strategy_binding_sha256(strategy)?;
    if state
        .strategy_binding_sha256
        .as_deref()
        .is_some_and(|current| current != fingerprint)
    {
        return Err("Strategy checkpoint 绑定指纹在 worker 生命周期中发生变化".into());
    }
    let mut candidate = state.clone();
    advance_cursor(&mut candidate, ranges)?;
    candidate.strategy_checkpoint = Some(checkpoint);
    candidate.strategy_binding_sha256 = Some(fingerprint);
    candidate.committed_run_id = Some(run_id);
    save_cursor(path, &candidate)?;
    *state = candidate;
    Ok(())
}

fn strategy_binding_sha256(strategy: &StrategyRuntimeConfig) -> Result<String, String> {
    // external_args are part of strategy behavior and are covered by this digest; only the
    // resulting digest is persisted. Never include external_env, credentials, runtime paths,
    // or account balances in the checkpoint identity.
    let identity = serde_json::json!({
        "id": strategy.id,
        "version": strategy.version,
        "python_module": strategy.python_module,
        "external_executable": strategy.external_executable,
        "external_args": strategy.external_args,
        "strategy_artifact_sha256": strategy.strategy_artifact_sha256,
        "python_strategy_spec": strategy.python_strategy_spec,
        "c_abi_sha256": strategy.c_abi_sha256,
        "account_id": strategy.account_id,
        "venue_id": strategy.venue_id,
        "instrument": strategy.instrument,
    });
    let encoded = serde_json::to_vec(&identity)
        .map_err(|error| format!("编码 Strategy checkpoint identity 失败: {error}"))?;
    Ok(qx_strategy::sha256_hex(&encoded))
}

pub(super) fn attach_ranges_to_job(
    job: &mut JobSpec,
    run: &mut qx_scheduler::JobRun,
    ranges: &[CursorRange],
) -> Result<(), String> {
    if ranges.is_empty() {
        return Ok(());
    }
    let reference = CursorReference {
        schema_version: CURSOR_SCHEMA_VERSION,
        ranges: ranges.to_vec(),
    };
    let payload = serde_json::to_string(&reference)
        .map_err(|error| format!("编码 Strategy EventLog 游标范围失败: {error}"))?;
    let digest = qx_strategy::sha256_hex(payload.as_bytes());
    job.input_refs
        .push(format!("{JOB_INPUT_REF_PREFIX}{payload}"));
    job.idempotency_key = format!("{}:execution:{digest}", job.idempotency_key);
    run.run_id = job.stable_key(&run.trading_day);
    Ok(())
}

pub(super) fn ranges_from_job(job: &JobSpec) -> Result<Vec<CursorRange>, String> {
    let refs = job
        .input_refs
        .iter()
        .filter_map(|reference| reference.strip_prefix(JOB_INPUT_REF_PREFIX))
        .collect::<Vec<_>>();
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    if refs.len() != 1 {
        return Err("Strategy Job 含多个 execution cursor 引用".into());
    }
    let reference: CursorReference = serde_json::from_str(refs[0])
        .map_err(|error| format!("Strategy Job execution cursor 引用无效: {error}"))?;
    if reference.schema_version != CURSOR_SCHEMA_VERSION || reference.ranges.is_empty() {
        return Err("Strategy Job execution cursor 版本或范围非法".into());
    }
    Ok(reference.ranges)
}

fn replay_execution_updates(
    events: &[Event],
    strategy_id: &str,
    from_seq: u64,
    through_seq: u64,
) -> Result<ReplayedExecutionUpdates, String> {
    let mut oms = qx_zhenlu::Oms::new();
    let mut updates = ReplayedExecutionUpdates::default();
    for event in events.iter().filter(|event| event.seq <= through_seq) {
        match &event.kind {
            EventKind::OrderSubmitted { order }
                if order_strategy_id(order) == Some(strategy_id) =>
            {
                oms.insert_replayed(order.clone())
                    .map_err(|error| format!("重放 Strategy OrderSubmitted 失败: {error:?}"))?;
                if event.seq >= from_seq {
                    append_update(
                        &mut updates.order_updates,
                        &oms,
                        order.client_id,
                        strategy_id,
                        event,
                        None,
                    )?;
                }
            }
            EventKind::Accepted {
                client_order_id, ..
            } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    oms.accept(*client_order_id)
                        .map_err(|error| format!("重放 Strategy Accepted 失败: {error:?}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::Rejected {
                client_order_id,
                reason,
            } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms
                        .get_mut(*client_order_id)
                        .ok_or_else(|| "重放 Strategy Rejected 时订单消失".to_string())?;
                    order
                        .status
                        .transition(OrderStatus::Rejected)
                        .map_err(|error| format!("重放 Strategy Rejected 状态失败: {error}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            Some(reason),
                        )?;
                    }
                }
            }
            EventKind::CancelPending { client_order_id } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms
                        .get_mut(*client_order_id)
                        .ok_or_else(|| "重放 Strategy CancelPending 时订单消失".to_string())?;
                    order
                        .status
                        .transition(OrderStatus::CancelPending)
                        .map_err(|error| {
                            format!("重放 Strategy CancelPending 状态失败: {error}")
                        })?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::Cancelled { client_order_id } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    oms.cancel(*client_order_id)
                        .map_err(|error| format!("重放 Strategy Cancelled 失败: {error:?}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::Filled { fill } => {
                if is_strategy_order(&oms, fill.order_id, strategy_id) {
                    let pre_fill_order = oms
                        .get(fill.order_id)
                        .cloned()
                        .ok_or_else(|| "重放 Strategy Filled 时订单消失".to_string())?;
                    oms.apply_fill(fill)
                        .map_err(|error| format!("重放 Strategy Filled 失败: {error:?}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            fill.order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                        updates.fill_updates.push(
                            StrategyFillUpdateV2::from_fill(
                                &pre_fill_order,
                                fill,
                                strategy_id,
                                event.seq,
                            )
                            .map_err(|error| format!("构造 StrategyFillUpdateV2 失败: {error}"))?,
                        );
                    }
                }
            }
            EventKind::ReconcileRequired { client_order_id } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms
                        .get_mut(*client_order_id)
                        .ok_or_else(|| "重放 Strategy ReconcileRequired 时订单消失".to_string())?;
                    order
                        .status
                        .transition(OrderStatus::Unknown)
                        .map_err(|error| format!("重放 Strategy Unknown 状态失败: {error}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    sort_updates(&mut updates.order_updates);
    sort_fill_updates(&mut updates.fill_updates);
    Ok(updates)
}

fn order_strategy_id(order: &Order) -> Option<&str> {
    order.trace.as_ref()?.strategy_id.as_deref()
}

fn is_strategy_order(oms: &qx_zhenlu::Oms, client_order_id: u64, strategy_id: &str) -> bool {
    oms.get(client_order_id)
        .and_then(order_strategy_id)
        .is_some_and(|owner| owner == strategy_id)
}

fn append_update(
    updates: &mut Vec<StrategyOrderStatusUpdate>,
    oms: &qx_zhenlu::Oms,
    client_order_id: u64,
    strategy_id: &str,
    event: &Event,
    reason: Option<&str>,
) -> Result<(), String> {
    let order = oms
        .get(client_order_id)
        .ok_or_else(|| format!("Strategy EventLog update 缺少 OMS 订单 {client_order_id}"))?;
    updates.push(
        StrategyOrderStatusUpdate::from_order(
            order,
            strategy_id,
            event.seq,
            event.ts,
            order.status,
            reason,
        )
        .map_err(|error| format!("构造 StrategyOrderStatusUpdate 失败: {error}"))?,
    );
    Ok(())
}

fn sort_updates(updates: &mut [StrategyOrderStatusUpdate]) {
    updates.sort_by(|left, right| {
        (
            left.ts,
            left.order.account_id.as_str(),
            left.order.instrument.as_str(),
            left.event_seq,
            left.order.order_id,
        )
            .cmp(&(
                right.ts,
                right.order.account_id.as_str(),
                right.order.instrument.as_str(),
                right.event_seq,
                right.order.order_id,
            ))
    });
}

fn sort_fill_updates(updates: &mut [StrategyFillUpdateV2]) {
    updates.sort_by(|left, right| {
        (
            left.ts,
            left.order.account_id.as_str(),
            left.order.instrument.as_str(),
            left.event_seq,
            left.order.order_id,
        )
            .cmp(&(
                right.ts,
                right.order.account_id.as_str(),
                right.order.instrument.as_str(),
                right.event_seq,
                right.order.order_id,
            ))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Quantity, Side};

    fn order(client_id: u64, strategy_id: &str) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("BTCUSDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(1_000_000_000),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "acct-a".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(strategy_id.into()),
                signal_id: Some(1),
                intent_id: Some(client_id),
                rule_version: Some("v1".into()),
            }),
            policy: None,
            exact_amounts: None,
        }
    }

    #[test]
    fn account_snapshots_replay_for_non_python_strategy_hosts() {
        let mut config: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../../../deploy/qianxing.runtime.strategy-backtest.example.json"
        ))
        .expect("strategy runtime example parses");
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("paper".into());
        config.strategy.python_strategy_spec = None;
        let events = [Event::new(
            0,
            10,
            0,
            EventKind::AccountBalanceSnapshot {
                account_id: "main".into(),
                venue_id: "paper".into(),
                balances: Vec::new(),
            },
        )];

        let updates = replay_account_updates(&events, &config, 0, 0);

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].event_seq, 0);
        assert_eq!(updates[0].event, "account_snapshot");
    }

    #[test]
    fn cursor_advancement_is_monotonic_and_durable_shape_round_trips() {
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 8)]),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            committed_run_id: None,
        };
        advance_cursor(
            &mut state,
            &[
                CursorRange {
                    log_name: "acct-a.okx".into(),
                    from_seq: 3,
                    through_seq: 5,
                },
                CursorRange {
                    log_name: "acct-b.binance".into(),
                    from_seq: 0,
                    through_seq: 2,
                },
            ],
        )
        .unwrap();
        assert_eq!(state.next_seq_by_log["acct-a.okx"], 8);
        assert_eq!(state.next_seq_by_log["acct-b.binance"], 3);
        let encoded = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<CursorState>(&encoded).unwrap(),
            state
        );
    }

    #[test]
    fn cursor_load_recovers_backup_after_interrupted_atomic_replace() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-strategy-cursor-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let backup = backup_path(&path);
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 17)]),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            committed_run_id: None,
        };
        save_cursor(&path, &state).unwrap();
        std::fs::rename(&path, &backup).unwrap();
        assert_eq!(load_cursor(&path).unwrap(), state);

        let updated = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 18)]),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            committed_run_id: None,
        };
        save_cursor(&path, &updated).unwrap();
        assert_eq!(load_cursor(&path).unwrap(), updated);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn class_checkpoint_and_execution_cursor_commit_as_one_recoverable_snapshot() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-strategy-checkpoint-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("class-strategy".into());
        strategy.version = "1.2.0".into();
        strategy.python_module = Some("example_strategy".into());
        strategy.python_strategy_spec = Some(serde_json::json!({"parameters": {"window": 4}}));
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            committed_run_id: None,
        };
        let checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "class-strategy".into(),
            strategy_version: "1.2.0".into(),
            adapter_state: serde_json::json!({"schema_version": 1}),
            strategy_state: serde_json::json!({"counter": 2}),
        };
        let ranges = vec![CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 0,
            through_seq: 8,
        }];

        commit_strategy_checkpoint(
            &mut state,
            &path,
            &strategy,
            77,
            &ranges,
            checkpoint.clone(),
        )
        .unwrap();

        let recovered = load_cursor(&path).unwrap();
        assert_eq!(recovered, state);
        assert_eq!(recovered.next_seq_by_log["acct-a.okx"], 9);
        assert_eq!(strategy_checkpoint(&recovered), Some(&checkpoint));
        assert!(is_committed_run(&recovered, 77));
        validate_checkpoint_binding(&recovered, &strategy).unwrap();
        let mut changed_strategy = strategy;
        changed_strategy.version = "1.3.0".into();
        assert!(validate_checkpoint_binding(&recovered, &changed_strategy)
            .unwrap_err()
            .contains("不匹配"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn c_abi_checkpoint_binding_tracks_verified_plugin_digest() {
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("native-strategy".into());
        strategy.version = "2.0.0".into();
        strategy.c_abi_library = Some("strategy.dll".into());
        strategy.c_abi_sha256 = Some("a".repeat(64));
        let checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "native-strategy".into(),
            strategy_version: "2.0.0".into(),
            adapter_state: serde_json::json!({ "schema_version": 1, "state_mode": "c-abi" }),
            strategy_state: serde_json::json!({ "counter": 4 }),
        };
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("account.okx".into(), 9)]),
            strategy_checkpoint: Some(checkpoint),
            strategy_binding_sha256: Some(strategy_binding_sha256(&strategy).unwrap()),
            committed_run_id: Some(77),
        };
        validate_checkpoint_binding_for_mode(&state, &strategy, true).unwrap();

        strategy.c_abi_sha256 = Some("b".repeat(64));
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, true)
                .unwrap_err()
                .contains("不匹配")
        );
    }

    #[test]
    fn external_process_checkpoint_binding_tracks_artifact_digest_and_fails_closed() {
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("external-strategy".into());
        strategy.version = "1.0.0".into();
        strategy.external_executable = Some("strategy-worker.exe".into());
        strategy.strategy_artifact_sha256 = Some("c".repeat(64));
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("account.okx".into(), 5)]),
            strategy_checkpoint: Some(StrategyCheckpointV1 {
                schema_version: 1,
                strategy_id: "external-strategy".into(),
                strategy_version: "1.0.0".into(),
                adapter_state: serde_json::json!({
                    "schema_version": 1,
                    "state_mode": "external-process"
                }),
                strategy_state: serde_json::json!({ "processed_requests": 4 }),
            }),
            strategy_binding_sha256: Some(strategy_binding_sha256(&strategy).unwrap()),
            committed_run_id: Some(5),
        };
        validate_checkpoint_binding_for_mode(&state, &strategy, true).unwrap();

        strategy.strategy_artifact_sha256 = Some("d".repeat(64));
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, true)
                .unwrap_err()
                .contains("不匹配")
        );
        strategy.strategy_artifact_sha256 = Some("c".repeat(64));
        strategy.external_args.push("--mode=changed".into());
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, true)
                .unwrap_err()
                .contains("不匹配")
        );
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, false)
                .unwrap_err()
                .contains("不支持恢复")
        );
    }

    #[test]
    fn failed_checkpoint_cursor_write_preserves_committed_state_and_cleans_temp_file() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-strategy-checkpoint-write-failure-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("class-strategy".into());
        strategy.version = "1.2.0".into();
        strategy.python_module = Some("example_strategy".into());
        strategy.python_strategy_spec = Some(serde_json::json!({"parameters": {"window": 4}}));
        let old_checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "class-strategy".into(),
            strategy_version: "1.2.0".into(),
            adapter_state: serde_json::json!({"schema_version": 1, "last_bar_ts": 10}),
            strategy_state: serde_json::json!({"counter": 2}),
        };
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            committed_run_id: None,
        };
        let initial_ranges = vec![CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 0,
            through_seq: 8,
        }];
        commit_strategy_checkpoint(
            &mut state,
            &path,
            &strategy,
            77,
            &initial_ranges,
            old_checkpoint.clone(),
        )
        .unwrap();
        let committed = state.clone();

        // A directory at the backup filename deterministically makes the
        // pre-commit backup cleanup fail on Windows and Unix alike.
        let backup = backup_path(&path);
        std::fs::create_dir(&backup).unwrap();
        let mut failed_checkpoint = old_checkpoint;
        failed_checkpoint.strategy_state = serde_json::json!({"counter": 3});
        let next_ranges = vec![CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 9,
            through_seq: 11,
        }];
        let error = commit_strategy_checkpoint(
            &mut state,
            &path,
            &strategy,
            78,
            &next_ranges,
            failed_checkpoint,
        )
        .unwrap_err();

        assert!(error.contains("清理策略执行事实游标旧恢复副本失败"));
        assert_eq!(state, committed);
        assert_eq!(load_cursor(&path).unwrap(), committed);
        let temporary = path.with_extension(format!("cursor.tmp.{}", std::process::id()));
        assert!(
            !temporary.exists(),
            "failed cursor temp file must be removed"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn execution_ranges_are_part_of_idempotency_without_changing_market_manifest() {
        let (mut job, mut run) =
            qx_scheduler::live_strategy_job(qx_scheduler::LiveStrategyJobInput {
                job_version: "strategy-v1",
                worker_id: "strategy-a",
                instrument: Some("BTCUSDT.OKX"),
                data_fingerprint: 99,
                trading_day: "2026-10-25".into(),
                lease_now: 1_800_000_000,
                environment: "paper",
            });
        let market_run_id = run.run_id;
        let range = CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 12,
            through_seq: 14,
        };
        attach_ranges_to_job(&mut job, &mut run, &[range.clone()]).unwrap();
        assert_ne!(run.run_id, market_run_id);
        assert_eq!(run.run_id, job.stable_key(&run.trading_day));
        assert_eq!(run.manifest_digest, Some(99));
        assert_eq!(ranges_from_job(&job).unwrap(), vec![range]);
        assert!(job.idempotency_key.contains(":execution:"));
    }

    #[test]
    fn replay_reconstructs_prior_order_state_and_emits_only_the_requested_strategy_range() {
        let events = vec![
            Event::new(
                0,
                10,
                5,
                EventKind::OrderSubmitted {
                    order: order(11, "strategy-a"),
                },
            ),
            Event::new(
                1,
                11,
                5,
                EventKind::Accepted {
                    client_order_id: 11,
                    venue_order_id: Some("venue-11".into()),
                },
            ),
            Event::new(
                2,
                12,
                5,
                EventKind::OrderSubmitted {
                    order: order(12, "strategy-a"),
                },
            ),
            Event::new(
                3,
                13,
                5,
                EventKind::Rejected {
                    client_order_id: 12,
                    reason: "venue rejected".into(),
                },
            ),
            Event::new(
                4,
                14,
                5,
                EventKind::OrderSubmitted {
                    order: order(13, "another-strategy"),
                },
            ),
        ];
        let updates = replay_execution_updates(&events, "strategy-a", 1, 4)
            .unwrap()
            .order_updates;
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].event_seq, 1);
        assert_eq!(
            updates[0].status,
            qx_strategy::StrategyOrderStatus::Accepted
        );
        assert_eq!(updates[1].event_seq, 2);
        assert_eq!(
            updates[1].status,
            qx_strategy::StrategyOrderStatus::Submitted
        );
        assert_eq!(updates[2].event_seq, 3);
        assert_eq!(
            updates[2].status,
            qx_strategy::StrategyOrderStatus::Rejected
        );
        assert_eq!(updates[2].reason.as_deref(), Some("venue rejected"));

        let initial = replay_execution_updates(&events, "strategy-a", 0, 0)
            .unwrap()
            .order_updates;
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].event_seq, 0);
        assert_eq!(
            initial[0].status,
            qx_strategy::StrategyOrderStatus::Submitted
        );
    }

    #[test]
    fn replay_emits_event_log_bound_fill_with_pre_fill_order_snapshot() {
        let mut fill = qx_core::Fill {
            order_id: 11,
            qty: Quantity::from_raw(500_000_000),
            price: qx_core::Price::from_raw(25_000_000_000),
            ts: 12,
            account_id: "acct-a".into(),
            strategy_id: Some("strategy-a".into()),
            ..qx_core::Fill::default()
        };
        fill.venue_id = Some("OKX".into());
        let events = vec![
            Event::new(
                0,
                10,
                5,
                EventKind::OrderSubmitted {
                    order: order(11, "strategy-a"),
                },
            ),
            Event::new(
                1,
                11,
                5,
                EventKind::Accepted {
                    client_order_id: 11,
                    venue_order_id: Some("venue-11".into()),
                },
            ),
            Event::new(2, 12, 5, EventKind::Filled { fill }),
        ];

        let replayed = replay_execution_updates(&events, "strategy-a", 2, 2).unwrap();
        assert_eq!(replayed.order_updates.len(), 1);
        assert_eq!(replayed.order_updates[0].event_seq, 2);
        assert_eq!(replayed.fill_updates.len(), 1);
        assert_eq!(replayed.fill_updates[0].event_seq, 2);
        assert_eq!(replayed.fill_updates[0].ts, 12);
        assert_eq!(replayed.fill_updates[0].order.filled_raw, 0);
        assert_eq!(replayed.fill_updates[0].fill.qty.raw(), 500_000_000);
        assert_eq!(replayed.fill_updates[0].fill.order_id, 11);
    }

    #[test]
    fn frozen_range_retry_replays_multiple_partial_fills_without_absorbing_new_tail() {
        let partial_fill = |ts| qx_core::Fill {
            order_id: 11,
            qty: Quantity::from_raw(500_000_000),
            price: qx_core::Price::from_raw(25_000_000_000),
            ts,
            account_id: "acct-a".into(),
            strategy_id: Some("strategy-a".into()),
            ..qx_core::Fill::default()
        };
        let mut events = vec![
            Event::new(
                0,
                10,
                5,
                EventKind::OrderSubmitted {
                    order: order(11, "strategy-a"),
                },
            ),
            Event::new(
                1,
                11,
                5,
                EventKind::Accepted {
                    client_order_id: 11,
                    venue_order_id: Some("venue-11".into()),
                },
            ),
            Event::new(
                2,
                12,
                5,
                EventKind::Filled {
                    fill: partial_fill(12),
                },
            ),
            Event::new(
                3,
                13,
                5,
                EventKind::Filled {
                    fill: partial_fill(13),
                },
            ),
        ];

        let first_attempt = replay_execution_updates(&events, "strategy-a", 2, 3).unwrap();
        assert_eq!(first_attempt.order_updates.len(), 2);
        assert_eq!(first_attempt.fill_updates.len(), 2);
        assert_eq!(
            first_attempt
                .fill_updates
                .iter()
                .map(|update| (update.event_seq, update.order.filled_raw))
                .collect::<Vec<_>>(),
            [(2, 0), (3, 500_000_000)]
        );
        assert_eq!(
            first_attempt
                .order_updates
                .iter()
                .map(|update| (update.event_seq, update.order.filled_raw))
                .collect::<Vec<_>>(),
            [(2, 500_000_000), (3, 1_000_000_000)]
        );

        // The job retry reuses its frozen [2, 3] range even after the EventLog grows.
        events.push(Event::new(
            4,
            14,
            5,
            EventKind::OrderSubmitted {
                order: order(12, "strategy-a"),
            },
        ));
        let retry = replay_execution_updates(&events, "strategy-a", 2, 3).unwrap();
        assert_eq!(retry, first_attempt);
    }
}
