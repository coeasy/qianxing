//! 牵星 CLI：端到端回测演示 + 确定性自校验。
//!
//! 这个演示的价值不在于策略本身，而在于它**用可执行的断言**证明了三件事：
//! 1. 同一输入两次运行 → 结果哈希完全一致（可重放）
//! 2. 修改任一参数   → 结果哈希发生变化（不是恒定输出）
//! 3. 决策只用 as_of(ts) 之前的数据（无前视偏差）

use qx_adapter::{
    AdapterReconcileIssue, BinanceSpotAuth, BinanceSpotCredentials, BinanceSpotMarketData,
    BinanceSpotMarketStream, BinanceSpotVenue, BinanceStreamRetryPolicy,
    BinanceUserStreamRunConfig, CcxtProcessClient, CcxtProcessVenue, CcxtRpc, HttpTransport,
    TlsHttpTransport,
};
use qx_api::{
    load_mtls_server_config_from_pem, ApiPolicy, ApiReadiness, ApiService, ApiState,
    ControlSubmitError, MtlsIdentityPemReloader, MtlsIdentityStore, ReconcileReportSnapshot,
    TlsConfigStore, TlsPemReloader,
};
use qx_control::{
    order_from_submit_command, CommandKind, ControlCommand, ControlPlane, Permission,
};
use qx_core::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot, CashflowKind, EventKind,
    FundingRateSnapshot, InstrumentId, MarginMode, Money, Order, OrderPolicy, OrderStatus,
    PositionMode, PositionSide, Price, Quantity, ReplayVerifier, RunManifest, Side,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_data::{JsonBarFrameProvider, JsonDatasetRegistry};
use qx_datastruct::BarFrame;
use qx_execution::ReconcilePort;
use qx_execution::{
    execute_paper_submit_effect, ingest_venue_events, ingest_venue_events_with_spec,
    submit_order_with_risk as execute_submit_order_with_risk, EventLogReconcilePort,
    HedgeOrderValidator, HedgeRecoveryWorker, RiskExecutionContext, VenuePortAdapter,
};
use qx_factor::{
    analyze_factor, CandidateRequest, FactorAnalysisConfig, FactorCatalog, FactorObservation,
    FeatureArtifact, FeatureDefinition, StrategyResearchSnapshot,
};
use qx_guanxing::{Bar, DataSourceId, QualityGate, QuoteTick, Verdict};
use qx_orchestrator::supervise_workers;
use qx_plugin::{Cardinality, Manifest, Provides, Registry, POINT_FEE_MODEL, POINT_MATCHER};
use qx_protocol::{AccountSnapshot, PositionSnapshot};
use qx_provider::{
    DataKind, DataProvider, DataQuery, ProviderCapability, ProviderError, ProviderErrorClass,
    ProviderRegistry, ProviderResult,
};
use qx_risk::{MaxNotionalRule, MaxQtyRule, NoShortRule, OrderRiskPosition, RuleSet};
use qx_runtime::{
    encode_strategy_columnar_input, load_control_state, ApiTransport, LiveEventPipeline,
    RuntimeBalanceDiscrepancy, RuntimeConfig, RuntimeEventEnvelope, RuntimeExternalEvent,
    RuntimeSupervisor, StorageBackend, StorageConsistency, StrategyContext, StrategyContractBars,
    StrategyContractInput, StrategyContractIntent, StrategyContractOutput, StrategyRuntimeConfig,
    StrategyTargetSnapshot, StrategyTransport, WorkerConfig, WorkerRole,
};
#[cfg(feature = "nats")]
use qx_runtime::{MessagingRuntimeConfig, WorkerContext};
use qx_scheduler::{JobSpec, JobStatus, JobWindow, RetryPolicy, ScheduleTick, Scheduler, Trigger};
#[cfg(feature = "nats")]
use qx_storage::{
    ConsumerStateStore, FileConsumerStateStore, FileOutboxStore, OutboxEvent, OutboxPublisher,
    OutboxRelay, OutboxStore,
};
use qx_storage::{
    ControlCommandQueue, ControlCommandQueueBackend, FileJobQueue, JobLease, JsonStateStore,
    QueuedJob, StorageError,
};
#[cfg(feature = "nats")]
use qx_storage::{NatsJetStreamConsumer, NatsJetStreamPublisher};
#[cfg(all(feature = "nats", feature = "postgres"))]
use qx_storage::{PostgresConsumerStateStore, PostgresOutboxStore};
#[cfg(feature = "postgres")]
use qx_storage::{
    PostgresControlCommandQueue, PostgresControlStore, PostgresEventLogStore, PostgresJobQueue,
};
#[cfg(all(feature = "sqlite", feature = "nats"))]
use qx_storage::{SqliteConsumerStateStore, SqliteOutboxStore};
#[cfg(feature = "sqlite")]
use qx_storage::{
    SqliteControlCommandQueue, SqliteControlStore, SqliteJobQueue, SqliteTokenBucket,
};
use qx_strategy::{
    BuiltinStrategy, BuiltinStrategyConfig, BuiltinStrategyKind, DynamicCAbiLoadPolicy,
    DynamicCAbiStrategy, MarketEvent as NativeMarketEvent, SharedRingConfig, SharedRingError,
    SharedRingReader, SharedRingWriter, Strategy as NativeStrategy,
    StrategyContext as NativeStrategyContext, StrategyFrame, StrategyFrameKind,
    DEFAULT_MAX_FRAME_BYTES,
};
use qx_xingban::{
    depth_frame_to_ticks, AShareFeeModel, AshareRuleConfig, BacktestConfig, BacktestEngine,
    BarStrategy, DataTier, DepthBarStrategy, DepthFrame, DeterministicRng, ExecutionCostRules,
    FeeModel, LatencyModel, MarginRule, MarginTier, NativeBarStrategy, NextBarOpenFillModel,
    NoMargin, OrderBookBacktestConfig, OrderBookBacktestEngine, RunManifestIdentity,
    TickBacktestConfig, TickBacktestEngine, TieredMargin, VirtualTradingConfig,
};
use qx_zhenlu::{
    rebalance_intent, FileSpreadOrderGroupStore, PaperVenue, RiskContext, RiskGate, Signal,
    SignalMerger, SpreadGroupAttribution, SpreadLegAttribution, SpreadOrderGroup,
    SpreadOrderGroupStatus, SpreadOrderGroupStore, SpreadOrderLeg, StrategyRuntime, Venue,
    VenueEvent,
};
mod api_service;
mod backtests;
mod ccxt_facts;
mod cli;
mod cli_args;
mod cli_help;
mod config_commands;
mod configured_backends;
mod dataset_commands;
mod ecosystem_smoke;
mod event_pipeline;
mod live_check;
mod market_bridges;
mod multi_leg;
mod path_resolution;
mod readiness;
mod runtime_check;
mod runtime_wiring;
mod scheduler;
mod selfcheck;
mod spread;
mod strategy_binding;
mod strategy_contract;
mod strategy_host;
mod venue_runtime;
mod worker_entry;
mod workers;

pub(crate) use api_service::*;
pub(crate) use backtests::*;
pub(crate) use ccxt_facts::*;
pub(crate) use cli_help::*;
pub(crate) use config_commands::*;
pub(crate) use configured_backends::*;
pub(crate) use ecosystem_smoke::*;
#[allow(unused_imports)] // 默认特性下本模块条目全部为 nats/postgres 门控
pub(crate) use event_pipeline::*;
pub(crate) use live_check::*;
pub(crate) use market_bridges::*;
pub(crate) use multi_leg::*;
pub(crate) use path_resolution::*;
pub(crate) use readiness::*;
pub(crate) use runtime_check::*;
pub(crate) use runtime_wiring::*;
pub(crate) use scheduler::*;
pub(crate) use spread::*;
pub(crate) use strategy_binding::*;
pub(crate) use strategy_contract::*;
pub(crate) use strategy_host::*;
pub(crate) use venue_runtime::*;
pub(crate) use worker_entry::*;

use dataset_commands::{
    run_dataset_bundle, run_dataset_ingest, verify_dataset_bundle_binding,
    verify_dataset_bundle_component_bindings,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use workers::{run_scheduler_worker, run_strategy_worker};

static NEXT_STRATEGY_RING_ID: AtomicU64 = AtomicU64::new(1);

fn run_unified_backtest(
    runtime: Option<&Path>,
    frame: Option<&Path>,
    spec: Option<&Path>,
) -> Result<(), String> {
    let default_runtime = repository_deploy_path("qianxing.runtime.strategy-backtest.example.json");
    let default_frame = repository_deploy_path("qianxing.bar-frame.example.json");
    run_strategy_backtest(
        runtime.unwrap_or(&default_runtime),
        frame.unwrap_or(&default_frame),
        spec,
    )
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
    cli::run();
}

#[cfg(test)]
mod tests;
