//! 运行时配置类型：profile、API、存储、消息与 worker 拓扑。

use super::*;

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

/// production 这一档的规范写法。
///
/// 单独成一格，是为了让「production 到底怎么写」在仓库里只出现一次：词表从它取、判定也从它取。
/// 改口径（例如改成 `prod`）时不可能只改到一半。
pub const PRODUCTION_ENVIRONMENT: &str = "production";

/// `environment` 的闭合写法名单（大小写不敏感、不含首尾空白）。
///
/// 这个字符串同时决定两件事：14 处 `production` 专属闸门里有 9 处就在运行时配置校验内
/// （另外 5 处在 CLI 侧的体检与就绪判定），以及实时策略作业的 `dry_run` 走模拟还是真实
/// 提交（只有 `paper` 模拟）。名单本身在这里单源，校验闸门与用例都读同一份；
/// 「是不是 production」的判定同样只有一个出口 —— [`RuntimeConfig::is_production`]。
pub const ENVIRONMENT_VOCAB: [&str; 4] = ["paper", "sandbox", "testnet", PRODUCTION_ENVIRONMENT];

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

/// 本机回环上的同源 BFF 控制台（`qx-cli console`）的部署参数。
///
/// 与 `api.bind` / `transport` 那套 mTLS 边界刻意分成两格：控制台面**不做** mTLS，它的信任
/// 边界是四道——只绑回环、启动令牌换会话、非 GET 必须带 CSRF、Origin 与 Host 必须同源。
/// 四道都失效才退回"和 `serve` 一样"的暴露面，所以它不是第二个鉴权层，而是浏览器侧的封装。
/// `operator` 是这层代理替浏览器声称的身份，页面不能自声明：`api.operators` 非空时它必须是
/// 其中一员（否则每个受保护端点都会 403，而配置侧看不出为什么）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleRuntimeConfig {
    /// 控制台面的监听地址；只接受回环地址。
    pub bind: String,
    /// 静态控制台资源目录（`web/console`），按文件名精确匹配，不做路径拼接。
    pub static_dir: String,
    /// 代理请求时声称的 operator 身份。
    pub operator: String,
    /// 承载启动令牌的环境变量名。令牌本身**不写进**运行时 JSON。
    pub bootstrap_token_env: String,
    /// 会话有效期（秒）。`None` 表示用 `qx-api` 自己那个默认值
    /// （`DEFAULT_CONSOLE_SESSION_TTL_SECONDS`）：与 `max_concurrent_connections` 同一条纪律，
    /// 数字只有一个定义点，配置侧不抄第二份。
    #[serde(default)]
    pub session_ttl_seconds: Option<u64>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRuntimeConfig {
    pub bind: String,
    pub transport: ApiTransport,
    pub tls: Option<TlsPaths>,
    #[serde(default)]
    pub operators: BTreeMap<String, OperatorConfig>,
    /// 允许跨源读本面的浏览器源，逐条必须是精确的 `http(s)://host[:port]`。
    ///
    /// 缺省（空）表示这份部署不开浏览器准入：响应里不带任何 `Access-Control-*`，
    /// `OPTIONS` 预检照常走路由分派落到 404 兜底。口径与拒绝理由的唯一实现住在
    /// `qx-api` 的 `admission::CorsPolicy::parse`，这里不复制第二份校验——
    /// 装配 `ApiService` 时调用它，坏源当场让 `serve` 起不来。
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
    /// 并发连接上限。一条连接一个线程，超限的连接当场收 503 而不是排队。
    ///
    /// `None` 表示用 `qx-api` 自己那个默认值（`DEFAULT_MAX_CONCURRENT_CONNECTIONS`）：
    /// 这个数字只有一个定义点，配置侧不再抄一份，否则改了默认值而配置模板里还写着旧的，
    /// 两侧就各讲一个上限。
    #[serde(default)]
    pub max_concurrent_connections: Option<usize>,
    /// 本机回环上的同源 BFF 控制台；`None` 表示这份部署不开控制台面（`qx-cli console` 拒绝启动）。
    #[serde(default)]
    pub console: Option<ConsoleRuntimeConfig>,
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
    ///
    /// 两种形状写的是互不相交的文件（`{name}.json` 与 `{name}.manifest.json` +
    /// `segments/`），改这个字段等于换一本账：同一 `storage.data_dir` 下留着另一本
    /// 历史时打开会当场拒绝，账户不会静默归零（`LiveEventPipeline` 的换后端闸门）。
    /// 分段换来的是按段归档与摘要校验，不是更省的稳态写入。
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
    /// 建立 NATS 连接的预算；此前只能退回 async-nats 自己的默认值。
    #[serde(default = "default_messaging_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// 一次 JetStream 请求（含发布确认）的预算。
    #[serde(default = "default_messaging_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// 空拉取的 batch expiry，决定 worker 多久能观察到停机令牌。
    #[serde(default = "default_messaging_pull_expires_ms")]
    pub pull_expires_ms: u64,
}

/// 三条 NATS 等待预算的默认值取自 `qx_storage::NatsWaitBudget::default()`，
/// 与它们所替换的依赖默认逐项相等；数值只在那一处写，改这里就会改运行时行为。
fn default_messaging_connect_timeout_ms() -> u64 {
    qx_storage::NatsWaitBudget::default().connect_timeout_ms
}

fn default_messaging_request_timeout_ms() -> u64 {
    qx_storage::NatsWaitBudget::default().request_timeout_ms
}

fn default_messaging_pull_expires_ms() -> u64 {
    qx_storage::NatsWaitBudget::default().pull_expires_ms
}

impl MessagingRuntimeConfig {
    /// 把配置面上的三条等待预算交给 NATS 适配器。
    pub fn nats_wait_budget(&self) -> qx_storage::NatsWaitBudget {
        qx_storage::NatsWaitBudget {
            connect_timeout_ms: self.connect_timeout_ms,
            request_timeout_ms: self.request_timeout_ms,
            pull_expires_ms: self.pull_expires_ms,
        }
    }
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
            connect_timeout_ms: default_messaging_connect_timeout_ms(),
            request_timeout_ms: default_messaging_request_timeout_ms(),
            pull_expires_ms: default_messaging_pull_expires_ms(),
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

impl RuntimeConfig {
    /// 「这份运行时配置是不是 production 档」的**唯一出口**。
    ///
    /// 词表由 [`ENVIRONMENT_VOCAB`] 单源、写法由 [`PRODUCTION_ENVIRONMENT`] 单源，判定由本方法
    /// 单源：14 处 production 专属加固闸门（运行时配置校验 9 处 + CLI 体检/就绪 5 处）必须全部
    /// 经由它。此前这 14 处各写一份 `environment.eq_ignore_ascii_case("production")`，危害不是
    /// 「现在算错」，而是改口径时只改到其中几份——漏掉的那一处加固会**静默失效**，而它守的
    /// 恰恰是「production 禁止明文 API / 必须配 Ed25519 公钥 / 必须配名义额上限」这类闸门。
    /// 同 `VenueId::is_binance` 先例（V13 §5 A3 / fam06）。
    pub fn is_production(&self) -> bool {
        self.environment
            .eq_ignore_ascii_case(PRODUCTION_ENVIRONMENT)
    }
}
