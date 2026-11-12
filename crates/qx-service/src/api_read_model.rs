//! Read-only API projections derived from persisted reconciliation reports.

use qx_api::ApiQueryModels;
use qx_api::ReconcileReportSnapshot;
use qx_core::{EventKind, ExactDecimal, InstrumentId, LedgerEntry, LedgerEntryKind, PositionSide};
use qx_protocol::{AccountSnapshot, EquityDiagnostic, EquityUnavailableReason};
use qx_runtime::{
    account_event_log_name, owns_account_event_log, settlement_currency_for_log,
    worker_account_event_log, LiveEventPipeline, RuntimeConfig, WorkerConfig, WorkerRole,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::pipeline::{event_log_exists, PipelineStorage};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountTradingDaysFile {
    schema_version: u32,
    account_trading_days: BTreeMap<String, BTreeMap<String, String>>,
}

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

/// Load snapshots using market specifications resolved by the runtime host.
/// Symbols such as `BTCUSDT.BINANCE` do not distinguish spot from perpetual contracts, so
/// valuation must use an explicit frozen product specification instead of guessing from names.
pub fn load_api_account_snapshots_with_market_specs(
    config: &RuntimeConfig,
    market_specs: &BTreeMap<InstrumentId, qx_core::TradingInstrumentSpec>,
) -> Result<Vec<AccountSnapshot>, String> {
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
        if let Some(snapshot) = load_api_account_snapshot_for_worker_with_market_specs(
            config,
            worker,
            &reports,
            market_specs,
        )? {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

fn load_api_account_snapshot_for_worker_with_market_specs(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
    reports: &[ReconcileReportSnapshot],
    market_specs: &BTreeMap<InstrumentId, qx_core::TradingInstrumentSpec>,
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
    let trading_day = configured_account_trading_day(config, account_id, venue_id)?;
    let currency = settlement_currency_for_log(config, &log_name)?;
    let pipeline = PipelineStorage::from_config(config)?
        .open_read_only(log_name.clone(), currency)
        .map_err(|error| format!("打开 API 账户 EventLog 失败: {error}"))?;
    let portfolio_id = config
        .account(account_id)
        .map(|account| {
            account
                .portfolio_id
                .as_deref()
                .unwrap_or(account.id.as_str())
        })
        // Legacy configs may omit the account registry. Keep each worker isolated by its
        // configured account id instead of silently merging every legacy account into one
        // synthetic "default" portfolio.
        .unwrap_or(account_id);
    Ok(Some(
        project_api_account_snapshot_with_portfolio_and_market_specs(
            worker,
            &log_name,
            &pipeline,
            reports,
            portfolio_id,
            market_specs,
            &trading_day,
        )?,
    ))
}

/// Read one account's current market-calendar day from the externally maintained mapping file.
/// The file nests `account_trading_days` by account and venue; timestamps are never used to
/// synthesize or infer this protocol field.
pub fn configured_account_trading_day(
    config: &RuntimeConfig,
    account_id: &str,
    venue_id: &str,
) -> Result<String, String> {
    let path = config
        .api
        .account_trading_days_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            "API 快照 v5 需要 api.account_trading_days_path 指向市场日历映射文件".to_owned()
        })?;
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取账户交易日历映射失败 path={path}: {error}"))?;
    let document: AccountTradingDaysFile = serde_json::from_str(&text)
        .map_err(|error| format!("账户交易日历映射 JSON 非法 path={path}: {error}"))?;
    if document.schema_version != 1 {
        return Err(format!("账户交易日历映射 schema_version 必须为 1: {path}"));
    }
    let trading_day = document
        .account_trading_days
        .get(account_id)
        .and_then(|venues| venues.get(venue_id))
        .ok_or_else(|| {
            format!("账户交易日历映射缺少 account_id={account_id} venue_id={venue_id}: {path}")
        })?;
    if !qx_protocol::is_valid_trading_day(trading_day) {
        return Err(format!(
            "账户交易日历映射中的 account_id={account_id} venue_id={venue_id} 不是有效 YYYYMMDD: {trading_day}"
        ));
    }
    Ok(trading_day.to_owned())
}

pub(crate) fn configured_market_specs(
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
) -> Result<BTreeMap<InstrumentId, qx_core::TradingInstrumentSpec>, String> {
    let mut specs = BTreeMap::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.instrument_spec_path.is_some())
    {
        for instrument in configured_market_spec_candidates(config, worker)? {
            let Some(spec) = crate::worker_market_spec::load_worker_market_spec_for_instrument(
                worker,
                &instrument,
                runtime_config_path,
            )?
            else {
                continue;
            };
            if let Some(existing) = specs.get(&instrument) {
                if existing != &spec {
                    return Err(format!("标的 {instrument} 配置了冲突的冻结市场规格"));
                }
            } else {
                specs.insert(instrument, spec);
            }
        }
    }
    Ok(specs)
}

fn configured_market_spec_candidates(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
) -> Result<BTreeSet<InstrumentId>, String> {
    let mut instruments = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .ok_or_else(|| format!("worker {} 的 instrument 非法: {symbol}", worker.id))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let identity = (worker.account_id.as_deref(), worker.venue_id.as_deref());
    if identity.0.is_some() && identity.1.is_some() {
        for strategy in config.workers.iter().filter(|candidate| {
            candidate.enabled
                && candidate.role == WorkerRole::Strategy
                && candidate.account_id.as_deref() == identity.0
                && candidate.venue_id.as_deref() == identity.1
        }) {
            for symbol in &strategy.symbols {
                let instrument = InstrumentId::parse(symbol).ok_or_else(|| {
                    format!("worker {} 的 instrument 非法: {symbol}", strategy.id)
                })?;
                instruments.insert(instrument);
            }
        }
        if config.strategy.account_id.as_deref() == identity.0
            && config.strategy.venue_id.as_deref() == identity.1
        {
            if let Some(symbol) = config.strategy.instrument.as_deref() {
                let instrument = InstrumentId::parse(symbol)
                    .ok_or_else(|| format!("策略 instrument 非法: {symbol}"))?;
                instruments.insert(instrument);
            }
        }
    }
    Ok(instruments)
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
        order_groups: load_order_groups(root)?,
    })
}

fn load_order_groups(root: &Path) -> Result<Vec<qx_core::OrderGroupState>, String> {
    use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};

    let groups_root = root.join("order-groups");
    if !groups_root.exists() {
        return Ok(Vec::new());
    }
    let store = FileOrderGroupStore::new(groups_root)?;
    store
        .group_ids()?
        .into_iter()
        .map(|group_id| {
            store
                .load(&group_id)?
                .ok_or_else(|| format!("订单组索引包含已消失的组: {group_id}"))
        })
        .collect()
}

/// Load and validate the latest persisted reconciliation report per worker.
pub fn load_reconcile_reports(root: &Path) -> Result<Vec<ReconcileReportSnapshot>, String> {
    qx_runtime::load_reconcile_reports(root)
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
    trading_day: &str,
) -> Result<AccountSnapshot, String> {
    project_api_account_snapshot_with_portfolio(
        worker,
        log_name,
        pipeline,
        reports,
        "default",
        trading_day,
    )
}

pub fn project_api_account_snapshot_with_portfolio(
    worker: &WorkerConfig,
    log_name: &str,
    pipeline: &LiveEventPipeline,
    reports: &[ReconcileReportSnapshot],
    portfolio_id: &str,
    trading_day: &str,
) -> Result<AccountSnapshot, String> {
    project_api_account_snapshot_with_portfolio_and_market_specs(
        worker,
        log_name,
        pipeline,
        reports,
        portfolio_id,
        &BTreeMap::new(),
        trading_day,
    )
}

pub fn project_api_account_snapshot_with_portfolio_and_market_specs(
    worker: &WorkerConfig,
    log_name: &str,
    pipeline: &LiveEventPipeline,
    reports: &[ReconcileReportSnapshot],
    portfolio_id: &str,
    market_specs: &BTreeMap<InstrumentId, qx_core::TradingInstrumentSpec>,
    trading_day: &str,
) -> Result<AccountSnapshot, String> {
    let account_id = worker.account_id.as_deref().unwrap_or_default().trim();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default().trim();
    let runtime_snapshot = pipeline.snapshot();
    let mut snapshot = AccountSnapshot::new(
        1,
        account_id,
        portfolio_id,
        venue_id,
        trading_day,
        runtime_snapshot.last_engine_ts.max(1),
    );
    snapshot.header.event_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);
    let account_balances = runtime_snapshot
        .account_balances
        .get(&(account_id.to_string(), venue_id.to_string()));
    if let Some(balances) = account_balances {
        snapshot
            .upsert_balances(balances)
            .map_err(|error| format!("账户 {account_id} 精确余额投影失败: {error:?}"))?;
    } else {
        snapshot.cash_raw = pipeline.ledger().cash_balances_for(account_id);
        snapshot.cash_exact = pipeline.ledger().cash_balances_exact_for(account_id);
    }
    let single_quote_spot = account_is_single_quote_spot(
        pipeline,
        account_id,
        pipeline.settlement_currency(),
        &snapshot.cash_raw,
        &snapshot.cash_exact,
        market_specs,
    );
    snapshot.equity_raw = if single_quote_spot {
        if account_balances.is_some() {
            // An exchange balance snapshot already contains the base-asset cash for spot holdings.
            // Adding ledger position value would count that asset twice, so only value a flat
            // account here; non-flat or multi-asset accounts need the FX/spec-aware portfolio path.
            (!account_has_open_positions(pipeline, account_id, venue_id, &runtime_snapshot))
                .then_some(
                    snapshot
                        .cash_raw
                        .get(pipeline.settlement_currency())
                        .copied()
                        .unwrap_or(0),
                )
        } else {
            let valuation = qx_core::ValuationContext::spot(
                account_id,
                pipeline.marks(),
                pipeline.settlement_currency(),
            );
            pipeline
                .ledger()
                .valuate(&valuation)
                .ok()
                .map(|result| result.equity.raw())
        }
    } else {
        None
    };
    snapshot.equity_diagnostic = if snapshot.equity_raw.is_some() {
        None
    } else if !single_quote_spot {
        Some(EquityDiagnostic {
            reason: EquityUnavailableReason::UnsupportedPortfolioValuation,
            missing_mark_instruments: Vec::new(),
        })
    } else if account_balances.is_some()
        && account_has_open_positions(pipeline, account_id, venue_id, &runtime_snapshot)
    {
        Some(EquityDiagnostic {
            reason: EquityUnavailableReason::ExchangeBalanceIncludesSpotPositions,
            missing_mark_instruments: Vec::new(),
        })
    } else {
        let missing_mark_instruments =
            account_missing_mark_instruments(pipeline, account_id, venue_id, &runtime_snapshot);
        Some(if missing_mark_instruments.is_empty() {
            EquityDiagnostic {
                reason: EquityUnavailableReason::ArithmeticOverflow,
                missing_mark_instruments,
            }
        } else {
            EquityDiagnostic {
                reason: EquityUnavailableReason::MissingMarkPrices,
                missing_mark_instruments,
            }
        })
    };
    snapshot.available_raw = Some(if let Some(balances) = account_balances {
        balances
            .iter()
            .find(|balance| {
                balance
                    .asset
                    .eq_ignore_ascii_case(pipeline.settlement_currency())
            })
            .map(|balance| {
                balance
                    .free
                    .raw()
                    .checked_sub(balance.borrowed.raw())
                    .ok_or_else(|| format!("账户 {account_id} 可用余额折算溢出"))
            })
            .transpose()?
            .unwrap_or(0)
    } else {
        pipeline
            .ledger()
            .cash_for(account_id, pipeline.settlement_currency())
    });
    snapshot.margin_raw =
        paper_account_initial_margin_raw(account_id, venue_id, pipeline, market_specs);
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
    let mut fees_in_one_currency = true;
    let mut fees_by_currency_raw = BTreeMap::<String, i128>::new();
    for entry in pipeline.ledger().entries().iter().filter(|entry| {
        entry.account_id == account_id && matches!(&entry.kind, LedgerEntryKind::Fee)
    }) {
        let currency = entry.currency.trim().to_ascii_uppercase();
        if currency.is_empty() {
            return Err(format!("账户 {account_id} 的成交费用缺少币种"));
        }
        let fee = entry
            .amount
            .raw()
            .checked_neg()
            .ok_or_else(|| format!("账户 {account_id} 的成交费用取反溢出"))?;
        let currency_total = fees_by_currency_raw.entry(currency.clone()).or_default();
        *currency_total = currency_total
            .checked_add(fee)
            .ok_or_else(|| format!("账户 {account_id} 的 {currency} 成交费用合计溢出"))?;
        if !currency.eq_ignore_ascii_case(pipeline.settlement_currency()) {
            fees_in_one_currency = false;
            continue;
        }
        fees_raw = fees_raw
            .checked_add(fee)
            .ok_or_else(|| format!("账户 {account_id} 的成交费用合计溢出"))?;
    }
    // The wire scalar has no currency tag. Publishing a sum across (for example) USDT and BTC
    // would be dimensionally wrong, so expose it only when every fee is in the log's currency.
    snapshot.fees_raw = fees_in_one_currency.then_some(fees_raw);
    snapshot.fees_by_currency_raw = fees_by_currency_raw;
    snapshot.realized_pnl_raw = single_quote_spot
        .then(|| pipeline.ledger().realized_pnl_for(account_id))
        .flatten();
    snapshot.unrealized_pnl_raw = single_quote_spot
        .then(|| {
            pipeline
                .ledger()
                .unrealized_pnl_for(account_id, pipeline.marks(), 1)
        })
        .flatten();
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
        let exact_position =
            pipeline
                .ledger()
                .position_for_side_exact(account_id, &instrument, PositionSide::Net);
        let exact_quantity = if exact_position.quantity.is_zero() && position.quantity.raw() != 0 {
            ExactDecimal::from_parts(position.quantity.raw(), 9)
                .map_err(|error| format!("Legacy position quantity is invalid: {error}"))?
        } else {
            exact_position.quantity
        };
        let exact_average_entry =
            if exact_position.average_entry.is_zero() && position.average_entry.raw() != 0 {
                ExactDecimal::from_parts(position.average_entry.raw(), 9)
                    .map_err(|error| format!("Legacy average entry is invalid: {error}"))?
            } else {
                exact_position.average_entry
            };
        let venue_position = runtime_snapshot
            .account_positions
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|positions| positions.iter().find(|item| item.instrument == instrument));
        let mut wire = venue_position
            .map(qx_protocol::PositionSnapshot::from)
            .unwrap_or_else(|| qx_protocol::PositionSnapshot {
                instrument: instrument.clone(),
                settlement_currency: None,
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
                exact_amounts: Some(qx_core::ExactAccountPositionAmounts {
                    quantity: exact_quantity,
                    average_price: (position.average_entry.raw() != 0)
                        .then_some(exact_average_entry),
                    mark_price: pipeline
                        .marks()
                        .get(&instrument)
                        .and_then(|price| ExactDecimal::from_parts(price.raw(), 9).ok()),
                    liquidation_price: None,
                    unrealized_pnl: None,
                    initial_margin: None,
                    maintenance_margin: None,
                }),
            });
        if wire.quantity_raw == 0
            && venue_position.is_none()
            && wire
                .exact_amounts
                .is_none_or(|amounts| amounts.quantity.is_zero())
        {
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
        if let Some(spec) = market_specs.get(&instrument) {
            project_position_unrealized_pnl(&mut wire, spec)?;
        }
        snapshot.positions.insert(instrument, wire);
    }
    apply_reconcile_reports(&mut snapshot, log_name, reports);
    snapshot.reconcile.recovery_state = "eventlog-replayed".into();
    Ok(snapshot)
}

fn paper_account_initial_margin_raw(
    account_id: &str,
    venue_id: &str,
    pipeline: &LiveEventPipeline,
    market_specs: &BTreeMap<InstrumentId, qx_core::TradingInstrumentSpec>,
) -> Option<i128> {
    if !venue_id.eq_ignore_ascii_case("paper") {
        return None;
    }

    let ledger = pipeline.ledger();
    let mut held_instruments = BTreeSet::new();
    for entry in ledger
        .entries()
        .iter()
        .filter(|entry| entry.account_id == account_id)
    {
        let Some(instrument) = entry.instrument.as_ref() else {
            continue;
        };
        let aggregate = ledger.position_for(account_id, instrument);
        let has_position = !aggregate.quantity.is_zero()
            || [PositionSide::Long, PositionSide::Short]
                .into_iter()
                .any(|side| {
                    !ledger
                        .position_for_side(account_id, instrument, side)
                        .quantity
                        .is_zero()
                });
        if has_position {
            held_instruments.insert(instrument.clone());
        }
    }
    if held_instruments
        .iter()
        .any(|instrument| !market_specs.contains_key(instrument))
    {
        return None;
    }

    let reporting_currency = pipeline.settlement_currency().trim().to_ascii_uppercase();
    let mut required_fx = ledger
        .cash_balances_for(account_id)
        .into_iter()
        .filter(|(_, amount)| *amount != 0)
        .map(|(currency, _)| currency.trim().to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    for instrument in &held_instruments {
        let spec = market_specs.get(instrument)?;
        required_fx.insert(if spec.product == qx_core::TradingProduct::Spot {
            spec.quote_currency.trim().to_ascii_uppercase()
        } else {
            spec.settlement_currency.trim().to_ascii_uppercase()
        });
    }
    required_fx.remove(&reporting_currency);
    let fx_rates = crate::risk_host::spot_fx_rates_to_reporting(
        market_specs,
        pipeline.marks(),
        required_fx,
        &reporting_currency,
    )
    .ok()?;
    let context = qx_core::PaperValuationContext::new(
        account_id,
        pipeline.marks(),
        &reporting_currency,
        market_specs,
        &fx_rates,
    );
    let result = ledger.valuate_paper(&context).ok()?;
    Some(result.initial_margin.raw())
}

/// Fill position PnL only when the venue omitted it and the read model has a frozen market
/// specification plus complete quantity, entry, and mark prices. The exact value stays on the
/// scale-aware sidecar; the legacy raw field is a truncated 1e-9 projection.
fn project_position_unrealized_pnl(
    position: &mut qx_protocol::PositionSnapshot,
    spec: &qx_core::TradingInstrumentSpec,
) -> Result<(), String> {
    if position.unrealized_pnl_raw.is_some()
        || position
            .exact_amounts
            .as_ref()
            .is_some_and(|amounts| amounts.unrealized_pnl.is_some())
    {
        return Ok(());
    }
    if spec.instrument != position.instrument {
        return Err(format!(
            "持仓 {} 的估值规格标的身份不匹配: {}",
            position.instrument, spec.instrument
        ));
    }
    spec.validate()
        .map_err(|error| format!("持仓 {} 的估值规格非法: {error}", position.instrument))?;
    if position
        .settlement_currency
        .as_deref()
        .is_some_and(|currency| {
            !currency.trim().is_empty()
                && !currency.eq_ignore_ascii_case(spec.settlement_currency.trim())
        })
    {
        return Err(format!(
            "持仓 {} 的结算币种与冻结规格不匹配",
            position.instrument
        ));
    }

    let exact_amounts = position.exact_amounts.as_ref();
    let quantity = exact_amounts
        .map(|amounts| amounts.quantity)
        .or_else(|| ExactDecimal::from_parts(position.quantity_raw, 9).ok());
    let average_price = exact_amounts
        .and_then(|amounts| amounts.average_price)
        .or_else(|| ExactDecimal::from_parts(position.average_price_raw, 9).ok());
    let mark_price = exact_amounts
        .and_then(|amounts| amounts.mark_price)
        .or_else(|| ExactDecimal::from_parts(position.mark_price_raw, 9).ok());
    let (Some(quantity), Some(average_price), Some(mark_price)) =
        (quantity, average_price, mark_price)
    else {
        return Ok(());
    };
    if quantity.is_zero() || !average_price.is_positive() || !mark_price.is_positive() {
        return Ok(());
    }

    let pnl = spec
        .unrealized_pnl_exact(quantity, average_price, mark_price)
        .map_err(|error| format!("持仓 {} 未实现盈亏计算失败: {error}", position.instrument))?;
    position.unrealized_pnl_raw = Some(pnl.to_raw_truncated(9).map_err(|error| {
        format!(
            "持仓 {} 未实现盈亏兼容折算失败: {error}",
            position.instrument
        )
    })?);
    if position
        .settlement_currency
        .as_deref()
        .is_none_or(|currency| currency.trim().is_empty())
    {
        position.settlement_currency = Some(spec.settlement_currency.trim().to_ascii_uppercase());
    }
    let amounts = position
        .exact_amounts
        .get_or_insert(qx_core::ExactAccountPositionAmounts {
            quantity,
            average_price: Some(average_price),
            mark_price: Some(mark_price),
            liquidation_price: None,
            unrealized_pnl: None,
            initial_margin: None,
            maintenance_margin: None,
        });
    amounts.unrealized_pnl = Some(pnl);
    Ok(())
}

/// The legacy account wire scalars do not carry a currency or product specification. Keep the
/// simple spot/single-quote presentation valid and leave equity/PnL absent for derivatives,
/// alternate quote currencies, or unparseable native symbols until an FX/spec-aware projection
/// is available. This prevents plausible-looking but incorrect totals in multi-market accounts.
fn account_is_single_quote_spot(
    pipeline: &LiveEventPipeline,
    account_id: &str,
    reporting_currency: &str,
    cash_by_currency: &BTreeMap<String, i128>,
    cash_by_currency_exact: &BTreeMap<String, ExactDecimal>,
    market_specs: &BTreeMap<InstrumentId, qx_core::TradingInstrumentSpec>,
) -> bool {
    if cash_by_currency_exact.iter().any(|(currency, amount)| {
        !amount.is_zero() && !currency.eq_ignore_ascii_case(reporting_currency)
    }) {
        return false;
    }
    if cash_by_currency.iter().any(|(currency, amount)| {
        *amount != 0 && !currency.eq_ignore_ascii_case(reporting_currency)
    }) {
        return false;
    }
    let instruments = pipeline
        .ledger()
        .entries()
        .iter()
        .filter(|entry| entry.account_id == account_id)
        .filter_map(|entry| entry.instrument.as_ref())
        .collect::<BTreeSet<_>>();
    instruments.into_iter().all(|instrument| {
        if let Some(spec) = market_specs.get(instrument) {
            return spec.product == qx_core::TradingProduct::Spot
                && spec.quote_currency.eq_ignore_ascii_case(reporting_currency)
                && spec
                    .settlement_currency
                    .eq_ignore_ascii_case(reporting_currency);
        }
        let Some((_, quote)) = instrument.symbol().split_once('/') else {
            return false;
        };
        !quote.contains(':') && quote.eq_ignore_ascii_case(reporting_currency)
    })
}

fn account_has_open_positions(
    pipeline: &LiveEventPipeline,
    account_id: &str,
    venue_id: &str,
    snapshot: &qx_runtime::LivePipelineSnapshot,
) -> bool {
    let venue_has_position = snapshot
        .account_positions
        .get(&(account_id.to_string(), venue_id.to_string()))
        .is_some_and(|positions| {
            positions.iter().any(|position| {
                position
                    .exact_amounts
                    .map_or(!position.quantity.is_zero(), |amounts| {
                        !amounts.quantity.is_zero()
                    })
            })
        });
    if venue_has_position {
        return true;
    }
    pipeline
        .ledger()
        .entries()
        .iter()
        .filter(|entry| entry.account_id == account_id)
        .filter_map(|entry| entry.instrument.as_ref())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .any(|instrument| {
            [PositionSide::Net, PositionSide::Long, PositionSide::Short]
                .into_iter()
                .any(|side| {
                    !pipeline
                        .ledger()
                        .position_for_side_exact(account_id, instrument, side)
                        .quantity
                        .is_zero()
                })
        })
}

fn account_missing_mark_instruments(
    pipeline: &LiveEventPipeline,
    account_id: &str,
    venue_id: &str,
    snapshot: &qx_runtime::LivePipelineSnapshot,
) -> Vec<String> {
    let mut instruments = BTreeSet::new();
    instruments.extend(
        snapshot
            .account_positions
            .iter()
            .filter(|((account, venue), _)| account == account_id && venue == venue_id)
            .flat_map(|(_, positions)| {
                positions
                    .iter()
                    .filter(|position| {
                        position
                            .exact_amounts
                            .map_or(!position.quantity.is_zero(), |amounts| {
                                !amounts.quantity.is_zero()
                            })
                    })
                    .map(|position| position.instrument.clone())
            }),
    );
    instruments.extend(
        pipeline
            .ledger()
            .entries()
            .iter()
            .filter(|entry| entry.account_id == account_id)
            .filter_map(|entry| entry.instrument.clone())
            .filter(|instrument| {
                [PositionSide::Net, PositionSide::Long, PositionSide::Short]
                    .into_iter()
                    .any(|side| {
                        !pipeline
                            .ledger()
                            .position_for_side_exact(account_id, instrument, side)
                            .quantity
                            .is_zero()
                    })
            }),
    );
    let mut missing = instruments
        .into_iter()
        .filter(|instrument| !pipeline.marks().contains_key(instrument))
        .map(|instrument| instrument.to_string())
        .collect::<Vec<_>>();
    missing.sort_unstable();
    missing
}

#[cfg(test)]
mod tests {
    use super::{
        apply_reconcile_reports, configured_account_trading_day, default_account_event_log,
        load_api_account_snapshots_with_market_specs, load_api_query_models,
        load_reconcile_reports, project_position_unrealized_pnl, query_api_models,
    };
    use crate::pipeline::PipelineStorage;
    use qx_api::ReconcileReportSnapshot;
    use qx_core::{
        AccountBalance, ExactAccountBalanceAmounts, ExactDecimal, InstrumentId, Money, Order,
        OrderStatus, Price, Quantity, Side, TradingInstrumentSpec, VenueId,
    };
    use qx_guanxing::QuoteTick;
    use qx_protocol::AccountSnapshot;
    use qx_runtime::{RuntimeConfig, RuntimeEventEnvelope, RuntimeExternalEvent};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn position_unrealized_pnl_uses_frozen_spec_and_keeps_exact_result() {
        let instrument = InstrumentId::new("BTCUSDT", VenueId::new("BINANCE"))
            .expect("valid instrument fixture");
        let mut position = qx_protocol::PositionSnapshot {
            instrument: instrument.clone(),
            settlement_currency: None,
            quantity_raw: Quantity::from_i64(2).raw(),
            today_quantity_raw: Quantity::from_i64(2).raw(),
            average_price_raw: Price::from_i64(100).raw(),
            mark_price_raw: Price::from_i64(110).raw(),
            unrealized_pnl_raw: None,
            margin_raw: None,
            exact_amounts: Some(qx_core::ExactAccountPositionAmounts {
                quantity: ExactDecimal::parse("2").expect("quantity"),
                average_price: Some(ExactDecimal::parse("100").expect("entry price")),
                mark_price: Some(ExactDecimal::parse("110").expect("mark price")),
                liquidation_price: None,
                unrealized_pnl: None,
                initial_margin: None,
                maintenance_margin: None,
            }),
        };
        let spec = TradingInstrumentSpec::spot_default(&instrument, "usdt");

        project_position_unrealized_pnl(&mut position, &spec)
            .expect("frozen spec can value the position");

        assert_eq!(position.unrealized_pnl_raw, Some(20 * qx_core::SCALE));
        assert_eq!(position.settlement_currency.as_deref(), Some("USDT"));
        assert_eq!(
            position
                .exact_amounts
                .and_then(|amounts| amounts.unrealized_pnl),
            Some(ExactDecimal::parse("20").expect("exact PnL"))
        );
    }

    #[test]
    fn position_unrealized_pnl_preserves_exchange_value_and_missing_inputs() {
        let instrument = InstrumentId::new("BTCUSDT", VenueId::new("BINANCE"))
            .expect("valid instrument fixture");
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        let mut reported = qx_protocol::PositionSnapshot {
            instrument: instrument.clone(),
            settlement_currency: Some("USDT".into()),
            quantity_raw: Quantity::from_i64(2).raw(),
            today_quantity_raw: Quantity::from_i64(2).raw(),
            average_price_raw: Price::from_i64(100).raw(),
            mark_price_raw: Price::from_i64(110).raw(),
            unrealized_pnl_raw: Some(7 * qx_core::SCALE),
            margin_raw: None,
            exact_amounts: None,
        };
        project_position_unrealized_pnl(&mut reported, &spec)
            .expect("exchange-reported PnL remains authoritative");
        assert_eq!(reported.unrealized_pnl_raw, Some(7 * qx_core::SCALE));

        let mut mismatched_currency = reported.clone();
        mismatched_currency.unrealized_pnl_raw = None;
        mismatched_currency.settlement_currency = Some("BTC".into());
        assert!(project_position_unrealized_pnl(&mut mismatched_currency, &spec).is_err());

        let mut missing_mark = qx_protocol::PositionSnapshot {
            instrument,
            settlement_currency: None,
            quantity_raw: Quantity::from_i64(2).raw(),
            today_quantity_raw: Quantity::from_i64(2).raw(),
            average_price_raw: Price::from_i64(100).raw(),
            mark_price_raw: 0,
            unrealized_pnl_raw: None,
            margin_raw: None,
            exact_amounts: None,
        };
        project_position_unrealized_pnl(&mut missing_mark, &spec)
            .expect("missing mark leaves PnL unknown");
        assert_eq!(missing_mark.unrealized_pnl_raw, None);
        assert_eq!(missing_mark.exact_amounts, None);
    }

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

    fn configure_account_trading_day(config: &mut RuntimeConfig, root: &Path) {
        let path = root.join("account-trading-days.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"account_trading_days":{"main":{"paper":"20260911"}}}"#,
        )
        .expect("write explicit account calendar mapping");
        config.api.account_trading_days_path = Some(path.to_string_lossy().into_owned());
    }

    #[test]
    fn api_snapshot_trading_day_comes_from_the_account_calendar_file() {
        let root = temp_root("account-trading-day-calendar");
        std::fs::create_dir_all(&root).expect("create calendar fixture directory");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("valid paper runtime fixture");
        assert!(configured_account_trading_day(&config, "main", "paper").is_err());

        configure_account_trading_day(&mut config, &root);
        assert_eq!(
            configured_account_trading_day(&config, "main", "paper").unwrap(),
            "20260911"
        );
        let path = config.api.account_trading_days_path.as_ref().unwrap();
        std::fs::write(
            path,
            r#"{"schema_version":1,"account_trading_days":{"main":{"paper":"20260230"}}}"#,
        )
        .expect("replace calendar mapping with an invalid day");
        assert!(configured_account_trading_day(&config, "main", "paper")
            .unwrap_err()
            .contains("不是有效 YYYYMMDD"));
        std::fs::remove_dir_all(root).unwrap();
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
        let mut snapshot = AccountSnapshot::new(1, "main", "default", "paper", "20260911", 1);
        let reports = [
            report("first", " main ", "Paper", 10, 1, 0),
            report("second", "main", "paper", 20, 0, 2),
            report("other", "shadow", "paper", 30, 8, 9),
        ];

        apply_reconcile_reports(&mut snapshot, "paper-main-paper-events", &reports);

        assert_eq!(snapshot.reconcile.last_reconcile_ts, Some(20));
        assert_eq!(snapshot.reconcile.discrepancy_count, Some(3));
        let mut no_reports = AccountSnapshot::new(2, "missing", "default", "paper", "20260911", 1);
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
        configure_account_trading_day(&mut config, &root);
        let log_name = "paper-main-paper-events";

        assert_eq!(default_account_event_log(&config, &root).unwrap(), None);
        let mut pipeline = PipelineStorage::from_config(&config)
            .unwrap()
            .open(log_name, "USDT")
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE"))
                    .expect("valid instrument fixture"),
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
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    balances: vec![AccountBalance {
                        asset: "USDT".into(),
                        free: Money::ZERO,
                        locked: Money::ZERO,
                        borrowed: Money::ZERO,
                        net_cash_balance: None,
                        exact_amounts: Some(ExactAccountBalanceAmounts {
                            free: ExactDecimal::parse("0.0000000003").unwrap(),
                            locked: ExactDecimal::ZERO,
                            borrowed: ExactDecimal::ZERO,
                            net_cash_balance: None,
                        }),
                    }],
                },
                12,
                12,
                2,
                "service-api-read-balance-test",
            ))
            .unwrap();
        drop(pipeline);

        assert_eq!(
            default_account_event_log(&config, &root)
                .unwrap()
                .as_deref(),
            Some(log_name)
        );
        let snapshots =
            load_api_account_snapshots_with_market_specs(&config, &BTreeMap::new()).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].header.account_id, "main");
        assert_eq!(snapshots[0].header.venue_id, "paper");
        assert_eq!(snapshots[0].cash_raw["USDT"], 0);
        assert_eq!(snapshots[0].cash_exact["USDT"].to_string(), "0.0000000003");
        assert!(query_api_models(&config).unwrap().ledger_entries.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_snapshot_names_every_open_position_missing_a_mark() {
        let root = temp_root("missing-equity-mark-diagnostic");
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("valid paper runtime fixture");
        config.storage.data_dir = root.to_string_lossy().into_owned();
        configure_account_trading_day(&mut config, &root);
        let log_name = "paper-main-paper-events";
        let mut pipeline = PipelineStorage::from_config(&config)
            .unwrap()
            .open(log_name, "USDT")
            .unwrap();
        let instrument = InstrumentId::parse("ETH/USDT.BINANCE").expect("spot instrument");
        pipeline
            .register_order(
                Order {
                    client_id: 9001,
                    instrument: instrument.clone(),
                    side: Side::Buy,
                    qty: Quantity::from_i64(1),
                    limit: None,
                    status: OrderStatus::PendingSubmit,
                    filled: Quantity::ZERO,
                    account_id: "main".into(),
                    trace: None,
                    policy: None,
                    exact_amounts: None,
                },
                1,
            )
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 9001,
                    venue_order_id: Some("paper-9001".into()),
                },
                2,
                2,
                1,
                "missing-mark-diagnostic-accepted",
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill {
                    fill: qx_core::Fill {
                        order_id: 9001,
                        qty: Quantity::from_i64(1),
                        price: Price::from_i64(100),
                        ts: 10,
                        ..qx_core::Fill::default()
                    },
                },
                10,
                10,
                2,
                "missing-mark-diagnostic-fill",
            ))
            .unwrap();
        drop(pipeline);

        let snapshot = load_api_account_snapshots_with_market_specs(&config, &BTreeMap::new())
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.header.account_id == "main")
            .expect("replayed account snapshot");
        assert_eq!(snapshot.equity_raw, None);
        let diagnostic = snapshot.equity_diagnostic.expect("equity diagnostic");
        assert_eq!(
            diagnostic.reason,
            qx_protocol::EquityUnavailableReason::MissingMarkPrices
        );
        assert_eq!(
            diagnostic.missing_mark_instruments,
            vec![instrument.to_string()]
        );
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
        configure_account_trading_day(&mut config, &root);
        let log_name = "paper-main-paper-events";
        let mut pipeline = PipelineStorage::from_config(&config)
            .unwrap()
            .open(log_name, "USDT")
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::new("BTCUSDT", VenueId::new("BINANCE"))
                    .expect("valid instrument fixture"),
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
        assert_eq!(
            load_api_account_snapshots_with_market_specs(&config, &BTreeMap::new())
                .unwrap()
                .len(),
            1
        );
        assert!(query_api_models(&config).unwrap().ledger_entries.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
