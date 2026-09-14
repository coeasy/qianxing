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
use qx_control::{CommandKind, ControlCommand, ControlPlane, Permission};
use qx_core::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot as VenuePositionSnapshot,
    CashflowKind, Event, EventKind, EventLog, FundingRateSnapshot, InstrumentId, Ledger,
    MarginMode, Money, Order, OrderPolicy, OrderStatus, PositionMode, PositionSide, Price,
    Priority, Quantity, ReplayVerifier, RunManifest, Side, TradingInstrumentSpec, TradingProduct,
    SCALE,
};
use qx_datastruct::BarFrame;
#[cfg(test)]
use qx_execution::execute_paper_submit_effect_with_storage;
use qx_execution::{
    execute_paper_submit_effect_with_storage_backend_and_pool, ingest_venue_events,
    ingest_venue_events_with_spec, submit_order as execute_submit_order,
    submit_order_with_risk as execute_submit_order_with_risk, RiskExecutionContext,
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
    encode_strategy_columnar_input, load_control_state, order_from_submit_command, ApiTransport,
    LiveEventPipeline, RuntimeBalanceDiscrepancy, RuntimeConfig, RuntimeEventEnvelope,
    RuntimeExternalEvent, RuntimeSupervisor, StorageBackend, StrategyContext, StrategyContractBars,
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
#[cfg(feature = "sqlite")]
use qx_storage::{
    SqliteConsumerStateStore, SqliteControlCommandQueue, SqliteControlStore, SqliteJobQueue,
    SqliteOutboxStore, SqliteTokenBucket,
};
use qx_strategy::{
    BuiltinStrategy, BuiltinStrategyConfig, BuiltinStrategyKind, DynamicCAbiLoadPolicy,
    DynamicCAbiStrategy, MarketEvent as NativeMarketEvent, SharedRingConfig, SharedRingError,
    SharedRingReader, SharedRingWriter, Strategy as NativeStrategy,
    StrategyContext as NativeStrategyContext, StrategyFrame, StrategyFrameKind,
    DEFAULT_MAX_FRAME_BYTES,
};
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarMatchingEngine, BarStrategy, DataTier, DeterministicRng,
    MakerTakerFeeModel, MarginRule, MarginTier, NativeBarStrategy, NextBarOpenFillModel, NoMargin,
    TieredMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::{
    rebalance_intent, MaxQtyRule, NoShortRule, Oms, PaperVenue, PositionSnapshot, RiskContext,
    RiskGate, Signal, SignalMerger, StrategyRuntime, Venue, VenueEvent,
};
mod workers;

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

/// 定点 → f64，仅用于打印。
fn f(x: i128) -> f64 {
    x as f64 / 1e9
}

/// 生成确定性合成行情（同一 seed 必然产生同一序列）。
fn gen_bars(n: usize, seed: u64) -> Vec<Bar> {
    let mut rng = DeterministicRng::new(seed);
    let mut bars = Vec::with_capacity(n);
    let mut px: i128 = 100_000_000_000; // 100.0

    for i in 0..n {
        let drift = ((rng.next_u64() % 2001) as i128) - 1000;
        px = (px + drift * 1_000_000).max(1_000_000_000);
        let open = px;
        let close = px + (((rng.next_u64() % 1001) as i128) - 500) * 1_000_000;
        let high = open.max(close) + ((rng.next_u64() % 501) as i128) * 1_000_000;
        let low = (open.min(close) - ((rng.next_u64() % 501) as i128) * 1_000_000).max(1_000_000);
        let volume = 1_000 + (rng.next_u64() % 5_000) as i128;
        bars.push(Bar::new(
            (i as u64 + 1) * 1_000_000_000,
            open,
            high,
            low,
            close,
            volume,
        ));
    }
    bars
}

struct Outcome {
    hash: u64,
    n_fills: usize,
    total_fee: i128,
    total_return: i128,
    max_drawdown: i128,
    final_equity: i128,
}

struct DemoProvider {
    capability: ProviderCapability,
    fail: bool,
}

impl DataProvider for DemoProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        if self.fail {
            return Err(ProviderError::new(
                ProviderErrorClass::SwitchProvider,
                "demo primary unavailable",
            ));
        }
        let mut result = ProviderResult {
            records: vec![qx_guanxing::RawRecord {
                source: DataSourceId::new(self.capability.provider_id.clone()),
                event_time: 1,
                receive_time: 2,
                payload_hash: 3,
                schema_version: 1,
            }],
            provider_id: self.capability.provider_id.clone(),
            provider_version: self.capability.version.clone(),
            request_id: "cli-ecosystem".into(),
            retry_chain: Vec::new(),
            received_at: 2,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

fn provider_capability(id: &str, priority: u32) -> ProviderCapability {
    ProviderCapability {
        provider_id: id.into(),
        version: "v1".into(),
        data_kinds: [DataKind::Bar].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["1d".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 10,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: u64::MAX,
        realtime: false,
        priority,
        quality_score: 100,
        cost_score: 1,
    }
}

fn run_ecosystem_smoke() {
    let instrument = InstrumentId::parse("DEMO.SIM").unwrap();

    let mut factors = FactorCatalog::default();
    factors
        .register_definition(FeatureDefinition {
            name: "momentum".into(),
            version: "v1".into(),
            formula: "close / close[-20] - 1".into(),
            input_fields: vec!["close".into()],
            dependencies: Vec::new(),
            point_in_time: true,
        })
        .unwrap();
    factors
        .publish_artifact(FeatureArtifact {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "synthetic-bars".into(),
            as_of: 20,
            coverage_bps: 10_000,
            values: [(instrument.clone(), 123_i128)].into_iter().collect(),
        })
        .unwrap();
    let second_instrument = InstrumentId::parse("DEMO2.SIM").unwrap();
    let report = analyze_factor(
        "momentum@v1",
        &[
            FactorObservation {
                timestamp: 1,
                instrument: instrument.clone(),
                factor_value: Some(1),
                forward_returns: [(1, 10)].into_iter().collect(),
                previous_exposure: Some(0),
                target_exposure: Some(100),
                capacity_raw: Some(1_000_000),
                exposures: BTreeMap::new(),
                group_labels: BTreeMap::new(),
            },
            FactorObservation {
                timestamp: 1,
                instrument: second_instrument,
                factor_value: Some(2),
                forward_returns: [(1, 20)].into_iter().collect(),
                previous_exposure: Some(0),
                target_exposure: Some(200),
                capacity_raw: Some(900_000),
                exposures: BTreeMap::new(),
                group_labels: BTreeMap::new(),
            },
        ],
        &FactorAnalysisConfig {
            input_fingerprint: "synthetic-bars".into(),
            ..FactorAnalysisConfig::default()
        },
    )
    .unwrap();
    factors.publish_report(report).unwrap();
    let candidate = factors
        .bind_candidate(CandidateRequest {
            strategy_version: "sma-cross-v1".into(),
            universe_version: "demo-universe-v1".into(),
            parameters: Default::default(),
            data_fingerprint: "synthetic-bars".into(),
            factor_keys: vec!["momentum@v1".into()],
            cost_bps: 8,
            train_start: 1,
            train_end: 100,
            validation_start: 101,
            validation_end: 200,
            intended_exposure: BTreeMap::new(),
            constraints: BTreeMap::new(),
            execution_model: "event-backtest@v1".into(),
            risk_model: "default-risk@v1".into(),
        })
        .unwrap();
    assert!(!candidate.event_verified);
    println!(
        "[因子 · FactorCatalog] feature={} candidate=VectorOnly ✓",
        candidate.config.feature_version
    );

    let mut base = AccountSnapshot::new(1, "main", "default", "SIM", 20);
    base.cash_raw.insert("USD".into(), 100_000);
    base.positions.insert(
        instrument.clone(),
        AccountPositionSnapshot {
            instrument,
            quantity_raw: 10,
            ..AccountPositionSnapshot::default()
        },
    );
    base.seal();
    let mut target = base.clone();
    target.cash_raw.insert("USD".into(), 99_000);
    target.equity_raw = 101_000;
    target.seal();
    let diff = base.diff(&target).unwrap();
    assert_eq!(diff.apply(&base).unwrap(), target);
    println!(
        "[协议 · QIFI Snapshot] schema=v1 diff_base={:016x} diff_target={:016x} ✓",
        diff.base_state_hash, diff.target_state_hash
    );

    let mut providers = ProviderRegistry::new();
    providers
        .register(Box::new(DemoProvider {
            capability: provider_capability("primary", 1),
            fail: true,
        }))
        .unwrap();
    providers
        .register(Box::new(DemoProvider {
            capability: provider_capability("backup", 2),
            fail: false,
        }))
        .unwrap();
    let result = providers
        .fetch_with_failover(&DataQuery {
            kind: DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: BTreeSet::new(),
            field_set: BTreeSet::new(),
            frequency: "1d".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 10,
            as_of: None,
        })
        .unwrap();
    assert_eq!(result.retry_chain, ["primary", "backup"]);
    println!(
        "[数据 · ProviderRegistry] provider={} retry_chain={:?} ✓",
        result.provider_id, result.retry_chain
    );

    let mut scheduler = Scheduler::default();
    scheduler
        .register(JobSpec {
            job_id: "load-bars".into(),
            job_version: "v1".into(),
            owner: "research".into(),
            enabled: true,
            trigger: Trigger::TradingCalendar {
                session: "post-close".into(),
            },
            window: JobWindow::PostClose,
            depends_on: vec![],
            input_refs: vec![],
            output_refs: vec!["bars".into()],
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: "data".into(),
            idempotency_key: "load-bars-daily".into(),
            permission_scope: "research".into(),
            audit_reason: "ecosystem smoke schedule".into(),
            dry_run: true,
        })
        .unwrap();
    assert_eq!(scheduler.ready_jobs(&BTreeSet::new()).len(), 1);
    println!(
        "[调度 · JobSpec] jobs={} ready=1 deterministic ✓",
        scheduler.len()
    );

    let mut control = ControlPlane::default();
    let audit = control
        .submit(
            ControlCommand {
                command_id: 1,
                request_id: "cli-control-1".into(),
                operator_id: "cli".into(),
                reason: "ecosystem smoke".into(),
                kind: CommandKind::PauseStrategy,
                target: "sma-cross-v1".into(),
                payload: BTreeMap::new(),
                permission: Permission::Trading,
                dry_run: true,
            },
            20,
        )
        .unwrap();
    println!(
        "[控制 · ControlCommand] status={:?} digest={:016x} ✓",
        audit.status, audit.command_digest
    );

    let api = ApiService::new(ApiState::default());
    assert_eq!(api.handle("GET", "/health", "", 20).status, 200);
    assert_eq!(
        api.handle("GET", "/schema/account-snapshot-v1", "", 20)
            .status,
        200
    );
    println!("[控制 · Query/WebSocket API] health/schema routes ✓");
}

fn mk_order(id: u64, instr: &InstrumentId, side: Side, qty: i64) -> Order {
    Order {
        client_id: id,
        instrument: instr.clone(),
        side,
        qty: Quantity::from_i64(qty),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

/// 双均线策略回测。**关键：bar t 决策 → bar t+1 开盘成交。**
fn run_backtest(bars: &[Bar], seed: u64, fast: usize, slow: usize) -> Outcome {
    let view = DataView::try_new(bars.to_vec(), DataSourceId::new("synthetic"))
        .expect("synthetic bars must pass quality gate");
    let instr = InstrumentId::parse("DEMO.SIM").unwrap();

    let mut matching = BarMatchingEngine::new(
        Box::new(NextBarOpenFillModel),
        Box::new(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        }),
        seed,
    );

    let mut log = EventLog::new();
    let mut analyser = BasicAnalyser::default();
    let mut ledger = Ledger::new();
    let deposit_id = ledger
        .deposit(
            "main",
            "USD",
            Money::from_raw(100_000_000_000_000),
            bars[0].ts,
        )
        .unwrap();
    let mut equity: Vec<i128> = Vec::new();
    let mut next_id: u64 = 1;
    let deposit_entry = ledger
        .entries()
        .iter()
        .find(|entry| entry.id == deposit_id)
        .cloned()
        .expect("initial deposit entry must exist");
    let deposit_seq = log.alloc_seq();
    log.append(Event::new(
        deposit_seq,
        deposit_entry.ts,
        Priority::APPLY,
        EventKind::LedgerApplied {
            entry: deposit_entry,
        },
    ));

    let mut risk = RiskGate::new();
    risk.add(Box::new(MaxQtyRule {
        max_qty: 100_000_000_000, // 100 单位
    }));
    risk.add(Box::new(NoShortRule));
    let mut oms = Oms::new();

    for i in (slow + 1)..bars.len() {
        // 决策只使用截至**上一根** bar 的数据
        let hist = view.as_of(bars[i - 1].ts);
        let pos = ledger.position_for("main", &instr).quantity.raw();
        if hist.len() > slow {
            let n = hist.len();
            let sum = |a: usize, b: usize| hist[a..b].iter().map(|x| x.close).sum::<i128>();
            let fast_now = sum(n - fast, n) / fast as i128;
            let slow_now = sum(n - slow, n) / slow as i128;
            let fast_prev = sum(n - fast - 1, n - 1) / fast as i128;
            let slow_prev = sum(n - slow - 1, n - 1) / slow as i128;

            let golden = fast_prev <= slow_prev && fast_now > slow_now;
            let death = fast_prev >= slow_prev && fast_now < slow_now;

            if golden && pos == 0 {
                let o = mk_order(next_id, &instr, Side::Buy, 10);
                if risk.check(&o, &PositionSnapshot::new(pos, 0)).is_ok() {
                    let _ = oms.submit(o.clone());
                    matching.submit(o);
                    next_id += 1;
                }
            } else if death && pos > 0 {
                let o = mk_order(next_id, &instr, Side::Sell, 10);
                if risk.check(&o, &PositionSnapshot::new(pos, 0)).is_ok() {
                    let _ = oms.submit(o.clone());
                    matching.submit(o);
                    next_id += 1;
                }
            }
        }

        // 本根 bar 开盘撮合上一根 bar 提交的挂单
        let fills = matching.on_bar(&bars[i], bars[i].ts);
        for fl in &fills {
            let order = oms.get(fl.order_id).cloned().unwrap();
            let _ = oms.apply_fill(fl);
            let entry_ids = ledger.apply_fill(&order, fl, "USD").unwrap();
            analyser.on_fill(fl);
            // 必须用 alloc_seq：append 不会推进 seq，否则所有事件 seq 都相同，
            // 事件日志将失去顺序信息（摘要里 seq 恒为 0）。
            let event_seq = log.alloc_seq();
            log.append(Event::new(
                event_seq,
                fl.ts,
                Priority::APPLY,
                EventKind::Filled { fill: fl.clone() },
            ));
            for entry_id in entry_ids {
                let seq = log.alloc_seq();
                let entry = ledger
                    .entries()
                    .iter()
                    .find(|entry| entry.id == entry_id)
                    .cloned()
                    .expect("账簿 entry 必须存在");
                log.append(Event::new(
                    seq,
                    fl.ts,
                    Priority::APPLY,
                    EventKind::LedgerApplied { entry },
                ));
            }
        }

        let mut marks = BTreeMap::new();
        marks.insert(instr.clone(), Price::from_raw(bars[i].close));
        equity.push(ledger.equity_for("main", &marks, "USD").unwrap());
    }

    let m = analyser.report(&equity);
    Outcome {
        hash: log.digest(),
        n_fills: m.n_fills,
        total_fee: m.total_fee,
        total_return: m.total_return,
        max_drawdown: m.max_drawdown,
        final_equity: m.final_equity,
    }
}

fn run_paper_smoke() {
    let instrument = InstrumentId::parse("DEMO.SIM").unwrap();
    let mut venue = PaperVenue::new("paper");
    let order = mk_order(9001, &instrument, Side::Buy, 2);
    let accepted = venue.submit(order, 1).unwrap();
    assert!(matches!(accepted.as_slice(), [VenueEvent::Accepted { .. }]));
    let events = venue.on_quote(
        &instrument,
        QuoteTick::new(
            2,
            Price::from_i64(99),
            Quantity::from_i64(10),
            Price::from_i64(100),
            Quantity::from_i64(10),
            1,
        ),
    );
    assert!(matches!(events.as_slice(), [VenueEvent::Fill(_)]));
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Filled);

    let remote_snapshot = venue.snapshot();
    venue.disconnect();
    assert!(venue
        .submit(mk_order(9002, &instrument, Side::Buy, 1), 3)
        .is_err());
    assert_eq!(venue.state(), qx_zhenlu::ConnectorState::ReconcileRequired);
    venue.reconnect();
    assert!(venue.connected());
    assert!(venue
        .reconcile_snapshot(&remote_snapshot)
        .unwrap()
        .is_empty());
    println!("[针路 · PaperVenue] 订单接受/报价成交/断线转对账/恢复通过 ✓");
}

fn read_runtime_config(path: &Path) -> Result<RuntimeConfig, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取运行时配置失败 {}: {error}", path.display()))?;
    RuntimeConfig::from_json(&payload)
}

#[derive(Clone, Debug)]
struct PipelineStorage {
    root: PathBuf,
    segment_events: Option<usize>,
    postgres_dsn: Option<String>,
    #[cfg(feature = "postgres")]
    postgres_pool_size: usize,
}

impl PipelineStorage {
    fn from_config(config: &RuntimeConfig) -> Result<Self, String> {
        Ok(Self {
            root: Path::new(&config.storage.data_dir).to_path_buf(),
            segment_events: config.storage.event_log_segment_events,
            postgres_dsn: configured_postgres_dsn(config)?,
            #[cfg(feature = "postgres")]
            postgres_pool_size: config.storage.postgres_pool_size,
        })
    }

    fn open(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        if let Some(dsn) = self.postgres_dsn.as_deref() {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = dsn;
                return Err(
                    "当前 qx-cli 未启用 postgres feature，无法打开 PostgreSQL EventLog".into(),
                );
            }
            #[cfg(feature = "postgres")]
            {
                return LiveEventPipeline::open_postgres_with_pool_size(
                    dsn,
                    self.postgres_pool_size,
                    log_name,
                    currency,
                )
                .map_err(|error| format!("打开 PostgreSQL EventLog 失败: {error:?}"));
            }
        }
        LiveEventPipeline::open_configured(
            self.root.clone(),
            log_name,
            currency,
            self.segment_events,
        )
        .map_err(|error| format!("打开运行时 EventLog 失败: {error:?}"))
    }
}

fn open_runtime_pipeline(
    config: &RuntimeConfig,
    root: &Path,
    log_name: impl Into<String>,
    currency: impl Into<String>,
) -> Result<LiveEventPipeline, String> {
    let storage = PipelineStorage::from_config(config)?;
    let mut storage = storage;
    storage.root = root.to_path_buf();
    storage.open(log_name, currency)
}

fn event_log_exists(config: &RuntimeConfig, root: &Path, log_name: &str) -> Result<bool, String> {
    if config.storage.backend == StorageBackend::Postgres {
        #[cfg(not(feature = "postgres"))]
        return Err("当前 qx-cli 未启用 postgres feature；无法查询 PostgreSQL EventLog".into());
        #[cfg(feature = "postgres")]
        {
            let dsn = postgres_dsn(config)?;
            let store = PostgresEventLogStore::connect_with_pool_size(
                &dsn,
                config.storage.postgres_pool_size,
            )
            .map_err(|error| format!("连接 PostgreSQL EventLog 失败: {error:?}"))?;
            return store
                .read_if_exists(log_name)
                .map(|value| value.is_some())
                .map_err(|error| format!("查询 PostgreSQL EventLog 失败: {error:?}"));
        }
    }
    Ok(root.join(format!("{log_name}.json")).exists()
        || (config.storage.event_log_segment_events.is_some()
            && root.join(format!("{log_name}.manifest.json")).exists()))
}

fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn worker_metrics_dir(config: &RuntimeConfig) -> PathBuf {
    Path::new(&config.storage.data_dir).join("worker-metrics")
}

#[cfg(feature = "nats")]
fn worker_metrics_path(config: &RuntimeConfig, worker_id: &str) -> PathBuf {
    worker_metrics_dir(config).join(format!("{worker_id}.prom"))
}

#[cfg(feature = "nats")]
fn write_worker_metrics(path: &Path, body: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("worker metrics 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 worker metrics 目录失败: {error}"))?;
    let temporary = path.with_extension(format!("prom.tmp.{}", std::process::id()));
    std::fs::write(&temporary, body)
        .map_err(|error| format!("写入 worker metrics 临时文件失败: {error}"))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("提交 worker metrics 文件失败: {error}")
    })
}

fn read_worker_metrics(directory: &Path, now_ms: u64, stale_after_ms: u64) -> String {
    let mut paths = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("prom"))
            .collect::<Vec<_>>(),
        Err(_) => return String::new(),
    };
    paths.sort();
    let mut output = String::new();
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let content = if worker_metrics_stale(&content, now_ms, stale_after_ms) {
                force_worker_metrics_down(&content)
            } else {
                content
            };
            output.push_str(&content);
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
    }
    output
}

fn worker_metrics_stale(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
    let heartbeat_ms = content.lines().find_map(|line| {
        if !line.starts_with("qx_worker_heartbeat_timestamp_seconds") {
            return None;
        }
        line.split_whitespace()
            .last()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| (value * 1_000.0) as u64)
    });
    heartbeat_ms.is_none_or(|heartbeat| now_ms.saturating_sub(heartbeat) > stale_after_ms)
}

fn force_worker_metrics_down(content: &str) -> String {
    let mut output = String::new();
    let mut worker_label = None;
    for line in content.lines() {
        if line.starts_with("qx_worker_up{") {
            worker_label = line
                .split("worker=\"")
                .nth(1)
                .and_then(|value| value.split('\"').next())
                .map(str::to_string);
            let mut fields = line.split_whitespace().collect::<Vec<_>>();
            if let Some(value) = fields.last_mut() {
                *value = "0";
            }
            output.push_str(&fields.join(" "));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if let Some(worker) = worker_label {
        output.push_str(&format!(
            "qx_worker_metrics_stale{{worker=\"{worker}\"}} 1\n"
        ));
    }
    output
}

fn worker_metrics_unhealthy(content: &str, now_ms: u64, stale_after_ms: u64) -> bool {
    if worker_metrics_stale(content, now_ms, stale_after_ms) {
        return true;
    }
    content.lines().any(|line| {
        line.starts_with("qx_worker_up{")
            && line
                .split_whitespace()
                .last()
                .is_none_or(|value| value != "1")
    })
}

fn validate_research_snapshot_binding(
    strategy: &StrategyRuntimeConfig,
    research: &StrategyResearchSnapshot,
) -> Result<(), String> {
    if let Some(expected) = strategy.research_data_fingerprint.as_deref() {
        let actual = research.candidate.config.data_fingerprint.as_str();
        if actual != expected {
            return Err(format!(
                "research snapshot data_fingerprint 不匹配: expected={expected} actual={actual}"
            ));
        }
    }
    Ok(())
}

fn configured_api_readiness(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    control_store: &ControlStateBackend,
    metrics_dir: &Path,
    now_ms: u64,
    stale_after_ms: u64,
) -> ApiReadiness {
    if control_store.load().is_err() {
        return ApiReadiness {
            ready: false,
            detail: "control_store_unavailable".into(),
        };
    }

    if config.environment.eq_ignore_ascii_case("production")
        && !production_trading_assets_ready(config, runtime_config_path)
    {
        return ApiReadiness {
            ready: false,
            detail: "trading_safety_assets_unavailable".into(),
        };
    }

    let strategies = if config.strategies.is_empty() {
        std::slice::from_ref(&config.strategy)
    } else {
        config.strategies.as_slice()
    };
    let root = Path::new(&config.storage.data_dir);
    for strategy in strategies {
        if !strategy.research_snapshot_required {
            continue;
        }
        let Some(configured) = strategy.research_snapshot_path.as_deref() else {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_unavailable".into(),
            };
        };
        let path = resolve_runtime_asset_path(runtime_config_path, root, configured);
        if !path.is_file() {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_unavailable".into(),
            };
        }
        let payload = match std::fs::read_to_string(&path) {
            Ok(payload) => payload,
            Err(_) => {
                return ApiReadiness {
                    ready: false,
                    detail: "research_snapshot_unreadable".into(),
                }
            }
        };
        let research = match StrategyResearchSnapshot::from_json(&payload) {
            Ok(research) => research,
            Err(_) => {
                return ApiReadiness {
                    ready: false,
                    detail: "research_snapshot_invalid".into(),
                }
            }
        };
        if validate_research_snapshot_binding(strategy, &research).is_err()
            || research
                .validate_for(
                    &strategy.version,
                    strategy
                        .research_data_fingerprint
                        .as_deref()
                        .unwrap_or(research.candidate.config.data_fingerprint.as_str()),
                    now_ms,
                    config.environment.eq_ignore_ascii_case("production"),
                )
                .is_err()
        {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_invalid".into(),
            };
        }
    }

    let metrics_unhealthy = std::fs::read_dir(metrics_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            (entry.path().extension().and_then(|value| value.to_str()) == Some("prom"))
                .then(|| std::fs::read_to_string(entry.path()).ok())
        })
        .flatten()
        .any(|content| worker_metrics_unhealthy(&content, now_ms, stale_after_ms));
    if metrics_unhealthy {
        return ApiReadiness {
            ready: false,
            detail: "worker_dependency_unavailable".into(),
        };
    }

    ApiReadiness {
        ready: true,
        detail: "dependencies_ready".into(),
    }
}

fn non_empty_env(name: &str) -> bool {
    !name.trim().is_empty() && std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

fn production_trading_assets_ready(config: &RuntimeConfig, runtime_config_path: &Path) -> bool {
    config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && matches!(
                    worker.role,
                    WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
                )
        })
        .all(|worker| {
            let credentials_ready = if let Some(credentials) = worker.credential_env.as_ref() {
                non_empty_env(&credentials.api_key) && non_empty_env(&credentials.secret)
            } else if let Some(credentials) = worker.credential_files.as_ref() {
                resolve_runtime_relative_path(runtime_config_path, &credentials.api_key).is_file()
                    && resolve_runtime_relative_path(runtime_config_path, &credentials.secret)
                        .is_file()
            } else if let Some(endpoint) = worker.endpoint.as_deref() {
                let path = resolve_runtime_relative_path(runtime_config_path, endpoint);
                let Ok(payload) = std::fs::read_to_string(path) else {
                    return false;
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    return false;
                };
                let Some(credentials) = value.get("credential_env") else {
                    return false;
                };
                let env_value = |key: &str| {
                    credentials
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(non_empty_env)
                };
                env_value("api_key") && env_value("secret")
            } else {
                false
            };
            if !credentials_ready {
                return false;
            }
            worker.instrument_spec_path.as_deref().is_none_or(|path| {
                resolve_runtime_relative_path(runtime_config_path, path).is_file()
            })
        })
}

#[cfg(feature = "nats")]
fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[derive(Clone)]
enum ControlStateBackend {
    Files(JsonStateStore),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteControlStore),
    #[cfg(feature = "postgres")]
    Postgres(PostgresControlStore),
}

impl ControlStateBackend {
    fn load(&self) -> Result<ControlPlane, String> {
        match self {
            Self::Files(store) => load_control_state(store.root()),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store
                .load_if_exists()
                .map_err(|error| format!("读取 SQLite 控制面失败: {error:?}"))
                .map(|state| state.unwrap_or_default()),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store
                .load_if_exists()
                .map_err(|error| format!("读取 PostgreSQL 控制面失败: {error:?}"))
                .map(|state| state.unwrap_or_default()),
        }
    }

    fn transact<T, E, F>(&self, update: F) -> Result<(ControlPlane, Result<T, E>), String>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, E>,
    {
        match self {
            Self::Files(store) => store
                .transact_control(update)
                .map_err(|error| format!("文件控制面事务失败: {error:?}")),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(store) => store
                .transact_control(update)
                .map_err(|error| format!("SQLite 控制面事务失败: {error:?}")),
            #[cfg(feature = "postgres")]
            Self::Postgres(store) => store
                .transact_control(update)
                .map_err(|error| format!("PostgreSQL 控制面事务失败: {error:?}")),
        }
    }
}

fn configured_control_store(config: &RuntimeConfig) -> Result<ControlStateBackend, String> {
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    match config.storage.backend {
        StorageBackend::Files => Ok(ControlStateBackend::Files(JsonStateStore::new(root))),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = root;
                Err(
                    "当前 qx-cli 未启用 sqlite feature；请使用 --features sqlite 启动生产配置"
                        .into(),
                )
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                SqliteControlStore::new(path)
                    .map(ControlStateBackend::Sqlite)
                    .map_err(|error| format!("初始化 SQLite 控制面失败: {error:?}"))
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = root;
                Err(
                    "当前 qx-cli 未启用 postgres feature；请使用 --features postgres 启动 PostgreSQL 配置"
                        .into(),
                )
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = postgres_dsn(config)?;
                PostgresControlStore::connect_with_pool_size(
                    &dsn,
                    config.storage.postgres_pool_size,
                )
                .map(ControlStateBackend::Postgres)
                .map_err(|error| format!("初始化 PostgreSQL 控制面失败: {error:?}"))
            }
        }
    }
}

fn configured_command_queue(
    config: &RuntimeConfig,
    root: &Path,
) -> Result<Arc<dyn ControlCommandQueueBackend>, String> {
    match config.storage.backend {
        StorageBackend::Files => Ok(Arc::new(ControlCommandQueue::new(
            root.join("control-queue"),
        ))),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = root;
                Err("当前 qx-cli 未启用 sqlite feature，无法初始化 SQLite 控制命令队列".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                SqliteControlCommandQueue::new(path)
                    .map(|queue| Arc::new(queue) as Arc<dyn ControlCommandQueueBackend>)
                    .map_err(|error| format!("初始化 SQLite 控制命令队列失败: {error:?}"))
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = root;
                Err(
                    "当前 qx-cli 未启用 postgres feature，无法初始化 PostgreSQL 控制命令队列"
                        .into(),
                )
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = postgres_dsn(config)?;
                PostgresControlCommandQueue::connect_with_pool_size(
                    &dsn,
                    config.storage.postgres_pool_size,
                )
                .map(|queue| Arc::new(queue) as Arc<dyn ControlCommandQueueBackend>)
                .map_err(|error| format!("初始化 PostgreSQL 控制命令队列失败: {error:?}"))
            }
        }
    }
}

enum ConfiguredJobQueue {
    Files(FileJobQueue),
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteJobQueue),
    #[cfg(feature = "postgres")]
    Postgres(PostgresJobQueue),
}

impl ConfiguredJobQueue {
    fn enqueue(
        &self,
        job: JobSpec,
        run: qx_scheduler::JobRun,
        enqueued_ts: u64,
    ) -> Result<(), StorageError> {
        match self {
            Self::Files(queue) => queue.enqueue(job, run, enqueued_ts).map(|_| ()),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.enqueue(job, run, enqueued_ts).map(|_| ()),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.enqueue(job, run, enqueued_ts).map(|_| ()),
        }
    }

    fn available(&self, now: u64) -> Result<Vec<QueuedJob>, StorageError> {
        match self {
            Self::Files(queue) => queue.available(now),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.available(now),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.available(now),
        }
    }

    fn claim(
        &self,
        run_id: u64,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<JobLease, StorageError> {
        match self {
            Self::Files(queue) => queue.claim(run_id, owner, now, lease_seconds),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.claim(run_id, owner, now, lease_seconds),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.claim(run_id, owner, now, lease_seconds),
        }
    }

    fn ack_at(
        &self,
        run_id: u64,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        match self {
            Self::Files(queue) => queue.ack_at(run_id, owner, fencing_token, now).map(|_| ()),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(queue) => queue.ack_at(run_id, owner, fencing_token, now).map(|_| ()),
            #[cfg(feature = "postgres")]
            Self::Postgres(queue) => queue.ack_at(run_id, owner, fencing_token, now).map(|_| ()),
        }
    }
}

fn runtime_path(root: &Path, configured: &str) -> std::path::PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn configured_job_queue(config: &RuntimeConfig, root: &Path) -> Result<ConfiguredJobQueue, String> {
    match config.storage.backend {
        StorageBackend::Files => Ok(ConfiguredJobQueue::Files(FileJobQueue::new(runtime_path(
            root,
            &config.scheduler.job_queue_path,
        )))),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = root;
                Err("当前 qx-cli 未启用 sqlite feature，无法初始化 SQLite JobQueue".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                SqliteJobQueue::new(path)
                    .map(ConfiguredJobQueue::Sqlite)
                    .map_err(|error| format!("初始化 SQLite JobQueue 失败: {error:?}"))
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = root;
                Err("当前 qx-cli 未启用 postgres feature，无法初始化 PostgreSQL JobQueue".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = postgres_dsn(config)?;
                PostgresJobQueue::connect_with_pool_size(&dsn, config.storage.postgres_pool_size)
                    .map(ConfiguredJobQueue::Postgres)
                    .map_err(|error| format!("初始化 PostgreSQL JobQueue 失败: {error:?}"))
            }
        }
    }
}

#[cfg(feature = "postgres")]
fn postgres_dsn(config: &RuntimeConfig) -> Result<String, String> {
    let env_name = config
        .storage
        .postgres_dsn_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "PostgreSQL backend 缺少 postgres_dsn_env".to_string())?;
    std::env::var(env_name).map_err(|error| {
        format!(
            "PostgreSQL DSN 环境变量 {} 不可用；凭证不能写入运行时配置: {}",
            env_name, error
        )
    })
}

fn configured_postgres_dsn(config: &RuntimeConfig) -> Result<Option<String>, String> {
    if config.storage.backend != StorageBackend::Postgres {
        return Ok(None);
    }
    #[cfg(not(feature = "postgres"))]
    {
        Err("当前 qx-cli 未启用 postgres feature，无法初始化 PostgreSQL EventLog".into())
    }
    #[cfg(feature = "postgres")]
    {
        postgres_dsn(config).map(Some)
    }
}

fn build_configured_api_service(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<ApiService, String> {
    std::fs::create_dir_all(&config.storage.data_dir)
        .map_err(|error| format!("创建运行时 data_dir 失败: {error}"))?;
    let control_root = Path::new(&config.storage.data_dir).to_path_buf();
    let control_store = configured_control_store(config)?;
    let control = control_store.load()?;
    let command_queue = configured_command_queue(config, &control_root)?;
    let mut state = ApiState::default();
    state.control = control;
    let (job_runs, ledger_entries, reconcile_reports) = load_api_query_models(config)?;
    state.job_runs = job_runs;
    state.ledger_entries = ledger_entries;
    state.reconcile_reports = reconcile_reports
        .into_iter()
        .map(|report| (report.worker_id.clone(), report))
        .collect();
    if let Some(snapshot) = load_api_account_snapshot(config)? {
        state
            .publish_snapshot(snapshot)
            .map_err(|error| format!("装载 API 账户查询快照失败: {error}"))?;
    }
    let mut policy = ApiPolicy::new();
    for (operator_id, operator) in &config.api.operators {
        policy = policy.grant(operator_id.clone(), operator.permission);
    }
    let metrics_dir = worker_metrics_dir(config);
    let worker_metrics_stale_after_ms = config.messaging.worker_stale_after_ms;
    let readiness_store = control_store.clone();
    let readiness_config = config.clone();
    let readiness_runtime_config_path = runtime_config_path.to_path_buf();
    let readiness_metrics_dir = metrics_dir.clone();
    let service = if config.api.operators.is_empty() {
        ApiService::new(state)
    } else {
        ApiService::with_policy(state, policy)
    }
    .with_worker_metrics_provider(move || {
        read_worker_metrics(
            &metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_readiness_provider(move || {
        configured_api_readiness(
            &readiness_config,
            &readiness_runtime_config_path,
            &readiness_store,
            &readiness_metrics_dir,
            runtime_timestamp_ms(),
            worker_metrics_stale_after_ms,
        )
    })
    .with_control_submitter({
        let store = control_store.clone();
        move |command, granted, ts| {
            if matches!(&command.kind, CommandKind::SubmitOrder) {
                order_from_submit_command(&command).map_err(|error| {
                    ControlSubmitError::Rejected(qx_control::ControlError::Invalid(format!(
                        "SubmitOrder 载荷非法: {error:?}"
                    )))
                })?;
            }
            let (plane, result) = store
                .transact(|plane| {
                    plane
                        .submit_as(command, granted, ts)
                        .map_err(ControlSubmitError::Rejected)
                })
                .map_err(ControlSubmitError::Unavailable)?;
            result.map(|audit| (plane, audit))
        }
    })
    .with_command_enqueuer({
        let queue = Arc::clone(&command_queue);
        move |command, ts| {
            queue
                .enqueue_command(command, ts)
                .map(|_| ())
                .map_err(|error| format!("写入控制命令队列失败: {error:?}"))
        }
    });
    if config.storage.backend == StorageBackend::Sqlite {
        #[cfg(not(feature = "sqlite"))]
        return Err("当前 qx-cli 未启用 sqlite feature；请使用 cargo run -p qx-cli --features sqlite -- serve ...".into());
        #[cfg(feature = "sqlite")]
        {
            let path = config
                .storage
                .sqlite_path
                .as_deref()
                .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
            let bucket = SqliteTokenBucket::new(path, "api", 100, 100)
                .map_err(|error| format!("初始化 SQLite API 限流失败: {error:?}"))?;
            let service = service.with_sqlite_rate_limit(bucket);
            return Ok(service);
        }
    }
    Ok(service)
}

/// 从持久化调度状态、账户 EventLog 和对账报告构造 API 读模型。
///
/// 这些数据只进入 QueryPort，不会被 API 反向写入 Scheduler、Ledger 或
/// Reconcile worker，避免查询层成为第二个事实拥有者。
type ApiQueryModels = (
    Vec<qx_scheduler::JobRun>,
    Vec<qx_core::LedgerEntry>,
    Vec<ReconcileReportSnapshot>,
);

fn load_api_query_models(config: &RuntimeConfig) -> Result<ApiQueryModels, String> {
    let root = Path::new(&config.storage.data_dir);
    let job_runs = {
        let state_path = runtime_path(root, &config.scheduler.state_path);
        if state_path.exists() {
            JsonStateStore::new(root)
                .load_scheduler_at(Path::new(&config.scheduler.state_path))
                .map_err(|error| format!("读取 API Scheduler 读模型失败: {error:?}"))?
                .runs()
        } else {
            Vec::new()
        }
    };

    let mut ledger_entries = Vec::new();
    if let Some(worker) = config.workers.iter().find(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
            )
            && worker.account_id.is_some()
            && worker.venue_id.is_some()
    }) {
        if let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        {
            if let Some(log_name) = account_event_log_name(account_id, venue_id) {
                if event_log_exists(config, root, &log_name)? {
                    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
                        .map_err(|error| format!("读取 API Ledger 读模型失败: {error}"))?;
                    ledger_entries = pipeline.ledger().entries().to_vec();
                }
            }
        }
    }

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
    Ok((job_runs, ledger_entries, reconcile_reports))
}

/// 从已持久化的账户 EventLog 构造 API 查询快照。
///
/// 该快照只是 QueryPort 的读模型：所有余额、持仓、订单和成交仍由 EventLog
/// 重放得到，不会反向写入 Ledger，也不会把柜台观察当成交易事实。
fn load_api_account_snapshot(config: &RuntimeConfig) -> Result<Option<AccountSnapshot>, String> {
    let worker = config.workers.iter().find(|worker| {
        worker.enabled
            && matches!(
                worker.role,
                WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
            )
            && worker.account_id.is_some()
            && worker.venue_id.is_some()
    });
    let Some(worker) = worker else {
        return Ok(None);
    };
    let account_id = worker.account_id.as_deref().unwrap_or_default();
    let venue_id = worker.venue_id.as_deref().unwrap_or_default();
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Ok(None);
    };
    let root = Path::new(&config.storage.data_dir);
    if !event_log_exists(config, root, &log_name)? {
        return Ok(None);
    }
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("打开 API 账户 EventLog 失败: {error}"))?;
    let runtime_snapshot = pipeline.snapshot();
    let as_of = runtime_snapshot.last_engine_ts.max(1);
    let mut snapshot = AccountSnapshot::new(1, account_id, "default", venue_id, as_of);
    snapshot.header.event_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.seq)
        .unwrap_or(0);
    snapshot.cash_raw = pipeline.ledger().cash_balances_for(account_id);
    snapshot.equity_raw = pipeline
        .ledger()
        .equity_for(account_id, pipeline.marks(), "USDT")
        .unwrap_or_else(|| pipeline.ledger().cash_for(account_id, "USDT"));
    snapshot.available_raw = snapshot.equity_raw;
    snapshot.orders = runtime_snapshot
        .orders
        .iter()
        .map(|order| {
            (
                order.client_id,
                qx_protocol::OrderSnapshot {
                    order_id: order.client_id,
                    client_order_id: order.client_id,
                    instrument: order.instrument.clone(),
                    side: order.side,
                    quantity_raw: order.qty.raw(),
                    filled_raw: order.filled.raw(),
                    status: order.status,
                },
            )
        })
        .collect();
    snapshot.fills = pipeline
        .log()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Filled { fill } => Some((
                event.seq,
                qx_protocol::FillSnapshot {
                    fill_id: event.seq,
                    order_id: fill.order_id,
                    quantity_raw: fill.qty.raw(),
                    price_raw: fill.price.raw(),
                    fee_raw: fill.fee.raw(),
                    ts: fill.ts,
                },
            )),
            _ => None,
        })
        .collect();

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
        let quantity_raw = venue_position
            .map(|position| position.quantity.raw())
            .unwrap_or_else(|| position.quantity.raw());
        if quantity_raw == 0 && venue_position.is_none() {
            continue;
        }
        let average_price_raw = venue_position
            .and_then(|position| position.average_price)
            .map(|price| price.raw())
            .unwrap_or_else(|| position.average_entry.raw());
        let mark_price_raw = venue_position
            .and_then(|position| position.mark_price)
            .or_else(|| pipeline.marks().get(&instrument).copied())
            .map(|price| price.raw())
            .unwrap_or(0);
        snapshot.positions.insert(
            instrument.clone(),
            qx_protocol::PositionSnapshot {
                instrument,
                quantity_raw,
                today_quantity_raw: quantity_raw,
                average_price_raw,
                mark_price_raw,
                unrealized_pnl_raw: venue_position
                    .map(|position| position.unrealized_pnl.raw())
                    .unwrap_or(0),
                margin_raw: venue_position
                    .map(|position| position.initial_margin.raw())
                    .unwrap_or(0),
            },
        );
    }
    snapshot.reconcile.recovery_state = "eventlog-replayed".into();
    Ok(Some(snapshot))
}

fn run_runtime_check(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let health = supervisor
        .health()
        .lock()
        .map_err(|_| "运行时健康锁已中毒".to_string())?
        .snapshot(0, config.shutdown_timeout_ms);
    println!(
        "[运行时 · 配置] environment={} api={:?} workers={} storage={:?}",
        config.environment,
        config.api.transport,
        health.services.len(),
        config.storage.backend
    );
    println!(
        "[运行时 · 指纹] config_fingerprint={} locked={}",
        config.fingerprint()?,
        config.config_fingerprint.is_some()
    );
    println!("[运行时 · 健康] overall={:?}", health.overall);
    for service in health.services {
        println!(
            "  {} role={:?} status={:?}",
            service.id, service.role, service.status
        );
    }
    Ok(())
}

fn repository_deploy_path(file_name: &str) -> PathBuf {
    let source_tree_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join(file_name);
    if source_tree_path.exists() {
        source_tree_path
    } else {
        PathBuf::from("deploy").join(file_name)
    }
}

fn print_cli_help() {
    println!(
        r#"牵星 Qianxing CLI

常用入口：
  init [runtime.json] [--force]
      从 deploy/qianxing.runtime.example.json 创建本地运行时配置。
  backtest [runtime.json] [bar-frame.json] [market-spec.json]
      使用统一 Rust 撮合引擎运行跨语言策略回测。
  builtin-strategies
      列出可直接用于回测/Paper/策略接入的 17 个内置策略。
  builtin-backtest <strategy> <bar-frame.json> [market-spec.json] [quantity]
      使用内置策略和统一 Rust 撮合引擎回测。
  multi-builtin-backtest <strategy> <primary-bar.json> <reference-bar.json> [primary-spec.json] [reference-spec.json] [quantity]
      对齐两条 BarFrame，使用同一信号驱动双腿独立账户回测。
  fast-backtest <manifest.json>
      并行执行多个独立回测任务，适合多标的、多币种和多参数批量验证。
  ccxt-builtin-backtest <ccxt-config> <strategy> <instrument> <start_ms> <end_ms> [timeframe] [market-spec.json] [quantity]
      一次完成 CCXT OHLCV 获取、内置策略回测和结果输出。
  paper-check [runtime.json]
      按 Scheduler → Strategy → Paper Execution → Ledger 验收主体链路。
  live-check [production.runtime.json]
      执行实盘启动前静态门禁，不连接交易所、不发送订单。
  runtime-check [runtime.json]
      校验运行时拓扑并输出健康与配置指纹。

核心运行入口：
  serve, supervise, scheduler-worker, strategy-worker, paper-worker
  binance-worker, ccxt-worker, ccxt-fetch-ohlcv, ccxt-backtest
  strategy-backtest, reconcile, ecosystem, paper

使用 `qianxing help` 查看入口摘要；既有入口参数保持兼容，完整说明见 README.md 与 deploy/README.md。"#
    );
}

fn run_init(output: &Path, force: bool) -> Result<(), String> {
    let template = repository_deploy_path("qianxing.runtime.example.json");
    if !template.exists() {
        return Err(format!("找不到运行时模板: {}", template.display()));
    }
    if output.exists() && !force {
        return Err(format!(
            "目标配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建配置目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::copy(&template, output)
        .map_err(|error| format!("写入运行时配置失败 {}: {error}", output.display()))?;
    let config = read_runtime_config(output)?;
    println!(
        "[初始化] 已创建 {} environment={} fingerprint={}",
        output.display(),
        config.environment,
        config.fingerprint()?
    );
    println!("下一步：qianxing backtest 或 qianxing paper-check");
    Ok(())
}

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

fn run_live_check(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut failures = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if !config.environment.eq_ignore_ascii_case("production") {
        failures.push(format!(
            "environment 必须为 production，当前为 {}",
            config.environment
        ));
    }
    match config.config_fingerprint.as_deref() {
        Some(expected) => match config.verify_fingerprint() {
            Ok(()) => println!("[PASS] config_fingerprint locked=true value={expected}"),
            Err(error) => failures.push(error),
        },
        None => failures.push("production 必须配置 config_fingerprint 发布锁".into()),
    }

    for (label, configured) in [
        (
            "api.tls.certificate_chain",
            config
                .api
                .tls
                .as_ref()
                .map(|tls| tls.certificate_chain.as_str()),
        ),
        (
            "api.tls.private_key",
            config.api.tls.as_ref().map(|tls| tls.private_key.as_str()),
        ),
        (
            "api.tls.client_ca",
            config.api.tls.as_ref().map(|tls| tls.client_ca.as_str()),
        ),
    ] {
        match configured {
            Some(file) if Path::new(file).exists() => println!("[PASS] {label}={file}"),
            Some(file) => failures.push(format!("{label} 文件不存在: {file}")),
            None => failures.push(format!("{label} 未配置")),
        }
    }

    let mut execution_count = 0_usize;
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        if worker.role == WorkerRole::Execution {
            execution_count += 1;
            if worker
                .venue_id
                .as_deref()
                .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
            {
                failures.push(format!(
                    "{} 是 production 中不允许启用的 Paper Execution worker",
                    worker.id
                ));
            }
            let spec = worker.instrument_spec_path.as_deref().unwrap_or_default();
            let spec_path = resolve_runtime_relative_path(path, spec);
            if spec.is_empty() || !spec_path.exists() {
                failures.push(format!(
                    "{} instrument_spec_path 不可用: {}",
                    worker.id,
                    spec_path.display()
                ));
            } else {
                println!(
                    "[PASS] {} instrument_spec={}",
                    worker.id,
                    spec_path.display()
                );
            }
            if worker.max_order_notional_raw.is_none() || worker.max_position_notional_raw.is_none()
            {
                failures.push(format!("{} 缺少订单或持仓名义额上限", worker.id));
            }
        }
        if matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        ) {
            let credential_ready = worker.credential_env.as_ref().is_some_and(|credential| {
                std::env::var_os(&credential.api_key).is_some_and(|value| !value.is_empty())
                    && std::env::var_os(&credential.secret).is_some_and(|value| !value.is_empty())
            }) || worker.credential_files.as_ref().is_some_and(
                |credential| {
                    Path::new(&credential.api_key).is_file()
                        && Path::new(&credential.secret).is_file()
                },
            );
            if credential_ready {
                println!("[PASS] {} credentials source is available", worker.id);
            } else {
                failures.push(format!("{} 凭据环境变量或凭据文件不可用", worker.id));
            }
        }
    }
    if execution_count == 0 {
        failures.push("production 至少需要一个启用的 Execution worker".into());
    }

    for strategy in config
        .strategies
        .iter()
        .chain(std::iter::once(&config.strategy))
        .filter(|strategy| strategy.account_id.is_some() || strategy.venue_id.is_some())
    {
        if let Some(snapshot) = strategy.research_snapshot_path.as_deref() {
            let snapshot_path = resolve_runtime_relative_path(path, snapshot);
            if snapshot_path.is_file() {
                println!("[PASS] research_snapshot={}", snapshot_path.display());
            } else {
                failures.push(format!(
                    "research_snapshot_path 文件不存在: {}",
                    snapshot_path.display()
                ));
            }
        }
    }
    if config.api.bind.starts_with("127.") || config.api.bind.starts_with("localhost") {
        warnings.push("API 仅绑定本机地址，适合单机部署，不适合跨节点访问".into());
    }

    for warning in warnings {
        println!("[WARN] {warning}");
    }
    if failures.is_empty() {
        println!("[PASS] live-check 全部通过：未连接交易所，未发送订单");
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("[FAIL] {failure}");
        }
        Err(format!("实盘前置检查失败，共 {} 项", failures.len()))
    }
}

fn utc_schedule_tick(timestamp_ms: u64) -> (String, ScheduleTick) {
    let seconds = timestamp_ms / 1_000;
    let days = (seconds / 86_400) as i64;
    let seconds_in_day = seconds % 86_400;
    let hour = (seconds_in_day / 3_600) as u8;
    let minute = ((seconds_in_day % 3_600) / 60) as u8;
    let (year, month, day) = civil_from_days(days);
    let weekday = (days + 4).rem_euclid(7) as u8;
    (
        format!("{year:04}{month:02}{day:02}"),
        ScheduleTick {
            minute,
            hour,
            day,
            month,
            weekday,
        },
    )
}

// Howard Hinnant 的 civil-from-days 算法；调度统一使用 UTC，避免把本机时区
// 隐式带入 RunManifest 和 Cron 判定。
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u8, day as u8)
}

fn scheduler_manifest(worker_id: &str, trading_day: &str, now: u64) -> qx_core::RunManifest {
    qx_core::RunManifest {
        run_id: format!("{worker_id}-{trading_day}-{now}"),
        code_commit: "workspace".into(),
        config_hash: "runtime-scheduler-v1".into(),
        data_fingerprint: format!("scheduler:{trading_day}"),
        clock_start: now,
        clock_end: now,
        global_seed: 0,
        determinism_mode: true,
        result_hash: format!("scheduler-{now}"),
        strategy_version: "scheduler-dispatch-v1".into(),
        instrument_spec_version: "runtime-v1".into(),
        model_fingerprint: "scheduler-dispatch".into(),
        input_event_hash: format!("input-{now}"),
        output_event_hash: format!("output-{now}"),
        runtime_version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn scheduler_jobs_path(runtime_config_path: &Path, configured: &str) -> PathBuf {
    resolve_runtime_relative_path(runtime_config_path, configured)
}

fn load_scheduler_state(
    config: &RuntimeConfig,
    root: &Path,
    runtime_config_path: &Path,
) -> Result<(JsonStateStore, Scheduler, PathBuf), String> {
    let store = JsonStateStore::new(root.to_path_buf());
    let state_reference = Path::new(&config.scheduler.state_path).to_path_buf();
    let state_path = runtime_path(root, &config.scheduler.state_path);
    let scheduler = if state_path.exists() {
        store
            .load_scheduler_at(&state_reference)
            .map_err(|error| format!("加载 Scheduler 状态失败: {error:?}"))?
    } else {
        let mut scheduler = Scheduler::default();
        let jobs_path = scheduler_jobs_path(runtime_config_path, &config.scheduler.jobs_path);
        if jobs_path.exists() {
            let jobs: Vec<JobSpec> = serde_json::from_str(
                &std::fs::read_to_string(&jobs_path)
                    .map_err(|error| format!("读取 Scheduler JobSpec 失败: {error}"))?,
            )
            .map_err(|error| format!("Scheduler JobSpec JSON 无效: {error}"))?;
            for job in jobs {
                scheduler
                    .register(job)
                    .map_err(|error| format!("注册 Scheduler JobSpec 失败: {error:?}"))?;
            }
        }
        store
            .save_scheduler_at(&state_reference, &scheduler)
            .map_err(|error| format!("初始化 Scheduler 状态失败: {error:?}"))?;
        scheduler
    };
    Ok((store, scheduler, state_reference))
}

fn dispatch_scheduled_jobs(
    state_store: &JsonStateStore,
    state_path: &Path,
    queue: &ConfiguredJobQueue,
    tick: &ScheduleTick,
    trading_day: &str,
    manifest: &qx_core::RunManifest,
    now: u64,
) -> Result<usize, String> {
    let (_, result) = state_store
        .transact_scheduler_at(state_path, |scheduler| {
            let completed = scheduler.completed_jobs();
            let job_ids = scheduler
                .due_jobs(tick, &completed)
                .map_err(|error| format!("计算 Scheduler 到期任务失败: {error:?}"))?
                .into_iter()
                .map(|job| job.job_id.clone())
                .collect::<Vec<_>>();
            let mut queued = 0_usize;
            for job_id in job_ids {
                let job = scheduler
                    .job(&job_id)
                    .cloned()
                    .ok_or_else(|| format!("Scheduler JobSpec 不存在: {job_id}"))?;
                let run = scheduler
                    .start_run_at(&job_id, trading_day, manifest.digest(), now)
                    .map_err(|error| format!("创建 JobRun 失败: {error:?}"))?;
                if run.status == JobStatus::Running {
                    queue
                        .enqueue(job, run, now)
                        .map_err(|error| format!("写入 JobQueue 失败: {error:?}"))?;
                    queued += 1;
                }
            }
            Ok(queued)
        })
        .map_err(|error| format!("Scheduler 状态事务失败: {error:?}"))?;
    result
}

fn account_event_log_name(account_id: &str, venue_id: &str) -> Option<String> {
    let venue = venue_id.trim().to_ascii_lowercase();
    if venue.is_empty() {
        return None;
    }
    let prefix = if venue == "paper" {
        "paper"
    } else if venue.contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    Some(format!("{prefix}-{account_id}-{venue_id}-events"))
}

fn ccxt_event_log_name(worker: &WorkerConfig) -> String {
    let venue = worker.venue_id.as_deref().unwrap_or("unknown");
    let prefix = if venue.to_ascii_lowercase().contains("binance") {
        "binance"
    } else {
        "ccxt"
    };
    format!(
        "{prefix}-{}-{venue}-events",
        worker.account_id.as_deref().unwrap_or("unknown")
    )
}

fn ccxt_market_event_log_name(worker: &WorkerConfig) -> String {
    format!("ccxt-market-{}-events", worker.id)
}

fn resolve_ccxt_config_path(runtime_path: &Path, configured: &str) -> String {
    resolve_runtime_relative_path(runtime_path, configured)
        .to_string_lossy()
        .into_owned()
}

fn is_explicit_absolute_path(configured: &str) -> bool {
    let bytes = configured.as_bytes();
    Path::new(configured).is_absolute()
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || configured.starts_with("\\\\")
}

fn resolve_runtime_relative_path(runtime_path: &Path, configured: &str) -> PathBuf {
    if is_explicit_absolute_path(configured) {
        PathBuf::from(configured)
    } else {
        runtime_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(configured)
    }
}

fn resolve_runtime_asset_path(
    runtime_config_path: &Path,
    storage_root: &Path,
    configured: &str,
) -> PathBuf {
    let config_candidate = resolve_runtime_relative_path(runtime_config_path, configured);
    if Path::new(configured).is_absolute() || config_candidate.exists() {
        return config_candidate;
    }
    let storage_candidate = runtime_path(storage_root, configured);
    if storage_candidate.exists() {
        return storage_candidate;
    }
    config_candidate
}

fn resolve_strategy_runtime_paths(strategy: &mut StrategyRuntimeConfig, runtime_path: &Path) {
    if let Some(configured) = strategy.target_snapshot_path.as_deref() {
        strategy.target_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.research_snapshot_path.as_deref() {
        strategy.research_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.bars_snapshot_path.as_deref() {
        strategy.bars_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.builtin_reference_bars_snapshot_path.as_deref() {
        strategy.builtin_reference_bars_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(executable) = strategy.external_executable.as_deref() {
        let path_like = executable.contains('/')
            || executable.contains('\\')
            || executable.starts_with('.')
            || Path::new(executable).is_absolute();
        if path_like {
            strategy.external_executable = Some(
                resolve_runtime_relative_path(runtime_path, executable)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(module) = strategy.python_module.as_deref() {
        let path_like = module.ends_with(".py") || module.contains('/') || module.contains('\\');
        if path_like {
            strategy.python_module = Some(
                resolve_runtime_relative_path(runtime_path, module)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(library) = strategy.c_abi_library.as_deref() {
        let path_like = library.contains('/')
            || library.contains('\\')
            || library.starts_with('.')
            || Path::new(library).is_absolute();
        if path_like {
            strategy.c_abi_library = Some(
                resolve_runtime_relative_path(runtime_path, library)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

fn verify_strategy_artifact(strategy: &StrategyRuntimeConfig) -> Result<(), String> {
    let Some(expected) = strategy.strategy_artifact_sha256.as_deref() else {
        return Ok(());
    };
    if let Some(reference) = strategy.external_executable.as_deref() {
        return qx_strategy::verify_file_sha256(reference, expected)
            .map_err(|error| format!("策略发布物校验失败 {}: {error}", reference));
    }
    let reference = strategy
        .python_module
        .as_deref()
        .ok_or_else(|| "strategy_artifact_sha256 缺少策略文件引用".to_string())?;
    // Python 可以配置 importable module name；Rust host 无法在不复制 Python
    // import 规则的情况下定位它，交由 Python worker 在 import 后按 __file__ 校验。
    // 显式文件路径仍在 spawn 前由 host 先校验，形成双重门禁。
    if !Path::new(reference).is_file()
        && !(reference.ends_with(".py")
            || reference.contains('/')
            || reference.contains('\\')
            || reference.starts_with('.')
            || Path::new(reference).is_absolute())
    {
        return Ok(());
    }
    qx_strategy::verify_file_sha256(reference, expected)
        .map_err(|error| format!("策略发布物校验失败 {}: {error}", reference))
}

fn validate_ccxt_worker_binding(
    worker: &WorkerConfig,
    ccxt_config_path: &Path,
) -> Result<(), String> {
    let expected = worker
        .venue_id
        .as_deref()
        .ok_or_else(|| format!("CCXT worker {} 缺少 venue_id", worker.id))?;
    let payload = std::fs::read_to_string(ccxt_config_path).map_err(|error| {
        format!(
            "读取 CCXT 配置失败 path={} error={error}",
            ccxt_config_path.display()
        )
    })?;
    let config: serde_json::Value =
        serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
            format!(
                "解析 CCXT 配置失败 path={} error={error}",
                ccxt_config_path.display()
            )
        })?;
    let configured = config
        .get("exchange_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "CCXT 配置缺少 exchange_id path={}",
                ccxt_config_path.display()
            )
        })?;
    if !configured.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "CCXT worker {} venue_id={} 与配置 exchange_id={} 不一致",
            worker.id, expected, configured
        ));
    }
    Ok(())
}

#[cfg(test)]
fn strategy_current_qty(root: &Path, config: &RuntimeConfig) -> Result<i128, String> {
    let Some(instrument_text) = config.strategy.instrument.as_deref() else {
        return Ok(0);
    };
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    strategy_current_qty_for(root, config, &instrument)
}

fn strategy_current_qty_for(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<i128, String> {
    let Some(account_id) = config.strategy.account_id.as_deref() else {
        return Ok(0);
    };
    // 多交易所套利的每条 intent 可以属于不同 venue；优先按
    // InstrumentId 的 venue 读取，不能把所有腿都误读成策略主腿。
    // 兼容旧的 paper/测试账户：历史配置可能把事件写入 strategy.venue_id
    // 对应的 EventLog，而 instrument 本身仍使用交易所 venue。
    let venue_id = instrument.venue.to_string();
    let Some(instrument_log_name) = account_event_log_name(account_id, &venue_id) else {
        return Err(format!(
            "Strategy 当前持仓暂不支持 venue_id={}；请先接入该 Venue 的账户事件归约",
            venue_id
        ));
    };
    let log_name = if event_log_exists(config, root, &instrument_log_name)? {
        instrument_log_name
    } else if let Some(configured_venue) = config.strategy.venue_id.as_deref() {
        let Some(configured_log_name) = account_event_log_name(account_id, configured_venue) else {
            return Ok(0);
        };
        if event_log_exists(config, root, &configured_log_name)? {
            configured_log_name
        } else {
            return Ok(0);
        }
    } else {
        return Ok(0);
    };
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("恢复 Strategy 账户 EventLog 失败: {error}"))?;
    Ok(pipeline
        .ledger()
        .position_for(account_id, instrument)
        .quantity
        .raw())
}

fn build_strategy_contract_input(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractInput, String> {
    let account_id = config
        .strategy
        .account_id
        .clone()
        .ok_or_else(|| "Python Strategy 必须配置 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .clone()
        .ok_or_else(|| "Python Strategy 必须配置 venue_id".to_string())?;
    if let Some(configured) = config.strategy.research_snapshot_path.as_deref() {
        let candidate = runtime_path(root, configured);
        let path = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(configured)
        };
        let research = StrategyResearchSnapshot::from_json(
            &std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读取 Python Strategy research snapshot 失败 {}: {error}",
                    path.display()
                )
            })?,
        )
        .map_err(|error| format!("Python Strategy research snapshot JSON 无效: {error:?}"))?;
        validate_research_snapshot_binding(&config.strategy, &research)?;
        let research_as_of = research.as_of;
        let data_fingerprint = research.candidate.config.data_fingerprint.clone();
        let (positions, cash, available_margin_raw, risk_state) =
            strategy_account_context(root, config, instrument)?;
        let context = StrategyContext {
            strategy_id: config
                .strategy
                .id
                .clone()
                .unwrap_or_else(|| config.strategy.version.clone()),
            strategy_version: config.strategy.version.clone(),
            data_fingerprint,
            as_of: research.as_of,
            research,
            account_id,
            venue_id,
            positions,
            cash,
            available_margin_raw,
            risk_state,
        };
        context
            .validate(now, config.environment.eq_ignore_ascii_case("production"))
            .map_err(|error| format!("Python StrategyContext 校验失败: {error}"))?;
        let bars = load_strategy_contract_bars(root, config, instrument, research_as_of)?
            .map(|(bars, _, _)| bars);
        return context.to_contract_input(request_id, instrument, bars);
    }

    let (positions, cash, available_margin_raw, risk_state) =
        strategy_account_context(root, config, instrument)?;
    let bars = load_strategy_contract_bars(root, config, instrument, now)?;
    let (bars, data_fingerprint, as_of) = if let Some((bars, data_fingerprint, as_of)) = bars {
        (Some(bars), data_fingerprint, as_of)
    } else {
        (None, "runtime-config-v1".into(), now.max(1))
    };
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: request_id.to_string(),
        strategy_id: config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| config.strategy.version.clone()),
        strategy_version: config.strategy.version.clone(),
        data_fingerprint,
        as_of,
        instrument: instrument.to_string(),
        positions,
        cash,
        available_margin_raw,
        risk_state,
        research_targets: BTreeMap::from([(instrument.to_string(), config.strategy.target_qty)]),
        bars,
    };
    input.validate()?;
    Ok(input)
}

const PYTHON_STRATEGY_TIMEOUT_MS: u64 = 2_000;

enum StrategyWireResponse {
    JsonLine(String),
    Frame(StrategyFrame),
}

struct PythonStrategyClient {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    responses: Option<Receiver<Result<StrategyWireResponse, String>>>,
    shared_input: Option<SharedRingWriter>,
    shared_output: Option<SharedRingReader>,
    ring_paths: Option<(PathBuf, PathBuf)>,
    timeout_ms: u64,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    label: String,
    transport: StrategyTransport,
    next_sequence: u64,
}

type StrategyProcessClient = PythonStrategyClient;

impl PythonStrategyClient {
    fn start(module: &str, timeout_ms: u64) -> Result<Self, String> {
        Self::start_with_transport(module, timeout_ms, StrategyTransport::Jsonl)
    }

    fn start_with_transport(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
    ) -> Result<Self, String> {
        Self::start_with_transport_config(
            module,
            timeout_ms,
            transport,
            SharedRingConfig::default(),
            None,
        )
    }

    fn start_with_transport_config(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
    ) -> Result<Self, String> {
        let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
        let python_path = python_module_search_path()?;
        let mut env = BTreeMap::new();
        env.insert(
            "PYTHONPATH".to_string(),
            python_path.to_string_lossy().into_owned(),
        );
        if let Some(artifact_sha256) = artifact_sha256 {
            env.insert(
                "QX_STRATEGY_ARTIFACT_SHA256".to_string(),
                artifact_sha256.to_string(),
            );
        }
        let mut args = vec![
            "-m".into(),
            "qianxing_strategy.worker".into(),
            "--module".into(),
            module.into(),
        ];
        if transport == StrategyTransport::FramedJson {
            args.push("--protocol".into());
            args.push("framed_json".into());
        }
        Self::start_process_with_transport_config(
            &python,
            &args,
            &env,
            timeout_ms,
            "Python Strategy",
            transport,
            ring_config,
        )
    }

    fn start_process_with_transport_config(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout_ms: u64,
        label: &str,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
    ) -> Result<Self, String> {
        if matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            ring_config
                .validate()
                .map_err(|error| format!("{label} 共享 ring 配置非法: {error}"))?;
        }
        let mut actual_args = args.to_vec();
        let mut shared_input = None;
        let mut shared_output = None;
        let mut ring_paths = None;
        if matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            let ring_id = NEXT_STRATEGY_RING_ID.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "qianxing-strategy-ring-{}-{ring_id}",
                std::process::id()
            ));
            let input_path = base.with_extension("input");
            let output_path = base.with_extension("output");
            let input = SharedRingWriter::create(&input_path, ring_config)
                .map_err(|error| format!("创建 {label} 输入 ring 失败: {error}"))?;
            let output = SharedRingWriter::create(&output_path, ring_config)
                .map_err(|error| format!("创建 {label} 输出 ring 失败: {error}"))?;
            drop(output);
            let reader = SharedRingReader::open(&output_path, ring_config)
                .map_err(|error| format!("打开 {label} 输出 ring 失败: {error}"))?;
            actual_args.extend([
                "--protocol".into(),
                if transport == StrategyTransport::SharedMemoryColumnar {
                    "shared_memory_columnar".into()
                } else {
                    "shared_memory_json".into()
                },
                "--input-ring".into(),
                input_path.to_string_lossy().into_owned(),
                "--output-ring".into(),
                output_path.to_string_lossy().into_owned(),
                "--ring-capacity".into(),
                ring_config.capacity.to_string(),
                "--ring-slot-bytes".into(),
                ring_config.slot_bytes.to_string(),
            ]);
            shared_input = Some(input);
            shared_output = Some(reader);
            ring_paths = Some((input_path, output_path));
        }
        let child_env = strategy_child_environment(env)?;
        let mut command = Command::new(executable);
        command
            .args(&actual_args)
            // 策略进程只能获得最小运行时环境，不能继承 API secret 等父进程变量。
            .env_clear()
            .envs(child_env);
        let shared = matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        );
        let mut child = command
            .stdin(if shared {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stdout(if shared {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            // Strategy stdout is the protocol; user diagnostics must use stderr.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("启动 {label} worker 失败: {error}"))?;
        let stdin = if shared { None } else { child.stdin.take() };
        let stdout = if shared { None } else { child.stdout.take() };
        if !shared && (stdin.is_none() || stdout.is_none()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{label} worker stdin/stdout 不可用"));
        }
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{label} worker stderr 不可用"));
            }
        };
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_tail_reader = Arc::clone(&stderr_tail);
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if let Ok(mut tail) = stderr_tail_reader.lock() {
                    tail.push_back(line.chars().take(512).collect());
                    while tail.len() > 16 {
                        tail.pop_front();
                    }
                }
            }
        });
        let responses = if shared {
            None
        } else {
            let stdout = stdout.expect("non-shared worker stdout checked above");
            let (sender, responses) = mpsc::channel();
            thread::spawn(move || match transport {
                StrategyTransport::Jsonl => {
                    let reader = BufReader::new(stdout);
                    for line in reader.lines() {
                        match line {
                            Ok(line) => {
                                if sender
                                    .send(Ok(StrategyWireResponse::JsonLine(line)))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(error) => {
                                let _ = sender
                                    .send(Err(format!("读取 Strategy worker 响应失败: {error}")));
                                return;
                            }
                        }
                    }
                    let _ = sender.send(Err("Strategy worker 已关闭输出".into()));
                }
                StrategyTransport::FramedJson => {
                    let mut reader = stdout;
                    loop {
                        match StrategyFrame::read_from(&mut reader, DEFAULT_MAX_FRAME_BYTES) {
                            Ok(frame) => {
                                if sender.send(Ok(StrategyWireResponse::Frame(frame))).is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                let _ = sender.send(Err(format!(
                                    "读取 Strategy worker 分帧响应失败: {error}"
                                )));
                                return;
                            }
                        }
                    }
                }
                StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar => {
                    unreachable!("shared transport has no pipe")
                }
            });
            Some(responses)
        };
        Ok(Self {
            child,
            stdin,
            responses,
            shared_input,
            shared_output,
            ring_paths,
            timeout_ms,
            stderr_tail,
            label: label.to_string(),
            transport,
            next_sequence: 1,
        })
    }

    fn diagnostics(&self) -> String {
        let Ok(tail) = self.stderr_tail.lock() else {
            return String::new();
        };
        if tail.is_empty() {
            String::new()
        } else {
            format!(
                "; stderr={}",
                tail.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        }
    }

    fn request(&mut self, input: &StrategyContractInput) -> Result<StrategyContractOutput, String> {
        let payload = input.to_json()?;
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let response = match self.transport {
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar => {
                let request_payload = if self.transport == StrategyTransport::SharedMemoryColumnar {
                    encode_strategy_columnar_input(input)?
                } else {
                    payload.into_bytes()
                };
                let frame = StrategyFrame::request(sequence, request_payload)
                    .encode(DEFAULT_MAX_FRAME_BYTES)
                    .map_err(|error| format!("编码 {} 共享分帧输入失败: {error}", self.label))?;
                let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
                let input_ring = self
                    .shared_input
                    .as_mut()
                    .ok_or_else(|| format!("{} 共享输入 ring 不可用", self.label))?;
                loop {
                    match input_ring.try_push(&frame) {
                        Ok(()) => break,
                        Err(SharedRingError::Full) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => {
                            return Err(format!("写入 {} 共享 ring 失败: {error}", self.label));
                        }
                    }
                }
                let output_ring = self
                    .shared_output
                    .as_mut()
                    .ok_or_else(|| format!("{} 共享输出 ring 不可用", self.label))?;
                loop {
                    match output_ring.try_pop_frame() {
                        Ok(frame) => break StrategyWireResponse::Frame(frame),
                        Err(SharedRingError::Empty) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(SharedRingError::Empty) => {
                            let _ = self.child.kill();
                            return Err(format!(
                                "{} worker 响应超时 timeout_ms={}{}",
                                self.label,
                                self.timeout_ms,
                                self.diagnostics()
                            ));
                        }
                        Err(error) => {
                            return Err(format!("读取 {} 共享 ring 失败: {error}", self.label));
                        }
                    }
                }
            }
            StrategyTransport::Jsonl | StrategyTransport::FramedJson => {
                let label = self.label.clone();
                let diagnostics = self.diagnostics();
                let stdin = self
                    .stdin
                    .as_mut()
                    .ok_or_else(|| format!("{} worker stdin 不可用", label))?;
                if self.transport == StrategyTransport::Jsonl {
                    stdin
                        .write_all(format!("{payload}\n").as_bytes())
                        .map_err(|error| {
                            format!("写入 {} 输入失败: {error}{}", label, diagnostics)
                        })?;
                } else {
                    let frame = StrategyFrame::request(sequence, payload.into_bytes())
                        .encode(DEFAULT_MAX_FRAME_BYTES)
                        .map_err(|error| format!("编码 {} 分帧输入失败: {error}", self.label))?;
                    stdin.write_all(&frame).map_err(|error| {
                        format!("写入 {} 分帧输入失败: {error}{}", label, diagnostics)
                    })?;
                }
                stdin
                    .flush()
                    .map_err(|error| format!("刷新 {} 输入失败: {error}{}", label, diagnostics))?;
                self.responses
                    .as_ref()
                    .ok_or_else(|| format!("{} worker 响应通道不可用", self.label))?
                    .recv_timeout(Duration::from_millis(self.timeout_ms))
                    .map_err(|error| match error {
                        mpsc::RecvTimeoutError::Timeout => {
                            let _ = self.child.kill();
                            format!(
                                "{} worker 响应超时 timeout_ms={}{}",
                                self.label,
                                self.timeout_ms,
                                self.diagnostics()
                            )
                        }
                        mpsc::RecvTimeoutError::Disconnected => {
                            format!("{} worker 响应通道已断开{}", self.label, self.diagnostics())
                        }
                    })??
            }
        };
        let line = match response {
            StrategyWireResponse::JsonLine(line) => line,
            StrategyWireResponse::Frame(frame) => {
                if frame.sequence != sequence {
                    return Err(format!(
                        "{} worker 响应序号不匹配: expected={} actual={}",
                        self.label, sequence, frame.sequence
                    ));
                }
                if !matches!(
                    frame.kind,
                    StrategyFrameKind::Response | StrategyFrameKind::Error
                ) {
                    return Err(format!(
                        "{} worker 返回非响应分帧: {:?}",
                        self.label, frame.kind
                    ));
                }
                String::from_utf8(frame.payload)
                    .map_err(|error| format!("{} worker 响应不是 UTF-8: {error}", self.label))?
            }
        };
        decode_python_strategy_response(&line, input)
            .map_err(|error| format!("{error}{}", self.diagnostics()))
    }
}

fn strategy_child_environment(
    configured: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut environment = BTreeMap::new();
    // These variables are needed for executable lookup and the Windows/Python
    // runtime, but do not carry account credentials.
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    for (key, value) in configured {
        let upper = key.to_ascii_uppercase();
        if [
            "SECRET",
            "TOKEN",
            "PASSWORD",
            "API_KEY",
            "PRIVATE_KEY",
            "CREDENTIAL",
        ]
        .iter()
        .any(|marker| upper.contains(marker))
        {
            return Err(format!(
                "策略 worker 环境变量 {key} 可能包含交易凭证，已拒绝"
            ));
        }
        environment.insert(key.clone(), value.clone());
    }
    Ok(environment)
}

impl Drop for PythonStrategyClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.shared_input.take();
        self.shared_output.take();
        if let Some((input, output)) = self.ring_paths.take() {
            let _ = std::fs::remove_file(input);
            let _ = std::fs::remove_file(output);
        }
    }
}

fn python_module_search_path() -> Result<std::ffi::OsString, String> {
    let python_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("python");
    let inherited_path = std::env::var("PYTHONPATH").unwrap_or_default();
    if inherited_path.trim().is_empty() {
        Ok(python_root.into_os_string())
    } else {
        let mut paths =
            std::env::split_paths(&std::ffi::OsString::from(inherited_path)).collect::<Vec<_>>();
        paths.insert(0, python_root);
        std::env::join_paths(paths)
            .map_err(|error| format!("构造 Python 模块搜索路径失败: {error}"))
    }
}

fn decode_python_strategy_response(
    line: &str,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let response: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| format!("Python Strategy 响应 JSON 无效: {error}"))?;
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(format!(
            "Python Strategy 拒绝请求: {}",
            response
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    let encoded = serde_json::to_string(
        response
            .get("output")
            .ok_or_else(|| "Python Strategy 响应缺少 output".to_string())?,
    )
    .map_err(|error| format!("编码 Python Strategy output 失败: {error}"))?;
    StrategyContractOutput::from_json_for(&encoded, input)
}

fn invoke_python_strategy(
    module: &str,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let mut client = PythonStrategyClient::start(module, PYTHON_STRATEGY_TIMEOUT_MS)?;
    client.request(input)
}

fn invoke_python_strategy_with_client(
    client: &mut PythonStrategyClient,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    client.request(input)
}

fn load_c_abi_strategy(strategy: &StrategyRuntimeConfig) -> Result<DynamicCAbiStrategy, String> {
    let library = strategy
        .c_abi_library
        .as_deref()
        .ok_or_else(|| "C ABI 策略缺少 c_abi_library".to_string())?;
    let expected_sha256 = strategy
        .c_abi_sha256
        .as_deref()
        .ok_or_else(|| "C ABI 策略缺少 c_abi_sha256".to_string())?;
    let mut policy = DynamicCAbiLoadPolicy::new(expected_sha256)
        .with_max_library_bytes(strategy.c_abi_max_library_bytes);
    if let (Some(public_key), Some(signature)) = (
        strategy.c_abi_ed25519_public_key.as_deref(),
        strategy.c_abi_ed25519_signature.as_deref(),
    ) {
        policy = policy.with_ed25519_signature(public_key, signature);
    }
    let config_json = serde_json::to_string(strategy)
        .map_err(|error| format!("C ABI 策略配置编码失败: {error}"))?;
    unsafe { DynamicCAbiStrategy::load_verified(library, &config_json, &policy) }
        .map_err(|error| format!("加载 C ABI 策略失败: {error}"))
}

fn native_strategy_context(
    strategy: &StrategyRuntimeConfig,
    input: &StrategyContractInput,
) -> qx_strategy::StrategyContext {
    qx_strategy::StrategyContext {
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
    }
}

fn strategy_contract_output_from_native_decision(
    decision: &qx_strategy::StrategyDecision,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let mut output = StrategyContractOutput::from_native_decision(decision)?;
    output.request_id = input.request_id.clone();
    output.strategy_id = input.strategy_id.clone();
    if output.instrument.is_empty() {
        output.instrument = input.instrument.clone();
    }
    if output.intents.is_empty() {
        output.target_qty = input
            .positions
            .get(&input.instrument)
            .copied()
            .unwrap_or_default();
    }
    output.validate_for(input)?;
    Ok(output)
}

fn invoke_c_abi_strategy(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
) -> Result<StrategyContractOutput, String> {
    context.validate()?;
    event.validate()?;
    if !*initialized {
        strategy.on_init(context)?;
        *initialized = true;
    }
    let decision = strategy.on_event(context, event)?;
    strategy_contract_output_from_native_decision(&decision, input)
}

fn builtin_strategy_config_from_runtime(
    strategy: &StrategyRuntimeConfig,
    instrument: &InstrumentId,
) -> Result<BuiltinStrategyConfig, String> {
    let name = strategy
        .builtin_strategy
        .as_deref()
        .ok_or_else(|| "Strategy 未配置 builtin_strategy".to_string())?;
    let kind = BuiltinStrategyKind::parse(name)?;
    let strategy_id = strategy
        .id
        .clone()
        .unwrap_or_else(|| format!("builtin-{}", kind.name()));
    let quantity_raw = strategy
        .builtin_quantity
        .map(i128::from)
        .or_else(|| (strategy.target_qty != 0).then_some(strategy.target_qty.abs()))
        .unwrap_or(1);
    if quantity_raw <= 0 {
        return Err("builtin_quantity 必须为正整数".into());
    }
    let mut config = BuiltinStrategyConfig {
        kind,
        strategy_id,
        strategy_version: strategy.version.clone(),
        instrument: instrument.clone(),
        quantity: Quantity::from_raw(quantity_raw),
        fast_window: 5,
        slow_window: 20,
        period: 14,
        threshold_bps: 100,
        reference_instrument: None,
        primary_policy: None,
        reference_policy: None,
    };
    if let Some(window) = strategy.builtin_fast_window {
        config.fast_window = window;
    }
    if let Some(window) = strategy.builtin_slow_window {
        config.slow_window = window;
    }
    if let Some(period) = strategy.builtin_period {
        config.period = period;
    }
    if let Some(threshold) = strategy.builtin_threshold_bps {
        config.threshold_bps = threshold;
    }
    if let Some(reference) = strategy.builtin_reference_instrument.as_deref() {
        config.reference_instrument = Some(
            InstrumentId::parse(reference)
                .ok_or_else(|| format!("builtin_reference_instrument 非法: {reference}"))?,
        );
    }
    if config.reference_instrument.is_some() {
        let primary_product = strategy.product.unwrap_or(TradingProduct::Spot);
        let primary_margin =
            strategy
                .margin_mode
                .unwrap_or(if primary_product == TradingProduct::Spot {
                    MarginMode::Cash
                } else {
                    MarginMode::Cross
                });
        let primary_position = strategy.position_mode.unwrap_or(PositionMode::OneWay);
        config.primary_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: primary_margin,
            position_mode: primary_position,
            leverage: strategy.leverage.unwrap_or(1),
            post_only: false,
        });
        let reference_margin = strategy
            .builtin_reference_margin_mode
            .unwrap_or(MarginMode::Cash);
        let reference_position = strategy
            .builtin_reference_position_mode
            .unwrap_or(PositionMode::OneWay);
        config.reference_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: reference_margin,
            position_mode: reference_position,
            leverage: strategy.builtin_reference_leverage.unwrap_or(1),
            post_only: false,
        });
    }
    config.validate()?;
    Ok(config)
}

fn invoke_builtin_strategy(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractOutput, String> {
    let input = build_strategy_contract_input(root, config, instrument, request_id, now)?;
    let bars = input.bars.as_ref().ok_or_else(|| {
        "builtin_strategy 运行时需要 bars_snapshot_path 提供 K 线历史".to_string()
    })?;
    let context = native_strategy_context(&config.strategy, &input);
    let mut strategy = BuiltinStrategy::new(builtin_strategy_config_from_runtime(
        &config.strategy,
        instrument,
    )?)?;
    strategy.on_init(&context)?;
    let reference_bars =
        if let Some(reference_text) = config.strategy.builtin_reference_instrument.as_deref() {
            let reference = InstrumentId::parse(reference_text)
                .ok_or_else(|| format!("builtin_reference_instrument 非法: {reference_text}"))?;
            let reference_path = config
                .strategy
                .builtin_reference_bars_snapshot_path
                .as_deref()
                .ok_or_else(|| "双腿套利缺少 builtin_reference_bars_snapshot_path".to_string())?;
            let mut reference_config = config.clone();
            reference_config.strategy.instrument = Some(reference.to_string());
            reference_config.strategy.bars_snapshot_path = Some(reference_path.into());
            load_strategy_contract_bars(root, &reference_config, &reference, now)?
                .map(|(bars, _, _)| (reference, bars))
        } else {
            None
        };
    let mut events = bars
        .ts
        .iter()
        .enumerate()
        .map(|(index, ts)| (*ts, false, index))
        .collect::<Vec<_>>();
    if let Some((_, reference)) = reference_bars.as_ref() {
        events.extend(
            reference
                .ts
                .iter()
                .enumerate()
                .map(|(index, ts)| (*ts, true, index)),
        );
    }
    events.sort_by_key(|(ts, is_reference, _)| (*ts, !*is_reference));
    let mut decision = None;
    for (_, is_reference, index) in events {
        let (event_instrument, event_bars) = if is_reference {
            let (reference, bars) = reference_bars.as_ref().ok_or("套利对冲腿 BarFrame 缺失")?;
            (reference.clone(), bars)
        } else {
            (instrument.clone(), bars)
        };
        let event = NativeMarketEvent::Bar {
            instrument: event_instrument,
            ts: event_bars.ts[index],
            open_raw: event_bars.open_raw[index],
            high_raw: event_bars.high_raw[index],
            low_raw: event_bars.low_raw[index],
            close_raw: event_bars.close_raw[index],
            volume_raw: event_bars.volume_raw[index],
        };
        decision = Some(strategy.on_event(&context, &event)?);
    }
    let decision = decision.ok_or_else(|| "builtin_strategy 可见 K 线为空".to_string())?;
    strategy_contract_output_from_native_decision(&decision, &input)
}

enum ContractStrategyClient {
    Process(Box<PythonStrategyClient>),
    Native(Box<DynamicCAbiStrategy>),
}

/// 把持久化 Python/C++ JSONL 策略和受信任 C ABI 策略接入与 Rust
/// 原生策略相同的 Bar 回测循环。
/// 回测引擎传入的 history 已经按 `as_of` 截止，因此跨语言策略不会看到当前
/// 正在撮合的 Bar；输出仍必须经过统一的 OrderIntent、Risk 和 OMS 转换。
struct ContractBarStrategy {
    config: RuntimeConfig,
    client: ContractStrategyClient,
    instrument: InstrumentId,
    initial_cash: Money,
    currency: String,
    data_fingerprint: String,
    native_initialized: bool,
}

impl ContractBarStrategy {
    fn from_config(
        config: RuntimeConfig,
        frame: &BarFrame,
        initial_cash: Money,
        currency: impl Into<String>,
    ) -> Result<Self, String> {
        let instrument = config
            .strategy
            .instrument
            .as_deref()
            .and_then(InstrumentId::parse)
            .ok_or_else(|| "跨语言回测策略缺少合法 strategy.instrument".to_string())?;
        if instrument != frame.instrument {
            return Err(format!(
                "跨语言回测 instrument 不一致: strategy={} frame={}",
                instrument, frame.instrument
            ));
        }
        let client = if let Some(module) = config.strategy.python_module.as_deref() {
            ContractStrategyClient::Process(Box::new(
                PythonStrategyClient::start_with_transport_config(
                    module,
                    config.strategy.python_timeout_ms,
                    config.strategy.transport,
                    SharedRingConfig {
                        capacity: config.strategy.shared_memory_capacity,
                        slot_bytes: config.strategy.shared_memory_slot_bytes,
                    },
                    config.strategy.strategy_artifact_sha256.as_deref(),
                )?,
            ))
        } else if let Some(executable) = config.strategy.external_executable.as_deref() {
            ContractStrategyClient::Process(Box::new(
                StrategyProcessClient::start_process_with_transport_config(
                    executable,
                    &config.strategy.external_args,
                    &config.strategy.external_env,
                    config.strategy.python_timeout_ms,
                    "外部 Strategy",
                    config.strategy.transport,
                    SharedRingConfig {
                        capacity: config.strategy.shared_memory_capacity,
                        slot_bytes: config.strategy.shared_memory_slot_bytes,
                    },
                )?,
            ))
        } else if let Some(library) = config.strategy.c_abi_library.as_deref() {
            let _ = library;
            let native = load_c_abi_strategy(&config.strategy)?;
            ContractStrategyClient::Native(Box::new(native))
        } else {
            return Err(
                "跨语言回测必须配置 strategy.python_module、external_executable 或 c_abi_library"
                    .into(),
            );
        };
        Ok(Self {
            config,
            client,
            instrument,
            initial_cash,
            currency: currency.into(),
            data_fingerprint: format!("{:016x}", frame.digest()),
            native_initialized: false,
        })
    }
}

impl BarStrategy for ContractBarStrategy {
    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let Some(visible) = history.last() else {
            return Ok(Vec::new());
        };
        if instrument != &self.instrument || visible.ts >= ts {
            return Err(qx_core::QxError::Invariant(
                "跨语言 Bar 策略收到不可见或未绑定的 Bar".into(),
            ));
        }
        let bars = StrategyContractBars {
            source: "backtest-bar-history-v1".into(),
            ts: history.iter().map(|bar| bar.ts).collect(),
            open_raw: history.iter().map(|bar| bar.open).collect(),
            high_raw: history.iter().map(|bar| bar.high).collect(),
            low_raw: history.iter().map(|bar| bar.low).collect(),
            close_raw: history.iter().map(|bar| bar.close).collect(),
            volume_raw: history.iter().map(|bar| bar.volume).collect(),
        };
        let strategy_id = self
            .config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| self.config.strategy.version.clone());
        let input = StrategyContractInput {
            schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: format!(
                "{strategy_id}:backtest:{visible_ts}",
                visible_ts = visible.ts
            ),
            strategy_id: strategy_id.clone(),
            strategy_version: self.config.strategy.version.clone(),
            data_fingerprint: self.data_fingerprint.clone(),
            as_of: visible.ts,
            instrument: instrument.to_string(),
            positions: BTreeMap::from([(instrument.to_string(), position)]),
            cash: BTreeMap::from([(self.currency.clone(), self.initial_cash.raw())]),
            available_margin_raw: Some(self.initial_cash.raw()),
            risk_state: "backtest-verified".into(),
            research_targets: BTreeMap::new(),
            bars: Some(bars),
        };
        let output = match &mut self.client {
            ContractStrategyClient::Process(client) => client
                .request(&input)
                .map_err(qx_core::QxError::BusinessViolation)?,
            ContractStrategyClient::Native(strategy) => {
                let context = native_strategy_context(&self.config.strategy, &input);
                let event = NativeMarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts: visible.ts,
                    open_raw: visible.open,
                    high_raw: visible.high,
                    low_raw: visible.low,
                    close_raw: visible.close,
                    volume_raw: visible.volume,
                };
                invoke_c_abi_strategy(
                    strategy,
                    &mut self.native_initialized,
                    &context,
                    &input,
                    &event,
                )
                .map_err(qx_core::QxError::BusinessViolation)?
            }
        };
        let mut orders = Vec::with_capacity(output.intents.len().max(1));
        if output.intents.is_empty() {
            let rebalance = output
                .build_rebalance_plan(&input, 10_000, 1)
                .map_err(qx_core::QxError::BusinessViolation)?;
            if rebalance.positions.is_empty() {
                return Ok(Vec::new());
            }
            if let Some(order) = build_strategy_order_with_signal(
                &self.config,
                &strategy_id,
                output.signal_id,
                visible.ts,
                position,
                output.target_qty,
                Some(&output),
            )
            .map_err(qx_core::QxError::BusinessViolation)?
            {
                orders.push(order);
            }
        } else {
            for intent in &output.intents {
                orders.push(
                    build_strategy_order_from_contract_intent(
                        &self.config,
                        &strategy_id,
                        output.signal_id,
                        intent,
                        visible.ts,
                        position,
                    )
                    .map_err(qx_core::QxError::BusinessViolation)?,
                );
            }
        }
        Ok(orders)
    }
}

fn strategy_target_qty(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    now: u64,
) -> Result<i128, String> {
    if let Some(configured) = config.strategy.research_snapshot_path.as_deref() {
        let candidate = runtime_path(root, configured);
        let path = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(configured)
        };
        let research = StrategyResearchSnapshot::from_json(
            &std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读取 Strategy research snapshot 失败 {}: {error}",
                    path.display()
                )
            })?,
        )
        .map_err(|error| format!("Strategy research snapshot JSON 无效: {error:?}"))?;
        validate_research_snapshot_binding(&config.strategy, &research)?;
        let account_id = config
            .strategy
            .account_id
            .clone()
            .ok_or_else(|| "research snapshot 策略必须配置 account_id".to_string())?;
        let venue_id = config
            .strategy
            .venue_id
            .clone()
            .ok_or_else(|| "research snapshot 策略必须配置 venue_id".to_string())?;
        let data_fingerprint = research.candidate.config.data_fingerprint.clone();
        let (positions, cash, available_margin_raw, risk_state) =
            strategy_account_context(root, config, instrument)?;
        let context = StrategyContext {
            strategy_id: config
                .strategy
                .id
                .clone()
                .unwrap_or_else(|| config.strategy.version.clone()),
            strategy_version: config.strategy.version.clone(),
            data_fingerprint,
            as_of: research.as_of,
            research,
            account_id,
            venue_id,
            positions,
            cash,
            available_margin_raw,
            risk_state,
        };
        context
            .validate(now, config.environment.eq_ignore_ascii_case("production"))
            .map_err(|error| format!("StrategyContext 校验失败: {error}"))?;
        return context.target_for(instrument).ok_or_else(|| {
            format!(
                "Strategy research snapshot 未提供 instrument={} 的目标仓位",
                instrument
            )
        });
    }
    let Some(configured) = config.strategy.target_snapshot_path.as_deref() else {
        return Ok(config.strategy.target_qty);
    };
    let candidate = runtime_path(root, configured);
    let path = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(configured)
    };
    let snapshot: StrategyTargetSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "读取 Strategy target snapshot 失败 {}: {error}",
                path.display()
            )
        })?)
        .map_err(|error| format!("Strategy target snapshot JSON 无效: {error}"))?;
    snapshot.validate_for(&config.strategy.version, now)?;
    snapshot.target_for(instrument).ok_or_else(|| {
        format!(
            "Strategy target snapshot 未提供 instrument={} 的目标仓位",
            instrument
        )
    })
}

type StrategyAccountContext = (
    BTreeMap<String, i128>,
    BTreeMap<String, i128>,
    Option<i128>,
    String,
);

fn load_strategy_contract_bars(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    as_of: u64,
) -> Result<Option<(StrategyContractBars, String, u64)>, String> {
    let Some(configured) = config.strategy.bars_snapshot_path.as_deref() else {
        return Ok(None);
    };
    let candidate = runtime_path(root, configured);
    let path = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(configured)
    };
    let payload = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "读取 Strategy bars_snapshot_path 失败 {}: {error}",
            path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "Strategy bars_snapshot_path BarFrame 无效 {}: {error:?}",
            path.display()
        )
    })?;
    if &frame.instrument != instrument {
        return Err(format!(
            "Strategy bars_snapshot_path instrument 不一致: strategy={} frame={}",
            instrument, frame.instrument
        ));
    }
    let visible: Vec<usize> = frame
        .ts
        .iter()
        .enumerate()
        .filter_map(|(index, ts)| (*ts <= as_of).then_some(index))
        .collect();
    let Some(last_index) = visible.last().copied() else {
        return Err(format!(
            "Strategy bars_snapshot_path 在 as_of={} 前没有可见 Bar",
            as_of
        ));
    };
    let bars = StrategyContractBars {
        source: frame.source.0.clone(),
        ts: visible.iter().map(|index| frame.ts[*index]).collect(),
        open_raw: visible.iter().map(|index| frame.open_raw[*index]).collect(),
        high_raw: visible.iter().map(|index| frame.high_raw[*index]).collect(),
        low_raw: visible.iter().map(|index| frame.low_raw[*index]).collect(),
        close_raw: visible
            .iter()
            .map(|index| frame.close_raw[*index])
            .collect(),
        volume_raw: visible
            .iter()
            .map(|index| frame.volume_raw[*index])
            .collect(),
    };
    bars.validate()?;
    Ok(Some((
        bars,
        format!("barframe:{:016x}", frame.digest()),
        frame.ts[last_index],
    )))
}

fn strategy_account_context(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<StrategyAccountContext, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 venue_id".to_string())?;
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Err(format!(
            "StrategyContext 当前不支持 venue_id={} 的账户事实归约",
            venue_id
        ));
    };
    if !event_log_exists(config, root, &log_name)? {
        return Ok((
            BTreeMap::new(),
            BTreeMap::new(),
            None,
            "account-snapshot-not-seen".into(),
        ));
    }
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("恢复 StrategyContext 账户 EventLog 失败: {error}"))?;
    let state = pipeline.ledger().position_for(account_id, instrument);
    let positions = [(instrument.to_string(), state.quantity.raw())]
        .into_iter()
        .collect();
    let cash = pipeline.ledger().cash_balances_for(account_id);
    let available_margin_raw = pipeline
        .ledger()
        .equity_for(account_id, pipeline.marks(), "USDT");
    Ok((
        positions,
        cash,
        available_margin_raw,
        "ledger-replayed-account-state".into(),
    ))
}

#[cfg(test)]
fn build_strategy_order(
    config: &RuntimeConfig,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
) -> Result<Option<Order>, String> {
    build_strategy_order_with_signal(
        config,
        strategy_id,
        run_id,
        now,
        current_qty,
        target_qty,
        None,
    )
}

/// 将跨语言 Strategy API v1 的单笔 intent 转为统一核心订单。
/// 该转换只负责语义翻译，订单仍必须经过 RiskExecutionContext、OMS 和 Venue。
fn build_strategy_order_from_contract_intent(
    config: &RuntimeConfig,
    strategy_id: &str,
    signal_id: u64,
    intent: &StrategyContractIntent,
    now: u64,
    current_qty: i128,
) -> Result<Order, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 account_id".to_string())?;
    let instrument = InstrumentId::parse(&intent.instrument)
        .ok_or_else(|| format!("Strategy intent instrument 非法: {}", intent.instrument))?;
    let side = match intent.side.to_ascii_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        other => return Err(format!("Strategy intent side 非法: {other}")),
    };
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    let position_mode = match intent.position_mode.as_deref() {
        None => config
            .strategy
            .position_mode
            .unwrap_or(PositionMode::OneWay),
        Some("one_way") => PositionMode::OneWay,
        Some("hedge") => PositionMode::Hedge,
        Some(other) => return Err(format!("Strategy intent position_mode 非法: {other}")),
    };
    let margin_mode = match intent.margin_mode.as_deref() {
        None => config
            .strategy
            .margin_mode
            .unwrap_or(if product == TradingProduct::Spot {
                MarginMode::Cash
            } else {
                MarginMode::Cross
            }),
        Some("cash") => MarginMode::Cash,
        Some("cross") => MarginMode::Cross,
        Some("isolated") => MarginMode::Isolated,
        Some(other) => return Err(format!("Strategy intent margin_mode 非法: {other}")),
    };
    let leverage = intent
        .leverage
        .unwrap_or(config.strategy.leverage.unwrap_or(1));
    let allow_short = if margin_mode == MarginMode::Cash {
        false
    } else {
        config
            .strategy
            .allow_short
            .unwrap_or(product.is_derivative())
    };
    let position_side = match intent
        .position_side
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("long") => PositionSide::Long,
        Some("short") => PositionSide::Short,
        Some("net") | None if position_mode == PositionMode::OneWay => PositionSide::Net,
        Some("net") | None => {
            if side == Side::Buy {
                PositionSide::Long
            } else {
                PositionSide::Short
            }
        }
        Some(other) => return Err(format!("Strategy intent position_side 非法: {other}")),
    };
    let policy = OrderPolicy {
        reduce_only: intent.reduce_only,
        position_side,
        margin_mode,
        position_mode,
        leverage,
        post_only: intent.post_only,
    };
    let mut order = Order {
        client_id: intent.intent_id,
        instrument,
        side,
        qty: qx_core::Quantity::from_raw(intent.qty_raw),
        limit: intent.limit_price_raw.map(Price::from_raw),
        status: OrderStatus::PendingSubmit,
        filled: qx_core::Quantity::ZERO,
        account_id: account_id.into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some(strategy_id.into()),
            signal_id: Some(signal_id),
            intent_id: Some(intent.intent_id),
            rule_version: Some(config.strategy.version.clone()),
        }),
        policy: None,
    };
    if product != TradingProduct::Spot
        || config.strategy.product.is_some()
        || config.strategy.margin_mode.is_some()
        || config.strategy.position_mode.is_some()
        || config.strategy.leverage.is_some()
        || intent.margin_mode.is_some()
        || intent.position_mode.is_some()
        || intent.leverage.is_some()
        || intent.reduce_only
        || intent.post_only
        || intent.position_side.is_some()
    {
        order.policy = Some(policy);
    }
    order
        .validate()
        .map_err(|error| format!("Strategy API v1 OrderIntent 转订单失败: {error}"))?;
    let mut risk = RiskGate::new();
    risk.add(Box::new(MaxQtyRule {
        max_qty: 1_000 * SCALE,
    }));
    if !allow_short {
        risk.add(Box::new(NoShortRule));
    }
    risk.check(&order, &PositionSnapshot::new(current_qty, 0))
        .map_err(|error| format!("Strategy API v1 RiskGate 拒绝 OrderIntent: {error:?}"))?;
    let _ = now;
    Ok(order)
}

fn build_strategy_order_with_signal(
    config: &RuntimeConfig,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
    contract_output: Option<&StrategyContractOutput>,
) -> Result<Option<Order>, String> {
    let instrument_text = config
        .strategy
        .instrument
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 instrument".to_string())?;
    let current = qx_portfolio::PortfolioState {
        portfolio_id: strategy_id.into(),
        timestamp: now,
        cash: 0,
        positions: BTreeMap::from([(instrument_text.to_string(), current_qty)]),
    };
    let rebalance = qx_portfolio::rebalance(
        &current,
        &[qx_portfolio::TargetPosition {
            instrument: instrument_text.to_string(),
            quantity: target_qty,
        }],
        &qx_portfolio::PortfolioConstraint {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        },
    )?;
    if rebalance.positions.is_empty() {
        return Ok(None);
    }
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 account_id".to_string())?;
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    let signal = Signal {
        strategy_id: strategy_id.into(),
        signal_id: contract_output
            .map(|output| output.signal_id)
            .unwrap_or_else(|| run_id.max(1)),
        instrument: instrument.clone(),
        target_qty,
        confidence: contract_output
            .map(|output| output.confidence)
            .unwrap_or(1_000),
        priority: contract_output.map(|output| output.priority).unwrap_or(0),
        expires_at: contract_output
            .map(|output| {
                if output.expires_at == 0 {
                    now
                } else {
                    output.expires_at
                }
            })
            .unwrap_or(now),
    };
    let targets = SignalMerger.merge(vec![signal], now);
    let target = targets
        .first()
        .ok_or_else(|| "Strategy Signal 已过期或为空".to_string())?;
    let Some(intent) = rebalance_intent(
        target,
        current_qty,
        strategy_id,
        account_id,
        run_id.max(1),
        now,
    ) else {
        return Ok(None);
    };
    let order = intent.into_order();
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    let allow_short = config
        .strategy
        .allow_short
        .unwrap_or(product.is_derivative());
    let position_mode = config
        .strategy
        .position_mode
        .unwrap_or(PositionMode::OneWay);
    let margin_mode = config
        .strategy
        .margin_mode
        .unwrap_or(if product == TradingProduct::Spot {
            MarginMode::Cash
        } else {
            MarginMode::Cross
        });
    let leverage = config.strategy.leverage.unwrap_or(1);
    let mut order = order;
    if product != TradingProduct::Spot
        || config.strategy.product.is_some()
        || config.strategy.margin_mode.is_some()
        || config.strategy.position_mode.is_some()
        || config.strategy.leverage.is_some()
    {
        order.policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: if position_mode == PositionMode::OneWay {
                PositionSide::Net
            } else if target_qty >= 0 {
                PositionSide::Long
            } else {
                PositionSide::Short
            },
            margin_mode,
            position_mode,
            leverage,
            post_only: false,
        });
    }
    order
        .validate()
        .map_err(|error| format!("Strategy OrderIntent 转订单失败: {error}"))?;
    let mut risk = RiskGate::new();
    risk.add(Box::new(MaxQtyRule {
        max_qty: 1_000 * SCALE,
    }));
    if !allow_short {
        risk.add(Box::new(NoShortRule));
    }
    risk.check(&order, &PositionSnapshot::new(current_qty, 0))
        .map_err(|error| format!("Strategy RiskGate 拒绝 OrderIntent: {error:?}"))?;
    Ok(Some(order))
}

fn strategy_submit_command(
    strategy_id: &str,
    order: &Order,
    dry_run: bool,
) -> Result<ControlCommand, String> {
    let payload = BTreeMap::from([(
        "order_json".into(),
        serde_json::to_string(order)
            .map_err(|error| format!("Strategy 订单序列化失败: {error}"))?,
    )]);
    Ok(ControlCommand {
        command_id: order.client_id,
        request_id: format!("strategy:{strategy_id}:{}", order.client_id),
        operator_id: strategy_id.into(),
        reason: "strategy signal -> portfolio -> risk -> order intent".into(),
        kind: CommandKind::SubmitOrder,
        target: order.client_id.to_string(),
        payload,
        permission: Permission::Trading,
        dry_run,
    })
}

fn persist_strategy_submit(
    control_store: &ControlStateBackend,
    command_queue: &dyn ControlCommandQueueBackend,
    command: &ControlCommand,
    now: u64,
) -> Result<String, String> {
    let (plane, result) = control_store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("Strategy SubmitOrder Accepted 持久化失败: {error}"))?;
    match result {
        Ok(_) => {
            command_queue
                .enqueue_command(command.clone(), now)
                .map_err(|error| format!("Strategy SubmitOrder 入队失败: {error:?}"))?;
            Ok("ORDER_INTENT_ACCEPTED".into())
        }
        Err(
            qx_control::ControlError::DuplicateCommand(_)
            | qx_control::ControlError::DuplicateRequest(_),
        ) => {
            let existing = plane
                .command(command.command_id)
                .ok_or_else(|| "Strategy 幂等命令缺少原命令".to_string())?;
            if existing.digest() != command.digest() {
                return Err("Strategy command_id 已被不同 OrderIntent 占用".into());
            }
            let status = plane
                .audit()
                .iter()
                .rev()
                .find(|record| record.command_id == command.command_id)
                .map(|record| record.status)
                .ok_or_else(|| "Strategy 幂等命令缺少审计记录".to_string())?;
            match status {
                qx_control::CommandStatus::Accepted => {
                    command_queue
                        .enqueue_command(command.clone(), now)
                        .map_err(|error| {
                            format!("Strategy 幂等 SubmitOrder 入队失败: {error:?}")
                        })?;
                    Ok("ORDER_INTENT_ALREADY_ACCEPTED".into())
                }
                qx_control::CommandStatus::Executed => Ok("ORDER_INTENT_ALREADY_EXECUTED".into()),
                qx_control::CommandStatus::Failed => {
                    Err("Strategy 原 OrderIntent 已执行失败".into())
                }
                qx_control::CommandStatus::Rejected => Err("Strategy 原 OrderIntent 已拒绝".into()),
            }
        }
        Err(error) => Err(format!("Strategy SubmitOrder 被控制面拒绝: {error:?}")),
    }
}

fn command_is_final(control: &ControlPlane, command_id: u64) -> bool {
    control
        .audit()
        .iter()
        .rev()
        .find(|record| record.command_id == command_id)
        .is_some_and(|record| {
            matches!(
                record.status,
                qx_control::CommandStatus::Rejected
                    | qx_control::CommandStatus::Executed
                    | qx_control::CommandStatus::Failed
            )
        })
}

fn run_runtime_api(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let service = build_configured_api_service(&config, path)?;
    let listener = TcpListener::bind(&config.api.bind)
        .map_err(|error| format!("绑定 API 地址失败 {}: {error}", config.api.bind))?;
    println!(
        "[运行时 · API] bind={} transport={:?}，按 Ctrl+C 停止",
        config.api.bind, config.api.transport
    );
    match config.api.transport {
        ApiTransport::Plaintext => {
            let worker = supervisor.spawn_worker("api", move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve(listener, runtime_timestamp_ms())
                    .map_err(|error| format!("API 服务停止: {error}"))
            })?;
            worker.join().map_err(|_| "API worker panic".to_string())?
        }
        ApiTransport::Mtls => {
            let tls = config
                .api
                .tls
                .as_ref()
                .ok_or_else(|| "mTLS API 缺少 tls 配置".to_string())?;
            let server_config = load_mtls_server_config_from_pem(
                &tls.certificate_chain,
                &tls.private_key,
                &tls.client_ca,
            )?;
            let operator_paths = config
                .api
                .operators
                .iter()
                .map(|(operator_id, operator)| {
                    (operator_id.clone(), PathBuf::from(&operator.certificate))
                })
                .collect::<BTreeMap<_, _>>();
            let identity_reloader = MtlsIdentityPemReloader::new(operator_paths)?;
            let identity_store = MtlsIdentityStore::new(identity_reloader.load()?);
            let reloader = TlsPemReloader::new(
                tls.certificate_chain.clone(),
                tls.private_key.clone(),
                tls.client_ca.clone(),
            );
            let store = TlsConfigStore::new(server_config);
            let reload_stop = Arc::new(AtomicBool::new(false));
            let reload_stop_thread = Arc::clone(&reload_stop);
            let reload_store = store.clone();
            let reload_identity_store = identity_store.clone();
            let reload_thread = thread::spawn(move || {
                while !reload_stop_thread.load(Ordering::Acquire) {
                    if let Err(error) = reloader.reload_if_changed(&reload_store) {
                        eprintln!("[运行时 · TLS] 证书轮询重载失败，保留当前配置: {error}");
                    }
                    if let Err(error) = identity_reloader.reload_if_changed(&reload_identity_store)
                    {
                        eprintln!(
                            "[运行时 · TLS] Operator 证书轮询重载失败，保留当前映射: {error}"
                        );
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            });
            let worker = match supervisor.spawn_worker("api", move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve_tls_mtls_with_stores(
                        listener,
                        &store,
                        &identity_store,
                        runtime_timestamp_ms(),
                    )
                    .map_err(|error| format!("mTLS API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    reload_stop.store(true, Ordering::Release);
                    let _ = reload_thread.join();
                    return Err(error);
                }
            };
            let result = worker
                .join()
                .map_err(|_| "mTLS API worker panic".to_string());
            reload_stop.store(true, Ordering::Release);
            let _ = reload_thread.join();
            result?
        }
    }
}

fn is_binance_testnet(worker: &WorkerConfig) -> bool {
    worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("testnet"))
        .unwrap_or(false)
        || worker
            .endpoint
            .as_deref()
            .map(|endpoint| endpoint.to_ascii_lowercase().contains("testnet"))
            .unwrap_or(false)
}

fn load_binance_worker_auth(worker: &WorkerConfig) -> Result<BinanceSpotAuth, String> {
    if let Some(files) = worker.credential_files.as_ref() {
        return BinanceSpotCredentials::from_files(&files.api_key, &files.secret)?.into_auth();
    }
    let env = worker.credential_env.as_ref().ok_or_else(|| {
        format!(
            "worker {} 缺少 credential_env 或 credential_files",
            worker.id
        )
    })?;
    BinanceSpotCredentials::from_env(&env.api_key, &env.secret)?.into_auth()
}

fn new_binance_venue(
    worker: &WorkerConfig,
    auth: BinanceSpotAuth,
) -> Result<BinanceSpotVenue, String> {
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    Ok(if is_binance_testnet(worker) {
        BinanceSpotVenue::testnet(worker.id.clone(), auth, transport)
    } else {
        BinanceSpotVenue::new(worker.id.clone(), auth, transport)
    })
}

fn configured_ws_endpoint(worker: &WorkerConfig) -> Result<(String, u16), String> {
    let endpoint = worker
        .endpoint
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 WebSocket endpoint", worker.id))?;
    let remainder = endpoint
        .strip_prefix("wss://")
        .or_else(|| endpoint.strip_prefix("ws://"))
        .ok_or_else(|| format!("worker {} endpoint 必须使用 ws:// 或 wss://", worker.id))?;
    let authority = remainder
        .split('/')
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| format!("worker {} endpoint 缺少主机", worker.id))?;
    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(host, port)| (host, port.parse::<u16>().unwrap_or_default()))
        .unwrap_or((authority, 443));
    if host.is_empty()
        || host.contains(':')
        || host.contains('\r')
        || host.contains('\n')
        || port == 0
    {
        return Err(format!("worker {} endpoint 主机非法", worker.id));
    }
    Ok((host.to_string(), port))
}

fn run_binance_public_probe(testnet: bool, instrument: &str) -> Result<(), String> {
    let instrument = InstrumentId::parse(instrument)
        .ok_or_else(|| format!("Binance public probe instrument 非法: {instrument}"))?;
    if !instrument.venue.as_str().eq_ignore_ascii_case("BINANCE") {
        return Err("Binance public probe 只接受 *.BINANCE InstrumentId".into());
    }
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    let mut market = if testnet {
        BinanceSpotMarketData::testnet(transport)
    } else {
        BinanceSpotMarketData::new(transport)
    };
    let quote = market
        .fetch_book_ticker(&instrument, runtime_timestamp_ms())
        .map_err(|error| format!("Binance public bookTicker 失败: {error:?}"))?;
    println!(
        "[Binance · PublicProbe] network={} instrument={} bid_raw={} ask_raw={} bid_qty_raw={} ask_qty_raw={} ts={}",
        if testnet { "testnet" } else { "mainnet" },
        instrument,
        quote.bid.raw(),
        quote.ask.raw(),
        quote.bid_qty.raw(),
        quote.ask_qty.raw(),
        quote.ts
    );
    Ok(())
}

fn run_binance_private_probe(runtime_path: &Path, worker_id: &str) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 Binance private probe worker: {worker_id}"))?;
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        )
    {
        return Err(format!("worker {worker_id} 不是启用的 Binance 私有 worker"));
    }
    if !worker
        .venue_id
        .as_deref()
        .is_some_and(|venue| venue.to_ascii_lowercase().contains("binance"))
    {
        return Err(format!("worker {worker_id} 不是 Binance worker"));
    }
    resolve_worker_runtime_paths(&mut worker, runtime_path);
    let auth = load_binance_worker_auth(&worker)?;
    let transport: Arc<dyn HttpTransport> =
        Arc::new(TlsHttpTransport::new(Duration::from_secs(10))?);
    let mut venue = if is_binance_testnet(&worker) {
        BinanceSpotVenue::testnet(worker.id.clone(), auth, transport)
    } else {
        BinanceSpotVenue::new(worker.id.clone(), auth, transport)
    };
    let balances = venue
        .fetch_account_balances(runtime_timestamp_ms())
        .map_err(|error| format!("Binance private account probe 失败: {error:?}"))?;
    println!(
        "[Binance · PrivateProbe] network={} worker={} balances={}",
        if is_binance_testnet(&worker) {
            "testnet"
        } else {
            "mainnet"
        },
        worker.id,
        balances.len()
    );
    Ok(())
}

fn binance_event_log_name(worker: &WorkerConfig) -> String {
    match worker.role {
        WorkerRole::MarketData => format!("{}-events", worker.id),
        WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler => format!(
            "binance-{}-{}-events",
            worker.account_id.as_deref().unwrap_or("unknown"),
            worker.venue_id.as_deref().unwrap_or("unknown")
        ),
        _ => format!("{}-events", worker.id),
    }
}

fn run_binance_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
) -> Result<(), String> {
    if worker.symbols.len() != 1 {
        return Err(format!(
            "当前 Binance market worker 要求恰好一个 symbol，实际 {}",
            worker.symbols.len()
        ));
    }
    let instrument = InstrumentId::parse(&worker.symbols[0])
        .ok_or_else(|| format!("worker {} instrument 非法", worker.id))?;
    let (host, port) = configured_ws_endpoint(&worker)?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建行情事件管线失败: {error}"))?;
    let mut stream = BinanceSpotMarketStream::connect_with_endpoint(
        instrument.clone(),
        host,
        port,
        Duration::from_secs(10),
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "market stream connected",
        Some(runtime_timestamp_ms()),
    )?;
    let mut quotes = 0_u64;
    while !context.should_stop() {
        match stream.recv_quote(runtime_timestamp_ms())? {
            Some(quote) => {
                let received_ts = runtime_timestamp_ms();
                pipeline
                    .ingest(RuntimeEventEnvelope::market_quote(
                        instrument.clone(),
                        quote.bid,
                        quote.ask,
                        quote.ts,
                        received_ts,
                        quote.source_seq,
                        format!("{}:quote:{}", worker.id, quote.source_seq),
                    ))
                    .map_err(|error| format!("行情事实归约失败: {error:?}"))?;
                quotes = quotes.saturating_add(1);
                context.heartbeat(received_ts)?;
            }
            None => break,
        }
    }
    let _ = stream.close();
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("market stream stopped quotes={quotes}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn run_binance_user_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
) -> Result<(), String> {
    let policy =
        BinanceStreamRetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(30))?;
    let initial_auth = load_binance_worker_auth(&worker)?;
    let mut venue = new_binance_venue(&worker, initial_auth)?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建用户流事件管线失败: {error}"))?;
    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("恢复用户流订单状态失败: {error:?}"))?;
    let stop_context = context.clone();
    let sleep_context = context.clone();
    let event_context = context.clone();
    let request_prefix = worker.id.clone();
    let worker_id = worker.id.clone();
    let (host, port) = configured_ws_endpoint(&worker)?;
    let mut source_seq = 0_u64;
    let mut should_stop = move || stop_context.should_stop();
    let mut sleep = move |delay| {
        thread::sleep(delay);
        let _ = sleep_context.heartbeat(runtime_timestamp_ms());
    };
    let mut on_event = move |payload: &str| -> Result<(), String> {
        pipeline
            .refresh()
            .map_err(|error| format!("刷新共享订单 EventLog 失败: {error:?}"))?;
        venue
            .refresh_orders(pipeline.orders())
            .map_err(|error| format!("刷新用户流订单状态失败: {error:?}"))?;
        let events = match venue.ingest_user_event(payload) {
            Ok(events) => events,
            Err(error) => {
                // listenKey 过期后必须关闭当前会话并重新订阅；重连后的
                // reconciler 会先用 REST 快照确认状态，再允许新的提交。
                if payload.contains("listenKeyExpired") {
                    venue.reconnect();
                }
                return Err(format!("Binance 用户事件处理失败: {error:?}"));
            }
        };
        let received_ts = runtime_timestamp_ms();
        let event_count = ingest_venue_events(
            &mut pipeline,
            events,
            &worker_id,
            received_ts,
            &mut source_seq,
        )?;
        event_context.heartbeat(received_ts)?;
        if event_count != 0 {
            event_context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!("user events={event_count}"),
                Some(runtime_timestamp_ms()),
            )?;
        }
        Ok(())
    };
    let run_config = BinanceUserStreamRunConfig::with_port(
        host,
        port,
        request_prefix,
        Duration::from_secs(10),
        policy,
    )?;
    let report = qx_adapter::run_binance_user_stream_with_config_loader(
        || load_binance_worker_auth(&worker),
        run_config,
        &mut should_stop,
        &mut sleep,
        &mut on_event,
    )?;
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("user stream stopped events={}", report.events),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn reconcile_issue_json(issue: &AdapterReconcileIssue) -> serde_json::Value {
    match issue {
        AdapterReconcileIssue::MissingLocally { client_order_id } => serde_json::json!({
            "kind": "missing_locally",
            "client_order_id": client_order_id,
        }),
        AdapterReconcileIssue::MissingAtVenue { client_order_id } => serde_json::json!({
            "kind": "missing_at_venue",
            "client_order_id": client_order_id,
        }),
        AdapterReconcileIssue::StatusMismatch {
            client_order_id,
            local,
            venue,
        } => serde_json::json!({
            "kind": "status_mismatch",
            "client_order_id": client_order_id,
            "local": local,
            "venue": venue,
        }),
        AdapterReconcileIssue::FilledMismatch {
            client_order_id,
            local,
            venue,
        } => serde_json::json!({
            "kind": "filled_mismatch",
            "client_order_id": client_order_id,
            "local_raw": local.raw(),
            "venue_raw": venue.raw(),
        }),
    }
}

struct ReconcileReportInput<'a> {
    pipeline_root: &'a Path,
    worker_id: &'a str,
    account_id: &'a str,
    venue_id: &'a str,
    observed_ts: u64,
    issues: &'a [AdapterReconcileIssue],
    balances_count: usize,
    balance_discrepancies: &'a [RuntimeBalanceDiscrepancy],
    position_snapshots_count: usize,
    funding_rate_snapshots_count: usize,
    cashflow_count: usize,
}

fn persist_reconcile_report(input: ReconcileReportInput<'_>) -> Result<(), String> {
    let report = ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: input.worker_id.into(),
        account_id: input.account_id.into(),
        venue_id: input.venue_id.into(),
        observed_ts: input.observed_ts,
        order_issues: input.issues.iter().map(reconcile_issue_json).collect(),
        balances_count: input.balances_count,
        balance_discrepancies: input
            .balance_discrepancies
            .iter()
            .map(|value| serde_json::to_value(value).expect("balance discrepancy is serializable"))
            .collect(),
        position_snapshots_count: input.position_snapshots_count,
        funding_rate_snapshots_count: input.funding_rate_snapshots_count,
        cashflow_count: input.cashflow_count,
    };
    report.validate()?;
    JsonStateStore::new(input.pipeline_root)
        .save_json_at(format!("reconcile/{}.json", input.worker_id), &report)
        .map(|_| ())
        .map_err(|error| format!("保存对账报告失败: {error:?}"))
}

fn run_binance_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    once: bool,
) -> Result<(), String> {
    let reconcile_symbols = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .filter(|instrument| instrument.venue.as_str().eq_ignore_ascii_case("BINANCE"))
                .map(|instrument| instrument.symbol)
                .ok_or_else(|| format!("worker {} 对账 symbol 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut pipeline = pipeline_storage
        .open(binance_event_log_name(&worker), "USDT")
        .map_err(|error| format!("创建对账事件管线失败: {error}"))?;
    let mut source_seq = 0_u64;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "reconciler starting",
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        // 每一轮重新读取凭据并创建连接器，使 Secret Manager 对投影文件的
        // 原子替换在下一轮对账生效；本轮请求仍使用同一认证上下文。
        let auth = load_binance_worker_auth(&worker)?;
        let mut venue = new_binance_venue(&worker, auth)?;
        venue
            .set_reconcile_symbols(reconcile_symbols.iter())
            .map_err(|error| format!("设置 Binance 对账 symbol 失败: {error:?}"))?;
        // fetch_and_reconcile 只允许在 Snapshotting 阶段执行；每一轮先显式
        // 进入对账态，避免把 Live 状态下的快照查询误当成已完成对账。
        pipeline
            .refresh()
            .map_err(|error| format!("刷新共享对账 EventLog 失败: {error:?}"))?;
        venue
            .restore_orders(pipeline.orders())
            .map_err(|error| format!("刷新对账订单状态失败: {error:?}"))?;
        venue.reconnect();
        let issues = venue
            .fetch_and_reconcile()
            .map_err(|error| format!("Binance 订单对账失败: {error:?}"))?;
        let balances = venue
            .fetch_account_balances(runtime_timestamp_ms())
            .map_err(|error| format!("Binance 账户余额同步失败: {error:?}"))?;
        let received_ts = runtime_timestamp_ms();
        let account_id = worker
            .account_id
            .clone()
            .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
        let venue_id = worker
            .venue_id
            .clone()
            .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
        let balance_facts = balances
            .iter()
            .map(|balance| AccountBalance {
                asset: balance.asset.clone(),
                free: balance.free,
                locked: balance.locked,
                borrowed: Money::ZERO,
            })
            .collect::<Vec<_>>();
        let balance_discrepancies = pipeline
            .settlement_balance_discrepancies(&account_id, &venue_id, &balance_facts)
            .map_err(|error| format!("账户余额对账失败: {error:?}"))?;
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root,
            worker_id: &worker.id,
            account_id: &account_id,
            venue_id: &venue_id,
            observed_ts: received_ts,
            issues: &issues,
            balances_count: balances.len(),
            balance_discrepancies: &balance_discrepancies,
            position_snapshots_count: 0,
            funding_rate_snapshots_count: 0,
            cashflow_count: 0,
        })?;
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: account_id.clone(),
                    venue_id: venue_id.clone(),
                    balances: balance_facts,
                },
                received_ts,
                received_ts,
                source_seq,
                format!("{}:balances:{}", worker.id, source_seq),
            ))
            .map_err(|error| format!("账户余额事实归约失败: {error:?}"))?;
        for issue in &issues {
            let client_order_id = match issue {
                AdapterReconcileIssue::MissingLocally { client_order_id }
                | AdapterReconcileIssue::MissingAtVenue { client_order_id }
                | AdapterReconcileIssue::StatusMismatch {
                    client_order_id, ..
                }
                | AdapterReconcileIssue::FilledMismatch {
                    client_order_id, ..
                } => *client_order_id,
            };
            source_seq = source_seq.saturating_add(1);
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::ReconcileRequired { client_order_id },
                    received_ts,
                    received_ts,
                    source_seq,
                    format!("{}:reconcile:{}", worker.id, client_order_id),
                ))
                .map_err(|error| format!("对账事实归约失败: {error:?}"))?;
        }
        context.heartbeat(received_ts)?;
        let balance_detail = balance_discrepancies
            .iter()
            .map(|discrepancy| {
                format!(
                    "{}:{}->{}",
                    discrepancy.asset, discrepancy.ledger_raw, discrepancy.venue_raw
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let status = if issues.is_empty() && balance_discrepancies.is_empty() {
            qx_runtime::ServiceStatus::Ready
        } else {
            qx_runtime::ServiceStatus::Degraded
        };
        context.mark(
            status,
            format!(
                "reconcile_issues={} balances={} balance_discrepancies={}{}",
                issues.len(),
                balances.len(),
                balance_discrepancies.len(),
                if balance_detail.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", balance_detail)
                }
            ),
            Some(runtime_timestamp_ms()),
        )?;
        if once {
            break;
        }
        for _ in 0..300 {
            if context.should_stop() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

/// 执行一条经过 ControlPlane 审计的 Binance SubmitOrder 命令。
///
/// 该入口是显式副作用命令：配置、命令权限、订单账户/交易所和 EventLog
/// 都先校验；订单事实先写入 `OrderSubmitted`，Venue 返回的 Accepted/Fill
/// 再按同一管线归约。网络返回未知时，EventLog 会保留 Submitted 状态，
/// 后续只能通过对账恢复，绝不自动重试补单。
fn validate_binance_submit_worker(worker: &WorkerConfig) -> Result<&str, String> {
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler
        )
    {
        return Err(format!(
            "worker {} 必须是已启用的 Binance 用户流、执行或对账角色",
            worker.id
        ));
    }
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("binance"))
        != Some(true)
    {
        return Err(format!("worker {} 不是 Binance Venue", worker.id));
    }
    worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))
}

fn binance_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case("BINANCE")
        })
        .unwrap_or(false)
}

fn resolve_worker_runtime_paths(worker: &mut WorkerConfig, runtime_path: &Path) {
    if let Some(files) = worker.credential_files.as_mut() {
        files.api_key = resolve_runtime_relative_path(runtime_path, &files.api_key)
            .to_string_lossy()
            .into_owned();
        files.secret = resolve_runtime_relative_path(runtime_path, &files.secret)
            .to_string_lossy()
            .into_owned();
    }
}

/// 从执行 worker 的冻结 market spec 和同一 EventLog 构造账户级风险快照。
///
/// 没有配置 `instrument_spec_path` 时保持兼容的基础执行路径；一旦配置，
/// 订单必须同时通过规格、杠杆、名义额、可用保证金和当前持仓预检，之后才
/// 能进入 OMS/Venue。这样 Paper、Binance、CCXT 共用同一个风险边界。
fn worker_risk_context(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&Path>,
) -> Result<Option<(RiskContext, PositionSnapshot)>, String> {
    let Some(spec) = load_worker_instrument_spec(worker, order, runtime_config_path)? else {
        return Ok(None);
    };
    let reference_price = order
        .limit
        .or_else(|| pipeline.marks().get(&order.instrument).copied());
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let settlement = worker
        .settlement_currency
        .as_deref()
        .unwrap_or(&spec.settlement_currency);
    let observed_available = worker.venue_id.as_deref().and_then(|venue_id| {
        pipeline
            .snapshot()
            .account_balances
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|balances| balances.iter().find(|balance| balance.asset == settlement))
            .map(|balance| balance.free.raw().saturating_sub(balance.borrowed.raw()))
    });
    let available_margin_raw = if let Some(available) = observed_available {
        available
    } else if worker
        .venue_id
        .as_deref()
        .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
    {
        pipeline
            .ledger()
            .equity_for(account_id, pipeline.marks(), settlement)
            .unwrap_or_else(|| pipeline.ledger().cash_for(account_id, settlement))
    } else {
        return Err(format!(
            "worker {} 尚未收到 {} 账户余额快照，拒绝执行账户级风控订单",
            worker.id, settlement
        ));
    };
    let aggregate = pipeline
        .ledger()
        .position_for(account_id, &order.instrument);
    let long_qty = pipeline
        .ledger()
        .position_for_side(account_id, &order.instrument, qx_core::PositionSide::Long)
        .quantity
        .raw();
    let short_qty = pipeline
        .ledger()
        .position_for_side(account_id, &order.instrument, qx_core::PositionSide::Short)
        .quantity
        .raw();
    let one_way_qty = aggregate
        .quantity
        .raw()
        .checked_sub(long_qty)
        .and_then(|value| value.checked_sub(short_qty))
        .ok_or_else(|| "拆分 one-way/hedge 持仓数量溢出".to_string())?;
    let gross_notional = reference_price
        .map(|price| {
            let one_way = spec.notional(one_way_qty.saturating_abs(), price.raw())?;
            let long = spec.notional(long_qty.saturating_abs(), price.raw())?;
            let short = spec.notional(short_qty.saturating_abs(), price.raw())?;
            one_way
                .checked_add(long)
                .and_then(|value| value.checked_add(short))
                .ok_or_else(|| qx_core::QxError::Invariant("当前 gross notional 溢出".into()))
        })
        .transpose()
        .map_err(|error| format!("计算当前持仓名义额失败: {error:?}"))?
        .unwrap_or(0);
    let position =
        PositionSnapshot::new_with_multiplier(one_way_qty, gross_notional, spec.contract_size)
            .with_hedge_legs(long_qty, short_qty);
    Ok(Some((
        RiskContext {
            available_margin_raw: Some(available_margin_raw),
            reference_price,
            instrument_spec: Some(spec),
            max_order_notional_raw: worker.max_order_notional_raw,
            max_position_notional_raw: worker.max_position_notional_raw,
        },
        position,
    )))
}

fn load_worker_instrument_spec(
    worker: &WorkerConfig,
    order: &Order,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return Ok(None);
    };
    let resolved_spec_path = runtime_config_path
        .map(|path| resolve_runtime_relative_path(path, spec_path))
        .unwrap_or_else(|| PathBuf::from(spec_path));
    let payload = std::fs::read_to_string(&resolved_spec_path).map_err(|error| {
        format!(
            "读取 worker market spec 失败 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    let spec = match serde_json::from_str::<TradingInstrumentSpec>(&payload) {
        Ok(spec) => spec,
        Err(_) => {
            let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
                format!(
                    "worker market spec JSON 无效 {}: {error}",
                    resolved_spec_path.display()
                )
            })?;
            ccxt_market_to_spec(&order.instrument, &market)?
        }
    };
    spec.validate()
        .map_err(|error| format!("worker market spec 非法: {error:?}"))?;
    if spec.instrument != order.instrument {
        return Err("worker market spec instrument 与订单不一致".into());
    }
    Ok(Some(spec))
}

fn execute_submit_order_with_worker_risk<V: Venue>(
    command: &ControlCommand,
    worker: &WorkerConfig,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
) -> Result<String, String> {
    let order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if let Some((risk, position)) =
        worker_risk_context(worker, &order, pipeline, runtime_config_path)?
    {
        let context = RiskExecutionContext {
            risk: &risk,
            position: &position,
        };
        execute_submit_order_with_risk(
            command, venue, pipeline, &worker.id, now, source_seq, &context,
        )
    } else {
        execute_submit_order(command, venue, pipeline, &worker.id, now, source_seq)
    }
}

fn execute_binance_submit_effect(
    command: &ControlCommand,
    worker: &WorkerConfig,
    pipeline: &mut LiveEventPipeline,
    venue: &mut BinanceSpotVenue,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
) -> Result<String, String> {
    let requested_order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let expected_account = validate_binance_submit_worker(worker)?;
    if requested_order.account_id != expected_account
        || !requested_order
            .instrument
            .venue
            .as_str()
            .eq_ignore_ascii_case("BINANCE")
    {
        return Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into());
    }

    venue
        .restore_orders(pipeline.orders())
        .map_err(|error| format!("装载待提交订单失败: {error:?}"))?;
    execute_submit_order_with_worker_risk(
        command,
        worker,
        venue,
        pipeline,
        now,
        source_seq,
        runtime_config_path,
    )
}

/// 为 Paper 账户写入一次可恢复、可幂等的初始资金事实。
/// 资金只通过 AccountCashflow 进入 Ledger，不直接修改内存余额。
fn seed_paper_initial_cash(
    pipeline: &mut LiveEventPipeline,
    worker: &WorkerConfig,
    now: u64,
) -> Result<(), String> {
    let Some(amount_raw) = worker.paper_initial_cash_raw else {
        return Ok(());
    };
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("Paper worker {} 缺少 account_id", worker.id))?;
    let currency = worker
        .settlement_currency
        .as_deref()
        .unwrap_or("USDT")
        .to_ascii_uppercase();
    let external_id = format!(
        "paper-initial-cash:{}:{}:{}",
        worker.id, account_id, currency
    );
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: account_id.into(),
                    venue_id: "paper".into(),
                    currency,
                    kind: CashflowKind::Transfer,
                    amount: Money::from_raw(amount_raw),
                    external_id: external_id.clone(),
                },
            },
            now,
            now,
            0,
            external_id,
        ))
        .map_err(|error| format!("写入 Paper 初始资金失败: {error:?}"))?;
    Ok(())
}

/// 使用同一控制面/队列/EventLog 语义跑一笔完全本地的 Paper 下单闭环。
/// 该入口用于验收执行编排，不连接网络，也不把 Paper 结果当作真实 Venue 结果。
fn run_paper_submit_order(path: &Path, command_path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 Paper SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("Paper SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("Paper SubmitOrder 订单载荷非法: {error:?}"))?;
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let postgres_dsn = configured_postgres_dsn(&config)?;
    let store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let now = runtime_timestamp_ms();
    let paper_worker = config
        .workers
        .iter()
        .find(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        })
        .cloned();
    if let Some(worker) = paper_worker.as_ref() {
        let mut pipeline = open_runtime_pipeline(&config, &root, "paper-events", "USDT")
            .map_err(|error| format!("打开 Paper 初始资金 EventLog 失败: {error}"))?;
        seed_paper_initial_cash(&mut pipeline, worker, now)?;
    }
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 Paper SubmitOrder Accepted 失败: {error}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("Paper SubmitOrder 未通过控制面校验: {error:?}"))?;
    queue
        .enqueue_command(command.clone(), now)
        .map_err(|error| format!("写入 Paper SubmitOrder 队列失败: {error:?}"))?;
    let lease = queue
        .claim_command(command.command_id, "paper-execution", now, 30)
        .map_err(|error| format!("领取 Paper SubmitOrder 租约失败: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".into())
    } else if let Some(worker) = paper_worker
        .as_ref()
        .filter(|worker| worker.instrument_spec_path.is_some())
    {
        let pipeline = open_runtime_pipeline(&config, &root, "paper-events", "USDT")
            .map_err(|error| format!("打开 Paper 风控 EventLog 失败: {error}"))?;
        let order = order_from_submit_command(&command)
            .map_err(|error| format!("Paper 订单载荷非法: {error:?}"))?;
        let (risk, position) = worker_risk_context(worker, &order, &pipeline, Some(path))?
            .ok_or_else(|| "Paper worker 风控配置缺少 RiskContext".to_string())?;
        execute_paper_submit_effect_with_storage_backend_and_pool(
            &command,
            &root,
            "paper-events",
            now,
            config.storage.event_log_segment_events,
            postgres_dsn.as_deref(),
            config.storage.postgres_pool_size,
            Some(risk),
            Some(position),
        )
    } else {
        execute_paper_submit_effect_with_storage_backend_and_pool(
            &command,
            &root,
            "paper-events",
            now,
            config.storage.event_log_segment_events,
            postgres_dsn.as_deref(),
            config.storage.postgres_pool_size,
            None,
            None,
        )
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 Paper SubmitOrder 终态失败: {error}"))?;
    let record = record_result.map_err(|error| format!("Paper 执行控制命令失败: {error:?}"))?;
    queue
        .ack_command_at(
            command.command_id,
            "paper-execution",
            lease.fencing_token,
            now,
        )
        .map_err(|error| format!("确认 Paper SubmitOrder 队列失败: {error:?}"))?;
    println!(
        "[Paper · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
}

fn paper_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && worker
                    .venue_id
                    .as_deref()
                    .map(|venue| venue.eq_ignore_ascii_case("paper"))
                    .unwrap_or(false)
                // Paper 是虚拟执行域，订单 instrument 可以来自 Binance、OKX、
                // Bybit 或自定义市场；不能把虚拟账户误绑死在某一个真实 Venue。
                && (worker.symbols.is_empty()
                    || worker.symbols.iter().any(|symbol| {
                        InstrumentId::parse(symbol)
                            .as_ref()
                            == Some(&order.instrument)
                    }))
        })
        .unwrap_or(false)
}

fn run_paper_execution_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled
        || worker.role != WorkerRole::Execution
        || worker
            .venue_id
            .as_deref()
            .map(|venue| !venue.eq_ignore_ascii_case("paper"))
            .unwrap_or(true)
    {
        return Err(format!(
            "worker {worker_id} 不是启用的 Paper Execution worker"
        ));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let log_name = format!(
        "paper-{}-{}-events",
        worker.account_id.as_deref().unwrap_or("unknown"),
        worker.venue_id.as_deref().unwrap_or("paper")
    );
    let segment_events = config.storage.event_log_segment_events;
    let runtime_config = config.clone();
    let postgres_dsn = configured_postgres_dsn(&config)?;
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let runtime_config_path = path.to_path_buf();
    let handle = supervisor.spawn_worker(&registered_id, move |context| {
        let mut pipeline = open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT")
            .map_err(|error| format!("打开 Paper 初始资金 EventLog 失败: {error}"))?;
        seed_paper_initial_cash(&mut pipeline, &worker, runtime_timestamp_ms())?;
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            "paper execution queue polling",
            Some(runtime_timestamp_ms()),
        )?;
        while !context.should_stop() {
            let now = runtime_timestamp_ms();
            let control = store.load()?;
            for command in control.pending().filter(|command| {
                matches!(command.kind, CommandKind::SubmitOrder)
                    && paper_submit_matches_worker(command, &worker)
            }) {
                queue
                    .enqueue_command(command.clone(), now)
                    .map_err(|error| format!("补入 Paper SubmitOrder 队列失败: {error:?}"))?;
            }
            let mut processed = 0_usize;
            for queued in queue
                .available_commands(now)
                .map_err(|error| format!("读取 Paper SubmitOrder 队列失败: {error:?}"))?
            {
                let command = queued.command.clone();
                if !paper_submit_matches_worker(&command, &worker) {
                    continue;
                }
                let lease = match queue.claim_command(command.command_id, context.id(), now, 30) {
                    Ok(lease) => lease,
                    Err(StorageError::LeaseHeld { .. }) => continue,
                    Err(error) => {
                        return Err(format!("领取 Paper SubmitOrder 租约失败: {error:?}"))
                    }
                };
                if command_is_final(&control, command.command_id) {
                    queue
                        .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                        .map_err(|error| format!("清理已终态 Paper SubmitOrder 失败: {error:?}"))?;
                    continue;
                }
                let action = if command.dry_run {
                    Ok("DRY_RUN_VALIDATED".into())
                } else {
                    let risk_snapshot = if worker.instrument_spec_path.is_some() {
                        let pipeline =
                            open_runtime_pipeline(&runtime_config, &root, &log_name, "USDT")
                                .map_err(|error| {
                                    format!("打开 Paper 风控 EventLog 失败: {error}")
                                })?;
                        let order = order_from_submit_command(&command)
                            .map_err(|error| format!("Paper 订单载荷非法: {error:?}"))?;
                        worker_risk_context(&worker, &order, &pipeline, Some(&runtime_config_path))?
                    } else {
                        None
                    };
                    match risk_snapshot {
                        Some((risk, position)) => {
                            execute_paper_submit_effect_with_storage_backend_and_pool(
                                &command,
                                &root,
                                &log_name,
                                now,
                                segment_events,
                                postgres_dsn.as_deref(),
                                runtime_config.storage.postgres_pool_size,
                                Some(risk),
                                Some(position),
                            )
                        }
                        None => execute_paper_submit_effect_with_storage_backend_and_pool(
                            &command,
                            &root,
                            &log_name,
                            now,
                            segment_events,
                            postgres_dsn.as_deref(),
                            runtime_config.storage.postgres_pool_size,
                            None,
                            None,
                        ),
                    }
                };
                let (_, record_result) = store
                    .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                    .map_err(|error| format!("回写 Paper SubmitOrder 终态失败: {error}"))?;
                let record = record_result
                    .map_err(|error| format!("Paper SubmitOrder 执行失败: {error:?}"))?;
                queue
                    .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                    .map_err(|error| format!("确认 Paper SubmitOrder 失败: {error:?}"))?;
                processed += 1;
                context.mark(
                    if record.status == qx_control::CommandStatus::Executed {
                        qx_runtime::ServiceStatus::Ready
                    } else {
                        qx_runtime::ServiceStatus::Degraded
                    },
                    format!(
                        "command_id={} status={:?}",
                        command.command_id, record.status
                    ),
                    Some(now),
                )?;
            }
            context.heartbeat(now)?;
            if once {
                println!(
                    "[Paper · Execution] worker={} processed={}",
                    context.id(),
                    processed
                );
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })?;
    handle
        .join()
        .map_err(|_| format!("Paper worker {worker_id} panic"))?
}

/// 按真实 worker 边界顺序执行一轮本地 Paper 主链路。
///
/// 该入口不是新的业务分支，而是把 Scheduler、Strategy 和 Execution 的
/// `--once` 验收顺序固定下来，便于 CI、部署检查和故障恢复测试复用。
fn run_paper_pipeline_once(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let scheduler_id = config
        .workers
        .iter()
        .find(|worker| worker.enabled && worker.role == WorkerRole::Scheduler)
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Scheduler worker".to_string())?;
    let strategy_id = config
        .workers
        .iter()
        .find(|worker| worker.enabled && worker.role == WorkerRole::Strategy)
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Strategy worker".to_string())?;
    let execution_id = config
        .workers
        .iter()
        .find(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
        })
        .map(|worker| worker.id.clone())
        .ok_or_else(|| "Paper 主链路缺少启用的 Paper Execution worker".to_string())?;

    run_scheduler_worker(path, &scheduler_id, true)?;
    run_strategy_worker(path, &strategy_id, true)?;
    run_paper_execution_worker(path, &execution_id, true)?;

    let root = Path::new(&config.storage.data_dir);
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == execution_id)
        .ok_or_else(|| "Paper Execution worker 配置在验收期间消失".to_string())?;
    let log_name = format!(
        "paper-{}-{}-events",
        worker.account_id.as_deref().unwrap_or("unknown"),
        worker.venue_id.as_deref().unwrap_or("paper")
    );
    let pipeline = open_runtime_pipeline(&config, root, log_name, "USDT")
        .map_err(|error| format!("打开 Paper 主链路 EventLog 失败: {error}"))?;
    if pipeline.orders().is_empty() || pipeline.ledger().entries().is_empty() {
        return Err("Paper 主链路验收未产生订单或 Ledger 事实".into());
    }
    let queue = ControlCommandQueue::new(root.join("control-queue"));
    if !queue
        .pending()
        .map_err(|error| format!("读取 Paper 主链路命令队列失败: {error:?}"))?
        .is_empty()
    {
        return Err("Paper 主链路验收结束后仍有未确认命令".into());
    }
    let control = load_control_state(root)?;
    let executed = control.audit().iter().any(|record| {
        record.status == qx_control::CommandStatus::Executed
            && record.command_id == pipeline.orders()[0].client_id
    });
    if !executed {
        return Err("Paper 主链路验收缺少 SubmitOrder Executed 审计记录".into());
    }
    println!(
        "[Paper · E2E] scheduler={} strategy={} execution={} orders={} ledger_entries={} ✓",
        scheduler_id,
        strategy_id,
        execution_id,
        pipeline.orders().len(),
        pipeline.ledger().entries().len()
    );
    Ok(())
}

fn run_binance_submit_order(
    path: &Path,
    worker_id: &str,
    command_path: &Path,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, path);
    validate_binance_submit_worker(&worker)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let now = runtime_timestamp_ms();
    let store = configured_control_store(&config)?;
    let (_, accepted_result) = store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("持久化 SubmitOrder Accepted 失败: {error:?}"))?;
    let accepted = accepted_result
        .map_err(|error| format!("SubmitOrder 未通过控制面权限/幂等校验: {error:?}"))?;

    let action = if command.dry_run {
        Ok("DRY_RUN_VALIDATED".into())
    } else {
        let mut pipeline = pipeline_storage
            .open(binance_event_log_name(&worker), "USDT")
            .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
        let auth = load_binance_worker_auth(&worker);
        match auth.and_then(|auth| new_binance_venue(&worker, auth)) {
            Ok(mut venue) => {
                let mut source_seq = 0_u64;
                execute_binance_submit_effect(
                    &command,
                    &worker,
                    &mut pipeline,
                    &mut venue,
                    now,
                    &mut source_seq,
                    Some(path),
                )
            }
            Err(error) => Err(error),
        }
    };
    let (_, record_result) = store
        .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
        .map_err(|error| format!("持久化 SubmitOrder 终态失败: {error:?}"))?;
    let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
    println!(
        "[执行 · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        accepted.status, record.status, command.command_id, record.result_code
    );
    if record.status == qx_control::CommandStatus::Failed {
        return Err(record.result_code);
    }
    Ok(())
}

/// 运行持久化控制命令队列的 Binance 执行 worker。
fn run_binance_execution_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    control_store: ControlStateBackend,
    queue: Arc<dyn ControlCommandQueueBackend>,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    let owner = worker.id.clone();
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        "execution queue polling",
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        let control = control_store.load()?;
        for command in control.pending().filter(|command| {
            matches!(&command.kind, CommandKind::SubmitOrder)
                && binance_submit_matches_worker(command, &worker)
        }) {
            queue
                .enqueue_command(command.clone(), now)
                .map_err(|error| format!("补入 SubmitOrder 队列失败: {error:?}"))?;
        }
        for queued in queue
            .available_commands(now)
            .map_err(|error| format!("读取 SubmitOrder 队列失败: {error:?}"))?
        {
            if context.should_stop() {
                break;
            }
            let command = queued.command.clone();
            if !binance_submit_matches_worker(&command, &worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, now, 30) {
                Ok(lease) => lease,
                Err(qx_storage::StorageError::LeaseHeld { .. }) => continue,
                Err(error) => return Err(format!("领取 SubmitOrder 租约失败: {error:?}")),
            };
            if command_is_final(&control, command.command_id) {
                queue
                    .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                    .map_err(|error| format!("清理已终态 SubmitOrder 失败: {error:?}"))?;
                continue;
            }
            let action = if command.dry_run {
                Ok("DRY_RUN_VALIDATED".into())
            } else {
                let mut pipeline = pipeline_storage
                    .open(binance_event_log_name(&worker), "USDT")
                    .map_err(|error| format!("打开执行 EventLog 失败: {error}"))?;
                let auth = load_binance_worker_auth(&worker)?;
                let mut venue = new_binance_venue(&worker, auth)?;
                venue
                    .restore_orders(pipeline.orders())
                    .map_err(|error| format!("恢复执行订单状态失败: {error:?}"))?;
                let mut source_seq = 0_u64;
                execute_binance_submit_effect(
                    &command,
                    &worker,
                    &mut pipeline,
                    &mut venue,
                    now,
                    &mut source_seq,
                    Some(&runtime_config_path),
                )
            };
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 SubmitOrder 终态失败: {error:?}"))?;
            let record = record_result.map_err(|error| format!("执行控制命令失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                .map_err(|error| format!("确认 SubmitOrder 队列失败: {error:?}"))?;
            context.mark(
                if record.status == qx_control::CommandStatus::Executed {
                    qx_runtime::ServiceStatus::Ready
                } else {
                    qx_runtime::ServiceStatus::Degraded
                },
                format!(
                    "command_id={} status={:?}",
                    command.command_id, record.status
                ),
                Some(now),
            )?;
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn ccxt_submit_matches_worker(command: &ControlCommand, worker: &WorkerConfig) -> bool {
    let Some(expected_account) = worker.account_id.as_deref() else {
        return false;
    };
    let Some(expected_venue) = worker.venue_id.as_deref() else {
        return false;
    };
    order_from_submit_command(command)
        .map(|order| {
            order.account_id == expected_account
                && order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(expected_venue)
        })
        .unwrap_or(false)
}

/// 通过公共 Python CCXT Worker 执行已审计 SubmitOrder。
///
/// `worker.endpoint` 约定为 CCXT JSON 配置文件路径，Python 解释器使用
/// `QX_PYTHON` 环境变量或默认 `python`。Rust 侧只负责命令队列、EventLog、
/// OMS 和 Ledger，交易所协议全部由公共 CCXT 完成。
#[allow(clippy::too_many_arguments)]
fn run_ccxt_execution_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    control_store: ControlStateBackend,
    queue: Arc<dyn ControlCommandQueueBackend>,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::Execution {
        return Err(format!("worker {} 不是 CCXT Execution worker", worker.id));
    }
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let owner = worker.id.clone();
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt execution queue polling venue={}",
            worker.venue_id.as_deref().unwrap_or("")
        ),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        let now = runtime_timestamp_ms();
        let control = control_store.load()?;
        for command in control.pending().filter(|command| {
            matches!(&command.kind, CommandKind::SubmitOrder)
                && ccxt_submit_matches_worker(command, &worker)
        }) {
            queue
                .enqueue_command(command.clone(), now)
                .map_err(|error| format!("补入 CCXT SubmitOrder 队列失败: {error:?}"))?;
        }
        for queued in queue
            .available_commands(now)
            .map_err(|error| format!("读取 CCXT SubmitOrder 队列失败: {error:?}"))?
        {
            if context.should_stop() {
                break;
            }
            let command = queued.command.clone();
            if !ccxt_submit_matches_worker(&command, &worker) {
                continue;
            }
            let lease = match queue.claim_command(command.command_id, &owner, now, 30) {
                Ok(lease) => lease,
                Err(StorageError::LeaseHeld { .. }) => continue,
                Err(error) => return Err(format!("领取 CCXT SubmitOrder 租约失败: {error:?}")),
            };
            if command_is_final(&control, command.command_id) {
                queue
                    .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                    .map_err(|error| format!("清理已终态 CCXT SubmitOrder 失败: {error:?}"))?;
                continue;
            }
            let action = if command.dry_run {
                Ok("DRY_RUN_VALIDATED".into())
            } else {
                let mut pipeline = pipeline_storage
                    .open(
                        ccxt_event_log_name(&worker),
                        worker.settlement_currency.as_deref().unwrap_or("USDT"),
                    )
                    .map_err(|error| format!("打开 CCXT EventLog 失败: {error}"))?;
                let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                    .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
                let mut venue = CcxtProcessVenue::new(
                    worker.venue_id.clone().unwrap_or_else(|| "ccxt".into()),
                    Box::new(client),
                );
                let mut source_seq = 0_u64;
                let requested_order = order_from_submit_command(&command)
                    .map_err(|error| format!("CCXT 订单载荷非法: {error:?}"))?;
                if requested_order.limit.is_none() && worker.instrument_spec_path.is_some() {
                    let ticker = venue
                        .stream_call(serde_json::json!({
                            "op": "fetch_ticker",
                            "instrument": requested_order.instrument.to_string(),
                        }))
                        .map_err(|error| format!("CCXT 风控参考价查询失败: {error:?}"))?;
                    let ticker = ticker
                        .get("ticker")
                        .ok_or_else(|| "CCXT ticker 响应缺少 ticker".to_string())?;
                    let bid = Price::from_raw(raw_json_i128(ticker, "bid_raw")?);
                    let ask = Price::from_raw(raw_json_i128(ticker, "ask_raw")?);
                    let event_ts = ticker
                        .get("timestamp_ms")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(now);
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::MarketQuote {
                                instrument: requested_order.instrument.clone(),
                                bid,
                                ask,
                            },
                            event_ts,
                            now,
                            source_seq,
                            format!("{}:risk-ticker:{}", worker.id, requested_order.client_id),
                        ))
                        .map_err(|error| format!("写入 CCXT 风控参考行情失败: {error:?}"))?;
                }
                execute_submit_order_with_worker_risk(
                    &command,
                    &worker,
                    &mut venue,
                    &mut pipeline,
                    now,
                    &mut source_seq,
                    Some(&runtime_config_path),
                )
            };
            let (_, record_result) = control_store
                .transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))
                .map_err(|error| format!("回写 CCXT SubmitOrder 终态失败: {error}"))?;
            let record =
                record_result.map_err(|error| format!("CCXT SubmitOrder 执行失败: {error:?}"))?;
            queue
                .ack_command_at(command.command_id, &owner, lease.fencing_token, now)
                .map_err(|error| format!("确认 CCXT SubmitOrder 失败: {error:?}"))?;
            context.mark(
                if record.status == qx_control::CommandStatus::Executed {
                    qx_runtime::ServiceStatus::Ready
                } else {
                    qx_runtime::ServiceStatus::Degraded
                },
                format!(
                    "command_id={} status={:?}",
                    command.command_id, record.status
                ),
                Some(now),
            )?;
        }
        context.heartbeat(now)?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// 使用公共 CCXT Pro `watch_orders` 接收账户订单变化，再通过同一 CCXT
/// REST `fetch_order` 补齐成交/费用并归约到 Runtime/EventLog。Pro 流只负责
/// 唤醒和提供 remote id，不直接修改本地订单或 Ledger。
fn run_ccxt_user_stream_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::UserStream {
        return Err(format!("worker {} 不是 CCXT UserStream worker", worker.id));
    }
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let log_name = ccxt_event_log_name(&worker);
    let mut pipeline = pipeline_storage
        .open(
            &log_name,
            worker.settlement_currency.as_deref().unwrap_or("USDT"),
        )
        .map_err(|error| format!("打开 CCXT UserStream EventLog 失败: {error}"))?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt pro user stream venue={}",
            worker.venue_id.as_deref().unwrap_or("")
        ),
        Some(runtime_timestamp_ms()),
    )?;
    let client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT Pro Worker 失败: {error}"))?;
    let mut venue = CcxtProcessVenue::new(
        worker.venue_id.clone().unwrap_or_else(|| "ccxt".into()),
        Box::new(client),
    );
    let mut source_seq = pipeline
        .log()
        .events()
        .last()
        .map(|event| event.source_seq)
        .unwrap_or(0);
    loop {
        if context.should_stop() {
            break;
        }
        pipeline
            .refresh()
            .map_err(|error| format!("刷新 CCXT UserStream EventLog 失败: {error:?}"))?;
        let received_ts = runtime_timestamp_ms();
        let result = match venue.stream_call(serde_json::json!({
            "op": "watch_orders",
            "instrument": null,
            "received_ts": received_ts,
        })) {
            Ok(result) => result,
            Err(error) => {
                let detail = format!("{error:?}");
                if detail.contains("[unsupported]") || detail.contains("[authentication]") {
                    return Err(format!("CCXT Pro 用户流不可用: {detail}"));
                }
                context.mark(
                    qx_runtime::ServiceStatus::Degraded,
                    format!("ccxt pro user stream reconnecting: {detail}"),
                    Some(received_ts),
                )?;
                thread::sleep(Duration::from_millis(500));
                let replacement = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
                    .map_err(|spawn_error| {
                        format!("重连公共 CCXT Pro Worker 失败: {spawn_error}")
                    })?;
                venue.replace_rpc(Box::new(replacement));
                continue;
            }
        };
        let event = result
            .get("event")
            .ok_or_else(|| "CCXT Pro watch_orders 响应缺少 event".to_string())?;
        if event.get("stream").and_then(serde_json::Value::as_str) != Some("orders") {
            return Err("CCXT Pro 用户流返回了非 orders 事件".into());
        }
        let events = event
            .get("events")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "CCXT Pro orders 事件缺少 events".to_string())?;
        let mut matched = 0_usize;
        let mut reduced = 0_usize;
        let mut reconcile_errors = 0_usize;
        for update in events {
            let remote_id = update
                .get("order_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "CCXT Pro order 事件缺少 order_id".to_string())?;
            let client_order_id = update
                .get("client_order_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<u64>().ok());
            let local = pipeline.orders().into_iter().find(|order| {
                client_order_id == Some(order.client_id)
                    || pipeline
                        .venue_order_id(order.client_id)
                        .as_deref()
                        .is_some_and(|value| value == remote_id)
            });
            let Some(order) = local else {
                // 未知 remote order 不允许自动注册或下单补偿；交给 Reconcile worker
                // 按交易所 open orders/账单做完整快照核对。
                continue;
            };
            matched = matched.saturating_add(1);
            venue
                .restore_order(order.clone(), remote_id)
                .map_err(|error| format!("恢复 CCXT Pro remote order 失败: {error:?}"))?;
            let event_ts = update
                .get("timestamp_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(received_ts);
            let venue_events = match venue.sync_order(order.client_id, event_ts) {
                Ok(events) => events,
                Err(error) => {
                    reconcile_errors = reconcile_errors.saturating_add(1);
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::ReconcileRequired {
                                client_order_id: order.client_id,
                            },
                            event_ts,
                            received_ts,
                            source_seq,
                            format!("{}:sync-error:{}", worker.id, order.client_id),
                        ))
                        .map_err(|ingest_error| {
                            format!(
                                "归约 CCXT Pro order 失败 {error:?}，且无法写入 ReconcileRequired: {ingest_error:?}"
                            )
                        })?;
                    continue;
                }
            };
            let reduced_events = if let Some(spec) =
                load_worker_instrument_spec(&worker, &order, Some(&runtime_config_path))?
            {
                ingest_venue_events_with_spec(
                    &mut pipeline,
                    venue_events,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                    &spec,
                )?
            } else {
                ingest_venue_events(
                    &mut pipeline,
                    venue_events,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                )?
            };
            reduced = reduced.saturating_add(reduced_events);
        }
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!(
                "ccxt pro orders matched={matched} reduced={reduced} reconcile_errors={reconcile_errors}"
            ),
            Some(received_ts),
        )?;
        context.heartbeat(received_ts)?;
        if once {
            break;
        }
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        "ccxt pro user stream stopped",
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

#[derive(Clone)]
struct LiveStrategyBarSpec {
    instrument: InstrumentId,
    timeframe: String,
    timeframe_ms: u64,
    history_limit: usize,
    closed_only: bool,
    snapshot_path: PathBuf,
}

fn timeframe_to_ms(timeframe: &str) -> Result<u64, String> {
    let value = timeframe.trim().to_ascii_lowercase();
    if value.len() < 2 {
        return Err(format!("CCXT timeframe 非法: {timeframe}"));
    }
    let (number, unit) = value.split_at(value.len() - 1);
    let number = number
        .parse::<u64>()
        .map_err(|_| format!("CCXT timeframe 数值非法: {timeframe}"))?;
    if number == 0 {
        return Err(format!("CCXT timeframe 必须为正数: {timeframe}"));
    }
    let unit_ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return Err(format!("不支持的 CCXT timeframe 单位: {timeframe}")),
    };
    number
        .checked_mul(unit_ms)
        .ok_or_else(|| format!("CCXT timeframe 溢出: {timeframe}"))
}

fn live_strategy_bar_specs(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    worker: &WorkerConfig,
) -> Result<Vec<LiveStrategyBarSpec>, String> {
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    let mut specs = Vec::new();
    for strategy in strategies {
        if !strategy.live_enabled {
            continue;
        }
        let instrument_text = strategy
            .instrument
            .as_deref()
            .ok_or_else(|| "实时策略必须配置 instrument".to_string())?;
        let instrument = InstrumentId::parse(instrument_text)
            .ok_or_else(|| format!("实时策略 instrument 非法: {instrument_text}"))?;
        let timeframe_ms = timeframe_to_ms(&strategy.live_timeframe)?;
        let primary_matches = worker.symbols.iter().any(|symbol| {
            InstrumentId::parse(symbol).is_some_and(|configured| configured == instrument)
        });
        if primary_matches {
            let configured_path = strategy
                .bars_snapshot_path
                .as_deref()
                .ok_or_else(|| "实时策略必须配置 bars_snapshot_path".to_string())?;
            specs.push(LiveStrategyBarSpec {
                instrument,
                timeframe: strategy.live_timeframe.clone(),
                timeframe_ms,
                history_limit: strategy.live_history_limit,
                closed_only: strategy.live_closed_only,
                snapshot_path: resolve_runtime_relative_path(runtime_config_path, configured_path),
            });
        }
        if let (Some(reference_text), Some(reference_path)) = (
            strategy.builtin_reference_instrument.as_deref(),
            strategy.builtin_reference_bars_snapshot_path.as_deref(),
        ) {
            let reference = InstrumentId::parse(reference_text)
                .ok_or_else(|| format!("实时策略对冲腿 instrument 非法: {reference_text}"))?;
            if worker.symbols.iter().any(|symbol| {
                InstrumentId::parse(symbol).is_some_and(|configured| configured == reference)
            }) {
                specs.push(LiveStrategyBarSpec {
                    instrument: reference,
                    timeframe: strategy.live_timeframe.clone(),
                    timeframe_ms,
                    history_limit: strategy.live_history_limit,
                    closed_only: strategy.live_closed_only,
                    snapshot_path: resolve_runtime_relative_path(
                        runtime_config_path,
                        reference_path,
                    ),
                });
            }
        }
    }
    specs.sort_by(|left, right| {
        left.instrument
            .to_string()
            .cmp(&right.instrument.to_string())
            .then(left.snapshot_path.cmp(&right.snapshot_path))
    });
    specs.dedup_by(|left, right| {
        left.instrument == right.instrument && left.snapshot_path == right.snapshot_path
    });
    Ok(specs)
}

fn closed_live_frame(
    frame: BarFrame,
    spec: &LiveStrategyBarSpec,
    now: u64,
) -> Result<Option<BarFrame>, String> {
    if frame.instrument != spec.instrument {
        return Err(format!(
            "实时 OHLCV instrument 不一致: expected={} actual={}",
            spec.instrument, frame.instrument
        ));
    }
    let visible = frame
        .ts
        .iter()
        .enumerate()
        .filter(|(_, ts)| {
            !spec.closed_only
                || ts
                    .checked_add(spec.timeframe_ms)
                    .is_some_and(|close_ts| close_ts <= now)
        })
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return Ok(None);
    }
    let start = visible.len().saturating_sub(spec.history_limit);
    let visible = &visible[start..];
    let result = BarFrame {
        instrument: frame.instrument,
        source: frame.source,
        ts: visible.iter().map(|(_, ts)| **ts).collect(),
        open_raw: visible
            .iter()
            .map(|(index, _)| frame.open_raw[*index])
            .collect(),
        high_raw: visible
            .iter()
            .map(|(index, _)| frame.high_raw[*index])
            .collect(),
        low_raw: visible
            .iter()
            .map(|(index, _)| frame.low_raw[*index])
            .collect(),
        close_raw: visible
            .iter()
            .map(|(index, _)| frame.close_raw[*index])
            .collect(),
        volume_raw: visible
            .iter()
            .map(|(index, _)| frame.volume_raw[*index])
            .collect(),
    };
    result
        .validate()
        .map_err(|error| format!("实时 BarFrame 校验失败: {error:?}"))?;
    Ok(Some(result))
}

fn write_live_bar_snapshot(path: &Path, frame: &BarFrame) -> Result<u64, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("实时 BarFrame 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建实时 BarFrame 目录失败 {}: {error}", parent.display()))?;
    let payload = frame.to_json();
    let temporary = path.with_extension(format!("barframe.tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入实时 BarFrame 临时文件失败: {error}"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        // Windows 无法直接覆盖已有文件；只在替换失败时删除旧快照，
        // Strategy worker 遇到短暂缺文件会等待下一轮，不会读取半份 JSON。
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path).map_err(|replacement| {
            format!(
                "提交实时 BarFrame 失败 {}: initial={error}; replacement={replacement}",
                path.display()
            )
        })?;
    }
    Ok(frame.digest())
}

/// 使用公共 CCXT REST ticker 和 OHLCV 轮询接入统一行情 EventLog，并将闭合
/// BarFrame 原子写入策略快照。Strategy worker 通过快照摘要触发幂等 JobQueue，
/// 因而不依赖 Scheduler 的固定周期，也不会因为未闭合 K 线反复下单。
fn run_ccxt_market_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::MarketData {
        return Err(format!("worker {} 不是 CCXT MarketData worker", worker.id));
    }
    if worker.symbols.is_empty() {
        return Err(format!("worker {} 至少需要一个 CCXT instrument", worker.id));
    }
    let runtime_config = read_runtime_config(&runtime_config_path)?;
    let live_specs = live_strategy_bar_specs(&runtime_config, &runtime_config_path, &worker)?;
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
        .map_err(|error| format!("启动公共 CCXT MarketData Worker 失败: {error}"))?;
    let mut pipeline = pipeline_storage
        .open(
            ccxt_market_event_log_name(&worker),
            worker.settlement_currency.as_deref().unwrap_or("USDT"),
        )
        .map_err(|error| format!("创建 CCXT 行情事件管线失败: {error}"))?;
    let instruments = worker
        .symbols
        .iter()
        .map(|symbol| {
            InstrumentId::parse(symbol)
                .ok_or_else(|| format!("worker {} instrument 非法: {symbol}", worker.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!(
            "ccxt ticker/ohlcv polling instruments={} live_snapshots={}",
            instruments.len(),
            live_specs.len()
        ),
        Some(runtime_timestamp_ms()),
    )?;
    let mut source_seq = 0_u64;
    let mut quotes = 0_u64;
    let mut bars_written = 0_u64;
    while !context.should_stop() {
        let cycle_now = runtime_timestamp_ms();
        for instrument in &instruments {
            let result = match client.call(serde_json::json!({
                "op": "fetch_ticker",
                "instrument": instrument.to_string(),
            })) {
                Ok(result) => result,
                Err(error) => {
                    context.mark(
                        qx_runtime::ServiceStatus::Degraded,
                        format!("CCXT ticker 暂时失败 {}: {error}; reconnecting", instrument),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let ticker = result
                .get("ticker")
                .ok_or_else(|| format!("CCXT ticker 响应缺少 ticker: {instrument}"))?;
            let bid = raw_json_i128(ticker, "bid_raw")?;
            let ask = raw_json_i128(ticker, "ask_raw")?;
            if bid <= 0 || ask <= 0 || bid > ask {
                return Err(format!("CCXT ticker 买卖价非法: {instrument}"));
            }
            let ts = ticker
                .get("timestamp_ms")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("CCXT ticker 缺少 timestamp_ms: {instrument}"))?;
            let received_ts = runtime_timestamp_ms();
            source_seq = source_seq.saturating_add(1);
            pipeline
                .ingest(RuntimeEventEnvelope::market_quote(
                    instrument.clone(),
                    qx_core::Price::from_raw(bid),
                    qx_core::Price::from_raw(ask),
                    ts,
                    received_ts,
                    source_seq,
                    format!("{}:ticker:{}", worker.id, source_seq),
                ))
                .map_err(|error| format!("CCXT 行情事实归约失败: {error:?}"))?;
            quotes = quotes.saturating_add(1);
            context.heartbeat(received_ts)?;
        }
        for spec in &live_specs {
            let start_ms = cycle_now.saturating_sub(
                spec.timeframe_ms
                    .saturating_mul(spec.history_limit.saturating_add(2) as u64),
            );
            let result = match client.call(serde_json::json!({
                "op": "fetch_ohlcv",
                "instrument": spec.instrument.to_string(),
                "timeframe": spec.timeframe,
                "start_ms": start_ms,
                "end_ms": cycle_now,
                "limit": spec.history_limit.saturating_add(2),
            })) {
                Ok(result) => result,
                Err(error) => {
                    context.mark(
                        qx_runtime::ServiceStatus::Degraded,
                        format!(
                            "CCXT OHLCV 暂时失败 {}: {error}; reconnecting",
                            spec.instrument
                        ),
                        Some(cycle_now),
                    )?;
                    client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None).map_err(
                        |spawn_error| format!("重启公共 CCXT Worker 失败: {spawn_error}"),
                    )?;
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let frame_value = result
                .get("frame")
                .ok_or_else(|| format!("CCXT OHLCV 响应缺少 frame: {}", spec.instrument))?;
            let frame = BarFrame::from_json(
                &serde_json::to_string(frame_value)
                    .map_err(|error| format!("编码 CCXT OHLCV frame 失败: {error}"))?,
            )
            .map_err(|error| format!("CCXT OHLCV BarFrame 非法 {}: {error:?}", spec.instrument))?;
            if let Some(closed) = closed_live_frame(frame, spec, cycle_now)? {
                write_live_bar_snapshot(&spec.snapshot_path, &closed)?;
                bars_written = bars_written.saturating_add(1);
            }
        }
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!(
                "ccxt live market healthy quotes={} bar_snapshots={}",
                quotes, bars_written
            ),
            Some(cycle_now),
        )?;
        if once {
            break;
        }
        thread::sleep(Duration::from_millis(1_000));
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        format!("ccxt market worker stopped quotes={quotes} bar_snapshots={bars_written}"),
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn raw_json_i128(value: &serde_json::Value, key: &str) -> Result<i128, String> {
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_i64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_u64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_str) {
        return value
            .parse::<i128>()
            .map_err(|_| format!("CCXT 字段不是定点整数: {key}"));
    }
    Err(format!("CCXT 字段不是定点整数: {key}"))
}

fn ccxt_money(value: &serde_json::Value, field: &str) -> Result<Money, String> {
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    Money::from_dec(&text).ok_or_else(|| format!("CCXT 余额字段非法: {field}={text}"))
}

fn ccxt_balance_facts(value: &serde_json::Value) -> Result<Vec<AccountBalance>, String> {
    let balance = value
        .get("balance")
        .ok_or_else(|| "CCXT balance 响应缺少 balance".to_string())?;
    let free = balance
        .get("free")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "CCXT balance 缺少 free map".to_string())?;
    let used = balance
        .get("used")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let debt = balance
        .get("debt")
        .or_else(|| balance.get("borrowed"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut assets = BTreeSet::new();
    assets.extend(free.keys().cloned());
    assets.extend(used.keys().cloned());
    assets.extend(debt.keys().cloned());
    let mut facts = Vec::new();
    for asset in assets {
        let free_amount = free
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("free.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        let locked_amount = used
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("used.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        let borrowed_amount = debt
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("debt.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        if free_amount.is_zero() && locked_amount.is_zero() && borrowed_amount.is_zero() {
            continue;
        }
        facts.push(AccountBalance {
            asset,
            free: free_amount,
            locked: locked_amount,
            borrowed: borrowed_amount,
        });
    }
    Ok(facts)
}

fn ccxt_position_facts(
    value: &serde_json::Value,
    venue_id: &str,
) -> Result<Vec<VenuePositionSnapshot>, String> {
    let positions = value
        .get("positions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT positions 响应缺少 positions 数组".to_string())?;
    let mut facts = Vec::with_capacity(positions.len());
    for position in positions {
        let symbol = position
            .get("symbol")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "CCXT position 缺少 symbol".to_string())?;
        let instrument = InstrumentId::parse(&format!("{symbol}.{}", venue_id.to_uppercase()))
            .ok_or_else(|| format!("CCXT position symbol 不是合法 InstrumentId: {symbol}"))?;
        let contracts = raw_json_i128(position, "contracts_raw")?;
        if contracts < 0 {
            return Err(format!("CCXT position contracts_raw 不能为负: {symbol}"));
        }
        if contracts == 0 {
            continue;
        }
        let side = position
            .get("side")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("long")
            .to_ascii_lowercase();
        let quantity = if side == "short" {
            contracts
                .checked_neg()
                .ok_or_else(|| "CCXT short position 数量溢出".to_string())?
        } else {
            contracts
        };
        let optional_price = |key: &str| -> Result<Option<Price>, String> {
            let Some(value) = position.get(key) else {
                return Ok(None);
            };
            if value.is_null() {
                return Ok(None);
            }
            let raw = raw_json_i128(position, key)?;
            if raw <= 0 {
                return Err(format!("CCXT position {key} 必须为正: {symbol}"));
            }
            Ok(Some(Price::from_raw(raw)))
        };
        let optional_money = |key: &str| -> Result<Money, String> {
            let Some(value) = position.get(key) else {
                return Ok(Money::ZERO);
            };
            if value.is_null() {
                return Ok(Money::ZERO);
            }
            let raw = raw_json_i128(position, key)?;
            Ok(Money::from_raw(raw))
        };
        let leverage = position
            .get("leverage")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        facts.push(VenuePositionSnapshot {
            instrument,
            quantity: Quantity::from_raw(quantity),
            average_price: optional_price("entry_price_raw")?,
            mark_price: optional_price("mark_price_raw")?,
            liquidation_price: optional_price("liquidation_price_raw")?,
            unrealized_pnl: optional_money("unrealized_pnl_raw")?,
            initial_margin: optional_money("initial_margin_raw")?,
            maintenance_margin: optional_money("maintenance_margin_raw")?,
            leverage,
            margin_mode: position
                .get("margin_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            position_side: Some(side),
        });
    }
    Ok(facts)
}

fn ccxt_funding_fact(
    value: &serde_json::Value,
    venue_id: &str,
) -> Result<(FundingRateSnapshot, u64), String> {
    let funding = value
        .get("funding")
        .ok_or_else(|| "CCXT funding 响应缺少 funding".to_string())?;
    let symbol = funding
        .get("symbol")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT funding 缺少 symbol".to_string())?;
    let instrument = InstrumentId::parse(&format!("{symbol}.{}", venue_id.to_uppercase()))
        .ok_or_else(|| format!("CCXT funding symbol 不是合法 InstrumentId: {symbol}"))?;
    let rate = raw_json_i128(funding, "funding_rate_bps")?;
    let rate = i64::try_from(rate).map_err(|_| "CCXT funding rate 超出 i64".to_string())?;
    let timestamp = funding
        .get("timestamp_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok((
        FundingRateSnapshot {
            instrument,
            funding_rate_bps: rate,
            next_funding_timestamp_ms: funding
                .get("next_funding_timestamp_ms")
                .and_then(serde_json::Value::as_u64),
        },
        timestamp,
    ))
}

fn ccxt_cashflow_facts(
    value: &serde_json::Value,
    account_id: &str,
    venue_id: &str,
) -> Result<Vec<(AccountCashflow, u64)>, String> {
    let rows = value
        .get("cashflows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT cashflow 响应缺少 cashflows 数组".to_string())?;
    let mut facts = Vec::with_capacity(rows.len());
    for row in rows {
        let external_id = row
            .get("external_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "CCXT cashflow 缺少 external_id".to_string())?;
        let currency = row
            .get("currency")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("CCXT cashflow {external_id} 缺少 currency"))?
            .to_ascii_uppercase();
        let kind = match row
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
        {
            "funding" => CashflowKind::Funding,
            "interest" => CashflowKind::Interest,
            "settlement" => CashflowKind::Settlement,
            "transfer" => CashflowKind::Transfer,
            other => return Err(format!("CCXT cashflow {external_id} kind 非法: {other}")),
        };
        let amount_raw = raw_json_i128(row, "amount_raw")?;
        let timestamp = row
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        facts.push((
            AccountCashflow {
                account_id: account_id.into(),
                venue_id: venue_id.into(),
                currency,
                kind,
                amount: Money::from_raw(amount_raw),
                external_id: external_id.into(),
            },
            timestamp,
        ));
    }
    Ok(facts)
}

fn cashflow_kind_key(kind: CashflowKind) -> &'static str {
    match kind {
        CashflowKind::Funding => "funding",
        CashflowKind::Interest => "interest",
        CashflowKind::Settlement => "settlement",
        CashflowKind::Transfer => "transfer",
    }
}

fn ingest_ccxt_cashflows(
    pipeline: &mut LiveEventPipeline,
    value: &serde_json::Value,
    account_id: &str,
    venue_id: &str,
    worker_id: &str,
    received_ts: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    let facts = ccxt_cashflow_facts(value, account_id, venue_id)?;
    let mut ingested = 0_usize;
    for (cashflow, event_ts) in facts {
        *source_seq = (*source_seq).saturating_add(1);
        let event_ts = if event_ts == 0 { received_ts } else { event_ts };
        let correlation_id = format!(
            "{}:cashflow:{}:{}:{}:{}",
            worker_id,
            cashflow_kind_key(cashflow.kind),
            cashflow.currency,
            cashflow.external_id,
            account_id
        );
        let receipt = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow { cashflow },
                event_ts,
                received_ts,
                *source_seq,
                correlation_id,
            ))
            .map_err(|error| format!("CCXT 现金流水事实归约失败: {error:?}"))?;
        if !receipt.deduplicated {
            ingested = ingested.saturating_add(1);
        }
    }
    Ok(ingested)
}

fn ccxt_error_is_optional_derivatives_capability(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("unsupported")
        || error.contains("not supported")
        || error.contains("非合约市场")
        || error.contains("fetchpositions")
}

fn run_ccxt_reconcile_worker(
    context: qx_runtime::WorkerContext,
    worker: WorkerConfig,
    pipeline_root: &Path,
    pipeline_storage: PipelineStorage,
    ccxt_config_path: String,
    runtime_config_path: PathBuf,
    once: bool,
) -> Result<(), String> {
    if worker.role != WorkerRole::Reconciler {
        return Err(format!("worker {} 不是 CCXT Reconciler worker", worker.id));
    }
    let account_id = worker
        .account_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let venue_id = worker
        .venue_id
        .clone()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let settlement_currency = worker
        .settlement_currency
        .clone()
        .unwrap_or_else(|| "USDT".into());
    let mut pipeline = pipeline_storage
        .open(ccxt_event_log_name(&worker), settlement_currency)
        .map_err(|error| format!("打开 CCXT 对账 EventLog 失败: {error}"))?;
    context.mark(
        qx_runtime::ServiceStatus::Ready,
        format!("ccxt reconcile polling venue={venue_id}"),
        Some(runtime_timestamp_ms()),
    )?;
    while !context.should_stop() {
        pipeline
            .refresh()
            .map_err(|error| format!("刷新 CCXT 对账 EventLog 失败: {error:?}"))?;
        let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path, None)
            .map_err(|error| format!("启动公共 CCXT Reconcile Worker 失败: {error}"))?;
        let balance_result = client
            .call(serde_json::json!({"op": "fetch_balance"}))
            .map_err(|error| format!("CCXT balance 对账失败: {error}"))?;
        let balances = ccxt_balance_facts(&balance_result)?;
        let received_ts = runtime_timestamp_ms();
        let mut source_seq = pipeline
            .log()
            .events()
            .last()
            .map(|event| event.source_seq)
            .unwrap_or(0);
        source_seq = source_seq.saturating_add(1);
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountBalanceSnapshot {
                    account_id: account_id.clone(),
                    venue_id: venue_id.clone(),
                    balances: balances.clone(),
                },
                received_ts,
                received_ts,
                source_seq,
                format!("{}:balances:{}", worker.id, received_ts),
            ))
            .map_err(|error| format!("CCXT 余额事实归约失败: {error:?}"))?;

        let cashflow_since_ms = pipeline
            .log()
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::AccountCashflow { .. } => Some(event.ts),
                _ => None,
            })
            .max();

        // 费率只是预估输入，真实资金费/利息/交割必须读取带外部 ID 的账单
        // 增量事实后才能进入 Ledger。优先使用统一 fetch_ledger；若交易所
        // 只暴露合约 funding history，则明确走该能力，不把缺失能力伪装成空账单。
        let mut cashflow_count = 0_usize;
        match client.call(serde_json::json!({
            "op": "fetch_ledger",
            "code": null,
            "since_ms": cashflow_since_ms,
        })) {
            Ok(cashflow_result) => {
                cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                    &mut pipeline,
                    &cashflow_result,
                    &account_id,
                    &venue_id,
                    &worker.id,
                    received_ts,
                    &mut source_seq,
                )?);
            }
            Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
            Err(error) => return Err(format!("CCXT 资金流水对账失败: {error}")),
        }
        // 部分交易所的 fetch_ledger 不包含 funding 账单；合约市场再读取
        // 专用 funding history。与 ledger 同一 external_id 会由流水事实幂等去重。
        for instrument in &worker.symbols {
            match client.call(serde_json::json!({
                "op": "fetch_funding_history",
                "instrument": instrument,
                "since_ms": cashflow_since_ms,
            })) {
                Ok(history_result) => {
                    cashflow_count = cashflow_count.saturating_add(ingest_ccxt_cashflows(
                        &mut pipeline,
                        &history_result,
                        &account_id,
                        &venue_id,
                        &worker.id,
                        received_ts,
                        &mut source_seq,
                    )?);
                }
                Err(history_error)
                    if ccxt_error_is_optional_derivatives_capability(&history_error) => {}
                Err(history_error) => {
                    return Err(format!(
                        "CCXT 资金费账单对账失败 {instrument}: {history_error}"
                    ));
                }
            }
        }

        // 合约账户的持仓、保证金和未实现盈亏必须进入同一可恢复事实流。
        // 现货交易所通常不支持 fetch_positions；这类能力缺失只跳过该类快照，
        // 不能把“未支持”误判成空仓。
        let position_request = if worker.symbols.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(worker.symbols)
        };
        match client.call(serde_json::json!({
            "op": "fetch_positions",
            "instruments": position_request,
        })) {
            Ok(position_result) => {
                let positions = ccxt_position_facts(&position_result, &venue_id)?;
                source_seq = source_seq.saturating_add(1);
                pipeline
                    .ingest(RuntimeEventEnvelope::venue(
                        RuntimeExternalEvent::AccountPositionSnapshot {
                            account_id: account_id.clone(),
                            venue_id: venue_id.clone(),
                            positions,
                        },
                        received_ts,
                        received_ts,
                        source_seq,
                        format!("{}:positions:{}", worker.id, received_ts),
                    ))
                    .map_err(|error| format!("CCXT 持仓事实归约失败: {error:?}"))?;
            }
            Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
            Err(error) => return Err(format!("CCXT 持仓对账失败: {error}")),
        }

        // 资金费率是风险输入，按 instrument 保存最新观察值；它不直接修改
        // Ledger，实际扣款已经由上面的 Cashflow 账单事实负责。
        for instrument in &worker.symbols {
            match client.call(serde_json::json!({
                "op": "fetch_funding_rate",
                "instrument": instrument,
            })) {
                Ok(funding_result) => {
                    let (snapshot, event_ts) = ccxt_funding_fact(&funding_result, &venue_id)?;
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::FundingRateSnapshot { snapshot },
                            event_ts,
                            received_ts,
                            source_seq,
                            format!("{}:funding:{}:{}", worker.id, instrument, received_ts),
                        ))
                        .map_err(|error| format!("CCXT 资金费率事实归约失败: {error:?}"))?;
                }
                Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {}
                Err(error) => {
                    return Err(format!("CCXT 资金费率对账失败 {instrument}: {error}"));
                }
            }
        }

        let mut venue = CcxtProcessVenue::new(venue_id.clone(), Box::new(client));
        let orders = pipeline.orders();
        let mut updates = 0_usize;
        for order in orders
            .into_iter()
            .filter(|order| !order.status.is_terminal())
        {
            let Some(remote_id) = pipeline.venue_order_id(order.client_id) else {
                source_seq = source_seq.saturating_add(1);
                pipeline
                    .ingest(RuntimeEventEnvelope::venue(
                        RuntimeExternalEvent::ReconcileRequired {
                            client_order_id: order.client_id,
                        },
                        received_ts,
                        received_ts,
                        source_seq,
                        format!("{}:missing-remote:{}", worker.id, order.client_id),
                    ))
                    .map_err(|error| format!("CCXT 缺失远端订单对账失败: {error:?}"))?;
                continue;
            };
            venue
                .restore_order(order.clone(), remote_id)
                .map_err(|error| format!("恢复 CCXT 本地订单失败: {error:?}"))?;
            match venue.sync_order(order.client_id, received_ts) {
                Ok(events) => {
                    updates += if let Some(spec) =
                        load_worker_instrument_spec(&worker, &order, Some(&runtime_config_path))?
                    {
                        ingest_venue_events_with_spec(
                            &mut pipeline,
                            events,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                            &spec,
                        )?
                    } else {
                        ingest_venue_events(
                            &mut pipeline,
                            events,
                            &worker.id,
                            received_ts,
                            &mut source_seq,
                        )?
                    };
                }
                Err(error) => {
                    source_seq = source_seq.saturating_add(1);
                    pipeline
                        .ingest(RuntimeEventEnvelope::venue(
                            RuntimeExternalEvent::ReconcileRequired {
                                client_order_id: order.client_id,
                            },
                            received_ts,
                            received_ts,
                            source_seq,
                            format!("{}:order-error:{}", worker.id, order.client_id),
                        ))
                        .map_err(|ingest_error| {
                            format!(
                                "CCXT 订单对账失败 {error:?}，且无法写入 ReconcileRequired: {ingest_error:?}"
                            )
                        })?;
                }
            }
        }
        let snapshot = pipeline.snapshot();
        let balance_discrepancies = pipeline
            .settlement_balance_discrepancies(&account_id, &venue_id, &balances)
            .map_err(|error| format!("CCXT 账户余额对账失败: {error:?}"))?;
        let position_snapshots_count = snapshot
            .account_positions
            .get(&(account_id.clone(), venue_id.clone()))
            .map(Vec::len)
            .unwrap_or(0);
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root,
            worker_id: &worker.id,
            account_id: &account_id,
            venue_id: &venue_id,
            observed_ts: received_ts,
            issues: &[],
            balances_count: balances.len(),
            balance_discrepancies: &balance_discrepancies,
            position_snapshots_count,
            funding_rate_snapshots_count: snapshot.funding_rates.len(),
            cashflow_count,
        })?;
        context.heartbeat(received_ts)?;
        context.mark(
            qx_runtime::ServiceStatus::Ready,
            format!("ccxt reconciled order_updates={updates}"),
            Some(received_ts),
        )?;
        if once {
            break;
        }
        thread::sleep(Duration::from_secs(5));
    }
    context.mark(
        qx_runtime::ServiceStatus::Stopped,
        "ccxt reconciler stopped",
        Some(runtime_timestamp_ms()),
    )?;
    Ok(())
}

fn run_ccxt_worker(
    path: &Path,
    worker_id: &str,
    ccxt_config_path: &Path,
    once: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled
        || !matches!(
            worker.role,
            WorkerRole::MarketData
                | WorkerRole::UserStream
                | WorkerRole::Execution
                | WorkerRole::Reconciler
        )
    {
        return Err(format!(
            "worker {worker_id} 不是启用的 CCXT MarketData/UserStream/Execution/Reconciler worker"
        ));
    }
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.eq_ignore_ascii_case("paper"))
        .unwrap_or(true)
    {
        return Err(format!("worker {worker_id} 必须绑定非 paper 的 CCXT venue"));
    }
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let queue = configured_command_queue(&config, &root)?;
    let ccxt_config_path = resolve_ccxt_config_path(path, &ccxt_config_path.to_string_lossy());
    validate_ccxt_worker_binding(&worker, Path::new(&ccxt_config_path))?;
    let supervisor = RuntimeSupervisor::new(config)?;
    let registered_id = worker.id.clone();
    let worker_role = worker.role;
    let handle = supervisor.spawn_worker(&registered_id, move |context| match worker_role {
        WorkerRole::MarketData => run_ccxt_market_worker(
            context,
            worker,
            pipeline_storage.clone(),
            ccxt_config_path,
            runtime_config_path.clone(),
            once,
        ),
        WorkerRole::UserStream => run_ccxt_user_stream_worker(
            context,
            worker,
            pipeline_storage.clone(),
            ccxt_config_path,
            runtime_config_path.clone(),
            once,
        ),
        WorkerRole::Reconciler => run_ccxt_reconcile_worker(
            context,
            worker,
            &root,
            pipeline_storage.clone(),
            ccxt_config_path,
            runtime_config_path.clone(),
            once,
        ),
        WorkerRole::Execution => run_ccxt_execution_worker(
            context,
            worker,
            pipeline_storage,
            control_store,
            queue,
            ccxt_config_path,
            runtime_config_path,
            once,
        ),
        _ => Err("unsupported CCXT worker role".into()),
    })?;
    handle
        .join()
        .map_err(|_| format!("CCXT worker {worker_id} panic"))?
}

/// 下载公共 CCXT OHLCV 快照，输出为 qianxing_bridge.BarFrame JSON，作为回测
/// 的不可变输入；回测运行期间不再访问交易所。
fn run_ccxt_fetch_ohlcv(
    ccxt_config_path: &Path,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    output_path: &Path,
) -> Result<(), String> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(frame)
            .map_err(|error| format!("编码 OHLCV 快照失败: {error}"))?,
    )
    .map_err(|error| format!("写入 OHLCV 快照失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · OHLCV] instrument={} timeframe={} output={} ✓",
        instrument,
        timeframe,
        output_path.display()
    );
    Ok(())
}

fn run_ccxt_market_spec(
    ccxt_config_path: &Path,
    instrument: &str,
    output_path: &Path,
) -> Result<(), String> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)?;
    let result = client
        .call(serde_json::json!({
            "op": "resolve_market",
            "instrument": instrument,
        }))
        .map_err(|error| format!("CCXT market spec 查询失败: {error}"))?;
    let mut market = result
        .get("market")
        .cloned()
        .ok_or_else(|| "CCXT market spec 响应缺少 market".to_string())?;
    let tiers = match client.call(serde_json::json!({
        "op": "fetch_leverage_tiers",
        "instrument": instrument,
    })) {
        Ok(value) => value
            .get("tiers")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        Err(error) if ccxt_error_is_optional_derivatives_capability(&error) => {
            serde_json::json!([])
        }
        Err(error) => return Err(format!("CCXT leverage tiers 查询失败: {error}")),
    };
    if let Some(object) = market.as_object_mut() {
        object.insert("leverage_tiers".into(), tiers);
    }
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&market)
            .map_err(|error| format!("编码 market spec 失败: {error}"))?,
    )
    .map_err(|error| format!("写入 market spec 失败 {}: {error}", output_path.display()))?;
    println!(
        "[CCXT · MarketSpec] instrument={} output={} ✓",
        instrument,
        output_path.display()
    );
    Ok(())
}

fn run_binance_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, path);
    if !worker.enabled {
        return Err(format!("worker {} 未启用", worker.id));
    }
    if !matches!(
        worker.role,
        WorkerRole::MarketData
            | WorkerRole::UserStream
            | WorkerRole::Execution
            | WorkerRole::Reconciler
    ) {
        return Err(format!(
            "worker {} 不是 Binance 数据/用户流/执行/对账角色",
            worker.id
        ));
    }
    if worker
        .venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("binance"))
        != Some(true)
    {
        return Err(format!("worker {} 不是 Binance Venue", worker.id));
    }
    if once && matches!(worker.role, WorkerRole::MarketData | WorkerRole::UserStream) {
        return Err("binance-worker --once 只支持 execution 或 reconciler worker".into());
    }
    let pipeline_root = Path::new(&config.storage.data_dir).to_path_buf();
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let runtime_config_path = path.to_path_buf();
    let control_store = configured_control_store(&config)?;
    let command_queue = configured_command_queue(&config, &pipeline_root)?;
    let supervisor = RuntimeSupervisor::new(config)?;
    let worker_role = worker.role;
    let registered_id = worker.id.clone();
    let worker_for_run = worker.clone();
    let handle = supervisor.spawn_worker(&registered_id, move |context| match worker_role {
        WorkerRole::MarketData => {
            run_binance_market_worker(context, worker_for_run.clone(), pipeline_storage.clone())
        }
        WorkerRole::UserStream => {
            run_binance_user_worker(context, worker_for_run.clone(), pipeline_storage.clone())
        }
        WorkerRole::Execution => run_binance_execution_worker(
            context,
            worker_for_run,
            pipeline_storage.clone(),
            control_store,
            command_queue,
            runtime_config_path,
            once,
        ),
        WorkerRole::Reconciler => run_binance_reconcile_worker(
            context,
            worker_for_run,
            &pipeline_root,
            pipeline_storage,
            once,
        ),
        _ => Err("unsupported Binance worker role".into()),
    })?;
    handle
        .join()
        .map_err(|_| format!("worker {worker_id} panic"))?
}

#[cfg(test)]
fn managed_worker_args(
    config: &RuntimeConfig,
    config_path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<Vec<(String, Vec<String>)>, String> {
    Ok(
        qx_orchestrator::plan_workers(config, config_path, allow_unmanaged_roles)?
            .into_iter()
            .map(|launch| (launch.worker_id, launch.args))
            .collect(),
    )
}

/// 跨平台进程托管入口。它只做拓扑校验、日志隔离和 fail-fast 生命周期管理，
/// 不替代 worker 的租约、幂等和 EventLog 恢复语义；任一子进程异常退出时会
/// 停止其余进程，避免 API/策略仍运行而执行器已经消失。
fn run_process_supervisor(path: &Path, allow_unmanaged_roles: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("解析 qx-cli 可执行文件失败: {error}"))?;
    let work_dir = std::env::current_dir().map_err(|error| format!("读取工作目录失败: {error}"))?;
    supervise_workers(&config, path, &executable, &work_dir, allow_unmanaged_roles)
}

struct SmaBarStrategy {
    fast: usize,
    slow: usize,
}

impl BarStrategy for SmaBarStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if self.fast == 0 || self.slow == 0 || self.fast >= self.slow {
            return None;
        }
        if history.len() < self.slow + 1 {
            return None;
        }
        let end = history.len();
        let sum = |start: usize, stop: usize| {
            history[start..stop]
                .iter()
                .map(|bar| bar.close)
                .sum::<i128>()
        };
        let fast_now = sum(end - self.fast, end) / self.fast as i128;
        let slow_now = sum(end - self.slow, end) / self.slow as i128;
        let fast_prev = sum(end - self.fast - 1, end - 1) / self.fast as i128;
        let slow_prev = sum(end - self.slow - 1, end - 1) / self.slow as i128;
        let golden = fast_prev <= slow_prev && fast_now > slow_now;
        let death = fast_prev >= slow_prev && fast_now < slow_now;
        let (side, should_trade) = if golden && position == 0 {
            (Side::Buy, true)
        } else if death && position > 0 {
            (Side::Sell, true)
        } else {
            (Side::Buy, false)
        };
        if !should_trade {
            return None;
        }
        Some(Order {
            client_id: 0,
            instrument: instrument.clone(),
            side,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        })
    }
}

fn ccxt_market_to_spec(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    let market_type = market
        .get("market_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("spot");
    let product = match market_type {
        "spot" => TradingProduct::Spot,
        "margin" => TradingProduct::Margin,
        "swap" | "perpetual" => TradingProduct::Perpetual,
        "future" | "futures" => TradingProduct::Future,
        other => return Err(format!("CCXT market type 不支持: {other}")),
    };
    let base_currency = market
        .get("base")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 base".to_string())?;
    let quote_currency = market
        .get("quote")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 quote".to_string())?;
    let settlement_currency = market
        .get("settle")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(quote_currency);
    let contract_size = raw_json_i128(market, "contract_size_raw")?;
    let linear = market
        .get("linear")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(!product.is_derivative());
    let inverse = market
        .get("inverse")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let max_leverage = market
        .get("max_leverage")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(if product.is_derivative() { 100 } else { 1 });
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product,
        base_currency: base_currency.into(),
        quote_currency: quote_currency.into(),
        settlement_currency: settlement_currency.into(),
        contract_size,
        linear,
        inverse,
        // CCXT 市场快照的精度字段由不同 exchange/precisionMode 表达；
        // 若未由上游归一化，使用最小定点单位并要求部署侧覆盖。
        price_tick: raw_json_i128(market, "price_tick_raw").unwrap_or(1),
        qty_step: raw_json_i128(market, "qty_step_raw").unwrap_or(1),
        min_qty: raw_json_i128(market, "min_qty_raw").unwrap_or(1),
        max_leverage,
        maintenance_margin_bps: market
            .get("maintenance_margin_bps")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(500),
        valid_from: 0,
        valid_to: market.get("expiry_ms").and_then(serde_json::Value::as_u64),
    };
    spec.validate()
        .map_err(|error| format!("CCXT market spec 非法: {error:?}"))?;
    Ok(spec)
}

fn ccxt_margin_rule_from_market(market: &serde_json::Value) -> Box<dyn MarginRule> {
    let Some(rows) = market
        .get("leverage_tiers")
        .and_then(serde_json::Value::as_array)
    else {
        return Box::new(NoMargin);
    };
    let tiers = rows
        .iter()
        .filter_map(|row| {
            let max_notional = row
                .get("max_notional_raw")
                .and_then(serde_json::Value::as_i64)
                .map(i128::from)
                .or_else(|| {
                    row.get("max_notional_raw")
                        .and_then(serde_json::Value::as_u64)
                        .map(i128::from)
                })
                .unwrap_or(i128::MAX);
            let initial_bp = row
                .get("initial_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let maintenance_bp = row
                .get("maintenance_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let max_leverage = row
                .get("max_leverage")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            (max_notional > 0 && initial_bp >= 0 && maintenance_bp >= 0).then_some(MarginTier {
                max_notional,
                initial_bp,
                maintenance_bp,
                max_leverage,
            })
        })
        .collect::<Vec<_>>();
    if tiers.is_empty() {
        Box::new(NoMargin)
    } else {
        Box::new(TieredMargin { tiers })
    }
}

fn run_strategy_backtest(
    runtime_path: &Path,
    frame_path: &Path,
    spec_path: Option<&Path>,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let payload = std::fs::read_to_string(frame_path).map_err(|error| {
        format!(
            "读取策略回测 BarFrame 失败 {}: {error}",
            frame_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "策略回测 BarFrame 校验失败 {}: {error:?}",
            frame_path.display()
        )
    })?;
    let bars: Vec<Bar> = (&frame).into();
    if bars.len() < 2 {
        return Err("跨语言 Bar 回测至少需要两根 Bar".into());
    }
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    for strategy in strategies {
        let strategy_id = strategy
            .id
            .clone()
            .unwrap_or_else(|| strategy.version.clone());
        let mut strategy_config = config.clone();
        strategy_config.strategy = strategy;
        strategy_config.strategies.clear();
        resolve_strategy_runtime_paths(&mut strategy_config.strategy, runtime_path);
        verify_strategy_artifact(&strategy_config.strategy)?;
        run_single_strategy_backtest(&strategy_config, &frame, &bars, spec_path, &strategy_id)?;
    }
    Ok(())
}

fn run_fast_backtest_manifest(manifest_path: &Path) -> Result<(), String> {
    let payload = std::fs::read_to_string(manifest_path).map_err(|error| {
        format!(
            "读取快速回测 manifest 失败 {}: {error}",
            manifest_path.display()
        )
    })?;
    let document: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("快速回测 manifest JSON 无效: {error}"))?;
    let jobs = document
        .get("jobs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "快速回测 manifest 必须包含 jobs 数组".to_string())?;
    if jobs.is_empty() || jobs.len() > 256 {
        return Err("快速回测 jobs 数量必须在 1..=256 内".into());
    }
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let resolve = |value: &str| {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    let mut parsed = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        let object = job
            .as_object()
            .ok_or_else(|| format!("快速回测 jobs[{index}] 必须是对象"))?;
        let runtime = object
            .get("runtime")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 runtime"))?;
        let bars = object
            .get("bars")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 bars"))?;
        let spec = object
            .get("market_spec")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(resolve);
        parsed.push((index, resolve(runtime), resolve(bars), spec));
    }
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(parsed.len());
        for (index, runtime, bars, spec) in parsed {
            handles.push(scope.spawn(move || {
                run_strategy_backtest(&runtime, &bars, spec.as_deref())
                    .map(|_| index)
                    .map_err(|error| format!("jobs[{index}] {error}"))
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "快速回测任务线程 panic".to_string())?
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    println!(
        "[Fast Backtest] manifest={} jobs={} completed={}",
        manifest_path.display(),
        jobs.len(),
        results.len()
    );
    Ok(())
}

fn run_single_strategy_backtest(
    config: &RuntimeConfig,
    frame: &BarFrame,
    bars: &[Bar],
    spec_path: Option<&Path>,
    strategy_id: &str,
) -> Result<(), String> {
    let mut margin: Box<dyn MarginRule> = Box::new(NoMargin);
    let instrument_spec = if let Some(spec_path) = spec_path {
        let spec_payload = std::fs::read_to_string(spec_path).map_err(|error| {
            format!(
                "读取策略回测 market spec 失败 {}: {error}",
                spec_path.display()
            )
        })?;
        let market: serde_json::Value = serde_json::from_str(&spec_payload)
            .map_err(|error| format!("策略回测 market spec JSON 无效: {error}"))?;
        margin = ccxt_margin_rule_from_market(&market);
        Some(ccxt_market_to_spec(&frame.instrument, &market)?)
    } else {
        None
    };
    if config
        .strategy
        .product
        .is_some_and(TradingProduct::is_derivative)
        && instrument_spec.is_none()
    {
        return Err("衍生品跨语言回测必须提供 market-spec.json".into());
    }
    let account_id = config
        .strategy
        .account_id
        .clone()
        .unwrap_or_else(|| "backtest".into());
    let currency = instrument_spec
        .as_ref()
        .map(|spec| spec.settlement_currency.clone())
        .unwrap_or_else(|| "USDT".into());
    let initial_cash = Money::from_i64(100_000);
    let backtest_config = BacktestConfig {
        instrument: frame.instrument.clone(),
        instrument_spec,
        account_id,
        currency,
        initial_cash,
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin,
        seed: 20260911,
        risk: RiskGate::new(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let report = if config.strategy.builtin_strategy.is_some() {
        if let Some(configured) = config.strategy.instrument.as_deref() {
            let configured = InstrumentId::parse(configured)
                .ok_or_else(|| format!("Strategy instrument 非法: {configured}"))?;
            if configured != frame.instrument {
                return Err(format!(
                    "内置策略回测 instrument 不一致: strategy={} frame={}",
                    configured, frame.instrument
                ));
            }
        }
        let builtin_config =
            builtin_strategy_config_from_runtime(&config.strategy, &frame.instrument)?;
        let context = NativeStrategyContext {
            strategy_id: builtin_config.strategy_id.clone(),
            strategy_version: builtin_config.strategy_version.clone(),
            account_id: config
                .strategy
                .account_id
                .clone()
                .unwrap_or_else(|| "backtest".into()),
            venue_id: config
                .strategy
                .venue_id
                .clone()
                .unwrap_or_else(|| frame.instrument.venue.to_string()),
            data_fingerprint: format!("barframe:{:016x}", frame.digest()),
            as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
            positions: BTreeMap::new(),
            cash: BTreeMap::from([(backtest_config.currency.clone(), initial_cash.raw())]),
            available_margin_raw: Some(initial_cash.raw()),
            risk_state: "backtest-verified".into(),
        };
        let strategy = BuiltinStrategy::new(builtin_config)?;
        let mut strategy = NativeBarStrategy::new(strategy, context);
        strategy
            .initialize()
            .map_err(|error| format!("初始化内置策略失败: {error:?}"))?;
        BacktestEngine::new(backtest_config)
            .run(bars, &mut strategy)
            .map_err(|error| format!("内置策略回测失败: {error:?}"))?
    } else {
        let mut strategy = ContractBarStrategy::from_config(
            config.clone(),
            frame,
            initial_cash,
            backtest_config.currency.clone(),
        )?;
        BacktestEngine::new(backtest_config)
            .run(bars, &mut strategy)
            .map_err(|error| format!("跨语言策略回测失败: {error:?}"))?
    };
    println!(
        "[Strategy · Backtest] strategy={} instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        strategy_id,
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    Ok(())
}

fn run_ccxt_backtest(
    frame_path: &Path,
    fast: usize,
    slow: usize,
    spec_path: Option<&Path>,
) -> Result<(), String> {
    if fast == 0 || slow == 0 || fast >= slow {
        return Err("回测 fast/slow 必须满足 0 < fast < slow".into());
    }
    let payload = std::fs::read_to_string(frame_path)
        .map_err(|error| format!("读取 BarFrame 失败 {}: {error}", frame_path.display()))?;
    let frame = BarFrame::from_json(&payload)
        .map_err(|error| format!("BarFrame 校验失败 {}: {error:?}", frame_path.display()))?;
    let instrument = frame.instrument.clone();
    let bars: Vec<Bar> = (&frame).into();
    let mut margin: Box<dyn MarginRule> = Box::new(NoMargin);
    let instrument_spec = if let Some(spec_path) = spec_path {
        let spec_payload = std::fs::read_to_string(spec_path).map_err(|error| {
            format!(
                "读取 CCXT market spec 失败 {}: {error}",
                spec_path.display()
            )
        })?;
        let market: serde_json::Value = serde_json::from_str(&spec_payload)
            .map_err(|error| format!("CCXT market spec JSON 无效: {error}"))?;
        margin = ccxt_margin_rule_from_market(&market);
        Some(ccxt_market_to_spec(&instrument, &market)?)
    } else {
        None
    };
    let config = BacktestConfig {
        instrument,
        instrument_spec,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin,
        seed: 20260911,
        risk: RiskGate::new(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let report = BacktestEngine::new(config)
        .run(&bars, &mut SmaBarStrategy { fast, slow })
        .map_err(|error| format!("CCXT 快照回测失败: {error:?}"))?;
    println!(
        "[CCXT · Backtest] instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    Ok(())
}

fn run_builtin_backtest(
    strategy_name: &str,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    let payload = std::fs::read_to_string(frame_path).map_err(|error| {
        format!(
            "读取内置策略 BarFrame 失败 {}: {error}",
            frame_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "内置策略 BarFrame 校验失败 {}: {error:?}",
            frame_path.display()
        )
    })?;
    let bars: Vec<Bar> = (&frame).into();
    if bars.len() < 3 {
        return Err("内置策略回测至少需要三根 Bar".into());
    }

    let mut margin: Box<dyn MarginRule> = Box::new(NoMargin);
    let instrument_spec = if let Some(spec_path) = spec_path {
        let spec_payload = std::fs::read_to_string(spec_path).map_err(|error| {
            format!(
                "读取内置策略 market spec 失败 {}: {error}",
                spec_path.display()
            )
        })?;
        let market: serde_json::Value = serde_json::from_str(&spec_payload)
            .map_err(|error| format!("内置策略 market spec JSON 无效: {error}"))?;
        margin = ccxt_margin_rule_from_market(&market);
        Some(ccxt_market_to_spec(&frame.instrument, &market)?)
    } else {
        None
    };
    let context = NativeStrategyContext {
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "main".into(),
        venue_id: frame.instrument.venue.to_string(),
        data_fingerprint: format!("barframe:{:?}", frame.source),
        as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "ready".into(),
    };
    let strategy_config = BuiltinStrategyConfig::new(
        kind,
        format!("builtin-{}", kind.name()),
        frame.instrument.clone(),
        Quantity::from_i64(quantity),
    )?;
    let strategy = BuiltinStrategy::new(strategy_config)?;
    let mut strategy = NativeBarStrategy::new(strategy, context);
    strategy
        .initialize()
        .map_err(|error| format!("初始化内置策略失败: {error:?}"))?;
    let config = BacktestConfig {
        instrument: frame.instrument.clone(),
        instrument_spec,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin,
        seed: 20260914,
        risk: RiskGate::new(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let report = BacktestEngine::new(config)
        .run(&bars, &mut strategy)
        .map_err(|error| format!("内置策略回测失败: {error:?}"))?;
    println!(
        "[Builtin · Backtest] strategy={} instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        kind.name(),
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    Ok(())
}

struct ScheduledTargetStrategy {
    instrument: InstrumentId,
    targets: BTreeMap<u64, i128>,
    policy: Option<OrderPolicy>,
    account_id: String,
}

impl BarStrategy for ScheduledTargetStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if instrument != &self.instrument {
            return None;
        }
        let visible_ts = history.last()?.ts;
        let target = self
            .targets
            .range(..=visible_ts)
            .next_back()
            .map(|(_, target)| *target)
            .unwrap_or(0);
        let delta = target.checked_sub(position)?;
        if delta == 0 {
            return None;
        }
        Some(Order {
            client_id: 0,
            instrument: self.instrument.clone(),
            side: if delta > 0 { Side::Buy } else { Side::Sell },
            qty: Quantity::from_raw(delta.checked_abs()?),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: self.account_id.clone(),
            trace: None,
            policy: self.policy,
        })
    }
}

fn read_bar_frame_for_multi_backtest(path: &Path, label: &str) -> Result<BarFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取{label} BarFrame 失败 {}: {error}", path.display()))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("{label} BarFrame 校验失败 {}: {error:?}", path.display()))
}

fn multi_backtest_market_spec(
    instrument: &InstrumentId,
    path: Option<&Path>,
) -> Result<(Option<TradingInstrumentSpec>, Box<dyn MarginRule>), String> {
    let Some(path) = path else {
        return Ok((None, Box::new(NoMargin)));
    };
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取多腿 market spec 失败 {}: {error}", path.display()))?;
    let market: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("多腿 market spec JSON 无效 {}: {error}", path.display()))?;
    let margin = ccxt_margin_rule_from_market(&market);
    let spec = ccxt_market_to_spec(instrument, &market)?;
    Ok((Some(spec), margin))
}

fn run_multi_builtin_backtest(
    strategy_name: &str,
    primary_path: &Path,
    reference_path: &Path,
    primary_spec_path: Option<&Path>,
    reference_spec_path: Option<&Path>,
    quantity: i64,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("多腿内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if !matches!(
        kind,
        BuiltinStrategyKind::PairsArbitrage
            | BuiltinStrategyKind::BasisArbitrage
            | BuiltinStrategyKind::CrossVenueArbitrage
            | BuiltinStrategyKind::SpotFuturesArbitrage
    ) {
        return Err(format!("多腿回测只接受套利策略，当前为 {}", kind.name()));
    }
    let primary_frame = read_bar_frame_for_multi_backtest(primary_path, "主腿")?;
    let reference_frame = read_bar_frame_for_multi_backtest(reference_path, "对冲腿")?;
    if primary_frame.ts != reference_frame.ts {
        return Err("多腿回测要求两条 BarFrame 的时间戳完全对齐".into());
    }
    let primary_bars: Vec<Bar> = (&primary_frame).into();
    let reference_bars: Vec<Bar> = (&reference_frame).into();
    if primary_bars.len() < 3 {
        return Err("多腿内置策略回测至少需要三根对齐 Bar".into());
    }
    let strategy_config = BuiltinStrategyConfig {
        kind,
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        instrument: primary_frame.instrument.clone(),
        quantity: Quantity::from_i64(quantity),
        fast_window: 5,
        slow_window: 20,
        period: 14,
        threshold_bps: 100,
        reference_instrument: Some(reference_frame.instrument.clone()),
        primary_policy: None,
        reference_policy: None,
    };
    let mut native = BuiltinStrategy::new(strategy_config)?;
    let mut context = NativeStrategyContext {
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "multi-leg-backtest".into(),
        venue_id: primary_frame.instrument.venue.to_string(),
        data_fingerprint: format!("{:?}:{:?}", primary_frame.source, reference_frame.source),
        as_of: primary_bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::from([
            (primary_frame.instrument.to_string(), 0),
            (reference_frame.instrument.to_string(), 0),
        ]),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "multi-leg-backtest".into(),
    };
    native
        .on_init(&context)
        .map_err(|error| format!("初始化多腿内置策略失败: {error}"))?;
    let mut primary_targets = BTreeMap::new();
    let mut reference_targets = BTreeMap::new();
    for (primary_bar, reference_bar) in primary_bars.iter().zip(&reference_bars) {
        context.as_of = primary_bar.ts;
        let reference_event = NativeMarketEvent::Bar {
            instrument: reference_frame.instrument.clone(),
            ts: reference_bar.ts,
            open_raw: reference_bar.open,
            high_raw: reference_bar.high,
            low_raw: reference_bar.low,
            close_raw: reference_bar.close,
            volume_raw: reference_bar.volume,
        };
        native
            .on_event(&context, &reference_event)
            .map_err(|error| format!("处理多腿对冲 Bar 失败: {error}"))?;
        let primary_event = NativeMarketEvent::Bar {
            instrument: primary_frame.instrument.clone(),
            ts: primary_bar.ts,
            open_raw: primary_bar.open,
            high_raw: primary_bar.high,
            low_raw: primary_bar.low,
            close_raw: primary_bar.close,
            volume_raw: primary_bar.volume,
        };
        let decision = native
            .on_event(&context, &primary_event)
            .map_err(|error| format!("处理多腿主 Bar 失败: {error}"))?;
        for intent in decision.intents {
            let instrument_key = intent.instrument.to_string();
            let current = context.positions.get(&instrument_key).copied().unwrap_or(0);
            let delta = if intent.side == Side::Buy {
                intent.qty.raw()
            } else {
                -intent.qty.raw()
            };
            let target = current.checked_add(delta).ok_or("多腿回测目标仓位溢出")?;
            context.positions.insert(instrument_key.clone(), target);
            if intent.instrument == primary_frame.instrument {
                primary_targets.insert(primary_bar.ts, target);
            } else if intent.instrument == reference_frame.instrument {
                reference_targets.insert(reference_bar.ts, target);
            }
        }
    }
    let (primary_spec, primary_margin) =
        multi_backtest_market_spec(&primary_frame.instrument, primary_spec_path)?;
    let (reference_spec, reference_margin) =
        multi_backtest_market_spec(&reference_frame.instrument, reference_spec_path)?;
    if primary_spec
        .as_ref()
        .is_some_and(|spec| spec.product.is_derivative())
        && primary_spec_path.is_none()
    {
        return Err("主腿衍生品多腿回测必须提供 market spec".into());
    }
    let run_leg = |frame: &BarFrame,
                   bars: &[Bar],
                   spec: Option<TradingInstrumentSpec>,
                   margin: Box<dyn MarginRule>,
                   targets: BTreeMap<u64, i128>,
                   leg: &str|
     -> Result<qx_xingban::BacktestReport, String> {
        let currency = spec
            .as_ref()
            .map(|value| value.settlement_currency.clone())
            .unwrap_or_else(|| "USDT".into());
        let mut strategy = ScheduledTargetStrategy {
            instrument: frame.instrument.clone(),
            targets,
            policy: None,
            account_id: format!("multi-leg-{leg}"),
        };
        BacktestEngine::new(BacktestConfig {
            instrument: frame.instrument.clone(),
            instrument_spec: spec,
            account_id: format!("multi-leg-{leg}"),
            currency,
            initial_cash: Money::from_i64(100_000),
            multiplier: 1,
            fill: Box::new(NextBarOpenFillModel),
            fee: Box::new(MakerTakerFeeModel {
                maker_bp: 2,
                taker_bp: 5,
            }),
            data_tier: DataTier::Bar,
            latency: Box::new(ZeroLatency),
            margin,
            seed: 20260914,
            risk: RiskGate::new(),
            virtual_trading: VirtualTradingConfig::default(),
        })
        .run(bars, &mut strategy)
        .map_err(|error| format!("{leg} 多腿回测失败: {error:?}"))
    };
    let primary_report = run_leg(
        &primary_frame,
        &primary_bars,
        primary_spec,
        primary_margin,
        primary_targets,
        "primary",
    )?;
    let reference_report = run_leg(
        &reference_frame,
        &reference_bars,
        reference_spec,
        reference_margin,
        reference_targets,
        "reference",
    )?;
    println!(
        "[Multi-leg · Backtest] strategy={} primary={} fills={} return_bps={} reference={} fills={} return_bps={} combined_return_bps={} result_hashes={:016x}/{:016x}",
        kind.name(),
        primary_frame.instrument,
        primary_report.fills.len(),
        primary_report.return_bps,
        reference_frame.instrument,
        reference_report.fills.len(),
        reference_report.return_bps,
        (i64::from(primary_report.return_bps) + i64::from(reference_report.return_bps)) / 2,
        primary_report.result_hash(),
        reference_report.result_hash()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_ccxt_builtin_backtest(
    ccxt_config_path: &Path,
    strategy_name: &str,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    spec_path: Option<&Path>,
    quantity: i64,
) -> Result<(), String> {
    if end_ms < start_ms {
        return Err("CCXT 内置策略回测 end_ms 不能早于 start_ms".into());
    }
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)
        .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    let temp_path = std::env::temp_dir().join(format!(
        "qianxing-ccxt-builtin-{}-{}.json",
        std::process::id(),
        runtime_timestamp_ms()
    ));
    std::fs::write(
        &temp_path,
        serde_json::to_string(frame)
            .map_err(|error| format!("编码 CCXT BarFrame 失败: {error}"))?,
    )
    .map_err(|error| format!("写入临时 CCXT BarFrame 失败: {error}"))?;
    let result = run_builtin_backtest(strategy_name, &temp_path, spec_path, quantity);
    let _ = std::fs::remove_file(&temp_path);
    result
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

#[cfg(feature = "nats")]
fn run_file_outbox_relay(
    data_root: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let store = FileOutboxStore::new(data_root);
    let publisher = NatsJetStreamPublisher::connect(nats_url, subject_prefix)?;
    let relay = OutboxRelay::new(
        store,
        publisher,
        format!("qx-cli-{}", std::process::id()),
        30,
    )
    .map_err(|error| format!("创建 Outbox relay 失败: {error:?}"))?;
    let report = relay
        .pump_once(runtime_timestamp_ms(), limit)
        .map_err(|error| format!("执行 Outbox relay 失败: {error:?}"))?;
    println!(
        "[Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
        report.last_error
    );
    Ok(())
}

#[cfg(all(feature = "nats", feature = "postgres"))]
fn run_postgres_outbox_relay(
    runtime_config_path: &Path,
    nats_url: &str,
    subject_prefix: &str,
    limit: usize,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_config_path)?;
    let dsn = configured_postgres_dsn(&config)?
        .ok_or_else(|| "outbox-relay-postgres 要求 storage.backend=postgres".to_string())?;
    let store =
        PostgresOutboxStore::connect_with_pool_size(&dsn, config.storage.postgres_pool_size)
            .map_err(|error| format!("打开 PostgreSQL Outbox 失败: {error:?}"))?;
    let publisher = NatsJetStreamPublisher::connect(nats_url, subject_prefix)?;
    let relay = OutboxRelay::new(
        store,
        publisher,
        format!("qx-cli-pg-{}", std::process::id()),
        30,
    )
    .map_err(|error| format!("创建 PostgreSQL Outbox relay 失败: {error:?}"))?;
    let report = relay
        .pump_once(runtime_timestamp_ms(), limit)
        .map_err(|error| format!("执行 PostgreSQL Outbox relay 失败: {error:?}"))?;
    println!(
        "[PostgreSQL Outbox relay] scanned={} published={} retried={} publish_failures={} lease_conflicts={} last_error={:?}",
        report.scanned,
        report.published,
        report.retried,
        report.publish_failures,
        report.lease_conflicts,
        report.last_error
    );
    Ok(())
}

#[cfg(feature = "nats")]
#[derive(Clone)]
struct WorkerMetricsSink {
    path: PathBuf,
    worker_id: String,
}

#[cfg(feature = "nats")]
impl WorkerMetricsSink {
    fn write(&self, body: &str) {
        if let Err(error) = write_worker_metrics(&self.path, body) {
            eprintln!(
                "worker={} metrics 写入失败，业务处理继续: {}",
                self.worker_id, error
            );
        }
    }
}

#[cfg(feature = "nats")]
#[derive(Default)]
struct RelayMetricTotals {
    scanned: u64,
    published: u64,
    retried: u64,
    lease_conflicts: u64,
    publish_failures: u64,
}

#[cfg(feature = "nats")]
impl RelayMetricTotals {
    fn apply(&mut self, report: &qx_storage::OutboxRelayReport) {
        self.scanned += report.scanned;
        self.published += report.published;
        self.retried += report.retried;
        self.lease_conflicts += report.lease_conflicts;
        self.publish_failures += report.publish_failures;
    }

    fn render(&self, sink: &WorkerMetricsSink, up: bool, now_ms: u64) -> String {
        let worker = prometheus_label(&sink.worker_id);
        format!(
            "qx_worker_up{{worker=\"{worker}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_scanned_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_published_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_retried_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_lease_conflicts_total{{worker=\"{worker}\"}} {}\n\
qx_outbox_relay_publish_failures_total{{worker=\"{worker}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.scanned,
            self.published,
            self.retried,
            self.lease_conflicts,
            self.publish_failures,
        )
    }
}

#[cfg(feature = "nats")]
#[derive(Default)]
struct ConsumerMetricTotals {
    received: u64,
    applied: u64,
    duplicates: u64,
    retried: u64,
    dead_lettered: u64,
    malformed: u64,
    ack_failures: u64,
}

#[cfg(feature = "nats")]
impl ConsumerMetricTotals {
    fn apply(&mut self, report: &qx_storage::NatsConsumerBatchReport) {
        self.received += report.received;
        self.applied += report.applied;
        self.duplicates += report.duplicates;
        self.retried += report.retried;
        self.dead_lettered += report.dead_lettered;
        self.malformed += report.malformed;
        self.ack_failures += report.ack_failures;
    }

    fn render(&self, sink: &WorkerMetricsSink, up: bool, now_ms: u64) -> String {
        let worker = prometheus_label(&sink.worker_id);
        format!(
            "qx_worker_up{{worker=\"{worker}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_received_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_applied_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_duplicates_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_retried_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_dead_lettered_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_malformed_total{{worker=\"{worker}\"}} {}\n\
qx_event_consumer_ack_failures_total{{worker=\"{worker}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.received,
            self.applied,
            self.duplicates,
            self.retried,
            self.dead_lettered,
            self.malformed,
            self.ack_failures,
        )
    }
}

#[cfg(feature = "nats")]
fn wait_for_worker_interval(context: &WorkerContext, interval_ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(interval_ms);
    while !context.should_stop() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(feature = "nats")]
fn run_outbox_relay_loop<S>(
    context: WorkerContext,
    relay: OutboxRelay<S, NatsJetStreamPublisher>,
    messaging: MessagingRuntimeConfig,
    metrics: WorkerMetricsSink,
    once: bool,
) -> Result<(), String>
where
    S: OutboxStore + 'static,
{
    let mut totals = RelayMetricTotals::default();
    metrics.write(&totals.render(&metrics, true, runtime_timestamp_ms()));
    loop {
        if context.should_stop() {
            break;
        }
        let now = runtime_timestamp_ms();
        let report = relay
            .pump_once(now, messaging.relay_batch_size)
            .map_err(|error| format!("Outbox relay 批次失败: {error:?}"))?;
        totals.apply(&report);
        metrics.write(&totals.render(&metrics, true, now));
        context.heartbeat(now)?;
        println!(
            "[Outbox relay worker={}] scanned={} published={} retried={} failures={} conflicts={} last_error={:?}",
            context.id(),
            report.scanned,
            report.published,
            report.retried,
            report.publish_failures,
            report.lease_conflicts,
            report.last_error
        );
        if once {
            break;
        }
        wait_for_worker_interval(&context, messaging.relay_interval_ms);
    }
    metrics.write(&totals.render(&metrics, false, runtime_timestamp_ms()));
    Ok(())
}

#[cfg(feature = "nats")]
fn run_relay_with_store<S>(
    supervisor: RuntimeSupervisor,
    worker_id: String,
    relay: OutboxRelay<S, NatsJetStreamPublisher>,
    messaging: MessagingRuntimeConfig,
    metrics: WorkerMetricsSink,
    once: bool,
) -> Result<(), String>
where
    S: OutboxStore + 'static,
{
    let handle = supervisor.spawn_worker(&worker_id, move |context| {
        run_outbox_relay_loop(context, relay, messaging, metrics, once)
    })?;
    handle
        .join()
        .map_err(|_| format!("Outbox relay worker {worker_id} panic"))?
}

#[cfg(feature = "nats")]
fn run_outbox_relay_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled || worker.role != WorkerRole::OutboxRelay {
        return Err(format!("worker {worker_id} 不是启用的 OutboxRelay worker"));
    }
    if !config.messaging.enabled {
        return Err("OutboxRelay worker 要求 messaging.enabled=true".into());
    }
    let publisher = NatsJetStreamPublisher::connect(
        &config.messaging.nats_url,
        &config.messaging.subject_prefix,
    )?;
    let relay_owner = format!("qx-relay-{worker_id}-{}", std::process::id());
    let lease_seconds = config.messaging.lease_seconds;
    let messaging = config.messaging.clone();
    let metrics = WorkerMetricsSink {
        path: worker_metrics_path(&config, worker_id),
        worker_id: worker_id.into(),
    };
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    match config.storage.backend {
        StorageBackend::Files => {
            let relay = OutboxRelay::new(
                FileOutboxStore::new(&config.storage.data_dir),
                publisher,
                relay_owner,
                lease_seconds,
            )
            .map_err(|error| format!("创建文件 Outbox relay 失败: {error:?}"))?;
            run_relay_with_store(
                supervisor,
                worker_id.into(),
                relay,
                messaging,
                metrics,
                once,
            )
        }
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前 qx-cli 未启用 sqlite feature，无法运行 SQLite Outbox relay".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                let relay = OutboxRelay::new(
                    SqliteOutboxStore::new(path)
                        .map_err(|error| format!("打开 SQLite Outbox 失败: {error:?}"))?,
                    publisher,
                    relay_owner,
                    lease_seconds,
                )
                .map_err(|error| format!("创建 SQLite Outbox relay 失败: {error:?}"))?;
                run_relay_with_store(
                    supervisor,
                    worker_id.into(),
                    relay,
                    messaging,
                    metrics,
                    once,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前 qx-cli 未启用 postgres feature，无法运行 PostgreSQL Outbox relay".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = configured_postgres_dsn(&config)?
                    .ok_or_else(|| "PostgreSQL backend 缺少 DSN".to_string())?;
                let relay = OutboxRelay::new(
                    PostgresOutboxStore::connect_with_pool_size(
                        &dsn,
                        config.storage.postgres_pool_size,
                    )
                    .map_err(|error| format!("打开 PostgreSQL Outbox 失败: {error:?}"))?,
                    publisher,
                    relay_owner,
                    lease_seconds,
                )
                .map_err(|error| format!("创建 PostgreSQL Outbox relay 失败: {error:?}"))?;
                run_relay_with_store(
                    supervisor,
                    worker_id.into(),
                    relay,
                    messaging,
                    metrics,
                    once,
                )
            }
        }
    }
}

#[cfg(feature = "nats")]
#[derive(Clone)]
struct EventConsumerHandler {
    executable: String,
    args: Vec<String>,
    timeout_ms: u64,
}

#[cfg(feature = "nats")]
struct EventConsumerRuntime {
    messaging: MessagingRuntimeConfig,
    handler: EventConsumerHandler,
    metrics: WorkerMetricsSink,
    once: bool,
}

#[cfg(feature = "nats")]
fn invoke_event_consumer_handler(
    handler: &EventConsumerHandler,
    event: &OutboxEvent,
) -> Result<(), String> {
    let payload = serde_json::to_vec(event)
        .map_err(|error| format!("事件 consumer envelope 序列化失败: {error}"))?;
    let mut command = Command::new(&handler.executable);
    command
        .args(&handler.args)
        .env_clear()
        .env("QX_EVENT_CONSUMER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动事件 consumer handler 失败: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let write_result = stdin
            .write_all(&payload)
            .and_then(|_| stdin.write_all(b"\n"));
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("写入事件 consumer handler stdin 失败: {error}"));
        }
    } else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("事件 consumer handler stdin 不可用".into());
    }
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("读取事件 consumer handler 状态失败: {error}"))?
        {
            if status.success() {
                return Ok(());
            }
            return Err(format!(
                "事件 consumer handler 退出失败: {}",
                status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".into())
            ));
        }
        if started.elapsed() >= Duration::from_millis(handler.timeout_ms) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "事件 consumer handler 超时: {}ms",
                handler.timeout_ms
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(feature = "nats")]
fn run_event_consumer_loop<S>(
    context: WorkerContext,
    consumer: NatsJetStreamConsumer,
    store: S,
    messaging: MessagingRuntimeConfig,
    handler: EventConsumerHandler,
    metrics: WorkerMetricsSink,
    once: bool,
) -> Result<(), String>
where
    S: ConsumerStateStore + Clone + Send + Sync + 'static,
{
    let mut totals = ConsumerMetricTotals::default();
    metrics.write(&totals.render(&metrics, true, runtime_timestamp_ms()));
    loop {
        if context.should_stop() {
            break;
        }
        let handler = handler.clone();
        let report = consumer.consume_batch(
            store.clone(),
            NatsJetStreamConsumer::current_timestamp(),
            messaging.consumer_batch_size,
            move |event| invoke_event_consumer_handler(&handler, event),
        )?;
        let now = runtime_timestamp_ms();
        totals.apply(&report);
        metrics.write(&totals.render(&metrics, true, now));
        context.heartbeat(now)?;
        println!(
            "[Event consumer worker={}] received={} applied={} duplicates={} retried={} dead_lettered={} malformed={} ack_failures={} last_error={:?}",
            context.id(),
            report.received,
            report.applied,
            report.duplicates,
            report.retried,
            report.dead_lettered,
            report.malformed,
            report.ack_failures,
            report.last_error
        );
        if once {
            break;
        }
        wait_for_worker_interval(&context, messaging.relay_interval_ms);
    }
    metrics.write(&totals.render(&metrics, false, runtime_timestamp_ms()));
    Ok(())
}

#[cfg(feature = "nats")]
fn run_consumer_with_store<S>(
    supervisor: RuntimeSupervisor,
    worker_id: String,
    consumer: NatsJetStreamConsumer,
    store: S,
    runtime: EventConsumerRuntime,
) -> Result<(), String>
where
    S: ConsumerStateStore + Clone + Send + Sync + 'static,
{
    let handle = supervisor.spawn_worker(&worker_id, move |context| {
        run_event_consumer_loop(
            context,
            consumer,
            store,
            runtime.messaging,
            runtime.handler,
            runtime.metrics,
            runtime.once,
        )
    })?;
    handle
        .join()
        .map_err(|_| format!("Event consumer worker {worker_id} panic"))?
}

#[cfg(feature = "nats")]
fn run_event_consumer_worker(path: &Path, worker_id: &str, once: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    if !worker.enabled || worker.role != WorkerRole::EventConsumer {
        return Err(format!(
            "worker {worker_id} 不是启用的 EventConsumer worker"
        ));
    }
    if !config.messaging.enabled {
        return Err("EventConsumer worker 要求 messaging.enabled=true".into());
    }
    let stream = config
        .messaging
        .consumer_stream
        .as_deref()
        .ok_or_else(|| "messaging.consumer_stream 未配置".to_string())?;
    let consumer_name = config
        .messaging
        .consumer_name
        .as_deref()
        .ok_or_else(|| "messaging.consumer_name 未配置".to_string())?;
    let group_id = config
        .messaging
        .consumer_group_id
        .as_deref()
        .ok_or_else(|| "messaging.consumer_group_id 未配置".to_string())?;
    let handler_executable = config
        .messaging
        .consumer_handler_executable
        .clone()
        .ok_or_else(|| "messaging.consumer_handler_executable 未配置".to_string())?;
    let consumer = NatsJetStreamConsumer::connect(
        &config.messaging.nats_url,
        stream,
        consumer_name,
        group_id,
        config.messaging.consumer_max_attempts,
    )?;
    let messaging = config.messaging.clone();
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let metrics = WorkerMetricsSink {
        path: worker_metrics_path(&config, worker_id),
        worker_id: worker_id.into(),
    };
    let handler = EventConsumerHandler {
        executable: handler_executable,
        args: messaging.consumer_handler_args.clone(),
        timeout_ms: messaging.consumer_handler_timeout_ms,
    };
    let runtime = EventConsumerRuntime {
        messaging,
        handler,
        metrics,
        once,
    };
    match config.storage.backend {
        StorageBackend::Files => run_consumer_with_store(
            supervisor,
            worker_id.into(),
            consumer,
            FileConsumerStateStore::new(&config.storage.data_dir),
            runtime,
        ),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前 qx-cli 未启用 sqlite feature，无法运行 SQLite EventConsumer".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let sqlite_path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                run_consumer_with_store(
                    supervisor,
                    worker_id.into(),
                    consumer,
                    SqliteConsumerStateStore::new(sqlite_path)
                        .map_err(|error| format!("打开 SQLite 消费状态失败: {error:?}"))?,
                    runtime,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前 qx-cli 未启用 postgres feature，无法运行 PostgreSQL EventConsumer".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = configured_postgres_dsn(&config)?
                    .ok_or_else(|| "PostgreSQL backend 缺少 DSN".to_string())?;
                run_consumer_with_store(
                    supervisor,
                    worker_id.into(),
                    consumer,
                    PostgresConsumerStateStore::connect_with_pool_size(
                        &dsn,
                        config.storage.postgres_pool_size,
                    )
                    .map_err(|error| format!("打开 PostgreSQL 消费状态失败: {error:?}"))?,
                    runtime,
                )
            }
        }
    }
}

#[cfg(feature = "nats")]
fn replay_dead_letter_from_store<S>(
    store: S,
    publisher: &NatsJetStreamPublisher,
    group_id: &str,
    event_id: &str,
) -> Result<(), String>
where
    S: ConsumerStateStore,
{
    let records = store
        .dead_letters(group_id, 10_000)
        .map_err(|error| format!("读取消费者死信失败: {error:?}"))?;
    let record = records
        .into_iter()
        .filter(|record| record.event_id == event_id)
        .max_by_key(|record| record.attempts)
        .ok_or_else(|| format!("找不到 group={group_id} event_id={event_id} 的死信记录"))?;
    let mut replay = record.event;
    // Replay is a new logical delivery. The deterministic suffix makes an
    // operator retry safe even though JetStream publisher itself is at-least-once.
    replay.event_id = format!("{}:replay:{}", replay.event_id, record.attempts);
    replay.attempts = 0;
    replay
        .validate()
        .map_err(|error| format!("重放事件校验失败: {error:?}"))?;
    publisher
        .publish(&replay)
        .map_err(|error| format!("发布死信重放事件失败: {error}"))?;
    println!(
        "[DLQ replay] group={} source_event_id={} replay_event_id={} published=true",
        group_id, event_id, replay.event_id
    );
    Ok(())
}

#[cfg(feature = "nats")]
fn run_dead_letter_replay(path: &Path, group_id: &str, event_id: &str) -> Result<(), String> {
    if group_id.trim().is_empty() || event_id.trim().is_empty() {
        return Err("DLQ 重放要求 group_id 和 event_id".into());
    }
    let config = read_runtime_config(path)?;
    if !config.messaging.enabled {
        return Err("DLQ 重放要求 messaging.enabled=true".into());
    }
    let publisher = NatsJetStreamPublisher::connect(
        &config.messaging.nats_url,
        &config.messaging.subject_prefix,
    )?;
    match config.storage.backend {
        StorageBackend::Files => replay_dead_letter_from_store(
            FileConsumerStateStore::new(&config.storage.data_dir),
            &publisher,
            group_id,
            event_id,
        ),
        StorageBackend::Sqlite => {
            #[cfg(not(feature = "sqlite"))]
            {
                Err("当前 qx-cli 未启用 sqlite feature，无法读取 SQLite DLQ".into())
            }
            #[cfg(feature = "sqlite")]
            {
                let sqlite_path = config
                    .storage
                    .sqlite_path
                    .as_deref()
                    .ok_or_else(|| "SQLite backend 缺少 sqlite_path".to_string())?;
                replay_dead_letter_from_store(
                    SqliteConsumerStateStore::new(sqlite_path)
                        .map_err(|error| format!("打开 SQLite 消费状态失败: {error:?}"))?,
                    &publisher,
                    group_id,
                    event_id,
                )
            }
        }
        StorageBackend::Postgres => {
            #[cfg(not(feature = "postgres"))]
            {
                Err("当前 qx-cli 未启用 postgres feature，无法读取 PostgreSQL DLQ".into())
            }
            #[cfg(feature = "postgres")]
            {
                let dsn = configured_postgres_dsn(&config)?
                    .ok_or_else(|| "PostgreSQL backend 缺少 DSN".to_string())?;
                replay_dead_letter_from_store(
                    PostgresConsumerStateStore::connect_with_pool_size(
                        &dsn,
                        config.storage.postgres_pool_size,
                    )
                    .map_err(|error| format!("打开 PostgreSQL 消费状态失败: {error:?}"))?,
                    &publisher,
                    group_id,
                    event_id,
                )
            }
        }
    }
}

fn main() {
    println!("牵星 Qianxing — 分级校准，量天定位\n");

    let mode = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    if matches!(mode.as_str(), "help" | "--help" | "-h") {
        print_cli_help();
        return;
    }
    if mode == "init" {
        let arguments = std::env::args().skip(2).collect::<Vec<_>>();
        let force = arguments.iter().any(|argument| argument == "--force");
        let output = arguments
            .iter()
            .find(|argument| !argument.starts_with('-'))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("qianxing.runtime.json"));
        if let Err(error) = run_init(&output, force) {
            eprintln!("初始化失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "backtest" {
        let runtime = std::env::args().nth(2).map(PathBuf::from);
        let frame = std::env::args().nth(3).map(PathBuf::from);
        let spec = std::env::args().nth(4).map(PathBuf::from);
        if let Err(error) =
            run_unified_backtest(runtime.as_deref(), frame.as_deref(), spec.as_deref())
        {
            eprintln!("统一策略回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "builtin-strategies" {
        for kind in BuiltinStrategyKind::ALL {
            println!("{}\t{}", kind.name(), kind.description());
        }
        return;
    }
    if mode == "builtin-backtest" {
        let strategy = match std::env::args().nth(2) {
            Some(value) => value,
            None => {
                eprintln!(
                    "builtin-backtest 需要 strategy bar-frame.json [market-spec.json] [quantity]"
                );
                std::process::exit(2);
            }
        };
        let frame = match std::env::args().nth(3) {
            Some(value) => value,
            None => {
                eprintln!("builtin-backtest 缺少 bar-frame.json");
                std::process::exit(2);
            }
        };
        let spec = std::env::args().nth(4).map(PathBuf::from);
        let quantity = std::env::args()
            .nth(5)
            .map(|value| value.parse::<i64>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("builtin-backtest quantity 非法");
                std::process::exit(2);
            })
            .unwrap_or(1);
        if let Err(error) =
            run_builtin_backtest(&strategy, Path::new(&frame), spec.as_deref(), quantity)
        {
            eprintln!("内置策略回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-builtin-backtest" {
        let ccxt_config = match std::env::args().nth(2) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-builtin-backtest 需要 ccxt-config strategy instrument start_ms end_ms [timeframe] [market-spec.json] [quantity]");
                std::process::exit(2);
            }
        };
        let strategy = match std::env::args().nth(3) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-builtin-backtest 缺少 strategy");
                std::process::exit(2);
            }
        };
        let instrument = match std::env::args().nth(4) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-builtin-backtest 缺少 instrument");
                std::process::exit(2);
            }
        };
        let start_ms = match std::env::args().nth(5).and_then(|value| value.parse().ok()) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-builtin-backtest start_ms 非法");
                std::process::exit(2);
            }
        };
        let end_ms = match std::env::args().nth(6).and_then(|value| value.parse().ok()) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-builtin-backtest end_ms 非法");
                std::process::exit(2);
            }
        };
        let timeframe = std::env::args().nth(7).unwrap_or_else(|| "1h".into());
        let spec = std::env::args().nth(8).map(PathBuf::from);
        let quantity = std::env::args()
            .nth(9)
            .map(|value| value.parse::<i64>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("ccxt-builtin-backtest quantity 非法");
                std::process::exit(2);
            })
            .unwrap_or(1);
        if let Err(error) = run_ccxt_builtin_backtest(
            Path::new(&ccxt_config),
            &strategy,
            &instrument,
            &timeframe,
            start_ms,
            end_ms,
            spec.as_deref(),
            quantity,
        ) {
            eprintln!("CCXT 内置策略回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "multi-builtin-backtest" {
        let strategy = match std::env::args().nth(2) {
            Some(value) => value,
            None => {
                eprintln!(
                    "multi-builtin-backtest 需要 strategy primary-bar.json reference-bar.json [primary-spec.json] [reference-spec.json] [quantity]"
                );
                std::process::exit(2);
            }
        };
        let primary = match std::env::args().nth(3) {
            Some(value) => value,
            None => {
                eprintln!("multi-builtin-backtest 缺少 primary-bar.json");
                std::process::exit(2);
            }
        };
        let reference = match std::env::args().nth(4) {
            Some(value) => value,
            None => {
                eprintln!("multi-builtin-backtest 缺少 reference-bar.json");
                std::process::exit(2);
            }
        };
        let primary_spec = std::env::args().nth(5).map(PathBuf::from);
        let reference_spec = std::env::args().nth(6).map(PathBuf::from);
        let quantity = std::env::args()
            .nth(7)
            .map(|value| value.parse::<i64>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("multi-builtin-backtest quantity 非法");
                std::process::exit(2);
            })
            .unwrap_or(1);
        if let Err(error) = run_multi_builtin_backtest(
            &strategy,
            Path::new(&primary),
            Path::new(&reference),
            primary_spec.as_deref(),
            reference_spec.as_deref(),
            quantity,
        ) {
            eprintln!("多腿内置策略回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "live-check" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
        if let Err(error) = run_live_check(Path::new(&path)) {
            eprintln!("实盘前置检查失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "recovery-child" {
        let args = std::env::args().collect::<Vec<_>>();
        if let Err(error) = run_recovery_child(&args) {
            eprintln!("跨进程恢复子进程失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "outbox-relay" {
        #[cfg(feature = "nats")]
        {
            let root = match std::env::args().nth(2) {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!("outbox-relay 需要 data-root nats-url subject-prefix [limit]");
                    std::process::exit(2);
                }
            };
            let url = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay 缺少 nats-url");
                    std::process::exit(2);
                }
            };
            let prefix = match std::env::args().nth(4) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay 缺少 subject-prefix");
                    std::process::exit(2);
                }
            };
            let limit = std::env::args()
                .nth(5)
                .map(|value| value.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("outbox-relay limit 非法");
                    std::process::exit(2);
                })
                .unwrap_or(100);
            if let Err(error) = run_file_outbox_relay(&root, &url, &prefix, limit) {
                eprintln!("Outbox relay 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("outbox-relay 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "outbox-relay-postgres" {
        #[cfg(all(feature = "nats", feature = "postgres"))]
        {
            let runtime_path = match std::env::args().nth(2) {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!(
                        "outbox-relay-postgres 需要 runtime.json nats-url subject-prefix [limit]"
                    );
                    std::process::exit(2);
                }
            };
            let url = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay-postgres 缺少 nats-url");
                    std::process::exit(2);
                }
            };
            let prefix = match std::env::args().nth(4) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay-postgres 缺少 subject-prefix");
                    std::process::exit(2);
                }
            };
            let limit = std::env::args()
                .nth(5)
                .map(|value| value.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("outbox-relay-postgres limit 非法");
                    std::process::exit(2);
                })
                .unwrap_or(100);
            if let Err(error) = run_postgres_outbox_relay(&runtime_path, &url, &prefix, limit) {
                eprintln!("PostgreSQL Outbox relay 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(all(feature = "nats", feature = "postgres")))]
        {
            eprintln!("outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "outbox-relay-worker" {
        #[cfg(feature = "nats")]
        {
            let path = std::env::args()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
            let worker_id = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay-worker 需要 runtime.json worker-id [--once]");
                    std::process::exit(2);
                }
            };
            let once = std::env::args().any(|argument| argument == "--once");
            if let Err(error) = run_outbox_relay_worker(&path, &worker_id, once) {
                eprintln!("Outbox relay worker 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("outbox-relay-worker 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "event-consumer-worker" {
        #[cfg(feature = "nats")]
        {
            let path = std::env::args()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
            let worker_id = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("event-consumer-worker 需要 runtime.json worker-id [--once]");
                    std::process::exit(2);
                }
            };
            let once = std::env::args().any(|argument| argument == "--once");
            if let Err(error) = run_event_consumer_worker(&path, &worker_id, once) {
                eprintln!("Event consumer worker 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("event-consumer-worker 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "consumer-dlq-replay" {
        #[cfg(feature = "nats")]
        {
            let path = match std::env::args().nth(2) {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!("consumer-dlq-replay 需要 runtime.json group-id event-id");
                    std::process::exit(2);
                }
            };
            let group_id = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("consumer-dlq-replay 缺少 group-id");
                    std::process::exit(2);
                }
            };
            let event_id = match std::env::args().nth(4) {
                Some(value) => value,
                None => {
                    eprintln!("consumer-dlq-replay 缺少 event-id");
                    std::process::exit(2);
                }
            };
            if let Err(error) = run_dead_letter_replay(&path, &group_id, &event_id) {
                eprintln!("DLQ 重放失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("consumer-dlq-replay 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "runtime-check" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        if let Err(error) = run_runtime_check(Path::new(&path)) {
            eprintln!("运行时配置校验失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "supervise" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let allow_unmanaged_roles =
            std::env::args().any(|argument| argument == "--allow-unmanaged-roles");
        if let Err(error) = run_process_supervisor(Path::new(&path), allow_unmanaged_roles) {
            eprintln!("进程监督器停止: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "serve" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        if let Err(error) = run_runtime_api(Path::new(&path)) {
            eprintln!("运行时 API 启动失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "scheduler-worker" || mode == "strategy-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("{mode} 需要 worker-id");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        let result = if mode == "scheduler-worker" {
            run_scheduler_worker(Path::new(&path), &worker_id, once)
        } else {
            run_strategy_worker(Path::new(&path), &worker_id, once)
        };
        if let Err(error) = result {
            eprintln!("{mode} 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("binance-worker 需要 worker-id");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        if let Err(error) = run_binance_worker(Path::new(&path), &worker_id, once) {
            eprintln!("Binance worker 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-public-probe" {
        let network = std::env::args().nth(2).unwrap_or_else(|| "testnet".into());
        let instrument = std::env::args()
            .nth(3)
            .unwrap_or_else(|| "BTCUSDT.BINANCE".into());
        let testnet = match network.as_str() {
            "testnet" => true,
            "mainnet" => false,
            _ => {
                eprintln!("binance-public-probe network 必须是 testnet 或 mainnet");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_binance_public_probe(testnet, &instrument) {
            eprintln!("Binance public probe 失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-private-probe" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
        let worker_id = std::env::args()
            .nth(3)
            .unwrap_or_else(|| "binance-execution-main".into());
        if let Err(error) = run_binance_private_probe(Path::new(&path), &worker_id) {
            eprintln!("Binance private probe 失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("ccxt-worker 需要 worker-id");
                std::process::exit(2);
            }
        };
        let ccxt_config = match std::env::args().nth(4) {
            Some(config) => config,
            None => {
                eprintln!("ccxt-worker 需要 ccxt-config.json");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        if let Err(error) =
            run_ccxt_worker(Path::new(&path), &worker_id, Path::new(&ccxt_config), once)
        {
            eprintln!("CCXT worker 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-fetch-ohlcv" {
        let ccxt_config = match std::env::args().nth(2) {
            Some(config) => config,
            None => {
                eprintln!("ccxt-fetch-ohlcv 需要 ccxt-config.json instrument start_ms end_ms output.json [timeframe]");
                std::process::exit(2);
            }
        };
        let instrument = match std::env::args().nth(3) {
            Some(instrument) => instrument,
            None => {
                eprintln!("ccxt-fetch-ohlcv 缺少 instrument");
                std::process::exit(2);
            }
        };
        let start_ms = match std::env::args().nth(4).and_then(|value| value.parse().ok()) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-fetch-ohlcv start_ms 非法");
                std::process::exit(2);
            }
        };
        let end_ms = match std::env::args().nth(5).and_then(|value| value.parse().ok()) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-fetch-ohlcv end_ms 非法");
                std::process::exit(2);
            }
        };
        let output = match std::env::args().nth(6) {
            Some(output) => output,
            None => {
                eprintln!("ccxt-fetch-ohlcv 缺少 output.json");
                std::process::exit(2);
            }
        };
        let timeframe = std::env::args().nth(7).unwrap_or_else(|| "1m".into());
        if let Err(error) = run_ccxt_fetch_ohlcv(
            Path::new(&ccxt_config),
            &instrument,
            &timeframe,
            start_ms,
            end_ms,
            Path::new(&output),
        ) {
            eprintln!("CCXT OHLCV 下载失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-backtest" {
        let frame = match std::env::args().nth(2) {
            Some(frame) => frame,
            None => {
                eprintln!("ccxt-backtest 需要 bar-frame.json [fast] [slow] [market-spec.json]");
                std::process::exit(2);
            }
        };
        let fast = std::env::args()
            .nth(3)
            .map(|value| value.parse::<usize>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("ccxt-backtest fast 非法");
                std::process::exit(2);
            })
            .unwrap_or(5);
        let slow = std::env::args()
            .nth(4)
            .map(|value| value.parse::<usize>())
            .transpose()
            .unwrap_or_else(|_| {
                eprintln!("ccxt-backtest slow 非法");
                std::process::exit(2);
            })
            .unwrap_or(20);
        let spec_path = std::env::args().nth(5).map(std::path::PathBuf::from);
        if let Err(error) = run_ccxt_backtest(Path::new(&frame), fast, slow, spec_path.as_deref()) {
            eprintln!("CCXT 快照回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "strategy-backtest" {
        let runtime = match std::env::args().nth(2) {
            Some(runtime) => runtime,
            None => {
                eprintln!("strategy-backtest 需要 runtime.json bar-frame.json [market-spec.json]");
                std::process::exit(2);
            }
        };
        let frame = match std::env::args().nth(3) {
            Some(frame) => frame,
            None => {
                eprintln!("strategy-backtest 缺少 bar-frame.json");
                std::process::exit(2);
            }
        };
        let spec_path = std::env::args().nth(4).map(std::path::PathBuf::from);
        if let Err(error) =
            run_strategy_backtest(Path::new(&runtime), Path::new(&frame), spec_path.as_deref())
        {
            eprintln!("跨语言策略回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "fast-backtest" {
        let manifest = match std::env::args().nth(2) {
            Some(manifest) => manifest,
            None => {
                eprintln!("fast-backtest 需要 manifest.json");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_fast_backtest_manifest(Path::new(&manifest)) {
            eprintln!("快速批量回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-market-spec" {
        let config = match std::env::args().nth(2) {
            Some(config) => config,
            None => {
                eprintln!("ccxt-market-spec 需要 ccxt-config.json instrument output.json");
                std::process::exit(2);
            }
        };
        let instrument = match std::env::args().nth(3) {
            Some(instrument) => instrument,
            None => {
                eprintln!("ccxt-market-spec 缺少 instrument");
                std::process::exit(2);
            }
        };
        let output = match std::env::args().nth(4) {
            Some(output) => output,
            None => {
                eprintln!("ccxt-market-spec 缺少 output.json");
                std::process::exit(2);
            }
        };
        if let Err(error) =
            run_ccxt_market_spec(Path::new(&config), &instrument, Path::new(&output))
        {
            eprintln!("CCXT market spec 下载失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-submit-order" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("binance-submit-order 需要 worker-id");
                std::process::exit(2);
            }
        };
        let command_path = match std::env::args().nth(4) {
            Some(command_path) => command_path,
            None => {
                eprintln!("binance-submit-order 需要 command.json");
                std::process::exit(2);
            }
        };
        if let Err(error) =
            run_binance_submit_order(Path::new(&path), &worker_id, Path::new(&command_path))
        {
            eprintln!("Binance SubmitOrder 执行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-submit-order" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let command_path = match std::env::args().nth(3) {
            Some(command_path) => command_path,
            None => {
                eprintln!("paper-submit-order 需要 command.json");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_paper_submit_order(Path::new(&path), Path::new(&command_path)) {
            eprintln!("Paper SubmitOrder 执行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("paper-worker 需要 worker-id");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        if let Err(error) = run_paper_execution_worker(Path::new(&path), &worker_id, once) {
            eprintln!("Paper Execution worker 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-e2e" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.paper-strategy.example.json".into());
        if let Err(error) = run_paper_pipeline_once(Path::new(&path)) {
            eprintln!("Paper 主链路验收失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-check" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.paper-strategy.example.json".into());
        if let Err(error) = run_paper_pipeline_once(Path::new(&path)) {
            eprintln!("Paper 主链路验收失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ecosystem" {
        run_ecosystem_smoke();
        return;
    }
    if mode == "paper" {
        run_paper_smoke();
        return;
    }
    if mode == "reconcile" {
        if let Some(path) = std::env::args().nth(2) {
            let worker_id = std::env::args()
                .nth(3)
                .unwrap_or_else(|| "reconciler-main".into());
            if let Err(error) = run_binance_worker(Path::new(&path), &worker_id, true) {
                eprintln!("Binance 对账失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        let local = [(1_u64, 10_i128), (2, 5)];
        let venue = [(1_u64, 10_i128), (2, 4)];
        let diffs = qx_genglu::reconcile_orders(
            &local
                .iter()
                .map(|(id, qty)| Order {
                    client_id: *id,
                    instrument: InstrumentId::parse("DEMO.SIM").unwrap(),
                    side: Side::Buy,
                    qty: Quantity::from_raw(*qty),
                    limit: None,
                    status: OrderStatus::PartiallyFilled,
                    filled: Quantity::from_raw(*qty),
                    account_id: "main".into(),
                    trace: None,
                    policy: None,
                })
                .collect::<Vec<_>>(),
            &venue,
        );
        println!("[更路 · reconcile] 差异数={} 明细={:?}", diffs.len(), diffs);
        return;
    }

    let bars = gen_bars(400, 20260910);

    // 1. 质量门
    let report = QualityGate::check(&bars);
    println!(
        "[观星 · 质量门] bars={} 判定={:?}",
        bars.len(),
        report.verdict()
    );
    assert_eq!(report.verdict(), Verdict::Ok, "合成数据不应有质量问题");

    // 2. 回测
    let a = run_backtest(&bars, 42, 5, 20);
    let b = run_backtest(&bars, 42, 5, 20); // 完全相同参数
    let c = run_backtest(&bars, 42, 6, 20); // 改一个参数

    println!(
        "\n[星板 · 回测 A] 成交={} 手续费={:.4} 总收益={:.2}% 最大回撤={:.2}% 终值={:.2}",
        a.n_fills,
        f(a.total_fee),
        f(a.total_return) * 100.0,
        f(a.max_drawdown) * 100.0,
        f(a.final_equity)
    );

    let manifest = RunManifest {
        run_id: "cli-demo".into(),
        code_commit: "workspace".into(),
        config_hash: "fast=5;slow=20;seed=42".to_string(),
        data_fingerprint: format!("synthetic:{}", bars.len()),
        clock_start: bars.first().map(|b| b.ts).unwrap_or(0),
        clock_end: bars.last().map(|b| b.ts).unwrap_or(0),
        global_seed: 42,
        determinism_mode: true,
        result_hash: format!("{:016x}", a.hash),
        strategy_version: "sma-cross-v1".into(),
        instrument_spec_version: "demo-v1".into(),
        model_fingerprint: "next-open+maker-taker".into(),
        input_event_hash: format!("bars:{}", bars.len()),
        output_event_hash: format!("{:016x}", a.hash),
        runtime_version: env!("CARGO_PKG_VERSION").into(),
    };
    println!("[更路 · RunManifest] digest={:016x}", manifest.digest());

    // 3. 三重重放校验（此处验证前两条）
    let same = ReplayVerifier::identical(a.hash, b.hash);
    let changed = ReplayVerifier::changed(a.hash, c.hash);
    println!("\n[更路 · 重放校验]");
    println!("  ① 同输入两次运行哈希一致 : {}", same);
    println!("  ② 改参数后哈希发生变化   : {}", changed);
    assert!(same, "相同输入必须产生相同结果");
    assert!(changed, "修改参数必须改变结果");

    if mode == "backtest" || mode == "verify" {
        if mode == "verify" {
            assert!(same && changed);
        }
        return;
    }

    // 4. 插件装配
    println!("\n[卯眼 · 插件装配]");
    let mut reg = Registry::new();
    reg.register(
        Manifest {
            id: "sys.simulation".into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_MATCHER.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: vec![],
            replaces: vec![],
            capabilities: vec!["simulation".into()],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .sign(),
    )
    .unwrap();
    reg.register(
        Manifest {
            id: "sys.transaction-cost".into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_FEE_MODEL.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: vec!["sys.simulation".into()],
            replaces: vec![],
            capabilities: vec!["fee".into()],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .sign(),
    )
    .unwrap();

    let order = reg.resolve_order().unwrap();
    println!("  插件数={} 加载顺序={:?}", reg.len(), order);
    println!("  独占冲突={:?}", reg.conflicts());

    println!("\n全部自校验通过 ✓");
    run_paper_smoke();
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_control::{CommandKind, CommandStatus};
    use qx_execution::execute_paper_submit_effect;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn runtime_relative_paths_resolve_from_runtime_config_directory() {
        assert_eq!(
            resolve_runtime_relative_path(
                Path::new("deploy/qianxing.runtime.production.example.json"),
                "qianxing.binance.spot.spec.json",
            ),
            PathBuf::from("deploy/qianxing.binance.spot.spec.json")
        );
        assert_eq!(
            resolve_runtime_relative_path(
                Path::new("deploy/runtime.json"),
                "C:/absolute/spec.json",
            ),
            PathBuf::from("C:/absolute/spec.json")
        );
        let mut strategy = StrategyRuntimeConfig {
            external_executable: Some("../strategy/bin/strategy.exe".into()),
            python_module: Some("../python/strategy.py".into()),
            target_snapshot_path: Some("../research/target.json".into()),
            research_snapshot_path: Some("../research/bundle.json".into()),
            ..StrategyRuntimeConfig::default()
        };
        resolve_strategy_runtime_paths(&mut strategy, Path::new("deploy/runtime.json"));
        assert_eq!(
            PathBuf::from(strategy.external_executable.unwrap()),
            resolve_runtime_relative_path(
                Path::new("deploy/runtime.json"),
                "../strategy/bin/strategy.exe"
            )
        );
        assert_eq!(
            PathBuf::from(strategy.python_module.unwrap()),
            resolve_runtime_relative_path(
                Path::new("deploy/runtime.json"),
                "../python/strategy.py"
            )
        );
        assert_eq!(
            PathBuf::from(strategy.target_snapshot_path.unwrap()),
            resolve_runtime_relative_path(
                Path::new("deploy/runtime.json"),
                "../research/target.json"
            )
        );
        assert_eq!(
            PathBuf::from(strategy.research_snapshot_path.unwrap()),
            resolve_runtime_relative_path(
                Path::new("deploy/runtime.json"),
                "../research/bundle.json"
            )
        );
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-runtime-asset-path-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config_dir = root.join("deploy");
        let storage_dir = root.join("data");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&storage_dir).unwrap();
        let runtime_path = config_dir.join("runtime.json");
        let configured = "research/bundle.json";
        let config_asset = config_dir.join(configured);
        let storage_asset = storage_dir.join(configured);
        std::fs::create_dir_all(config_asset.parent().unwrap()).unwrap();
        std::fs::create_dir_all(storage_asset.parent().unwrap()).unwrap();
        std::fs::write(&storage_asset, "legacy").unwrap();
        assert_eq!(
            resolve_runtime_asset_path(&runtime_path, &storage_dir, configured),
            storage_asset
        );
        std::fs::write(&config_asset, "current").unwrap();
        assert_eq!(
            resolve_runtime_asset_path(&runtime_path, &storage_dir, configured),
            config_asset
        );
        let _ = std::fs::remove_dir_all(root);
        let mut worker = WorkerConfig {
            id: "execution".into(),
            role: WorkerRole::Execution,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("binance".into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: Some(qx_runtime::CredentialFiles {
                api_key: "../secrets/key".into(),
                secret: "../secrets/secret".into(),
            }),
            instrument_spec_path: Some("market.json".into()),
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        };
        resolve_worker_runtime_paths(&mut worker, Path::new("deploy/runtime.json"));
        let files = worker.credential_files.unwrap();
        assert_eq!(
            PathBuf::from(files.api_key),
            resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../secrets/key")
        );
        assert_eq!(
            PathBuf::from(files.secret),
            resolve_runtime_relative_path(Path::new("deploy/runtime.json"), "../secrets/secret")
        );
    }

    #[test]
    fn strategy_artifact_sha256_is_checked_before_spawn() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-strategy-artifact-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("strategy.bin");
        std::fs::write(&path, "artifact").unwrap();
        let mut strategy = StrategyRuntimeConfig {
            external_executable: Some(path.to_string_lossy().into_owned()),
            strategy_artifact_sha256: Some(
                "c7c5c1d70c5dec4416ab6158afd0b223ef40c29b1dc1f97ed9428b94d4cadb1c".into(),
            ),
            ..StrategyRuntimeConfig::default()
        };
        assert!(verify_strategy_artifact(&strategy).is_ok());
        std::fs::write(&path, "tampered").unwrap();
        assert!(verify_strategy_artifact(&strategy).is_err());
        strategy.strategy_artifact_sha256 = Some("not-a-digest".into());
        assert!(verify_strategy_artifact(&strategy).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "nats")]
    #[test]
    fn worker_metrics_are_atomic_and_aggregated_deterministically() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-worker-metrics-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("worker-metrics").join("a.prom");
        let second = root.join("worker-metrics").join("b.prom");
        write_worker_metrics(
            &second,
            "qx_worker_up{worker=\"b\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"b\"} 2\n",
        )
        .unwrap();
        write_worker_metrics(
            &first,
            "qx_worker_up{worker=\"a\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"a\"} 2\n",
        )
        .unwrap();
        let body = read_worker_metrics(&root.join("worker-metrics"), 2_000, 30_000);
        assert_eq!(
            body,
            "qx_worker_up{worker=\"a\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"a\"} 2\nqx_worker_up{worker=\"b\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"b\"} 2\n"
        );
        assert!(!root.join("worker-metrics").join("a.prom.tmp").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "nats")]
    #[test]
    fn stale_worker_metrics_are_exposed_as_down() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-worker-metrics-stale-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("worker-metrics").join("relay.prom");
        write_worker_metrics(
            &path,
            "qx_worker_up{worker=\"relay\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"relay\"} 1\n",
        )
        .unwrap();
        let body = read_worker_metrics(&root.join("worker-metrics"), 40_000, 30_000);
        assert!(body.contains("qx_worker_up{worker=\"relay\"} 0"));
        assert!(body.contains("qx_worker_metrics_stale{worker=\"relay\"} 1"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unhealthy_worker_metrics_block_readiness() {
        let healthy =
            "qx_worker_up{worker=\"relay\"} 1\nqx_worker_heartbeat_timestamp_seconds{worker=\"relay\"} 2\n";
        let down =
            "qx_worker_up{worker=\"relay\"} 0\nqx_worker_heartbeat_timestamp_seconds{worker=\"relay\"} 2\n";
        assert!(!worker_metrics_unhealthy(healthy, 2_000, 30_000));
        assert!(worker_metrics_unhealthy(down, 2_000, 30_000));
        assert!(worker_metrics_unhealthy(healthy, 40_000, 30_000));
    }

    #[test]
    fn production_readiness_requires_private_worker_assets() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-readiness-assets-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let key = root.join("key");
        let secret = root.join("secret");
        let spec = root.join("spec.json");
        std::fs::write(&key, "key").unwrap();
        std::fs::write(&secret, "secret").unwrap();
        std::fs::write(&spec, "{}").unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.production.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        for worker in &mut config.workers {
            if matches!(worker.role, WorkerRole::UserStream | WorkerRole::Reconciler) {
                worker.enabled = false;
            }
        }
        let execution = config
            .workers
            .iter_mut()
            .find(|worker| worker.role == WorkerRole::Execution)
            .unwrap();
        execution.credential_env = None;
        execution.credential_files = Some(qx_runtime::CredentialFiles {
            api_key: key.to_string_lossy().into_owned(),
            secret: secret.to_string_lossy().into_owned(),
        });
        execution.instrument_spec_path = Some(spec.to_string_lossy().into_owned());
        let config_path = root.join("runtime.json");
        assert!(production_trading_assets_ready(&config, &config_path));
        std::fs::remove_file(&secret).unwrap();
        assert!(!production_trading_assets_ready(&config, &config_path));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "nats")]
    #[test]
    fn prometheus_labels_escape_control_characters() {
        assert_eq!(prometheus_label("a\\b\"c\nd\re"), "a\\\\b\\\"c\\nd\\re");
    }

    #[test]
    fn strategy_child_environment_rejects_credentials_and_keeps_runtime_allowlist() {
        let safe = BTreeMap::from([("QX_MODE".to_string(), "paper".to_string())]);
        let child = strategy_child_environment(&safe).unwrap();
        assert_eq!(child.get("QX_MODE"), Some(&"paper".to_string()));
        assert!(!child.contains_key("QX_API_KEY"));
        let secret = BTreeMap::from([("EXCHANGE_SECRET".to_string(), "x".to_string())]);
        assert!(strategy_child_environment(&secret).is_err());
    }

    #[test]
    fn ccxt_worker_rejects_exchange_config_mismatch() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-ccxt-binding-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("ccxt.json");
        std::fs::write(&config_path, r#"{"exchange_id":"okx"}"#).unwrap();
        let worker = WorkerConfig {
            id: "ccxt-binance".into(),
            role: WorkerRole::Execution,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("binance".into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        };
        let error = validate_ccxt_worker_binding(&worker, &config_path).unwrap_err();
        assert!(error.contains("不一致"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_initial_cash_is_idempotent_and_replayed_into_ledger() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-paper-cash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut pipeline = LiveEventPipeline::open(&root, "paper-events", "USDT").unwrap();
        let worker = WorkerConfig {
            id: "paper-execution".into(),
            role: WorkerRole::Execution,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("paper".into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: Some("USDT".into()),
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: Some(1_000),
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        };
        seed_paper_initial_cash(&mut pipeline, &worker, 10).unwrap();
        seed_paper_initial_cash(&mut pipeline, &worker, 11).unwrap();
        assert_eq!(pipeline.ledger().cash_for("main", "USDT"), 1_000);
        assert_eq!(pipeline.log().events().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ccxt_cashflow_facts_map_signed_bills_and_stable_external_ids() {
        let value = serde_json::json!({
            "cashflows": [
                {
                    "external_id": "fund-1",
                    "currency": "usdt",
                    "kind": "funding",
                    "amount_raw": -120000000_i64,
                    "timestamp_ms": 5000
                },
                {
                    "external_id": "interest-1",
                    "currency": "USDT",
                    "kind": "interest",
                    "amount_raw": 30000000_i64,
                    "timestamp_ms": 5100
                }
            ]
        });
        let facts = ccxt_cashflow_facts(&value, "main", "OKX").unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].0.kind, CashflowKind::Funding);
        assert_eq!(facts[0].0.currency, "USDT");
        assert_eq!(facts[0].0.amount, Money::from_raw(-120000000));
        assert_eq!(facts[1].0.kind, CashflowKind::Interest);
        assert_eq!(facts[1].1, 5100);
    }

    #[test]
    fn ccxt_market_tiers_drive_backtest_margin_rule() {
        let market = serde_json::json!({
            "leverage_tiers": [{
                "max_notional_raw": 100000000000000_i64,
                "initial_margin_bps": 2000,
                "maintenance_margin_bps": 1000,
                "max_leverage": 5
            }]
        });
        let rule = ccxt_margin_rule_from_market(&market);
        assert_eq!(rule.initial_margin(100_000), 20_000);
        assert_eq!(rule.maintain_margin(100_000), 10_000);
    }

    #[test]
    fn supervisor_maps_paper_topology_and_rejects_unknown_venue_without_opt_in() {
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json");
        let config = read_runtime_config(&template).unwrap();
        let launches = managed_worker_args(&config, Path::new("runtime.json"), false).unwrap();
        assert_eq!(launches.len(), 4);
        assert!(launches
            .iter()
            .any(|(id, args)| id == "paper-execution" && args[0] == "paper-worker"));

        let mut unsupported = config;
        unsupported
            .workers
            .iter_mut()
            .find(|worker| worker.id == "paper-execution")
            .unwrap()
            .venue_id = Some("unmanaged-venue".into());
        assert!(managed_worker_args(&unsupported, Path::new("runtime.json"), false).is_err());
        assert!(managed_worker_args(&unsupported, Path::new("runtime.json"), true).is_ok());
    }

    #[test]
    fn supervisor_routes_endpoint_backed_execution_to_public_ccxt() {
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.ccxt.example.json");
        let config = read_runtime_config(&template).unwrap();
        let launches = managed_worker_args(&config, &template, false).unwrap();
        assert_eq!(launches.len(), 6);
        assert!(launches
            .iter()
            .any(|(id, args)| id == "ccxt-market-main" && args[0] == "ccxt-worker"));
        assert!(launches
            .iter()
            .any(|(id, args)| id == "ccxt-reconciler-main" && args[0] == "ccxt-worker"));
        let (_, args) = launches
            .iter()
            .find(|(id, _)| id == "ccxt-execution-main")
            .unwrap();
        assert_eq!(args[0], "ccxt-worker");
        assert_eq!(args[1], template.to_string_lossy());
        assert_eq!(args[2], "ccxt-execution-main");
        assert_eq!(
            args[3],
            template
                .parent()
                .unwrap()
                .join("qianxing.ccxt.exchange.example.json")
                .to_string_lossy()
        );
    }

    #[test]
    fn ccxt_barframe_snapshot_runs_through_rust_backtest_engine() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-ccxt-backtest-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let frame = BarFrame {
            instrument: InstrumentId::parse("BTC/USDT.OKX").unwrap(),
            source: DataSourceId::new("ccxt:test"),
            ts: (1..=30).collect(),
            open_raw: (1..=30).map(|value| value * 1_000_000_000).collect(),
            high_raw: (1..=30).map(|value| value * 1_000_000_000 + 1).collect(),
            low_raw: (1..=30).map(|value| value * 1_000_000_000 - 1).collect(),
            close_raw: (1..=30).map(|value| value * 1_000_000_000).collect(),
            volume_raw: vec![1_000_000_000; 30],
        };
        let path = root.join("bars.json");
        std::fs::write(&path, frame.to_json()).unwrap();
        run_ccxt_backtest(&path, 5, 20, None).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ccxt_derivative_snapshots_map_to_signed_positions_and_funding_facts() {
        let response = serde_json::json!({
            "positions": [{
                "symbol": "BTC/USDT:USDT",
                "side": "short",
                "contracts_raw": "2000000000",
                "entry_price_raw": 100000000000_i64,
                "mark_price_raw": 99000000000_i64,
                "unrealized_pnl_raw": 2000000000,
                "initial_margin_raw": 10000000000_i64,
                "maintenance_margin_raw": 5000000000_i64,
                "leverage": 10,
                "margin_mode": "isolated"
            }]
        });
        let positions = ccxt_position_facts(&response, "okx").unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity.raw(), -2_000_000_000);
        assert_eq!(positions[0].leverage, Some(10));

        let funding = serde_json::json!({
            "funding": {
                "symbol": "BTC/USDT:USDT",
                "timestamp_ms": 100,
                "funding_rate_bps": 3,
                "next_funding_timestamp_ms": 200
            }
        });
        let (snapshot, ts) = ccxt_funding_fact(&funding, "okx").unwrap();
        assert_eq!(snapshot.funding_rate_bps, 3);
        assert_eq!(snapshot.next_funding_timestamp_ms, Some(200));
        assert_eq!(ts, 100);
    }

    #[test]
    fn dry_run_submit_order_is_audited_without_credentials_or_network() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-submit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&root).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.production.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.config_fingerprint = None;
        config.environment = "test".into();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        config.storage.event_log_segment_events = Some(2);
        config.storage.backend = StorageBackend::Files;
        config.storage.sqlite_path = None;
        let config_path = root.join("runtime.json");
        std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

        let order = mk_order(
            7001,
            &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            Side::Buy,
            1,
        );
        let mut payload = BTreeMap::new();
        payload.insert("order_json".into(), serde_json::to_string(&order).unwrap());
        let command = ControlCommand {
            command_id: 7001,
            request_id: "submit-7001".into(),
            operator_id: "ops".into(),
            reason: "dry run integration".into(),
            kind: CommandKind::SubmitOrder,
            target: "7001".into(),
            payload,
            permission: Permission::Trading,
            dry_run: true,
        };
        let command_path = root.join("command.json");
        std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

        run_binance_submit_order(&config_path, "binance-user-main", &command_path).unwrap();
        let state = load_control_state(&data_dir).unwrap();
        assert_eq!(state.audit().len(), 2);
        assert_eq!(state.audit()[0].status, CommandStatus::Accepted);
        assert_eq!(state.audit()[1].status, CommandStatus::Executed);
        assert!(state.pending().next().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_submit_order_runs_queue_pipeline_ledger_and_ack() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-paper-submit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&root).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        let config_path = root.join("runtime.json");
        std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
        let order = mk_order(
            8001,
            &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            Side::Buy,
            1,
        );
        let command = ControlCommand {
            command_id: 8001,
            request_id: "paper-submit-8001".into(),
            operator_id: "paper".into(),
            reason: "paper execution integration".into(),
            kind: CommandKind::SubmitOrder,
            target: "8001".into(),
            payload: BTreeMap::from([(
                "order_json".into(),
                serde_json::to_string(&order).unwrap(),
            )]),
            permission: Permission::Trading,
            dry_run: false,
        };
        let command_path = root.join("command.json");
        std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();
        run_paper_submit_order(&config_path, &command_path).unwrap();
        let state = load_control_state(&data_dir).unwrap();
        assert_eq!(state.audit().len(), 2);
        let pipeline = LiveEventPipeline::open(&data_dir, "paper-events", "USDT").unwrap();
        assert_eq!(pipeline.ledger().entries().len(), 2);
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert!(
            qx_storage::ControlCommandQueue::new(data_dir.join("control-queue"))
                .pending()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn strategy_worker_executes_pause_command_through_persistent_queue() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-strategy-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&root).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        let config_path = root.join("runtime.json");
        std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

        let command = ControlCommand {
            command_id: 9001,
            request_id: "pause-strategy-9001".into(),
            operator_id: "ops".into(),
            reason: "strategy control integration".into(),
            kind: CommandKind::PauseStrategy,
            target: "strategy-paper".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: false,
        };
        let store = JsonStateStore::new(&data_dir);
        let (_, accepted) = store
            .transact_control(|plane| plane.submit_as(command.clone(), Permission::Trading, 1))
            .unwrap();
        assert!(accepted.is_ok());
        ControlCommandQueue::new(data_dir.join("control-queue"))
            .enqueue(command, 1)
            .unwrap();

        run_strategy_worker(&config_path, "strategy-paper", true).unwrap();
        let state = load_control_state(&data_dir).unwrap();
        assert_eq!(state.audit().len(), 2);
        assert_eq!(state.audit()[1].status, CommandStatus::Executed);
        assert_eq!(state.audit()[1].result_code, "STRATEGY_PAUSED");
        assert!(ControlCommandQueue::new(data_dir.join("control-queue"))
            .pending()
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn strategy_signal_portfolio_risk_emits_idempotent_submit_order() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-strategy-order-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&root).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("binance-testnet".into());
        config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
        config.strategy.target_qty = 1;
        let order = build_strategy_order(
            &config,
            "strategy-paper",
            9101,
            10,
            0,
            config.strategy.target_qty,
        )
        .unwrap()
        .unwrap();
        let command = strategy_submit_command("strategy-paper", &order, false).unwrap();
        let store = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
        let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
        let result = persist_strategy_submit(&store, &queue, &command, 10).unwrap();
        assert_eq!(result, "ORDER_INTENT_ACCEPTED");
        let retry = persist_strategy_submit(&store, &queue, &command, 11).unwrap();
        assert_eq!(retry, "ORDER_INTENT_ALREADY_ACCEPTED");
        let state = load_control_state(&data_dir).unwrap();
        assert_eq!(state.audit().len(), 1);
        assert_eq!(queue.pending().unwrap().len(), 1);
        let queued = queue.pending().unwrap().pop().unwrap();
        assert_eq!(queued.command.command_id, 9101);
        assert!(queued.command.payload.contains_key("order_json"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn strategy_target_zero_emits_close_order_for_existing_position() {
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("binance-testnet".into());
        config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
        config.strategy.target_qty = 0;

        let order = build_strategy_order(&config, "strategy-close", 9102, 10, 2, 0)
            .unwrap()
            .expect("target zero must close an existing position");
        assert_eq!(order.side, Side::Sell);
        assert_eq!(order.qty.raw(), 2);
    }

    #[test]
    fn strategy_worker_reads_candidate_factor_bundle_as_context() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-research-context-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        config.strategy.target_qty = 0;
        config.strategy.target_snapshot_path = None;
        config.strategy.research_snapshot_path =
            Some(root.join("research.json").to_string_lossy().into_owned());
        config.strategy.research_data_fingerprint = Some("bars-1".into());
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let mut catalog = FactorCatalog::default();
        catalog
            .register_definition(FeatureDefinition {
                name: "momentum".into(),
                version: "v1".into(),
                formula: "close / close[-20] - 1".into(),
                input_fields: vec!["close".into()],
                dependencies: Vec::new(),
                point_in_time: true,
            })
            .unwrap();
        let artifact = FeatureArtifact {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-1".into(),
            as_of: 20,
            coverage_bps: 10_000,
            values: [(instrument.clone(), 100)].into_iter().collect(),
        };
        let report = qx_factor::FactorReport {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-1".into(),
            observation_hash: 1,
            analysis_start: 1,
            analysis_end: 20,
            sample_count: 1,
            coverage_bps: 10_000,
            ic_bps: 1,
            rank_ic_bps: 1,
            turnover_bps: 1,
            transform: None,
            missing_policy: "reject".into(),
            decay_bps: 0,
            capacity_raw: 1,
            exposures: BTreeMap::new(),
        };
        catalog.publish_artifact(artifact.clone()).unwrap();
        catalog.publish_report(report.clone()).unwrap();
        let candidate = catalog
            .bind_candidate(CandidateRequest {
                strategy_version: config.strategy.version.clone(),
                universe_version: "universe-v1".into(),
                parameters: qx_guanxing::ParameterSet::default(),
                data_fingerprint: "bars-1".into(),
                factor_keys: vec!["momentum@v1".into()],
                cost_bps: 1,
                train_start: 1,
                train_end: 10,
                validation_start: 11,
                validation_end: 20,
                intended_exposure: [(instrument.clone(), 2)].into_iter().collect(),
                constraints: BTreeMap::new(),
                execution_model: "event-backtest@v1".into(),
                risk_model: "default-risk@v1".into(),
            })
            .unwrap();
        let research = StrategyResearchSnapshot {
            schema_version: StrategyResearchSnapshot::SCHEMA_VERSION,
            candidate,
            artifacts: vec![artifact],
            reports: vec![report],
            as_of: 20,
        };
        std::fs::write(
            config.strategy.research_snapshot_path.as_ref().unwrap(),
            research.to_json().unwrap(),
        )
        .unwrap();
        assert_eq!(
            strategy_target_qty(&data_dir, &config, &instrument, 21).unwrap(),
            2
        );
        config.strategy.research_data_fingerprint = Some("wrong-fingerprint".into());
        assert!(strategy_target_qty(&data_dir, &config, &instrument, 21).is_err());
        config.strategy.research_data_fingerprint = Some("bars-1".into());
        config.strategy.version = "wrong-version".into();
        assert!(strategy_target_qty(&data_dir, &config, &instrument, 21).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn derivative_strategy_emits_leveraged_short_policy_without_no_short_rule() {
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.strategy.product = Some(TradingProduct::Perpetual);
        config.strategy.margin_mode = Some(MarginMode::Isolated);
        config.strategy.position_mode = Some(PositionMode::OneWay);
        config.strategy.leverage = Some(5);
        config.strategy.allow_short = Some(true);
        config.strategy.target_qty = -1;
        config.validate().unwrap();
        let order = build_strategy_order(&config, "strategy-perp", 9301, 10, 0, -1)
            .unwrap()
            .unwrap();
        let policy = order.policy.unwrap();
        assert_eq!(policy.leverage, 5);
        assert_eq!(policy.margin_mode, MarginMode::Isolated);
        assert_eq!(policy.position_mode, PositionMode::OneWay);
        assert_eq!(policy.position_side, PositionSide::Net);
    }

    #[test]
    fn strategy_backtest_accepts_builtin_runtime_config() {
        let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy");
        let runtime = deploy.join("qianxing.runtime.builtin-strategy.example.json");
        let frame = deploy.join("qianxing.bar-frame.example.json");
        run_strategy_backtest(&runtime, &frame, None).unwrap();
    }

    #[test]
    fn builtin_strategy_worker_path_reads_bar_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-builtin-worker-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.builtin-strategy.example.json");
        let mut config = read_runtime_config(&runtime).unwrap();
        config.storage.data_dir = root.to_string_lossy().into_owned();
        resolve_strategy_runtime_paths(&mut config.strategy, &runtime);
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let output =
            invoke_builtin_strategy(&root, &config, &instrument, "builtin-worker-test", u64::MAX)
                .unwrap();
        assert_eq!(output.request_id, "builtin-worker-test");
        assert_eq!(output.strategy_id, "strategy-builtin");
        assert_eq!(output.instrument, "BTCUSDT.BINANCE");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn api_snapshot_is_rebuilt_from_persisted_account_eventlog() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-api-read-model-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = root.to_string_lossy().into_owned();
        config.storage.event_log_segment_events = Some(2);
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let order = mk_order(9401, &instrument, Side::Buy, 1);
        let mut pipeline = LiveEventPipeline::open_configured(
            &root,
            "paper-main-paper-events",
            "USDT",
            config.storage.event_log_segment_events,
        )
        .unwrap();
        pipeline.register_order(order, 1).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: 9401,
                    venue_order_id: Some("paper-9401".into()),
                },
                2,
                2,
                1,
                "api-read-model:accepted",
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill {
                    fill: qx_core::Fill {
                        order_id: 9401,
                        qty: Quantity::from_i64(1),
                        price: Price::from_i64(100),
                        ts: 3,
                        account_id: "main".into(),
                        ..qx_core::Fill::default()
                    },
                },
                3,
                3,
                2,
                "api-read-model:fill",
            ))
            .unwrap();
        let snapshot = load_api_account_snapshot(&config).unwrap().unwrap();
        assert_eq!(snapshot.header.account_id, "main");
        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(snapshot.fills.len(), 1);
        assert_eq!(snapshot.positions[&instrument].quantity_raw, SCALE);
        assert_eq!(snapshot.reconcile.recovery_state, "eventlog-replayed");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_strategy_reads_filled_position_before_emitting_next_order() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-paper-position-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        config.storage.event_log_segment_events = Some(2);

        let order = build_strategy_order(
            &config,
            "strategy-paper",
            9201,
            10,
            0,
            config.strategy.target_qty,
        )
        .unwrap()
        .unwrap();
        let command = strategy_submit_command("strategy-paper", &order, false).unwrap();
        let log_name = "paper-main-paper-events";
        let result = execute_paper_submit_effect_with_storage(
            &command,
            &data_dir,
            log_name,
            10,
            config.storage.event_log_segment_events,
            None,
            None,
        )
        .unwrap();
        assert!(result.starts_with("PAPER_EXECUTED fills=1"));

        let current_qty = strategy_current_qty(&data_dir, &config).unwrap();
        assert_eq!(current_qty, config.strategy.target_qty);
        assert!(build_strategy_order(
            &config,
            "strategy-paper",
            9202,
            11,
            current_qty,
            config.strategy.target_qty,
        )
        .unwrap()
        .is_none());

        let pipeline = LiveEventPipeline::open_configured(
            &data_dir,
            log_name,
            "USDT",
            config.storage.event_log_segment_events,
        )
        .unwrap();
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert_eq!(pipeline.ledger().entries().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_worker_cleans_stale_queue_after_terminal_commit() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-paper-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        let config_path = root.join("runtime.json");
        std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

        let order = build_strategy_order(
            &config,
            "strategy-paper",
            9301,
            10,
            0,
            config.strategy.target_qty,
        )
        .unwrap()
        .unwrap();
        let command = strategy_submit_command("strategy-paper", &order, false).unwrap();
        let store = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
        let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
        store
            .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
            .unwrap()
            .1
            .unwrap();
        queue.enqueue(command.clone(), 10).unwrap();

        let log_name = "paper-main-paper-events";
        execute_paper_submit_effect(&command, &data_dir, log_name, 10).unwrap();
        store
            .transact(|plane| {
                plane.execute(command.command_id, 11, |_| Ok("PAPER_EXECUTED".into()))
            })
            .unwrap()
            .1
            .unwrap();

        run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();
        assert!(queue.pending().unwrap().is_empty());
        let pipeline = LiveEventPipeline::open(&data_dir, log_name, "USDT").unwrap();
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert_eq!(pipeline.ledger().entries().len(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-paper-e2e-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data_dir = root.join("data");
        std::fs::create_dir_all(&root).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json");
        let mut config = read_runtime_config(&template).unwrap();
        config.storage.data_dir = data_dir.to_string_lossy().into_owned();
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        config.scheduler.jobs_path = workspace_root
            .join("deploy")
            .join("qianxing.scheduler.paper-order-smoke.json")
            .to_string_lossy()
            .into_owned();
        config.strategy.target_snapshot_path = Some(
            workspace_root
                .join("deploy")
                .join("qianxing.strategy-target.paper.json")
                .to_string_lossy()
                .into_owned(),
        );
        config.strategy.target_qty = 0;
        let config_path = root.join("runtime.json");
        std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

        run_paper_pipeline_once(&config_path).unwrap();
        run_paper_pipeline_once(&config_path).unwrap();
        let pipeline =
            LiveEventPipeline::open(&data_dir, "paper-main-paper-events", "USDT").unwrap();
        assert_eq!(pipeline.orders().len(), 1);
        assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
        assert_eq!(pipeline.ledger().entries().len(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconcile_report_persists_structured_balance_discrepancy() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-cli-reconcile-report-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let discrepancy = RuntimeBalanceDiscrepancy {
            account_id: "main".into(),
            venue_id: "binance".into(),
            asset: "USDT".into(),
            ledger_raw: 0,
            venue_raw: Money::from_i64(10).raw(),
        };
        persist_reconcile_report(ReconcileReportInput {
            pipeline_root: &root,
            worker_id: "reconciler-main",
            account_id: "main",
            venue_id: "binance",
            observed_ts: 42,
            issues: &[],
            balances_count: 1,
            balance_discrepancies: &[discrepancy],
            position_snapshots_count: 0,
            funding_rate_snapshots_count: 0,
            cashflow_count: 0,
        })
        .unwrap();
        let report: serde_json::Value = JsonStateStore::new(&root)
            .load_json_at("reconcile/reconciler-main.json")
            .unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["balance_discrepancies"][0]["asset"], "USDT");
        assert_eq!(
            report["balance_discrepancies"][0]["venue_raw"],
            10_000_000_000_i64
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rust_invokes_python_strategy_jsonl_worker_through_versioned_contract() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let module = workspace_root
            .join("python")
            .join("tests")
            .join("fixtures")
            .join("strategy_target.py");
        let input = StrategyContractInput {
            schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: "request-python-1".into(),
            strategy_id: "strategy-python".into(),
            strategy_version: "v1".into(),
            data_fingerprint: "bars-sha256".into(),
            as_of: 1_700_000_000,
            instrument: "BTC/USDT.OKX".into(),
            positions: BTreeMap::new(),
            cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
            available_margin_raw: Some(100_000_000),
            risk_state: "ready".into(),
            research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
            bars: None,
        };
        let output = invoke_python_strategy(module.to_string_lossy().as_ref(), &input).unwrap();
        assert_eq!(output.target_qty, 3);
        assert_eq!(output.signal_id, 7);
        assert_eq!(output.confidence, 800);
        assert_eq!(output.priority, 2);
        assert_eq!(output.request_id, input.request_id);
        let artifact_sha256 = qx_strategy::sha256_hex(&std::fs::read(&module).unwrap());
        let mut client = PythonStrategyClient::start_with_transport_config(
            module.to_string_lossy().as_ref(),
            PYTHON_STRATEGY_TIMEOUT_MS,
            StrategyTransport::Jsonl,
            SharedRingConfig::default(),
            Some(&artifact_sha256),
        )
        .unwrap();
        let first = client.request(&input).unwrap();
        let second = client.request(&input).unwrap();
        assert_eq!(first.signal_id, second.signal_id);
        assert_eq!(first.target_qty, 3);
    }

    #[test]
    fn rust_invokes_python_multi_intent_strategy_contract() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let module = workspace_root
            .join("python")
            .join("tests")
            .join("fixtures")
            .join("strategy_multi.py");
        let input = StrategyContractInput {
            schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: "request-python-multi".into(),
            strategy_id: "strategy-python-multi".into(),
            strategy_version: "v1".into(),
            data_fingerprint: "bars-sha256".into(),
            as_of: 1_700_000_000,
            instrument: "BTCUSDT.BINANCE".into(),
            positions: BTreeMap::new(),
            cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
            available_margin_raw: Some(100_000_000),
            risk_state: "ready".into(),
            research_targets: BTreeMap::new(),
            bars: None,
        };
        let output = invoke_python_strategy(module.to_string_lossy().as_ref(), &input).unwrap();
        assert_eq!(output.intents.len(), 2);
        assert_eq!(output.intents[0].side, "buy");
        assert_eq!(output.intents[1].intent_id, 802);
    }
}
