//! 不依赖特定 Web 框架的本地 API 边界。
//!
//! 该层只做协议解析、权限入口和事件/快照查询，不直接修改 Ledger；写操作必须
//! 进入 `ControlPlane`，由上层执行器完成实际动作并回写审计。

mod event_cursor;

use event_cursor::{events_after_cursor, parse_after_cursor};
use qx_control::{
    AuditRecord, ControlCommand, ControlError, ControlPlane, Permission, RetirementSummary,
};
use qx_core::{Event, EventKind, EventLog, Fnv1a, LedgerEntry};
use qx_protocol::{
    AccountSnapshot, ProjectionEnvelope, ProjectionLineage, ACCOUNT_SNAPSHOT_JSON_SCHEMA,
    PROJECTION_ENVELOPE_SCHEMA_VERSION,
};
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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

mod snapshot_history;
use snapshot_history::SnapshotHistory;

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

/// 游标 → 事件批次的唯一实现在 `event_cursor`（V12 R4-g）。
fn read_bus_events(state: &EventBusState, after: Option<u64>) -> Result<Vec<Event>, EventBusError> {
    event_cursor::events_after_cursor(state.events.iter(), state.next_seq, after)
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

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ApiProjectionKey {
    pub account_id: String,
    pub venue_id: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectionHealth {
    pub healthy: bool,
    pub last_projected_seq: Option<u64>,
    pub source_digest: Option<u64>,
    pub error: Option<String>,
}

impl Default for ProjectionHealth {
    fn default() -> Self {
        Self {
            healthy: true,
            last_projected_seq: None,
            source_digest: None,
            error: None,
        }
    }
}

impl ApiProjectionKey {
    pub fn new(account_id: impl Into<String>, venue_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            venue_id: venue_id.into(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.account_id.trim().is_empty() || self.venue_id.trim().is_empty() {
            return Err("API 投影必须同时指定非空 account_id 和 venue_id".into());
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ApiAccountProjection {
    pub snapshot: Option<AccountSnapshot>,
    snapshot_history: SnapshotHistory,
    pub events: EventLog,
    pub event_bus: ApiEventBus,
    pub health: ProjectionHealth,
}

impl ApiAccountProjection {
    fn append_projected_event(&mut self, event: Event) -> Result<(), String> {
        let expected = self.events.next_seq();
        let bus_expected = self.event_bus.next_seq();
        if event.seq != expected || event.seq != bus_expected {
            return Err(format!(
                "账户投影事件游标不连续: event_seq={} event_log_next={} event_bus_next={}",
                event.seq, expected, bus_expected
            ));
        }
        self.events
            .append_checked(event.clone())
            .map_err(|error| format!("account projected event rejected: {error:?}"))?;
        self.event_bus
            .publish(event)
            .map_err(|error| format!("account projected event bus rejected: {error:?}"))
    }

    fn project_event_log(
        &mut self,
        key: &ApiProjectionKey,
        source: &EventLog,
    ) -> Result<usize, String> {
        let result = self.project_event_log_inner(key, source);
        match &result {
            Ok(_) => {
                self.health.healthy = true;
                self.health.last_projected_seq = self.events.events().last().map(|event| event.seq);
                self.health.source_digest = Some(source.digest());
                self.health.error = None;
            }
            Err(error) => {
                self.health.healthy = false;
                self.health.error = Some(error.clone());
            }
        }
        result
    }

    fn project_event_log_inner(
        &mut self,
        key: &ApiProjectionKey,
        source: &EventLog,
    ) -> Result<usize, String> {
        let mut projected = 0;
        for event in source.events() {
            validate_projection_event(key, event)?;
            if event.seq < self.events.next_seq() {
                let existing = self
                    .events
                    .events()
                    .iter()
                    .find(|current| current.seq == event.seq);
                if existing == Some(event) {
                    continue;
                }
                return Err(format!(
                    "账户投影检测到事件内容漂移: event_seq={}",
                    event.seq
                ));
            }
            self.append_projected_event(event.clone())?;
            projected += 1;
        }
        Ok(projected)
    }

    fn publish_snapshot(&mut self, snapshot: AccountSnapshot) -> u64 {
        let hash = snapshot.state_hash();
        self.snapshot_history.insert(hash, snapshot.clone());
        self.snapshot = Some(snapshot);
        hash
    }
}

/// 账户读模型的刷新者（API 投影桥）此刻在不在跑。
///
/// `serve` 之外的装配（进程内测试、只读工具）根本没有桥可谈，所以默认是 `Unreported`
/// 而不是"未运行"——`/ready` 对前者不加判定，只对 `Stopped` 判定（V11 I1）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProjectionRefresher {
    #[default]
    Unreported,
    Running,
    Stopped(String),
}

#[derive(Default)]
pub struct ApiState {
    pub snapshot: Option<AccountSnapshot>,
    snapshot_history: SnapshotHistory,
    /// 按 account_id + venue_id 隔离的查询/订阅读模型。事件与实时游标只住在这里：
    /// 旧的全局 `events`/`event_bus` 已在 V11 F1 删除，它们唯一的写入者是两个零生产调用的兼容入口。
    pub projections: BTreeMap<ApiProjectionKey, ApiAccountProjection>,
    pub control: ControlPlane,
    /// 只读运维读模型；调度、账簿和对账事实仍由各自 owner 写入。
    pub job_runs: Vec<JobRun>,
    pub ledger_entries: Vec<LedgerEntry>,
    pub reconcile_reports: BTreeMap<String, ReconcileReportSnapshot>,
    /// 谁在推进 `projections`/`snapshot`：桥没跑，这两半就停在最后一轮（V11 I1）。
    pub projection_refresher: ProjectionRefresher,
}

/// API 限流额度只有一处定义：进程内令牌桶、`with_rate_limit`、以及 `qx-cli`
/// 装配的文件/SQLite 共享桶都必须引用这两个常量，否则"换一个存储后端"会
/// 顺带改掉限流策略（V12 §16）。
pub const DEFAULT_RATE_LIMIT_CAPACITY: u64 = 100;
pub const DEFAULT_RATE_LIMIT_REFILL_PER_SECOND: u64 = 100;

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
        if !self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.header.account_id.trim().is_empty()
                || snapshot.header.venue_id.trim().is_empty()
        }) {
            let snapshot = self.snapshot.clone().expect("snapshot was just stored");
            let key = ApiProjectionKey::new(
                snapshot.header.account_id.clone(),
                snapshot.header.venue_id.clone(),
            );
            key.validate()?;
            self.projections
                .entry(key)
                .or_default()
                .publish_snapshot(snapshot);
        }
        Ok(hash)
    }

    /// 写入指定账户/交易所的快照投影，不会覆盖兼容的全局主账户快照。
    pub fn publish_snapshot_for(
        &mut self,
        account_id: impl Into<String>,
        venue_id: impl Into<String>,
        mut snapshot: AccountSnapshot,
    ) -> Result<u64, String> {
        let key = ApiProjectionKey::new(account_id, venue_id);
        key.validate()?;
        if snapshot.header.account_id != key.account_id || snapshot.header.venue_id != key.venue_id
        {
            return Err("账户投影快照的 account_id/venue_id 与目标不一致".into());
        }
        snapshot.header.state_hash = 0;
        snapshot
            .validate()
            .map_err(|error| format!("snapshot invalid: {error:?}"))?;
        snapshot.seal();
        let hash = snapshot.state_hash();
        self.projections
            .entry(key)
            .or_default()
            .publish_snapshot(snapshot);
        Ok(hash)
    }

    pub fn account_snapshot_for(
        &self,
        account_id: &str,
        venue_id: &str,
    ) -> Option<AccountSnapshot> {
        self.projections
            .get(&ApiProjectionKey::new(account_id, venue_id))
            .and_then(|projection| projection.snapshot.clone())
    }

    pub fn projection_health(&self, account_id: &str, venue_id: &str) -> Option<ProjectionHealth> {
        self.projections
            .get(&ApiProjectionKey::new(account_id, venue_id))
            .map(|projection| projection.health.clone())
    }

    /// 将某一账户/交易所的事实日志投影到隔离的 API 读模型。每个投影拥有
    /// 独立的 EventLog 和实时游标，因此一个账户的 retention gap 不会污染另一个账户。
    /// 这是 API 读模型唯一的事件写入者（V11 F1）：被删掉的两个兼容入口各自复制过
    /// 一遍"前缀幂等 + 漂移失败"，同一规则两处表达就是下一次漂移的入口。
    pub fn project_account_event_log(
        &mut self,
        account_id: impl Into<String>,
        venue_id: impl Into<String>,
        source: &EventLog,
    ) -> Result<usize, String> {
        let key = ApiProjectionKey::new(account_id, venue_id);
        key.validate()?;
        self.projections
            .entry(key.clone())
            .or_default()
            .project_event_log(&key, source)
    }
}

fn validate_projection_event(key: &ApiProjectionKey, event: &Event) -> Result<(), String> {
    let identity = match &event.kind {
        EventKind::AccountBalanceSnapshot {
            account_id,
            venue_id,
            ..
        }
        | EventKind::AccountPositionSnapshot {
            account_id,
            venue_id,
            ..
        } => Some((account_id.as_str(), Some(venue_id.as_str()))),
        EventKind::AccountCashflow { cashflow } => Some((
            cashflow.account_id.as_str(),
            Some(cashflow.venue_id.as_str()),
        )),
        // 订单与账簿条目没有"账户域"这一栏，只有标的后缀：paper 账户交易的符号就叫
        // BTCUSDT.BINANCE，拿它当账户 venue 会把本账户自己的事实判成外来事实（V11 R13）。
        // 这两条只核对账户身份；交易所侧的串读由带显式 venue 的余额/持仓/资金/成交事实守住。
        EventKind::OrderSubmitted { order } => Some((order.account_id.as_str(), None)),
        EventKind::Filled { fill } => Some((fill.account_id.as_str(), fill.venue_id.as_deref())),
        EventKind::LedgerApplied { entry } => Some((entry.account_id.as_str(), None)),
        EventKind::Timer { .. }
        | EventKind::MarketBar { .. }
        | EventKind::MarketQuote { .. }
        | EventKind::FundingRateSnapshot { .. }
        | EventKind::Submit { .. }
        | EventKind::Accepted { .. }
        | EventKind::Rejected { .. }
        | EventKind::Cancelled { .. }
        | EventKind::ReconcileRequired { .. }
        | EventKind::Settle => None,
    };
    if let Some((account_id, venue_id)) = identity {
        if (!account_id.trim().is_empty() && account_id != key.account_id)
            || venue_id.is_some_and(|venue_id| !venue_id.eq_ignore_ascii_case(&key.venue_id))
        {
            return Err(format!(
                "事件身份与 API 投影不匹配: expected={}/{} actual={}/{} seq={}",
                key.account_id,
                key.venue_id,
                account_id,
                venue_id.unwrap_or("unknown"),
                event.seq
            ));
        }
    }
    Ok(())
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
    query_models_provider: Option<QueryModelsProvider>,
    control_provider: Option<ControlProvider>,
    /// 同时在场的长连接名额（V11 R7-e）：接一条占一格，连接线程结束（含 panic）时归还。
    live: Arc<LiveConnectionBudget>,
}

#[derive(Default)]
struct ApiMetrics {
    requests_total: AtomicU64,
    rate_limit_rejected_total: AtomicU64,
    authentication_rejected_total: AtomicU64,
    /// 因撞上 [`MAX_LIVE_CONNECTIONS`] 而被拒的长连接条数（V11 R7-e）。写侧不留在原地：
    /// 它由下面的摘要念进 `/metrics`，运维数得到“今天拒了几条”。
    connections_rejected_total: AtomicU64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ApiMetricsSnapshot {
    pub requests_total: u64,
    pub rate_limit_rejected_total: u64,
    pub authentication_rejected_total: u64,
    pub connections_rejected_total: u64,
}

impl ApiMetricsSnapshot {
    pub fn to_prometheus(self) -> String {
        // 每格指标各占一行，分隔符必须是真换行：上一版这里写的是两字符的字面反斜杠加 n，于是
        // `/metrics` 整份文本挤成一行，`deploy/prometheus/qianxing-alerts.yml` 那批告警一条都
        // 取不到样本——端点仍回 200，链路却是断的（V11 R7-i）。
        format!(
            "# HELP qx_api_requests_total Total API requests received.\n\
# TYPE qx_api_requests_total counter\n\
qx_api_requests_total {}\n\
# HELP qx_api_rate_limit_rejected_total Requests rejected by the rate limiter.\n\
# TYPE qx_api_rate_limit_rejected_total counter\n\
qx_api_rate_limit_rejected_total {}\n\
# HELP qx_api_authentication_rejected_total Requests rejected by the API policy.\n\
# TYPE qx_api_authentication_rejected_total counter\n\
qx_api_authentication_rejected_total {}\n\
# HELP qx_api_connections_rejected_total Long connections refused by the live-connection ceiling.\n\
# TYPE qx_api_connections_rejected_total counter\n\
qx_api_connections_rejected_total {}\n",
            self.requests_total,
            self.rate_limit_rejected_total,
            self.authentication_rejected_total,
            self.connections_rejected_total
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
/// 控制面的现读出口：读端点经它取 store 那一份，而不是念进程内 boot 副本（V11 H1）。
type ControlProvider = Arc<dyn Fn() -> Result<ControlPlane, String> + Send + Sync>;

/// `/scheduler/runs`、`/account/ledger`、`/reconcile/reports` 三个只读端点共用的一份现读结果。
/// 对账报告在这里是**列表**：`ApiState` 里那张按 worker_id 键控的表只是它的查询副本。
#[derive(Default, Clone)]
pub struct ApiQueryModels {
    pub job_runs: Vec<JobRun>,
    pub ledger_entries: Vec<LedgerEntry>,
    pub reconcile_reports: Vec<ReconcileReportSnapshot>,
}

type QueryModelsProvider = Arc<dyn Fn() -> Result<ApiQueryModels, String> + Send + Sync>;

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
    /// 下面三个是"这一轮有没有去取"的覆盖度，不是账户事实本身：`None` 表示这条对账链
    /// 没有取该项（Binance 现货 worker 不查持仓/资金费/账单，CCXT 在该交易所报
    /// "能力不支持"时跳过），`Some(0)` 才是取了且为空。此前三者是不可区分的 `usize`
    /// 且 Binance 侧硬写 0，读侧把"没取"当成"账户没有"（V11 Q69）。
    #[serde(default)]
    pub position_snapshots_count: Option<usize>,
    #[serde(default)]
    pub funding_rate_snapshots_count: Option<usize>,
    #[serde(default)]
    pub cashflow_count: Option<usize>,
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
    /// 窗口之外那半本账：终态退场命令的累计摘要，与 `/control/audit` 的 `retirement` 同一格。
    fn control_retirement(&self) -> RetirementSummary;
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

/// 长驻循环与连接线程共用的停机回调。V11 J2 把它留在 accept 一侧，R7-e 把它递进连接线程，
/// 于是“停机”对正在场的长连接同样是可执行的请求，而不是只对没人接听的套接字生效。
type StopToken = Arc<dyn Fn() -> bool + Send + Sync>;

/// 长连接名额的计数本体：上界只有一个来源（[`MAX_LIVE_CONNECTIONS`]），计数随 `ApiService`
/// 的每份克隆共享——每台进程一份，而不是每个 handler 一份。
struct LiveConnectionBudget {
    live: AtomicUsize,
    max: usize,
}

impl LiveConnectionBudget {
    fn bounded() -> Self {
        Self {
            live: AtomicUsize::new(0),
            max: MAX_LIVE_CONNECTIONS,
        }
    }

    /// 领一格：满了当场返回 `None`，不自旋也不排队——让 accept 循环等一个空位会把“过载”
    /// 变成“没反应”，而运维要的是看得见的一句拒绝加一个数得出来的计数。
    fn reserve(self: &Arc<Self>) -> Option<LiveConnectionSlot> {
        let taken = self
            .live
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                if live < self.max {
                    Some(live + 1)
                } else {
                    None
                }
            })
            .is_ok();
        taken.then(|| LiveConnectionSlot {
            budget: Arc::clone(self),
        })
    }
}

/// 名额的归还凭证：连接线程返回或 panic 展开时 `Drop` 减一。少了这颗，一次 panic 就永久
/// 占住一格，几次之后整台 API 只会回 503——那正是本轮要挡住的形状反过来咬人。
struct LiveConnectionSlot {
    budget: Arc<LiveConnectionBudget>,
}

impl Drop for LiveConnectionSlot {
    fn drop(&mut self) {
        self.budget.live.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ApiService {
    pub fn new(state: ApiState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            policy: None,
            rate_limiter: Arc::new(LocalRateLimitBackend {
                limiter: Mutex::new(ApiRateLimiter::new(
                    DEFAULT_RATE_LIMIT_CAPACITY,
                    DEFAULT_RATE_LIMIT_REFILL_PER_SECOND,
                )),
            }),
            control_submitter: None,
            command_enqueuer: None,
            metrics: Arc::new(ApiMetrics::default()),
            worker_metrics_provider: None,
            readiness_provider: None,
            query_models_provider: None,
            control_provider: None,
            live: Arc::new(LiveConnectionBudget::bounded()),
        }
    }

    pub fn with_policy(state: ApiState, policy: ApiPolicy) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            policy: Some(policy),
            rate_limiter: Arc::new(LocalRateLimitBackend {
                limiter: Mutex::new(ApiRateLimiter::new(
                    DEFAULT_RATE_LIMIT_CAPACITY,
                    DEFAULT_RATE_LIMIT_REFILL_PER_SECOND,
                )),
            }),
            control_submitter: None,
            command_enqueuer: None,
            metrics: Arc::new(ApiMetrics::default()),
            worker_metrics_provider: None,
            readiness_provider: None,
            query_models_provider: None,
            control_provider: None,
            live: Arc::new(LiveConnectionBudget::bounded()),
        }
    }

    pub fn state(&self) -> Arc<Mutex<ApiState>> {
        Arc::clone(&self.state)
    }

    /// 报一次"谁在推进账户读模型"：投影桥启动前报 `Running`，每条启动失败的路径报
    /// `Stopped(原因)`，线程退出（含 panic）也要报。没有这格状态时 `/ready` 只看投影本身
    /// 的健康位，而 boot 那份永远是健康的——桥没起来或已经死了，读模型停在最后一轮却
    /// 照样报 Ready（V11 I1，与 R5 把"没有指标"读成健康同一形状）。
    pub fn report_projection_refresher(&self, refresher: ProjectionRefresher) {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .projection_refresher = refresher;
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
            connections_rejected_total: self
                .metrics
                .connections_rejected_total
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

    /// 三份只读运维读模型的现读出口：调度状态、账户账簿、对账报告。
    ///
    /// 装 provider 之前，这三份只在 `build_configured_api_service` 启动时读一次，之后
    /// `serve` 进程把它们当事实念到进程结束——对账 worker 每轮覆写
    /// `reconcile/<worker-id>.json`，API 却永远看不见（V11 S3）。`Err` 表示"这一次没读到"，
    /// 端点必须据此报错，不得退回上一份或空数组。
    pub fn with_query_models_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Result<ApiQueryModels, String> + Send + Sync + 'static,
    {
        self.query_models_provider = Some(Arc::new(provider));
        self
    }

    pub fn with_control_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Result<ControlPlane, String> + Send + Sync + 'static,
    {
        self.control_provider = Some(Arc::new(provider));
        self
    }

    /// 取控制面：装了 provider 就现读 store 并回填进程内那份，没装则读后者。
    ///
    /// 执行 worker 在**另一个进程**用 `ControlPlane::execute` 把终态追加进同一本 store，而
    /// boot 装进 `state.control` 的那一份此后只会被本进程的 POST 换掉：不现读的 `/control/audit`
    /// 就把命令永远念成 `Accepted`，而同一本 store 在 `/ready` 那里已经是每请求 `load()`（V11 H1）。
    fn control_plane(&self) -> Result<ControlPlane, String> {
        let Some(provider) = &self.control_provider else {
            let state = self.state.lock().expect("api state mutex poisoned");
            return Ok(state.control.clone());
        };
        let plane = provider()?;
        self.state.lock().expect("api state mutex poisoned").control = plane.clone();
        Ok(plane)
    }

    /// 取三份只读运维读模型：装了 provider 就现读并把结果回填到查询副本，没装则读
    /// 启动时装进 `state` 的那份。回填的是"最后已知副本"，供现读失败时的 trait 读点退回，
    /// 不是拿它当 `QueryPort` 的读点（V11 S3 / I2）。
    fn query_models(&self) -> Result<ApiQueryModels, String> {
        let Some(provider) = &self.query_models_provider else {
            let state = self.state.lock().expect("api state mutex poisoned");
            return Ok(ApiQueryModels {
                job_runs: state.job_runs.clone(),
                ledger_entries: state.ledger_entries.clone(),
                reconcile_reports: state.reconcile_reports.values().cloned().collect(),
            });
        };
        let models = provider()?;
        {
            let mut state = self.state.lock().expect("api state mutex poisoned");
            state.job_runs = models.job_runs.clone();
            state.ledger_entries = models.ledger_entries.clone();
            state.reconcile_reports = models
                .reconcile_reports
                .iter()
                .map(|report| (report.worker_id.clone(), report.clone()))
                .collect();
        }
        Ok(models)
    }

    /// `QueryPort` 侧的现读：与 HTTP 端点问同一个出口，读不到才退回进程内的最后已知副本。
    ///
    /// trait 签名没有 `Result`，"这一次没读到"只能由端点那侧表达，所以这里的退回不是把
    /// 失败念成空表。反过来（trait 直接读副本）在 V11 I2 之前是实际形状：副本只被
    /// `query_models()` 回填，装配完到第一个 HTTP 请求之间，trait 说的仍是 boot 那一份。
    fn query_models_last_known(&self) -> ApiQueryModels {
        if let Ok(models) = self.query_models() {
            return models;
        }
        let state = self.state.lock().expect("api state mutex poisoned");
        ApiQueryModels {
            job_runs: state.job_runs.clone(),
            ledger_entries: state.ledger_entries.clone(),
            reconcile_reports: state.reconcile_reports.values().cloned().collect(),
        }
    }

    /// `QueryPort` 侧的控制面现读：与 `/control/audit` 问同一个出口，读不到才退回副本。
    ///
    /// 退回的理由与 [`Self::query_models_last_known`] 同一颗：trait 签名没有 `Result`，
    /// "这一次没读到"由端点那侧如实报 503，这里只负责别让流水停在 boot 那一份。
    fn control_plane_last_known(&self) -> ControlPlane {
        if let Ok(plane) = self.control_plane() {
            return plane;
        }
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .control
            .clone()
    }

    pub fn query_port(&self) -> &dyn QueryPort {
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

    pub fn project_account_event_log(
        &self,
        account_id: impl Into<String>,
        venue_id: impl Into<String>,
        source: &EventLog,
    ) -> Result<usize, String> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .project_account_event_log(account_id, venue_id, source)
    }

    pub fn account_snapshot_for(
        &self,
        account_id: &str,
        venue_id: &str,
    ) -> Option<AccountSnapshot> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .account_snapshot_for(account_id, venue_id)
    }

    pub fn projection_health(&self, account_id: &str, venue_id: &str) -> Option<ProjectionHealth> {
        self.state
            .lock()
            .expect("api state mutex poisoned")
            .projection_health(account_id, venue_id)
    }

    fn projection_readiness(&self) -> Option<String> {
        let state = self.state.lock().expect("api state mutex poisoned");
        // 刷新者已经不在跑、而读模型里确实有账户投影：这份数据从这一秒起就是旧的（V11 I1）。
        // 没有投影可刷新时不作判定——那是真的没有账户域的拓扑，不是断链。
        if let ProjectionRefresher::Stopped(reason) = &state.projection_refresher {
            if !state.projections.is_empty() {
                return Some(format!("projection_refresher_stopped reason={reason}"));
            }
        }
        state
            .projections
            .iter()
            .find(|(_, projection)| !projection.health.healthy)
            .map(|(key, projection)| {
                format!(
                    "projection_stale account_id={} venue_id={} error={}",
                    key.account_id,
                    key.venue_id,
                    projection.health.error.as_deref().unwrap_or("unknown")
                )
            })
    }

    fn snapshot_for_query(&self, query: &str) -> Result<Option<AccountSnapshot>, String> {
        // 带 account_id/venue_id 的查询走与公共键读法同一处查找，避免路由与嵌入方
        // 各自实现一遍"键怎么映射到投影"。
        if let Some(key) = projection_key_from_query(query)? {
            return Ok(self.account_snapshot_for(&key.account_id, &key.venue_id));
        }
        let state = self.state.lock().expect("api state mutex poisoned");
        Ok(state.snapshot.clone())
    }

    fn projection_events_for_query(&self, query: &str) -> Result<Vec<Event>, String> {
        let key = projection_key_from_query(query)?;
        let state = self.state.lock().expect("api state mutex poisoned");
        // 事件只按账户投影存着；缺键不再"挑一个默认账户"，也不再看那份空的全局面。
        let key = key.ok_or_else(|| "事件读模型需要同时提供 account_id 与 venue_id".to_string())?;
        Ok(state
            .projections
            .get(&key)
            .map_or_else(Vec::new, |projection| projection.events.events().to_vec()))
    }

    fn snapshot_envelope_for_query(
        &self,
        query: &str,
    ) -> Result<Option<ProjectionEnvelope<serde_json::Value>>, String> {
        let key = projection_key_from_query(query)?;
        let state = self.state.lock().expect("api state mutex poisoned");
        let (snapshot, source_digest) = match key {
            Some(key) => {
                let Some(projection) = state.projections.get(&key) else {
                    return Ok(None);
                };
                (projection.snapshot.clone(), projection.events.digest())
            }
            // 全局快照由 `publish_snapshot` 按身份镜像进账户投影，摘要就取那份；
            // 没有那份就没有可核对的事件源，宁可不给信封也不编一个 0。
            None => {
                let Some(snapshot) = state.snapshot.clone() else {
                    return Ok(None);
                };
                let key = ApiProjectionKey::new(
                    snapshot.header.account_id.clone(),
                    snapshot.header.venue_id.clone(),
                );
                match state.projections.get(&key) {
                    Some(projection) => (Some(snapshot), projection.events.digest()),
                    None => return Ok(None),
                }
            }
        };
        Ok(snapshot.map(|snapshot| {
            let event_seq = snapshot.header.event_seq;
            let state_hash = snapshot.state_hash();
            ProjectionEnvelope {
                schema_version: PROJECTION_ENVELOPE_SCHEMA_VERSION,
                kind: "account_snapshot".into(),
                tenant_id: snapshot.header.account_id.clone(),
                run_id: format!(
                    "account:{}:{}",
                    snapshot.header.account_id, snapshot.header.venue_id
                ),
                account_id: snapshot.header.account_id.clone(),
                portfolio_id: snapshot.header.portfolio_id.clone(),
                venue_id: snapshot.header.venue_id.clone(),
                as_of: snapshot.header.as_of,
                event_seq,
                cursor: format!("{event_seq}:{state_hash:016x}"),
                state_hash,
                source: "eventlog".into(),
                lineage: ProjectionLineage {
                    source_digest: Some(format!("{source_digest:016x}")),
                    ..ProjectionLineage::default()
                },
                // `data` 只认 `to_json` 这一份线格式（V12 R4-h）：`/schema/account-snapshot-v1`
                // 公布的就是它。`AccountSnapshot` 的 serde 派生形状没有 `protocol`/顶层
                // `schema_version`，照 schema 校验必失败——同一端点家族不能发两种契约。
                data: serde_json::from_str(&snapshot.to_json()).expect("账户快照线格式必须可解析"),
            }
        }))
    }

    #[cfg(test)]
    pub(crate) fn with_rate_limit(mut self, capacity: u64, refill_per_second: u64) -> Self {
        self.rate_limiter = Arc::new(LocalRateLimitBackend {
            limiter: Mutex::new(ApiRateLimiter::new(capacity, refill_per_second)),
        });
        self
    }

    /// 用例把上界调小才真撞得到门：生产的 64 格要占住 64 颗线程才算数，那是夹具的代价不是判据。
    #[cfg(test)]
    pub(crate) fn with_max_live_connections(mut self, max: usize) -> Self {
        self.live = Arc::new(LiveConnectionBudget {
            live: AtomicUsize::new(0),
            max,
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

    /// 带可信身份的入口（仅 crate 内可见）。`operator_id` 必须由认证网关/进程边界
    /// 注入，不能来自命令体；产品装配只经 `serve_*` 的 mTLS 身份映射走到这里，
    /// 因此不对外公开，避免宿主把认证入口当成可伪造的公共 API（V12 §16）。
    #[cfg(test)]
    pub(crate) fn handle_as(
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
                let mut readiness = self
                    .readiness_provider
                    .as_ref()
                    .map(|provider| provider())
                    .unwrap_or(ApiReadiness {
                        ready: true,
                        detail: "api_ready".into(),
                    });
                if let Some(detail) = self.projection_readiness() {
                    readiness.ready = false;
                    readiness.detail = detail;
                }
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
            ("GET", "/account/snapshot/envelope") => {
                match self.snapshot_envelope_for_query(query) {
                    Err(error) => ApiResponse::json(400, error_json(&error)),
                    Ok(Some(envelope)) => ApiResponse::json(
                        200,
                        serde_json::to_string(&envelope)
                            .expect("projection envelope is serializable"),
                    ),
                    Ok(None) => ApiResponse::json(404, "{\"error\":\"snapshot_not_found\"}"),
                }
            }
            ("GET", "/account/snapshot") => match self.snapshot_for_query(query) {
                Err(error) => ApiResponse::json(400, error_json(&error)),
                Ok(Some(snapshot)) => ApiResponse::json(200, snapshot.to_json()),
                Ok(None) => ApiResponse::json(404, "{\"error\":\"snapshot_not_found\"}"),
            },
            ("GET", "/account/orders") => {
                let snapshot = match self.snapshot_for_query(query) {
                    Ok(snapshot) => snapshot,
                    Err(error) => return ApiResponse::json(400, error_json(&error)),
                };
                let orders = snapshot
                    .map(|snapshot| snapshot.orders.into_values().collect::<Vec<_>>())
                    .unwrap_or_default();
                ApiResponse::json(
                    200,
                    serde_json::to_string(&orders).expect("order snapshots are serializable"),
                )
            }
            ("GET", "/account/positions") => {
                let snapshot = match self.snapshot_for_query(query) {
                    Ok(snapshot) => snapshot,
                    Err(error) => return ApiResponse::json(400, error_json(&error)),
                };
                let positions = snapshot
                    .map(|snapshot| snapshot.positions.into_values().collect::<Vec<_>>())
                    .unwrap_or_default();
                ApiResponse::json(
                    200,
                    serde_json::to_string(&positions).expect("position snapshots are serializable"),
                )
            }
            ("GET", "/account/balances") => {
                let snapshot = match self.snapshot_for_query(query) {
                    Ok(snapshot) => snapshot,
                    Err(error) => return ApiResponse::json(400, error_json(&error)),
                };
                let body = serde_json::json!({
                    "cash_raw": snapshot.as_ref().map(|snapshot| &snapshot.cash_raw).cloned().unwrap_or_default(),
                    "equity_raw": snapshot.as_ref().map(|snapshot| snapshot.equity_raw),
                    "available_raw": snapshot.as_ref().and_then(|snapshot| snapshot.available_raw),
                    "margin_raw": snapshot.as_ref().and_then(|snapshot| snapshot.margin_raw),
                });
                ApiResponse::json(200, body.to_string())
            }
            ("GET", "/control/audit") => match self.control_plane() {
                Err(error) => ApiResponse::json(
                    503,
                    error_json(&format!("control_state_unavailable: {error}")),
                ),
                // 流水是有界窗口（V11 R5-1）：同一份响应里交出窗口外的累计摘要，
                // 否则"只剩最近 1000 条"会被读成"总共只有 1000 条"。
                Ok(plane) => ApiResponse::json(
                    200,
                    serde_json::json!({
                        "records": plane.audit(),
                        "retirement": plane.retirement(),
                    })
                    .to_string(),
                ),
            },
            ("GET", "/scheduler/runs") => match self.query_models() {
                Ok(models) => ApiResponse::json(
                    200,
                    serde_json::to_string(&models.job_runs).expect("job runs are serializable"),
                ),
                Err(error) => ApiResponse::json(503, error_json(&error)),
            },
            ("GET", "/account/ledger") => match self.query_models() {
                Ok(models) => ApiResponse::json(
                    200,
                    serde_json::to_string(&models.ledger_entries)
                        .expect("ledger entries are serializable"),
                ),
                Err(error) => ApiResponse::json(503, error_json(&error)),
            },
            ("GET", "/reconcile/reports") => match self.query_models() {
                Ok(models) => ApiResponse::json(
                    200,
                    serde_json::to_string(&models.reconcile_reports)
                        .expect("reconcile reports are serializable"),
                ),
                Err(error) => ApiResponse::json(503, error_json(&error)),
            },
            ("GET", "/account/snapshot/diff") => self.snapshot_diff(query),
            ("GET", "/events") => {
                let all_events = match self.projection_events_for_query(query) {
                    Ok(events) => events,
                    Err(error) => return ApiResponse::json(400, error_json(&error)),
                };
                let after = match parse_after_cursor(query) {
                    Ok(after) => after,
                    Err(error) => return ApiResponse::json(400, error_json(error)),
                };
                // V12 R4-g：`after` 与 `/events/live` 同一个口径——事件序号，不是这条投影
                // 日志的下标。投影日志的 `next_seq` 恒等于末条 seq+1（`validate` 保证），
                // 因此这里按末条推导。
                let next_seq = all_events.last().map_or(0, |event| event.seq + 1);
                let events = match events_after_cursor(all_events.iter(), next_seq, after) {
                    Ok(events) => events,
                    Err(EventBusError::CursorTooOld { .. } | EventBusError::CursorAhead { .. }) => {
                        return ApiResponse::json(409, error_json("event_cursor_requires_snapshot"))
                    }
                    Err(error) => return ApiResponse::json(500, error_json(&format!("{error:?}"))),
                };
                match serde_json::to_string(&events) {
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
        let after = match parse_after_cursor(query) {
            Ok(after) => after,
            Err(error) => return ApiResponse::json(400, error_json(error)),
        };
        let key = match projection_key_from_query(query) {
            Ok(key) => key,
            Err(error) => return ApiResponse::json(400, error_json(&error)),
        };
        let event_bus = {
            let state = self.state.lock().expect("api state mutex poisoned");
            // 实时游标是账户级状态，缺键没有"全局那条"可退回（V11 F1）。
            match key {
                Some(key) => state
                    .projections
                    .get(&key)
                    .map(|projection| projection.event_bus.clone())
                    .unwrap_or_default(),
                None => {
                    return ApiResponse::json(400, error_json("account_id_and_venue_id_required"))
                }
            }
        };
        match event_bus.read_after(after) {
            Ok(events) => {
                let envelopes = events
                    .into_iter()
                    .map(event_projection_envelope)
                    .collect::<Vec<_>>();
                ApiResponse::json(
                    200,
                    serde_json::to_string(&envelopes)
                        .expect("projection event envelopes are serializable"),
                )
            }
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
        let key = match projection_key_from_query(query) {
            Ok(key) => key,
            Err(error) => return ApiResponse::json(400, error_json(&error)),
        };
        let state = self.state.lock().expect("api state mutex poisoned");
        let (history, target) = match key {
            Some(key) => {
                let Some(projection) = state.projections.get(&key) else {
                    return ApiResponse::json(409, error_json("snapshot_base_not_found"));
                };
                (&projection.snapshot_history, projection.snapshot.as_ref())
            }
            None => (&state.snapshot_history, state.snapshot.as_ref()),
        };
        let Some(base) = history.get(&base_hash) else {
            return ApiResponse::json(409, error_json("snapshot_base_not_found"));
        };
        let Some(target) = target else {
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
                // 受理已经落账，队列却没写进去：把这条 202 念成"已交给执行者"就是假通告。
                // 503 说的是"这一半没成"，补投由执行 worker 每轮的 `pending()` 扫描负责（V11 R6-3）。
                if let Err(error) = enqueuer(queued_command, ts) {
                    return ApiResponse::json(
                        503,
                        error_json(&format!("control_command_not_queued: {error}")),
                    );
                }
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

    /// 处理一条 HTTP/1.1 连接（仅 crate 内可见）：与 `serve` 的区别只在它同步处理
    /// 单条连接、不起线程，供本 crate 的集成用例钉住确定性的请求/响应顺序（V12 §16）。
    #[cfg(test)]
    pub(crate) fn serve_once(&self, listener: &TcpListener, ts: u64) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        let stop: StopToken = Arc::new(|| false);
        self.serve_stream_as(stream, ts, None, &stop)
    }

    /// 使用调用方提供的证书和私钥配置服务端 TLS。
    ///
    /// `ServerConfig` 必须由部署边界构造并安全加载证书/私钥；API 层不提供跳过
    /// TLS 或动态信任任何客户端的快捷开关。HTTP 与 WebSocket 处理仍复用同一套
    /// 权限、审计、快照和事件游标语义。
    #[cfg(test)]
    pub(crate) fn serve_once_tls(
        &self,
        listener: &TcpListener,
        config: Arc<ServerConfig>,
        ts: u64,
    ) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        let stop: StopToken = Arc::new(|| false);
        self.serve_stream_as(tls_stream(stream, config)?, ts, None, &stop)
    }

    fn serve_stream_as<S>(
        &self,
        mut stream: S,
        ts: u64,
        authenticated_operator: Option<&str>,
        stop: &StopToken,
    ) -> std::io::Result<()>
    where
        S: Read + Write,
    {
        let request = read_request(&mut stream, HTTP_REQUEST_BUDGET)?;
        let request = String::from_utf8_lossy(&request);
        self.dispatch_request(&mut stream, &request, ts, authenticated_operator, stop)
    }

    fn dispatch_request<S: Read + Write>(
        &self,
        stream: &mut S,
        request: &str,
        ts: u64,
        authenticated_operator: Option<&str>,
        stop: &StopToken,
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
            return self.serve_websocket(stream, request, stop);
        }
        let response = match parse_http_request(request) {
            Ok(parsed) => {
                self.handle_inner(&parsed.0, &parsed.1, &parsed.2, ts, authenticated_operator)
            }
            Err(error) => ApiResponse::json(400, error_json(&error)),
        };
        write_http_response(stream, &response)
    }

    /// 持续接受 mTLS 连接，并在每个新连接握手时读取当前 TLS 配置和 Operator
    /// 身份映射。两份配置均由调用方的轮询重载器原子替换。
    ///
    /// `stop` 每轮 accept 前问一次：置起之后这条长驻循环会自己收，调用方的 `join`
    /// 才可能返回（V11 J2）；同一枚令牌也递进连接线程，正在场的长连接因此随之收尾（R7-e）。
    pub fn serve_tls_mtls_with_stores(
        &self,
        listener: TcpListener,
        configs: &TlsConfigStore,
        identities: &MtlsIdentityStore,
        ts: u64,
        stop: impl Fn() -> bool + Send + Sync + 'static,
    ) -> std::io::Result<()> {
        let stop: StopToken = Arc::new(stop);
        self.accept_polling(listener, &stop, |stream| {
            configure_connection(&stream)?;
            let stream = tls_stream(stream, configs.current())?;
            let policy = identities.current();
            let operator_id = policy
                .operator_for(stream.conn.peer_certificates())
                .map(str::to_string);
            self.spawn_connection(stream, ts, operator_id, Arc::clone(&stop));
            Ok(())
        })
    }

    /// 每个长连接独立处理，避免 WebSocket 或慢客户端占住监听循环。
    /// 连接线程只拥有 API 的共享读模型和不可变服务配置；领域事实仍由
    /// Runtime owner 写入，连接处理失败只影响当前客户端。
    ///
    /// 起线程前先领一格长连接名额（V11 R7-e）：领不到就地回 503 而不是再开一颗线程；已给的
    /// 那一格由 [`LiveConnectionSlot::drop`] 归还，处理函数 panic 时也归还。
    fn spawn_connection<S>(&self, stream: S, ts: u64, operator_id: Option<String>, stop: StopToken)
    where
        S: Read + Write + Send + 'static,
    {
        let Some(slot) = self.live.reserve() else {
            self.metrics
                .connections_rejected_total
                .fetch_add(1, Ordering::Relaxed);
            reject_overloaded(stream);
            return;
        };
        let service = self.clone();
        std::thread::spawn(move || {
            let _slot = slot;
            if let Err(error) = service.serve_stream_as(stream, ts, operator_id.as_deref(), &stop) {
                eprintln!("[qx-api] connection closed with error: {error}");
            }
        });
    }

    /// 长驻明文服务循环。`stop` 每轮问一次，令牌置起后返回 `Ok(())` 而不是永远等在 accept。
    pub fn serve(
        &self,
        listener: TcpListener,
        ts: u64,
        stop: impl Fn() -> bool + Send + Sync + 'static,
    ) -> std::io::Result<()> {
        let stop: StopToken = Arc::new(stop);
        self.accept_polling(listener, &stop, |stream| {
            configure_connection(&stream)?;
            self.spawn_connection(stream, ts, None, Arc::clone(&stop));
            Ok(())
        })
    }

    /// 带停机令牌地接受连接：非阻塞轮询 + 固定节拍，因此不需要额外的自连唤醒或信号依赖。
    /// 两条长驻入口（明文与 mTLS）共用这一颗，终止性只有一处实现。
    fn accept_polling<F>(
        &self,
        listener: TcpListener,
        stop: &StopToken,
        mut on_connection: F,
    ) -> std::io::Result<()>
    where
        F: FnMut(TcpStream) -> std::io::Result<()>,
    {
        listener.set_nonblocking(true)?;
        loop {
            if stop() {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    // 被接出来的流显式转回阻塞：监听套接字开了非阻塞之后，流是否继承这个模式
                    // 各平台不一致，而紧跟着的 `configure_connection` 要设读超时。本机
                    // （Windows + rustc 1.98）实测拿掉这一行不红，仍按防御性复位保留。
                    stream.set_nonblocking(false)?;
                    on_connection(stream)?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) => return Err(error),
            }
        }
        let _ = listener.set_nonblocking(false);
        Ok(())
    }

    fn serve_websocket<S: Read + Write>(
        &self,
        stream: &mut S,
        request: &str,
        stop: &StopToken,
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
        // 升级之前先问清这条流属于哪个账户：缺键时 400 比"101 之后永远静默"诚实（V11 F1）。
        let projection_key = match projection_key_from_query(websocket_query(request)) {
            Ok(Some(key)) => key,
            Ok(None) => {
                stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")?;
                return Ok(());
            }
            Err(error) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, error)),
        };
        let handshake = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(handshake.as_bytes())?;
        // 流只接在账户投影上：桥接线程按 account_id+venue_id 写 projections，
        // 而一条事实都没投影过来时接上空总线等推送，不退回那份没有写入者的全局面。
        let (event_bus, projected, snapshot) = {
            let state = self.state.lock().expect("api state mutex poisoned");
            match state.projections.get(&projection_key) {
                Some(projection) => (
                    projection.event_bus.clone(),
                    projection.events.events().to_vec(),
                    projection.snapshot.clone(),
                ),
                None => (ApiEventBus::default(), Vec::new(), None),
            }
        };
        write_ws_text(stream, "{\"type\":\"connected\",\"stream\":\"qianxing\"}")?;
        if let Some(snapshot) = &snapshot {
            write_ws_text(
                stream,
                &format!("{{\"type\":\"snapshot\",\"data\":{}}}", snapshot.to_json()),
            )?;
        }
        if !projected.is_empty() {
            let events = projected
                .iter()
                .cloned()
                .map(event_projection_envelope)
                .collect::<Vec<_>>();
            let events = serde_json::to_string(&events)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            write_ws_text(
                stream,
                &format!("{{\"type\":\"events\",\"data\":{events}}}"),
            )?;
        }
        let mut cursor = projected.last().map(|event| event.seq);
        let mut client_buffer = [0_u8; 2048];
        loop {
            // 停机请求之后不再等新事件：线程与名额要归还，进程才可能把长连接收干净。少了这一臂，
            // 一条静默不关（半开、不 FIN）的流会占到进程退出，并把一格名额一起占死（V11 R7-e）。
            if stop() {
                return Ok(());
            }
            match event_bus.wait_after(cursor, Duration::from_millis(100)) {
                Ok(events) => {
                    for event in events {
                        let event_seq = event.seq;
                        let envelope = event_projection_envelope(event);
                        write_ws_text(
                            stream,
                            &format!(
                                "{{\"type\":\"event\",\"data\":{}}}",
                                serde_json::to_string(&envelope).map_err(|error| {
                                    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                                })?
                            ),
                        )?;
                        cursor = Some(event_seq);
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

fn event_projection_envelope(event: Event) -> ProjectionEnvelope<Event> {
    let context = event.metadata.context.clone();
    let mut digest = Fnv1a::new();
    event.digest(&mut digest);
    let state_hash = digest.finish();
    ProjectionEnvelope {
        schema_version: PROJECTION_ENVELOPE_SCHEMA_VERSION,
        kind: "event".into(),
        tenant_id: non_empty_or_system(context.tenant_id),
        run_id: non_empty_or_system(context.run_id),
        account_id: non_empty_or_system(context.account_id),
        portfolio_id: non_empty_or_system(context.portfolio_id),
        venue_id: non_empty_or_system(event_venue_id(&event)),
        as_of: event.engine_time,
        event_seq: event.seq,
        cursor: format!("{}:{state_hash:016x}", event.seq),
        state_hash,
        source: "eventlog".into(),
        // 这份信封来自进程内总线，它没有可发布的整体摘要：三格血缘如实缺席，
        // 而不是填一个组件名冒充摘要（V11 L2）。
        lineage: ProjectionLineage::default(),
        data: event,
    }
}

fn non_empty_or_system(value: String) -> String {
    if value.trim().is_empty() {
        "system".into()
    } else {
        value
    }
}

fn event_venue_id(event: &Event) -> String {
    match &event.kind {
        EventKind::AccountBalanceSnapshot { venue_id, .. }
        | EventKind::AccountPositionSnapshot { venue_id, .. } => venue_id.clone(),
        EventKind::AccountCashflow { cashflow } => cashflow.venue_id.clone(),
        EventKind::OrderSubmitted { order } => order.instrument.venue.to_string(),
        EventKind::Filled { fill } => fill.venue_id.clone().unwrap_or_default(),
        EventKind::LedgerApplied { entry } => entry
            .instrument
            .as_ref()
            .map(|instrument| instrument.venue.to_string())
            .unwrap_or_default(),
        _ => event.metadata.context.account_id.clone(),
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
        // 与 HTTP 端点同一个读点（V11 H1）：装了 provider 就是 store 那一份。这里的
        // 退回不是把失败念成空表——现读失败时退回进程内的最后已知副本，而 HTTP 侧对
        // 同一次失败如实报 503（trait 签名没有 Result，降级由端点表达）。
        self.control_plane_last_known().audit().to_vec()
    }

    fn control_retirement(&self) -> RetirementSummary {
        // 流水与摘要必须出自同一次现读：分两次读会把"这一页窗口"和"窗口外累计"念成
        // 两个时刻的账（V11 R7-8）。
        self.control_plane_last_known().retirement()
    }

    fn job_runs(&self) -> Vec<JobRun> {
        self.query_models_last_known().job_runs
    }

    fn ledger_entries(&self) -> Vec<LedgerEntry> {
        self.query_models_last_known().ledger_entries
    }

    fn reconcile_reports(&self) -> Vec<ReconcileReportSnapshot> {
        self.query_models_last_known().reconcile_reports
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

/// 长驻 accept 循环查停机令牌的节拍。空闲时一晚也就几千次 load，换来不用引信号处理依赖。
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 一台 API 进程同时在场的长连接上界（V11 R7-e）。每接一条就是一颗 OS 线程（默认栈 8 MiB），
/// 64 颗 ≈ 0.5 GiB 虚拟内存，而这条链的真实读者是运维控制台：每操作员每块面板一条流，到不了
/// 这个数。上界的价值在于“到不了”这件事可被证明，不在于贴着真实流量调；要按部署改它得先添
/// 配置面，本轮零新配置项。
const MAX_LIVE_CONNECTIONS: usize = 64;

/// 一个完整请求（头部 + body）允许的**整体**时长。`configure_connection` 的 100 ms 只界住
/// **单次** `read`：对端每 99 ms 挤一个字节就能让读取循环永远读不完，1 MiB 的体积上限要
/// 29 小时才挡得住——体积有界不等于时长有界（V11 R4-9，与 O7 的握手块同一判据）。
const HTTP_REQUEST_BUDGET: Duration = Duration::from_secs(5);

/// 单个请求的字节上限（头部 + body）。
const HTTP_REQUEST_MAX_BYTES: usize = 1_048_576;

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

fn read_request<S: Read>(stream: &mut S, budget: Duration) -> std::io::Result<Vec<u8>> {
    let deadline = Instant::now() + budget;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    // 头部扫描的游标：`\r\n\r\n` 只可能在"上次扫过的位置 − 3 字节"之后新出现，因此每一轮
    // 只看新到的字节。修前是整缓冲区重扫，被逐字节喂满时一次请求要走平方级的比较。
    let mut scanned_upto = 0_usize;
    let mut expected: Option<usize> = None;
    loop {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "http request exceeded overall budget {budget:?} after {} bytes",
                    request.len()
                ),
            ));
        }
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > HTTP_REQUEST_MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http request too large",
            ));
        }
        if expected.is_none() {
            let from = scanned_upto;
            scanned_upto = request.len().saturating_sub(3);
            if let Some(offset) = request[from..]
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                expected = Some(expected_request_length(&request, from + offset + 4)?);
            }
        }
        if request.len() >= expected.unwrap_or(usize::MAX) {
            break;
        }
    }
    Ok(request)
}

/// 头部到手之后还算不出该收多少字节就不算读完：`Content-Length` 缺失按 0 处理，
/// 与头部一起封顶，超过体量的声明当场拒掉。
fn expected_request_length(request: &[u8], header_end: usize) -> std::io::Result<usize> {
    let header = String::from_utf8_lossy(&request[..header_end]);
    let mut content_length = 0_usize;
    for line in header.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            content_length = value.trim().parse::<usize>().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Content-Length")
            })?;
            break;
        }
    }
    let expected = header_end.checked_add(content_length).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "request length overflow")
    })?;
    if expected > HTTP_REQUEST_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "http request too large",
        ));
    }
    Ok(expected)
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
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
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

/// 名额满时的拒绝。写不出去也照样收尾：这一格买不到“客户端一定看得见 503”的保证——
/// mTLS 流还没握手完时写不进去。被拒的条数先计进 `/metrics`，运维数得到，而不是只看见静默。
///
/// 回话之前先把已经到达的字节读掉：带着未读数据关闭套接字会发出 RST 而不是 FIN，那句 503
/// 就被自己抹掉了（Windows 上读侧直接看到 ConnectionReset）。这一读有 `configure_connection`
/// 的 100 ms 上界，不重走 `read_request` 的整请求装配——名额满时最不该做的就是多干活。
fn reject_overloaded<S: Read + Write>(mut stream: S) {
    let mut drained = [0_u8; 2048];
    let _ = stream.read(&mut drained);
    let response = ApiResponse::json(503, error_json("too_many_live_connections"));
    let _ = write_http_response(&mut stream, &response);
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

fn projection_key_from_query(query: &str) -> Result<Option<ApiProjectionKey>, String> {
    let account_id = query_value(query, "account_id");
    let venue_id = query_value(query, "venue_id");
    match (account_id, venue_id) {
        (None, None) => Ok(None),
        (Some(account_id), Some(venue_id)) => {
            let key = ApiProjectionKey::new(account_id, venue_id);
            key.validate()?;
            Ok(Some(key))
        }
        _ => Err("account_id 和 venue_id 必须同时提供".into()),
    }
}

/// 从 WebSocket 握手请求行取查询串（`GET /stream?account_id=..&venue_id=..`）；没有 `?` 给空串。
fn websocket_query(request: &str) -> &str {
    request.split_whitespace().nth(1).map_or("", |path| {
        path.split_once('?').map_or("", |(_, query)| query)
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
mod tests;
