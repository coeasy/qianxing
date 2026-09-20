//! 运行时配置类型：profile、API、存储、消息与 worker 拓扑。

use super::*;

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

/// 运行时部署 profile。
///
/// `single_node` 是本阶段的默认工业化基线：运行时事实、控制面和队列
/// 使用本地 SQLite/Files，不要求 PostgreSQL 或 NATS。`distributed` 仅保留
/// 给后续多节点部署，不能因为编译了 feature 就被单机配置隐式启用。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProfile {
    #[default]
    SingleNode,
    Distributed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiTransport {
    Plaintext,
    Mtls,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsPaths {
    pub certificate_chain: String,
    pub private_key: String,
    pub client_ca: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub permission: Permission,
    pub certificate: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// 运行时事实的持久化/传播一致性等级。
///
/// `local_durable` 适用于单机文件或 SQLite，依靠顺序追加、恢复扫描和本地
/// 租约保证一致性；`transactional` 要求 PostgreSQL 在同一事务中提交领域
/// 事实和 Outbox；`distributed_outbox` 表示在持久化事实之后通过 Outbox
/// Relay 异步发布到 NATS，不把消息发布误称为跨系统原子事务。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageConsistency {
    #[default]
    LocalDurable,
    Transactional,
    DistributedOutbox,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRuntimeConfig {
    pub backend: StorageBackend,
    #[serde(default)]
    pub consistency: StorageConsistency,
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
#[serde(deny_unknown_fields)]
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
    SpreadRecovery,
    Scheduler,
    Reconciler,
    Strategy,
    OutboxRelay,
    EventConsumer,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct CredentialEnv {
    pub api_key: String,
    pub secret: String,
}

/// 由 Secret Manager、CSI driver 或容器 secrets 投影的凭据文件路径。
/// 文件内容不进入运行时 JSON、健康详情或事件日志；worker 在建立新连接、
/// 新一轮对账和新订单执行前重新读取文件，以支持原子替换式轮换。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialFiles {
    pub api_key: String,
    pub secret: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub schema_version: u32,
    pub environment: String,
    #[serde(default)]
    pub profile: RuntimeProfile,
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
