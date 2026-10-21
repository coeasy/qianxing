//! Read-only API projections derived from persisted reconciliation reports.

use qx_api::ApiQueryModels;
use qx_api::ReconcileReportSnapshot;
use qx_core::{EventKind, InstrumentId, LedgerEntry};
use qx_protocol::AccountSnapshot;
use qx_runtime::{
    account_event_log_name, owns_account_event_log, settlement_currency_for_log,
    worker_account_event_log, LiveEventPipeline, RuntimeConfig, WorkerConfig,
};
use std::collections::BTreeSet;
use std::path::Path;

use crate::pipeline::{event_log_exists, PipelineStorage};

/// Select the first configured account writer whose canonical EventLog has persisted facts.
pub fn default_account_event_log(
    config: &RuntimeConfig,
    root: &Path,
) -> Result<Option<String>, String> {
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        let Some(log_name) = worker_account_event_log(worker) else {
            continue;
        };
        if event_log_exists(config, root, &log_name)? {
            return Ok(Some(log_name));
        }
    }
    Ok(None)
}

/// Load current operational query models, including the ledger from the default account log.
pub fn query_api_models(config: &RuntimeConfig) -> Result<ApiQueryModels, String> {
    let root = Path::new(&config.storage.data_dir);
    let mut ledger_entries = Vec::new();
    if let Some(log_name) = default_account_event_log(config, root)? {
        let currency = settlement_currency_for_log(config, &log_name)?;
        let pipeline = PipelineStorage::from_config(config)?
            .open_read_only(log_name, currency)
            .map_err(|error| format!("读取 API Ledger 读模型失败: {error}"))?;
        ledger_entries = pipeline.ledger().entries().to_vec();
    }
    load_api_query_models(
        root,
        Path::new(&config.scheduler.state_path),
        ledger_entries,
    )
}

/// Load one snapshot per existing canonical account EventLog.
pub fn load_api_account_snapshots(config: &RuntimeConfig) -> Result<Vec<AccountSnapshot>, String> {
    let reports = load_reconcile_reports(Path::new(&config.storage.data_dir))?;
    let mut seen = BTreeSet::new();
    let mut snapshots = Vec::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        let Some(identity) = worker_account_event_log(worker) else {
            continue;
        };
        if !seen.insert(identity) {
            continue;
        }
        if let Some(snapshot) = load_api_account_snapshot_for_worker(config, worker, &reports)? {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

/// Open and project one account log for an API snapshot read.
pub fn load_api_account_snapshot_for_worker(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
    reports: &[ReconcileReportSnapshot],
) -> Result<Option<AccountSnapshot>, String> {
    let account_id = worker.account_id.as_deref().unwrap_or_default().trim();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default().trim();
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Ok(None);
    };
    let root = Path::new(&config.storage.data_dir);
    if !event_log_exists(config, root, &log_name)? {
        return Ok(None);
    }
    let currency = settlement_currency_for_log(config, &log_name)?;
    let pipeline = PipelineStorage::from_config(config)?
        .open_read_only(log_name.clone(), currency)
        .map_err(|error| format!("打开 API 账户 EventLog 失败: {error}"))?;
    Ok(Some(project_api_account_snapshot(
        worker, &log_name, &pipeline, reports,
    )?))
}

/// Assemble the read-only API models from service-owned persisted state and the ledger projection
/// supplied by the runtime host. The host owns backend selection; this service owns query assembly.
pub fn load_api_query_models(
    root: &Path,
    scheduler_state_path: &Path,
    ledger_entries: Vec<LedgerEntry>,
) -> Result<ApiQueryModels, String> {
    let job_runs = if root.join(scheduler_state_path).exists() {
        qx_storage::JsonStateStore::new(root)
            .load_scheduler_at(scheduler_state_path)
            .map_err(|error| format!("读取 API Scheduler 读模型失败: {error:?}"))?
            .runs()
    } else {
        Vec::new()
    };

    Ok(ApiQueryModels {
        job_runs,
        ledger_entries,
        reconcile_reports: load_reconcile_reports(root)?,
    })
}

/// Load and validate the latest persisted reconciliation report per worker.
pub fn load_reconcile_reports(root: &Path) -> Result<Vec<ReconcileReportSnapshot>, String> {
    let mut reconcile_reports = Vec::new();
    let report_root = root.join("reconcile");
    if report_root.exists() {
        for entry in std::fs::read_dir(&report_root)
            .map_err(|error| format!("读取 API 对账报告目录失败: {error}"))?
        {
            let path = entry
                .map_err(|error| format!("读取 API 对账报告目录项失败: {error}"))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let report: ReconcileReportSnapshot = serde_json::from_str(
                &std::fs::read_to_string(&path)
                    .map_err(|error| format!("读取对账报告失败 {}: {error}", path.display()))?,
            )
            .map_err(|error| format!("对账报告 JSON 无效 {}: {error}", path.display()))?;
            report
                .validate()
                .map_err(|error| format!("对账报告校验失败 {}: {error}", path.display()))?;
            reconcile_reports.push(report);
        }
        reconcile_reports.sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
    }
    Ok(reconcile_reports)
}

/// Project the reports belonging to one canonical account EventLog into its snapshot.
pub fn apply_reconcile_reports(
    snapshot: &mut AccountSnapshot,
    log_name: &str,
    reports: &[ReconcileReportSnapshot],
) {
    let mine = reports.iter().filter(|report| {
        qx_runtime::account_event_log_name(&report.account_id, &report.venue_id).as_deref()
            == Some(log_name)
    });
    let last_reconcile_ts = mine.clone().map(|report| report.observed_ts).max();
    snapshot.reconcile.last_reconcile_ts = last_reconcile_ts;
    snapshot.reconcile.discrepancy_count = last_reconcile_ts.map(|_| {
        mine.fold(0_u32, |total, report| {
            let items = report
                .order_issues
                .len()
                .saturating_add(report.balance_discrepancies.len());
            total.saturating_add(u32::try_from(items).unwrap_or(u32::MAX))
        })
    });
}

/// Project one already-open, read-only account EventLog into the API wire snapshot.
/// Opening and backend selection stay with the runtime host; account valuation and wire mapping
/// have one service owner shared by API startup and live read-model refreshes.
pub fn project_api_account_snapshot(
    worker: &WorkerConfig,
    log_name: &str,
    pipeline: &LiveEventPipeline,
    reports: &[ReconcileReportSnapshot],
) -> Result<AccountSnapshot, String> {
    let account_id = worker.account_id.as_deref().unwrap_or_default().trim();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default().trim();
    let runtime_snapshot = pipeline.snapshot();
    let mut snapshot = AccountSnapshot::new(
        1,
        account_id,
        "default",
        venue_id,
        runtime_snapshot.last_engine_ts.max(1),
    );
    snapshot.header.event_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);
    snapshot.cash_raw = pipeline.ledger().cash_balances_for(account_id);
    snapshot.equity_raw =
        pipeline
            .ledger()
            .equity_for(account_id, pipeline.marks(), pipeline.settlement_currency());
    snapshot.available_raw = Some(
        pipeline
            .ledger()
            .cash_for(account_id, pipeline.settlement_currency()),
    );
    let fill_facts = pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Filled { fill } => Some((
                event.seq,
                qx_protocol::FillSnapshot::from_fact(event.seq, fill),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut fees_raw = 0_i128;
    for (_, fill) in &fill_facts {
        fees_raw = fill.fee_raw.checked_add(fees_raw).ok_or_else(|| {
            format!("账户 {account_id} 的成交费用合计溢出，拒绝发布费用口径错误的快照")
        })?;
    }
    snapshot.fees_raw = Some(fees_raw);
    snapshot.realized_pnl_raw = pipeline.ledger().realized_pnl_for(account_id);
    snapshot.unrealized_pnl_raw =
        pipeline
            .ledger()
            .unrealized_pnl_for(account_id, pipeline.marks(), 1);
    snapshot.orders = runtime_snapshot
        .orders
        .iter()
        .map(|order| (order.client_id, qx_protocol::OrderSnapshot::from(order)))
        .collect();
    snapshot.fills = fill_facts.into_iter().collect();

    let mut instruments = BTreeSet::new();
    instruments.extend(
        worker
            .symbols
            .iter()
            .filter_map(|symbol| InstrumentId::parse(symbol)),
    );
    instruments.extend(
        snapshot
            .orders
            .values()
            .map(|order| order.instrument.clone()),
    );
    instruments.extend(
        runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .into_iter()
            .flat_map(|positions| positions.iter().map(|position| position.instrument.clone())),
    );
    for instrument in instruments {
        let position = pipeline.ledger().position_for(account_id, &instrument);
        let venue_position = runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|positions| positions.iter().find(|item| item.instrument == instrument));
        let mut wire = venue_position
            .map(qx_protocol::PositionSnapshot::from)
            .unwrap_or_else(|| qx_protocol::PositionSnapshot {
                instrument: instrument.clone(),
                quantity_raw: position.quantity.raw(),
                today_quantity_raw: position.quantity.raw(),
                average_price_raw: position.average_entry.raw(),
                mark_price_raw: pipeline
                    .marks()
                    .get(&instrument)
                    .map(|price| price.raw())
                    .unwrap_or(0),
                unrealized_pnl_raw: None,
                margin_raw: None,
            });
        if wire.quantity_raw == 0 && venue_position.is_none() {
            continue;
        }
        if wire.average_price_raw == 0 {
            wire.average_price_raw = position.average_entry.raw();
        }
        if wire.mark_price_raw == 0 {
            wire.mark_price_raw = pipeline
                .marks()
                .get(&instrument)
                .map(|price| price.raw())
                .unwrap_or(0);
        }
        snapshot.positions.insert(instrument, wire);
    }
    apply_reconcile_reports(&mut snapshot, log_name, reports);
    snapshot.reconcile.recovery_state = "eventlog-replayed".into();
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_reconcile_reports, default_account_event_log, load_api_account_snapshots,
        load_api_query_models, load_reconcile_reports, query_api_models,
    };
    use crate::pipeline::PipelineStorage;
    use qx_api::ReconcileReportSnapshot;
    use qx_core::{InstrumentId, Price, Quantity, VenueId};
    use qx_guanxing::QuoteTick;
    use qx_protocol::AccountSnapshot;
    use qx_runtime::{RuntimeConfig, RuntimeEventEnvelope};
    use std::path::{Path, PathBuf};

    fn temp_root(label: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("target")
            .join(format!(
                "qx-service-api-read-model-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).expect("create isolated test root");
        root
    }

    fn report(
        worker: &str,
        account: &str,
        venue: &str,
        ts: u64,
        orders: usize,
        balances: usize,
    ) -> ReconcileReportSnapshot {
        ReconcileReportSnapshot {
            schema_version: 1,
            worker_id: worker.into(),
            account_id: account.into(),
            venue_id: venue.into(),
            observed_ts: ts,
            order_issues: vec![serde_json::json!({"issue": "order"}); orders],
            balances_count: 0,
            balance_discrepancies: vec![serde_json::json!({"issue": "balance"}); balances],
            position_snapshots_count: None,
            funding_rate_snapshots_count: None,
            cashflow_count: None,
        }
    }

    #[test]
    fn report_loader_validates_and_sorts_by_worker_identity() {
        let root = temp_root("sort");
        let reports = root.join("reconcile");
        std::fs::create_dir_all(&reports).unwrap();
        for report in [
            report("worker-z", "main", "paper", 20, 0, 0),
            report("worker-a", "main", "paper", 10, 0, 0),
        ] {
            std::fs::write(
                reports.join(format!("{}.json", report.worker_id)),
                serde_json::to_vec(&report).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(reports.join("ignored.txt"), "not a report").unwrap();

        let loaded = load_reconcile_reports(&root).unwrap();

        assert_eq!(
            loaded
                .iter()
                .map(|report| report.worker_id.as_str())
                .collect::<Vec<_>>(),
            ["worker-a", "worker-z"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn report_projection_aggregates_only_the_canonical_account_and_preserves_absence() {
        let mut snapshot = AccountSnapshot::new(1, "main", "default", "paper", 1);
        let reports = [
            report("first", " main ", "Paper", 10, 1, 0),
            report("second", "main", "paper", 20, 0, 2),
            report("other", "shadow", "paper", 30, 8, 9),
        ];

        apply_reconcile_reports(&mut snapshot, "paper-main-paper-events", &reports);

        assert_eq!(snapshot.reconcile.last_reconcile_ts, Some(20));
        assert_eq!(snapshot.reconcile.discrepancy_count, Some(3));
        let mut no_reports = AccountSnapshot::new(2, "missing", "default", "paper", 1);
        apply_reconcile_reports(&mut no_reports, "paper-missing-paper-events", &reports);
        assert_eq!(no_reports.reconcile.last_reconcile_ts, None);
        assert_eq!(no_reports.reconcile.discrepancy_count, None);
    }

    #[test]
    fn query_model_loader_treats_missing_scheduler_state_as_empty_and_keeps_supplied_ledger() {
        let root = temp_root("query-models");
        let mut ledger = qx_core::Ledger::new();
        let entry_id = ledger
            .deposit("main", "USDT", qx_core::Money::from_i64(10), 1)
            .expect("deposit ledger fixture");
        let ledger_entry = ledger
            .entry(entry_id)
            .expect("persisted ledger entry")
            .clone();

        let loaded = load_api_query_models(
            &root,
            Path::new("scheduler.json"),
            vec![ledger_entry.clone()],
        )
        .expect("an API read model can start before the scheduler has persisted state");

        assert!(loaded.job_runs.is_empty());
        assert_eq!(loaded.ledger_entries.len(), 1);
        assert_eq!(loaded.ledger_entries[0].id, ledger_entry.id);
        assert!(loaded.reconcile_reports.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn service_api_readers_select_default_account_and_project_existing_logs() {
        let root = temp_root("service-api-pipelines");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("valid paper runtime fixture");
        config.storage.data_dir = root.to_string_lossy().into_owned();
        let log_name = "paper-main-paper-events";

        assert_eq!(default_account_event_log(&config, &root).unwrap(), None);
        let mut pipeline = PipelineStorage::from_config(&config)
            .unwrap()
            .open(log_name, "USDT")
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                QuoteTick::new(
                    10,
                    Price::from_i64(99),
                    Quantity::from_i64(2),
                    Price::from_i64(100),
                    Quantity::from_i64(3),
                    1,
                ),
                11,
                1,
                "service-api-read-test",
            ))
            .unwrap();
        drop(pipeline);

        assert_eq!(
            default_account_event_log(&config, &root)
                .unwrap()
                .as_deref(),
            Some(log_name)
        );
        let snapshots = load_api_account_snapshots(&config).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].header.account_id, "main");
        assert_eq!(snapshots[0].header.venue_id, "paper");
        assert!(query_api_models(&config).unwrap().ledger_entries.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn service_api_readers_use_the_configured_sqlite_event_log() {
        let root = temp_root("service-api-sqlite");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("valid paper runtime fixture");
        config.storage.backend = qx_runtime::StorageBackend::Sqlite;
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config.storage.sqlite_path =
            Some(root.join("event-log.sqlite").to_string_lossy().into_owned());
        let log_name = "paper-main-paper-events";
        let mut pipeline = PipelineStorage::from_config(&config)
            .unwrap()
            .open(log_name, "USDT")
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
                QuoteTick::new(
                    10,
                    Price::from_i64(99),
                    Quantity::from_i64(2),
                    Price::from_i64(100),
                    Quantity::from_i64(3),
                    1,
                ),
                11,
                1,
                "service-api-sqlite-read-test",
            ))
            .unwrap();
        drop(pipeline);

        assert_eq!(
            default_account_event_log(&config, &root)
                .unwrap()
                .as_deref(),
            Some(log_name)
        );
        assert_eq!(load_api_account_snapshots(&config).unwrap().len(), 1);
        assert!(query_api_models(&config).unwrap().ledger_entries.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
