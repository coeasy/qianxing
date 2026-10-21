//! Canonical worker ownership and settlement-currency rules for account EventLogs.

use crate::{account_event_log_name, RuntimeConfig, WorkerConfig, WorkerRole};
use qx_core::DEFAULT_SETTLEMENT_CURRENCY;
use std::collections::{BTreeMap, BTreeSet};

/// A worker's declared account EventLog identity, if both identity fields are present.
pub fn worker_account_event_log(worker: &WorkerConfig) -> Option<String> {
    account_event_log_name(worker.account_id.as_deref()?, worker.venue_id.as_deref()?)
}

/// Require an account EventLog identity when the caller is about to write account facts.
pub fn required_account_event_log(worker: &WorkerConfig) -> Result<String, String> {
    worker_account_event_log(worker).ok_or_else(|| {
        format!(
            "FAIL_CLOSED: worker {}（role={:?}）缺少 account_id/venue_id，无法确定账户级 EventLog",
            worker.id, worker.role
        )
    })
}

/// Normalize the worker-declared currency for non-account market logs and diagnostics.
pub fn worker_settlement_currency(worker: &WorkerConfig) -> String {
    worker
        .settlement_currency
        .as_deref()
        .unwrap_or(DEFAULT_SETTLEMENT_CURRENCY)
        .to_ascii_uppercase()
}

/// Whether a worker role is allowed to append account cash-ledger facts.
pub fn writes_account_ledger(worker: &WorkerConfig) -> bool {
    worker.enabled
        && matches!(
            worker.role,
            WorkerRole::UserStream
                | WorkerRole::Execution
                | WorkerRole::SpreadRecovery
                | WorkerRole::Reconciler
        )
}

/// Whether this worker owns an account-scoped EventLog write path.
pub fn owns_account_event_log(worker: &WorkerConfig) -> bool {
    writes_account_ledger(worker) && worker.account_id.is_some() && worker.venue_id.is_some()
}

/// Resolve the currency from the account registry, then validate all active writers.
pub fn settlement_currency_for_log(
    config: &RuntimeConfig,
    log_name: &str,
) -> Result<String, String> {
    if let Some(currency) = config.workers.iter().find_map(|worker| {
        let account_id = worker.account_id.as_deref()?;
        let venue_id = worker.venue_id.as_deref()?;
        (account_event_log_name(account_id, venue_id).as_deref() == Some(log_name))
            .then(|| config.account(account_id)?.settlement_currency.clone())
            .flatten()
    }) {
        return Ok(currency.to_ascii_uppercase());
    }
    settlement_currency_among_workers(&config.workers, log_name)
}

/// List each enabled account EventLog once, with its canonical account identity and currency.
///
/// Multiple writer roles may share one account/venue log. Deduplicate by the normalized log name
/// so API projections and runtime bridges never create parallel cursors for the same ledger.
pub fn configured_account_event_logs(
    config: &RuntimeConfig,
) -> Result<Vec<(String, String, String, String)>, String> {
    let mut identities = BTreeMap::<String, (String, String)>::new();
    for worker in config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
    {
        let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        else {
            continue;
        };
        let Some(log_name) = account_event_log_name(account_id, venue_id) else {
            continue;
        };
        identities
            .entry(log_name)
            .or_insert_with(|| (account_id.to_string(), venue_id.to_string()));
    }
    identities
        .into_iter()
        .map(|(log_name, (account_id, venue_id))| {
            Ok((
                account_id,
                venue_id,
                log_name.clone(),
                settlement_currency_for_log(config, &log_name)?,
            ))
        })
        .collect()
}

/// Resolve a log's currency from all enabled account-ledger writers.
fn settlement_currency_among_workers(
    workers: &[WorkerConfig],
    log_name: &str,
) -> Result<String, String> {
    let declared = account_log_currency_declarations(workers, log_name);
    let distinct = declared
        .iter()
        .map(|(_, currency)| currency.clone())
        .collect::<BTreeSet<_>>();
    if distinct.len() > 1 {
        return Err(account_log_currency_conflict_message(log_name, &declared));
    }
    Ok(distinct
        .into_iter()
        .next()
        .unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.to_string()))
}

/// Resolve a writer's account currency, rejecting disagreement before facts are persisted.
pub fn account_worker_settlement_currency(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
) -> Result<String, String> {
    if let Some(account_id) = worker.account_id.as_deref() {
        if let Some(currency) = config
            .account(account_id)
            .and_then(|account| account.settlement_currency.as_deref())
        {
            return Ok(currency.to_ascii_uppercase());
        }
    }
    account_worker_currency_among_workers(&config.workers, worker)
}

/// The worker slice must include every writer for the target account/venue.
fn account_worker_currency_among_workers(
    workers: &[WorkerConfig],
    worker: &WorkerConfig,
) -> Result<String, String> {
    settlement_currency_among_workers(workers, &required_account_event_log(worker)?)
}

fn account_log_currency_declarations(
    workers: &[WorkerConfig],
    log_name: &str,
) -> Vec<(String, String)> {
    workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
        .filter_map(|worker| {
            let name =
                account_event_log_name(worker.account_id.as_deref()?, worker.venue_id.as_deref()?)?;
            (name == log_name).then(|| (worker.id.clone(), worker_settlement_currency(worker)))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn account_log_currency_conflict_message(log_name: &str, declared: &[(String, String)]) -> String {
    let parties = declared
        .iter()
        .map(|(worker_id, currency)| format!("{worker_id}={currency}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "FAIL_CLOSED: 账户日志 {log_name} 的写入方声明了不一致的 settlement_currency（{parties}）；\
         同一本账只能有一个记账口径，请统一后重启"
    )
}

/// List every account log whose active writers disagree on the settlement currency.
fn account_log_settlement_conflicts(config: &RuntimeConfig) -> Vec<String> {
    config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
        .filter_map(|worker| {
            account_event_log_name(worker.account_id.as_deref()?, worker.venue_id.as_deref()?)
        })
        .collect::<BTreeSet<_>>()
        .iter()
        .filter_map(|name| settlement_currency_for_log(config, name).err())
        .collect()
}

/// Add the settlement-currency invariant to the runtime doctor report.
pub fn check_account_log_settlement(
    config: &RuntimeConfig,
    checks: &mut Vec<serde_json::Value>,
    failures: &mut Vec<String>,
) {
    let conflicts = account_log_settlement_conflicts(config);
    checks.push(serde_json::json!({
        "name": "account_log_settlement",
        "status": if conflicts.is_empty() { "pass" } else { "fail" },
        "message": if conflicts.is_empty() {
            "账户事实日志的启用写入方共用同一记账币种".to_string()
        } else {
            conflicts.join("；")
        }
    }));
    failures.extend(conflicts);
}

#[cfg(test)]
mod tests {
    use super::{
        account_worker_settlement_currency, configured_account_event_logs, owns_account_event_log,
        settlement_currency_for_log, writes_account_ledger, WorkerConfig, WorkerRole,
    };
    use crate::{AccountRuntimeConfig, RuntimeConfig};

    fn worker(id: &str, role: WorkerRole, currency: &str) -> WorkerConfig {
        WorkerConfig {
            id: id.into(),
            role,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("paper".into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: Some(currency.into()),
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        }
    }

    fn paper_config() -> RuntimeConfig {
        RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper runtime fixture must stay valid")
    }

    #[test]
    fn account_currency_conflicts_fail_closed_and_name_every_writer() {
        let mut config = paper_config();
        config.workers = vec![
            worker("execution-usd", WorkerRole::Execution, "USD"),
            worker("reconcile-cny", WorkerRole::Reconciler, "CNY"),
            {
                let mut disabled = worker("disabled-eur", WorkerRole::Execution, "EUR");
                disabled.enabled = false;
                disabled
            },
        ];
        let error = settlement_currency_for_log(&config, "paper-main-paper-events").unwrap_err();
        assert!(error.contains("execution-usd=USD"), "{error}");
        assert!(error.contains("reconcile-cny=CNY"), "{error}");
        assert!(!error.contains("disabled-eur"), "{error}");
    }

    #[test]
    fn account_registry_currency_is_normalized_and_shared_by_all_writers() {
        let mut config = paper_config();
        config.accounts = vec![AccountRuntimeConfig {
            id: "main".into(),
            settlement_currency: Some("cny".into()),
            initial_cash_raw: None,
        }];
        config.workers = vec![worker("execution", WorkerRole::Execution, "USD")];
        let log_name = "paper-main-paper-events";
        assert_eq!(
            settlement_currency_for_log(&config, log_name).unwrap(),
            "CNY"
        );
        assert_eq!(
            account_worker_settlement_currency(&config, &config.workers[0]).unwrap(),
            "CNY"
        );
    }

    #[test]
    fn only_enabled_account_fact_writers_own_account_logs() {
        let mut execution = worker("execution", WorkerRole::Execution, "USDT");
        assert!(writes_account_ledger(&execution));
        assert!(owns_account_event_log(&execution));
        execution.enabled = false;
        assert!(!writes_account_ledger(&execution));
        assert!(!owns_account_event_log(&execution));

        let strategy = worker("strategy", WorkerRole::Strategy, "USDT");
        assert!(!writes_account_ledger(&strategy));
        assert!(!owns_account_event_log(&strategy));
    }

    #[test]
    fn configured_account_logs_deduplicate_normalized_identity_and_exclude_non_writers() {
        let mut config = paper_config();
        config.accounts.clear();
        config.workers = vec![
            worker("execution", WorkerRole::Execution, "cny"),
            {
                let mut reconciler = worker("reconciler", WorkerRole::Reconciler, "CNY");
                reconciler.venue_id = Some("PAPER".into());
                reconciler
            },
            worker("strategy", WorkerRole::Strategy, "USDT"),
            {
                let mut disabled = worker("disabled", WorkerRole::Execution, "EUR");
                disabled.enabled = false;
                disabled
            },
        ];

        let logs = configured_account_event_logs(&config).expect("valid shared account ledger");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].2, "paper-main-paper-events");
        assert_eq!(logs[0].3, "CNY");
    }
}
