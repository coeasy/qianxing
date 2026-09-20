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
    AShareFeeModel, AccountBalance, AccountCashflow,
    AccountPositionSnapshot as VenuePositionSnapshot, CashflowKind, Event, EventKind, EventLog,
    FeeModel, FundingRateSnapshot, InstrumentId, Ledger, MarginMode, Money, Order, OrderPolicy,
    OrderStatus, PositionMode, PositionSide, Price, Priority, Quantity, ReplayVerifier,
    RunManifest, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_data::{JsonBarFrameProvider, JsonDatasetRegistry};
use qx_datastruct::BarFrame;
#[cfg(test)]
use qx_execution::execute_paper_submit_effect_with_storage;
use qx_execution::{
    execute_paper_submit_effect_with_storage_backend_and_pool,
    execute_paper_submit_effect_with_storage_backend_and_pool_with_quote, ingest_venue_events,
    ingest_venue_events_with_spec, submit_order_with_risk as execute_submit_order_with_risk,
    HedgeOrderValidator, HedgeRecoveryWorker, RiskExecutionContext, VenuePortAdapter,
};
use qx_factor::{
    analyze_factor, CandidateRequest, FactorAnalysisConfig, FactorCatalog, FactorObservation,
    FeatureArtifact, FeatureDefinition, StrategyResearchSnapshot,
};
use qx_genglu::BasicAnalyser;
use qx_guanxing::{Bar, DataSourceId, DataView, QualityGate, QuoteTick, Verdict};
use qx_orchestrator::supervise_workers;
use qx_plugin::{Cardinality, Manifest, Provides, Registry, POINT_FEE_MODEL, POINT_MATCHER};
use qx_protocol::{AccountSnapshot, PositionSnapshot as AccountPositionSnapshot};
use qx_provider::{
    DataKind, DataProvider, DataQuery, ProviderCapability, ProviderError, ProviderErrorClass,
    ProviderRegistry, ProviderResult,
};
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
    AshareRuleConfig, BacktestConfig, BacktestEngine, BarMatchingEngine, BarStrategy, DataTier,
    DeterministicRng, ExecutionCostRules, MarginRule, MarginTier, NativeBarStrategy,
    NextBarOpenFillModel, NoMargin, RunManifestIdentity, TieredMargin, VirtualTradingConfig,
};
use qx_zhenlu::{
    rebalance_intent, FileSpreadOrderGroupStore, MaxQtyRule, NoShortRule, Oms, PaperVenue,
    PositionSnapshot, RiskContext, RiskGate, Signal, SignalMerger, SpreadOrderGroup,
    SpreadOrderGroupStatus, SpreadOrderGroupStore, SpreadOrderLeg, StrategyRuntime, Venue,
    VenueEvent,
};

mod backtest;
mod binance;
mod ccxt;
mod checks;
mod cli;
mod config;
mod costs;
mod dataset_commands;
mod demo;
mod metrics;
#[cfg(feature = "nats")]
mod outbox;
mod outbox_commands;
mod paper;
mod paths;
mod reconcile;
mod recovery;
mod risk;
mod runtime;
mod scheduler;
mod strategy;
mod supervisor;
mod workers;

pub(crate) use crate::backtest::*;
pub(crate) use crate::binance::*;
pub(crate) use crate::ccxt::*;
pub(crate) use crate::checks::*;
pub(crate) use crate::cli::*;
pub(crate) use crate::config::*;
pub(crate) use crate::costs::*;
pub(crate) use crate::dataset_commands::*;
pub(crate) use crate::demo::*;
pub(crate) use crate::metrics::*;
#[cfg(feature = "nats")]
pub(crate) use crate::outbox::*;
pub(crate) use crate::outbox_commands::*;
pub(crate) use crate::paper::*;
pub(crate) use crate::paths::*;
pub(crate) use crate::reconcile::*;
pub(crate) use crate::recovery::*;
pub(crate) use crate::risk::*;
pub(crate) use crate::runtime::*;
pub(crate) use crate::scheduler::*;
pub(crate) use crate::strategy::*;
pub(crate) use crate::supervisor::*;
pub(crate) use crate::workers::*;

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

/// 牵星 CLI 进程入口：只做「取 argv → 决定是否打印横幅 → 查表派发」。
///
/// 命令名、用法与摘要都来自 `cli.rs` 的 `qx_commands!` 表；这里不再出现任何
/// `if mode == "..."` 分支。无参数（或显式 `all`）继续运行确定性演示链路，
/// 未知命令则直接以 2 号退出码失败，不再静默落回演示。
fn main() {
    let argv = std::env::args().collect::<Vec<_>>();
    let invoked = argv.get(1).cloned().unwrap_or_else(|| "all".into());
    if !machine_output(&argv) {
        println!("牵星 Qianxing — 分级校准，量天定位\n");
    }
    match find_command(&invoked) {
        Some(command) => (command.run)(&argv),
        None => {
            eprintln!("未知命令 {invoked}；运行 `qx-cli help` 查看全部入口。");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests;
