use crate::*;
use qx_execution::{ExecutionGateway, RiskContextPort};

const ALGO_PAGE_SIZE: usize = 32;

/// Advance one durable page of TWAP/VWAP parents on the account's Paper event log.
/// The market cursor is committed before matching so a worker crash cannot replay
/// one quote into a partially filled order; a quote may be skipped on that crash,
/// which is acceptable for Paper simulation and never used to certify Live fills.
#[allow(clippy::too_many_arguments)] // Worker, durable cursor, event pipeline, and run clock are separate lifecycle inputs.
pub(crate) fn run_paper_algo_execution_cycle(
    config: &RuntimeConfig,
    config_path: &Path,
    worker: &WorkerConfig,
    root: &Path,
    _log_name: &str,
    pipeline: &mut LiveEventPipeline,
    checkpoints: &qx_runtime::AlgoExecutionCheckpointStore,
    after_parent_id: Option<&str>,
    now: u64,
) -> Result<(usize, Option<String>), String> {
    let mut venue = PaperVenue::new(
        "paper",
        execution_cost_binding_from_config(config, Some(config_path))?.fee_model(),
    );
    let next_venue_id = pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Accepted {
                venue_order_id: Some(id),
                ..
            } => id
                .strip_prefix("paper-")
                .and_then(|value| value.parse::<u64>().ok()),
            _ => None,
        })
        .max()
        .unwrap_or_default()
        .saturating_add(1);
    venue
        .restore_orders_with_next_venue_id(pipeline.orders(), next_venue_id)
        .map_err(|error| format!("恢复算法 PaperVenue 订单失败: {error:?}"))?;
    let cursor_store = JsonStateStore::new(root);
    let cursor_path = format!("paper-algo-market-cursor/{}.json", worker.id);
    let last_event_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.seq)
        .unwrap_or_default();
    let mut cursor = load_or_create_market_cursor(&cursor_store, &cursor_path, last_event_seq)?;

    // Snapshot quote inputs before ingesting derived fills, since ingestion grows
    // the same EventLog that backs this pipeline.
    let quotes = pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::MarketQuote {
                instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
            } => Some((
                event.seq,
                event.ts,
                event.source_seq,
                instrument.clone(),
                *bid,
                *bid_qty,
                *ask,
                *ask_qty,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut venue_source_seq = 0_u64;
    for (event_seq, ts, source_seq, instrument, bid, bid_qty, ask, ask_qty) in quotes {
        if event_seq <= cursor {
            continue;
        }
        // Persist before the externalized Paper match. On restart the worker will
        // wait for a fresh quote instead of applying a stale partial-fill quote twice.
        let (next_cursor, should_process) =
            advance_market_cursor(&cursor_store, &cursor_path, event_seq)?;
        cursor = next_cursor;
        if !should_process {
            continue;
        }
        let quote = QuoteTick::new(ts, bid, bid_qty, ask, ask_qty, source_seq);
        let events = venue.on_quote(&instrument, quote);
        if !events.is_empty() {
            let spec =
                load_worker_market_spec_for_instrument(worker, &instrument, Some(config_path))?
                    .ok_or_else(|| {
                        format!(
                            "FAIL_CLOSED: Paper algo 成交缺少 {} 的冻结 market spec",
                            instrument
                        )
                    })?;
            ingest_venue_events_with_spec(
                pipeline,
                events,
                &format!("{}:algo-paper", worker.id),
                now,
                &mut venue_source_seq,
                &spec,
            )?;
        }
    }

    let parents = checkpoints.list_active(after_parent_id, ALGO_PAGE_SIZE)?;
    let page_cursor = (parents.len() == ALGO_PAGE_SIZE)
        .then(|| parents.last().map(|state| state.parent_id.clone()))
        .flatten();
    let mut advanced = 0;
    for state in parents {
        if worker
            .account_id
            .as_deref()
            .is_some_and(|account_id| account_id != state.parent_order.account_id)
            || (!worker.symbols.is_empty()
                && !worker.symbols.iter().any(|symbol| {
                    InstrumentId::parse(symbol).as_ref() == Some(&state.parent_order.instrument)
                }))
        {
            continue;
        }
        let (risk, position) = worker_risk_context_with_config(
            worker,
            &state.parent_order,
            pipeline,
            config,
            Some(config_path),
        )?
        .ok_or_else(|| {
            format!(
                "FAIL_CLOSED: Paper algo worker {} 缺少冻结风控配置",
                worker.id
            )
        })?;
        let instrument_spec = risk
            .instrument_spec
            .clone()
            .ok_or_else(|| "Paper algo 风控上下文缺少冻结 market spec".to_string())?;
        let risk_port = RiskContextPort {
            risk: &risk,
            position: &position,
        };
        let mut venue_port = VenuePortAdapter::new_for_any_venue(venue);
        let mut source_seq = 0_u64;
        let step = {
            let mut gateway =
                ExecutionGateway::new(&mut venue_port, pipeline, &worker.id, now, &mut source_seq)
                    .with_instrument_spec(instrument_spec);
            checkpoints.step(&state.parent_id, now, &mut gateway, &risk_port)?
        };
        venue = venue_port.into_inner();
        advanced += 1;
        eprintln!(
            "[Paper · AlgoExecution] worker={} parent={} step={step:?}",
            worker.id, state.parent_id
        );
    }
    Ok((advanced, page_cursor))
}

fn load_or_create_market_cursor(
    store: &JsonStateStore,
    path: &str,
    initial: u64,
) -> Result<u64, String> {
    match store.load_json_at::<u64>(path) {
        Ok(cursor) => Ok(cursor),
        Err(_) => match store.create_json_at(path, &initial) {
            Ok(_) => Ok(initial),
            Err(_) => store
                .load_json_at(path)
                .map_err(|error| format!("读取 Paper algo 行情游标失败: {error:?}")),
        },
    }
}

fn advance_market_cursor(
    store: &JsonStateStore,
    path: &str,
    event_seq: u64,
) -> Result<(u64, bool), String> {
    let (cursor, updated) = store
        .transact_json_at(path, |cursor: &mut u64| -> Result<bool, String> {
            let should_process = event_seq > *cursor;
            *cursor = (*cursor).max(event_seq);
            Ok(should_process)
        })
        .map_err(|error| format!("持久化 Paper algo 行情游标失败: {error:?}"))?;
    let should_process = updated?;
    Ok((cursor, should_process))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn temp_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("qx-paper-algo-cursor-{stamp}"))
    }

    #[test]
    fn market_cursor_is_monotonic_and_claims_each_quote_once() {
        let root = temp_root();
        let store = JsonStateStore::new(&root);
        assert_eq!(
            load_or_create_market_cursor(&store, "cursor.json", 8).unwrap(),
            8
        );
        assert_eq!(
            advance_market_cursor(&store, "cursor.json", 9).unwrap(),
            (9, true)
        );
        assert_eq!(
            advance_market_cursor(&store, "cursor.json", 9).unwrap(),
            (9, false)
        );
        assert_eq!(
            advance_market_cursor(&store, "cursor.json", 7).unwrap(),
            (9, false)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
