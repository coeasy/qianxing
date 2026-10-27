//! C ABI host binding for in-process Rust/C++ strategies.
//!
//! The layout intentionally mirrors `cpp/include/qianxing_strategy.h`. The
//! host owns the vtable (usually obtained from a shared library loader), while
//! this adapter owns only the opaque strategy handle and copies decisions out
//! of plugin-owned memory before calling `free_decision`.

use super::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyErrorV1,
    StrategyFillUpdateV2, StrategyHistoryRequirements, StrategyOrderIntent,
    StrategyOrderStatusUpdate, StrategyStopContextV1, StrategyStopReason,
};
use qx_core::{
    ExactDecimal, ExactOrderAmounts, InstrumentId, OrderPolicy, PositionSide, Price, Quantity, Side,
};
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::ptr;

use ring::signature::{UnparsedPublicKey, ED25519};

const DEFAULT_MAX_LIBRARY_BYTES: u64 = 256 * 1024 * 1024;

pub const QX_C_STRATEGY_API_VERSION: u32 = 1;
pub const QX_C_STRATEGY_ABI_VERSION: u32 = 2;
pub const QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2: u32 = 2;
const MAX_PLUGIN_INTENTS: usize = 100_000;
const MAX_PLUGIN_STRING_BYTES: usize = 1_048_576;
const ERROR_BUFFER_SIZE: usize = 512;
const MAX_DIAGNOSTIC_STATE_BYTES: usize = 64 * 1024;
const MAX_ORDER_POLICY_BYTES: usize = 4 * 1024;

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
#[derive(Clone, Copy, Debug)]
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

/// Versioned warmup-ready event for the optional C ABI lifecycle callback.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxStrategyWarmupCompleteV1 {
    pub struct_size: u32,
    pub schema_version: u32,
    pub instrument: *const c_char,
    pub instrument_len: usize,
    pub has_instrument: u8,
    pub observed_bars: u64,
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
#[derive(Clone, Copy, Debug)]
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

pub const QX_MARKET_EVENT_SCHEMA_VERSION_V2: u32 = 2;
pub const QX_MARKET_EVENT_SCHEMA_VERSION_V3: u32 = 3;
pub const QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION_V1: u32 = 1;

/// Order side across the C ABI, as a fixed-width integer rather than a
/// `#[repr(C)]` enum. The host must reject an invalid discriminant, but
/// reading a C enum whose stored value is not one of the declared
/// enumerators is undefined behavior in Rust — a hostile or buggy plugin that
/// writes `side = 3` would reach the match before any check could run. The
/// layout is unchanged (4 bytes), so existing plugins need no recompilation.
pub const QX_ORDER_SIDE_BUY: u32 = 1;
pub const QX_ORDER_SIDE_SELL: u32 = 2;

#[repr(C)]
pub struct QxOrderIntent {
    pub intent_id: u64,
    pub instrument: *const c_char,
    pub instrument_len: usize,
    pub side: u32,
    pub qty_raw: QxRaw128,
    pub limit_price_raw: QxRaw128,
    pub has_limit_price: u8,
    pub reduce_only: u8,
    pub post_only: u8,
    pub position_side: *const c_char,
    pub position_side_len: usize,
}

#[repr(C)]
pub struct QxStrategyDecision {
    pub schema_version: u32,
    pub request_id: *const c_char,
    pub request_id_len: usize,
    pub strategy_id: *const c_char,
    pub strategy_id_len: usize,
    pub signal_id: u64,
    pub confidence: QxRaw128,
    pub priority: i32,
    pub expires_at: u64,
    pub intents: *mut QxOrderIntent,
    pub intents_len: usize,
    /// Plugin-declared readable element capacity of `intents`.
    pub intents_capacity: usize,
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

/// Optional dynamic-library export `qx_strategy_diagnostic_state`.
/// It writes one bounded UTF-8 JSON value into host-owned memory and never transfers ownership.
pub type QxStrategyDiagnosticState =
    unsafe extern "C" fn(QxStrategyHandle, *mut c_char, usize, *mut usize) -> i32;
/// Optional dynamic-library export that writes strategy-owned checkpoint JSON into a bounded
/// host buffer. The JSON value must be an object and must fit the strategy checkpoint limit.
pub type QxStrategyCheckpointSnapshotV1 =
    unsafe extern "C" fn(QxStrategyHandle, *mut c_char, usize, *mut usize) -> i32;
/// Optional dynamic-library export that restores strategy-owned checkpoint JSON. The host
/// passes UTF-8 JSON bytes and a bounded error buffer; the published vtable stays unchanged.
pub type QxStrategyCheckpointRestoreV1 =
    unsafe extern "C" fn(QxStrategyHandle, *const c_char, usize, *mut c_char, usize) -> i32;

/// Optional dynamic-library lifecycle exports. These independent symbols keep
/// older `QxStrategyVTable` layouts binary compatible.
pub type QxStrategyStartCallbackV1 =
    unsafe extern "C" fn(QxStrategyHandle, *const QxStrategyContext, *mut c_char, usize) -> i32;
pub type QxStrategyStopCallbackV1 =
    unsafe extern "C" fn(QxStrategyHandle, *mut c_char, usize) -> i32;
/// Optional v2 stop callback receiving the versioned JSON stop-context envelope.
pub type QxStrategyStopCallbackV2 =
    unsafe extern "C" fn(QxStrategyHandle, *const c_char, *mut c_char, usize) -> i32;
/// Optional dynamic-library export for a bounded `StrategyErrorV1` JSON payload.
pub type QxStrategyErrorCallbackV1 =
    unsafe extern "C" fn(QxStrategyHandle, *const c_char, *mut c_char, usize) -> i32;
/// Optional callback that declares the closed-Bar warmup count.
pub type QxStrategyHistoryRequirementsCallbackV1 =
    unsafe extern "C" fn(QxStrategyHandle, *mut u64, *mut c_char, usize) -> i32;
/// Optional callback invoked when one instrument has completed its declared warmup.
pub type QxStrategyWarmupCompleteCallbackV1 = unsafe extern "C" fn(
    QxStrategyHandle,
    *const QxStrategyWarmupCompleteV1,
    *mut c_char,
    usize,
) -> i32;
/// Optional dynamic-library callback for committed order-state facts. This is
/// a separate symbol so the legacy fill-only vtable callback keeps its meaning.
pub type QxStrategyJsonUpdateCallbackV1 = unsafe extern "C" fn(
    QxStrategyHandle,
    *const c_char,
    *mut QxStrategyDecision,
    *mut c_char,
    usize,
) -> i32;
/// Optional dynamic-library callback for a committed, sequence-bound fill.
/// The JSON payload is `StrategyFillUpdateV2`; the legacy vtable remains unchanged.
pub type QxStrategyFillUpdateCallbackV2 = QxStrategyJsonUpdateCallbackV1;
/// Optional dynamic-library callback for committed order-state facts.
pub type QxStrategyOrderStatusCallbackV1 = QxStrategyJsonUpdateCallbackV1;
/// Optional dynamic-library callback for committed account balance/position snapshots.
/// The JSON payload is `StrategyAccountUpdateV1` and keeps the stable vtable unchanged.
pub type QxStrategyAccountUpdateCallbackV1 = QxStrategyJsonUpdateCallbackV1;
/// Optional typed order-status callback. It is resolved independently from
/// v1 and does not change the published strategy vtable layout.
pub type QxStrategyOrderStatusCallbackV2 = unsafe extern "C" fn(
    QxStrategyHandle,
    *const QxStrategyOrderStatusUpdateV2,
    *mut QxStrategyDecision,
    *mut c_char,
    usize,
) -> i32;

pub const QX_ORDER_STATUS_EVENT_ORDER_STATUS: u32 = 1;
pub const QX_ORDER_STATUS_PENDING_SUBMIT: u32 = 1;
pub const QX_ORDER_STATUS_SUBMITTED: u32 = 2;
pub const QX_ORDER_STATUS_ACCEPTED: u32 = 3;
pub const QX_ORDER_STATUS_WORKING: u32 = 4;
pub const QX_ORDER_STATUS_PARTIALLY_FILLED: u32 = 5;
pub const QX_ORDER_STATUS_FILLED: u32 = 6;
pub const QX_ORDER_STATUS_CANCEL_PENDING: u32 = 7;
pub const QX_ORDER_STATUS_CANCELLED: u32 = 8;
pub const QX_ORDER_STATUS_REJECTED: u32 = 9;
pub const QX_ORDER_STATUS_EXPIRED: u32 = 10;
pub const QX_ORDER_STATUS_UNKNOWN: u32 = 11;

/// Exact decimal represented as signed coefficient / 10^scale.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QxStrategyDecimalV1 {
    pub coefficient: QxRaw128,
    pub scale: u32,
}

/// Exact account/position value paired with a legacy context key. Text and
/// numeric pointers are borrowed for the duration of the context callback.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QxStrategyExactKvV1 {
    pub key: *const c_char,
    pub key_len: usize,
    pub value: QxStrategyDecimalV1,
}

/// Versioned exact StrategyContext envelope. The embedded v1 context remains
/// the compatibility projection; sidecars preserve values below 1e-9.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxStrategyContextV2 {
    pub struct_size: u32,
    pub schema_version: u32,
    pub context_v1: QxStrategyContext,
    pub positions_exact: *const QxStrategyExactKvV1,
    pub positions_exact_len: usize,
    pub cash_exact: *const QxStrategyExactKvV1,
    pub cash_exact_len: usize,
    pub available_margin_exact: QxStrategyDecimalV1,
    pub has_available_margin_exact: u8,
}

/// Optional export `qx_strategy_on_context_v2`, called before each callback
/// that receives a v1 StrategyContext so the plugin can refresh exact state.
pub type QxStrategyContextCallbackV2 =
    unsafe extern "C" fn(QxStrategyHandle, *const QxStrategyContextV2, *mut c_char, usize) -> i32;

/// Exact source OHLCV values for a C ABI market-event v2 callback.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QxStrategyExactOhlcvV1 {
    pub open: QxStrategyDecimalV1,
    pub high: QxStrategyDecimalV1,
    pub low: QxStrategyDecimalV1,
    pub close: QxStrategyDecimalV1,
    pub volume: QxStrategyDecimalV1,
}

/// Versioned event envelope. The embedded `raw_event` preserves the v1 fields;
/// exact OHLCV is available only when `has_exact_ohlcv` is non-zero.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxMarketEventV2 {
    pub struct_size: u32,
    pub schema_version: u32,
    pub raw_event: QxMarketEvent,
    pub has_exact_ohlcv: u8,
    pub exact_ohlcv: QxStrategyExactOhlcvV1,
}

/// Exact source price and quantity for a single order-book level. This array
/// is parallel to the raw `QxBookLevel` array in the embedded v1 event.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxStrategyExactBookLevelV1 {
    pub has_exact: u8,
    pub price: QxStrategyDecimalV1,
    pub qty: QxStrategyDecimalV1,
}

/// Versioned market event with exact order-book sidecars. The sidecar arrays
/// have the same lengths and ordering as `event_v2.raw_event` bids and asks.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxMarketEventV3 {
    pub struct_size: u32,
    pub schema_version: u32,
    pub event_v2: QxMarketEventV2,
    pub exact_bids: *const QxStrategyExactBookLevelV1,
    pub exact_bids_len: usize,
    pub exact_asks: *const QxStrategyExactBookLevelV1,
    pub exact_asks_len: usize,
}

/// Optional symbol `qx_strategy_on_market_event_v3`, which supersedes the
/// v2 callback when present and carries exact OHLCV plus order-book levels.
pub type QxStrategyMarketEventCallbackV3 = unsafe extern "C" fn(
    QxStrategyHandle,
    *const QxStrategyContext,
    *const QxMarketEventV3,
    *mut QxStrategyDecision,
    *mut c_char,
    usize,
) -> i32;

/// Optional symbol `qx_strategy_on_market_event_v2`. It leaves the stable
/// strategy vtable untouched and lets the host fail closed for exact Bars
/// when a plugin only implements the raw v1 callback.
pub type QxStrategyMarketEventCallbackV2 = unsafe extern "C" fn(
    QxStrategyHandle,
    *const QxStrategyContext,
    *const QxMarketEventV2,
    *mut QxStrategyDecision,
    *mut c_char,
    usize,
) -> i32;

/// Optional exact amount fields for strategy order-status v2.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QxStrategyExactOrderAmountsV1 {
    pub quantity: QxStrategyDecimalV1,
    pub limit_price: QxStrategyDecimalV1,
    pub has_limit_price: u8,
    pub filled: QxStrategyDecimalV1,
    pub quote_budget: QxStrategyDecimalV1,
    pub has_quote_budget: u8,
    pub quote_filled: QxStrategyDecimalV1,
    pub market_reference_price: QxStrategyDecimalV1,
    pub has_market_reference_price: u8,
}

/// Typed order facts attached to a committed status transition. Text pointers
/// are host-owned and valid only for the duration of the callback.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxStrategyOrderUpdateV2 {
    pub order_id: u64,
    pub account_id: *const c_char,
    pub account_id_len: usize,
    pub instrument: *const c_char,
    pub instrument_len: usize,
    pub side: u32,
    pub qty_raw: QxRaw128,
    pub filled_raw: QxRaw128,
    pub strategy_id: *const c_char,
    pub strategy_id_len: usize,
    pub exact_amounts: *const QxStrategyExactOrderAmountsV1,
}

/// Typed, versioned order-status update. Integer event/status codes are stable
/// constants so invalid C enum discriminants cannot trigger Rust UB.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QxStrategyOrderStatusUpdateV2 {
    pub schema_version: u32,
    pub event: u32,
    pub event_seq: u64,
    pub ts: u64,
    pub status: u32,
    pub order: QxStrategyOrderUpdateV2,
    pub reason: *const c_char,
    pub reason_len: usize,
    pub has_reason: u8,
}
/// Optional dynamic-library export for per-intent derivative execution policy.
/// The callback writes a bounded UTF-8 JSON object into host-owned memory.
pub type QxStrategyOrderPolicyCallbackV1 = unsafe extern "C" fn(
    QxStrategyHandle,
    u64,
    *mut c_char,
    usize,
    *mut usize,
    *mut c_char,
    usize,
) -> i32;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QxStrategyOrderPolicyPayloadV1 {
    schema_version: u32,
    #[serde(default)]
    margin_mode: Option<String>,
    #[serde(default)]
    position_mode: Option<String>,
    #[serde(default)]
    leverage: Option<u32>,
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
    exact_position_keys: Vec<CString>,
    exact_cash_keys: Vec<CString>,
    positions: Vec<QxStrategyKv>,
    cash: Vec<QxStrategyKv>,
    context: QxStrategyContext,
    positions_exact: Vec<QxStrategyExactKvV1>,
    cash_exact: Vec<QxStrategyExactKvV1>,
    context_v2: QxStrategyContextV2,
    has_lossy_exact_projection: bool,
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
        let exact_position_keys = context
            .positions_exact
            .keys()
            .map(|key| cstring(key, "exact position instrument"))
            .collect::<Result<Vec<_>, _>>()?;
        let exact_cash_keys = context
            .cash_exact
            .keys()
            .map(|key| cstring(key, "exact cash currency"))
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
        let positions_exact = exact_position_keys
            .iter()
            .zip(context.positions_exact.values())
            .map(|(key, value)| QxStrategyExactKvV1 {
                key: key.as_ptr(),
                key_len: key.as_bytes().len(),
                value: strategy_decimal(*value),
            })
            .collect::<Vec<_>>();
        let cash_exact = exact_cash_keys
            .iter()
            .zip(context.cash_exact.values())
            .map(|(key, value)| QxStrategyExactKvV1 {
                key: key.as_ptr(),
                key_len: key.as_bytes().len(),
                value: strategy_decimal(*value),
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
        let zero = strategy_decimal(ExactDecimal::ZERO);
        let context_v2 = QxStrategyContextV2 {
            struct_size: std::mem::size_of::<QxStrategyContextV2>() as u32,
            schema_version: QX_STRATEGY_CONTEXT_SCHEMA_VERSION_V2,
            context_v1: context_wire,
            positions_exact: positions_exact.as_ptr(),
            positions_exact_len: positions_exact.len(),
            cash_exact: cash_exact.as_ptr(),
            cash_exact_len: cash_exact.len(),
            available_margin_exact: context
                .available_margin_exact
                .map_or(zero, strategy_decimal),
            has_available_margin_exact: u8::from(context.available_margin_exact.is_some()),
        };
        let has_lossy_exact_projection =
            context.positions_exact.iter().any(|(instrument, exact)| {
                exact.to_raw(9).ok() != context.positions.get(instrument).copied()
            }) || context.cash_exact.iter().any(|(currency, exact)| {
                exact.to_raw(9).ok() != context.cash.get(currency).copied()
            }) || context
                .available_margin_exact
                .is_some_and(|exact| exact.to_raw(9).ok() != context.available_margin_raw);
        Ok(Self {
            strategy_id,
            strategy_version,
            account_id,
            venue_id,
            data_fingerprint,
            risk_state,
            position_keys,
            cash_keys,
            exact_position_keys,
            exact_cash_keys,
            positions,
            cash,
            context: context_wire,
            positions_exact,
            cash_exact,
            context_v2,
            has_lossy_exact_projection,
        })
    }
}

struct OrderStatusBuffers {
    account_id: CString,
    instrument: CString,
    strategy_id: CString,
    reason: Option<CString>,
}

impl OrderStatusBuffers {
    fn new(update: &StrategyOrderStatusUpdate) -> Result<Self, String> {
        Ok(Self {
            account_id: cstring(&update.order.account_id, "order status account_id")?,
            instrument: cstring(&update.order.instrument, "order status instrument")?,
            strategy_id: cstring(&update.order.strategy_id, "order status strategy_id")?,
            reason: update
                .reason
                .as_deref()
                .map(|value| cstring(value, "order status reason"))
                .transpose()?,
        })
    }

    fn wire(
        &self,
        update: &StrategyOrderStatusUpdate,
        exact_amounts: Option<&QxStrategyExactOrderAmountsV1>,
    ) -> QxStrategyOrderStatusUpdateV2 {
        let side = match update.order.side {
            Side::Buy => QX_ORDER_SIDE_BUY,
            Side::Sell => QX_ORDER_SIDE_SELL,
        };
        QxStrategyOrderStatusUpdateV2 {
            schema_version: update.schema_version,
            event: QX_ORDER_STATUS_EVENT_ORDER_STATUS,
            event_seq: update.event_seq,
            ts: update.ts,
            status: order_status_code(update.status),
            order: QxStrategyOrderUpdateV2 {
                order_id: update.order.order_id,
                account_id: self.account_id.as_ptr(),
                account_id_len: self.account_id.as_bytes().len(),
                instrument: self.instrument.as_ptr(),
                instrument_len: self.instrument.as_bytes().len(),
                side,
                qty_raw: QxRaw128::from_i128(update.order.qty_raw),
                filled_raw: QxRaw128::from_i128(update.order.filled_raw),
                strategy_id: self.strategy_id.as_ptr(),
                strategy_id_len: self.strategy_id.as_bytes().len(),
                exact_amounts: exact_amounts.map_or(ptr::null(), std::ptr::from_ref),
            },
            reason: self
                .reason
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            reason_len: self
                .reason
                .as_ref()
                .map_or(0, |value| value.as_bytes().len()),
            has_reason: u8::from(self.reason.is_some()),
        }
    }
}

fn strategy_decimal(value: ExactDecimal) -> QxStrategyDecimalV1 {
    QxStrategyDecimalV1 {
        coefficient: QxRaw128::from_i128(value.coefficient()),
        scale: u32::from(value.scale()),
    }
}

fn exact_amounts_wire(amounts: ExactOrderAmounts) -> QxStrategyExactOrderAmountsV1 {
    let zero = strategy_decimal(ExactDecimal::ZERO);
    QxStrategyExactOrderAmountsV1 {
        quantity: strategy_decimal(amounts.quantity),
        limit_price: amounts.limit_price.map_or(zero, strategy_decimal),
        has_limit_price: u8::from(amounts.limit_price.is_some()),
        filled: strategy_decimal(amounts.filled),
        quote_budget: amounts.quote_budget.map_or(zero, strategy_decimal),
        has_quote_budget: u8::from(amounts.quote_budget.is_some()),
        quote_filled: strategy_decimal(amounts.quote_filled),
        market_reference_price: amounts
            .market_reference_price
            .map_or(zero, strategy_decimal),
        has_market_reference_price: u8::from(amounts.market_reference_price.is_some()),
    }
}

fn order_status_code(status: super::StrategyOrderStatus) -> u32 {
    use super::StrategyOrderStatus::*;
    match status {
        PendingSubmit => QX_ORDER_STATUS_PENDING_SUBMIT,
        Submitted => QX_ORDER_STATUS_SUBMITTED,
        Accepted => QX_ORDER_STATUS_ACCEPTED,
        Working => QX_ORDER_STATUS_WORKING,
        PartiallyFilled => QX_ORDER_STATUS_PARTIALLY_FILLED,
        Filled => QX_ORDER_STATUS_FILLED,
        CancelPending => QX_ORDER_STATUS_CANCEL_PENDING,
        Cancelled => QX_ORDER_STATUS_CANCELLED,
        Rejected => QX_ORDER_STATUS_REJECTED,
        Expired => QX_ORDER_STATUS_EXPIRED,
        Unknown => QX_ORDER_STATUS_UNKNOWN,
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
    exact_bids: Vec<QxStrategyExactBookLevelV1>,
    exact_asks: Vec<QxStrategyExactBookLevelV1>,
    event: QxMarketEvent,
    event_v2: QxMarketEventV2,
    event_v3: QxMarketEventV3,
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
        let zero_decimal = strategy_decimal(ExactDecimal::ZERO);
        let (has_exact_ohlcv, exact_ohlcv) = match event {
            MarketEvent::Bar {
                exact_ohlcv: Some(exact),
                ..
            } => (
                1,
                QxStrategyExactOhlcvV1 {
                    open: strategy_decimal(exact.open),
                    high: strategy_decimal(exact.high),
                    low: strategy_decimal(exact.low),
                    close: strategy_decimal(exact.close),
                    volume: strategy_decimal(exact.volume),
                },
            ),
            _ => (
                0,
                QxStrategyExactOhlcvV1 {
                    open: zero_decimal,
                    high: zero_decimal,
                    low: zero_decimal,
                    close: zero_decimal,
                    volume: zero_decimal,
                },
            ),
        };
        let event_v2 = QxMarketEventV2 {
            struct_size: std::mem::size_of::<QxMarketEventV2>() as u32,
            schema_version: QX_MARKET_EVENT_SCHEMA_VERSION_V2,
            raw_event: event_wire,
            has_exact_ohlcv,
            exact_ohlcv,
        };
        let zero_decimal = strategy_decimal(ExactDecimal::ZERO);
        let (exact_bids, exact_asks) = match event {
            MarketEvent::OrderBook { bids, asks, .. } => {
                let encode = |level: &super::OrderBookLevel| QxStrategyExactBookLevelV1 {
                    has_exact: u8::from(level.price_exact.is_some()),
                    price: level.price_exact.map_or(zero_decimal, strategy_decimal),
                    qty: level.qty_exact.map_or(zero_decimal, strategy_decimal),
                };
                (
                    bids.iter().map(encode).collect(),
                    asks.iter().map(encode).collect(),
                )
            }
            _ => (Vec::new(), Vec::new()),
        };
        let event_v3 = QxMarketEventV3 {
            struct_size: std::mem::size_of::<QxMarketEventV3>() as u32,
            schema_version: QX_MARKET_EVENT_SCHEMA_VERSION_V3,
            event_v2,
            exact_bids: exact_bids.as_ptr(),
            exact_bids_len: exact_bids.len(),
            exact_asks: exact_asks.as_ptr(),
            exact_asks_len: exact_asks.len(),
        };
        Ok(Self {
            instrument,
            timer_name,
            bids,
            asks,
            exact_bids,
            exact_asks,
            event: event_wire,
            event_v2,
            event_v3,
        })
    }
}

/// An in-process C ABI strategy. The shared library loader is intentionally
/// outside this type: applications can choose OS-specific loading, signing,
/// and sandbox policy before passing a validated vtable here.
pub struct CAbiStrategy {
    vtable: &'static QxStrategyVTable,
    handle: QxStrategyHandle,
    diagnostic_state: Option<QxStrategyDiagnosticState>,
    checkpoint_snapshot: Option<QxStrategyCheckpointSnapshotV1>,
    checkpoint_restore: Option<QxStrategyCheckpointRestoreV1>,
    on_start: Option<QxStrategyStartCallbackV1>,
    on_stop: Option<QxStrategyStopCallbackV1>,
    on_stop_v2: Option<QxStrategyStopCallbackV2>,
    on_error: Option<QxStrategyErrorCallbackV1>,
    history_requirements: Option<QxStrategyHistoryRequirementsCallbackV1>,
    on_warmup_complete: Option<QxStrategyWarmupCompleteCallbackV1>,
    on_order_status: Option<QxStrategyOrderStatusCallbackV1>,
    on_account_update: Option<QxStrategyAccountUpdateCallbackV1>,
    on_order_status_v2: Option<QxStrategyOrderStatusCallbackV2>,
    on_fill_update_v2: Option<QxStrategyFillUpdateCallbackV2>,
    on_market_event_v2: Option<QxStrategyMarketEventCallbackV2>,
    on_market_event_v3: Option<QxStrategyMarketEventCallbackV3>,
    on_context_v2: Option<QxStrategyContextCallbackV2>,
    order_policy: Option<QxStrategyOrderPolicyCallbackV1>,
}

fn required_callback<T: Copy>(callback: Option<T>, name: &str) -> Result<T, String> {
    callback.ok_or_else(|| format!("C ABI Strategy {name} 回调缺失"))
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
        if vtable.abi_version != QX_C_STRATEGY_ABI_VERSION
            || vtable.create.is_none()
            || vtable.on_init.is_none()
            || vtable.on_event.is_none()
            || vtable.free_decision.is_none()
            || vtable.destroy.is_none()
        {
            return Err("C ABI Strategy vtable 版本或回调不完整".into());
        }
        let config = cstring(config_json, "config_json")?;
        let handle = (required_callback(vtable.create, "create")?)(config.as_ptr());
        if handle.is_null() {
            return Err("C ABI Strategy create 返回空句柄".into());
        }
        Ok(Self {
            vtable,
            handle,
            diagnostic_state: None,
            checkpoint_snapshot: None,
            checkpoint_restore: None,
            on_start: None,
            on_stop: None,
            on_stop_v2: None,
            on_error: None,
            history_requirements: None,
            on_warmup_complete: None,
            on_order_status: None,
            on_account_update: None,
            on_order_status_v2: None,
            on_fill_update_v2: None,
            on_market_event_v2: None,
            on_market_event_v3: None,
            on_context_v2: None,
            order_policy: None,
        })
    }

    /// Attach the optional history declaration to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the bounded error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_history_requirements_callback(
        mut self,
        callback: QxStrategyHistoryRequirementsCallbackV1,
    ) -> Self {
        self.history_requirements = Some(callback);
        self
    }

    /// Attach the optional warmup-complete callback to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the borrowed event and bounded error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_warmup_complete_callback(
        mut self,
        callback: QxStrategyWarmupCompleteCallbackV1,
    ) -> Self {
        self.on_warmup_complete = Some(callback);
        self
    }

    /// Attach the optional StrategyErrorV1 callback to a statically supplied plugin.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the bounded error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_error_callback(mut self, callback: QxStrategyErrorCallbackV1) -> Self {
        self.on_error = Some(callback);
        self
    }

    /// Whether this plugin can consume exact source OHLCV market events.
    pub fn supports_exact_market_data(&self) -> bool {
        self.on_market_event_v2.is_some() || self.on_market_event_v3.is_some()
    }

    /// Whether the plugin can consume exact source order-book levels.
    pub fn supports_exact_orderbook(&self) -> bool {
        self.on_market_event_v3.is_some()
    }

    /// Whether the plugin can consume exact position, cash, and margin context.
    pub fn supports_exact_context(&self) -> bool {
        self.on_context_v2.is_some()
    }

    /// Whether both durable strategy-state checkpoint exports are available.
    pub fn supports_state_checkpoint(&self) -> bool {
        self.checkpoint_snapshot.is_some() && self.checkpoint_restore.is_some()
    }

    /// Whether exactly one of the paired checkpoint exports is present.
    pub fn has_partial_state_checkpoint_exports(&self) -> bool {
        self.checkpoint_snapshot.is_some() != self.checkpoint_restore.is_some()
    }

    /// Attach the optional state snapshot export to a statically supplied plugin.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey the bounded
    /// JSON buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_checkpoint_snapshot(
        mut self,
        callback: QxStrategyCheckpointSnapshotV1,
    ) -> Self {
        self.checkpoint_snapshot = Some(callback);
        self
    }

    /// Attach the optional state restore export to a statically supplied plugin.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey the bounded
    /// JSON/error buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_checkpoint_restore(
        mut self,
        callback: QxStrategyCheckpointRestoreV1,
    ) -> Self {
        self.checkpoint_restore = Some(callback);
        self
    }

    /// Restore strategy-owned JSON state. The host validates the checkpoint envelope first.
    pub fn restore_strategy_checkpoint(&mut self, state: &serde_json::Value) -> Result<(), String> {
        let callback = self
            .checkpoint_restore
            .ok_or_else(|| "C ABI Strategy 缺少 checkpoint restore 导出".to_string())?;
        if !self.supports_state_checkpoint() {
            return Err("C ABI Strategy checkpoint snapshot/restore 导出必须成对提供".into());
        }
        if !state.is_object() {
            return Err("C ABI Strategy checkpoint state 必须是 JSON object".into());
        }
        let payload = serde_json::to_vec(state)
            .map_err(|error| format!("C ABI Strategy checkpoint JSON 编码失败: {error}"))?;
        if payload.is_empty() || payload.len() > MAX_DIAGNOSTIC_STATE_BYTES {
            return Err("C ABI Strategy checkpoint JSON 长度超出限制".into());
        }
        let payload_text = std::str::from_utf8(&payload)
            .map_err(|error| format!("C ABI Strategy checkpoint 不是 UTF-8: {error}"))?;
        let payload = cstring(payload_text, "strategy_checkpoint_json")?;
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                payload.as_ptr(),
                payload.as_bytes().len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error(
                "C ABI Strategy checkpoint restore",
                code,
                &error,
            ))
        }
    }

    /// Snapshot strategy-owned JSON state using the bounded host-owned buffer.
    pub fn snapshot_strategy_checkpoint(&self) -> Result<serde_json::Value, String> {
        let callback = self
            .checkpoint_snapshot
            .ok_or_else(|| "C ABI Strategy 缺少 checkpoint snapshot 导出".to_string())?;
        if !self.supports_state_checkpoint() {
            return Err("C ABI Strategy checkpoint snapshot/restore 导出必须成对提供".into());
        }
        let mut output = vec![0 as c_char; MAX_DIAGNOSTIC_STATE_BYTES];
        let mut output_len = 0_usize;
        let code = unsafe {
            callback(
                self.handle,
                output.as_mut_ptr(),
                output.len(),
                &mut output_len,
            )
        };
        if code != 0 || output_len == 0 || output_len > output.len() {
            return Err(format!(
                "C ABI Strategy checkpoint snapshot 失败或长度非法: code={code} len={output_len}"
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<u8>(), output_len) };
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("C ABI Strategy checkpoint JSON 无效: {error}"))?;
        if !value.is_object() {
            return Err("C ABI Strategy checkpoint snapshot 必须返回 JSON object".into());
        }
        Ok(value)
    }

    /// Attach the optional exact StrategyContext callback to a static plugin.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the borrowed context and bounded error-buffer contract in the C header.
    pub unsafe fn with_context_callback_v2(
        mut self,
        callback: QxStrategyContextCallbackV2,
    ) -> Self {
        self.on_context_v2 = Some(callback);
        self
    }

    /// Attach the optional v2 market-event callback to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the bounded decision/error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_market_event_callback_v2(
        mut self,
        callback: QxStrategyMarketEventCallbackV2,
    ) -> Self {
        self.on_market_event_v2 = Some(callback);
        self
    }

    /// Attach the optional v3 market-event callback to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the bounded decision/error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_market_event_callback_v3(
        mut self,
        callback: QxStrategyMarketEventCallbackV3,
    ) -> Self {
        self.on_market_event_v3 = Some(callback);
        self
    }

    /// Whether the optional order-status callback export was resolved for this plugin.
    pub fn supports_order_status_updates(&self) -> bool {
        self.on_order_status.is_some() || self.on_order_status_v2.is_some()
    }

    /// Whether the optional account/position snapshot callback was resolved.
    pub fn supports_account_updates(&self) -> bool {
        self.on_account_update.is_some()
    }

    /// Attach the optional account/position callback to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey the
    /// bounded decision/error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_account_update_callback(
        mut self,
        callback: QxStrategyAccountUpdateCallbackV1,
    ) -> Self {
        self.on_account_update = Some(callback);
        self
    }

    /// Whether this plugin can consume the sequence-bound fill update, either
    /// through its V2 export or the legacy fill-only vtable callback.
    pub fn supports_fill_updates(&self) -> bool {
        self.on_fill_update_v2.is_some() || self.vtable.on_order_update.is_some()
    }

    /// Attach the optional fill V2 callback to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for this strategy's lifetime and obey
    /// the bounded decision/error-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_fill_update_callback(
        mut self,
        callback: QxStrategyFillUpdateCallbackV2,
    ) -> Self {
        self.on_fill_update_v2 = Some(callback);
        self
    }

    /// Whether this strategy can override per-intent derivative policy fields.
    pub fn supports_order_policy_overrides(&self) -> bool {
        self.order_policy.is_some()
    }

    /// Attach the optional versioned policy callback to a statically supplied vtable.
    ///
    /// # Safety
    ///
    /// `callback` must remain executable for the full lifetime of this strategy and must obey
    /// the bounded output-buffer contract in `qianxing_strategy.h`.
    pub unsafe fn with_order_policy_callback(
        mut self,
        callback: QxStrategyOrderPolicyCallbackV1,
    ) -> Self {
        self.order_policy = Some(callback);
        self
    }

    fn call_error(label: &str, code: i32, buffer: &[c_char; ERROR_BUFFER_SIZE]) -> String {
        let bytes = buffer
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect::<Vec<_>>();
        let detail = String::from_utf8_lossy(&bytes).trim().to_string();
        if detail.is_empty() {
            format!("{label} 失败 code={code}")
        } else {
            format!("{label} 失败 code={code}: {detail}")
        }
    }

    fn prepare_context(&mut self, context: &StrategyContext) -> Result<ContextBuffers, String> {
        context.validate()?;
        let buffers = ContextBuffers::new(context)?;
        if let Some(callback) = self.on_context_v2 {
            let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
            let code = unsafe {
                callback(
                    self.handle,
                    &buffers.context_v2,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            if code != 0 {
                return Err(Self::call_error(
                    "C ABI Strategy on_context_v2",
                    code,
                    &error,
                ));
            }
        } else if buffers.has_lossy_exact_projection {
            return Err(
                "C ABI Strategy 收到无法由 1e-9 raw 表示的精确账户/持仓上下文，但插件未实现 qx_strategy_on_context_v2；拒绝有损回退"
                    .into(),
            );
        }
        Ok(buffers)
    }

    fn call_stop_v1(&mut self) -> Result<(), String> {
        let Some(callback) = self.on_stop else {
            return Ok(());
        };
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe { callback(self.handle, error.as_mut_ptr(), error.len()) };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error("C ABI Strategy on_stop", code, &error))
        }
    }

    fn decode_decision(
        &self,
        decision: &QxStrategyDecision,
        context: &StrategyContext,
        event_ts: u64,
    ) -> Result<StrategyDecision, String> {
        if decision.intents_capacity > MAX_PLUGIN_INTENTS
            || decision.intents_len > decision.intents_capacity
            || (decision.intents_len > 0 && decision.intents.is_null())
        {
            return Err(format!(
                "C ABI Strategy 返回的 intents 指针或长度/容量非法: len={} capacity={} max={MAX_PLUGIN_INTENTS}",
                decision.intents_len, decision.intents_capacity
            ));
        }
        let request_id =
            read_required_string(decision.request_id, decision.request_id_len, "request_id")?;
        let strategy_id = read_required_string(
            decision.strategy_id,
            decision.strategy_id_len,
            "strategy_id",
        )?;
        let raw_intents: &[QxOrderIntent] = if decision.intents_len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(decision.intents, decision.intents_len) }
        };
        let mut intents = Vec::with_capacity(raw_intents.len());
        for raw in raw_intents {
            let instrument_text =
                read_required_string(raw.instrument, raw.instrument_len, "intent instrument")?;
            let instrument = InstrumentId::parse(&instrument_text)
                .ok_or_else(|| format!("C ABI intent instrument 非法: {instrument_text}"))?;
            let side = match raw.side {
                QX_ORDER_SIDE_BUY => Side::Buy,
                QX_ORDER_SIDE_SELL => Side::Sell,
                other => return Err(format!("C ABI intent side 非法: {other}")),
            };
            let position_side = if raw.position_side.is_null() && raw.position_side_len == 0 {
                None
            } else {
                let position_side = read_required_string(
                    raw.position_side,
                    raw.position_side_len,
                    "position_side",
                )?;
                Some(match position_side.to_ascii_lowercase().as_str() {
                    "long" => PositionSide::Long,
                    "short" => PositionSide::Short,
                    "net" => PositionSide::Net,
                    other => return Err(format!("C ABI position_side 非法: {other}")),
                })
            };
            let extension = self.order_policy_for_intent(raw.intent_id)?;
            let policy = self.merge_order_policy(position_side, extension)?;
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

                qty_exact: None,
                limit_price_exact: None,
                quote_budget: None,
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

    fn order_policy_for_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<QxStrategyOrderPolicyPayloadV1>, String> {
        let Some(callback) = self.order_policy else {
            return Ok(None);
        };
        let mut output = vec![0 as c_char; MAX_ORDER_POLICY_BYTES];
        let mut output_len = 0_usize;
        let mut error_buffer = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                intent_id,
                output.as_mut_ptr(),
                output.len(),
                &mut output_len,
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };
        if code != 0 {
            return Err(Self::call_error(
                "C ABI order policy callback",
                code,
                &error_buffer,
            ));
        }
        if output_len == 0 || output_len > output.len() {
            return Err(format!(
                "C ABI order policy JSON 长度非法: len={output_len} max={}",
                output.len()
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<u8>(), output_len) };
        let text = std::str::from_utf8(bytes)
            .map_err(|error| format!("C ABI order policy JSON 不是合法 UTF-8: {error}"))?;
        if text.contains('\0') {
            return Err("C ABI order policy JSON 含有 NUL".into());
        }
        let payload: QxStrategyOrderPolicyPayloadV1 = serde_json::from_str(text)
            .map_err(|error| format!("C ABI order policy JSON 非法: {error}"))?;
        if payload.schema_version != 1 {
            return Err(format!(
                "C ABI order policy schema_version 不受支持: {}",
                payload.schema_version
            ));
        }
        if payload.leverage == Some(0) {
            return Err("C ABI order policy leverage 必须大于0".into());
        }
        let supplied_fields = usize::from(payload.margin_mode.is_some())
            + usize::from(payload.position_mode.is_some())
            + usize::from(payload.leverage.is_some());
        if supplied_fields != 0 && supplied_fields != 3 {
            return Err(
                "C ABI order policy 的 margin_mode、position_mode、leverage 必须同时提供".into(),
            );
        }
        Ok(Some(payload))
    }

    fn merge_order_policy(
        &self,
        position_side: Option<PositionSide>,
        extension: Option<QxStrategyOrderPolicyPayloadV1>,
    ) -> Result<Option<OrderPolicy>, String> {
        let has_extension_policy = extension.as_ref().is_some_and(|policy| {
            policy.margin_mode.is_some()
                || policy.position_mode.is_some()
                || policy.leverage.is_some()
        });
        if position_side.is_none() && !has_extension_policy {
            return Ok(None);
        }
        let mut policy = OrderPolicy::default();
        if let Some(position_side) = position_side {
            policy.position_side = position_side;
        }
        if let Some(extension) = extension {
            if let Some(margin_mode) = extension.margin_mode {
                policy.margin_mode = match margin_mode.as_str() {
                    "cash" => qx_core::MarginMode::Cash,
                    "cross" => qx_core::MarginMode::Cross,
                    "isolated" => qx_core::MarginMode::Isolated,
                    other => return Err(format!("C ABI margin_mode 非法: {other}")),
                };
            }
            if let Some(position_mode) = extension.position_mode {
                policy.position_mode = match position_mode.as_str() {
                    "one_way" => qx_core::PositionMode::OneWay,
                    "hedge" => qx_core::PositionMode::Hedge,
                    other => return Err(format!("C ABI position_mode 非法: {other}")),
                };
            }
            if let Some(leverage) = extension.leverage {
                policy.leverage = leverage;
            }
        }
        Ok(Some(policy))
    }
}

impl Strategy for CAbiStrategy {
    fn on_error(&mut self, error: &StrategyErrorV1) -> Result<(), String> {
        error.validate()?;
        let Some(callback) = self.on_error else {
            return Ok(());
        };
        let payload = serde_json::to_string(error)
            .map_err(|error| format!("C ABI StrategyErrorV1 JSON 编码失败: {error}"))?;
        if payload.len() > MAX_PLUGIN_STRING_BYTES {
            return Err("C ABI StrategyErrorV1 超过大小限制".into());
        }
        let payload = cstring(&payload, "strategy_error_json")?;
        let mut callback_error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                payload.as_ptr(),
                callback_error.as_mut_ptr(),
                callback_error.len(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error(
                "C ABI Strategy on_error_v1",
                code,
                &callback_error,
            ))
        }
    }

    fn diagnostic_state(&self) -> Option<serde_json::Value> {
        let callback = self.diagnostic_state?;
        let mut output = vec![0 as c_char; MAX_DIAGNOSTIC_STATE_BYTES];
        let mut output_len = 0_usize;
        let code = unsafe {
            callback(
                self.handle,
                output.as_mut_ptr(),
                output.len(),
                &mut output_len,
            )
        };
        if code != 0 || output_len == 0 || output_len > output.len() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<u8>(), output_len) };
        let value = std::str::from_utf8(bytes).ok()?;
        serde_json::from_str(value).ok()
    }

    fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
        let Some(callback) = self.history_requirements else {
            return Ok(StrategyHistoryRequirements::default());
        };
        let mut warmup_bars = 0_u64;
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                &mut warmup_bars,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code != 0 {
            return Err(Self::call_error(
                "C ABI Strategy history_requirements",
                code,
                &error,
            ));
        }
        Ok(StrategyHistoryRequirements { warmup_bars })
    }

    fn on_warmup_complete(
        &mut self,
        context: &StrategyContext,
        instrument: Option<&InstrumentId>,
        observed_bars: u64,
    ) -> Result<(), String> {
        let Some(callback) = self.on_warmup_complete else {
            return Ok(());
        };
        let _context_buffers = self.prepare_context(context)?;
        let instrument = instrument
            .map(|value| cstring(&value.to_string(), "warmup instrument"))
            .transpose()?;
        let event = QxStrategyWarmupCompleteV1 {
            struct_size: std::mem::size_of::<QxStrategyWarmupCompleteV1>() as u32,
            schema_version: QX_STRATEGY_WARMUP_COMPLETE_SCHEMA_VERSION_V1,
            instrument: instrument
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            instrument_len: instrument
                .as_ref()
                .map_or(0, |value| value.as_bytes().len()),
            has_instrument: u8::from(instrument.is_some()),
            observed_bars,
        };
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe { callback(self.handle, &event, error.as_mut_ptr(), error.len()) };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error(
                "C ABI Strategy on_warmup_complete",
                code,
                &error,
            ))
        }
    }

    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        let buffers = self.prepare_context(context)?;
        let callback = required_callback(self.vtable.on_init, "on_init")?;
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
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

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        let Some(callback) = self.on_start else {
            return Ok(());
        };
        let buffers = self.prepare_context(context)?;
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                &buffers.context,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error("C ABI Strategy on_start", code, &error))
        }
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.on_stop_with_context(&StrategyStopContextV1::host_stop(None))
    }

    fn on_stop_with_context(&mut self, stop: &StrategyStopContextV1) -> Result<(), String> {
        stop.validate()?;
        let Some(callback) = self.on_stop_v2 else {
            return self.call_stop_v1();
        };
        let payload = serde_json::to_string(stop)
            .map_err(|error| format!("C ABI StrategyStopContextV1 序列化失败: {error}"))?;
        if payload.len() > MAX_PLUGIN_STRING_BYTES {
            return Err("C ABI StrategyStopContextV1 超过大小限制".into());
        }
        let payload = cstring(&payload, "stop_context_json")?;
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = unsafe {
            callback(
                self.handle,
                payload.as_ptr(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(Self::call_error("C ABI Strategy on_stop_v2", code, &error))
        }
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        context.validate()?;
        event.validate()?;
        let context_buffers = self.prepare_context(context)?;
        let event_buffers = EventBuffers::new(event)?;
        if event_buffers.event_v2.has_exact_ohlcv != 0
            && self.on_market_event_v2.is_none()
            && self.on_market_event_v3.is_none()
        {
            return Err(
                "C ABI Strategy 收到精确 OHLCV，但插件未实现 qx_strategy_on_market_event_v2；拒绝降级为 raw v1"
                    .into(),
            );
        }
        let has_exact_orderbook = matches!(
            event,
            MarketEvent::OrderBook { bids, asks, .. }
                if bids.iter().chain(asks.iter()).any(|level| level.price_exact.is_some())
        );
        if has_exact_orderbook && self.on_market_event_v3.is_none() {
            return Err(
                "C ABI Strategy 收到精确盘口价量，但插件未实现 qx_strategy_on_market_event_v3；拒绝降级为 raw v1/v2"
                    .into(),
            );
        }
        let mut decision = QxStrategyDecision {
            schema_version: 0,
            request_id: ptr::null(),
            request_id_len: 0,
            strategy_id: ptr::null(),
            strategy_id_len: 0,
            signal_id: 0,
            confidence: QxRaw128::from_i128(0),
            priority: 0,
            expires_at: 0,
            intents: ptr::null_mut(),
            intents_len: 0,
            intents_capacity: 0,
        };
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let free_decision = required_callback(self.vtable.free_decision, "free_decision")?;
        let (code, callback_label) = if let Some(callback) = self.on_market_event_v3 {
            (
                unsafe {
                    callback(
                        self.handle,
                        &context_buffers.context,
                        &event_buffers.event_v3,
                        &mut decision,
                        error.as_mut_ptr(),
                        error.len(),
                    )
                },
                "C ABI Strategy on_market_event_v3",
            )
        } else if let Some(callback) = self.on_market_event_v2 {
            (
                unsafe {
                    callback(
                        self.handle,
                        &context_buffers.context,
                        &event_buffers.event_v2,
                        &mut decision,
                        error.as_mut_ptr(),
                        error.len(),
                    )
                },
                "C ABI Strategy on_market_event_v2",
            )
        } else {
            let callback = required_callback(self.vtable.on_event, "on_event")?;
            (
                unsafe {
                    callback(
                        self.handle,
                        &context_buffers.context,
                        &event_buffers.event,
                        &mut decision,
                        error.as_mut_ptr(),
                        error.len(),
                    )
                },
                "C ABI Strategy on_event",
            )
        };
        if code != 0 {
            return Err(Self::call_error(callback_label, code, &error));
        }
        let decoded = self.decode_decision(&decision, context, event.ts());
        unsafe {
            free_decision(self.handle, &mut decision);
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
        let _context_buffers = self.prepare_context(context)?;
        dispatch_json_update(
            self,
            context,
            update,
            callback,
            "C ABI Strategy on_order_update",
            context.as_of,
        )
    }

    fn on_strategy_fill_update_v2(
        &mut self,
        context: &StrategyContext,
        update: &StrategyFillUpdateV2,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        if let Some(callback) = self.on_fill_update_v2 {
            let _context_buffers = self.prepare_context(context)?;
            let payload = serde_json::to_string(update)
                .map_err(|error| format!("C ABI Strategy fill V2 JSON 编码失败: {error}"))?;
            return dispatch_json_update(
                self,
                context,
                &payload,
                callback,
                "C ABI Strategy on_fill_update_v2",
                update.ts,
            );
        }
        let legacy = serde_json::to_string(&update.legacy_projection())
            .map_err(|error| format!("C ABI Strategy legacy fill JSON 编码失败: {error}"))?;
        self.on_order_update(context, &legacy)
    }

    fn on_strategy_order_status(
        &mut self,
        context: &StrategyContext,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        if self.on_order_status.is_none() && self.on_order_status_v2.is_none() {
            return Ok(None);
        }
        let _context_buffers = self.prepare_context(context)?;
        let free_decision = required_callback(self.vtable.free_decision, "free_decision")?;
        let mut decision = empty_decision();
        let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
        let code = if let Some(callback) = self.on_order_status_v2 {
            let buffers = OrderStatusBuffers::new(update)?;
            let exact_amounts = update.order.exact_amounts.map(exact_amounts_wire);
            let wire = buffers.wire(update, exact_amounts.as_ref());
            unsafe {
                callback(
                    self.handle,
                    &wire,
                    &mut decision,
                    error.as_mut_ptr(),
                    error.len(),
                )
            }
        } else if let Some(callback) = self.on_order_status {
            let payload = serde_json::to_string(update)
                .map_err(|error| format!("C ABI Strategy order status JSON 编码失败: {error}"))?;
            let payload = cstring(&payload, "order status update")?;
            unsafe {
                callback(
                    self.handle,
                    payload.as_ptr(),
                    &mut decision,
                    error.as_mut_ptr(),
                    error.len(),
                )
            }
        } else {
            return Ok(None);
        };
        if code != 0 {
            unsafe { free_decision(self.handle, &mut decision) };
            return Err(Self::call_error(
                if self.on_order_status_v2.is_some() {
                    "C ABI Strategy on_order_status_v2"
                } else {
                    "C ABI Strategy on_order_status_v1"
                },
                code,
                &error,
            ));
        }
        if decision.signal_id == 0 {
            unsafe { free_decision(self.handle, &mut decision) };
            return Ok(None);
        }
        let decoded = self.decode_decision(&decision, context, update.ts);
        unsafe { free_decision(self.handle, &mut decision) };
        decoded.map(Some)
    }

    fn on_strategy_account_update(
        &mut self,
        context: &StrategyContext,
        update: &crate::contract::StrategyAccountUpdateV1,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        let Some(callback) = self.on_account_update else {
            return Ok(None);
        };
        let _context_buffers = self.prepare_context(context)?;
        let payload = serde_json::to_string(update)
            .map_err(|error| format!("C ABI Strategy account update JSON 编码失败: {error}"))?;
        dispatch_json_update(
            self,
            context,
            &payload,
            callback,
            "C ABI Strategy on_account_update_v1",
            update.ts,
        )
    }
}

fn dispatch_json_update(
    strategy: &mut CAbiStrategy,
    context: &StrategyContext,
    update_json: &str,
    callback: QxStrategyJsonUpdateCallbackV1,
    label: &str,
    decision_ts: u64,
) -> Result<Option<StrategyDecision>, String> {
    let free_decision = required_callback(strategy.vtable.free_decision, "free_decision")?;
    let update = cstring(update_json, "strategy update")?;
    let mut decision = empty_decision();
    let mut error = [0 as c_char; ERROR_BUFFER_SIZE];
    let code = unsafe {
        callback(
            strategy.handle,
            update.as_ptr(),
            &mut decision,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if code != 0 {
        unsafe { free_decision(strategy.handle, &mut decision) };
        return Err(CAbiStrategy::call_error(label, code, &error));
    }
    if decision.signal_id == 0 {
        unsafe { free_decision(strategy.handle, &mut decision) };
        return Ok(None);
    }
    let decoded = strategy.decode_decision(&decision, context, decision_ts);
    unsafe { free_decision(strategy.handle, &mut decision) };
    decoded.map(Some)
}

fn empty_decision() -> QxStrategyDecision {
    QxStrategyDecision {
        schema_version: 0,
        request_id: ptr::null(),
        request_id_len: 0,
        strategy_id: ptr::null(),
        strategy_id_len: 0,
        signal_id: 0,
        confidence: QxRaw128::from_i128(0),
        priority: 0,
        expires_at: 0,
        intents: ptr::null_mut(),
        intents_len: 0,
        intents_capacity: 0,
    }
}

impl Drop for CAbiStrategy {
    fn drop(&mut self) {
        let _ = self
            .vtable
            .destroy
            .map(|destroy| unsafe { destroy(self.handle) });
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
        let diagnostic_state = library
            .get::<QxStrategyDiagnosticState>(b"qx_strategy_diagnostic_state\0")
            .ok()
            .map(|callback| *callback);
        let on_start = library
            .get::<QxStrategyStartCallbackV1>(b"qx_strategy_on_start_v1\0")
            .ok()
            .map(|callback| *callback);
        let checkpoint_snapshot = library
            .get::<QxStrategyCheckpointSnapshotV1>(b"qx_strategy_checkpoint_snapshot_v1\0")
            .ok()
            .map(|callback| *callback);
        let checkpoint_restore = library
            .get::<QxStrategyCheckpointRestoreV1>(b"qx_strategy_checkpoint_restore_v1\0")
            .ok()
            .map(|callback| *callback);
        if checkpoint_snapshot.is_some() != checkpoint_restore.is_some() {
            return Err("C ABI Strategy checkpoint snapshot/restore 导出必须成对提供".into());
        }
        let on_stop = library
            .get::<QxStrategyStopCallbackV1>(b"qx_strategy_on_stop_v1\0")
            .ok()
            .map(|callback| *callback);
        let on_stop_v2 = library
            .get::<QxStrategyStopCallbackV2>(b"qx_strategy_on_stop_v2\0")
            .ok()
            .map(|callback| *callback);
        let on_error = library
            .get::<QxStrategyErrorCallbackV1>(b"qx_strategy_on_error_v1\0")
            .ok()
            .map(|callback| *callback);
        let history_requirements = library
            .get::<QxStrategyHistoryRequirementsCallbackV1>(
                b"qx_strategy_history_requirements_v1\0",
            )
            .ok()
            .map(|callback| *callback);
        let on_warmup_complete = library
            .get::<QxStrategyWarmupCompleteCallbackV1>(b"qx_strategy_on_warmup_complete_v1\0")
            .ok()
            .map(|callback| *callback);
        let on_order_status = library
            .get::<QxStrategyOrderStatusCallbackV1>(b"qx_strategy_on_order_status_v1\0")
            .ok()
            .map(|callback| *callback);
        let on_account_update = library
            .get::<QxStrategyAccountUpdateCallbackV1>(b"qx_strategy_on_account_update_v1\0")
            .ok()
            .map(|callback| *callback);
        let on_order_status_v2 = library
            .get::<QxStrategyOrderStatusCallbackV2>(b"qx_strategy_on_order_status_v2\0")
            .ok()
            .map(|callback| *callback);
        let on_fill_update_v2 = library
            .get::<QxStrategyFillUpdateCallbackV2>(b"qx_strategy_on_fill_update_v2\0")
            .ok()
            .map(|callback| *callback);
        let on_market_event_v2 = library
            .get::<QxStrategyMarketEventCallbackV2>(b"qx_strategy_on_market_event_v2\0")
            .ok()
            .map(|callback| *callback);
        let on_market_event_v3 = library
            .get::<QxStrategyMarketEventCallbackV3>(b"qx_strategy_on_market_event_v3\0")
            .ok()
            .map(|callback| *callback);
        let on_context_v2 = library
            .get::<QxStrategyContextCallbackV2>(b"qx_strategy_on_context_v2\0")
            .ok()
            .map(|callback| *callback);
        let order_policy = library
            .get::<QxStrategyOrderPolicyCallbackV1>(b"qx_strategy_get_order_policy_v1\0")
            .ok()
            .map(|callback| *callback);
        let mut inner = CAbiStrategy::from_vtable(vtable, config_json)?;
        inner.diagnostic_state = diagnostic_state;
        inner.checkpoint_snapshot = checkpoint_snapshot;
        inner.checkpoint_restore = checkpoint_restore;
        inner.on_start = on_start;
        inner.on_stop = on_stop;
        inner.on_stop_v2 = on_stop_v2;
        inner.on_error = on_error;
        inner.history_requirements = history_requirements;
        inner.on_warmup_complete = on_warmup_complete;
        inner.on_order_status = on_order_status;
        inner.on_account_update = on_account_update;
        inner.on_order_status_v2 = on_order_status_v2;
        inner.on_fill_update_v2 = on_fill_update_v2;
        inner.on_market_event_v2 = on_market_event_v2;
        inner.on_market_event_v3 = on_market_event_v3;
        inner.on_context_v2 = on_context_v2;
        inner.order_policy = order_policy;
        Ok(Self {
            inner,
            _library: library,
        })
    }

    pub fn inner(&self) -> &CAbiStrategy {
        &self.inner
    }

    pub fn supports_state_checkpoint(&self) -> bool {
        self.inner.supports_state_checkpoint()
    }

    pub fn has_partial_state_checkpoint_exports(&self) -> bool {
        self.inner.has_partial_state_checkpoint_exports()
    }

    pub fn restore_strategy_checkpoint(&mut self, state: &serde_json::Value) -> Result<(), String> {
        self.inner.restore_strategy_checkpoint(state)
    }

    pub fn snapshot_strategy_checkpoint(&self) -> Result<serde_json::Value, String> {
        self.inner.snapshot_strategy_checkpoint()
    }

    pub fn supports_order_status_updates(&self) -> bool {
        self.inner.supports_order_status_updates()
    }

    pub fn supports_account_updates(&self) -> bool {
        self.inner.supports_account_updates()
    }

    pub fn supports_fill_updates(&self) -> bool {
        self.inner.supports_fill_updates()
    }

    pub fn supports_order_policy_overrides(&self) -> bool {
        self.inner.supports_order_policy_overrides()
    }

    pub fn supports_exact_market_data(&self) -> bool {
        self.inner.supports_exact_market_data()
    }

    pub fn supports_exact_orderbook(&self) -> bool {
        self.inner.supports_exact_orderbook()
    }

    pub fn supports_exact_context(&self) -> bool {
        self.inner.supports_exact_context()
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
    fn on_error(&mut self, error: &StrategyErrorV1) -> Result<(), String> {
        self.inner.on_error(error)
    }

    fn diagnostic_state(&self) -> Option<serde_json::Value> {
        self.inner.diagnostic_state()
    }

    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.inner.on_init(context)
    }

    fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
        self.inner.history_requirements()
    }

    fn on_warmup_complete(
        &mut self,
        context: &StrategyContext,
        instrument: Option<&InstrumentId>,
        observed_bars: u64,
    ) -> Result<(), String> {
        self.inner
            .on_warmup_complete(context, instrument, observed_bars)
    }

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.inner.on_start(context)
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

    fn on_strategy_fill_update_v2(
        &mut self,
        context: &StrategyContext,
        update: &StrategyFillUpdateV2,
    ) -> Result<Option<StrategyDecision>, String> {
        self.inner.on_strategy_fill_update_v2(context, update)
    }

    fn on_strategy_order_status(
        &mut self,
        context: &StrategyContext,
        update: &StrategyOrderStatusUpdate,
    ) -> Result<Option<StrategyDecision>, String> {
        self.inner.on_strategy_order_status(context, update)
    }

    fn on_strategy_account_update(
        &mut self,
        context: &StrategyContext,
        update: &crate::contract::StrategyAccountUpdateV1,
    ) -> Result<Option<StrategyDecision>, String> {
        self.inner.on_strategy_account_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.inner.on_stop_with_context(&StrategyStopContextV1::new(
            StrategyStopReason::Shutdown,
            None,
        ))
    }

    fn on_stop_with_context(&mut self, stop: &StrategyStopContextV1) -> Result<(), String> {
        self.inner.on_stop_with_context(stop)
    }
}

fn cstring(value: &str, field: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| format!("C ABI {field} 不能包含 NUL"))
}

fn read_required_string(
    pointer: *const c_char,
    length: usize,
    field: &str,
) -> Result<String, String> {
    if pointer.is_null() {
        return Err(format!("C ABI {field} 不能为空"));
    }
    if length == 0 || length > MAX_PLUGIN_STRING_BYTES {
        return Err(format!(
            "C ABI {field} 长度非法: len={length} max={MAX_PLUGIN_STRING_BYTES}"
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("C ABI {field} 不是合法 UTF-8: {error}"))?;
    if text.contains('\0') {
        return Err(format!("C ABI {field} 含有内嵌 NUL"));
    }
    Ok(text.to_owned())
}

#[cfg(test)]
#[path = "c_api/tests.rs"]
mod tests;
