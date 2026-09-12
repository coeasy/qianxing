//! 牵星运行时装配边界。
//!
//! 该 crate 不拥有交易事实，也不替代 Kernel；它只负责把 API、行情、用户流、
//! 调度和对账 worker 的配置校验成一个可审计的进程拓扑，并提供确定性的健康状态
//! 与停机信号。具体 worker 通过此边界注入，不允许把凭证或可变账户状态写入配置摘要。

use qx_control::Permission;
use qx_core::{InstrumentId, MarginMode, PositionMode, TradingProduct};
use qx_storage::JsonStateStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

mod pipeline;

pub use pipeline::{
    order_from_submit_command, pipeline_path, LiveEventPipeline, LivePipelineSnapshot,
    PipelineMetricsSnapshot, RuntimeBalanceDiscrepancy, RuntimeEventEnvelope, RuntimeExternalEvent,
    RuntimeIngestReceipt,
};

pub const STRATEGY_CONTRACT_SCHEMA_VERSION: u32 = qx_strategy::STRATEGY_API_VERSION;

/// Python/其他语言策略进程看到的稳定、只读 JSON 输入。
///
/// 该对象不暴露 Rust 内部结构体或可变句柄；所有数量、金额保持定点整数，
/// instrument 使用字符串，便于 Python/Arrow/其他语言无损解析。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContractInput {
    pub schema_version: u32,
    pub request_id: String,
    pub strategy_id: String,
    pub strategy_version: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub instrument: String,
    pub positions: BTreeMap<String, i128>,
    pub cash: BTreeMap<String, i128>,
    pub available_margin_raw: Option<i128>,
    pub risk_state: String,
    /// 已经通过 PIT/研究快照校验的目标映射；Python 策略不得自行读取研究文件。
    pub research_targets: BTreeMap<String, i128>,
    #[serde(default)]
    pub bars: Option<StrategyContractBars>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContractBars {
    pub source: String,
    pub ts: Vec<u64>,
    pub open_raw: Vec<i128>,
    pub high_raw: Vec<i128>,
    pub low_raw: Vec<i128>,
    pub close_raw: Vec<i128>,
    pub volume_raw: Vec<i128>,
}

/// Columnar request payload used inside `shared_memory_columnar`.
///
/// The control/identity fields remain canonical JSON so all language bindings
/// share one schema. Bar history is appended as fixed-width little-endian
/// columns, avoiding per-value JSON number parsing on the hot path.
pub const STRATEGY_COLUMNAR_MAGIC: [u8; 4] = *b"QXCB";
pub const STRATEGY_COLUMNAR_VERSION: u16 = 1;
pub const STRATEGY_COLUMNAR_HEADER_LEN: usize = 16;

pub fn encode_strategy_columnar_input(input: &StrategyContractInput) -> Result<Vec<u8>, String> {
    input.validate()?;
    let bars = input
        .bars
        .as_ref()
        .ok_or_else(|| "shared_memory_columnar 要求 StrategyContractInput 包含 bars".to_string())?;
    let rows = bars.ts.len();
    let mut metadata = input.clone();
    metadata.bars = None;
    let mut metadata_value = serde_json::to_value(&metadata)
        .map_err(|error| format!("策略列式输入元数据编码失败: {error}"))?;
    metadata_value["__qx_bars_source"] = serde_json::Value::String(bars.source.clone());
    let metadata = serde_json::to_vec(&metadata_value)
        .map_err(|error| format!("策略列式输入元数据编码失败: {error}"))?;
    let column_bytes = rows
        .checked_mul(8 + 5 * 16)
        .ok_or_else(|| "策略列式输入长度溢出".to_string())?;
    let total = STRATEGY_COLUMNAR_HEADER_LEN
        .checked_add(metadata.len())
        .and_then(|value| value.checked_add(column_bytes))
        .ok_or_else(|| "策略列式输入总长度溢出".to_string())?;
    if total > qx_strategy::DEFAULT_MAX_FRAME_BYTES {
        return Err(format!(
            "策略列式输入超过大小上限: bytes={total} max={}",
            qx_strategy::DEFAULT_MAX_FRAME_BYTES
        ));
    }
    if metadata.len() > u32::MAX as usize || rows > u32::MAX as usize {
        return Err("策略列式输入元数据或行数超过 u32 上限".into());
    }
    let mut encoded = Vec::with_capacity(total);
    encoded.extend_from_slice(&STRATEGY_COLUMNAR_MAGIC);
    encoded.extend_from_slice(&STRATEGY_COLUMNAR_VERSION.to_le_bytes());
    encoded.extend_from_slice(&0_u16.to_le_bytes());
    encoded.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    encoded.extend_from_slice(&(rows as u32).to_le_bytes());
    encoded.extend_from_slice(&metadata);
    for value in &bars.ts {
        encoded.extend_from_slice(&value.to_le_bytes());
    }
    for column in [
        &bars.open_raw,
        &bars.high_raw,
        &bars.low_raw,
        &bars.close_raw,
        &bars.volume_raw,
    ] {
        for value in column {
            encoded.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(encoded)
}

/// 跨语言策略可以直接返回的订单意图。
///
/// 旧版策略仍然可以只返回 `target_qty`，运行时会继续走目标仓位再平衡；
/// 新版 Rust/C++/Python 策略可以返回多个 intent，表达组合、做市和多腿
/// 订单。数量、价格始终使用核心定点 raw 单位，最终仍必须经过 Risk/OMS。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContractIntent {
    pub intent_id: u64,
    pub instrument: String,
    /// `buy` 或 `sell`，避免不同语言对 Rust enum 名称的依赖。
    pub side: String,
    pub qty_raw: i128,
    #[serde(default)]
    pub limit_price_raw: Option<i128>,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub post_only: bool,
    #[serde(default)]
    pub position_side: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContractOutput {
    pub schema_version: u32,
    pub request_id: String,
    pub strategy_id: String,
    pub signal_id: u64,
    pub instrument: String,
    pub target_qty: i128,
    pub confidence: i128,
    pub priority: i32,
    pub expires_at: u64,
    /// 新版策略输出；为空时兼容旧版 `target_qty` 语义。
    #[serde(default)]
    pub intents: Vec<StrategyContractIntent>,
}

impl StrategyContractBars {
    pub fn validate(&self) -> Result<(), String> {
        if self.source.trim().is_empty() || self.ts.is_empty() {
            return Err("策略 Bar 契约缺少 source 或数据".into());
        }
        let length = self.ts.len();
        if [
            self.open_raw.len(),
            self.high_raw.len(),
            self.low_raw.len(),
            self.close_raw.len(),
            self.volume_raw.len(),
        ]
        .into_iter()
        .any(|value| value != length)
        {
            return Err("策略 Bar 契约列长度不一致".into());
        }
        if self.ts.windows(2).any(|window| window[0] >= window[1]) {
            return Err("策略 Bar 契约时间戳必须严格递增".into());
        }
        Ok(())
    }
}

impl StrategyContractInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_CONTRACT_SCHEMA_VERSION
            || self.request_id.trim().is_empty()
            || self.strategy_id.trim().is_empty()
            || self.strategy_version.trim().is_empty()
            || self.data_fingerprint.trim().is_empty()
            || self.as_of == 0
            || self.risk_state.trim().is_empty()
            || self.available_margin_raw.is_some_and(|value| value < 0)
            || InstrumentId::parse(&self.instrument).is_none()
        {
            return Err("StrategyContractInput 身份、时间、标的或风险字段非法".into());
        }
        for key in self.positions.keys().chain(self.research_targets.keys()) {
            if InstrumentId::parse(key).is_none() {
                return Err(format!("StrategyContractInput instrument 非法: {key}"));
            }
        }
        if let Some(bars) = &self.bars {
            bars.validate()?;
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| format!("策略输入契约序列化失败: {error}"))
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let value: Self = serde_json::from_str(input)
            .map_err(|error| format!("策略输入契约 JSON 无效: {error}"))?;
        value.validate()?;
        Ok(value)
    }
}

impl StrategyContractOutput {
    /// 将原生 Rust Strategy SDK 的类型投影到跨进程 JSON 契约，保证 Rust
    /// 原生策略和 Python/C++ 策略最终走同一条 Strategy worker 归约路径。
    pub fn from_native_decision(decision: &qx_strategy::StrategyDecision) -> Result<Self, String> {
        let intents: Vec<StrategyContractIntent> = decision
            .intents
            .iter()
            .map(|intent| StrategyContractIntent {
                intent_id: intent.intent_id,
                instrument: intent.instrument.to_string(),
                side: match intent.side {
                    qx_core::Side::Buy => "buy".into(),
                    qx_core::Side::Sell => "sell".into(),
                },
                qty_raw: intent.qty.raw(),
                limit_price_raw: intent.limit.map(|price| price.raw()),
                reduce_only: intent.reduce_only,
                post_only: intent.post_only,
                position_side: intent
                    .policy
                    .map(|policy| format!("{:?}", policy.position_side).to_ascii_lowercase()),
            })
            .collect();
        let output = Self {
            schema_version: decision.schema_version,
            request_id: decision.request_id.clone(),
            strategy_id: decision.strategy_id.clone(),
            signal_id: decision.signal_id,
            instrument: intents
                .first()
                .map(|intent| intent.instrument.clone())
                .unwrap_or_default(),
            target_qty: 0,
            confidence: decision.confidence,
            priority: decision.priority,
            expires_at: decision.expires_at,
            intents,
        };
        if output.schema_version != STRATEGY_CONTRACT_SCHEMA_VERSION {
            return Err("原生 StrategyDecision schema_version 不匹配".into());
        }
        Ok(output)
    }

    pub fn validate_for(&self, input: &StrategyContractInput) -> Result<(), String> {
        if self.schema_version != STRATEGY_CONTRACT_SCHEMA_VERSION
            || self.request_id != input.request_id
            || self.strategy_id != input.strategy_id
            || self.signal_id == 0
            || self.instrument != input.instrument
            || InstrumentId::parse(&self.instrument).is_none()
            || (self.expires_at != 0 && self.expires_at < input.as_of)
        {
            return Err("StrategyContractOutput 与输入身份、标的或有效期不一致".into());
        }
        let mut intent_ids = BTreeSet::new();
        for intent in &self.intents {
            if intent.intent_id == 0
                || !intent_ids.insert(intent.intent_id)
                || InstrumentId::parse(&intent.instrument).is_none()
                || intent.qty_raw <= 0
                || !matches!(intent.side.to_ascii_lowercase().as_str(), "buy" | "sell")
                || intent.limit_price_raw.is_some_and(|price| price <= 0)
                || intent.position_side.as_deref().is_some_and(|side| {
                    !matches!(side.to_ascii_lowercase().as_str(), "net" | "long" | "short")
                })
            {
                return Err("StrategyContractOutput 中存在非法或重复的 OrderIntent".into());
            }
        }
        Ok(())
    }

    pub fn to_json_for(&self, input: &StrategyContractInput) -> Result<String, String> {
        self.validate_for(input)?;
        serde_json::to_string(self).map_err(|error| format!("策略输出契约序列化失败: {error}"))
    }

    pub fn from_json_for(input: &str, request: &StrategyContractInput) -> Result<Self, String> {
        let value: Self = serde_json::from_str(input)
            .map_err(|error| format!("策略输出契约 JSON 无效: {error}"))?;
        value.validate_for(request)?;
        Ok(value)
    }
}

/// 策略在一个决策时点看到的只读输入。研究产物、账户观察和风险状态
/// 统一进入这个边界，策略函数不能自行读取文件、Venue 或 Ledger 可变引用。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyContext {
    pub strategy_id: String,
    pub strategy_version: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub research: qx_factor::StrategyResearchSnapshot,
    pub account_id: String,
    pub venue_id: String,
    pub positions: BTreeMap<String, i128>,
    pub cash: BTreeMap<String, i128>,
    pub available_margin_raw: Option<i128>,
    pub risk_state: String,
}

impl StrategyContext {
    pub fn validate(&self, now: u64, require_event_verified: bool) -> Result<(), String> {
        if self.strategy_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
            || self.risk_state.trim().is_empty()
            || self.as_of != self.research.as_of
            || self.data_fingerprint.trim().is_empty()
            || self.available_margin_raw.is_some_and(|value| value < 0)
        {
            return Err("StrategyContext 账户、风险状态或可用保证金非法".into());
        }
        self.research
            .validate_for(
                &self.strategy_version,
                &self.data_fingerprint,
                now,
                require_event_verified,
            )
            .map_err(|error| format!("StrategyContext 研究输入非法: {error:?}"))
    }

    pub fn target_for(&self, instrument: &InstrumentId) -> Option<i128> {
        self.research.target_for(instrument)
    }

    pub fn to_contract_input(
        &self,
        request_id: impl Into<String>,
        instrument: &InstrumentId,
        bars: Option<StrategyContractBars>,
    ) -> Result<StrategyContractInput, String> {
        let research_targets = self
            .research
            .candidate
            .config
            .intended_exposure
            .iter()
            .map(|(instrument, target)| (instrument.to_string(), *target))
            .collect();
        let input = StrategyContractInput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: request_id.into(),
            strategy_id: self.strategy_id.clone(),
            strategy_version: self.strategy_version.clone(),
            data_fingerprint: self.data_fingerprint.clone(),
            as_of: self.as_of,
            instrument: instrument.to_string(),
            positions: self.positions.clone(),
            cash: self.cash.clone(),
            available_margin_raw: self.available_margin_raw,
            risk_state: self.risk_state.clone(),
            research_targets,
            bars,
        };
        input.validate()?;
        Ok(input)
    }
}

/// 加载控制面状态；首次启动返回空状态，损坏的 JSON 不会被吞掉。
pub fn load_control_state(
    root: impl Into<std::path::PathBuf>,
) -> Result<qx_control::ControlPlane, String> {
    JsonStateStore::new(root)
        .load_control_if_exists()
        .map_err(|error| format!("读取控制面状态失败: {error:?}"))
        .map(|state| state.unwrap_or_default())
}

pub fn save_control_state(
    root: impl Into<std::path::PathBuf>,
    state: &qx_control::ControlPlane,
) -> Result<(), String> {
    JsonStateStore::new(root)
        .save_control(state)
        .map(|_| ())
        .map_err(|error| format!("保存控制面状态失败: {error:?}"))
}

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiTransport {
    Plaintext,
    Mtls,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TlsPaths {
    pub certificate_chain: String,
    pub private_key: String,
    pub client_ca: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OperatorConfig {
    pub permission: Permission,
    pub certificate: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ApiRuntimeConfig {
    pub bind: String,
    pub transport: ApiTransport,
    pub tls: Option<TlsPaths>,
    #[serde(default)]
    pub operators: BTreeMap<String, OperatorConfig>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    Files,
    Sqlite,
    Postgres,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StorageRuntimeConfig {
    pub backend: StorageBackend,
    pub data_dir: String,
    pub sqlite_path: Option<String>,
    /// PostgreSQL DSN 的环境变量名；不允许把带密码的 DSN 写入运行时 JSON。
    #[serde(default)]
    pub postgres_dsn_env: Option<String>,
    /// PostgreSQL 单进程连接池大小；仅 Postgres backend 使用。
    #[serde(default = "default_postgres_pool_size")]
    pub postgres_pool_size: usize,
    /// 可选的 EventLog 分段大小。未配置时使用兼容的单文件日志；配置后
    /// 运行时应通过 `LiveEventPipeline::open_configured` 打开不可变分段日志。
    #[serde(default)]
    pub event_log_segment_events: Option<usize>,
}

fn default_messaging_nats_url() -> String {
    "nats://127.0.0.1:4222".into()
}

fn default_messaging_subject_prefix() -> String {
    "qianxing".into()
}

fn default_messaging_relay_interval_ms() -> u64 {
    250
}

fn default_messaging_relay_batch_size() -> usize {
    100
}

fn default_messaging_lease_seconds() -> u64 {
    30
}

fn default_messaging_consumer_batch_size() -> usize {
    100
}

fn default_messaging_consumer_max_attempts() -> u32 {
    3
}

fn default_messaging_consumer_handler_timeout_ms() -> u64 {
    5_000
}

fn default_messaging_worker_stale_after_ms() -> u64 {
    30_000
}

/// 事件 Outbox/MQ worker 的运行参数。Stream、consumer、复制和保留策略仍由
/// NATS/部署系统创建；运行时只负责连接既有 subject 并持续执行 relay。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MessagingRuntimeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_messaging_nats_url")]
    pub nats_url: String,
    #[serde(default = "default_messaging_subject_prefix")]
    pub subject_prefix: String,
    #[serde(default = "default_messaging_relay_interval_ms")]
    pub relay_interval_ms: u64,
    #[serde(default = "default_messaging_relay_batch_size")]
    pub relay_batch_size: usize,
    #[serde(default = "default_messaging_lease_seconds")]
    pub lease_seconds: u64,
    /// 已由部署系统创建的 JetStream stream/durable consumer；只在
    /// `EventConsumer` worker 启用时必填。
    #[serde(default)]
    pub consumer_stream: Option<String>,
    #[serde(default)]
    pub consumer_name: Option<String>,
    #[serde(default)]
    pub consumer_group_id: Option<String>,
    #[serde(default = "default_messaging_consumer_batch_size")]
    pub consumer_batch_size: usize,
    #[serde(default = "default_messaging_consumer_max_attempts")]
    pub consumer_max_attempts: u32,
    /// 外部 reducer/业务服务协议：每条 Outbox envelope 以一行 JSON 写入 stdin，
    /// 退出码 0 表示成功，非 0 表示可重试失败。
    #[serde(default)]
    pub consumer_handler_executable: Option<String>,
    #[serde(default)]
    pub consumer_handler_args: Vec<String>,
    #[serde(default = "default_messaging_consumer_handler_timeout_ms")]
    pub consumer_handler_timeout_ms: u64,
    /// API 聚合 worker 指标时使用的失联判定窗口。
    #[serde(default = "default_messaging_worker_stale_after_ms")]
    pub worker_stale_after_ms: u64,
}

impl Default for MessagingRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            nats_url: default_messaging_nats_url(),
            subject_prefix: default_messaging_subject_prefix(),
            relay_interval_ms: default_messaging_relay_interval_ms(),
            relay_batch_size: default_messaging_relay_batch_size(),
            lease_seconds: default_messaging_lease_seconds(),
            consumer_stream: None,
            consumer_name: None,
            consumer_group_id: None,
            consumer_batch_size: default_messaging_consumer_batch_size(),
            consumer_max_attempts: default_messaging_consumer_max_attempts(),
            consumer_handler_executable: None,
            consumer_handler_args: Vec::new(),
            consumer_handler_timeout_ms: default_messaging_consumer_handler_timeout_ms(),
            worker_stale_after_ms: default_messaging_worker_stale_after_ms(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
    Api,
    MarketData,
    UserStream,
    Execution,
    Scheduler,
    Reconciler,
    Strategy,
    OutboxRelay,
    EventConsumer,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WorkerConfig {
    pub id: String,
    pub role: WorkerRole,
    pub enabled: bool,
    pub account_id: Option<String>,
    pub venue_id: Option<String>,
    pub endpoint: Option<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
    /// 账户主结算币种；为空时仅兼容回退到 USDT。
    #[serde(default)]
    pub settlement_currency: Option<String>,
    #[serde(default)]
    pub credential_env: Option<CredentialEnv>,
    #[serde(default)]
    pub credential_files: Option<CredentialFiles>,
    /// 可选的冻结市场规格文件。配置后，Execution worker 会在调用 Venue
    /// 前使用同一份规格执行数量、价格、杠杆和保证金预检。
    #[serde(default)]
    pub instrument_spec_path: Option<String>,
    /// Paper 虚拟账户启动时幂等注入的结算币初始资金 raw 值。
    #[serde(default)]
    pub paper_initial_cash_raw: Option<i128>,
    /// 账户级订单名义额上限，使用核心定点 raw 单位。
    #[serde(default)]
    pub max_order_notional_raw: Option<i128>,
    /// 账户级持仓名义额上限，使用核心定点 raw 单位。
    #[serde(default)]
    pub max_position_notional_raw: Option<i128>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CredentialEnv {
    pub api_key: String,
    pub secret: String,
}

/// 由 Secret Manager、CSI driver 或容器 secrets 投影的凭据文件路径。
/// 文件内容不进入运行时 JSON、健康详情或事件日志；worker 在建立新连接、
/// 新一轮对账和新订单执行前重新读取文件，以支持原子替换式轮换。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CredentialFiles {
    pub api_key: String,
    pub secret: String,
}

fn default_scheduler_state_path() -> String {
    "scheduler.json".into()
}

fn default_scheduler_jobs_path() -> String {
    "scheduler.jobs.json".into()
}

fn default_scheduler_job_queue_path() -> String {
    "job-queue".into()
}

fn default_scheduler_tick_interval_ms() -> u64 {
    1_000
}

fn default_strategy_version() -> String {
    "strategy-runtime-v1".into()
}

fn default_strategy_max_orders() -> u64 {
    1_000
}

fn default_strategy_python_timeout_ms() -> u64 {
    2_000
}

fn default_strategy_c_abi_max_library_bytes() -> u64 {
    64 * 1024 * 1024
}

fn default_postgres_pool_size() -> usize {
    4
}

fn default_strategy_shared_memory_capacity() -> u32 {
    qx_strategy::DEFAULT_RING_CAPACITY
}

fn default_strategy_shared_memory_slot_bytes() -> u32 {
    qx_strategy::DEFAULT_RING_SLOT_BYTES
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyTransport {
    #[default]
    Jsonl,
    FramedJson,
    SharedMemoryJson,
    SharedMemoryColumnar,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyTargetSnapshot {
    pub schema_version: u32,
    pub strategy_version: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    /// JSON 使用 InstrumentId 字符串作为 key，避免结构体 key 的非稳定编码。
    pub targets: BTreeMap<String, i128>,
}

impl StrategyTargetSnapshot {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(format!(
                "StrategyTargetSnapshot schema_version 必须为 {}",
                Self::SCHEMA_VERSION
            ));
        }
        if self.strategy_version.trim().is_empty() || self.data_fingerprint.trim().is_empty() {
            return Err("StrategyTargetSnapshot 缺少 strategy_version 或 data_fingerprint".into());
        }
        if self.targets.is_empty() {
            return Err("StrategyTargetSnapshot targets 不能为空".into());
        }
        for instrument in self.targets.keys() {
            InstrumentId::parse(instrument)
                .ok_or_else(|| format!("StrategyTargetSnapshot instrument 非法: {instrument}"))?;
        }
        Ok(())
    }

    /// 校验该产物是否属于当前运行中的策略版本，并且不是未来时间的结果。
    ///
    /// `as_of == 0` 表示离线构建器没有提供时间锚点，属于非法运行时输入；
    /// 这样可以避免把另一个策略版本或尚未到达的研究结果静默带入交易链路。
    pub fn validate_for(&self, strategy_version: &str, now: u64) -> Result<(), String> {
        self.validate()?;
        if strategy_version.trim().is_empty() || self.strategy_version != strategy_version {
            return Err(format!(
                "StrategyTargetSnapshot strategy_version 不匹配: snapshot={} runtime={}",
                self.strategy_version, strategy_version
            ));
        }
        if self.as_of == 0 || self.as_of > now {
            return Err(format!(
                "StrategyTargetSnapshot as_of 无效或晚于运行时: as_of={} now={}",
                self.as_of, now
            ));
        }
        Ok(())
    }

    pub fn target_for(&self, instrument: &InstrumentId) -> Option<i128> {
        self.targets.get(&instrument.to_string()).copied()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SchedulerRuntimeConfig {
    #[serde(default = "default_scheduler_state_path")]
    pub state_path: String,
    #[serde(default = "default_scheduler_jobs_path")]
    pub jobs_path: String,
    #[serde(default = "default_scheduler_job_queue_path")]
    pub job_queue_path: String,
    #[serde(default = "default_scheduler_tick_interval_ms")]
    pub tick_interval_ms: u64,
}

impl Default for SchedulerRuntimeConfig {
    fn default() -> Self {
        Self {
            state_path: default_scheduler_state_path(),
            jobs_path: default_scheduler_jobs_path(),
            job_queue_path: default_scheduler_job_queue_path(),
            tick_interval_ms: default_scheduler_tick_interval_ms(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyRuntimeConfig {
    /// 多策略运行时中用于绑定 Strategy worker 的稳定 ID；旧版单策略配置可省略。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "default_strategy_version")]
    pub version: String,
    #[serde(default = "default_strategy_max_orders")]
    pub max_orders: u64,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub venue_id: Option<String>,
    #[serde(default)]
    pub instrument: Option<String>,
    #[serde(default)]
    pub target_qty: i128,
    #[serde(default)]
    pub target_snapshot_path: Option<String>,
    /// 研究层输出的 CandidateBinding + FeatureArtifact + FactorReport 快照。
    /// 配置此字段后，Strategy 不允许退化为只读取裸目标仓位文件。
    #[serde(default)]
    pub research_snapshot_path: Option<String>,
    /// 要求策略必须由研究快照驱动；生产环境中的已绑定策略必须显式开启。
    #[serde(default)]
    pub research_snapshot_required: bool,
    /// 发布时锁定的研究数据指纹；运行时会拒绝快照与该指纹不一致。
    #[serde(default)]
    pub research_data_fingerprint: Option<String>,
    /// 策略订单的统一产品语义；省略时兼容现货 Cash/1x/NoShort。
    #[serde(default)]
    pub product: Option<TradingProduct>,
    #[serde(default)]
    pub margin_mode: Option<MarginMode>,
    #[serde(default)]
    pub position_mode: Option<PositionMode>,
    #[serde(default)]
    pub leverage: Option<u32>,
    /// None 时：衍生品默认允许双向，现货/杠杆默认禁止空头，必须显式开启。
    #[serde(default)]
    pub allow_short: Option<bool>,
    /// 可选 Python JSONL 策略模块；Strategy Worker 只通过稳定契约调用它。
    #[serde(default)]
    pub python_module: Option<String>,
    /// JSONL 保持兼容；framed_json 使用 QXSF，shared_memory_columnar 使用 QXCB 固定列。
    #[serde(default)]
    pub transport: StrategyTransport,
    /// SharedMemoryJson/SharedMemoryColumnar 的 SPSC ring 容量和固定槽位大小。
    #[serde(default = "default_strategy_shared_memory_capacity")]
    pub shared_memory_capacity: u32,
    #[serde(default = "default_strategy_shared_memory_slot_bytes")]
    pub shared_memory_slot_bytes: u32,
    /// Python 策略单次事件处理超时；超时会终止该策略 worker，避免未知状态继续下单。
    #[serde(default = "default_strategy_python_timeout_ms")]
    pub python_timeout_ms: u64,
    /// Rust/C++/其他语言策略可编译为独立进程，通过同一 JSONL 策略契约接入。
    #[serde(default)]
    pub external_executable: Option<String>,
    /// 独立策略文件或 Python `.py` 文件的发布摘要；配置后会在 worker
    /// 启动前读取文件并校验 SHA-256。C ABI 动态库使用 c_abi_sha256。
    #[serde(default)]
    pub strategy_artifact_sha256: Option<String>,
    #[serde(default)]
    pub external_args: Vec<String>,
    #[serde(default)]
    pub external_env: BTreeMap<String, String>,
    /// 受信任 C/C++ 原生策略动态库；必须同时配置 SHA-256 白名单。
    #[serde(default)]
    pub c_abi_library: Option<String>,
    #[serde(default)]
    pub c_abi_sha256: Option<String>,
    #[serde(default = "default_strategy_c_abi_max_library_bytes")]
    pub c_abi_max_library_bytes: u64,
    #[serde(default)]
    pub c_abi_ed25519_public_key: Option<String>,
    #[serde(default)]
    pub c_abi_ed25519_signature: Option<String>,
}

impl Default for StrategyRuntimeConfig {
    fn default() -> Self {
        Self {
            id: None,
            version: default_strategy_version(),
            max_orders: default_strategy_max_orders(),
            account_id: None,
            venue_id: None,
            instrument: None,
            target_qty: 0,
            target_snapshot_path: None,
            research_snapshot_path: None,
            research_snapshot_required: false,
            research_data_fingerprint: None,
            product: None,
            margin_mode: None,
            position_mode: None,
            leverage: None,
            allow_short: None,
            python_module: None,
            transport: StrategyTransport::Jsonl,
            shared_memory_capacity: default_strategy_shared_memory_capacity(),
            shared_memory_slot_bytes: default_strategy_shared_memory_slot_bytes(),
            python_timeout_ms: default_strategy_python_timeout_ms(),
            external_executable: None,
            strategy_artifact_sha256: None,
            external_args: Vec::new(),
            external_env: BTreeMap::new(),
            c_abi_library: None,
            c_abi_sha256: None,
            c_abi_max_library_bytes: default_strategy_c_abi_max_library_bytes(),
            c_abi_ed25519_public_key: None,
            c_abi_ed25519_signature: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub schema_version: u32,
    pub environment: String,
    /// 发布后可选的配置锁指纹。计算时排除本字段本身，避免修改其他配置
    /// 后通过同步修改 fingerprint 绕过启动校验。
    #[serde(default)]
    pub config_fingerprint: Option<String>,
    pub api: ApiRuntimeConfig,
    pub storage: StorageRuntimeConfig,
    #[serde(default)]
    pub messaging: MessagingRuntimeConfig,
    pub workers: Vec<WorkerConfig>,
    pub shutdown_timeout_ms: u64,
    #[serde(default)]
    pub scheduler: SchedulerRuntimeConfig,
    #[serde(default)]
    pub strategy: StrategyRuntimeConfig,
    /// 多策略配置。为空时继续使用兼容字段 `strategy`。
    #[serde(default)]
    pub strategies: Vec<StrategyRuntimeConfig>,
}

impl RuntimeConfig {
    pub fn from_json(payload: &str) -> Result<Self, String> {
        let config: Self = serde_json::from_str(payload)
            .map_err(|error| format!("运行时配置 JSON 无效: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| format!("运行时配置编码失败: {error}"))
    }

    /// 计算规范化运行时配置指纹；指纹字段自身被清空后再编码，保证发布值
    /// 可以稳定地写回同一份 JSON，并覆盖策略、worker、凭据引用、存储和 API
    /// 等全部运行时参数。
    pub fn fingerprint(&self) -> Result<String, String> {
        let mut canonical = self.clone();
        canonical.config_fingerprint = None;
        let payload = serde_json::to_vec(&canonical)
            .map_err(|error| format!("运行时配置指纹编码失败: {error}"))?;
        Ok(qx_strategy::sha256_hex(&payload))
    }

    pub fn verify_fingerprint(&self) -> Result<(), String> {
        let Some(expected) = self.config_fingerprint.as_deref() else {
            return Ok(());
        };
        if expected.trim().is_empty() {
            return Err("config_fingerprint 不能为空字符串".into());
        }
        let actual = self.fingerprint()?;
        if expected != actual {
            return Err(format!(
                "运行时配置指纹不匹配: expected={expected} actual={actual}"
            ));
        }
        Ok(())
    }

    pub fn strategy_for_worker(&self, worker_id: &str) -> Result<StrategyRuntimeConfig, String> {
        if self.strategies.is_empty() {
            return Ok(self.strategy.clone());
        }
        self.strategies
            .iter()
            .find(|strategy| strategy.id.as_deref() == Some(worker_id))
            .cloned()
            .ok_or_else(|| format!("没有为 Strategy worker {worker_id} 配置策略实例"))
    }

    fn validate_strategy_config(
        &self,
        strategy: &StrategyRuntimeConfig,
        label: &str,
        require_binding: bool,
    ) -> Result<(), String> {
        if strategy.version.trim().is_empty() || strategy.max_orders == 0 {
            return Err(format!("{label} version 不能为空且 max_orders 必须大于 0"));
        }
        let product = strategy.product.unwrap_or(TradingProduct::Spot);
        let allow_short = strategy.allow_short.unwrap_or(product.is_derivative());
        if strategy.target_qty < 0 && !allow_short {
            return Err(format!(
                "{label} target_qty 不能为负；当前产品/策略未开启 allow_short"
            ));
        }
        if strategy
            .target_snapshot_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} target_snapshot_path 不能为空字符串"));
        }
        if strategy
            .research_snapshot_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} research_snapshot_path 不能为空字符串"));
        }
        if strategy.research_snapshot_required && strategy.research_snapshot_path.is_none() {
            return Err(format!(
                "{label} research_snapshot_required=true 时必须配置 research_snapshot_path"
            ));
        }
        if strategy
            .research_data_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint.trim().is_empty())
        {
            return Err(format!("{label} research_data_fingerprint 不能为空字符串"));
        }
        if strategy.research_snapshot_required && strategy.research_data_fingerprint.is_none() {
            return Err(format!(
                "{label} research_snapshot_required=true 时必须配置 research_data_fingerprint"
            ));
        }
        if strategy
            .python_module
            .as_deref()
            .is_some_and(|module| module.trim().is_empty())
        {
            return Err(format!("{label} python_module 不能为空字符串"));
        }
        if strategy.python_timeout_ms == 0 || strategy.python_timeout_ms > 60_000 {
            return Err(format!("{label} python_timeout_ms 必须在 1..=60000 内"));
        }
        if matches!(
            strategy.transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            qx_strategy::SharedRingConfig {
                capacity: strategy.shared_memory_capacity,
                slot_bytes: strategy.shared_memory_slot_bytes,
            }
            .validate()
            .map_err(|error| format!("{label} shared memory ring 配置非法: {error}"))?;
        }
        if strategy
            .external_executable
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} external_executable 不能为空字符串"));
        }
        if strategy
            .c_abi_library
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} c_abi_library 不能为空字符串"));
        }
        if strategy.c_abi_library.is_some() && strategy.c_abi_sha256.is_none() {
            return Err(format!("{label} c_abi_library 必须同时配置 c_abi_sha256"));
        }
        if strategy.c_abi_sha256.is_some() && strategy.c_abi_library.is_none() {
            return Err(format!(
                "{label} c_abi_sha256 只能与 c_abi_library 一起配置"
            ));
        }
        if strategy.c_abi_library.is_some() && strategy.c_abi_max_library_bytes == 0 {
            return Err(format!("{label} c_abi_max_library_bytes 必须大于 0"));
        }
        if strategy
            .strategy_artifact_sha256
            .as_deref()
            .is_some_and(|digest| {
                digest.len() != 64 || !digest.chars().all(|value| value.is_ascii_hexdigit())
            })
        {
            return Err(format!(
                "{label} strategy_artifact_sha256 必须是64位十六进制摘要"
            ));
        }
        if strategy.strategy_artifact_sha256.is_some()
            && strategy.python_module.is_none()
            && strategy.external_executable.is_none()
        {
            return Err(format!(
                "{label} strategy_artifact_sha256 必须与 python_module 或 external_executable 一起配置"
            ));
        }
        if strategy.c_abi_ed25519_public_key.is_some() != strategy.c_abi_ed25519_signature.is_some()
        {
            return Err(format!("{label} C ABI Ed25519 公钥和签名必须成对配置"));
        }
        if self.environment.eq_ignore_ascii_case("production")
            && strategy.c_abi_library.is_some()
            && strategy.c_abi_ed25519_public_key.is_none()
        {
            return Err(format!(
                "{label} production C ABI 策略必须配置 Ed25519 公钥和签名"
            ));
        }
        if self.environment.eq_ignore_ascii_case("production")
            && (strategy.external_executable.is_some() || strategy.python_module.is_some())
            && strategy.strategy_artifact_sha256.is_none()
        {
            return Err(format!(
                "{label} production 外部策略必须配置 strategy_artifact_sha256"
            ));
        }
        let strategy_sources = [
            strategy.python_module.is_some(),
            strategy.external_executable.is_some(),
            strategy.c_abi_library.is_some(),
        ]
        .into_iter()
        .filter(|configured| *configured)
        .count();
        if strategy_sources > 1 {
            return Err(format!(
                "{label} python_module、external_executable、c_abi_library 只能配置一个"
            ));
        }
        if strategy
            .external_args
            .iter()
            .any(|arg| arg.contains('\n') || arg.contains('\r'))
        {
            return Err(format!("{label} external_args 不能包含换行"));
        }
        if strategy.external_env.iter().any(|(key, value)| {
            key.trim().is_empty()
                || key.contains('=')
                || value.contains('\n')
                || value.contains('\r')
                || {
                    let upper = key.to_ascii_uppercase();
                    [
                        "SECRET",
                        "TOKEN",
                        "PASSWORD",
                        "API_KEY",
                        "PRIVATE_KEY",
                        "CREDENTIAL",
                    ]
                    .iter()
                    .any(|marker| upper.contains(marker))
                }
        }) {
            return Err(format!(
                "{label} external_env 含敏感凭证或非法键值；策略进程不得接收交易凭证"
            ));
        }
        if strategy.target_snapshot_path.is_some() && strategy.research_snapshot_path.is_some() {
            return Err(format!(
                "{label} 不能同时配置 target_snapshot_path 和 research_snapshot_path"
            ));
        }
        let leverage = strategy.leverage.unwrap_or(1);
        if leverage == 0 {
            return Err(format!("{label} leverage 必须大于 0"));
        }
        if product == TradingProduct::Spot
            && (leverage != 1
                || strategy
                    .margin_mode
                    .is_some_and(|mode| mode != MarginMode::Cash)
                || strategy.position_mode == Some(PositionMode::Hedge))
        {
            return Err(format!("{label} 现货只能使用 Cash/1x/OneWay"));
        }
        if strategy.allow_short == Some(true) && product == TradingProduct::Spot {
            return Err(format!("{label} 现货不能开启 allow_short"));
        }
        let strategy_binding_configured = strategy.account_id.is_some()
            || strategy.venue_id.is_some()
            || strategy.instrument.is_some();
        if require_binding && !strategy_binding_configured {
            return Err(format!("{label} 必须配置 account_id、venue_id、instrument"));
        }
        if !strategy_binding_configured {
            return Ok(());
        }
        if self.environment.eq_ignore_ascii_case("production")
            && (!strategy.research_snapshot_required || strategy.research_snapshot_path.is_none())
        {
            return Err(format!(
                "{label} production 已绑定交易对象，必须启用并配置 research_snapshot_path"
            ));
        }
        let account_id = strategy
            .account_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{label} account_id、venue_id、instrument 必须成组配置"))?;
        let venue_id = strategy
            .venue_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{label} account_id、venue_id、instrument 必须成组配置"))?;
        let instrument = strategy
            .instrument
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .and_then(InstrumentId::parse)
            .ok_or_else(|| format!("{label} instrument 不是合法 InstrumentId"))?;
        let has_matching_worker = self.workers.iter().any(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Strategy
                && worker.account_id.as_deref() == Some(account_id)
                && worker.venue_id.as_deref() == Some(venue_id)
                && (worker.symbols.is_empty()
                    || worker
                        .symbols
                        .iter()
                        .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(&instrument)))
        });
        if !has_matching_worker {
            return Err(format!(
                "{label} 绑定 {}@{} / {} 没有匹配的启用 strategy worker",
                account_id, venue_id, instrument
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RUNTIME_SCHEMA_VERSION {
            return Err(format!(
                "运行时配置 schema_version 必须为 {RUNTIME_SCHEMA_VERSION}"
            ));
        }
        if self.environment.trim().is_empty() {
            return Err("运行时 environment 不能为空".into());
        }
        if self
            .config_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint.trim().is_empty())
        {
            return Err("config_fingerprint 不能为空字符串".into());
        }
        self.api
            .bind
            .parse::<SocketAddr>()
            .map_err(|error| format!("API bind 不是合法 SocketAddr: {error}"))?;
        if self.api.transport == ApiTransport::Mtls && self.api.tls.is_none() {
            return Err("mTLS API 必须配置 tls 证书路径".into());
        }
        if self.api.transport == ApiTransport::Mtls && self.api.operators.is_empty() {
            return Err("mTLS API 必须至少配置一个 Operator 证书映射".into());
        }
        if self.api.transport == ApiTransport::Plaintext
            && self.environment.eq_ignore_ascii_case("production")
        {
            return Err("production 环境禁止使用明文 API".into());
        }
        if self.api.transport == ApiTransport::Plaintext && !self.api.operators.is_empty() {
            return Err("明文 API 不能声明需要 mTLS 证书的 Operator 映射".into());
        }
        for (operator_id, operator) in &self.api.operators {
            if operator_id.trim().is_empty() || operator.certificate.trim().is_empty() {
                return Err("Operator 配置必须包含 id 和 certificate".into());
            }
        }
        if let Some(tls) = &self.api.tls {
            if [
                tls.certificate_chain.as_str(),
                tls.private_key.as_str(),
                tls.client_ca.as_str(),
            ]
            .iter()
            .any(|path| path.trim().is_empty())
            {
                return Err("TLS 证书链、私钥和客户端 CA 路径不能为空".into());
            }
        }
        if self.storage.data_dir.trim().is_empty() {
            return Err("storage.data_dir 不能为空".into());
        }
        if self.environment.eq_ignore_ascii_case("production")
            && self.storage.backend != StorageBackend::Postgres
        {
            return Err(
                "production 环境 EventLog/Outbox 必须使用 PostgreSQL transactional backend".into(),
            );
        }
        if self
            .storage
            .event_log_segment_events
            .is_some_and(|events| events == 0)
        {
            return Err("storage.event_log_segment_events 必须大于 0".into());
        }
        if self.storage.backend == StorageBackend::Sqlite
            && self
                .storage
                .sqlite_path
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err("SQLite backend 必须配置 sqlite_path".into());
        }
        if self.storage.backend == StorageBackend::Postgres
            && self
                .storage
                .postgres_dsn_env
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err("PostgreSQL backend 必须配置 postgres_dsn_env".into());
        }
        if self.storage.backend == StorageBackend::Postgres
            && (self.storage.postgres_pool_size == 0 || self.storage.postgres_pool_size > 128)
        {
            return Err("PostgreSQL postgres_pool_size 必须在 1..=128 内".into());
        }
        if self.messaging.enabled {
            if self.messaging.nats_url.trim().is_empty()
                || self.messaging.subject_prefix.trim().is_empty()
            {
                return Err("启用 messaging 时 nats_url 和 subject_prefix 不能为空".into());
            }
            if self.messaging.relay_interval_ms == 0 || self.messaging.relay_interval_ms > 300_000 {
                return Err("messaging.relay_interval_ms 必须在 1..=300000 内".into());
            }
            if self.messaging.relay_batch_size == 0 || self.messaging.relay_batch_size > 10_000 {
                return Err("messaging.relay_batch_size 必须在 1..=10000 内".into());
            }
            if self.messaging.lease_seconds == 0 || self.messaging.lease_seconds > 86_400 {
                return Err("messaging.lease_seconds 必须在 1..=86400 内".into());
            }
            if self.messaging.worker_stale_after_ms == 0
                || self.messaging.worker_stale_after_ms > 86_400_000
            {
                return Err("messaging.worker_stale_after_ms 必须在 1..=86400000 内".into());
            }
        }
        if self
            .workers
            .iter()
            .any(|worker| worker.enabled && worker.role == WorkerRole::OutboxRelay)
            && !self.messaging.enabled
        {
            return Err("启用 OutboxRelay worker 时必须启用 messaging".into());
        }
        let has_event_consumer = self
            .workers
            .iter()
            .any(|worker| worker.enabled && worker.role == WorkerRole::EventConsumer);
        if has_event_consumer {
            if !self.messaging.enabled {
                return Err("启用 EventConsumer worker 时必须启用 messaging".into());
            }
            for (value, field) in [
                (&self.messaging.consumer_stream, "consumer_stream"),
                (&self.messaging.consumer_name, "consumer_name"),
                (&self.messaging.consumer_group_id, "consumer_group_id"),
                (
                    &self.messaging.consumer_handler_executable,
                    "consumer_handler_executable",
                ),
            ] {
                if value
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Err(format!("启用 EventConsumer 时 messaging.{field} 不能为空"));
                }
            }
            if self.messaging.consumer_batch_size == 0
                || self.messaging.consumer_batch_size > 10_000
            {
                return Err("messaging.consumer_batch_size 必须在 1..=10000 内".into());
            }
            if self.messaging.consumer_max_attempts == 0 {
                return Err("messaging.consumer_max_attempts 必须大于 0".into());
            }
            if self.messaging.consumer_handler_timeout_ms == 0
                || self.messaging.consumer_handler_timeout_ms > 300_000
            {
                return Err("messaging.consumer_handler_timeout_ms 必须在 1..=300000 内".into());
            }
            if self
                .messaging
                .consumer_handler_executable
                .as_deref()
                .is_some_and(|value| value.contains('\n') || value.contains('\r'))
                || self
                    .messaging
                    .consumer_handler_args
                    .iter()
                    .any(|value| value.contains('\n') || value.contains('\r'))
            {
                return Err("EventConsumer handler executable/args 不能包含换行".into());
            }
        }
        if self.shutdown_timeout_ms == 0 || self.shutdown_timeout_ms > 300_000 {
            return Err("shutdown_timeout_ms 必须在 1..=300000 内".into());
        }
        if self.scheduler.state_path.trim().is_empty()
            || self.scheduler.jobs_path.trim().is_empty()
            || self.scheduler.job_queue_path.trim().is_empty()
        {
            return Err("Scheduler state/jobs/job_queue 路径不能为空".into());
        }
        if self.scheduler.tick_interval_ms == 0 || self.scheduler.tick_interval_ms > 300_000 {
            return Err("Scheduler tick_interval_ms 必须在 1..=300000 内".into());
        }
        self.validate_strategy_config(&self.strategy, "Strategy", false)?;
        let mut strategy_ids = std::collections::BTreeSet::new();
        for strategy in &self.strategies {
            let id = strategy
                .id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "多策略配置的 id 必须非空且匹配 Strategy worker".to_string())?;
            if !strategy_ids.insert(id.to_string()) {
                return Err(format!("多策略配置 id 重复: {id}"));
            }
            if !self.workers.iter().any(|worker| {
                worker.enabled && worker.role == WorkerRole::Strategy && worker.id == id
            }) {
                return Err(format!("多策略配置 {id} 没有匹配的启用 Strategy worker"));
            }
            self.validate_strategy_config(strategy, &format!("Strategy[{id}]"), true)?;
            let instrument = strategy
                .instrument
                .as_deref()
                .and_then(InstrumentId::parse)
                .ok_or_else(|| format!("Strategy[{id}] instrument 不是合法 InstrumentId"))?;
            let worker_binding_matches = self.workers.iter().any(|worker| {
                worker.enabled
                    && worker.role == WorkerRole::Strategy
                    && worker.id == id
                    && worker.account_id.as_deref() == strategy.account_id.as_deref()
                    && worker.venue_id.as_deref() == strategy.venue_id.as_deref()
                    && (worker.symbols.is_empty()
                        || worker.symbols.iter().any(|symbol| {
                            InstrumentId::parse(symbol).as_ref() == Some(&instrument)
                        }))
            });
            if !worker_binding_matches {
                return Err(format!(
                    "Strategy[{id}] 的 account/venue/instrument 与 worker 绑定不一致"
                ));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut enabled_api_workers = 0_u8;
        for worker in &self.workers {
            if worker.id.trim().is_empty()
                || !worker
                    .id
                    .chars()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.'))
                || !ids.insert(worker.id.clone())
            {
                return Err(format!("worker id 为空或重复: {}", worker.id));
            }
            if worker
                .instrument_spec_path
                .as_deref()
                .is_some_and(|path| path.trim().is_empty())
            {
                return Err(format!("{} instrument_spec_path 不能为空字符串", worker.id));
            }
            if worker
                .paper_initial_cash_raw
                .is_some_and(|amount| amount <= 0)
            {
                return Err(format!("{} paper_initial_cash_raw 必须为正数", worker.id));
            }
            if worker.paper_initial_cash_raw.is_some()
                && !worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
            {
                return Err(format!(
                    "{} paper_initial_cash_raw 只能配置在 Paper worker",
                    worker.id
                ));
            }
            if worker
                .max_order_notional_raw
                .is_some_and(|limit| limit <= 0)
                || worker
                    .max_position_notional_raw
                    .is_some_and(|limit| limit <= 0)
            {
                return Err(format!("{} 风控名义额上限必须为正数", worker.id));
            }
            if worker
                .settlement_currency
                .as_deref()
                .is_some_and(|currency| currency.trim().is_empty())
            {
                return Err(format!("{} settlement_currency 不能为空字符串", worker.id));
            }
            if !worker.enabled {
                continue;
            }
            if worker.role == WorkerRole::Api {
                enabled_api_workers = enabled_api_workers.saturating_add(1);
            }
            if worker.role == WorkerRole::Execution
                && worker.instrument_spec_path.is_none()
                && (self.environment.eq_ignore_ascii_case("production")
                    || !worker
                        .venue_id
                        .as_deref()
                        .is_some_and(|venue| venue.eq_ignore_ascii_case("paper")))
            {
                return Err(format!(
                    "{} Execution worker 必须配置 instrument_spec_path；仅非 production 的 Paper smoke 允许兼容省略",
                    worker.id
                ));
            }
            if worker.role == WorkerRole::Execution
                && self.environment.eq_ignore_ascii_case("production")
            {
                if worker.max_order_notional_raw.is_none() {
                    return Err(format!(
                        "{} production Execution worker 必须配置 max_order_notional_raw",
                        worker.id
                    ));
                }
                if worker.max_position_notional_raw.is_none() {
                    return Err(format!(
                        "{} production Execution worker 必须配置 max_position_notional_raw",
                        worker.id
                    ));
                }
            }
            match worker.role {
                WorkerRole::UserStream | WorkerRole::Execution | WorkerRole::Reconciler => {
                    if worker
                        .account_id
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or_default()
                        .is_empty()
                        || worker
                            .venue_id
                            .as_deref()
                            .map(str::trim)
                            .unwrap_or_default()
                            .is_empty()
                    {
                        return Err(format!(
                            "{} worker 必须配置 account_id 和 venue_id",
                            worker.id
                        ));
                    }
                    if is_binance(&worker.venue_id) {
                        let has_env = valid_credential_env(worker.credential_env.as_ref());
                        let has_files = valid_credential_files(worker.credential_files.as_ref());
                        if has_env == has_files {
                            return Err(format!(
                                "{} Binance worker 必须且只能配置有效 credential_env 或 credential_files",
                                worker.id
                            ));
                        }
                    }
                }
                WorkerRole::Api
                | WorkerRole::MarketData
                | WorkerRole::Scheduler
                | WorkerRole::Strategy
                | WorkerRole::OutboxRelay
                | WorkerRole::EventConsumer => {}
            }
            if matches!(worker.role, WorkerRole::MarketData | WorkerRole::UserStream)
                && worker
                    .endpoint
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .is_empty()
            {
                return Err(format!("{} worker 必须配置 endpoint", worker.id));
            }
            if worker.role == WorkerRole::MarketData
                && is_binance(&worker.venue_id)
                && worker.symbols.is_empty()
            {
                return Err(format!(
                    "{} Binance 行情 worker 至少需要一个 symbol",
                    worker.id
                ));
            }
            if worker.instrument_spec_path.is_none()
                && (worker.max_order_notional_raw.is_some()
                    || worker.max_position_notional_raw.is_some())
            {
                return Err(format!(
                    "{} 配置名义额上限时必须同时配置 instrument_spec_path",
                    worker.id
                ));
            }
            if worker.role == WorkerRole::Reconciler && is_binance(&worker.venue_id) {
                for symbol in &worker.symbols {
                    let valid = InstrumentId::parse(symbol).is_some_and(|instrument| {
                        instrument.venue.as_str().eq_ignore_ascii_case("BINANCE")
                    });
                    if !valid {
                        return Err(format!(
                            "{} Binance 对账 symbol 必须是合法的 *.BINANCE InstrumentId: {}",
                            worker.id, symbol
                        ));
                    }
                }
            }
        }
        if enabled_api_workers != 1 {
            return Err("运行时配置必须且只能启用一个 api worker".into());
        }
        self.verify_fingerprint()?;
        Ok(())
    }
}

fn is_binance(venue_id: &Option<String>) -> bool {
    venue_id
        .as_deref()
        .map(|venue| venue.to_ascii_lowercase().contains("binance"))
        .unwrap_or(false)
}

fn valid_credential_env(credentials: Option<&CredentialEnv>) -> bool {
    credentials
        .map(|credentials| {
            [credentials.api_key.as_str(), credentials.secret.as_str()]
                .iter()
                .all(|name| {
                    !name.trim().is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                })
        })
        .unwrap_or(false)
}

fn valid_credential_files(credentials: Option<&CredentialFiles>) -> bool {
    credentials
        .map(|credentials| {
            [credentials.api_key.as_str(), credentials.secret.as_str()]
                .iter()
                .all(|path| !path.trim().is_empty())
        })
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Starting,
    Ready,
    Running,
    Degraded,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub id: String,
    pub role: WorkerRole,
    pub status: ServiceStatus,
    pub last_heartbeat_ms: Option<u64>,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallHealth {
    Starting,
    Ready,
    Degraded,
    Failed,
    Stopped,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub overall: OverallHealth,
    pub services: Vec<ServiceHealth>,
}

#[derive(Clone, Default)]
pub struct HealthRegistry {
    services: BTreeMap<String, ServiceHealth>,
}

impl HealthRegistry {
    pub fn register(&mut self, id: impl Into<String>, role: WorkerRole) -> Result<(), String> {
        let id = id.into();
        if id.trim().is_empty() || self.services.contains_key(&id) {
            return Err(format!("服务 id 为空或重复: {id}"));
        }
        self.services.insert(
            id.clone(),
            ServiceHealth {
                id,
                role,
                status: ServiceStatus::Starting,
                last_heartbeat_ms: None,
                detail: "registered".into(),
            },
        );
        Ok(())
    }

    pub fn mark(
        &mut self,
        id: &str,
        status: ServiceStatus,
        detail: impl Into<String>,
        now_ms: Option<u64>,
    ) -> Result<(), String> {
        let service = self
            .services
            .get_mut(id)
            .ok_or_else(|| format!("未知服务: {id}"))?;
        service.status = status;
        service.detail = detail.into();
        if now_ms.is_some() {
            service.last_heartbeat_ms = now_ms;
        }
        Ok(())
    }

    pub fn heartbeat(&mut self, id: &str, now_ms: u64) -> Result<(), String> {
        let service = self
            .services
            .get_mut(id)
            .ok_or_else(|| format!("未知服务: {id}"))?;
        service.last_heartbeat_ms = Some(now_ms);
        if service.status == ServiceStatus::Starting {
            service.status = ServiceStatus::Ready;
        }
        Ok(())
    }

    pub fn snapshot(&self, now_ms: u64, stale_after_ms: u64) -> HealthSnapshot {
        let mut services: Vec<_> = self.services.values().cloned().collect();
        services.sort_by(|left, right| left.id.cmp(&right.id));
        let awaiting_first_heartbeat = services.iter().any(|service| {
            matches!(
                service.status,
                ServiceStatus::Ready | ServiceStatus::Running
            ) && service.last_heartbeat_ms.is_none()
        });
        let stale = services.iter().any(|service| {
            matches!(
                service.status,
                ServiceStatus::Ready | ServiceStatus::Running
            ) && service
                .last_heartbeat_ms
                .map(|heartbeat| now_ms.saturating_sub(heartbeat) > stale_after_ms)
                .unwrap_or(false)
        });
        let overall = if services.is_empty() {
            OverallHealth::Stopped
        } else if services
            .iter()
            .any(|service| service.status == ServiceStatus::Failed)
        {
            OverallHealth::Failed
        } else if services
            .iter()
            .all(|service| service.status == ServiceStatus::Stopped)
        {
            OverallHealth::Stopped
        } else if stale
            || services.iter().any(|service| {
                matches!(
                    service.status,
                    ServiceStatus::Degraded | ServiceStatus::Stopping
                )
            })
        {
            OverallHealth::Degraded
        } else if awaiting_first_heartbeat
            || services
                .iter()
                .any(|service| service.status == ServiceStatus::Starting)
        {
            OverallHealth::Starting
        } else {
            OverallHealth::Ready
        };
        HealthSnapshot { overall, services }
    }
}

#[derive(Clone, Default)]
pub struct ShutdownToken(Arc<AtomicBool>);

impl ShutdownToken {
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct RuntimeSupervisor {
    config: RuntimeConfig,
    health: Arc<Mutex<HealthRegistry>>,
    shutdown: ShutdownToken,
}

#[derive(Clone)]
pub struct WorkerContext {
    id: String,
    shutdown: ShutdownToken,
    health: Arc<Mutex<HealthRegistry>>,
}

impl WorkerContext {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn should_stop(&self) -> bool {
        self.shutdown.is_requested()
    }

    pub fn heartbeat(&self, now_ms: u64) -> Result<(), String> {
        self.health
            .lock()
            .map_err(|_| "运行时健康锁已中毒".to_string())?
            .heartbeat(&self.id, now_ms)
    }

    pub fn mark(
        &self,
        status: ServiceStatus,
        detail: impl Into<String>,
        now_ms: Option<u64>,
    ) -> Result<(), String> {
        self.health
            .lock()
            .map_err(|_| "运行时健康锁已中毒".to_string())?
            .mark(&self.id, status, detail, now_ms)
    }
}

impl RuntimeSupervisor {
    pub fn new(config: RuntimeConfig) -> Result<Self, String> {
        config.validate()?;
        let mut registry = HealthRegistry::default();
        for worker in config.workers.iter().filter(|worker| worker.enabled) {
            registry.register(worker.id.clone(), worker.role)?;
        }
        Ok(Self {
            config,
            health: Arc::new(Mutex::new(registry)),
            shutdown: ShutdownToken::default(),
        })
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    pub fn health(&self) -> Arc<Mutex<HealthRegistry>> {
        Arc::clone(&self.health)
    }

    pub fn shutdown_token(&self) -> ShutdownToken {
        self.shutdown.clone()
    }

    pub fn request_shutdown(&self) {
        self.shutdown.request();
        if let Ok(mut health) = self.health.lock() {
            let ids = health.services.keys().cloned().collect::<Vec<_>>();
            for id in ids {
                if let Some(service) = health.services.get(&id) {
                    if matches!(
                        service.status,
                        ServiceStatus::Ready | ServiceStatus::Running
                    ) {
                        let _ =
                            health.mark(&id, ServiceStatus::Stopping, "shutdown requested", None);
                    }
                }
            }
        }
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown.is_requested()
    }

    /// 启动一个由调用方注入的 worker；worker 不允许直接改 Kernel 状态，必须通过
    /// 已有的 Adapter/Control/Storage 契约提交事实。线程异常被转换为 Failed 状态，
    /// 返回 `Err` 的正常退出也会留下可查询的失败原因。
    pub fn spawn_worker<F>(
        &self,
        id: &str,
        run: F,
    ) -> Result<JoinHandle<Result<(), String>>, String>
    where
        F: FnOnce(WorkerContext) -> Result<(), String> + Send + 'static,
    {
        {
            let health = self
                .health
                .lock()
                .map_err(|_| "运行时健康锁已中毒".to_string())?;
            if !health.services.contains_key(id) {
                return Err(format!("未知 worker: {id}"));
            }
        }
        let context = WorkerContext {
            id: id.into(),
            shutdown: self.shutdown.clone(),
            health: Arc::clone(&self.health),
        };
        let thread_id = id.to_string();
        Ok(std::thread::spawn(move || {
            let _ = context.mark(ServiceStatus::Ready, "running", None);
            let _ = context.mark(ServiceStatus::Running, "worker loop running", None);
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(context.clone())));
            match result {
                Ok(Ok(())) => {
                    let _ = context.mark(ServiceStatus::Stopping, "worker stopping", None);
                    let _ = context.mark(ServiceStatus::Stopped, "stopped", None);
                    Ok(())
                }
                Ok(Err(error)) => {
                    let _ = context.mark(ServiceStatus::Failed, error.clone(), None);
                    Err(format!("worker {thread_id} failed"))
                }
                Err(_) => {
                    let _ = context.mark(ServiceStatus::Failed, "worker panicked", None);
                    Err(format!("worker {thread_id} panicked"))
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_strategy_contract_is_versioned_pit_bounded_and_identity_bound() {
        let input = StrategyContractInput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: "request-1".into(),
            strategy_id: "strategy-1".into(),
            strategy_version: "v1".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 10,
            instrument: "BTCUSDT.BINANCE".into(),
            positions: BTreeMap::from([("BTCUSDT.BINANCE".into(), 2)]),
            cash: BTreeMap::from([("USDT".into(), 100)]),
            available_margin_raw: Some(90),
            risk_state: "verified".into(),
            research_targets: BTreeMap::from([("BTCUSDT.BINANCE".into(), 3)]),
            bars: Some(StrategyContractBars {
                source: "snapshot-1".into(),
                ts: vec![9, 10],
                open_raw: vec![1, 2],
                high_raw: vec![2, 3],
                low_raw: vec![1, 2],
                close_raw: vec![2, 3],
                volume_raw: vec![10, 11],
            }),
        };
        let restored = StrategyContractInput::from_json(&input.to_json().unwrap()).unwrap();
        assert_eq!(restored, input);
        let output = StrategyContractOutput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: "request-1".into(),
            strategy_id: "strategy-1".into(),
            signal_id: 1,
            instrument: "BTCUSDT.BINANCE".into(),
            target_qty: 3,
            confidence: 900,
            priority: 1,
            expires_at: 10,
            intents: Vec::new(),
        };
        let encoded = output.to_json_for(&input).unwrap();
        assert_eq!(
            StrategyContractOutput::from_json_for(&encoded, &input).unwrap(),
            output
        );
        let mut mismatched = output;
        mismatched.request_id = "other".into();
        assert!(mismatched.validate_for(&input).is_err());
    }

    #[test]
    fn strategy_columnar_input_preserves_metadata_and_fixed_width_columns() {
        let input = StrategyContractInput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: "columnar-1".into(),
            strategy_id: "strategy-1".into(),
            strategy_version: "v1".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 10,
            instrument: "BTCUSDT.BINANCE".into(),
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: Some(90),
            risk_state: "verified".into(),
            research_targets: BTreeMap::from([("BTCUSDT.BINANCE".into(), 3)]),
            bars: Some(StrategyContractBars {
                source: "snapshot-1".into(),
                ts: vec![9, 10],
                open_raw: vec![1, 2],
                high_raw: vec![2, 3],
                low_raw: vec![1, 2],
                close_raw: vec![2, 3],
                volume_raw: vec![10, 11],
            }),
        };
        let encoded = encode_strategy_columnar_input(&input).unwrap();
        assert_eq!(&encoded[..4], b"QXCB");
        assert_eq!(u16::from_le_bytes([encoded[4], encoded[5]]), 1);
        let metadata_len = u32::from_le_bytes(encoded[8..12].try_into().unwrap()) as usize;
        let metadata: serde_json::Value = serde_json::from_slice(
            &encoded[STRATEGY_COLUMNAR_HEADER_LEN..STRATEGY_COLUMNAR_HEADER_LEN + metadata_len],
        )
        .unwrap();
        assert!(metadata["bars"].is_null());
        assert_eq!(metadata["__qx_bars_source"], "snapshot-1");
        assert_eq!(
            encoded.len(),
            STRATEGY_COLUMNAR_HEADER_LEN + metadata_len + 2 * (8 + 5 * 16)
        );
    }

    #[test]
    fn strategy_contract_accepts_multiple_order_intents() {
        let input = StrategyContractInput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: "request-intents".into(),
            strategy_id: "strategy-portfolio".into(),
            strategy_version: "v1".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 10,
            instrument: "BTCUSDT.BINANCE".into(),
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: Some(100),
            risk_state: "ready".into(),
            research_targets: BTreeMap::new(),
            bars: None,
        };
        let output = StrategyContractOutput {
            schema_version: STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: input.request_id.clone(),
            strategy_id: input.strategy_id.clone(),
            signal_id: 99,
            instrument: input.instrument.clone(),
            target_qty: 0,
            confidence: 500,
            priority: 1,
            expires_at: 10,
            intents: vec![
                StrategyContractIntent {
                    intent_id: 1001,
                    instrument: "BTCUSDT.BINANCE".into(),
                    side: "buy".into(),
                    qty_raw: 2,
                    limit_price_raw: Some(100),
                    reduce_only: false,
                    post_only: true,
                    position_side: Some("net".into()),
                },
                StrategyContractIntent {
                    intent_id: 1002,
                    instrument: "ETHUSDT.BINANCE".into(),
                    side: "sell".into(),
                    qty_raw: 1,
                    limit_price_raw: None,
                    reduce_only: true,
                    post_only: false,
                    position_side: Some("net".into()),
                },
            ],
        };
        let encoded = output.to_json_for(&input).unwrap();
        let restored = StrategyContractOutput::from_json_for(&encoded, &input).unwrap();
        assert_eq!(restored, output);
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            schema_version: RUNTIME_SCHEMA_VERSION,
            environment: "paper".into(),
            config_fingerprint: None,
            api: ApiRuntimeConfig {
                bind: "127.0.0.1:19090".into(),
                transport: ApiTransport::Plaintext,
                tls: None,
                operators: BTreeMap::new(),
            },
            storage: StorageRuntimeConfig {
                backend: StorageBackend::Files,
                data_dir: "data".into(),
                sqlite_path: None,
                postgres_dsn_env: None,
                postgres_pool_size: default_postgres_pool_size(),
                event_log_segment_events: None,
            },
            messaging: MessagingRuntimeConfig::default(),
            workers: vec![
                WorkerConfig {
                    id: "api".into(),
                    role: WorkerRole::Api,
                    enabled: true,
                    account_id: None,
                    venue_id: None,
                    endpoint: None,
                    symbols: Vec::new(),
                    settlement_currency: None,
                    credential_env: None,
                    credential_files: None,
                    instrument_spec_path: None,
                    paper_initial_cash_raw: None,
                    max_order_notional_raw: None,
                    max_position_notional_raw: None,
                },
                WorkerConfig {
                    id: "market".into(),
                    role: WorkerRole::MarketData,
                    enabled: true,
                    account_id: None,
                    venue_id: None,
                    endpoint: Some("https://example.test".into()),
                    symbols: vec!["BTCUSDT.BINANCE".into()],
                    settlement_currency: None,
                    credential_env: None,
                    credential_files: None,
                    instrument_spec_path: None,
                    paper_initial_cash_raw: None,
                    max_order_notional_raw: None,
                    max_position_notional_raw: None,
                },
            ],
            shutdown_timeout_ms: 10_000,
            scheduler: SchedulerRuntimeConfig::default(),
            strategy: StrategyRuntimeConfig::default(),
            strategies: Vec::new(),
        }
    }

    #[test]
    fn runtime_config_round_trips_and_rejects_production_plaintext() {
        let encoded = config().to_json().unwrap();
        assert_eq!(RuntimeConfig::from_json(&encoded).unwrap(), config());
        let mut production = config();
        production.environment = "production".into();
        assert!(production.validate().is_err());

        let mut mtls = config();
        mtls.api.transport = ApiTransport::Mtls;
        assert!(mtls.validate().is_err());
        mtls.api.tls = Some(TlsPaths {
            certificate_chain: "server.pem".into(),
            private_key: "server.key".into(),
            client_ca: "clients.pem".into(),
        });
        assert!(mtls.validate().is_err());
    }

    #[test]
    fn runtime_config_fingerprint_locks_published_configuration() {
        let mut locked = config();
        let fingerprint = locked.fingerprint().unwrap();
        assert_eq!(fingerprint.len(), 64);
        locked.config_fingerprint = Some(fingerprint);
        let encoded = locked.to_json().unwrap();
        assert_eq!(RuntimeConfig::from_json(&encoded).unwrap(), locked);

        let mut tampered = locked.clone();
        tampered.storage.data_dir = "data/tampered".into();
        let tampered_payload = serde_json::to_string(&tampered).unwrap();
        let error = RuntimeConfig::from_json(&tampered_payload).unwrap_err();
        assert!(error.contains("配置指纹不匹配"));
    }

    #[test]
    fn production_bound_strategy_requires_research_snapshot() {
        let mut config = config();
        config.environment = "production".into();
        config.storage.backend = StorageBackend::Postgres;
        config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
        config.api.transport = ApiTransport::Mtls;
        config.api.tls = Some(TlsPaths {
            certificate_chain: "server.pem".into(),
            private_key: "server.key".into(),
            client_ca: "clients.pem".into(),
        });
        config.api.operators.insert(
            "ops".into(),
            OperatorConfig {
                permission: Permission::Admin,
                certificate: "ops.pem".into(),
            },
        );
        config.workers.push(WorkerConfig {
            id: "strategy-main".into(),
            role: WorkerRole::Strategy,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("binance".into()),
            endpoint: None,
            symbols: vec!["BTCUSDT.BINANCE".into()],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("binance".into());
        config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
        assert!(config.validate().is_err());

        config.strategy.research_snapshot_required = true;
        config.strategy.research_snapshot_path = Some("research.json".into());
        config.strategy.research_data_fingerprint = Some("bars-sha256".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn research_snapshot_required_rejects_missing_path() {
        let mut config = config();
        config.strategy.research_snapshot_required = true;
        assert!(config.validate().is_err());
    }

    #[test]
    fn c_abi_strategy_requires_digest_and_exclusive_source() {
        let mut config = config();
        config.strategy.c_abi_library = Some("strategy.dll".into());
        assert!(config.validate().is_err());

        config.strategy.c_abi_sha256 = Some("ab".repeat(32));
        assert!(config.validate().is_ok());

        config.strategy.python_module = Some("example_strategy".into());
        assert!(config.validate().is_err());

        config.strategy.python_module = None;
        config.strategy.c_abi_ed25519_public_key = Some("00".repeat(32));
        assert!(config.validate().is_err());
    }

    #[test]
    fn production_c_abi_strategy_requires_detached_signature() {
        let mut config = config();
        config.environment = "production".into();
        config.storage.backend = StorageBackend::Postgres;
        config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
        config.api.transport = ApiTransport::Mtls;
        config.api.tls = Some(TlsPaths {
            certificate_chain: "server.pem".into(),
            private_key: "server.key".into(),
            client_ca: "clients.pem".into(),
        });
        config.api.operators.insert(
            "ops".into(),
            OperatorConfig {
                permission: Permission::Admin,
                certificate: "ops.pem".into(),
            },
        );
        config.strategy.c_abi_library = Some("strategy.dll".into());
        config.strategy.c_abi_sha256 = Some("ab".repeat(32));
        assert!(config.validate().is_err());
        config.strategy.c_abi_ed25519_public_key = Some("00".repeat(32));
        config.strategy.c_abi_ed25519_signature = Some("00".repeat(64));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn production_external_strategy_requires_artifact_lock() {
        let mut config = config();
        config.environment = "production".into();
        config.storage.backend = StorageBackend::Postgres;
        config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
        config.api.transport = ApiTransport::Mtls;
        config.api.tls = Some(TlsPaths {
            certificate_chain: "server.pem".into(),
            private_key: "server.key".into(),
            client_ca: "clients.pem".into(),
        });
        config.api.operators.insert(
            "ops".into(),
            OperatorConfig {
                permission: Permission::Admin,
                certificate: "ops.pem".into(),
            },
        );
        config.strategy.python_module = Some("strategy.production".into());
        let error = config.validate().unwrap_err();
        assert!(error.contains("strategy_artifact_sha256"));
        config.strategy.strategy_artifact_sha256 = Some("00".repeat(32));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn multi_strategy_instances_are_bound_to_workers_and_jobs_can_select_them() {
        let mut config = config();
        config.workers.push(WorkerConfig {
            id: "strategy-alpha".into(),
            role: WorkerRole::Strategy,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("okx".into()),
            endpoint: None,
            symbols: vec!["BTC/USDT.OKX".into()],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        config.strategies.push(StrategyRuntimeConfig {
            id: Some("strategy-alpha".into()),
            version: "alpha-v1".into(),
            max_orders: 10,
            account_id: Some("main".into()),
            venue_id: Some("okx".into()),
            instrument: Some("BTC/USDT.OKX".into()),
            target_qty: 0,
            target_snapshot_path: None,
            research_snapshot_path: None,
            research_snapshot_required: false,
            research_data_fingerprint: None,
            product: None,
            margin_mode: None,
            position_mode: None,
            leverage: None,
            allow_short: None,
            python_module: None,
            transport: StrategyTransport::Jsonl,
            shared_memory_capacity: default_strategy_shared_memory_capacity(),
            shared_memory_slot_bytes: default_strategy_shared_memory_slot_bytes(),
            python_timeout_ms: default_strategy_python_timeout_ms(),
            external_executable: None,
            strategy_artifact_sha256: None,
            external_args: Vec::new(),
            external_env: BTreeMap::new(),
            c_abi_library: None,
            c_abi_sha256: None,
            c_abi_max_library_bytes: default_strategy_c_abi_max_library_bytes(),
            c_abi_ed25519_public_key: None,
            c_abi_ed25519_signature: None,
        });
        config.validate().unwrap();
        assert_eq!(
            config
                .strategy_for_worker("strategy-alpha")
                .unwrap()
                .version,
            "alpha-v1"
        );
        assert!(config.strategy_for_worker("strategy-missing").is_err());
    }

    #[test]
    fn strategy_process_configuration_is_exclusive_and_validated() {
        let mut config = config();
        config.strategy.python_module = Some("demo_strategy".into());
        config.strategy.external_executable = Some("strategy.exe".into());
        assert!(config.validate().is_err());

        config.strategy.python_module = None;
        config.strategy.external_executable = Some("strategy.exe".into());
        config.strategy.external_args = vec!["--mode".into(), "jsonl".into()];
        config
            .strategy
            .external_env
            .insert("QX_MODE".into(), "paper".into());
        assert!(config.validate().is_ok());
        config
            .strategy
            .external_env
            .insert("EXCHANGE_API_KEY".into(), "must-not-pass".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn segmented_event_log_storage_configuration_is_positive_and_optional() {
        let mut config = config();
        assert_eq!(config.storage.event_log_segment_events, None);
        config.storage.event_log_segment_events = Some(1024);
        assert!(config.validate().is_ok());
        config.storage.event_log_segment_events = Some(0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn messaging_worker_requires_valid_runtime_contract() {
        let mut relay_config = config();
        relay_config.workers.push(WorkerConfig {
            id: "outbox-relay".into(),
            role: WorkerRole::OutboxRelay,
            enabled: true,
            account_id: None,
            venue_id: None,
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(relay_config.validate().is_err());
        relay_config.messaging.enabled = true;
        assert!(relay_config.validate().is_ok());
        relay_config.messaging.relay_batch_size = 0;
        assert!(relay_config.validate().is_err());
        relay_config.messaging.relay_batch_size = 100;
        relay_config.messaging.worker_stale_after_ms = 0;
        assert!(relay_config.validate().is_err());

        let mut consumer = config();
        consumer.messaging.enabled = true;
        consumer.messaging.consumer_stream = Some("QIANXING_EVENTS".into());
        consumer.messaging.consumer_name = Some("ledger-reducer".into());
        consumer.messaging.consumer_group_id = Some("ledger-reducer".into());
        consumer.messaging.consumer_handler_executable = Some("python".into());
        consumer.workers.push(WorkerConfig {
            id: "ledger-reducer".into(),
            role: WorkerRole::EventConsumer,
            enabled: true,
            account_id: None,
            venue_id: None,
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(consumer.validate().is_ok());
        consumer.messaging.consumer_handler_timeout_ms = 0;
        assert!(consumer.validate().is_err());
    }

    #[test]
    fn worker_ids_are_safe_for_runtime_artifact_names() {
        let mut invalid = config();
        invalid.messaging.enabled = true;
        invalid.workers.push(WorkerConfig {
            id: "relay/primary".into(),
            role: WorkerRole::OutboxRelay,
            enabled: true,
            account_id: None,
            venue_id: None,
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn postgres_storage_requires_secret_manager_environment_name() {
        let mut config = config();
        config.storage.backend = StorageBackend::Postgres;
        assert!(config.validate().is_err());
        config.storage.postgres_dsn_env = Some("QX_POSTGRES_DSN".into());
        assert!(config.validate().is_ok());
        config.storage.postgres_pool_size = 0;
        assert!(config.validate().is_err());
        config.storage.postgres_pool_size = 8;
        assert!(config.validate().is_ok());
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("postgresql://"));
    }

    #[test]
    fn binance_private_workers_require_one_valid_credential_source() {
        let mut invalid = config();
        invalid.workers.push(WorkerConfig {
            id: "user".into(),
            role: WorkerRole::UserStream,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("binance-testnet".into()),
            endpoint: Some("wss://ws-api.testnet.binance.vision/ws-api/v3".into()),
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(invalid.validate().is_err());
        invalid.workers.last_mut().unwrap().credential_env = Some(CredentialEnv {
            api_key: "QX_BINANCE_TESTNET_API_KEY".into(),
            secret: "QX_BINANCE_TESTNET_API_SECRET".into(),
        });
        assert!(invalid.validate().is_ok());
        invalid.workers.last_mut().unwrap().credential_files = Some(CredentialFiles {
            api_key: "/run/secrets/api-key".into(),
            secret: "/run/secrets/secret".into(),
        });
        assert!(invalid.validate().is_err());
        invalid.workers.last_mut().unwrap().credential_env = None;
        assert!(invalid.validate().is_ok());
        invalid.workers.last_mut().unwrap().credential_files = None;
        invalid.workers.last_mut().unwrap().credential_env = Some(CredentialEnv {
            api_key: "QX-BAD".into(),
            secret: "QX_SECRET".into(),
        });
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn execution_worker_requires_account_venue_and_credentials() {
        let mut config = config();
        config.workers.push(WorkerConfig {
            id: "execution".into(),
            role: WorkerRole::Execution,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("binance-testnet".into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(config.validate().is_err());
        config.workers.last_mut().unwrap().credential_env = Some(CredentialEnv {
            api_key: "QX_BINANCE_TESTNET_API_KEY".into(),
            secret: "QX_BINANCE_TESTNET_API_SECRET".into(),
        });
        config.workers.last_mut().unwrap().instrument_spec_path = Some("market-spec.json".into());
        assert!(config.validate().is_ok());
        config.workers.last_mut().unwrap().credential_env = None;
        config.workers.last_mut().unwrap().credential_files = Some(CredentialFiles {
            api_key: "/run/secrets/api-key".into(),
            secret: "/run/secrets/secret".into(),
        });
        assert!(config.validate().is_ok());
    }

    #[test]
    fn execution_risk_limits_require_a_frozen_instrument_spec() {
        let mut config = config();
        config.workers.push(WorkerConfig {
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
            paper_initial_cash_raw: None,
            max_order_notional_raw: Some(1_000),
            max_position_notional_raw: None,
        });
        assert!(config.validate().is_err());
        config.workers.last_mut().unwrap().instrument_spec_path = Some("market-spec.json".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn strategy_binding_requires_a_matching_enabled_worker_and_nonnegative_target() {
        let mut config = config();
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("paper".into());
        config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
        assert!(config.validate().is_err());

        config.workers.push(WorkerConfig {
            id: "strategy".into(),
            role: WorkerRole::Strategy,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("paper".into()),
            endpoint: None,
            symbols: vec!["BTCUSDT.BINANCE".into()],
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(config.validate().is_ok());
        config.strategy.target_qty = -1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn strategy_target_snapshot_is_bound_to_runtime_version_and_time() {
        let snapshot = StrategyTargetSnapshot {
            schema_version: StrategyTargetSnapshot::SCHEMA_VERSION,
            strategy_version: "strategy-runtime-v1".into(),
            data_fingerprint: "data-1".into(),
            as_of: 10,
            targets: BTreeMap::from([("BTCUSDT.BINANCE".into(), 1)]),
        };
        assert!(snapshot.validate_for("strategy-runtime-v1", 10).is_ok());
        assert!(snapshot.validate_for("strategy-runtime-v2", 10).is_err());
        assert!(snapshot.validate_for("strategy-runtime-v1", 9).is_err());
    }

    #[test]
    fn health_snapshot_detects_stale_ready_service() {
        let mut health = HealthRegistry::default();
        health.register("market", WorkerRole::MarketData).unwrap();
        health.heartbeat("market", 10).unwrap();
        assert_eq!(health.snapshot(20, 100).overall, OverallHealth::Ready);
        assert_eq!(health.snapshot(200, 100).overall, OverallHealth::Degraded);
    }

    #[test]
    fn supervisor_registers_only_enabled_workers_and_exposes_shutdown() {
        let mut config = config();
        config.workers.push(WorkerConfig {
            id: "disabled".into(),
            role: WorkerRole::Scheduler,
            enabled: false,
            account_id: None,
            venue_id: None,
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: None,
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: None,
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        let supervisor = RuntimeSupervisor::new(config).unwrap();
        assert_eq!(
            supervisor
                .health()
                .lock()
                .unwrap()
                .snapshot(0, 100)
                .services
                .len(),
            2
        );
        assert!(!supervisor.is_shutdown_requested());
        supervisor.request_shutdown();
        assert!(supervisor.is_shutdown_requested());
    }

    #[test]
    fn supervisor_runs_worker_and_records_terminal_state() {
        let supervisor = RuntimeSupervisor::new(config()).unwrap();
        let worker = supervisor
            .spawn_worker("market", |context| {
                context.heartbeat(10)?;
                assert!(!context.should_stop());
                Ok(())
            })
            .unwrap();
        assert!(worker.join().unwrap().is_ok());
        supervisor
            .health()
            .lock()
            .unwrap()
            .mark("api", ServiceStatus::Stopped, "test", None)
            .unwrap();
        let snapshot = supervisor.health().lock().unwrap().snapshot(10, 100);
        assert_eq!(snapshot.overall, OverallHealth::Stopped);
        assert_eq!(snapshot.services[0].status, ServiceStatus::Stopped);
    }

    #[test]
    fn supervisor_converts_worker_panic_to_failed_health() {
        let supervisor = RuntimeSupervisor::new(config()).unwrap();
        let worker = supervisor
            .spawn_worker("market", |_context| -> Result<(), String> {
                panic!("injected worker panic");
            })
            .unwrap();
        assert!(worker.join().unwrap().is_err());
        supervisor
            .health()
            .lock()
            .unwrap()
            .mark("api", ServiceStatus::Stopped, "test", None)
            .unwrap();
        assert_eq!(
            supervisor.health().lock().unwrap().snapshot(0, 100).overall,
            OverallHealth::Failed
        );
    }
}
