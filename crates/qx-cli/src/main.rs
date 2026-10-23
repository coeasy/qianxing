//! 牵星 CLI：端到端回测演示 + 确定性自校验。
//!
//! 这个演示的价值不在于策略本身，而在于它**用可执行的断言**证明了三件事：
//! 1. 同一输入两次运行 → 结果哈希完全一致（可重放）
//! 2. 修改任一参数   → 结果哈希发生变化（不是恒定输出）
//! 3. 决策只用 as_of(ts) 之前的数据（无前视偏差）

use qx_adapter::{
    BinanceSpotAuth, BinanceSpotCredentials, BinanceSpotMarketData, BinanceSpotVenue,
    HttpTransport, TlsHttpTransport,
};
#[cfg(test)]
use qx_api::ReconcileReportSnapshot;
#[cfg(test)]
use qx_control::CommandKind;
use qx_control::{order_from_submit_command, ControlCommand, Permission};
#[allow(unused_imports)]
use qx_core::AccountPositionSnapshot;
#[allow(unused_imports)]
use qx_core::{
    AccountBalance, AccountCashflow, CashflowKind, EventKind, InstrumentId, Ledger, Money, Order,
    Price, Quantity, RunManifest, TradingInstrumentSpec, TradingProduct, VenueFamily,
    DEFAULT_SETTLEMENT_CURRENCY,
};
use qx_data::JsonDatasetRegistry;
use qx_datastruct::BarFrame;
#[cfg(test)]
use qx_execution::execute_paper_submit_effect;
use qx_guanxing::{Bar, QuoteTick};
#[cfg(test)]
use qx_risk::OrderRiskPosition;
#[cfg(test)]
use qx_runtime::RuntimeBalanceDiscrepancy;
#[cfg(test)]
use qx_runtime::RuntimeSupervisor;
#[cfg(test)]
use qx_runtime::{LiveEventPipeline, RuntimeExternalEvent};
use qx_runtime::{
    OutboxRecovery, RuntimeConfig, RuntimeEventEnvelope, StrategyContractIntent,
    StrategyContractOutput, WorkerConfig, WorkerRole,
};
use qx_scheduler::{claimable_by, utc_schedule_tick};
#[cfg(test)]
use qx_storage::JsonStateStore;
#[cfg(test)]
use qx_storage::StorageError;
use qx_storage::{ControlCommandQueue, ControlCommandQueueBackend};
#[cfg(test)]
use qx_strategy::BuiltinStrategyConfig;
use qx_strategy::BuiltinStrategyKind;
#[cfg(test)]
use qx_xingban::{MarginRule, NoMargin};
#[cfg(test)]
use qx_zhenlu::FileSpreadOrderGroupStore;
#[cfg(test)]
use qx_zhenlu::RiskContext;
#[cfg(test)]
use qx_zhenlu::RiskGate;
#[cfg(test)]
use qx_zhenlu::SpreadOrderGroupStore;
mod api_algo_execution;
mod api_service;
mod app_args;
mod app_commands;
mod backtests;
mod build_identity;
mod ccxt_facts;
mod ccxt_submit_args;
mod cli;
mod cli_args;
mod cli_help;
mod cli_value_parsers;
mod config_commands;
mod configured_backends;
mod console_args;
mod console_serve;
mod contract_adapters;
mod data_validate;
mod data_validate_args;
mod dataset_commands;
mod deploy_lookup;
mod ecosystem_smoke;
mod event_pipeline;
mod evidence_signature;
mod evidence_signature_args;
mod guide;
mod guide_args;
mod init_guidance;
mod init_project;
mod live_check;
mod market_bridges;
mod market_spec;
mod multi_leg;
mod native_trust;
mod panic_boundary;
mod path_resolution;
mod plan_args;
mod plan_commands;
mod project_manifest;
mod quickstart;
mod readiness;
mod report_args;
mod report_benchmark;
mod report_command;
mod report_comparison;
mod report_comparison_benchmark;
mod report_comparison_command;
mod report_credibility;
mod report_equity_csv;
mod report_fidelity;
mod report_html;
mod report_html_ashare;
mod report_multi_account;
mod report_multi_market;
mod report_multi_market_depth;
mod report_multi_market_depth_render;
mod report_output_paths;
mod report_readout;
mod report_svg;
mod report_xlsx;
mod run_entry_arguments;
mod run_evidence;
mod run_evidence_build_attestation;
mod run_evidence_experiment_recompute;
mod run_evidence_export;
mod run_evidence_multi;
mod run_evidence_multi_account;
mod run_evidence_multi_market;
mod run_evidence_recompute;
mod run_evidence_verify;
mod run_usage;
mod runtime_check;
mod runtime_wiring;
mod scheduler;
mod selfcheck;
mod spread;
mod storage_adopt;
mod storage_adopt_args;
mod strategy_binding;
mod strategy_contract;
mod strategy_diagnostics;
mod strategy_host;
mod strategy_live_state;
mod strategy_multi_account_diagnostics;
mod usage_errors;
mod venue_evidence;
mod venue_runtime;
mod worker_entry;
mod worker_runtime;
mod worker_shutdown;
mod workers;

pub(crate) use api_service::*;
pub(crate) use backtests::*;
pub(crate) use ccxt_facts::*;
pub(crate) use cli_help::*;
pub(crate) use config_commands::*;
pub(crate) use configured_backends::*;
pub(crate) use console_args::*;
pub(crate) use console_serve::*;
pub(crate) use deploy_lookup::*;
pub(crate) use ecosystem_smoke::*;
#[allow(unused_imports)] // 默认特性下本模块条目全部为 nats/postgres 门控
pub(crate) use event_pipeline::*;
pub(crate) use init_project::*;
pub(crate) use live_check::*;
pub(crate) use market_bridges::*;
pub(crate) use market_spec::*;
pub(crate) use multi_leg::*;
pub(crate) use panic_boundary::*;
pub(crate) use path_resolution::*;
pub(crate) use plan_commands::*;
#[cfg(test)]
pub(crate) use qx_runtime::{
    read_capped_worker_line, record_stderr_tail, stderr_tail_note, STDERR_TAIL_LINES,
    STDERR_TAIL_MAX_CHARS,
};
pub(crate) use qx_service::pipeline_metrics_report::PipelineMetricsReporter;
#[cfg(test)]
pub(crate) use qx_service::risk_host::LIVE_BALANCE_SNAPSHOT_MAX_AGE_MS;
pub(crate) use qx_service::worker_market_spec::{
    load_worker_market_spec_for_instrument, validate_configured_market_specs,
    validate_worker_market_specs, worker_market_spec_failures,
};
pub(crate) use readiness::*;
pub(crate) use report_command::*;
pub(crate) use report_comparison::*;
#[allow(unused_imports)]
pub(crate) use report_credibility::*;
pub(crate) use report_html::*;
pub(crate) use report_html_ashare::*;
pub(crate) use report_readout::*;
#[allow(unused_imports)]
pub(crate) use report_svg::*;
pub(crate) use run_entry_arguments::*;
pub(crate) use run_usage::*;
pub(crate) use runtime_check::*;
pub(crate) use runtime_wiring::*;
pub(crate) use scheduler::*;
pub(crate) use spread::*;
pub(crate) use strategy_binding::*;
pub(crate) use strategy_contract::*;
pub(crate) use strategy_host::*;
pub(crate) use strategy_live_state::*;
pub(crate) use venue_runtime::*;
pub(crate) use worker_entry::*;
#[allow(unused_imports)]
pub(crate) use worker_runtime::*;
#[allow(unused_imports)] // 默认特性下 join 只由 NATS event pipeline 使用
pub(crate) use worker_shutdown::*;

use data_validate::{validate_dataset, validate_depth_dataset};
#[cfg(test)]
use dataset_commands::verify_dataset_bundle_binding;
use dataset_commands::{run_dataset_bundle, run_dataset_ingest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::thread;
use std::time::Duration;
use workers::{run_scheduler_worker, run_strategy_worker};

fn run_unified_backtest(
    runtime: Option<&Path>,
    frame: Option<&Path>,
    spec: Option<&Path>,
) -> Result<(), String> {
    // 默认这份从「跨语言策略模板」换成「内置策略模板」（V13 R31 ① F6）：README 安装路径 A 是
    // 只装 CLI、不装 Python，而旧默认那份策略由 Python worker 承担，`qx-cli backtest` 一敲就
    // 撞解释器（`logs/s736_…`：无 QX_PYTHON 退 2；`logs/s737_…`：内置与 A 股模板都退 0）。
    //
    // V13 R28 起这一份**不再就地拼路径**：`run` 统一入口的写侧（本函数）与读侧（`run doctor` /
    // `run runtime-check` / `run report`）必须解析到同一份，否则 `run backtest` 刚写出的摘要
    // 读侧一个都找不到。单一出口是 `unified_default_runtime_path`，`run_umbrella_default_check`
    // 逐处核对它。
    let default_runtime = unified_default_runtime_path();
    // 用默认那一份配置时，先把「用的哪一份、产物会落在哪」印出来：改前裸 `backtest` 在任意目录
    // 都会静默建出一棵 data/ 产物树，屏幕上没有一句它读的是哪个配置（V13 R31 ① F5）。
    if runtime.is_none() {
        let cwd = std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|_| "<未知目录>".to_string());
        println!("[默认输入] 运行时配置={}", default_runtime.display());
        println!("[默认落点] 产物写在该配置的 storage.data_dir，相对当前目录 {cwd} 解析");
    }
    let runtime = runtime.unwrap_or(&default_runtime);
    // 只喂配置时，BarFrame 取这份配置自己声明的那份 bars；声明缺席才回落到仓库示例夹具。
    let default_frame = backtest_frame_from_config(runtime)
        .unwrap_or_else(|| repository_deploy_path("qianxing.bar-frame.example.json"));
    run_strategy_backtest(runtime, frame.unwrap_or(&default_frame), spec)
}

/// 供跨进程恢复验收器调用的极小子进程入口。
///
/// 子进程只操作持久化控制命令队列，不持有父进程内存状态；因此可以真实验证
/// 进程退出后租约过期接管、fencing token 递增、旧 worker 确认被拒绝以及新
/// worker 确认成功，而不是把这些场景缩小成同进程函数调用。
fn run_recovery_child(args: &[String]) -> Result<(), String> {
    let root = args
        .get(2)
        .ok_or_else(|| "recovery-child 缺少 queue-root".to_string())?;
    let action = args
        .get(3)
        .ok_or_else(|| "recovery-child 缺少 action".to_string())?;
    let owner = args
        .get(4)
        .ok_or_else(|| "recovery-child 缺少 owner".to_string())?;
    let now = args
        .get(5)
        .ok_or_else(|| "recovery-child 缺少 now".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("recovery-child now 非法: {error}"))?;
    let queue = ControlCommandQueue::new(root);
    match action.as_str() {
        "claim" | "takeover" => {
            let lease_seconds = args
                .get(6)
                .ok_or_else(|| "recovery-child claim 缺少 lease_seconds".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child lease_seconds 非法: {error}"))?;
            let command_id = args
                .get(7)
                .ok_or_else(|| "recovery-child claim 缺少 command_id".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child command_id 非法: {error}"))?;
            let lease = queue
                .claim(command_id, owner, now, lease_seconds)
                .map_err(|error| format!("recovery-child {action} 失败: {error:?}"))?;
            println!("{}", lease.fencing_token);
        }
        "stale-ack" | "ack" => {
            let token = args
                .get(6)
                .ok_or_else(|| "recovery-child ack 缺少 fencing_token".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child fencing_token 非法: {error}"))?;
            let command_id = args
                .get(7)
                .ok_or_else(|| "recovery-child ack 缺少 command_id".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child command_id 非法: {error}"))?;
            let result = queue.ack_at(command_id, owner, token, now);
            if action == "stale-ack" {
                if result.is_ok() {
                    return Err("旧 worker fencing token 意外确认成功".into());
                }
                println!("rejected");
            } else {
                result.map_err(|error| format!("recovery-child ack 失败: {error:?}"))?;
                println!("acked");
            }
        }
        other => return Err(format!("recovery-child action 不支持: {other}")),
    }
    Ok(())
}

/// Python 解释器解析：`QX_PYTHON` 环境变量优先，否则回落到 `python`。
///
/// CLI 里所有跨语言子进程（CCXT worker、Python/C++ 策略 worker）都从这里取解释器，
/// 避免同一份运行时配置在不同 worker 下指向不同解释器。
fn python_interpreter() -> String {
    python_interpreter_origin().0
}

/// 解释器本身 + 它的来源。来源只用于失败诊断：本机 `python` 可能只是 WindowsApps 的
/// 占位桩（实测 `python --version` 无 stdout/stderr、退出码 49），只报"worker 已关闭输出"
/// 无法区分"解释器不可用"与"策略代码报错"，所以回落必须在失败信息里说出来。
pub(crate) fn python_interpreter_origin() -> (String, &'static str) {
    match std::env::var("QX_PYTHON") {
        Ok(value) if !value.trim().is_empty() => (value, "来自 QX_PYTHON"),
        _ => ("python".into(), "QX_PYTHON 未设置，回落 PATH python"),
    }
}

fn main() {
    qx_runtime::install_stdin_shutdown_listener();
    if let Err(diagnostic) = run_cli_with_panic_boundary(cli::run) {
        eprintln!("[qx-cli · Panic] {diagnostic}");
        std::process::exit(101);
    }
}

fn run_cli_with_panic_boundary(run: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let worker = std::thread::Builder::new()
        .name("qx-cli-command".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                .map_err(|payload| panic_diagnostic("cli-process", payload.as_ref()))
        })
        .map_err(|error| format!("启动 CLI 命令线程失败: {error}"))?;
    match worker.join() {
        Ok(result) => result,
        Err(payload) => Err(panic_diagnostic("cli-process", payload.as_ref())),
    }
}

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod tests;
