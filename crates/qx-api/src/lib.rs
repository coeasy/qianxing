//! 不依赖特定 Web 框架的本地 API 边界。
//!
//! 该层只做协议解析、权限入口和事件/快照查询，不直接修改 Ledger；写操作必须
//! 进入 `ControlPlane`，由上层执行器完成实际动作并回写审计。

use qx_control::{AuditRecord, ControlCommand, ControlError, ControlPlane, Permission};
use qx_core::{Event, EventLog, LedgerEntry};
use qx_protocol::{AccountSnapshot, ACCOUNT_SNAPSHOT_JSON_SCHEMA};
use qx_scheduler::JobRun;
use qx_storage::FileTokenBucket;
#[cfg(feature = "sqlite")]
use qx_storage::SqliteTokenBucket;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EventBusError {
    CapacityMustBePositive,
    SequenceGap { expected: u64, actual: u64 },
    CursorTooOld { requested: u64, oldest: u64 },
    CursorAhead { requested: u64, next_seq: u64 },
}

#[derive(Clone)]
pub struct ApiEventBus {
    inner: Arc<(Mutex<EventBusState>, Condvar)>,
}

struct EventBusState {
    capacity: usize,
    next_seq: u64,
    events: VecDeque<Event>,
}

impl Default for ApiEventBus {
    fn default() -> Self {
        Self::new(4096).expect("default API event bus capacity must be valid")
    }
}

impl ApiEventBus {
    pub fn new(capacity: usize) -> Result<Self, EventBusError> {
        if capacity == 0 {
            return Err(EventBusError::CapacityMustBePositive);
        }
        Ok(Self {
            inner: Arc::new((
                Mutex::new(EventBusState {
                    capacity,
                    next_seq: 0,
                    events: VecDeque::with_capacity(capacity),
                }),
                Condvar::new(),
            )),
        })
    }

    /// 发布不可变事件；事件序号必须是连续的，避免订阅者把丢序误当成当前状态。
    pub fn publish(&self, event: Event) -> Result<(), EventBusError> {
        let (lock, wake) = &*self.inner;
        let mut state = lock.lock().expect("event bus mutex poisoned");
        if event.seq != state.next_seq {
            return Err(EventBusError::SequenceGap {
                expected: state.next_seq,
                actual: event.seq,
            });
        }
        state.next_seq = state.next_seq.saturating_add(1);
        state.events.push_back(event);
        while state.events.len() > state.capacity {
            state.events.pop_front();
        }
        wake.notify_all();
        Ok(())
    }

    pub fn next_seq(&self) -> u64 {
        self.inner
            .0
            .lock()
            .expect("event bus mutex poisoned")
            .next_seq
    }

    pub fn read_after(&self, after: Option<u64>) -> Result<Vec<Event>, EventBusError> {
        let state = self.inner.0.lock().expect("event bus mutex poisoned");
        read_bus_events(&state, after)
    }

    /// 等待新事件；超时返回空批次，游标越界/超前始终显式报错。
    pub fn wait_after(
        &self,
        after: Option<u64>,
        timeout: Duration,
    ) -> Result<Vec<Event>, EventBusError> {
        let (lock, wake) = &*self.inner;
        let mut state = lock.lock().expect("event bus mutex poisoned");
        let deadline = Instant::now() + timeout;
        loop {
            let events = read_bus_events(&state, after)?;
            if !events.is_empty() {
                return Ok(events);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Vec::new());
            }
            let (next, result) = wake
                .wait_timeout(state, remaining)
                .expect("event bus condvar poisoned");
            state = next;
            if result.timed_out() {
                return Ok(Vec::new());
            }
        }
    }
}

fn read_bus_events(state: &EventBusState, after: Option<u64>) -> Result<Vec<Event>, EventBusError> {
    if let Some(after) = after {
        if after >= state.next_seq {
            return Err(EventBusError::CursorAhead {
                requested: after,
                next_seq: state.next_seq,
            });
        }
        if let Some(oldest) = state.events.front().map(|event| event.seq) {
            if after.saturating_add(1) < oldest {
                return Err(EventBusError::CursorTooOld {
                    requested: after,
                    oldest,
                });
            }
        }
        Ok(state
            .events
            .iter()
            .filter(|event| event.seq > after)
            .cloned()
            .collect())
    } else {
        Ok(state.events.iter().cloned().collect())
    }
}

/// 可热替换的 TLS 服务端配置。新连接读取最新配置，已有连接继续使用握手时的配置。
#[derive(Clone)]
pub struct TlsConfigStore {
    current: Arc<RwLock<Arc<ServerConfig>>>,
}

impl TlsConfigStore {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            current: Arc::new(RwLock::new(config)),
        }
    }

    pub fn current(&self) -> Arc<ServerConfig> {
        Arc::clone(&self.current.read().expect("TLS config store lock poisoned"))
    }

    pub fn replace(&self, config: Arc<ServerConfig>) {
        *self
            .current
            .write()
            .expect("TLS config store lock poisoned") = config;
    }
}

/// 构造要求客户端证书的 TLS 服务端配置；证书解析、私钥加载和信任根管理由部署边界负责。
pub fn build_mtls_server_config(
    client_roots: RootCertStore,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, String> {
    let verifier = WebPkiClientVerifier::builder(Arc::new(client_roots))
        .build()
        .map_err(|error| format!("mTLS 客户端证书校验器构造失败: {error}"))?;
    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificate_chain, private_key)
        .map_err(|error| format!("mTLS 服务端证书配置非法: {error}"))?;
    Ok(Arc::new(config))
}

/// 从 PEM 文件加载 mTLS 服务端配置。
///
/// 该函数只负责把部署提供的证书链、私钥和客户端 CA 解析成 rustls 配置；
/// 私钥权限、秘密管理和证书签发仍由部署系统负责。解析失败会在替换配置前返回，
/// 因而不会破坏 `TlsConfigStore` 中当前仍可用的配置。
pub fn load_mtls_server_config_from_pem(
    certificate_chain_path: impl AsRef<Path>,
    private_key_path: impl AsRef<Path>,
    client_ca_path: impl AsRef<Path>,
) -> Result<Arc<ServerConfig>, String> {
    let certificate_chain = CertificateDer::pem_file_iter(certificate_chain_path.as_ref())
        .map_err(|error| format!("读取 mTLS 服务端证书链失败: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 mTLS 服务端证书链失败: {error}"))?;
    if certificate_chain.is_empty() {
        return Err("mTLS 服务端证书链不能为空".into());
    }

    let private_key = PrivateKeyDer::from_pem_file(private_key_path.as_ref())
        .map_err(|error| format!("解析 mTLS 服务端私钥失败: {error}"))?;
    let mut client_roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_file_iter(client_ca_path.as_ref())
        .map_err(|error| format!("读取 mTLS 客户端 CA 失败: {error}"))?
    {
        let certificate =
            certificate.map_err(|error| format!("解析 mTLS 客户端 CA 失败: {error}"))?;
        client_roots
            .add(certificate)
            .map_err(|error| format!("加入 mTLS 客户端 CA 失败: {error}"))?;
    }
    if client_roots.is_empty() {
        return Err("mTLS 客户端 CA 不能为空".into());
    }
    build_mtls_server_config(client_roots, certificate_chain, private_key)
}

/// 加载一个 PEM 文件中的证书链；主要用于构造 mTLS 证书到 Operator 的显式映射。
pub fn load_certificate_chain_from_pem(
    certificate_chain_path: impl AsRef<Path>,
) -> Result<Vec<CertificateDer<'static>>, String> {
    let certificates = CertificateDer::pem_file_iter(certificate_chain_path.as_ref())
        .map_err(|error| format!("读取 PEM 证书链失败: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 PEM 证书链失败: {error}"))?;
    if certificates.is_empty() {
        return Err("PEM 证书链不能为空".into());
    }
    Ok(certificates)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PemFileStamp {
    modified: Option<SystemTime>,
    length: u64,
}

impl PemFileStamp {
    fn read(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("读取证书文件元数据失败 {}: {error}", path.display()))?;
        Ok(Self {
            modified: metadata.modified().ok(),
            length: metadata.len(),
        })
    }
}

/// 基于文件元数据的可轮询 TLS 配置重载器。
///
/// 它不自行创建后台线程，调用方可在配置管理器或服务主循环中周期性调用
/// `reload_if_changed`。新文件解析失败时保留旧配置，避免证书轮换的中间态导致服务中断。
#[derive(Clone)]
pub struct TlsPemReloader {
    certificate_chain_path: PathBuf,
    private_key_path: PathBuf,
    client_ca_path: PathBuf,
    last_stamp: Arc<Mutex<Option<(PemFileStamp, PemFileStamp, PemFileStamp)>>>,
}

impl TlsPemReloader {
    pub fn new(
        certificate_chain_path: impl Into<PathBuf>,
        private_key_path: impl Into<PathBuf>,
        client_ca_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            certificate_chain_path: certificate_chain_path.into(),
            private_key_path: private_key_path.into(),
            client_ca_path: client_ca_path.into(),
            last_stamp: Arc::new(Mutex::new(None)),
        }
    }

    pub fn load(&self) -> Result<Arc<ServerConfig>, String> {
        load_mtls_server_config_from_pem(
            &self.certificate_chain_path,
            &self.private_key_path,
            &self.client_ca_path,
        )
    }

    /// 检查三份 PEM 文件是否发生变化；成功加载并替换时返回 `true`。
    pub fn reload_if_changed(&self, store: &TlsConfigStore) -> Result<bool, String> {
        let stamp = (
            PemFileStamp::read(&self.certificate_chain_path)?,
            PemFileStamp::read(&self.private_key_path)?,
            PemFileStamp::read(&self.client_ca_path)?,
        );
        {
            let last = self
                .last_stamp
                .lock()
                .expect("TLS PEM reloader lock poisoned");
            if last.as_ref() == Some(&stamp) {
                return Ok(false);
            }
        }
        let config = self.load()?;
        store.replace(config);
        *self
            .last_stamp
            .lock()
            .expect("TLS PEM reloader lock poisoned") = Some(stamp);
        Ok(true)
    }
}

/// mTLS 客户端证书到可信操作员身份的显式映射。
///
/// 映射使用完整 DER 证书作为键，避免把证书主题名当作唯一身份；证书轮换时
/// 必须显式加入新证书并通过 `TlsConfigStore` 替换服务端信任配置。
#[derive(Clone, Default)]
pub struct MtlsIdentityPolicy {
    identities: BTreeMap<Vec<u8>, String>,
}

impl MtlsIdentityPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant_certificate(
        mut self,
        certificate: CertificateDer<'static>,
        operator_id: impl Into<String>,
    ) -> Result<Self, String> {
        let operator_id = operator_id.into();
        if operator_id.trim().is_empty() || certificate.as_ref().is_empty() {
            return Err("mTLS 证书或 operator_id 不能为空".into());
        }
        self.identities
            .insert(certificate.as_ref().to_vec(), operator_id);
        Ok(self)
    }

    pub fn operator_for(&self, certificates: Option<&[CertificateDer<'static>]>) -> Option<&str> {
        let certificate = certificates?.first()?;
        self.identities
            .get(certificate.as_ref())
            .map(String::as_str)
    }
}

/// 可热替换的 mTLS Operator 身份映射。
///
/// 新连接握手时读取当前策略；已有连接的身份不会被重新解释。策略替换只能
/// 使用调用方已经校验过的完整 DER 证书映射，不能通过主题名或请求体自声明身份。
#[derive(Clone)]
pub struct MtlsIdentityStore {
    current: Arc<RwLock<Arc<MtlsIdentityPolicy>>>,
}

impl MtlsIdentityStore {
    pub fn new(policy: MtlsIdentityPolicy) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(policy))),
        }
    }

    pub fn current(&self) -> Arc<MtlsIdentityPolicy> {
        let current = self
            .current
            .read()
            .expect("mTLS identity store lock poisoned");
        Arc::clone(&current)
    }

    pub fn replace(&self, policy: MtlsIdentityPolicy) {
        *self
            .current
            .write()
            .expect("mTLS identity store lock poisoned") = Arc::new(policy);
    }
}

/// 从多个 Operator 的 PEM 证书链构造完整身份映射。
pub fn load_mtls_identity_policy_from_pem(
    operators: &BTreeMap<String, PathBuf>,
) -> Result<MtlsIdentityPolicy, String> {
    let mut policy = MtlsIdentityPolicy::new();
    for (operator_id, certificate_path) in operators {
        let certificate = load_certificate_chain_from_pem(certificate_path)?
            .into_iter()
            .next()
            .ok_or_else(|| format!("Operator {operator_id} 证书链为空"))?;
        policy = policy.grant_certificate(certificate, operator_id.clone())?;
    }
    Ok(policy)
}

/// 基于 Operator PEM 文件元数据的可轮询身份策略重载器。
#[derive(Clone)]
pub struct MtlsIdentityPemReloader {
    operators: BTreeMap<String, PathBuf>,
    last_stamps: Arc<Mutex<Option<BTreeMap<String, PemFileStamp>>>>,
}

impl MtlsIdentityPemReloader {
    pub fn new(operators: BTreeMap<String, PathBuf>) -> Result<Self, String> {
        if operators.is_empty() {
            return Err("mTLS Operator 映射不能为空".into());
        }
        if operators.keys().any(|id| id.trim().is_empty()) {
            return Err("mTLS Operator id 不能为空".into());
        }
        Ok(Self {
            operators,
            last_stamps: Arc::new(Mutex::new(None)),
        })
    }

    pub fn load(&self) -> Result<MtlsIdentityPolicy, String> {
        load_mtls_identity_policy_from_pem(&self.operators)
    }

    /// 证书文件变更且新策略全部解析成功时替换；任一文件失败则保留旧策略。
    pub fn reload_if_changed(&self, store: &MtlsIdentityStore) -> Result<bool, String> {
        let mut stamps = BTreeMap::new();
        for (operator_id, path) in &self.operators {
            stamps.insert(operator_id.clone(), PemFileStamp::read(path)?);
        }
        {
            let last = self
                .last_stamps
                .lock()
                .expect("mTLS identity reloader lock poisoned");
            if last.as_ref() == Some(&stamps) {
                return Ok(false);
            }
        }
        let policy = self.load()?;
        store.replace(policy);
        *self
            .last_stamps
            .lock()
            .expect("mTLS identity reloader lock poisoned") = Some(stamps);
        Ok(true)
    }
}

#[derive(Default)]
pub struct ApiState {
    pub snapshot: Option<AccountSnapshot>,
    snapshot_history: BTreeMap<u64, AccountSnapshot>,
    pub events: EventLog,
    pub event_bus: ApiEventBus,
    pub control: ControlPlane,
    /// 只读运维读模型；调度、账簿和对账事实仍由各自 owner 写入。
    pub job_runs: Vec<JobRun>,
    pub ledger_entries: Vec<LedgerEntry>,
    pub reconcile_reports: BTreeMap<String, ReconcileReportSnapshot>,
}

#[derive(Clone, Debug)]
pub struct ApiRateLimiter {
    capacity: u64,
    tokens: u64,
    refill_per_second: u64,
    last_ts: u64,
}

impl ApiRateLimiter {
    pub fn new(capacity: u64, refill_per_second: u64) -> Self {
        assert!(capacity > 0, "API rate limit capacity must be positive");
        Self {
            capacity,
            tokens: capacity,
            refill_per_second,
            last_ts: 0,
        }
    }

    /// `now` 使用 API 调用方约定的秒级单调时间；生产网关应在边界统一时间单位。
    pub fn try_acquire(&mut self, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.last_ts);
        let refill = elapsed.saturating_mul(self.refill_per_second);
        self.tokens = self.tokens.saturating_add(refill).min(self.capacity);
        self.last_ts = now;
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

trait ApiRateLimitBackend: Send + Sync {
    fn try_acquire(&self, now: u64) -> Result<bool, String>;
}

struct LocalRateLimitBackend {
    limiter: Mutex<ApiRateLimiter>,
}

impl ApiRateLimitBackend for LocalRateLimitBackend {
    fn try_acquire(&self, now: u64) -> Result<bool, String> {
        self.limiter
            .lock()
            .map_err(|_| "api rate limiter mutex poisoned".to_string())
            .map(|mut limiter| limiter.try_acquire(now))
    }
}

struct SharedFileRateLimitBackend {
    bucket: FileTokenBucket,
}

#[cfg(feature = "sqlite")]
struct SharedSqliteRateLimitBackend {
    bucket: SqliteTokenBucket,
}

#[cfg(feature = "sqlite")]
impl ApiRateLimitBackend for SharedSqliteRateLimitBackend {
    fn try_acquire(&self, now: u64) -> Result<bool, String> {
        self.bucket
            .try_acquire(now, 1)
            .map_err(|error| format!("SQLite API rate limiter unavailable: {error:?}"))
    }
}

impl ApiRateLimitBackend for SharedFileRateLimitBackend {
    fn try_acquire(&self, now: u64) -> Result<bool, String> {
        self.bucket
            .try_acquire(now, 1)
            .map_err(|error| format!("shared API rate limiter unavailable: {error:?}"))
    }
}

impl ApiState {
    pub fn publish_snapshot(&mut self, mut snapshot: AccountSnapshot) -> Result<u64, String> {
        snapshot.header.state_hash = 0;
        snapshot
            .validate()
            .map_err(|error| format!("snapshot invalid: {error:?}"))?;
        snapshot.seal();
        let hash = snapshot.state_hash();
        self.snapshot_history.insert(hash, snapshot.clone());
        self.snapshot = Some(snapshot);
        Ok(hash)
    }

    /// 事件同时写入历史日志和实时总线；外部恢复代码若直接装载 EventLog，
    /// 应在完成恢复后按序重新发布到 event_bus。
    pub fn publish_event(&mut self, event: Event) -> Result<(), String> {
        self.events
            .append_checked(event.clone())
            .map_err(|error| format!("event log rejected event: {error:?}"))?;
        self.event_bus
            .publish(event)
            .map_err(|error| format!("event bus rejected event: {error:?}"))
    }
}

/// API 层的服务端身份映射。命令体中的 `permission` 仅用于审计，不能替代此表。
#[derive(Clone, Default)]
pub struct ApiPolicy {
    operators: BTreeMap<String, Permission>,
}

impl ApiPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant(mut self, operator_id: impl Into<String>, permission: Permission) -> Self {
        self.operators.insert(operator_id.into(), permission);
        self
    }

    pub fn permission(&self, operator_id: &str) -> Option<Permission> {
        self.operators.get(operator_id).copied()
    }
}

#[derive(Clone)]
pub struct ApiService {
    state: Arc<Mutex<ApiState>>,
    /// None 表示仅供已受信的进程内调用；网络/生产入口应使用 `with_policy`。
    policy: Option<ApiPolicy>,
    rate_limiter: Arc<dyn ApiRateLimitBackend>,
    control_submitter: Option<ControlSubmitter>,
    command_enqueuer: Option<CommandEnqueuer>,
    metrics: Arc<ApiMetrics>,
    worker_metrics_provider: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    readiness_provider: Option<ReadinessProvider>,
}

#[derive(Default)]
struct ApiMetrics {
    requests_total: AtomicU64,
    rate_limit_rejected_total: AtomicU64,
    authentication_rejected_total: AtomicU64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ApiMetricsSnapshot {
    pub requests_total: u64,
    pub rate_limit_rejected_total: u64,
    pub authentication_rejected_total: u64,
}

impl ApiMetricsSnapshot {
    pub fn to_prometheus(self) -> String {
        format!(
            "# HELP qx_api_requests_total Total API requests received.\\n\
# TYPE qx_api_requests_total counter\\n\
qx_api_requests_total {}\\n\
# HELP qx_api_rate_limit_rejected_total Requests rejected by the rate limiter.\\n\
# TYPE qx_api_rate_limit_rejected_total counter\\n\
qx_api_rate_limit_rejected_total {}\\n\
# HELP qx_api_authentication_rejected_total Requests rejected by the API policy.\\n\
# TYPE qx_api_authentication_rejected_total counter\\n\
qx_api_authentication_rejected_total {}\\n",
            self.requests_total, self.rate_limit_rejected_total, self.authentication_rejected_total
        )
    }
}

#[derive(Debug)]
pub enum ControlSubmitError {
    Rejected(ControlError),
    Unavailable(String),
}

type ControlSubmitter = Arc<
    dyn Fn(
            ControlCommand,
            Permission,
            u64,
        ) -> Result<(ControlPlane, AuditRecord), ControlSubmitError>
        + Send
        + Sync,
>;
type CommandEnqueuer = Arc<dyn Fn(ControlCommand, u64) -> Result<(), String> + Send + Sync>;
type ReadinessProvider = Arc<dyn Fn() -> ApiReadiness + Send + Sync>;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ApiResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ApiReadiness {
    pub ready: bool,
    pub detail: String,
}

/// 对账读模型。订单差异和余额差异保留为 JSON 事实，避免 API 层依赖某个
/// Venue 适配器的枚举；写入前由 Reconcile worker 生成并校验 schema_version。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReconcileReportSnapshot {
    pub schema_version: u32,
    pub worker_id: String,
    pub account_id: String,
    pub venue_id: String,
    pub observed_ts: u64,
    #[serde(default)]
    pub order_issues: Vec<serde_json::Value>,
    pub balances_count: usize,
    #[serde(default)]
    pub balance_discrepancies: Vec<serde_json::Value>,
    pub position_snapshots_count: usize,
    pub funding_rate_snapshots_count: usize,
    pub cashflow_count: usize,
}

impl ReconcileReportSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.worker_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
        {
            return Err("对账报告身份或 schema_version 非法".into());
        }
        Ok(())
    }
}

/// API 查询端口。网络协议只依赖这些只读方法，不应直接拿到 Ledger、Venue
/// 或可变控制面引用；未来替换为独立 QueryService 时保持同一契约。
pub trait QueryPort {
    fn account_snapshot(&self) -> Option<AccountSnapshot>;
    fn account_orders(&self) -> Vec<qx_protocol::OrderSnapshot>;
    fn account_positions(&self) -> Vec<qx_protocol::PositionSnapshot>;
    fn account_cash(&self) -> BTreeMap<String, i128>;
    fn control_audit(&self) -> Vec<AuditRecord>;
    fn events_after(&self, after: Option<u64>) -> Result<Vec<Event>, EventBusError>;
    fn job_runs(&self) -> Vec<JobRun>;
    fn ledger_entries(&self) -> Vec<LedgerEntry>;
    fn reconcile_reports(&self) -> Vec<ReconcileReportSnapshot>;
}

/// API 控制端口。提交之后是否进入队列、调用哪个 Venue 和如何归约事实，
/// 由外部 Control/Execution worker 决定；API 只返回 Accepted 审计事实。
pub trait ControlPort {
    fn submit_control(
        &self,
        command: ControlCommand,
        ts: u64,
        authenticated_operator: Option<&str>,
    ) -> ApiResponse;
}

impl ApiResponse {
    fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8".into(),
            body: body.into(),
        }
    }

    fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8".into(),
            body: body.into(),
        }
    }

    fn prometheus(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/plain; version=0.0.4; charset=utf-8".into(),
            body: body.into(),
        }
    }
}

impl ApiService {
    pub fn new(state: ApiState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            policy: None,
            rate_limiter: Arc::new(LocalRateLimitBackend {
                limiter: Mutex::new(ApiRateLimiter::new(100, 100)),
            }),
            control_submitter: None,
            command_enqueuer: None,
            metrics: Arc::new(ApiMetrics::default()),
            worker_metrics_provider: None,
            readiness_provider: None,
        }
    }

    pub fn with_policy(state: ApiState, policy: ApiPolicy) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            policy: Some(policy),
            rate_limiter: Arc::new(LocalRateLimitBackend {
                limiter: Mutex::new(ApiRateLimiter::new(100, 100)),
            }),
            control_submitter: None,
            command_enqueuer: None,
            metrics: Arc::new(ApiMetrics::default()),
            worker_metrics_provider: None,
            readiness_provider: None,
        }
    }

    pub fn state(&self) -> Arc<Mutex<ApiState>> {
        Arc::clone(&self.state)
    }

    pub fn metrics(&self) -> ApiMetricsSnapshot {
        ApiMetricsSnapshot {
            requests_total: self.metrics.requests_total.load(Ordering::Relaxed),
            rate_limit_rejected_total: self
                .metrics
                .rate_limit_rejected_total
                .load(Ordering::Relaxed),
            authentication_rejected_total: self
                .metrics
                .authentication_rejected_total
                .load(Ordering::Relaxed),
        }
    }

    /// Append metrics emitted by supervised worker processes. The provider is
    /// intentionally read-only and evaluated only for `/metrics`, so a dead
    /// worker cannot block the API control/query path.
    pub fn with_worker_metrics_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        self.worker_metrics_provider = Some(Arc::new(provider));
        self
    }

    /// Configure dependency-aware readiness. `/health` remains a cheap
    /// liveness endpoint; `/ready` uses this provider and returns 503 when the
    /// API should not receive traffic.
    pub fn with_readiness_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> ApiReadiness + Send + Sync + 'static,
    {
        self.readiness_provider = Some(Arc::new(provider));
        self
    }

    pub fn query_port(&self) -> &dyn QueryPort {
        self
    }

    pub fn control_port(&self) -> &dyn ControlPort {
        self
    }

    /// 将控制命令提交委托给持久化事务；回调应在返回前完成权限、幂等和原子保存。
    pub fn with_control_submitter<F>(mut self, submitter: F) -> Self
    where
        F: Fn(
                ControlCommand,
                Permission,
                u64,
            ) -> Result<(ControlPlane, AuditRecord), ControlSubmitError>
            + Send
            + Sync
            + 'static,
    {
        self.control_submitter = Some(Arc::new(submitter));
        self
    }

    /// 控制面提交成功后，把命令放入可恢复执行队列。队列失败不会撤销已持久化的
    /// Accepted 命令，执行器启动时会扫描控制面 pending 命令重新补入队列。
    pub fn with_command_enqueuer<F>(mut self, enqueuer: F) -> Self
    where
        F: Fn(ControlCommand, u64) -> Result<(), String> + Send + Sync + 'static,
    {
        self.command_enqueuer = Some(Arc::new(enqueuer));
        self
    }

    pub fn publish_event(&self, event: Event) -> Result<(), String> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .publish_event(event)
    }

    pub fn with_rate_limit(mut self, capacity: u64, refill_per_second: u64) -> Self {
        self.rate_limiter = Arc::new(LocalRateLimitBackend {
            limiter: Mutex::new(ApiRateLimiter::new(capacity, refill_per_second)),
        });
        self
    }

    /// 使用共享文件系统持久化令牌桶；多个进程可共享同一限流状态。
    pub fn with_shared_file_rate_limit(mut self, bucket: FileTokenBucket) -> Self {
        self.rate_limiter = Arc::new(SharedFileRateLimitBackend { bucket });
        self
    }

    /// 使用 SQLite 事务令牌桶；启用 `qx-api/sqlite` feature 后可跨进程共享。
    #[cfg(feature = "sqlite")]
    pub fn with_sqlite_rate_limit(mut self, bucket: SqliteTokenBucket) -> Self {
        self.rate_limiter = Arc::new(SharedSqliteRateLimitBackend { bucket });
        self
    }

    pub fn handle(&self, method: &str, path: &str, body: &str, ts: u64) -> ApiResponse {
        self.handle_inner(method, path, body, ts, None)
    }

    /// 带可信身份的入口。`operator_id` 必须由认证网关/进程边界注入，不能来自命令体。
    pub fn handle_as(
        &self,
        operator_id: &str,
        method: &str,
        path: &str,
        body: &str,
        ts: u64,
    ) -> ApiResponse {
        self.handle_inner(method, path, body, ts, Some(operator_id))
    }

    fn handle_inner(
        &self,
        method: &str,
        path: &str,
        body: &str,
        ts: u64,
        authenticated_operator: Option<&str>,
    ) -> ApiResponse {
        self.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
        match self.rate_limiter.try_acquire(ts) {
            Ok(true) => {}
            Ok(false) => {
                self.metrics
                    .rate_limit_rejected_total
                    .fetch_add(1, Ordering::Relaxed);
                return ApiResponse::json(429, error_json("api_rate_limit_exceeded"));
            }
            Err(error) => {
                return ApiResponse::json(
                    503,
                    error_json(&format!("api_rate_limit_backend_unavailable: {error}")),
                )
            }
        }
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        if self.policy.is_some()
            && !matches!(route, "/health" | "/ready" | "/schema/account-snapshot-v1")
        {
            let authorized = authenticated_operator
                .and_then(|operator| self.policy.as_ref()?.permission(operator))
                .is_some();
            if !authorized {
                self.metrics
                    .authentication_rejected_total
                    .fetch_add(1, Ordering::Relaxed);
                return ApiResponse::json(403, error_json("authenticated_operator_required"));
            }
        }
        match (method, route) {
            ("GET", "/health") => ApiResponse::json(200, "{\"status\":\"ok\"}"),
            ("GET", "/ready") => {
                let readiness = self
                    .readiness_provider
                    .as_ref()
                    .map(|provider| provider())
                    .unwrap_or(ApiReadiness {
                        ready: true,
                        detail: "api_ready".into(),
                    });
                let status = if readiness.ready { 200 } else { 503 };
                ApiResponse::json(
                    status,
                    serde_json::to_string(&readiness)
                        .expect("API readiness snapshot is serializable"),
                )
            }
            ("GET", "/metrics") => {
                let mut body = self.metrics().to_prometheus();
                if let Some(provider) = &self.worker_metrics_provider {
                    body.push_str(&provider());
                }
                ApiResponse::prometheus(body)
            }
            ("GET", "/schema/account-snapshot-v1") => {
                ApiResponse::json(200, ACCOUNT_SNAPSHOT_JSON_SCHEMA)
            }
            ("GET", "/account/snapshot") => {
                let state = self.state.lock().expect("api state mutex poisoned");
                match &state.snapshot {
                    Some(snapshot) => ApiResponse::json(200, snapshot.to_json()),
                    None => ApiResponse::json(404, "{\"error\":\"snapshot_not_found\"}"),
                }
            }
            ("GET", "/account/orders") => {
                let orders = self.account_orders();
                ApiResponse::json(
                    200,
                    serde_json::to_string(&orders).expect("order snapshots are serializable"),
                )
            }
            ("GET", "/account/positions") => {
                let positions = self.account_positions();
                ApiResponse::json(
                    200,
                    serde_json::to_string(&positions).expect("position snapshots are serializable"),
                )
            }
            ("GET", "/account/balances") => {
                let state = self.state.lock().expect("api state mutex poisoned");
                let body = serde_json::json!({
                    "cash_raw": state.snapshot.as_ref().map(|snapshot| &snapshot.cash_raw).cloned().unwrap_or_default(),
                    "equity_raw": state.snapshot.as_ref().map(|snapshot| snapshot.equity_raw),
                    "available_raw": state.snapshot.as_ref().map(|snapshot| snapshot.available_raw),
                    "margin_raw": state.snapshot.as_ref().map(|snapshot| snapshot.margin_raw),
                });
                ApiResponse::json(200, body.to_string())
            }
            ("GET", "/control/audit") => ApiResponse::json(
                200,
                serde_json::to_string(&self.control_audit()).expect("audit is serializable"),
            ),
            ("GET", "/scheduler/runs") => ApiResponse::json(
                200,
                serde_json::to_string(&self.job_runs()).expect("job runs are serializable"),
            ),
            ("GET", "/account/ledger") => ApiResponse::json(
                200,
                serde_json::to_string(&self.ledger_entries())
                    .expect("ledger entries are serializable"),
            ),
            ("GET", "/reconcile/reports") => ApiResponse::json(
                200,
                serde_json::to_string(&self.reconcile_reports())
                    .expect("reconcile reports are serializable"),
            ),
            ("GET", "/account/snapshot/diff") => self.snapshot_diff(query),
            ("GET", "/events") => {
                let state = self.state.lock().expect("api state mutex poisoned");
                let after = match query_value(query, "after") {
                    None => u64::MAX,
                    Some(value) => match value.parse::<u64>() {
                        Ok(value) => value,
                        Err(_) => {
                            return ApiResponse::json(
                                400,
                                error_json("after must be an unsigned integer"),
                            )
                        }
                    },
                };
                if after != u64::MAX && after >= state.events.next_seq() {
                    return ApiResponse::json(409, error_json("event_cursor_requires_snapshot"));
                }
                let events = if after == u64::MAX {
                    state.events.events()
                } else {
                    &state.events.events()[(after as usize + 1).min(state.events.len())..]
                };
                match serde_json::to_string(events) {
                    Ok(events) => ApiResponse::json(200, events),
                    Err(error) => ApiResponse::json(500, error_json(&error.to_string())),
                }
            }
            ("GET", "/events/live") => self.live_events(query),
            ("POST", "/control/commands") => self.submit_command(body, ts, authenticated_operator),
            _ => ApiResponse::text(404, "not found"),
        }
    }

    fn live_events(&self, query: &str) -> ApiResponse {
        let after = match query_value(query, "after") {
            None => None,
            Some(value) => match value.parse::<u64>() {
                Ok(value) => Some(value),
                Err(_) => {
                    return ApiResponse::json(400, error_json("after must be an unsigned integer"))
                }
            },
        };
        let state = self.state.lock().expect("api state mutex poisoned");
        match state.event_bus.read_after(after) {
            Ok(events) => ApiResponse::json(
                200,
                serde_json::to_string(&events).expect("event bus events are serializable"),
            ),
            Err(EventBusError::CursorTooOld { .. } | EventBusError::CursorAhead { .. }) => {
                ApiResponse::json(409, error_json("event_cursor_requires_snapshot"))
            }
            Err(error) => ApiResponse::json(500, error_json(&format!("{error:?}"))),
        }
    }

    fn snapshot_diff(&self, query: &str) -> ApiResponse {
        let Some(base_hash) = query_value(query, "base_hash") else {
            return ApiResponse::json(400, error_json("base_hash is required"));
        };
        let Ok(base_hash) = base_hash.parse::<u64>() else {
            return ApiResponse::json(400, error_json("base_hash must be an unsigned integer"));
        };
        let state = self.state.lock().expect("api state mutex poisoned");
        let Some(base) = state.snapshot_history.get(&base_hash) else {
            return ApiResponse::json(409, error_json("snapshot_base_not_found"));
        };
        let Some(target) = state.snapshot.as_ref() else {
            return ApiResponse::json(404, error_json("snapshot_not_found"));
        };
        match base.diff(target) {
            Ok(diff) => ApiResponse::json(
                200,
                serde_json::to_string(&diff).expect("snapshot diff is serializable"),
            ),
            Err(error) => ApiResponse::json(409, error_json(&format!("{error:?}"))),
        }
    }

    fn submit_command(
        &self,
        body: &str,
        ts: u64,
        authenticated_operator: Option<&str>,
    ) -> ApiResponse {
        let mut command: ControlCommand = match serde_json::from_str(body) {
            Ok(command) => command,
            Err(error) => return ApiResponse::json(400, error_json(&error.to_string())),
        };
        let granted = match &self.policy {
            Some(policy) => {
                let Some(operator_id) =
                    authenticated_operator.filter(|operator_id| !operator_id.trim().is_empty())
                else {
                    return ApiResponse::json(403, error_json("authenticated_operator_required"));
                };
                command.operator_id = operator_id.to_string();
                match policy.permission(operator_id) {
                    Some(permission) => permission,
                    None => return ApiResponse::json(403, error_json("forbidden")),
                }
            }
            None => command.permission,
        };
        if let Some(submitter) = &self.control_submitter {
            let queued_command = command.clone();
            let (plane, audit) = match submitter(command, granted, ts) {
                Ok(result) => result,
                Err(ControlSubmitError::Unavailable(error)) => {
                    return ApiResponse::json(
                        503,
                        error_json(&format!("control_state_unavailable: {error}")),
                    )
                }
                Err(ControlSubmitError::Rejected(error)) => {
                    let status = match error {
                        ControlError::Forbidden => 403,
                        ControlError::Invalid(_) => 400,
                        _ => 409,
                    };
                    return ApiResponse::json(status, error_json(&format!("{error:?}")));
                }
            };
            self.state.lock().expect("api state mutex poisoned").control = plane;
            if let Some(enqueuer) = &self.command_enqueuer {
                let _ = enqueuer(queued_command, ts);
            }
            return ApiResponse::json(
                202,
                serde_json::to_string(&audit).expect("audit is serializable"),
            );
        }
        let mut state = self.state.lock().expect("api state mutex poisoned");
        let result = state.control.submit_as(command, granted, ts);
        match result {
            Ok(audit) => ApiResponse::json(
                202,
                serde_json::to_string(&audit).expect("audit is serializable"),
            ),
            Err(error) => {
                let status = match error {
                    ControlError::Forbidden => 403,
                    ControlError::Invalid(_) => 400,
                    _ => 409,
                };
                ApiResponse::json(status, error_json(&format!("{error:?}")))
            }
        }
    }

    /// 处理一个 HTTP/1.1 请求；用于本地控制面和集成测试。
    pub fn serve_once(&self, listener: &TcpListener, ts: u64) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        self.serve_stream_as(stream, ts, None)
    }

    /// 网络入口的可信身份版本；身份应由已认证的上游边界注入。
    pub fn serve_once_as(
        &self,
        listener: &TcpListener,
        ts: u64,
        operator_id: &str,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        self.serve_stream_as(stream, ts, Some(operator_id))
    }

    /// 使用调用方提供的证书和私钥配置服务端 TLS。
    ///
    /// `ServerConfig` 必须由部署边界构造并安全加载证书/私钥；API 层不提供跳过
    /// TLS 或动态信任任何客户端的快捷开关。HTTP 与 WebSocket 处理仍复用同一套
    /// 权限、审计、快照和事件游标语义。
    pub fn serve_once_tls(
        &self,
        listener: &TcpListener,
        config: Arc<ServerConfig>,
        ts: u64,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        self.serve_stream(tls_stream(stream, config)?, ts)
    }

    /// 带可信身份注入的 TLS 单请求入口。
    pub fn serve_once_tls_as(
        &self,
        listener: &TcpListener,
        config: Arc<ServerConfig>,
        ts: u64,
        operator_id: &str,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        self.serve_stream_as(tls_stream(stream, config)?, ts, Some(operator_id))
    }

    /// 使用可热替换配置处理单个 TLS 连接。
    pub fn serve_once_tls_with_store(
        &self,
        listener: &TcpListener,
        configs: &TlsConfigStore,
        ts: u64,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        self.serve_stream(tls_stream(stream, configs.current())?, ts)
    }

    /// 使用可热替换配置持续处理 TLS 连接。
    pub fn serve_tls_with_store(
        &self,
        listener: TcpListener,
        configs: &TlsConfigStore,
        ts: u64,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            self.serve_stream(tls_stream(stream, configs.current())?, ts)?;
        }
        Ok(())
    }

    /// 使用可热替换配置持续处理 TLS 连接，并注入可信操作员身份。
    pub fn serve_tls_with_store_as(
        &self,
        listener: TcpListener,
        configs: &TlsConfigStore,
        ts: u64,
        operator_id: &str,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            self.serve_stream_as(
                tls_stream(stream, configs.current())?,
                ts,
                Some(operator_id),
            )?;
        }
        Ok(())
    }

    fn serve_stream<S>(&self, stream: S, ts: u64) -> std::io::Result<()>
    where
        S: Read + Write,
    {
        self.serve_stream_as(stream, ts, None)
    }

    fn serve_stream_as<S>(
        &self,
        mut stream: S,
        ts: u64,
        authenticated_operator: Option<&str>,
    ) -> std::io::Result<()>
    where
        S: Read + Write,
    {
        let request = read_request(&mut stream)?;
        let request = String::from_utf8_lossy(&request);
        self.dispatch_request(&mut stream, &request, ts, authenticated_operator)
    }

    fn dispatch_request<S: Read + Write>(
        &self,
        stream: &mut S,
        request: &str,
        ts: u64,
        authenticated_operator: Option<&str>,
    ) -> std::io::Result<()> {
        if request.to_ascii_lowercase().contains("upgrade: websocket") {
            if self.policy.is_some()
                && authenticated_operator
                    .and_then(|operator| self.policy.as_ref()?.permission(operator))
                    .is_none()
            {
                write_http_response(
                    stream,
                    &ApiResponse::json(403, error_json("authenticated_operator_required")),
                )?;
                return Ok(());
            }
            return self.serve_websocket(stream, request);
        }
        let response = match parse_http_request(request) {
            Ok(parsed) => {
                self.handle_inner(&parsed.0, &parsed.1, &parsed.2, ts, authenticated_operator)
            }
            Err(error) => ApiResponse::json(400, error_json(&error)),
        };
        write_http_response(stream, &response)
    }

    /// 使用 mTLS 客户端证书自动解析可信操作员身份的单连接入口。
    pub fn serve_once_tls_mtls(
        &self,
        listener: &TcpListener,
        config: Arc<ServerConfig>,
        identities: &MtlsIdentityPolicy,
        ts: u64,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        let mut stream = tls_stream(stream, config)?;
        let request = read_request(&mut stream)?;
        let request = String::from_utf8_lossy(&request);
        let operator_id = identities
            .operator_for(stream.conn.peer_certificates())
            .map(str::to_string);
        self.dispatch_request(&mut stream, &request, ts, operator_id.as_deref())
    }

    /// 持续接受 TLS 连接，并以客户端证书映射 Operator 身份。
    pub fn serve_tls_mtls(
        &self,
        listener: TcpListener,
        config: Arc<ServerConfig>,
        identities: &MtlsIdentityPolicy,
        ts: u64,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            let mut stream = tls_stream(stream, Arc::clone(&config))?;
            let request = read_request(&mut stream)?;
            let request = String::from_utf8_lossy(&request);
            let operator_id = identities
                .operator_for(stream.conn.peer_certificates())
                .map(str::to_string);
            self.dispatch_request(&mut stream, &request, ts, operator_id.as_deref())?;
        }
        Ok(())
    }

    /// 持续接受 mTLS 连接，并在每个新连接握手时读取当前可热替换配置。
    /// 已建立的 TLS 连接继续使用握手时的配置；配置文件解析失败由调用方的
    /// reloader 处理，当前仍可用的配置不会被替换。
    pub fn serve_tls_mtls_with_store(
        &self,
        listener: TcpListener,
        configs: &TlsConfigStore,
        identities: &MtlsIdentityPolicy,
        ts: u64,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            let mut stream = tls_stream(stream, configs.current())?;
            let request = read_request(&mut stream)?;
            let request = String::from_utf8_lossy(&request);
            let operator_id = identities
                .operator_for(stream.conn.peer_certificates())
                .map(str::to_string);
            self.dispatch_request(&mut stream, &request, ts, operator_id.as_deref())?;
        }
        Ok(())
    }

    /// 持续接受 mTLS 连接，并在每个新连接握手时读取当前 TLS 配置和 Operator
    /// 身份映射。两份配置均由调用方的轮询重载器原子替换。
    pub fn serve_tls_mtls_with_stores(
        &self,
        listener: TcpListener,
        configs: &TlsConfigStore,
        identities: &MtlsIdentityStore,
        ts: u64,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            let mut stream = tls_stream(stream, configs.current())?;
            let request = read_request(&mut stream)?;
            let request = String::from_utf8_lossy(&request);
            let policy = identities.current();
            let operator_id = policy
                .operator_for(stream.conn.peer_certificates())
                .map(str::to_string);
            self.dispatch_request(&mut stream, &request, ts, operator_id.as_deref())?;
        }
        Ok(())
    }

    pub fn serve(&self, listener: TcpListener, ts: u64) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            self.serve_stream(stream, ts)?;
        }
        Ok(())
    }

    pub fn serve_as(
        &self,
        listener: TcpListener,
        ts: u64,
        operator_id: &str,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            self.serve_stream_as(stream, ts, Some(operator_id))?;
        }
        Ok(())
    }

    /// 持续接受 TLS HTTP/WebSocket 连接；每个连接复用同一份不可变服务端配置。
    pub fn serve_tls(
        &self,
        listener: TcpListener,
        config: Arc<ServerConfig>,
        ts: u64,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            self.serve_stream(tls_stream(stream, Arc::clone(&config))?, ts)?;
        }
        Ok(())
    }

    /// 持续 TLS 服务的可信身份版本；身份仍必须由上游认证边界注入。
    pub fn serve_tls_as(
        &self,
        listener: TcpListener,
        config: Arc<ServerConfig>,
        ts: u64,
        operator_id: &str,
    ) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            configure_connection(&stream)?;
            self.serve_stream_as(
                tls_stream(stream, Arc::clone(&config))?,
                ts,
                Some(operator_id),
            )?;
        }
        Ok(())
    }

    fn serve_websocket<S: Read + Write>(
        &self,
        stream: &mut S,
        request: &str,
    ) -> std::io::Result<()> {
        let key = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("Sec-WebSocket-Key")
                    .then_some(value.trim())
            })
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing websocket key")
            })?;
        let accept = websocket_accept(key);
        let handshake = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(handshake.as_bytes())?;
        let state = self.state.lock().expect("api state mutex poisoned");
        let event_bus = state.event_bus.clone();
        let initial_cursor = state.events.events().last().map(|event| event.seq);
        write_ws_text(stream, "{\"type\":\"connected\",\"stream\":\"qianxing\"}")?;
        if let Some(snapshot) = &state.snapshot {
            write_ws_text(
                stream,
                &format!("{{\"type\":\"snapshot\",\"data\":{}}}", snapshot.to_json()),
            )?;
        }
        if !state.events.is_empty() {
            let events = serde_json::to_string(state.events.events())
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            write_ws_text(
                stream,
                &format!("{{\"type\":\"events\",\"data\":{events}}}"),
            )?;
        }
        drop(state);
        let mut cursor = initial_cursor;
        let mut client_buffer = [0_u8; 2048];
        loop {
            match event_bus.wait_after(cursor, Duration::from_millis(100)) {
                Ok(events) => {
                    for event in events {
                        write_ws_text(
                            stream,
                            &format!(
                                "{{\"type\":\"event\",\"data\":{}}}",
                                serde_json::to_string(&event).map_err(|error| {
                                    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                                })?
                            ),
                        )?;
                        cursor = Some(event.seq);
                    }
                }
                Err(EventBusError::CursorTooOld { .. } | EventBusError::CursorAhead { .. }) => {
                    write_ws_text(stream, "{\"type\":\"resync_required\"}")?;
                    return Ok(());
                }
                Err(error) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("event bus error: {error:?}"),
                    ));
                }
            }
            match stream.read(&mut client_buffer) {
                Ok(0) => return Ok(()),
                Ok(size)
                    if client_buffer[..size]
                        .iter()
                        .any(|byte| (*byte & 0x0f) == 0x8) =>
                {
                    return Ok(())
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }
}

impl QueryPort for ApiService {
    fn account_snapshot(&self) -> Option<AccountSnapshot> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .snapshot
            .clone()
    }

    fn account_orders(&self) -> Vec<qx_protocol::OrderSnapshot> {
        self.account_snapshot()
            .map(|snapshot| snapshot.orders.into_values().collect())
            .unwrap_or_default()
    }

    fn account_positions(&self) -> Vec<qx_protocol::PositionSnapshot> {
        self.account_snapshot()
            .map(|snapshot| snapshot.positions.into_values().collect())
            .unwrap_or_default()
    }

    fn account_cash(&self) -> BTreeMap<String, i128> {
        self.account_snapshot()
            .map(|snapshot| snapshot.cash_raw)
            .unwrap_or_default()
    }

    fn control_audit(&self) -> Vec<AuditRecord> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .control
            .audit()
            .to_vec()
    }

    fn events_after(&self, after: Option<u64>) -> Result<Vec<Event>, EventBusError> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .event_bus
            .read_after(after)
    }

    fn job_runs(&self) -> Vec<JobRun> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .job_runs
            .clone()
    }

    fn ledger_entries(&self) -> Vec<LedgerEntry> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .ledger_entries
            .clone()
    }

    fn reconcile_reports(&self) -> Vec<ReconcileReportSnapshot> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .reconcile_reports
            .values()
            .cloned()
            .collect()
    }
}

impl ControlPort for ApiService {
    fn submit_control(
        &self,
        command: ControlCommand,
        ts: u64,
        authenticated_operator: Option<&str>,
    ) -> ApiResponse {
        let body = match serde_json::to_string(&command) {
            Ok(body) => body,
            Err(error) => return ApiResponse::json(400, error_json(&error.to_string())),
        };
        self.submit_command(&body, ts, authenticated_operator)
    }
}

fn configure_connection(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))
}

fn tls_stream(
    stream: TcpStream,
    config: Arc<ServerConfig>,
) -> std::io::Result<StreamOwned<ServerConnection, TcpStream>> {
    let connection = ServerConnection::new(config).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("TLS server config invalid: {error}"),
        )
    })?;
    Ok(StreamOwned::new(connection, stream))
}

fn write_ws_text<S: Write>(stream: &mut S, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let mut frame = vec![0x81_u8];
    match payload.len() {
        0..=125 => frame.push(payload.len() as u8),
        126..=65_535 => {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            frame.push(127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

fn read_request<S: Read>(stream: &mut S) -> std::io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > 1_048_576 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http request too large",
            ));
        }
        if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let header = String::from_utf8_lossy(&request[..header_end]);
            let mut content_length = 0_usize;
            for line in header.lines() {
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                if name.eq_ignore_ascii_case("Content-Length") {
                    content_length = value.trim().parse::<usize>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid Content-Length",
                        )
                    })?;
                    break;
                }
            }
            let expected = header_end.checked_add(content_length).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "request length overflow")
            })?;
            if expected > 1_048_576 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "http request too large",
                ));
            }
            if request.len() >= expected {
                break;
            }
        }
    }
    Ok(request)
}

fn parse_http_request(request: &str) -> Result<(String, String, String), String> {
    let (header, body) = request
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed http request".to_string())?;
    let mut first = header.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().ok_or_else(|| "missing method".to_string())?;
    let path = first.next().ok_or_else(|| "missing path".to_string())?;
    Ok((method.into(), path.into(), body.into()))
}

fn write_http_response<S: Write>(stream: &mut S, response: &ApiResponse) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())
}

fn error_json(message: &str) -> String {
    format!("{{\"error\":{}}}", json_string(message))
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then_some(value)
    })
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

pub fn websocket_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&input))
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let mut h = [
        0x67452301_u32,
        0xEFCDAB89,
        0x98BADCFE,
        0x10325476,
        0xC3D2E1F0,
    ];
    let bit_len = (input.len() as u64) * 8;
    let mut data = input.to_vec();
    data.push(0x80);
    while !(data.len() + 8).is_multiple_of(64) {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in data.chunks(64) {
        let mut w = [0_u32; 80];
        for (i, slot) in w.iter_mut().take(16).enumerate() {
            let offset = i * 4;
            *slot = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, value) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*value);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0_u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (a << 16) | (b << 8) | c;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_control::{CommandKind, Permission};
    use qx_core::{Event, EventKind, Priority};
    use qx_protocol::AccountSnapshot;
    use rustls::server::{ClientHello, ResolvesServerCert};
    use rustls::sign::CertifiedKey;
    use std::collections::BTreeMap;
    use std::net::{Shutdown, TcpStream};

    #[derive(Debug)]
    struct NoCertificateResolver;

    impl ResolvesServerCert for NoCertificateResolver {
        fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            None
        }
    }

    #[test]
    fn websocket_accept_matches_rfc_example() {
        assert_eq!(
            websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn control_and_query_routes_are_audited() {
        let service = ApiService::new(ApiState::default());
        assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
        let metrics = service.handle("GET", "/metrics", "", 1);
        assert_eq!(metrics.status, 200);
        assert!(metrics.body.contains("qx_api_requests_total"));
        let command = ControlCommand {
            command_id: 1,
            request_id: "api-1".into(),
            operator_id: "ops".into(),
            reason: "pause after alert".into(),
            kind: CommandKind::PauseStrategy,
            target: "s1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        };
        let body = serde_json::to_string(&command).unwrap();
        assert_eq!(
            service.handle("POST", "/control/commands", &body, 2).status,
            202
        );
        assert_eq!(service.state().lock().unwrap().control.audit().len(), 1);
    }

    #[test]
    fn metrics_route_appends_supervised_worker_metrics() {
        let service = ApiService::new(ApiState::default())
            .with_worker_metrics_provider(|| "qx_worker_up{worker=\"relay\"} 1\n".into());
        let metrics = service.handle("GET", "/metrics", "", 1);
        assert_eq!(metrics.status, 200);
        assert!(metrics.body.contains("qx_worker_up{worker=\"relay\"} 1"));
    }

    #[test]
    fn readiness_separates_liveness_from_dependency_health() {
        let service =
            ApiService::new(ApiState::default()).with_readiness_provider(|| ApiReadiness {
                ready: false,
                detail: "control_store_unavailable".into(),
            });
        assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
        let ready = service.handle("GET", "/ready", "", 2);
        assert_eq!(ready.status, 503);
        assert!(ready.body.contains("control_store_unavailable"));
    }

    #[test]
    fn query_port_exposes_account_orders_positions_balances_and_audit() {
        let mut state = ApiState::default();
        let mut snapshot = AccountSnapshot::new(10, "main", "default", "paper", 100);
        snapshot.cash_raw.insert("USDT".into(), 123);
        let instrument = qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        snapshot.orders.insert(
            7,
            qx_protocol::OrderSnapshot {
                order_id: 7,
                client_order_id: 7,
                instrument: instrument.clone(),
                side: qx_core::Side::Buy,
                quantity_raw: 1,
                filled_raw: 0,
                status: qx_core::OrderStatus::Accepted,
            },
        );
        snapshot.positions.insert(
            instrument.clone(),
            qx_protocol::PositionSnapshot {
                instrument,
                quantity_raw: 1,
                ..qx_protocol::PositionSnapshot::default()
            },
        );
        state.job_runs.push(qx_scheduler::JobRun {
            run_id: 99,
            job_id: "strategy".into(),
            trading_day: "20260911".into(),
            attempt: 1,
            status: qx_scheduler::JobStatus::Running,
            manifest_digest: Some(7),
            error_code: None,
            next_retry_ts: None,
            started_ts: 1,
            deadline_ts: 2,
        });
        state.ledger_entries.push(qx_core::LedgerEntry {
            id: 1,
            account_id: "main".into(),
            currency: "USDT".into(),
            kind: qx_core::LedgerEntryKind::Adjustment,
            amount: qx_core::Money::from_raw(123),
            instrument: None,
            quantity: qx_core::Quantity::ZERO,
            price: None,
            order_id: None,
            ts: 100,
            multiplier: 1,
            position_side: None,
        });
        state.reconcile_reports.insert(
            "reconciler".into(),
            ReconcileReportSnapshot {
                schema_version: 1,
                worker_id: "reconciler".into(),
                account_id: "main".into(),
                venue_id: "paper".into(),
                observed_ts: 100,
                order_issues: Vec::new(),
                balances_count: 1,
                balance_discrepancies: Vec::new(),
                position_snapshots_count: 1,
                funding_rate_snapshots_count: 0,
                cashflow_count: 0,
            },
        );
        state.publish_snapshot(snapshot).unwrap();
        let service = ApiService::new(state);
        assert_eq!(service.handle("GET", "/account/orders", "", 1).status, 200);
        assert_eq!(
            service.handle("GET", "/account/positions", "", 2).status,
            200
        );
        assert_eq!(
            service.handle("GET", "/account/balances", "", 3).status,
            200
        );
        assert_eq!(service.handle("GET", "/control/audit", "", 4).status, 200);
        assert_eq!(service.handle("GET", "/scheduler/runs", "", 5).status, 200);
        assert_eq!(service.handle("GET", "/account/ledger", "", 6).status, 200);
        assert_eq!(
            service.handle("GET", "/reconcile/reports", "", 7).status,
            200
        );
        assert_eq!(service.query_port().account_cash()["USDT"], 123);
        assert_eq!(service.query_port().account_orders().len(), 1);
        assert_eq!(service.query_port().account_positions().len(), 1);
        assert_eq!(service.query_port().job_runs().len(), 1);
        assert_eq!(service.query_port().ledger_entries().len(), 1);
        assert_eq!(service.query_port().reconcile_reports().len(), 1);
    }

    #[test]
    fn control_submitter_persists_before_api_accepts() {
        let persisted = Arc::new(Mutex::new(ControlPlane::default()));
        let persisted_for_callback = Arc::clone(&persisted);
        let service = ApiService::new(ApiState::default()).with_control_submitter(
            move |command, granted, ts| {
                let mut plane = persisted_for_callback.lock().unwrap();
                let audit = plane
                    .submit_as(command, granted, ts)
                    .map_err(ControlSubmitError::Rejected)?;
                Ok((plane.clone(), audit))
            },
        );
        let command = ControlCommand {
            command_id: 2,
            request_id: "durable-api-2".into(),
            operator_id: "ops".into(),
            reason: "durable command".into(),
            kind: CommandKind::SubmitOrder,
            target: "2".into(),
            payload: BTreeMap::from([("order_json".into(), "{}".into())]),
            permission: Permission::Trading,
            dry_run: true,
        };
        let body = serde_json::to_string(&command).unwrap();
        let response = service.handle("POST", "/control/commands", &body, 3);
        assert_eq!(response.status, 202);
        assert_eq!(
            service.handle("POST", "/control/commands", &body, 4).status,
            409
        );
        assert_eq!(persisted.lock().unwrap().audit().len(), 1);
        assert_eq!(service.state().lock().unwrap().control.audit().len(), 1);
    }

    #[test]
    fn configured_api_policy_rejects_self_asserted_permission() {
        let service = ApiService::with_policy(
            ApiState::default(),
            ApiPolicy::new().grant("ops", Permission::ReadOnly),
        );
        assert_eq!(service.handle("GET", "/metrics", "", 1).status, 403);
        let command = ControlCommand {
            command_id: 1,
            request_id: "api-secure-1".into(),
            operator_id: "ops".into(),
            reason: "attempt".into(),
            kind: CommandKind::CancelOrder,
            target: "order-1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        };
        let body = serde_json::to_string(&command).unwrap();
        assert_eq!(
            service
                .handle_as("ops", "POST", "/control/commands", &body, 2)
                .status,
            403
        );
        assert!(service.state().lock().unwrap().control.audit().is_empty());
    }

    #[test]
    fn protected_api_does_not_accept_operator_from_command_body() {
        let service = ApiService::with_policy(
            ApiState::default(),
            ApiPolicy::new().grant("ops", Permission::Trading),
        );
        let command = ControlCommand {
            command_id: 1,
            request_id: "api-untrusted-1".into(),
            operator_id: "ops".into(),
            reason: "missing trusted identity".into(),
            kind: CommandKind::CancelOrder,
            target: "order-1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        };
        let body = serde_json::to_string(&command).unwrap();
        assert_eq!(
            service.handle("POST", "/control/commands", &body, 2).status,
            403
        );
        assert!(service.state().lock().unwrap().control.audit().is_empty());
    }

    #[test]
    fn trusted_api_identity_overrides_command_body_identity() {
        let service = ApiService::with_policy(
            ApiState::default(),
            ApiPolicy::new().grant("ops", Permission::Trading),
        );
        let command = ControlCommand {
            command_id: 1,
            request_id: "api-trusted-1".into(),
            operator_id: "forged".into(),
            reason: "trusted boundary test".into(),
            kind: CommandKind::CancelOrder,
            target: "order-1".into(),
            payload: BTreeMap::new(),
            permission: Permission::Trading,
            dry_run: true,
        };
        let body = serde_json::to_string(&command).unwrap();
        assert_eq!(
            service
                .handle_as("ops", "POST", "/control/commands", &body, 2)
                .status,
            202
        );
        assert_eq!(
            service.state().lock().unwrap().control.audit()[0].operator_id,
            "ops"
        );
    }

    #[test]
    fn api_rate_limit_is_deterministic_for_a_single_process() {
        let service = ApiService::new(ApiState::default()).with_rate_limit(1, 0);
        assert_eq!(service.handle("GET", "/health", "", 1).status, 200);
        assert_eq!(service.handle("GET", "/health", "", 1).status, 429);
        assert_eq!(service.handle("GET", "/health", "", 2).status, 429);
    }

    #[test]
    fn shared_file_rate_limit_is_visible_to_multiple_api_services() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-api-rate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = ApiService::new(ApiState::default())
            .with_shared_file_rate_limit(FileTokenBucket::new(&root, "api", 1, 0).unwrap());
        let second = ApiService::new(ApiState::default())
            .with_shared_file_rate_limit(FileTokenBucket::new(&root, "api", 1, 0).unwrap());
        assert_eq!(first.handle("GET", "/health", "", 1).status, 200);
        assert_eq!(second.handle("GET", "/health", "", 1).status, 429);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn event_bus_wakes_waiters_and_reports_retention_gaps() {
        let bus = ApiEventBus::new(1).unwrap();
        bus.publish(Event::new(0, 1, Priority::POST, EventKind::Settle))
            .unwrap();
        let waiter = bus.clone();
        let thread =
            std::thread::spawn(move || waiter.wait_after(Some(0), Duration::from_secs(1)).unwrap());
        bus.publish(Event::new(1, 2, Priority::POST, EventKind::Settle))
            .unwrap();
        assert_eq!(thread.join().unwrap().len(), 1);
        bus.publish(Event::new(2, 3, Priority::POST, EventKind::Settle))
            .unwrap();
        assert!(matches!(
            bus.read_after(Some(0)),
            Err(EventBusError::CursorTooOld { .. })
        ));
    }

    #[test]
    fn live_event_route_uses_the_realtime_cursor_contract() {
        let service = ApiService::new(ApiState::default());
        service
            .publish_event(Event::new(0, 1, Priority::POST, EventKind::Settle))
            .unwrap();
        let response = service.handle("GET", "/events/live?after=1", "", 1);
        assert_eq!(response.status, 409);
        let response = service.handle("GET", "/events/live", "", 1);
        assert_eq!(response.status, 200);
        assert!(response.body.contains("Settle"));
    }

    #[test]
    fn mtls_identity_policy_maps_exact_certificate_der() {
        let certificate = CertificateDer::from(vec![1, 2, 3, 4]);
        let policy = MtlsIdentityPolicy::new()
            .grant_certificate(certificate.clone(), "ops")
            .unwrap();
        assert_eq!(
            policy.operator_for(Some(std::slice::from_ref(&certificate))),
            Some("ops")
        );
        let other = CertificateDer::from(vec![1, 2, 3, 5]);
        assert_eq!(
            policy.operator_for(Some(std::slice::from_ref(&other))),
            None
        );
    }

    #[test]
    fn mtls_identity_store_replaces_operator_mapping_atomically() {
        let first_certificate = CertificateDer::from(vec![9, 8, 7]);
        let second_certificate = CertificateDer::from(vec![6, 5, 4]);
        let first = MtlsIdentityPolicy::new()
            .grant_certificate(first_certificate.clone(), "ops-old")
            .unwrap();
        let second = MtlsIdentityPolicy::new()
            .grant_certificate(second_certificate.clone(), "ops-new")
            .unwrap();
        let store = MtlsIdentityStore::new(first);
        assert_eq!(
            store
                .current()
                .operator_for(Some(std::slice::from_ref(&first_certificate))),
            Some("ops-old")
        );
        store.replace(second);
        let current = store.current();
        assert_eq!(
            current.operator_for(Some(std::slice::from_ref(&second_certificate))),
            Some("ops-new")
        );
        assert_eq!(
            current.operator_for(Some(std::slice::from_ref(&first_certificate))),
            None
        );
    }

    #[test]
    fn tls_config_store_exposes_rotation_boundary() {
        let first = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(NoCertificateResolver)),
        );
        let second = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(NoCertificateResolver)),
        );
        let store = TlsConfigStore::new(Arc::clone(&first));
        assert!(Arc::ptr_eq(&store.current(), &first));
        store.replace(Arc::clone(&second));
        assert!(Arc::ptr_eq(&store.current(), &second));
    }

    #[test]
    fn snapshot_diff_and_event_cursor_require_a_valid_base() {
        let mut state = ApiState::default();
        let mut base = AccountSnapshot::new(1, "a", "p", "paper", 1);
        base.cash_raw.insert("USD".into(), 100);
        let base_hash = state.publish_snapshot(base).unwrap();
        let mut target = AccountSnapshot::new(2, "a", "p", "paper", 2);
        target.cash_raw.insert("USD".into(), 120);
        state.publish_snapshot(target).unwrap();
        let seq = state.events.alloc_seq();
        state
            .events
            .append(Event::new(seq, 2, Priority::POST, EventKind::Settle));
        let seq = state.events.alloc_seq();
        state
            .events
            .append(Event::new(seq, 3, Priority::POST, EventKind::Settle));
        let service = ApiService::new(state);
        let diff = service.handle(
            "GET",
            &format!("/account/snapshot/diff?base_hash={base_hash}"),
            "",
            3,
        );
        assert_eq!(diff.status, 200);
        assert!(diff.body.contains("base_state_hash"));
        let events = service.handle("GET", "/events?after=0", "", 3);
        assert_eq!(events.status, 200);
        assert!(events.body.contains("Settle"));
        assert_eq!(
            service
                .handle("GET", "/account/snapshot/diff?base_hash=999", "", 3)
                .status,
            409
        );
    }

    #[test]
    fn http_server_serves_health_route() {
        let service = ApiService::new(ApiState::default());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || service.serve_once(&listener, 1));
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("{\"status\":\"ok\"}"));
    }

    #[test]
    fn tls_server_rejects_plaintext_before_http_dispatch() {
        let service = ApiService::new(ApiState::default());
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(NoCertificateResolver));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker =
            std::thread::spawn(move || service.serve_once_tls(&listener, Arc::new(config), 1));
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let result = worker.join().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn websocket_server_sends_connection_and_event_batches() {
        let mut state = ApiState::default();
        let seq = state.events.alloc_seq();
        state
            .publish_event(Event::new(seq, 1, Priority::POST, EventKind::Settle))
            .unwrap();
        let service = ApiService::new(state);
        let publisher = service.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || service.serve_once(&listener, 1));
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(
                b"GET /stream HTTP/1.1\r\nHost: localhost\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut response = Vec::new();
        loop {
            let mut chunk = [0_u8; 8192];
            let count = client.read(&mut chunk).unwrap();
            assert!(count > 0);
            response.extend_from_slice(&chunk[..count]);
            if String::from_utf8_lossy(&response).contains("events") {
                break;
            }
        }
        let next_seq = publisher.state().lock().unwrap().events.next_seq();
        publisher
            .publish_event(Event::new(next_seq, 2, Priority::POST, EventKind::Settle))
            .unwrap();
        let mut pushed = Vec::new();
        loop {
            let mut chunk = [0_u8; 8192];
            let count = client.read(&mut chunk).unwrap();
            assert!(count > 0);
            pushed.extend_from_slice(&chunk[..count]);
            if String::from_utf8_lossy(&pushed).contains("\"type\":\"event\"") {
                break;
            }
        }
        client.write_all(&[0x88, 0x80, 0, 0, 0, 0]).unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        worker.join().unwrap().unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 101 Switching Protocols"));
        assert!(response.contains("qianxing"));
        assert!(response.contains("events"));
    }
}
