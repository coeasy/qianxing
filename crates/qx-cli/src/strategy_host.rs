//! 跨语言策略宿主：Python 子进程、受信任 C ABI 动态库与内置策略的契约装配。
//!
//! 三种策略传输在这里收敛为同一份 `StrategyContractInput/Output`，
//! 使回测、Paper 与 Live worker 共享同一套指纹与超时约束。

mod builtin;
mod c_abi;
pub(crate) use c_abi::{
    invoke_c_abi_strategy, invoke_c_abi_strategy_with_execution_updates, load_c_abi_strategy,
};
mod execution_policy;
mod imports;
pub(crate) use builtin::invoke_builtin_strategy;
mod process_client;
mod providers;
mod python_client;
pub(crate) use execution_policy::SandboxExecutionPolicy;
use imports::*;
#[cfg(test)]
pub(crate) use qx_orchestrator::shared_ring_arguments;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
mod strategy_client;
pub(crate) use python_client::{
    invoke_python_strategy, invoke_python_strategy_with_client, PythonStrategyClient,
    StrategyProcessClient,
};
pub(crate) use strategy_client::ContractStrategyClient;
// Production reads strategy.python_timeout_ms; this constant only pins the test default.
#[cfg(test)]
pub(crate) const PYTHON_STRATEGY_TIMEOUT_MS: u64 = 2_000;
static SANDBOX_AUDIT_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn sandbox_audit_path(root: &Path, host_worker_id: &str) -> PathBuf {
    let worker_digest = qx_strategy::sha256_hex(host_worker_id.as_bytes());
    let sequence = SANDBOX_AUDIT_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    root.join("strategy-sandbox-audit").join(format!(
        "{}-{timestamp_ms}-{}-{sequence}.jsonl",
        &worker_digest[..16],
        std::process::id()
    ))
}

#[cfg(test)]
mod sandbox_audit_path_tests {
    use super::sandbox_audit_path;
    use std::path::Path;

    #[test]
    fn sandbox_audit_path_is_unique_and_does_not_expose_worker_id() {
        let first = sandbox_audit_path(Path::new("runtime-data"), "strategy-worker-secret-id");
        let second = sandbox_audit_path(Path::new("runtime-data"), "strategy-worker-secret-id");

        assert!(first.starts_with(Path::new("runtime-data/strategy-sandbox-audit")));
        assert_ne!(first, second);
        assert!(!first
            .to_string_lossy()
            .contains("strategy-worker-secret-id"));
    }
}

/// Adapt resolved runtime fields to the application-owned native strategy context.
pub(crate) fn native_strategy_context(
    strategy: &StrategyRuntimeConfig,
    input: &StrategyContractInput,
) -> Result<qx_strategy::StrategyContext, String> {
    let mut context = qx_app::build_strategy_context(qx_app::StrategyContextBindings {
        strategy_id: input.strategy_id.clone(),
        strategy_version: input.strategy_version.clone(),
        account_id: strategy
            .account_id
            .clone()
            .unwrap_or_else(|| "runtime".into()),
        venue_id: strategy
            .venue_id
            .clone()
            .unwrap_or_else(|| "runtime".into()),
        data_fingerprint: input.data_fingerprint.clone(),
        as_of: input.as_of,
        positions: input.positions.clone(),
        cash: input.cash.clone(),
        available_margin_raw: input.available_margin_raw,
        risk_state: input.risk_state.clone(),
    })
    .map_err(|error| error.to_string())?;
    context.positions_exact = input
        .positions_exact
        .iter()
        .map(|(instrument, amount)| {
            qx_core::ExactDecimal::parse(amount)
                .map(|amount| (instrument.clone(), amount))
                .map_err(|error| {
                    format!("StrategyContractInput positions_exact[{instrument}] 非法: {error}")
                })
        })
        .collect::<Result<_, _>>()?;
    context.cash_exact = input
        .cash_exact
        .iter()
        .map(|(currency, amount)| {
            qx_core::ExactDecimal::parse(amount)
                .map(|amount| (currency.clone(), amount))
                .map_err(|error| {
                    format!("StrategyContractInput cash_exact[{currency}] 非法: {error}")
                })
        })
        .collect::<Result<_, _>>()?;
    context.available_margin_exact = input
        .available_margin_exact
        .as_deref()
        .map(qx_core::ExactDecimal::parse)
        .transpose()
        .map_err(|error| format!("StrategyContractInput available_margin_exact 非法: {error}"))?;
    context.validate()?;
    Ok(context)
}

#[cfg(test)]
mod execution_callback_order_tests {
    use super::c_abi::{sort_execution_callbacks, ExecutionCallback};

    #[test]
    fn order_status_precedes_fill_for_the_same_event_log_sequence() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../qx-strategy/tests/fixtures/strategy-execution-sequence-v1.json"
        ))
        .unwrap();
        let events = fixture["events"].as_array().unwrap();
        let mut statuses = Vec::new();
        let mut fills = Vec::new();
        for event in events {
            match event["kind"].as_str().unwrap() {
                "order_status" => statuses.push(
                    serde_json::from_value::<qx_strategy::StrategyOrderStatusUpdate>(
                        event["update"].clone(),
                    )
                    .unwrap(),
                ),
                "fill" => fills.push(
                    serde_json::from_value::<qx_strategy::StrategyFillUpdateV2>(
                        event["update"].clone(),
                    )
                    .unwrap(),
                ),
                kind => panic!("unexpected execution update kind: {kind}"),
            }
        }
        let mut callbacks = statuses
            .iter()
            .map(ExecutionCallback::OrderStatus)
            .chain(fills.iter().map(ExecutionCallback::Fill))
            .collect::<Vec<_>>();

        sort_execution_callbacks(&mut callbacks);

        let callback_order = callbacks
            .iter()
            .map(|callback| match callback {
                ExecutionCallback::OrderStatus(update) => format!(
                    "{}:{}:{}:order_status",
                    update.order.account_id, update.order.instrument, update.event_seq
                ),
                ExecutionCallback::Fill(update) => format!(
                    "{}:{}:{}:fill",
                    update.order.account_id, update.order.instrument, update.event_seq
                ),
                ExecutionCallback::Account(update) => format!(
                    "{}:{}:account:{}",
                    update.account_id, update.venue_id, update.event_seq
                ),
            })
            .collect::<Vec<_>>();
        let expected_callback_order = fixture["expected_callback_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(callback_order, expected_callback_order);
    }

    #[test]
    fn account_snapshot_is_ordered_with_order_and_fill_facts() {
        let account_update = qx_strategy::contract::StrategyAccountUpdateV1 {
            schema_version: 1,
            event: "account_snapshot".into(),
            event_seq: 22,
            ts: 10,
            account_id: "paper".into(),
            venue_id: "BINANCE".into(),
            balances: Some(Vec::new()),
            positions: None,
        };
        let order_update: qx_strategy::StrategyOrderStatusUpdate = serde_json::from_str(
            include_str!("../../qx-strategy/tests/fixtures/strategy-order-status-accepted-v1.json"),
        )
        .unwrap();
        let mut callbacks = vec![
            ExecutionCallback::OrderStatus(&order_update),
            ExecutionCallback::Account(&account_update),
        ];

        sort_execution_callbacks(&mut callbacks);

        assert!(matches!(callbacks[0], ExecutionCallback::Account(_)));
        assert!(matches!(callbacks[1], ExecutionCallback::OrderStatus(_)));
    }
}
