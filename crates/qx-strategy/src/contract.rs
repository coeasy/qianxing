//! 策略进程契约载荷：输入/Bar/意图/输出结构与列式编码。

use super::*;
use qx_core::{
    AccountBalance, AccountPositionSnapshot, ExactDecimal, InstrumentId, MarginMode, PositionMode,
};
use std::collections::BTreeSet;

pub const STRATEGY_CONTRACT_SCHEMA_VERSION: u32 = STRATEGY_API_VERSION;
/// Input-only v2 adds committed execution facts while leaving the output contract at v1.
pub const STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION: u32 = 2;
/// Input-only v3 adds independent, committed fill facts to the v1/v2 strategy input.
pub const STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION: u32 = 3;
/// Input-only v4 carries a durable checkpoint for a process strategy.
pub const STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION: u32 = 4;
/// Input-only v5 carries exchange-precision OHLCV beside the legacy raw columns.
pub const STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION: u32 = 5;
/// Input-only v6 carries durable, exact L1 quote events for Runtime Tick strategies.
pub const STRATEGY_CONTRACT_INPUT_V6_SCHEMA_VERSION: u32 = 6;
/// Input-only v7 carries committed account balance and position snapshots.
pub const STRATEGY_CONTRACT_INPUT_V7_SCHEMA_VERSION: u32 = 7;
/// Input-only v8 carries durable, exact L2 order-book snapshots for Runtime strategies.
pub const STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION: u32 = 8;
/// Input-only v9 carries exact cash and available-margin account facts for process strategies.
pub const STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION: u32 = 9;
/// JSON wire version for committed account and position snapshots.
pub const STRATEGY_ACCOUNT_UPDATE_SCHEMA_VERSION: u32 = 1;
pub const STRATEGY_CHECKPOINT_SCHEMA_VERSION: u32 = 1;

/// Durable, JSON-only state exchanged with a process strategy worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyCheckpointV1 {
    pub schema_version: u32,
    pub strategy_id: String,
    pub strategy_version: String,
    pub adapter_state: serde_json::Value,
    pub strategy_state: serde_json::Value,
}

impl StrategyCheckpointV1 {
    pub fn validate_for_input(&self, input: &StrategyContractInput) -> Result<(), String> {
        if self.schema_version != STRATEGY_CHECKPOINT_SCHEMA_VERSION {
            return Err(format!(
                "Strategy checkpoint schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, STRATEGY_CHECKPOINT_SCHEMA_VERSION
            ));
        }
        if self.strategy_id != input.strategy_id || self.strategy_version != input.strategy_version
        {
            return Err("Strategy checkpoint 与当前策略身份/版本不匹配".into());
        }
        if !self.adapter_state.is_object() || !self.strategy_state.is_object() {
            return Err(
                "Strategy checkpoint 的 adapter_state 和 strategy_state 必须是 JSON object".into(),
            );
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|error| format!("Strategy checkpoint 编码失败: {error}"))?;
        if encoded.len() > 64 * 1024 {
            return Err("Strategy checkpoint 超过 64 KiB 上限".into());
        }
        Ok(())
    }
}

/// Python/其他语言策略进程看到的稳定、只读 JSON 输入。
///
/// 该对象不暴露 Rust 内部结构体或可变句柄；所有数量、金额保持定点整数，
/// instrument 使用字符串，便于 Python/Arrow/其他语言无损解析。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractInput {
    pub schema_version: u32,
    pub request_id: String,
    pub strategy_id: String,
    pub strategy_version: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub instrument: String,
    pub positions: BTreeMap<String, i128>,
    /// Exact signed positions alongside the legacy 1e-9 projection.
    #[serde(default)]
    pub positions_exact: BTreeMap<String, String>,
    pub cash: BTreeMap<String, i128>,
    /// Exact cash amounts alongside the legacy 1e-9 projection. Non-empty maps require input v9.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cash_exact: BTreeMap<String, String>,
    pub available_margin_raw: Option<i128>,
    /// Exact buying-power amount alongside the legacy 1e-9 projection. Requires input v9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_margin_exact: Option<String>,
    pub risk_state: String,
    /// 已经通过 PIT/研究快照校验的目标映射；Python 策略不得自行读取研究文件。
    pub research_targets: BTreeMap<String, i128>,
    /// 除主行情外的已对齐市场 Bar，键为完整 InstrumentId。
    #[serde(default)]
    pub market_bars: BTreeMap<String, StrategyContractBars>,
    #[serde(default)]
    pub bars: Option<StrategyContractBars>,
    /// Committed order-state facts for process strategies. Non-empty updates require input v2;
    /// v1 serialization remains byte-shape compatible by omitting this empty field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_updates: Vec<StrategyOrderStatusUpdate>,
    /// Committed fills for process strategies. Non-empty fills require input v3 and retain
    /// the authoritative EventLog sequence so file strategies can deduplicate redelivery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fill_updates: Vec<StrategyFillUpdateV2>,
    /// Last committed process-strategy state. Input v4 is required when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_checkpoint: Option<StrategyCheckpointV1>,
    /// Frozen L1 quote facts read from market-data EventLogs. Non-empty batches require input v6.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub market_ticks: Vec<StrategyContractMarketTickV1>,
    /// Committed, account-scoped balance/position snapshots. Non-empty updates require input v7.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_updates: Vec<StrategyAccountUpdateV1>,
    /// Frozen L2 snapshots read from market-data EventLogs. Non-empty batches require input v8.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub market_order_books: Vec<StrategyContractMarketOrderBookV1>,
}

/// One authoritative account observation from an account EventLog.
///
/// `account_snapshot` carries the complete reported balance set, including an empty set;
/// `position_snapshot` carries the complete reported position set, including an empty set.
/// These observations do not mutate the Rust ledger and must not be interpreted as fills.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyAccountUpdateV1 {
    pub schema_version: u32,
    pub event: String,
    pub event_seq: u64,
    pub ts: u64,
    pub account_id: String,
    pub venue_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balances: Option<Vec<AccountBalance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positions: Option<Vec<AccountPositionSnapshot>>,
}

impl StrategyAccountUpdateV1 {
    pub fn validate(&self, as_of: u64) -> Result<(), String> {
        if self.schema_version != STRATEGY_ACCOUNT_UPDATE_SCHEMA_VERSION
            || self.ts == 0
            || self.ts > as_of
            || self.account_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
        {
            return Err("StrategyAccountUpdate identity, schema or timestamp is invalid".into());
        }
        match self.event.as_str() {
            "account_snapshot" => {
                let balances = self.balances.as_ref().ok_or_else(|| {
                    "StrategyAccountUpdate account_snapshot must contain balances".to_string()
                })?;
                if self.positions.is_some() {
                    return Err(
                        "StrategyAccountUpdate account_snapshot cannot contain positions".into(),
                    );
                }
                let mut assets = BTreeSet::new();
                for balance in balances {
                    if balance.asset.trim().is_empty() || !assets.insert(&balance.asset) {
                        return Err(
                            "StrategyAccountUpdate balances contain an empty or duplicate asset"
                                .into(),
                        );
                    }
                    if let Some(exact) = balance.exact_amounts {
                        validate_exact_projection(exact.free, balance.free.raw(), "balance free")?;
                        validate_exact_projection(
                            exact.locked,
                            balance.locked.raw(),
                            "balance locked",
                        )?;
                        validate_exact_projection(
                            exact.borrowed,
                            balance.borrowed.raw(),
                            "balance borrowed",
                        )?;
                        if let Some(value) = exact.net_cash_balance {
                            let raw = balance
                                .net_cash_balance
                                .ok_or_else(|| {
                                    "StrategyAccountUpdate exact net cash has no raw projection"
                                        .to_string()
                                })?
                                .raw();
                            validate_exact_projection(value, raw, "balance net cash")?;
                        }
                    }
                }
            }
            "position_snapshot" => {
                let positions = self.positions.as_ref().ok_or_else(|| {
                    "StrategyAccountUpdate position_snapshot must contain positions".to_string()
                })?;
                if self.balances.is_some() {
                    return Err(
                        "StrategyAccountUpdate position_snapshot cannot contain balances".into(),
                    );
                }
                let mut identities = BTreeSet::new();
                for position in positions {
                    let instrument = position.instrument.to_string();
                    let side = position.position_side.as_deref().unwrap_or("net");
                    if !identities.insert((instrument, side.to_string())) {
                        return Err(
                            "StrategyAccountUpdate positions contain duplicate instrument/side"
                                .into(),
                        );
                    }
                    if position
                        .position_side
                        .as_deref()
                        .is_some_and(|side| side.trim().is_empty())
                    {
                        return Err("StrategyAccountUpdate position_side cannot be empty".into());
                    }
                    if let Some(exact) = position.exact_amounts {
                        validate_exact_projection(
                            exact.quantity,
                            position.quantity.raw(),
                            "position quantity",
                        )?;
                        for (name, value, raw) in [
                            (
                                "average_price",
                                exact.average_price,
                                position.average_price.map(|value| value.raw()),
                            ),
                            (
                                "mark_price",
                                exact.mark_price,
                                position.mark_price.map(|value| value.raw()),
                            ),
                            (
                                "liquidation_price",
                                exact.liquidation_price,
                                position.liquidation_price.map(|value| value.raw()),
                            ),
                            (
                                "unrealized_pnl",
                                exact.unrealized_pnl,
                                position.unrealized_pnl.map(|value| value.raw()),
                            ),
                            (
                                "initial_margin",
                                exact.initial_margin,
                                position.initial_margin.map(|value| value.raw()),
                            ),
                            (
                                "maintenance_margin",
                                exact.maintenance_margin,
                                position.maintenance_margin.map(|value| value.raw()),
                            ),
                        ] {
                            if let Some(value) = value {
                                let raw = raw.ok_or_else(|| {
                                    format!(
                                        "StrategyAccountUpdate exact {name} has no raw projection"
                                    )
                                })?;
                                validate_exact_projection(value, raw, name)?;
                            }
                        }
                    }
                }
            }
            _ => return Err("StrategyAccountUpdate event is unsupported".into()),
        }
        Ok(())
    }

    /// Bind an account observation to the strategy's configured account and venue.
    pub fn validate_for(&self, context: &StrategyContext) -> Result<(), String> {
        self.validate(context.as_of)?;
        if self.account_id != context.account_id || self.venue_id != context.venue_id {
            return Err(
                "StrategyAccountUpdate account/venue does not match StrategyContext".into(),
            );
        }
        Ok(())
    }
}

fn validate_exact_projection(exact: ExactDecimal, raw: i128, field: &str) -> Result<(), String> {
    let projected = exact_to_raw_1e9(exact, field).map_err(|error| {
        format!("StrategyAccountUpdate {field} exact amount is invalid: {error}")
    })?;
    if projected != raw {
        return Err(format!(
            "StrategyAccountUpdate {field} exact amount conflicts with its raw projection"
        ));
    }
    Ok(())
}

fn exact_to_raw_1e9(exact: ExactDecimal, field: &str) -> Result<i128, String> {
    exact
        .to_raw_truncated(9)
        .map_err(|error| format!("{field} raw projection failed: {error}"))
}

fn validate_contract_exact_projection(
    exact: ExactDecimal,
    raw: i128,
    field: &str,
) -> Result<(), String> {
    if exact_to_raw_1e9(exact, field)? != raw {
        return Err(format!("StrategyContractInput {field} 与 raw 投影冲突"));
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractBars {
    pub source: String,
    pub ts: Vec<u64>,
    pub open_raw: Vec<i128>,
    pub high_raw: Vec<i128>,
    pub low_raw: Vec<i128>,
    pub close_raw: Vec<i128>,
    pub volume_raw: Vec<i128>,
    /// Per-row source precision. `None` rows are legacy raw-only Bars; this
    /// sidecar is accepted only by StrategyContractInput v5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_ohlcv: Option<Vec<Option<ExactOhlcv>>>,
}

/// One durable L1 quote with both the legacy raw projection and source precision.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractMarketTickV1 {
    pub stream_id: String,
    pub event_seq: u64,
    pub source_seq: u64,
    pub ts: u64,
    pub instrument: String,
    pub bid_raw: i128,
    pub ask_raw: i128,
    pub bid_qty_raw: i128,
    pub ask_qty_raw: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_exact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_exact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_qty_exact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_qty_exact: Option<String>,
}

/// One exact price/quantity level in a durable market order-book snapshot.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractOrderBookLevelV1 {
    pub price_raw: i128,
    pub qty_raw: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_exact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty_exact: Option<String>,
}

/// One immutable order-book snapshot from a committed market EventLog.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractMarketOrderBookV1 {
    pub stream_id: String,
    pub event_seq: u64,
    pub source_seq: u64,
    pub ts: u64,
    pub instrument: String,
    pub sequence: u64,
    pub bids: Vec<StrategyContractOrderBookLevelV1>,
    pub asks: Vec<StrategyContractOrderBookLevelV1>,
}

impl StrategyContractMarketOrderBookV1 {
    pub fn validate(&self, as_of: u64) -> Result<(), String> {
        if self.stream_id.trim().is_empty()
            || self.ts == 0
            || self.ts > as_of
            || self.sequence == 0
            || InstrumentId::parse(&self.instrument).is_none()
        {
            return Err("Strategy market order book identity or timestamp is invalid".into());
        }
        if self.bids.is_empty()
            || self.asks.is_empty()
            || self.bids.len() > 100
            || self.asks.len() > 100
        {
            return Err("Strategy market order book requires 1..=100 levels per side".into());
        }
        let validate_side = |levels: &[StrategyContractOrderBookLevelV1], bid_side: bool| {
            let mut previous: Option<ExactDecimal> = None;
            for (index, level) in levels.iter().enumerate() {
                let name = if bid_side { "bid" } else { "ask" };
                let price = validate_order_book_amount(
                    level.price_exact.as_deref(),
                    level.price_raw,
                    &format!("{name}[{index}].price"),
                )?;
                validate_order_book_amount(
                    level.qty_exact.as_deref(),
                    level.qty_raw,
                    &format!("{name}[{index}].qty"),
                )?;
                if let Some(prior) = previous {
                    let ordering = prior.checked_cmp(price).ok_or_else(|| {
                        "Strategy market order book prices cannot be compared".to_string()
                    })?;
                    if if bid_side {
                        !ordering.is_gt()
                    } else {
                        !ordering.is_lt()
                    } {
                        return Err(
                            "Strategy market order book levels are not strictly sorted".into()
                        );
                    }
                }
                previous = Some(price);
            }
            Ok::<(), String>(())
        };
        validate_side(&self.bids, true)?;
        validate_side(&self.asks, false)?;
        let bid = validate_order_book_amount(
            self.bids[0].price_exact.as_deref(),
            self.bids[0].price_raw,
            "best_bid.price",
        )?;
        let ask = validate_order_book_amount(
            self.asks[0].price_exact.as_deref(),
            self.asks[0].price_raw,
            "best_ask.price",
        )?;
        if bid.checked_cmp(ask).is_none_or(|ordering| ordering.is_gt()) {
            return Err("Strategy market order book best bid exceeds best ask".into());
        }
        Ok(())
    }
}

fn validate_order_book_amount(
    exact: Option<&str>,
    raw: i128,
    field: &str,
) -> Result<ExactDecimal, String> {
    if raw < 0 {
        return Err(format!(
            "Strategy market order book {field}_raw cannot be negative"
        ));
    }
    let value = match exact {
        Some(text) => ExactDecimal::parse(text).map_err(|error| {
            format!("Strategy market order book {field}_exact is invalid: {error}")
        })?,
        None => ExactDecimal::from_parts(raw, 9).map_err(|error| {
            format!("Strategy market order book {field}_raw is invalid: {error}")
        })?,
    };
    if !value.is_positive() {
        return Err(format!(
            "Strategy market order book {field} must be positive"
        ));
    }
    let projection = value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, qx_core::DecimalRounding::TowardZero)
        .and_then(|scaled| scaled.to_raw(9).ok())
        .ok_or_else(|| format!("Strategy market order book {field} exceeds the raw range"))?;
    if projection != raw {
        return Err(format!(
            "Strategy market order book {field}_exact conflicts with its raw projection"
        ));
    }
    Ok(value)
}

impl StrategyContractMarketTickV1 {
    pub fn validate(&self, as_of: u64) -> Result<(), String> {
        if self.stream_id.trim().is_empty()
            || self.ts == 0
            || self.ts > as_of
            || InstrumentId::parse(&self.instrument).is_none()
        {
            return Err("Strategy market tick identity or timestamp is invalid".into());
        }
        let bid = self.exact_or_raw(self.bid_exact.as_deref(), self.bid_raw, "bid", false)?;
        let ask = self.exact_or_raw(self.ask_exact.as_deref(), self.ask_raw, "ask", false)?;
        if bid.checked_cmp(ask).is_none_or(|ordering| ordering.is_gt()) {
            return Err("Strategy market tick bid exceeds ask".into());
        }
        self.exact_or_raw(
            self.bid_qty_exact.as_deref(),
            self.bid_qty_raw,
            "bid_qty",
            true,
        )?;
        self.exact_or_raw(
            self.ask_qty_exact.as_deref(),
            self.ask_qty_raw,
            "ask_qty",
            true,
        )?;
        Ok(())
    }

    fn exact_or_raw(
        &self,
        exact: Option<&str>,
        raw: i128,
        field: &str,
        allow_zero: bool,
    ) -> Result<ExactDecimal, String> {
        let value = match exact {
            Some(value) => ExactDecimal::parse(value).map_err(|error| {
                format!("Strategy market tick {field}_exact is invalid: {error}")
            })?,
            None => ExactDecimal::from_parts(raw, 9)
                .map_err(|error| format!("Strategy market tick {field}_raw is invalid: {error}"))?,
        };
        if value.coefficient() < 0 || (!allow_zero && value.coefficient() == 0) {
            return Err(format!("Strategy market tick {field} must be positive"));
        }
        let projection = if value.scale() <= 9 {
            value
                .coefficient()
                .checked_mul(10_i128.pow(u32::from(9 - value.scale())))
        } else {
            Some(value.coefficient() / 10_i128.pow(u32::from(value.scale() - 9)))
        }
        .ok_or_else(|| format!("Strategy market tick {field} raw projection overflows"))?;
        if projection != raw {
            return Err(format!(
                "Strategy market tick {field}_exact conflicts with its raw projection"
            ));
        }
        Ok(value)
    }
}

/// Columnar request payload used inside `shared_memory_columnar`.
///
/// The control/identity fields remain canonical JSON so all language bindings
/// share one schema. Bar history is appended as fixed-width little-endian
/// columns, avoiding per-value JSON number parsing on the hot path.
pub const STRATEGY_COLUMNAR_MAGIC: [u8; 4] = *b"QXCB";
pub const STRATEGY_COLUMNAR_VERSION: u16 = 1;
/// Columnar version 2 adds a source-precision OHLCV sidecar in metadata.
pub const STRATEGY_COLUMNAR_EXACT_VERSION: u16 = 2;
pub const STRATEGY_COLUMNAR_EXACT_OHLCV_FLAG: u16 = 1;
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
    if let Some(exact_ohlcv) = &bars.exact_ohlcv {
        metadata_value["__qx_bars_exact_ohlcv"] = serde_json::to_value(exact_ohlcv)
            .map_err(|error| format!("策略列式精确 OHLCV 元数据编码失败: {error}"))?;
    }
    let metadata = serde_json::to_vec(&metadata_value)
        .map_err(|error| format!("策略列式输入元数据编码失败: {error}"))?;
    let column_bytes = rows
        .checked_mul(8 + 5 * 16)
        .ok_or_else(|| "策略列式输入长度溢出".to_string())?;
    let total = STRATEGY_COLUMNAR_HEADER_LEN
        .checked_add(metadata.len())
        .and_then(|value| value.checked_add(column_bytes))
        .ok_or_else(|| "策略列式输入总长度溢出".to_string())?;
    if total > DEFAULT_MAX_FRAME_BYTES {
        return Err(format!(
            "策略列式输入超过大小上限: bytes={total} max={}",
            DEFAULT_MAX_FRAME_BYTES
        ));
    }
    if metadata.len() > u32::MAX as usize || rows > u32::MAX as usize {
        return Err("策略列式输入元数据或行数超过 u32 上限".into());
    }
    let mut encoded = Vec::with_capacity(total);
    encoded.extend_from_slice(&STRATEGY_COLUMNAR_MAGIC);
    let has_exact_ohlcv = bars.has_exact_ohlcv();
    let version = if has_exact_ohlcv {
        STRATEGY_COLUMNAR_EXACT_VERSION
    } else {
        STRATEGY_COLUMNAR_VERSION
    };
    let flags = if has_exact_ohlcv {
        STRATEGY_COLUMNAR_EXACT_OHLCV_FLAG
    } else {
        0
    };
    encoded.extend_from_slice(&version.to_le_bytes());
    encoded.extend_from_slice(&flags.to_le_bytes());
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
///
/// `deny_unknown_fields` 兑现 `schemas/strategy_api_v1.schema.json` 的
/// `additionalProperties: false`：拼错的可选键（`post_onli`、`margin_mod`）过去会被
/// serde 静默丢掉，那一腿于是按运行时默认档位成交，而策略作者以为自己在它上面开了
/// 对冲或 post-only（V12 R4-k）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractIntent {
    pub intent_id: u64,
    pub instrument: String,
    /// Optional account route for multi-account strategies. When omitted, the runtime strategy account is used.
    #[serde(default)]
    pub account_id: Option<String>,
    /// `buy` 或 `sell`，避免不同语言对 Rust enum 名称的依赖。
    pub side: String,
    pub qty_raw: i128,
    /// Optional exact decimal quantity for live crypto intents. When set, `qty_raw` is zero.
    #[serde(default)]
    pub qty_exact: Option<String>,
    /// Quote-currency spending cap for a Bitget Spot market buy.
    #[serde(default)]
    pub quote_budget_exact: Option<String>,
    #[serde(default)]
    pub limit_price_raw: Option<i128>,
    /// Optional exact decimal limit price; only valid with `qty_exact`.
    #[serde(default)]
    pub limit_price_exact: Option<String>,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub post_only: bool,
    #[serde(default)]
    pub position_side: Option<String>,
    /// 多腿策略可为每条腿覆盖执行模式；为空时沿用运行时主策略配置。
    #[serde(default)]
    pub margin_mode: Option<String>,
    #[serde(default)]
    pub position_mode: Option<String>,
    #[serde(default)]
    pub leverage: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyContractOutput {
    pub schema_version: u32,
    pub request_id: String,
    pub strategy_id: String,
    pub signal_id: u64,
    pub instrument: String,
    pub target_qty: i128,
    /// Optional exact signed target position; when present `target_qty` must be zero.
    #[serde(default)]
    pub target_qty_exact: Option<String>,
    pub confidence: i128,
    pub priority: i32,
    pub expires_at: u64,
    /// 新版策略输出；为空时兼容旧版 `target_qty` 语义。
    #[serde(default)]
    pub intents: Vec<StrategyContractIntent>,
}

impl StrategyContractBars {
    pub fn has_exact_ohlcv(&self) -> bool {
        self.exact_ohlcv
            .as_ref()
            .is_some_and(|rows| rows.iter().any(Option::is_some))
    }

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
        if let Some(exact_rows) = &self.exact_ohlcv {
            if exact_rows.len() != length {
                return Err("策略 Bar 契约 exact_ohlcv 行数不一致".into());
            }
            let mut found_exact = false;
            for (index, exact) in exact_rows.iter().enumerate() {
                let Some(exact) = exact else { continue };
                found_exact = true;
                exact
                    .validate()
                    .map_err(|error| format!("策略 Bar 契约 exact_ohlcv[{index}] 无效: {error}"))?;
                let projected = exact.raw_projection().map_err(|error| {
                    format!("策略 Bar 契约 exact_ohlcv[{index}] 投影失败: {error}")
                })?;
                let raw = [
                    self.open_raw[index],
                    self.high_raw[index],
                    self.low_raw[index],
                    self.close_raw[index],
                    self.volume_raw[index],
                ];
                if projected != raw {
                    return Err(format!(
                        "策略 Bar 契约 exact_ohlcv[{index}] 与 raw 投影不一致"
                    ));
                }
            }
            if !found_exact {
                return Err("策略 Bar 契约 exact_ohlcv 至少需要一行精确值".into());
            }
        }
        Ok(())
    }
}

impl StrategyContractInput {
    pub fn has_exact_ohlcv(&self) -> bool {
        self.bars
            .as_ref()
            .is_some_and(StrategyContractBars::has_exact_ohlcv)
            || self
                .market_bars
                .values()
                .any(StrategyContractBars::has_exact_ohlcv)
    }

    /// Select the narrowest input schema that represents all optional facts.
    /// Exact account facts use v9 and can coexist with the preceding feature versions.
    pub fn schema_version_for_features(&self) -> u32 {
        if !self.cash_exact.is_empty() || self.available_margin_exact.is_some() {
            STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION
        } else if !self.market_order_books.is_empty() {
            STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION
        } else if !self.account_updates.is_empty() {
            STRATEGY_CONTRACT_INPUT_V7_SCHEMA_VERSION
        } else if !self.market_ticks.is_empty() {
            STRATEGY_CONTRACT_INPUT_V6_SCHEMA_VERSION
        } else if self.has_exact_ohlcv() {
            STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION
        } else if self.strategy_checkpoint.is_some() {
            STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION
        } else if !self.fill_updates.is_empty() {
            STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION
        } else if !self.execution_updates.is_empty() {
            STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION
        } else {
            STRATEGY_CONTRACT_SCHEMA_VERSION
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let has_exact_account_facts =
            !self.cash_exact.is_empty() || self.available_margin_exact.is_some();
        if has_exact_account_facts
            && self.schema_version != STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION
        {
            return Err("StrategyContractInput exact cash/margin requires schema_version=9".into());
        }
        if self.schema_version == STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION
            && !has_exact_account_facts
        {
            return Err("策略输入 v9 必须携带非空 cash_exact 或 available_margin_exact".into());
        }
        // 版本单独立一臂：折叠进下面那句通用文案时，读到 `schema_version=7` 的调用方只看到
        // 「身份、时间、标的或风险字段非法」——最需要读出的那一格（版本不受支持）反而读不出来。
        // Python 侧 `StrategyInput.validate` 早就具名（`unsupported strategy schema_version: N`），
        // 两边在同一份产物上一严一宽。T1-2 要求「旧产物读取必须走显式迁移或给出具名拒绝原因」，
        // 故这一格与其他七份契约的 `schema_version={} 不受支持（本构建只认 {}）` 同口径。
        match self.schema_version {
            STRATEGY_CONTRACT_SCHEMA_VERSION
                if !self.execution_updates.is_empty()
                    || !self.fill_updates.is_empty()
                    || !self.account_updates.is_empty()
                    || !self.market_order_books.is_empty() =>
            {
                return Err("策略输入 v1 不允许包含执行更新或账户更新".into());
            }
            STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION
                if self.execution_updates.is_empty()
                    || !self.fill_updates.is_empty()
                    || !self.account_updates.is_empty()
                    || !self.market_order_books.is_empty() =>
            {
                return Err("策略输入 v2 必须含非空 execution_updates，不允许 fill_updates 或 account_updates（账户更新需要 v7）".into());
            }
            STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION
                if self.fill_updates.is_empty()
                    || !self.account_updates.is_empty()
                    || !self.market_order_books.is_empty() =>
            {
                return Err(
                    "策略输入 v3 必须包含非空 fill_updates，且不允许 account_updates（账户更新需要 v7）".into(),
                );
            }
            STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION
                if self.strategy_checkpoint.is_none()
                    || !self.account_updates.is_empty()
                    || !self.market_order_books.is_empty() =>
            {
                return Err("策略输入 v4 必须包含 strategy_checkpoint，且不允许 account_updates（账户更新需要 v7）".into());
            }
            STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION
                if !self.has_exact_ohlcv()
                    || !self.account_updates.is_empty()
                    || !self.market_order_books.is_empty() =>
            {
                return Err(
                    "策略输入 v5 必须包含精确 OHLCV，且不允许 account_updates（账户更新需要 v7）"
                        .into(),
                );
            }
            STRATEGY_CONTRACT_INPUT_V6_SCHEMA_VERSION
                if self.market_ticks.is_empty()
                    || !self.account_updates.is_empty()
                    || !self.market_order_books.is_empty() =>
            {
                return Err("策略输入 v6 必须包含 market_ticks，且不允许 account_updates（账户更新需要 v7）".into());
            }
            STRATEGY_CONTRACT_INPUT_V7_SCHEMA_VERSION
                if self.account_updates.is_empty() || !self.market_order_books.is_empty() =>
            {
                return Err("策略输入 v7 必须包含 account_updates".into());
            }
            STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION if self.market_order_books.is_empty() => {
                return Err("策略输入 v8 必须包含 market_order_books".into());
            }
            STRATEGY_CONTRACT_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION
                if self.has_exact_ohlcv() =>
            {
                return Err("策略输入 v1-v4 不允许包含精确 OHLCV".into());
            }
            STRATEGY_CONTRACT_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION
                if self.strategy_checkpoint.is_some() =>
            {
                return Err("策略输入 v1-v3 不允许包含 strategy_checkpoint".into());
            }
            STRATEGY_CONTRACT_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION
                if !self.market_ticks.is_empty() =>
            {
                return Err("策略输入 v1-v5 不允许包含 market_ticks".into());
            }
            STRATEGY_CONTRACT_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V2_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V3_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V5_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V6_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V7_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V8_SCHEMA_VERSION
            | STRATEGY_CONTRACT_INPUT_V9_SCHEMA_VERSION => {}
            version => {
                return Err(format!(
                    "策略输入契约 schema_version={version} 不受支持（本构建只认 1、2、3、4、5、6、7、8 和 9）"
                ));
            }
        }
        if self.request_id.trim().is_empty()
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
        if let Some(checkpoint) = &self.strategy_checkpoint {
            checkpoint.validate_for_input(self)?;
        }
        for key in self
            .positions
            .keys()
            .chain(self.positions_exact.keys())
            .chain(self.research_targets.keys())
        {
            let instrument_key = key
                .rsplit_once('|')
                .map(|(account_id, instrument)| {
                    !account_id.trim().is_empty() && InstrumentId::parse(instrument).is_some()
                })
                .unwrap_or_else(|| InstrumentId::parse(key).is_some());
            if !instrument_key {
                return Err(format!("StrategyContractInput instrument 非法: {key}"));
            }
        }
        for (instrument, quantity) in &self.positions_exact {
            let exact = ExactDecimal::parse(quantity).map_err(|error| {
                format!("StrategyContractInput positions_exact[{instrument}] 非法: {error}")
            })?;
            if let Some(raw) = self.positions.get(instrument) {
                let projection = if exact.scale() <= 9 {
                    exact
                        .coefficient()
                        .checked_mul(10_i128.pow(u32::from(9 - exact.scale())))
                } else {
                    Some(exact.coefficient() / 10_i128.pow(u32::from(exact.scale() - 9)))
                }
                .ok_or_else(|| {
                    format!("StrategyContractInput positions_exact[{instrument}] raw 投影溢出")
                })?;
                if projection != *raw {
                    return Err(format!(
                        "StrategyContractInput positions_exact[{instrument}] 与 positions raw 投影冲突"
                    ));
                }
            }
        }
        for (currency, amount) in &self.cash_exact {
            if currency.trim().is_empty() {
                return Err("StrategyContractInput cash_exact currency 不能为空".into());
            }
            let exact = ExactDecimal::parse(amount).map_err(|error| {
                format!("StrategyContractInput cash_exact[{currency}] 非法: {error}")
            })?;
            let raw = self.cash.get(currency).ok_or_else(|| {
                format!("StrategyContractInput cash_exact[{currency}] 缺少 cash raw 投影")
            })?;
            validate_contract_exact_projection(exact, *raw, &format!("cash_exact[{currency}]"))?;
        }
        if let Some(amount) = &self.available_margin_exact {
            let exact = ExactDecimal::parse(amount).map_err(|error| {
                format!("StrategyContractInput available_margin_exact 非法: {error}")
            })?;
            if exact.coefficient() < 0 {
                return Err("StrategyContractInput available_margin_exact 不能为负".into());
            }
            let raw = self.available_margin_raw.ok_or_else(|| {
                "StrategyContractInput available_margin_exact 缺少 raw 投影".to_string()
            })?;
            validate_contract_exact_projection(exact, raw, "available_margin_exact")?;
        }
        if let Some(bars) = &self.bars {
            bars.validate()?;
        }
        for (instrument, bars) in &self.market_bars {
            if InstrumentId::parse(instrument).is_none() || instrument == &self.instrument {
                return Err(format!(
                    "StrategyContractInput market_bars instrument 非法或重复主行情: {instrument}"
                ));
            }
            bars.validate()?;
            if bars
                .ts
                .last()
                .is_some_and(|timestamp| *timestamp > self.as_of)
            {
                return Err(format!(
                    "StrategyContractInput market_bars[{instrument}] 含有 as_of 之后的 Bar"
                ));
            }
        }
        if self.market_ticks.len() > 10_000 {
            return Err("Strategy market tick batch exceeds 10000 events".into());
        }
        let mut previous_tick_key: Option<(u64, &str, u64)> = None;
        let mut tick_identities = BTreeSet::new();
        for tick in &self.market_ticks {
            tick.validate(self.as_of)?;
            if !tick_identities.insert((tick.stream_id.as_str(), tick.event_seq)) {
                return Err(
                    "Strategy market tick batch contains duplicate EventLog identities".into(),
                );
            }
            let key = (tick.ts, tick.stream_id.as_str(), tick.event_seq);
            if previous_tick_key.is_some_and(|previous| previous >= key) {
                return Err(
                    "Strategy market ticks must be ordered by timestamp, stream, and sequence"
                        .into(),
                );
            }
            previous_tick_key = Some(key);
        }
        if self.market_order_books.len() > 10_000 {
            return Err("Strategy market order-book batch exceeds 10000 events".into());
        }
        let mut previous_book_key: Option<(u64, &str, u64)> = None;
        let mut book_identities = BTreeSet::new();
        for book in &self.market_order_books {
            book.validate(self.as_of)?;
            if !book_identities.insert((book.stream_id.as_str(), book.event_seq)) {
                return Err(
                    "Strategy market order-book batch contains duplicate EventLog identities"
                        .into(),
                );
            }
            let key = (book.ts, book.stream_id.as_str(), book.event_seq);
            if previous_book_key.is_some_and(|previous| previous >= key) {
                return Err(
                    "Strategy market order books must be ordered by timestamp, stream, and sequence"
                        .into(),
                );
            }
            previous_book_key = Some(key);
        }
        let mut update_identities = BTreeSet::new();
        for update in &self.execution_updates {
            update.validate()?;
            if update.order.strategy_id != self.strategy_id {
                return Err("StrategyContractInput execution update 策略身份不匹配".into());
            }
            if update.ts > self.as_of {
                return Err("StrategyContractInput execution update 晚于 as_of".into());
            }
            let identity = (
                update.order.account_id.as_str(),
                update.order.instrument.as_str(),
                update.order.order_id,
                update.event_seq,
            );
            if !update_identities.insert(identity) {
                return Err("StrategyContractInput execution_updates 包含重复事件身份".into());
            }
        }
        let mut fill_identities = BTreeSet::new();
        for update in &self.fill_updates {
            update.validate()?;
            if update.order.strategy_id != self.strategy_id {
                return Err("StrategyContractInput fill update 策略身份不匹配".into());
            }
            if update.ts > self.as_of {
                return Err("StrategyContractInput fill update 晚于 as_of".into());
            }
            let identity = (
                update.order.account_id.as_str(),
                update.order.instrument.as_str(),
                update.order.order_id,
                update.event_seq,
            );
            if !fill_identities.insert(identity) {
                return Err("StrategyContractInput fill_updates 包含重复事件身份".into());
            }
        }
        let mut account_identities = BTreeSet::new();
        let mut previous_account_key: Option<(u64, &str, &str, u64)> = None;
        for update in &self.account_updates {
            update.validate(self.as_of)?;
            let identity = (
                update.account_id.as_str(),
                update.venue_id.as_str(),
                update.event_seq,
            );
            if !account_identities.insert(identity) {
                return Err("StrategyContractInput account_updates 包含重复 EventLog 身份".into());
            }
            let key = (
                update.ts,
                update.account_id.as_str(),
                update.venue_id.as_str(),
                update.event_seq,
            );
            if previous_account_key.is_some_and(|previous| previous >= key) {
                return Err(
                    "StrategyContractInput account_updates 必须按时间、账户、venue、序号递增"
                        .into(),
                );
            }
            previous_account_key = Some(key);
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
    pub fn from_native_decision(decision: &StrategyDecision) -> Result<Self, String> {
        let intents: Vec<StrategyContractIntent> = decision
            .intents
            .iter()
            .map(|intent| StrategyContractIntent {
                intent_id: intent.intent_id,
                instrument: intent.instrument.to_string(),
                account_id: None,
                side: match intent.side {
                    qx_core::Side::Buy => "buy".into(),
                    qx_core::Side::Sell => "sell".into(),
                },
                qty_raw: intent.qty.raw(),
                qty_exact: intent.qty_exact.map(|quantity| quantity.to_string()),
                quote_budget_exact: intent.quote_budget.map(|budget| budget.to_string()),
                limit_price_raw: intent.limit.map(|price| price.raw()),
                limit_price_exact: intent.limit_price_exact.map(|price| price.to_string()),
                reduce_only: intent.reduce_only,
                post_only: intent.post_only,
                position_side: intent
                    .policy
                    .map(|policy| format!("{:?}", policy.position_side).to_ascii_lowercase()),
                margin_mode: intent.policy.map(|policy| match policy.margin_mode {
                    MarginMode::Cash => "cash".into(),
                    MarginMode::Cross => "cross".into(),
                    MarginMode::Isolated => "isolated".into(),
                }),
                position_mode: intent.policy.map(|policy| match policy.position_mode {
                    PositionMode::OneWay => "one_way".into(),
                    PositionMode::Hedge => "hedge".into(),
                }),
                leverage: intent.policy.map(|policy| policy.leverage),
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
            target_qty_exact: None,
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
        // 逐臂点名，与 Python `StrategyOutput.validate_for` 同一口径：折叠成一句"与输入不一致"
        // 时，最需要读出差在哪一格的恰好是 worker 已放行、宿主才挡下的那条跨语言分叉。
        if self.schema_version != STRATEGY_CONTRACT_SCHEMA_VERSION {
            return Err(format!(
                "StrategyContractOutput schema_version 不匹配，期望 {STRATEGY_CONTRACT_SCHEMA_VERSION} 实得 {}",
                self.schema_version
            ));
        }
        if self.request_id != input.request_id {
            return Err("StrategyContractOutput request_id 与输入不一致".into());
        }
        if self.strategy_id != input.strategy_id {
            return Err("StrategyContractOutput strategy_id 与输入不一致".into());
        }
        if self.signal_id == 0 {
            return Err("StrategyContractOutput signal_id 必须为正".into());
        }
        if self.instrument != input.instrument {
            return Err("StrategyContractOutput instrument 与输入不一致".into());
        }
        if InstrumentId::parse(&self.instrument).is_none() {
            return Err(format!(
                "StrategyContractOutput instrument 非法: {}",
                self.instrument
            ));
        }
        if self.expires_at != 0 && self.expires_at < input.as_of {
            return Err("StrategyContractOutput expires_at 早于输入 as_of".into());
        }
        if let Some(target_qty_exact) = self.target_qty_exact.as_deref() {
            if self.target_qty != 0 {
                return Err(
                    "StrategyContractOutput target_qty 与 target_qty_exact 不能同时使用".into(),
                );
            }
            ExactDecimal::parse(target_qty_exact).map_err(|error| {
                format!("StrategyContractOutput target_qty_exact 非法: {error}")
            })?;
            if !self.intents.is_empty() {
                return Err(
                    "StrategyContractOutput target_qty_exact 不能与 intents 同时使用".into(),
                );
            }
        }
        let mut intent_ids = BTreeSet::new();
        for intent in &self.intents {
            let exact_quantity = intent
                .qty_exact
                .as_deref()
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| format!("StrategyContractOutput qty_exact 非法: {error}"))?;
            let exact_limit = intent
                .limit_price_exact
                .as_deref()
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| {
                    format!("StrategyContractOutput limit_price_exact 非法: {error}")
                })?;
            let exact_quote_budget = intent
                .quote_budget_exact
                .as_deref()
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| {
                    format!("StrategyContractOutput quote_budget_exact 非法: {error}")
                })?;
            let quantity_valid = match (exact_quantity, exact_quote_budget) {
                (Some(quantity), None) => intent.qty_raw == 0 && quantity.is_positive(),
                (None, Some(budget)) => intent.qty_raw == 0 && budget.is_positive(),
                (None, None) => intent.qty_raw > 0,
                (Some(_), Some(_)) => false,
            };
            let price_valid = match exact_limit {
                Some(price) => {
                    exact_quantity.is_some()
                        && intent.limit_price_raw.is_none()
                        && price.is_positive()
                }
                None => intent.limit_price_raw.is_none_or(|price| price > 0),
            };
            if intent.intent_id == 0
                || !intent_ids.insert(intent.intent_id)
                || InstrumentId::parse(&intent.instrument).is_none()
                || intent
                    .account_id
                    .as_deref()
                    .is_some_and(|account_id| account_id.trim().is_empty())
                || !quantity_valid
                || !matches!(intent.side.to_ascii_lowercase().as_str(), "buy" | "sell")
                || !price_valid
                || (exact_quantity.is_some() && intent.limit_price_raw.is_some())
                || (exact_quote_budget.is_some()
                    && (exact_quantity.is_some()
                        || exact_limit.is_some()
                        || intent.limit_price_raw.is_some()
                        || !intent.side.eq_ignore_ascii_case("buy")))
                || intent.position_side.as_deref().is_some_and(|side| {
                    !matches!(side.to_ascii_lowercase().as_str(), "net" | "long" | "short")
                })
                || intent.margin_mode.as_deref().is_some_and(|mode| {
                    !matches!(
                        mode.to_ascii_lowercase().as_str(),
                        "cash" | "cross" | "isolated"
                    )
                })
                || intent.position_mode.as_deref().is_some_and(|mode| {
                    !matches!(mode.to_ascii_lowercase().as_str(), "one_way" | "hedge")
                })
                || intent.leverage.is_some_and(|leverage| leverage == 0)
            {
                return Err("StrategyContractOutput 中存在非法或重复的 OrderIntent".into());
            }
        }
        Ok(())
    }

    pub fn from_json_for(input: &str, request: &StrategyContractInput) -> Result<Self, String> {
        let value: Self = serde_json::from_str(input)
            .map_err(|error| format!("策略输出契约 JSON 无效: {error}"))?;
        value.validate_for(request)?;
        Ok(value)
    }

    /// Decode a process worker response envelope and validate its contained strategy output.
    pub fn from_python_response(
        line: &str,
        request: &StrategyContractInput,
    ) -> Result<Self, String> {
        Self::from_python_response_with_checkpoint(line, request).map(|(output, _)| output)
    }

    pub fn from_python_response_with_checkpoint(
        line: &str,
        request: &StrategyContractInput,
    ) -> Result<(Self, Option<StrategyCheckpointV1>), String> {
        Self::from_strategy_process_response_with_checkpoint(line, request)
    }

    /// Decode the shared JSONL/framed process response envelope, optionally including
    /// a durable checkpoint. The Python-named method above remains for source compatibility.
    pub fn from_strategy_process_response_with_checkpoint(
        line: &str,
        request: &StrategyContractInput,
    ) -> Result<(Self, Option<StrategyCheckpointV1>), String> {
        let response: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("Strategy process 响应 JSON 无效: {error}"))?;
        if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(format!(
                "Strategy process 拒绝请求: {}",
                response
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            ));
        }
        let encoded = serde_json::to_string(
            response
                .get("output")
                .ok_or_else(|| "Strategy process 响应缺少 output".to_string())?,
        )
        .map_err(|error| format!("编码 Strategy process output 失败: {error}"))?;
        let output = Self::from_json_for(&encoded, request)?;
        let checkpoint = response
            .get("strategy_checkpoint")
            .map(|value| {
                let checkpoint: StrategyCheckpointV1 = serde_json::from_value(value.clone())
                    .map_err(|error| format!("Strategy process checkpoint 无效: {error}"))?;
                checkpoint.validate_for_input(request)?;
                Ok::<_, String>(checkpoint)
            })
            .transpose()?;
        Ok((output, checkpoint))
    }
}

#[cfg(test)]
mod exact_raw_json_tests {
    use super::StrategyContractIntent;

    #[test]
    fn strategy_contract_intent_preserves_i128_raw_json_exactly() {
        let qty_raw = 100_000_000_000_000_000_001_i128;
        let input = format!(
            r#"{{"intent_id":7,"instrument":"BTC-USDT-SWAP","side":"buy","qty_raw":{qty_raw}}}"#
        );

        let intent: StrategyContractIntent = serde_json::from_str(&input).unwrap();
        assert_eq!(intent.qty_raw, qty_raw);
        assert!(serde_json::to_string(&intent)
            .unwrap()
            .contains(&qty_raw.to_string()));

        let value: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(value["qty_raw"].to_string(), qty_raw.to_string());
    }
}

#[cfg(test)]
mod account_update_version_tests {
    use super::{
        StrategyAccountUpdateV1, StrategyCheckpointV1, StrategyContractInput,
        StrategyContractMarketOrderBookV1, StrategyContractOrderBookLevelV1,
        StrategyContractOutput, STRATEGY_CHECKPOINT_SCHEMA_VERSION,
        STRATEGY_CONTRACT_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    fn input(schema_version: u32) -> StrategyContractInput {
        StrategyContractInput {
            schema_version,
            request_id: "strategy:1".into(),
            strategy_id: "strategy".into(),
            strategy_version: "1".into(),
            data_fingerprint: "fixture".into(),
            as_of: 1,
            instrument: "BTCUSDT.BINANCE".into(),
            positions: BTreeMap::new(),
            positions_exact: BTreeMap::new(),
            cash: BTreeMap::new(),
            cash_exact: std::collections::BTreeMap::new(),
            available_margin_exact: None,
            available_margin_raw: None,
            risk_state: "ready".into(),
            research_targets: BTreeMap::new(),
            market_bars: BTreeMap::new(),
            bars: None,
            execution_updates: Vec::new(),
            fill_updates: Vec::new(),
            strategy_checkpoint: None,
            market_ticks: Vec::new(),
            market_order_books: Vec::new(),
            account_updates: Vec::new(),
        }
    }

    fn empty_account_snapshot() -> StrategyAccountUpdateV1 {
        StrategyAccountUpdateV1 {
            schema_version: 1,
            event: "account_snapshot".into(),
            event_seq: 0,
            ts: 1,
            account_id: "account".into(),
            venue_id: "BINANCE".into(),
            balances: Some(Vec::new()),
            positions: None,
        }
    }

    #[test]
    fn external_process_response_round_trips_durable_checkpoint() {
        let checkpoint = StrategyCheckpointV1 {
            schema_version: STRATEGY_CHECKPOINT_SCHEMA_VERSION,
            strategy_id: "strategy".into(),
            strategy_version: "1".into(),
            adapter_state: serde_json::json!({
                "schema_version": 1,
                "state_mode": "external-process"
            }),
            strategy_state: serde_json::json!({ "processed_requests": 3 }),
        };
        let mut request = input(4);
        request.strategy_checkpoint = Some(checkpoint.clone());
        request.validate().unwrap();
        let response = serde_json::json!({
            "ok": true,
            "output": {
                "schema_version": STRATEGY_CONTRACT_SCHEMA_VERSION,
                "request_id": request.request_id,
                "strategy_id": request.strategy_id,
                "signal_id": 1,
                "instrument": request.instrument,
                "target_qty": 0,
                "confidence": 0,
                "priority": 0,
                "expires_at": 1,
                "intents": []
            },
            "strategy_checkpoint": checkpoint
        });

        let (output, restored) = StrategyContractOutput::from_python_response_with_checkpoint(
            &response.to_string(),
            &request,
        )
        .unwrap();

        assert_eq!(output.strategy_id, "strategy");
        assert_eq!(restored, Some(checkpoint));
    }

    #[test]
    fn legacy_strategy_input_version_rejects_account_update_with_upgrade_hint() {
        let mut legacy = input(6);
        legacy.account_updates.push(empty_account_snapshot());

        let error = legacy.validate().unwrap_err();

        assert!(error.contains("account_updates"));
        assert!(error.contains("v7"));
    }

    #[test]
    fn strategy_input_v7_accepts_the_account_snapshot_contract() {
        let mut current = input(7);
        current.account_updates.push(empty_account_snapshot());

        assert_eq!(current.schema_version_for_features(), 7);
        current.validate().unwrap();
    }

    #[test]
    fn strategy_input_v8_accepts_exact_order_book_and_rejects_crossed_or_unsorted_levels() {
        let level = |price: &str, quantity: &str| StrategyContractOrderBookLevelV1 {
            price_raw: 0,
            qty_raw: 0,
            price_exact: Some(price.into()),
            qty_exact: Some(quantity.into()),
        };
        let book = StrategyContractMarketOrderBookV1 {
            stream_id: "market:binance".into(),
            event_seq: 9,
            source_seq: 12,
            ts: 1,
            instrument: "BTCUSDT.BINANCE".into(),
            sequence: 88,
            bids: vec![level("0.0000000007", "0.0000000002")],
            asks: vec![level("0.0000000009", "0.0000000004")],
        };
        let mut current = input(8);
        current.market_order_books.push(book.clone());
        assert_eq!(current.schema_version_for_features(), 8);
        current.validate().unwrap();
        let encoded = serde_json::to_value(&current).unwrap();
        assert_eq!(
            encoded["market_order_books"][0]["bids"][0]["price_exact"],
            "0.0000000007"
        );
        assert_eq!(
            serde_json::from_value::<StrategyContractInput>(encoded).unwrap(),
            current
        );

        let mut crossed = current.clone();
        crossed.market_order_books[0].asks[0].price_exact = Some("0.0000000006".into());
        assert!(crossed
            .validate()
            .unwrap_err()
            .contains("best bid exceeds best ask"));

        let mut unsorted = current;
        unsorted.market_order_books[0].bids = vec![
            level("0.0000000007", "0.0000000002"),
            level("0.0000000008", "0.0000000002"),
        ];
        unsorted.market_order_books[0].asks = vec![level("0.0000000009", "0.0000000004")];
        assert!(unsorted
            .validate()
            .unwrap_err()
            .contains("not strictly sorted"));
    }
}
