//! 策略进程契约载荷：输入/Bar/意图/输出结构与列式编码。

use super::*;

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

    /// 将兼容的单目标仓位输出转换为统一组合调仓计划。
    ///
    /// 多订单 `intents[]` 已经表达了明确的订单增量，不在这里再次推导目标仓位；
    /// 只有旧版 `target_qty` 输出走该路径，从而保证回测、Paper 和实盘兼容路径
    /// 使用同一套“目标仓位 → 数量增量”语义，并且能正确处理目标为零的清仓。
    pub fn build_rebalance_plan(
        &self,
        input: &StrategyContractInput,
        max_turnover_bps: u32,
        min_trade_size: i128,
    ) -> Result<qx_zhenlu::portfolio::RebalancePlan, String> {
        self.validate_for(input)?;
        if !self.intents.is_empty() {
            return Err("包含 intents[] 的策略输出不能再次转换为 target_qty 调仓计划".into());
        }
        let current = qx_zhenlu::portfolio::PortfolioState {
            portfolio_id: input.strategy_id.clone(),
            timestamp: input.as_of,
            cash: input
                .cash
                .values()
                .copied()
                .fold(0_i128, i128::saturating_add),
            positions: input.positions.clone(),
        };
        qx_zhenlu::portfolio::rebalance(
            &current,
            &[qx_core::TargetPosition::single(
                InstrumentId::parse(&self.instrument)
                    .ok_or_else(|| format!("instrument 非法: {}", self.instrument))?,
                self.target_qty,
            )],
            &qx_zhenlu::portfolio::PortfolioConstraint {
                max_turnover_bps,
                min_trade_size,
            },
        )
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
