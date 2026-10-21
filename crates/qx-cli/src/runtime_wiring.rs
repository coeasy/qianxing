//! 运行时接线：读取 runtime 配置并装配 EventLog 管道及配置边界。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use runtime_wiring::*;` 再导出，行为与拆分前逐字相同。

use crate::{read_example_json, reject_ashare_rules_config};
use qx_runtime::{RuntimeConfig, WorkerConfig, WorkerRole};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
pub(crate) use qx_runtime::settlement_currency_for_log;
#[cfg(test)]
pub(crate) use qx_service::backtest_bindings::ExecutionCostBinding;
pub(crate) use qx_service::backtest_bindings::{
    cost_rules_problem, default_execution_cost_binding, execution_cost_binding_from_config,
};
pub(crate) use qx_service::pipeline::{event_log_exists, open_account_pipeline, PipelineStorage};

pub(crate) fn read_runtime_config(path: &Path) -> Result<RuntimeConfig, String> {
    let payload = read_example_json(path, "运行时配置")?;
    RuntimeConfig::from_json(&payload)
}

pub(crate) use qx_service::risk_host::{
    config_margin_mode, strategy_allows_short, strategy_risk_gate,
};

pub(crate) fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 租约域时钟：epoch 秒。租约（`JobLease`/`ControlCommandLease`/outbox）、`JobSpec.timeout_seconds`
/// 与 `JobRun` 的 `started_ts`/`deadline_ts`/`next_retry_ts` 全按秒相加比较，而运行时唯一的墙钟是毫秒：
/// 直接喂毫秒会让 30 秒租约在 30 毫秒后"过期"，作业还在跑就被别的 worker 抢走、收尾 `ack_at` 变
/// `LeaseExpired`、`deadline_ts` 在启动瞬间就已超时。跨进租约域的调用点只在这里换算一次。
pub(crate) fn lease_clock(timestamp_ms: u64) -> u64 {
    qx_runtime::lease_clock(timestamp_ms)
}

pub(crate) use qx_service::worker_metrics::worker_metrics_unhealthy;
#[cfg(test)]
pub(crate) use qx_service::worker_metrics::write_worker_metrics;

/// A 股交易制度缺的那个状态，写进拒绝文案而不是只说"不支持"（V11 Q65）。
///
/// 费率倒是接得上（`execution_cost_binding_from_config` 就是挂钩点），但只接费率会让产物显示
/// "A 股佣金"而订单照样 T+0、照样可以 150 股买入 —— 比整段不认更容易骗人。
const ASHARE_SUBMIT_GAP: &str =
    "T+1 需要\"今日买入\"的结算状态、整手与涨跌停要按板块和昨收逐单判定，\
     而提交前的账户闸门只做规格、杠杆、名义额与可用保证金预检";

/// 会向 Venue 提交**新订单**的角色。`SpreadRecovery` 算在内：补腿本身就是一笔新订单（V10 Q57）。
fn worker_submits_orders(worker: &WorkerConfig) -> bool {
    matches!(
        worker.role,
        WorkerRole::Execution | WorkerRole::SpreadRecovery
    )
}

/// Paper/Live 的提交入口在动手前先拒 A 股段（V11 Q65，与 Q61 的 `book` / `multi-builtin` 同一口径）。
///
/// 判据只有一个：**这条路径会不会把一笔新订单送进 Venue**。行情、用户流、对账、策略与 API worker
/// 不在此列 —— 它们本来就不承诺交易制度，把它们一起拒会让"研究用配置"连启动都做不到。
/// `worker` 缺位表示一次性提交入口（`paper submit-order` / `binance submit-order`），按提交对待。
pub(crate) fn reject_ashare_rules_on_submit_path(
    config_path: &Path,
    worker: Option<&WorkerConfig>,
    entry: &str,
) -> Result<(), String> {
    if worker.is_some_and(worker_submits_orders) || worker.is_none() {
        return reject_ashare_rules_config(Some(config_path), entry, ASHARE_SUBMIT_GAP);
    }
    Ok(())
}

// Keep test-only adapters after production declarations so source audits can inspect the
// complete production module instead of treating the first `#[cfg(test)]` as end-of-file.
#[cfg(test)]
pub(crate) use qx_service::risk_host::CONSERVATIVE_DEFAULT_RULE_SET_VERSION;
#[cfg(test)]
pub(crate) use qx_service::worker_metrics::{read_worker_metrics, worker_metrics_dir};

/// Historical path adapter retained for configuration binding unit tests.
#[cfg(test)]
pub(crate) fn execution_cost_binding(
    runtime_config_path: Option<&Path>,
) -> Result<ExecutionCostBinding, String> {
    match runtime_config_path {
        Some(path) => {
            let config = read_runtime_config(path)?;
            execution_cost_binding_from_config(&config, Some(path))
        }
        None => Ok(default_execution_cost_binding()),
    }
}
