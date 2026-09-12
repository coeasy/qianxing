//! C ABI host binding for in-process Rust/C++ strategies.
//!
//! The layout intentionally mirrors `cpp/include/qianxing_strategy.h`. The
//! host owns the vtable (usually obtained from a shared library loader), while
//! this adapter owns only the opaque strategy handle and copies decisions out
//! of plugin-owned memory before calling `free_decision`.

use super::{MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent};
use qx_core::{InstrumentId, OrderPolicy, PositionSide, Price, Quantity, Side};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Read;
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::ptr;

use ring::signature::{UnparsedPublicKey, ED25519};

const DEFAULT_MAX_LIBRARY_BYTES: u64 = 256 * 1024 * 1024;

pub const QX_C_STRATEGY_API_VERSION: u32 = 1;
const MAX_PLUGIN_INTENTS: usize = 100_000;
const ERROR_BUFFER_SIZE: usize = 512;

pub fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QxRaw128 {
    pub lo: u64,
    pub hi: i64,
}

impl QxRaw128 {
    pub fn from_i128(value: i128) -> Self {
        Self {
            lo: value as u128 as u64,
            hi: (value >> 64) as i64,
        }
    }

    pub fn into_i128(self) -> i128 {
        ((self.hi as i128) << 64) | self.lo as i128
    }
}

#[repr(C)]
pub struct QxStrategyKv {
    pub key: *const c_char,
    pub value_raw: QxRaw128,
}

#[repr(C)]
pub struct QxStrategyContext {
    pub schema_version: u32,
    pub strategy_id: *const c_char,
    pub strategy_version: *const c_char,
    pub account_id: *const c_char,
    pub venue_id: *const c_char,
    pub data_fingerprint: *const c_char,
    pub as_of: u64,
    pub positions: *const QxStrategyKv,
    pub positions_len: usize,
    pub cash: *const QxStrategyKv,
    pub cash_len: usize,
    pub available_margin_raw: QxRaw128,
    pub has_available_margin: u8,
    pub risk_state: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QxMarketEventKind {
    Bar = 1,
    Tick = 2,
    Timer = 3,
    OrderBook = 4,
}

#[repr(C)]
pub struct QxBookLevel {
    pub price_raw: QxRaw128,
    pub qty_raw: QxRaw128,
}

#[repr(C)]
pub struct QxMarketEvent {
    pub schema_version: u32,
    pub kind: QxMarketEventKind,
    pub instrument: *const c_char,
    pub ts: u64,
    pub open_raw: QxRaw128,
    pub high_raw: QxRaw128,
    pub low_raw: QxRaw128,
    pub close_raw: QxRaw128,
    pub volume_raw: QxRaw128,
    pub bid_raw: QxRaw128,
    pub ask_raw: QxRaw128,
    pub last_raw: QxRaw128,
    pub has_last: u8,
    pub sequence: u64,
    pub bids: *const QxBookLevel,
    pub bids_len: usize,
    pub asks: *const QxBookLevel,
    pub asks_len: usize,
    pub timer_name: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QxOrderSide {
    Buy = 1,
    Sell = 2,
}

#[repr(C)]
pub struct QxOrderIntent {
    pub intent_id: u64,
    pub instrument: *const c_char,
    pub side: QxOrderSide,
    pub qty_raw: QxRaw128,
    pub limit_price_raw: QxRaw128,
    pub has_limit_price: u8,
    pub reduce_only: u8,
    pub post_only: u8,
    pub position_side: *const c_char,
}

#[repr(C)]
pub struct QxStrategyDecision {
    pub schema_version: u32,
    pub request_id: *const c_char,
    pub strategy_id: *const c_char,
    pub signal_id: u64,
    pub confidence: QxRaw128,
    pub priority: i32,
    pub expires_at: u64,
    pub intents: *mut QxOrderIntent,
    pub intents_len: usize,
}

pub type QxStrategyHandle = *mut c_void;

#[repr(C)]
pub struct QxStrategyVTable {
    pub abi_version: u32,
    pub create: Option<unsafe extern "C" fn(*const c_char) -> QxStrategyHandle>,
    pub on_init: Option<
        unsafe extern "C" fn(QxStrategyHandle, *const QxStrategyContext, *mut c_char, usize) -> i32,
    >,
    pub on_event: Option<
        unsafe extern "C" fn(
            QxStrategyHandle,
            *const QxStrategyContext,
            *const QxMarketEvent,
            *mut QxStrategyDecision,
            *mut c_char,
            usize,
        ) -> i32,
    >,
    pub on_order_update: Option<
        unsafe extern "C" fn(
            QxStrategyHandle,
            *const c_char,
            *mut QxStrategyDecision,
            *mut c_char,
            usize,
        ) -> i32,
    >,
    pub free_decision: Option<unsafe extern "C" fn(QxStrategyHandle, *mut QxStrategyDecision)>,
    pub destroy: Option<unsafe extern "C" fn(QxStrategyHandle)>,
}

// These fields intentionally exist to keep the C pointers alive while a
// callback runs; the wire struct is the only field read by the host.
#[allow(dead_code)]
struct ContextBuffers {
    strategy_id: CString,
    strategy_version: CString,
    account_id: CString,
    venue_id: CString,
    data_fingerprint: CString,
    risk_state: CString,
    position_keys: Vec<CString>,
    cash_keys: Vec<CString>,
    positions: Vec<QxStrategyKv>,
    cash: Vec<QxStrategyKv>,
    context: QxStrategyContext,
}

impl ContextBuffers {
    fn new(context: &StrategyContext) -> Result<Self, String> {
        let strategy_id = cstring(&context.strategy_id, "strategy_id")?;
        let strategy_version = cstring(&context.strategy_version, "strategy_version")?;
        let account_id = cstring(&context.account_id, "account_id")?;
        let venue_id = cstring(&context.venue_id, "venue_id")?;
        let data_fingerprint = cstring(&context.data_fingerprint, "data_fingerprint")?;
        let risk_state = cstring(&context.risk_state, "risk_state")?;
        let position_keys = context
            .positions
            .keys()
            .map(|key| cstring(key, "position instrument"))
            .collect::<Result<Vec<_>, _>>()?;
        let cash_keys = context
            .cash
            .keys()
            .map(|key| cstring(key, "cash currency"))
            .collect::<Result<Vec<_>, _>>()?;
        let positions = position_keys
            .iter()
            .zip(context.positions.values())
            .map(|(key, value)| QxStrategyKv {
                key: key.as_ptr(),
                value_raw: QxRaw128::from_i128(*value),
            })
            .collect::<Vec<_>>();
        let cash = cash_keys
            .iter()
            .zip(context.cash.values())
            .map(|(key, value)| QxStrategyKv {
                key: key.as_ptr(),
                value_raw: QxRaw128::from_i128(*value),
            })
            .collect::<Vec<_>>();
        let (available_margin_raw, has_available_margin) = context
            .available_margin_raw
            .map(|value| (QxRaw128::from_i128(value), 1))
            .unwrap_or((QxRaw128::from_i128(0), 0));
        let context_wire = QxStrategyContext {
            schema_version: QX_C_STRATEGY_API_VERSION,
            strategy_id: strategy_id.as_ptr(),
            strategy_version: strategy_version.as_ptr(),
            account_id: account_id.as_ptr(),
            venue_id: venue_id.as_ptr(),
            data_fingerprint: data_fingerprint.as_ptr(),
            as_of: context.as_of,
            positions: positions.as_ptr(),
            positions_len: positions.len(),
            cash: cash.as_ptr(),
            cash_len: cash.len(),
            available_margin_raw,
            has_available_margin,
            risk_state: risk_state.as_ptr(),
        };
        Ok(Self {
            strategy_id,
            strategy_version,
            account_id,
            venue_id,
            data_fingerprint,
            risk_state,
            position_keys,
            cash_keys,
            positions,
            cash,
            context: context_wire,
        })
    }
}

// Same ownership role as ContextBuffers: vectors and C strings back pointers
// stored in `event` for the duration of the plugin callback.
#[allow(dead_code)]
struct EventBuffers {
    instrument: Option<CString>,
    timer_name: Option<CString>,
    bids: Vec<QxBookLevel>,
    asks: Vec<QxBookLevel>,
    event: QxMarketEvent,
}

impl EventBuffers {
    fn new(event: &MarketEvent) -> Result<Self, String> {
        let instrument = event
            .instrument()
            .map(|value| cstring(&value.to_string(), "event instrument"))
            .transpose()?;
        let timer_name = match event {
            MarketEvent::Timer { name, .. } => Some(cstring(name, "timer name")?),
            _ => None,
        };
        let zero = QxRaw128::from_i128(0);
        let mut event_wire = QxMarketEvent {
            schema_version: QX_C_STRATEGY_API_VERSION,
            kind: QxMarketEventKind::Timer,
            instrument: instrument
                .as_ref()
                .map_or(ptr::null(), |value| value.as_c_str().as_ptr()),
            ts: event.ts(),
            open_raw: zero,
            high_raw: zero,
            low_raw: zero,
            close_raw: zero,
            volume_raw: zero,
            bid_raw: zero,
            ask_raw: zero,
            last_raw: zero,
            has_last: 0,
            sequence: 0,
            bids: ptr::null(),
            bids_len: 0,
            asks: ptr::null(),
            asks_len: 0,
            timer_name: timer_name
                .as_ref()
                .map_or(ptr::null(), |value| value.as_c_str().as_ptr()),
        };
        let (bids, asks) = match event {
            MarketEvent::Bar {
                open_raw,
                high_raw,
                low_raw,
                close_raw,
                volume_raw,
                ..
            } => {
                event_wire.kind = QxMarketEventKind::Bar;
                event_wire.open_raw = QxRaw128::from_i128(*open_raw);
                event_wire.high_raw = QxRaw128::from_i128(*high_raw);
                event_wire.low_raw = QxRaw128::from_i128(*low_raw);
                event_wire.close_raw = QxRaw128::from_i128(*close_raw);
                event_wire.volume_raw = QxRaw128::from_i128(*volume_raw);
                (Vec::new(), Vec::new())
            }
            MarketEvent::Tick {
                bid_raw,
                ask_raw,
                last_raw,
                volume_raw,
                ..
            } => {
                event_wire.kind = QxMarketEventKind::Tick;
                event_wire.bid_raw = QxRaw128::from_i128(*bid_raw);
                event_wire.ask_raw = QxRaw128::from_i128(*ask_raw);
                if let Some(value) = last_raw {
                    event_wire.last_raw = QxRaw128::from_i128(*value);
                    event_wire.has_last = 1;
                }
                if let Some(value) = volume_raw {
                    event_wire.volume_raw = QxRaw128::from_i128(*value);
                }
                (Vec::new(), Vec::new())
            }
            MarketEvent::OrderBook {
                sequence,
                bids,
                asks,
                ..
            } => {
                event_wire.kind = QxMarketEventKind::OrderBook;
                event_wire.sequence = *sequence;
                let bids = bids
                    .iter()
                    .map(|level| QxBookLevel {
                        price_raw: QxRaw128::from_i128(level.price_raw),
                        qty_raw: QxRaw128::from_i128(level.qty_raw),
                    })
                    .collect::<Vec<_>>();
                let asks = asks
                    .iter()
                    .map(|level| QxBookLevel {
                        price_raw: QxRaw128::from_i128(level.price_raw),
                        qty_raw: QxRaw128::from_i128(level.qty_raw),
                    })
                    .collect::<Vec<_>>();
                (bids, asks)
            }
            MarketEvent::Timer { .. } => (Vec::new(), Vec::new()),
        };
        event_wire.bids = bids.as_ptr();
        event_wire.bids_len = bids.len();
        event_wire.asks = asks.as_ptr();
        event_wire.asks_len = asks.len();
        Ok(Self {
            instrument,
            timer_name,
            bids,
            asks,
            event: event_wire,
        })
    }
}

/// An in-process C ABI strategy. The shared library loader is intentionally
/// outside this type: applications can choose OS-specific loading, signing,
/// and sandbox policy before passing a validated vtable here.
pub struct CAbiStrategy {
    vtable: &'static QxStrategyVTable,
    handle: QxStrategyHandle,
}

impl CAbiStrategy {
    /// # Safety
    ///
    /// `vtable` must remain valid for the lifetime of the returned strategy,
    /// and every callback must obey the ownership rules in the public C ABI.
    pub unsafe fn from_vtable(
        vtable: &'static QxStrategyVTable,
        config_json: &str,
    ) -> Result<Self, String> {
        if vtable.abi_version != QX_C_STRATEGY_API_VERSION
            || vtable.create.is_none()
            || vtable.on_init.is_none()
            || vtable.on_event.is_none()
            || vtable.free_decision.is_none()
            || vtable.destroy.is_none()
        {
            return Err("C ABI Strategy vtable 版本或回调不完整".into());
        }
        let config = cstring(config_json, "config_json")?;
        let handle = (vtable.create.expect("validated create"))(config.as_ptr());
        if handle.is_null() {
            return Err("C ABI Strategy create 返回空句柄".into());
        }
        Ok(Self { vtable, handle })
    }

    fn call_error(label: &str, code: i32, buffer: &[c_char; ERROR_BUFFER_SIZE]) -> String {
        let detail = unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_string();
        if detail.is_empty() {
            format!("{label} 失败 code={code}")
        } else {
            format!("{label} 失败 code={code}: {detail}")
        }
    }

    fn decode_decision(
        &self,
        decision: &QxStrategyDecision,
        context: &StrategyContext,
        event_ts: u64,
    ) -> Result<StrategyDecision, String> {
        if decision.intents_len > MAX_PLUGIN_INTENTS
            || (decision.intents_len > 0 && decision.intents.is_null())
        {
            return Err("C ABI Strategy 返回的 intents 指针或长度非法".into());
        }
        let request_id = read_required_string(decision.request_id, "request_id")?;
        let strategy_id = read_required_string(decision.strategy_id, "strategy_id")?;
        let raw_intents =
            unsafe { std::slice::from_raw_parts(decision.intents, decision.intents_len) };
        let mut intents = Vec::with_capacity(raw_intents.len());
        for raw in raw_intents {
            let instrument_text = read_required_string(raw.instrument, "intent instrument")?;
            let instrument = InstrumentId::parse(&instrument_text)
                .ok_or_else(|| format!("C ABI intent instrument 非法: {instrument_text}"))?;
            let side = match raw.side {
                QxOrderSide::Buy => Side::Buy,
                QxOrderSide::Sell => Side::Sell,
            };
            let policy = if raw.position_side.is_null() {
                None
            } else {
                let position_side = read_required_string(raw.position_side, "position_side")?;
                Some(OrderPolicy {
                    position_side: match position_side.to_ascii_lowercase().as_str() {
                        "long" => PositionSide::Long,
                        "short" => PositionSide::Short,
                        "net" => PositionSide::Net,
                        other => return Err(format!("C ABI position_side 非法: {other}")),
                    },
                    ..OrderPolicy::default()
                })
            };
            intents.push(StrategyOrderIntent {
                intent_id: raw.intent_id,
                instrument,
                side,
                qty: Quantity::from_raw(raw.qty_raw.into_i128()),
                limit: (raw.has_limit_price != 0)
                    .then(|| Price::from_raw(raw.limit_price_raw.into_i128())),
                policy,
                reduce_only: raw.reduce_only != 0,
                post_only: raw.post_only != 0,
            });
        }
        let output = StrategyDecision {
            schema_version: decision.schema_version,
            request_id,
            strategy_id,
            signal_id: decision.signal_id,
            confidence: decision.confidence.into_i128(),
            priority: decision.priority,
            expires_at: decision.expires_at,
            intents,
        };
        output.validate_for(context, event_ts)?;
        Ok(output)
    }
}

impl Strategy for CAbiStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        context.validate()?;
        let buffers = ContextBuffers::new(context)?;
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            (self.vtable.on_init.expect("validated on_init"))(
                self.handle,
                &buffers.context,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error("C ABI Strategy on_init", code, &error))
        }
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        context.validate()?;
        event.validate()?;
        let context_buffers = ContextBuffers::new(context)?;
        let event_buffers = EventBuffers::new(event)?;
        let mut decision = QxStrategyDecision {
            schema_version: 0,
            request_id: ptr::null(),
            strategy_id: ptr::null(),
            signal_id: 0,
            confidence: QxRaw128::from_i128(0),
            priority: 0,
            expires_at: 0,
            intents: ptr::null_mut(),
            intents_len: 0,
        };
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            (self.vtable.on_event.expect("validated on_event"))(
                self.handle,
                &context_buffers.context,
                &event_buffers.event,
                &mut decision,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code != 0 {
            return Err(Self::call_error("C ABI Strategy on_event", code, &error));
        }
        let decoded = self.decode_decision(&decision, context, event.ts());
        unsafe {
            (self.vtable.free_decision.expect("validated free_decision"))(
                self.handle,
                &mut decision,
            );
        }
        decoded
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        let Some(callback) = self.vtable.on_order_update else {
            return Ok(None);
        };
        let update = cstring(update, "order update")?;
        let mut decision = QxStrategyDecision {
            schema_version: 0,
            request_id: ptr::null(),
            strategy_id: ptr::null(),
            signal_id: 0,
            confidence: QxRaw128::from_i128(0),
            priority: 0,
            expires_at: 0,
            intents: ptr::null_mut(),
            intents_len: 0,
        };
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                update.as_ptr(),
                &mut decision,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code != 0 {
            return Err(Self::call_error(
                "C ABI Strategy on_order_update",
                code,
                &error,
            ));
        }
        if decision.signal_id == 0 {
            unsafe {
                (self.vtable.free_decision.expect("validated free_decision"))(
                    self.handle,
                    &mut decision,
                );
            }
            return Ok(None);
        }
        let decoded = self.decode_decision(&decision, context, context.as_of);
        unsafe {
            (self.vtable.free_decision.expect("validated free_decision"))(
                self.handle,
                &mut decision,
            );
        }
        decoded.map(Some)
    }
}

impl Drop for CAbiStrategy {
    fn drop(&mut self) {
        unsafe {
            (self.vtable.destroy.expect("validated destroy"))(self.handle);
        }
    }
}

unsafe impl Send for CAbiStrategy {}

/// 动态库版 C ABI Host。库句柄字段放在 `inner` 之后，确保 Rust 先销毁
/// 插件策略句柄，再卸载包含 vtable 和回调代码的共享库。
pub struct DynamicCAbiStrategy {
    inner: CAbiStrategy,
    _library: libloading::Library,
}

/// 动态插件加载前的可审计策略。
///
/// `expected_sha256` 必须来自受信任发布清单；没有摘要白名单时，调用方
/// 应使用独立进程 JSONL 策略，而不是把未验证的库加载进交易进程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCAbiLoadPolicy {
    pub expected_sha256: String,
    pub max_library_bytes: u64,
    /// Optional detached Ed25519 signature over the exact library bytes.
    /// The public key must come from a deployment trust root; this field does
    /// not itself provide key rotation or sandboxing.
    pub ed25519_public_key: Option<String>,
    pub ed25519_signature: Option<String>,
}

impl DynamicCAbiLoadPolicy {
    pub fn new(expected_sha256: impl Into<String>) -> Self {
        Self {
            expected_sha256: expected_sha256.into(),
            max_library_bytes: DEFAULT_MAX_LIBRARY_BYTES,
            ed25519_public_key: None,
            ed25519_signature: None,
        }
    }

    pub fn with_max_library_bytes(mut self, max_library_bytes: u64) -> Self {
        self.max_library_bytes = max_library_bytes;
        self
    }

    pub fn with_ed25519_signature(
        mut self,
        public_key_hex: impl Into<String>,
        signature_hex: impl Into<String>,
    ) -> Self {
        self.ed25519_public_key = Some(public_key_hex.into());
        self.ed25519_signature = Some(signature_hex.into());
        self
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_library_bytes == 0 {
            return Err("C ABI Strategy 动态库大小上限必须大于0".into());
        }
        if self.expected_sha256.len() != 64
            || !self
                .expected_sha256
                .chars()
                .all(|value| value.is_ascii_hexdigit())
        {
            return Err("C ABI Strategy expected_sha256 必须是64位十六进制摘要".into());
        }
        if self.ed25519_public_key.is_some() != self.ed25519_signature.is_some() {
            return Err("C ABI Strategy Ed25519 公钥和签名必须成对配置".into());
        }
        if let Some(public_key) = &self.ed25519_public_key {
            if public_key.len() != 64 || !public_key.chars().all(|value| value.is_ascii_hexdigit())
            {
                return Err("C ABI Strategy Ed25519 公钥必须是32字节十六进制".into());
            }
        }
        if let Some(signature) = &self.ed25519_signature {
            if signature.len() != 128 || !signature.chars().all(|value| value.is_ascii_hexdigit()) {
                return Err("C ABI Strategy Ed25519 签名必须是64字节十六进制".into());
            }
        }
        Ok(())
    }
}

impl DynamicCAbiStrategy {
    /// # Safety
    ///
    /// 动态库必须是受信任、与当前进程 ABI/架构兼容的插件；不受信任插件
    /// 应在独立进程或沙箱内运行，而不是直接加载到交易进程。
    pub unsafe fn load(path: impl AsRef<Path>, config_json: &str) -> Result<Self, String> {
        Self::load_unverified(path.as_ref(), config_json)
    }

    /// 仅在调用方已经通过外部发布策略校验后加载动态库。
    ///
    /// 该入口在 `Library::new` 之前校验普通文件、大小和 SHA-256，避免把
    /// 被替换或异常膨胀的文件交给动态链接器。它不提供沙箱；不受信任代码
    /// 仍必须使用独立进程策略边界。
    ///
    /// # Safety
    ///
    /// The verified library must still obey the public C ABI ownership and
    /// callback rules. Hash verification authenticates the selected file,
    /// but does not make arbitrary native code memory-safe or sandboxed.
    pub unsafe fn load_verified(
        path: impl AsRef<Path>,
        config_json: &str,
        policy: &DynamicCAbiLoadPolicy,
    ) -> Result<Self, String> {
        policy.validate()?;
        verify_library_file(path.as_ref(), policy)?;
        Self::load_unverified(path.as_ref(), config_json)
    }

    unsafe fn load_unverified(path: &Path, config_json: &str) -> Result<Self, String> {
        let library = libloading::Library::new(path)
            .map_err(|error| format!("加载 C ABI Strategy 动态库失败: {error}"))?;
        let get_vtable: libloading::Symbol<unsafe extern "C" fn() -> *const QxStrategyVTable> =
            library
                .get(b"qx_strategy_get_vtable\0")
                .map_err(|error| format!("C ABI Strategy 缺少 qx_strategy_get_vtable: {error}"))?;
        let vtable_ptr = get_vtable();
        if vtable_ptr.is_null() {
            return Err("C ABI Strategy qx_strategy_get_vtable 返回空指针".into());
        }
        // The library is stored in this struct and dropped after `inner`; the
        // erased lifetime is therefore bounded by DynamicCAbiStrategy itself.
        let vtable: &'static QxStrategyVTable = &*vtable_ptr;
        let inner = CAbiStrategy::from_vtable(vtable, config_json)?;
        Ok(Self {
            inner,
            _library: library,
        })
    }

    pub fn inner(&self) -> &CAbiStrategy {
        &self.inner
    }
}

/// 校验独立策略文件的 SHA-256。该函数不加载文件、不执行文件，供
/// external_executable/Python `.py` 进程边界在 spawn 前做发布物校验。
pub fn verify_file_sha256(path: impl AsRef<Path>, expected_sha256: &str) -> Result<(), String> {
    let path = path.as_ref();
    if expected_sha256.len() != 64
        || !expected_sha256
            .chars()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err("策略文件 expected_sha256 必须是64位十六进制摘要".into());
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("读取策略文件元数据失败 {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("策略文件不是非空普通文件: {}", path.display()));
    }
    let mut file = File::open(path)
        .map_err(|error| format!("打开策略文件失败 {}: {error}", path.display()))?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取策略文件失败 {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    let actual_hex = context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !actual_hex.eq_ignore_ascii_case(expected_sha256) {
        return Err(format!(
            "策略文件 SHA-256 不匹配: expected={} actual={}",
            expected_sha256, actual_hex
        ));
    }
    Ok(())
}

fn verify_library_file(path: &Path, policy: &DynamicCAbiLoadPolicy) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("读取 C ABI Strategy 动态库元数据失败: {error}"))?;
    if !metadata.is_file() {
        return Err("C ABI Strategy 动态库路径不是普通文件".into());
    }
    if metadata.len() == 0 || metadata.len() > policy.max_library_bytes {
        return Err(format!(
            "C ABI Strategy 动态库大小非法: bytes={} max={}",
            metadata.len(),
            policy.max_library_bytes
        ));
    }
    let mut file =
        File::open(path).map_err(|error| format!("打开 C ABI Strategy 动态库失败: {error}"))?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    let mut signed_bytes = if policy.ed25519_signature.is_some() {
        let capacity = usize::try_from(metadata.len())
            .map_err(|_| "C ABI Strategy 动态库大小超过当前平台可寻址范围".to_string())?;
        Some(Vec::with_capacity(capacity))
    } else {
        None
    };
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取 C ABI Strategy 动态库失败: {error}"))?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
        if let Some(bytes) = signed_bytes.as_mut() {
            bytes.extend_from_slice(&buffer[..read]);
        }
    }
    let actual = context.finish();
    let actual_hex = actual
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !actual_hex.eq_ignore_ascii_case(&policy.expected_sha256) {
        return Err(format!(
            "C ABI Strategy 动态库 SHA-256 不匹配: expected={} actual={}",
            policy.expected_sha256, actual_hex
        ));
    }
    if let (Some(public_key), Some(signature), Some(bytes)) = (
        policy.ed25519_public_key.as_deref(),
        policy.ed25519_signature.as_deref(),
        signed_bytes.as_deref(),
    ) {
        let public_key = decode_hex(public_key, 32, "公钥")?;
        let signature = decode_hex(signature, 64, "签名")?;
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(bytes, &signature)
            .map_err(|_| "C ABI Strategy Ed25519 签名校验失败".to_string())?;
    }
    Ok(())
}

fn decode_hex(value: &str, expected_bytes: usize, label: &str) -> Result<Vec<u8>, String> {
    if value.len() != expected_bytes * 2 || !value.chars().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("C ABI Strategy Ed25519 {label}十六进制长度非法"));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| format!("C ABI Strategy Ed25519 {label}不是合法十六进制"))
        })
        .collect()
}

impl Strategy for DynamicCAbiStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.inner.on_init(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        self.inner.on_event(context, event)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        self.inner.on_order_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.inner.on_stop()
    }
}

fn cstring(value: &str, field: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| format!("C ABI {field} 不能包含 NUL"))
}

fn read_required_string(pointer: *const c_char, field: &str) -> Result<String, String> {
    if pointer.is_null() {
        return Err(format!("C ABI {field} 不能为空"));
    }
    Ok(unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DESTROYED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn fake_create(_config: *const c_char) -> QxStrategyHandle {
        Box::into_raw(Box::new(7_u8)).cast()
    }

    unsafe extern "C" fn fake_init(
        _handle: QxStrategyHandle,
        _context: *const QxStrategyContext,
        _error: *mut c_char,
        _error_len: usize,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn fake_event(
        _handle: QxStrategyHandle,
        _context: *const QxStrategyContext,
        event: *const QxMarketEvent,
        decision: *mut QxStrategyDecision,
        _error: *mut c_char,
        _error_len: usize,
    ) -> i32 {
        let intents = Box::new([QxOrderIntent {
            intent_id: (*event).ts,
            instrument: c"BTCUSDT.BINANCE".as_ptr(),
            side: QxOrderSide::Buy,
            qty_raw: QxRaw128::from_i128(1_000_000_000),
            limit_price_raw: QxRaw128::from_i128(0),
            has_limit_price: 0,
            reduce_only: 0,
            post_only: 0,
            position_side: ptr::null(),
        }]);
        (*decision).schema_version = QX_C_STRATEGY_API_VERSION;
        (*decision).request_id = c"abi-request".as_ptr();
        (*decision).strategy_id = c"abi-test".as_ptr();
        (*decision).signal_id = (*event).ts;
        (*decision).confidence = QxRaw128::from_i128(1);
        (*decision).priority = 0;
        (*decision).expires_at = (*event).ts;
        (*decision).intents = Box::into_raw(intents).cast();
        (*decision).intents_len = 1;
        0
    }

    unsafe extern "C" fn fake_free(_handle: QxStrategyHandle, decision: *mut QxStrategyDecision) {
        if !(*decision).intents.is_null() {
            let pointer = std::ptr::slice_from_raw_parts_mut((*decision).intents, 1);
            drop(Box::from_raw(pointer));
            (*decision).intents = ptr::null_mut();
            (*decision).intents_len = 0;
        }
    }

    unsafe extern "C" fn fake_destroy(handle: QxStrategyHandle) {
        drop(Box::from_raw(handle.cast::<u8>()));
        DESTROYED.fetch_add(1, Ordering::SeqCst);
    }

    static FAKE_VTABLE: QxStrategyVTable = QxStrategyVTable {
        abi_version: QX_C_STRATEGY_API_VERSION,
        create: Some(fake_create),
        on_init: Some(fake_init),
        on_event: Some(fake_event),
        on_order_update: None,
        free_decision: Some(fake_free),
        destroy: Some(fake_destroy),
    };

    #[test]
    fn raw128_round_trips_signed_extremes() {
        for value in [0, 1, -1, i128::MAX, i128::MIN] {
            assert_eq!(QxRaw128::from_i128(value).into_i128(), value);
        }
    }

    #[test]
    fn c_layout_matches_expected_pointer_free_sizes() {
        assert_eq!(std::mem::size_of::<QxRaw128>(), 16);
        assert_eq!(std::mem::align_of::<QxRaw128>(), 8);
        assert_eq!(QX_C_STRATEGY_API_VERSION, crate::STRATEGY_API_VERSION);
    }

    #[test]
    fn verified_loader_rejects_invalid_digest_and_size() {
        let path = std::env::temp_dir().join(format!(
            "qianxing-strategy-digest-{}.bin",
            std::process::id()
        ));
        fs::write(&path, b"qianxing-plugin").unwrap();
        let bad = DynamicCAbiLoadPolicy::new("00".repeat(32));
        assert!(verify_library_file(&path, &bad)
            .unwrap_err()
            .contains("SHA-256 不匹配"));
        let digest = ring::digest::digest(&ring::digest::SHA256, b"qianxing-plugin");
        let expected = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let too_small = DynamicCAbiLoadPolicy::new(expected).with_max_library_bytes(1);
        assert!(verify_library_file(&path, &too_small)
            .unwrap_err()
            .contains("大小非法"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn verified_loader_accepts_and_rejects_detached_ed25519_signature() {
        let path = std::env::temp_dir().join(format!(
            "qianxing-strategy-signature-{}.bin",
            std::process::id()
        ));
        let bytes = b"qianxing-signed-plugin";
        fs::write(&path, bytes).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        let expected = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let seed = [9_u8; 32];
        let key_pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        let signature = key_pair.sign(bytes);
        let public_key = key_pair
            .public_key()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let signature_hex = signature
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let policy = DynamicCAbiLoadPolicy::new(expected.clone())
            .with_ed25519_signature(public_key, signature_hex.clone());
        verify_library_file(&path, &policy).unwrap();
        let bad = DynamicCAbiLoadPolicy::new(expected)
            .with_ed25519_signature("00".repeat(32), signature_hex);
        assert!(verify_library_file(&path, &bad).is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn c_abi_strategy_copies_plugin_decision_before_freeing_it() {
        let context = StrategyContext {
            strategy_id: "abi-test".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 10,
            positions: BTreeMap::new(),
            cash: BTreeMap::new(),
            available_margin_raw: Some(1_000_000_000),
            risk_state: "ready".into(),
        };
        let mut strategy = unsafe { CAbiStrategy::from_vtable(&FAKE_VTABLE, "{}") }.unwrap();
        strategy.on_init(&context).unwrap();
        let decision = strategy
            .on_event(
                &context,
                &MarketEvent::Timer {
                    name: "heartbeat".into(),
                    ts: 10,
                },
            )
            .unwrap();
        assert_eq!(decision.intents.len(), 1);
        assert_eq!(decision.intents[0].qty.raw(), 1_000_000_000);
        drop(strategy);
        assert_eq!(DESTROYED.load(Ordering::SeqCst), 1);
    }
}
