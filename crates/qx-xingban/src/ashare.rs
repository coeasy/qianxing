//! A 股 Bar 回测规则：交易日、T+1、整手、涨跌停、停牌和费用参数。
//!
//! 规则与数据源解耦。数据源只提供 BarFrame；本模块决定该 Bar 是否可交易、
//! 订单是否合规以及成交后哪些仓位在当日仍不可卖出。

use qx_core::{Fnv1a, Order, Side, SCALE};
use qx_guanxing::Bar;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DAY_MS: u64 = 86_400_000;
const SHANGHAI_OFFSET_MS: u64 = 8 * 3_600_000;

mod json;
mod timecal;
mod trading;

pub(crate) use json::*;
pub(crate) use timecal::*;
pub use trading::*;

/// Python→Rust A 股数据契约版本。文档缺省 `schema_version` 视为 `0`（旧格式，
/// 宽松解析），`>= 1` 启用严格模式：顶层必须携带非空 `source`，未知字段直接
/// 报错，且每条公司行为必须提供 PIT 可见时间 `published_at`。
///
/// 必须与 `python/qianxing_ashare.ASHARE_SCHEMA_VERSION` 以及
/// `crates/qx-data/src/provider.rs` 的 BarFrame 契约保持一致；任何 bump 都要
/// 同时更新下面的字段白名单，否则 Python 写出的文档会被 Rust fail-closed 拒绝。
pub const ASHARE_SCHEMA_VERSION: u32 = 1;

/// v1 公司行为文档（信封）顶层字段白名单，与 Python
/// `ASHARE_ACTION_ENVELOPE_FIELDS` 逐项一致。
const ASHARE_ACTION_ENVELOPE_FIELDS: [&str; 5] =
    ["schema_version", "source", "instrument", "as_of", "actions"];

/// v1 单条公司行为的字段白名单，与 Python `ASHARE_ACTION_FIELDS` 逐项一致。
const ASHARE_ACTION_FIELDS: [&str; 35] = [
    "announcement_date",
    "action_type",
    "cash_dividend_raw",
    "conversion_price_raw",
    "conversion_qty_raw",
    "conversion_ratio_den",
    "conversion_ratio_num",
    "conversion_target_instrument",
    "conversion_target_qty_raw",
    "convertible_bond_instrument",
    "ex_date",
    "interest_per_bond_raw",
    "instrument",
    "issue_price_raw",
    "issuer_free_float_shares_raw",
    "issuer_total_shares_raw",
    "payment_date",
    "published_at",
    "raw_payload",
    "record_date",
    "repurchase_price_raw",
    "repurchase_qty_raw",
    "rights_expiry_qty_raw",
    "rights_instrument",
    "rights_issue_price_raw",
    "rights_issue_ratio_den",
    "rights_issue_ratio_num",
    "settlement_price_raw",
    "settlement_qty_raw",
    "share_ratio_den",
    "share_ratio_num",
    "source",
    "subscription_end",
    "subscription_qty_raw",
    "subscription_start",
];

/// v1 交易日历字段白名单，与 Python `ASHARE_CALENDAR_FIELDS` 逐项一致。
const ASHARE_CALENDAR_FIELDS: [&str; 5] = [
    "calendar_id",
    "trading_days",
    "sessions",
    "schema_version",
    "source",
];

fn default_true() -> bool {
    true
}

fn default_lot_size() -> i128 {
    100 * SCALE
}

fn default_price_tick() -> i128 {
    SCALE / 100
}

fn default_limit_bp() -> i64 {
    1_000
}

fn default_commission_bp() -> i64 {
    3
}

fn default_min_commission() -> i128 {
    5 * SCALE
}

fn default_stamp_duty_bp() -> i64 {
    5
}

fn default_transfer_fee_bp() -> i64 {
    1
}

fn default_ratio() -> i128 {
    1
}

fn default_action_type() -> AshareCorporateActionType {
    AshareCorporateActionType::CashDividend
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AshareBoard {
    #[default]
    Main,
    ChiNext,
    Star,
    Beijing,
    Etf,
    St,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AshareCorporateActionType {
    #[default]
    CashDividend,
    BonusShare,
    CapitalTransfer,
    RightsIssue,
    RightsIssueExpiry,
    NewShareIssue,
    Repurchase,
    ConvertibleBondIssue,
    ConvertibleBondInterest,
    ConvertibleBondRedemption,
    ConvertibleBondCall,
    ConvertibleBondPut,
    ConvertibleBondConversion,
    Suspension,
    CapitalChange,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AshareCorporateActionEvent {
    pub ts: u64,
    /// 公告/登记/认购/结算日期使用独立时间轴；`ts` 保持旧格式的除权/生效日。
    #[serde(default)]
    pub announcement_ts: Option<u64>,
    #[serde(default)]
    pub record_ts: Option<u64>,
    #[serde(default)]
    pub payment_ts: Option<u64>,
    #[serde(default)]
    pub subscription_start_ts: Option<u64>,
    #[serde(default)]
    pub subscription_end_ts: Option<u64>,
    #[serde(default = "default_action_type")]
    pub action_type: AshareCorporateActionType,
    #[serde(default)]
    pub cash_dividend_raw: i128,
    #[serde(default = "default_ratio")]
    pub split_num: i128,
    #[serde(default = "default_ratio")]
    pub split_den: i128,
    #[serde(default)]
    pub rights_issue_price_raw: i128,
    #[serde(default)]
    pub rights_issue_ratio_num: i128,
    #[serde(default = "default_ratio")]
    pub rights_issue_ratio_den: i128,
    #[serde(default)]
    pub issue_price_raw: i128,
    #[serde(default)]
    pub conversion_price_raw: i128,
    #[serde(default)]
    pub conversion_ratio_num: i128,
    #[serde(default = "default_ratio")]
    pub conversion_ratio_den: i128,
    /// 需要用户/策略明确确认的认购与转股事实；缺失时回测必须拒绝。
    #[serde(default)]
    pub rights_instrument: Option<String>,
    #[serde(default)]
    pub subscription_qty_raw: i128,
    /// 配股权利失效数量；不复用认购数量，避免把认购和到期事实混为一谈。
    #[serde(default)]
    pub rights_expiry_qty_raw: i128,
    #[serde(default)]
    pub repurchase_qty_raw: i128,
    #[serde(default)]
    pub repurchase_price_raw: i128,
    #[serde(default)]
    pub convertible_bond_instrument: Option<String>,
    #[serde(default)]
    pub conversion_target_instrument: Option<String>,
    #[serde(default)]
    pub conversion_qty_raw: i128,
    #[serde(default)]
    pub conversion_target_qty_raw: i128,
    /// 可转债每张/每单位的票息；只用于 ConvertibleBondInterest。
    #[serde(default)]
    pub interest_per_bond_raw: i128,
    /// 回售/赎回的明确交付数量与结算价格。
    #[serde(default)]
    pub settlement_qty_raw: i128,
    #[serde(default)]
    pub settlement_price_raw: i128,
    /// CapitalChange 必须提供发行人层面的绝对总股本；禁止从账户持仓或
    /// “变更数量”反推，避免把发行人事实错误地当成账户账务。
    #[serde(default)]
    pub issuer_total_shares_raw: i128,
    /// 可选的绝对流通股本。数据源没有该字段时保持 None，而不是把总股本
    /// 猜测为流通股本。
    #[serde(default)]
    pub issuer_free_float_shares_raw: Option<i128>,
    #[serde(default)]
    pub source: String,
    /// PIT 可见时间（epoch 毫秒），随快照携带供审计。唯一的判定发生在公司行为
    /// JSON 加载闸门：信封带 `as_of` 时按 `published_at_ms <= as_of` 取舍，
    /// 旧格式缺省为 `None`，此时无法证明可见性的事件一律 fail-closed 丢弃。
    #[serde(default)]
    pub published_at_ms: Option<u64>,
    /// 数据源原始字段快照，仅用于审计和重放，不参与撮合语义。
    #[serde(default)]
    pub raw_payload: Option<serde_json::Value>,
}

/// `raw_payload` 只承载 JSON 值；`serde_json` 无法表示 NaN/Infinity，因此
/// 该结构上的 `Eq` 是全成立的，手工实现以保持既有公开约束不变。
impl Eq for AshareCorporateActionEvent {}

/// 发行人层面的股本快照，不产生账户 Ledger entry。
///
/// 该事实用于回测结果解释、流通市值/股本校验和后续选股数据绑定；它与
/// 账户持仓严格分离，只有显式提供的绝对值才允许进入回测。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AshareIssuerCapitalSnapshot {
    pub instrument: String,
    pub effective_ts: u64,
    pub total_shares_raw: i128,
    #[serde(default)]
    pub free_float_shares_raw: Option<i128>,
    #[serde(default)]
    pub source: String,
}

/// 一次公司行为 JSON 加载的契约与 PIT 报告。
///
/// 该报告把“这份数据按哪个契约版本写出、来自哪里、研究截止日是多少、
/// 有多少事件因为尚未公告被挡在外面”变成可登记的结构，供质量报告和
/// DatasetManifest 血缘使用；`row_count == applied_actions + hidden_actions
/// + halted_timestamps`。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AshareCorporateActionLoadReport {
    /// 文档契约版本；`0` 表示旧格式（无 `schema_version`）。
    pub schema_version: u32,
    /// 文档级来源；旧格式数组允许为空。
    pub source: String,
    pub instrument: String,
    /// 信封携带的 PIT 截止时间（epoch 毫秒）。
    pub as_of_ms: Option<u64>,
    pub row_count: usize,
    /// 真正进入规则快照的公司行为数量。
    pub applied_actions: usize,
    /// `published_at > as_of`（或无法证明可见性）而被挡下的行数。
    pub hidden_actions: usize,
    /// 作为停牌事实处理的行数。
    pub halted_timestamps: usize,
}

/// 一次交易日历 JSON 加载的契约报告；字段语义与公司行为报告一致。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AshareCalendarLoadReport {
    pub schema_version: u32,
    pub source: String,
    pub calendar_id: String,
    pub trading_days: usize,
    pub sessions: usize,
}

/// 已解析的公司行为文档：契约版本、来源、绑定标的、PIT 截止时间和事件行。
struct AshareActionDocument<'a> {
    schema_version: u32,
    source: String,
    instrument: Option<&'a str>,
    as_of_ms: Option<u64>,
    actions: &'a [serde_json::Value],
}

impl<'a> AshareActionDocument<'a> {
    /// `schema_version >= 1` 即严格模式：信封与每行都受白名单约束。
    fn is_strict(&self) -> bool {
        self.schema_version >= 1
    }

    /// PIT 可见性判定：没有 `as_of` 时全部可见，否则必须能证明
    /// `published_at <= as_of`。
    fn visible_at(&self, published_at_ms: Option<u64>) -> bool {
        match self.as_of_ms {
            None => true,
            Some(as_of) => published_at_ms.is_some_and(|published| published <= as_of),
        }
    }

    /// 解析 v0 数组 / v0 `{actions:[...]}` 包装 / v1 信封。
    ///
    /// 缺省 `schema_version` 一律按 `0` 处理并保留旧的宽松解析路径，因此
    /// 历史样例继续可加载；高于 [`ASHARE_SCHEMA_VERSION`] 的版本 fail-closed，
    /// 避免新文档被旧 Rust 静默降级解析。
    fn parse(document: &'a serde_json::Value) -> Result<Self, String> {
        let (object, actions) = match document {
            serde_json::Value::Array(actions) => (None, actions.as_slice()),
            serde_json::Value::Object(object) => {
                let actions = object
                    .get("actions")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| {
                        "公司行为 JSON 必须是数组或包含 actions 数组的对象".to_string()
                    })?;
                (Some(object), actions.as_slice())
            }
            _ => {
                return Err("公司行为 JSON 必须是数组或包含 actions 数组的对象".into());
            }
        };
        let schema_version =
            object.map_or(Ok(0), |object| json_schema_version(object, "公司行为"))?;
        if schema_version > ASHARE_SCHEMA_VERSION {
            return Err(format!(
                "不支持的公司行为 schema_version={schema_version}（本构建支持 0 旧格式与 1..={ASHARE_SCHEMA_VERSION}）"
            ));
        }
        let Some(object) = object else {
            // 顶层数组是 v0 的最小形态：没有信封字段，来源和可见性由行自身提供。
            return Ok(Self {
                schema_version: 0,
                source: String::new(),
                instrument: None,
                as_of_ms: None,
                actions,
            });
        };
        let instrument = object
            .get("instrument")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let as_of_ms = object
            .get("as_of")
            .filter(|value| !value.is_null())
            .map(timestamp_to_ms)
            .transpose()?;
        let source = object
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if schema_version >= 1 {
            reject_unknown_fields(object, &ASHARE_ACTION_ENVELOPE_FIELDS, "公司行为文档")?;
            let missing: Vec<&str> = ASHARE_ACTION_ENVELOPE_FIELDS
                .iter()
                .copied()
                .filter(|key| !object.contains_key(*key))
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "公司行为 schema_version={schema_version} 缺少字段: {missing:?}"
                ));
            }
            if source.is_empty() {
                return Err(format!(
                    "公司行为 schema_version={schema_version} 要求非空 source"
                ));
            }
            if instrument.is_none() {
                return Err(format!(
                    "公司行为 schema_version={schema_version} 要求非空 instrument"
                ));
            }
        }
        Ok(Self {
            schema_version,
            source,
            instrument,
            as_of_ms,
            actions,
        })
    }
}

impl Default for AshareCorporateActionEvent {
    fn default() -> Self {
        Self {
            ts: 0,
            announcement_ts: None,
            record_ts: None,
            payment_ts: None,
            subscription_start_ts: None,
            subscription_end_ts: None,
            action_type: AshareCorporateActionType::default(),
            cash_dividend_raw: 0,
            split_num: default_ratio(),
            split_den: default_ratio(),
            rights_issue_price_raw: 0,
            rights_issue_ratio_num: 0,
            rights_issue_ratio_den: default_ratio(),
            issue_price_raw: 0,
            conversion_price_raw: 0,
            conversion_ratio_num: 0,
            conversion_ratio_den: default_ratio(),
            rights_instrument: None,
            subscription_qty_raw: 0,
            rights_expiry_qty_raw: 0,
            repurchase_qty_raw: 0,
            repurchase_price_raw: 0,
            convertible_bond_instrument: None,
            conversion_target_instrument: None,
            conversion_qty_raw: 0,
            conversion_target_qty_raw: 0,
            interest_per_bond_raw: 0,
            settlement_qty_raw: 0,
            settlement_price_raw: 0,
            issuer_total_shares_raw: 0,
            issuer_free_float_shares_raw: None,
            source: String::new(),
            published_at_ms: None,
            raw_payload: None,
        }
    }
}

impl AshareBoard {
    pub fn default_limit_bp(self) -> i64 {
        match self {
            Self::Main | Self::St | Self::Etf => 1_000,
            Self::ChiNext | Self::Star => 2_000,
            Self::Beijing => 3_000,
        }
    }
}

/// A 股规则配置。所有金额、价格和数量仍使用 Qianxing 的 1e9 定点单位。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AshareRuleConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub board: AshareBoard,
    #[serde(default = "default_true")]
    pub t_plus_one: bool,
    #[serde(default = "default_lot_size")]
    pub lot_size: i128,
    #[serde(default = "default_true")]
    pub allow_odd_lot_sell: bool,
    #[serde(default = "default_limit_bp")]
    pub limit_up_bp: i64,
    #[serde(default = "default_limit_bp")]
    pub limit_down_bp: i64,
    #[serde(default = "default_price_tick")]
    pub price_tick: i128,
    #[serde(default = "default_commission_bp")]
    pub commission_bp: i64,
    #[serde(default = "default_min_commission")]
    pub min_commission: i128,
    #[serde(default = "default_stamp_duty_bp")]
    pub stamp_duty_bp: i64,
    #[serde(default = "default_transfer_fee_bp")]
    pub transfer_fee_bp: i64,
    /// 非空时只允许这些 bar 时间戳交易；为空表示由输入数据决定。
    #[serde(default)]
    pub trading_timestamps: Vec<u64>,
    /// 交易日午夜（Asia/Shanghai）时间戳。日线 Bar 使用该集合判断交易日，
    /// 分钟 Bar 再结合 session_windows 判断具体时段。
    #[serde(default)]
    pub trading_days: Vec<u64>,
    /// 可选交易时段，元素为 [start_ts, end_ts)，支持集合竞价/连续竞价拆分。
    #[serde(default)]
    pub session_windows: Vec<[u64; 2]>,
    /// 停牌 bar 时间戳。停牌 bar 不接受新订单，也不产生成交。
    #[serde(default)]
    pub halted_timestamps: Vec<u64>,
    /// 可选昨收覆盖（除权除息日的锚），key 为被锚定 Bar 的 ts；缺省由 `previous_close` 推导。
    #[serde(default)]
    pub previous_close_raw: BTreeMap<u64, i128>,
    #[serde(default)]
    pub corporate_actions: Vec<AshareCorporateActionEvent>,
}

impl Default for AshareRuleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            board: AshareBoard::Main,
            t_plus_one: true,
            lot_size: default_lot_size(),
            allow_odd_lot_sell: true,
            limit_up_bp: default_limit_bp(),
            limit_down_bp: default_limit_bp(),
            price_tick: default_price_tick(),
            commission_bp: default_commission_bp(),
            min_commission: default_min_commission(),
            stamp_duty_bp: default_stamp_duty_bp(),
            transfer_fee_bp: default_transfer_fee_bp(),
            trading_timestamps: Vec::new(),
            trading_days: Vec::new(),
            session_windows: Vec::new(),
            halted_timestamps: Vec::new(),
            previous_close_raw: BTreeMap::new(),
            corporate_actions: Vec::new(),
        }
    }
}

impl AshareRuleConfig {
    /// 将 Python A 股数据层输出的公司行为 JSON 转换为规则快照。
    ///
    /// 输入可以是 v0 动作数组（`[...]` 或 `{ "actions": [...] }`），也可以是
    /// v1 信封 `{schema_version, source, instrument, as_of, actions}`。转换只
    /// 负责规范化日期、比例和字段；复杂事件仍由回测账本的支持矩阵决定，不能
    /// 因为完成了 JSON 解析就被误当成已实现的资金/权利语义。
    ///
    /// 返回值保持既有计数语义（文档中的行数）；需要契约版本、来源和 PIT
    /// 过滤统计时改用 [`AshareRuleConfig::apply_corporate_actions_json_with_report`]。
    pub fn apply_corporate_actions_json(
        &mut self,
        instrument: &str,
        payload: &str,
    ) -> Result<usize, String> {
        self.apply_corporate_actions_json_with_report(instrument, payload)
            .map(|report| report.row_count)
    }

    /// 与 [`AshareRuleConfig::apply_corporate_actions_json`] 共用同一条解析路径，
    /// 额外返回可审计的契约/PIT 报告。
    ///
    /// 严格模式（`schema_version >= 1`）下：信封字段集合与每行字段集合都受
    /// 白名单约束、`source` 必须非空、每行必须提供 `published_at`。信封携带
    /// `as_of` 时，`published_at > as_of`（或缺少可见时间）的事件不会进入
    /// 规则快照，避免把未来公告的公司行为写进历史回测。
    pub fn apply_corporate_actions_json_with_report(
        &mut self,
        instrument: &str,
        payload: &str,
    ) -> Result<AshareCorporateActionLoadReport, String> {
        if instrument.trim().is_empty() {
            return Err("A 股公司行为 JSON 缺少 instrument".into());
        }
        let document: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| format!("公司行为 JSON 无效: {error}"))?;
        let contract = AshareActionDocument::parse(&document)?;
        if let Some(actual) = contract.instrument {
            if actual != instrument {
                return Err(format!(
                    "公司行为 instrument 不匹配: expected={instrument} actual={actual}"
                ));
            }
        }
        let strict = contract.is_strict();
        let actions = contract.actions;
        let mut converted = Vec::with_capacity(actions.len());
        let mut halted = Vec::new();
        let mut hidden = 0_usize;
        for (index, action) in actions.iter().enumerate() {
            let object = action
                .as_object()
                .ok_or_else(|| format!("公司行为 actions[{index}] 必须是对象"))?;
            if strict {
                reject_unknown_fields(
                    object,
                    &ASHARE_ACTION_FIELDS,
                    &format!("公司行为 actions[{index}]"),
                )?;
            }
            if let Some(actual) = object.get("instrument").and_then(serde_json::Value::as_str) {
                if actual != instrument {
                    return Err(format!(
                        "公司行为 instrument 不匹配: expected={instrument} actual={actual}"
                    ));
                }
            }
            let ex_date = object
                .get("ex_date")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("公司行为 actions[{index}] 缺少 ex_date"))?;
            let ts = date_to_shanghai_midnight_ms(ex_date)
                .map_err(|error| format!("公司行为 actions[{index}] ex_date 非法: {error}"))?;
            let published_at_ms = json_optional_timestamp(object, "published_at")
                .map_err(|error| format!("公司行为 actions[{index}] published_at 非法: {error}"))?;
            if strict && published_at_ms.is_none() {
                return Err(format!(
                    "公司行为 actions[{index}] 在 schema_version={} 下必须提供 published_at",
                    contract.schema_version
                ));
            }
            let raw_payload = json_raw_payload(object, strict)
                .map_err(|error| format!("公司行为 actions[{index}] raw_payload 非法: {error}"))?;
            let action_type = object
                .get("action_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let parsed_type = parse_json_action_type(action_type).ok_or_else(|| {
                format!("公司行为 actions[{index}] action_type 非法: {action_type}")
            })?;
            // PIT 闸门放在语义校验之后：不可见事件不进入快照，但格式错误仍
            // 必须 fail-closed，避免坏数据被“看不见”静默吞掉。
            if !contract.visible_at(published_at_ms) {
                hidden += 1;
                continue;
            }
            if parsed_type == AshareCorporateActionType::Suspension {
                halted.push(ts);
                continue;
            }
            let mut row_source: String = object
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into();
            if row_source.trim().is_empty() && !contract.source.is_empty() {
                // 行内缺省来源时用文档级来源补齐，Rust 侧不会产出无血缘事件。
                row_source = contract.source.clone();
            }
            converted.push(AshareCorporateActionEvent {
                ts,
                announcement_ts: json_optional_date(object, "announcement_date")?,
                record_ts: json_optional_date(object, "record_date")?,
                payment_ts: json_optional_date(object, "payment_date")?,
                subscription_start_ts: json_optional_date(object, "subscription_start")?,
                subscription_end_ts: json_optional_date(object, "subscription_end")?,
                action_type: parsed_type,
                cash_dividend_raw: json_i128(object, "cash_dividend_raw")?,
                split_num: json_i128_or(object, "share_ratio_num", 1)?,
                split_den: json_i128_or(object, "share_ratio_den", 1)?,
                rights_issue_price_raw: json_i128(object, "rights_issue_price_raw")?,
                rights_issue_ratio_num: json_i128(object, "rights_issue_ratio_num")?,
                rights_issue_ratio_den: json_i128_or(object, "rights_issue_ratio_den", 1)?,
                issue_price_raw: json_i128(object, "issue_price_raw")?,
                conversion_price_raw: json_i128(object, "conversion_price_raw")?,
                conversion_ratio_num: json_i128(object, "conversion_ratio_num")?,
                conversion_ratio_den: json_i128_or(object, "conversion_ratio_den", 1)?,
                rights_instrument: object
                    .get("rights_instrument")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                subscription_qty_raw: json_i128(object, "subscription_qty_raw")?,
                rights_expiry_qty_raw: json_i128(object, "rights_expiry_qty_raw")?,
                repurchase_qty_raw: json_i128(object, "repurchase_qty_raw")?,
                repurchase_price_raw: json_i128(object, "repurchase_price_raw")?,
                convertible_bond_instrument: object
                    .get("convertible_bond_instrument")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                conversion_target_instrument: object
                    .get("conversion_target_instrument")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                conversion_qty_raw: json_i128(object, "conversion_qty_raw")?,
                conversion_target_qty_raw: json_i128(object, "conversion_target_qty_raw")?,
                interest_per_bond_raw: json_i128(object, "interest_per_bond_raw")?,
                settlement_qty_raw: json_i128(object, "settlement_qty_raw")?,
                settlement_price_raw: json_i128(object, "settlement_price_raw")?,
                issuer_total_shares_raw: json_i128(object, "issuer_total_shares_raw")?,
                issuer_free_float_shares_raw: json_optional_i128(
                    object,
                    "issuer_free_float_shares_raw",
                )?,
                source: row_source,
                published_at_ms,
                raw_payload,
            });
        }
        converted.sort_by_key(|event| event.ts);
        halted.sort_unstable();
        halted.dedup();
        let applied_actions = converted.len();
        let halted_count = halted.len();
        // 先在候选快照上校验，失败时不污染调用方已有的规则配置。
        let mut candidate = self.clone();
        candidate.corporate_actions.extend(converted);
        candidate.corporate_actions.sort_by_key(|event| event.ts);
        candidate.halted_timestamps.extend(halted);
        candidate.halted_timestamps.sort_unstable();
        candidate.halted_timestamps.dedup();
        candidate.validate()?;
        *self = candidate;
        Ok(AshareCorporateActionLoadReport {
            schema_version: contract.schema_version,
            source: contract.source,
            instrument: instrument.to_owned(),
            as_of_ms: contract.as_of_ms,
            row_count: actions.len(),
            applied_actions,
            hidden_actions: hidden,
            halted_timestamps: halted_count,
        })
    }

    /// 将 Python AshareTradingCalendar JSON 合并到规则快照。交易日使用
    /// Asia/Shanghai 午夜时间戳；如果日历包含 sessions，则为每个交易日展开
    /// [start, end) 时段。日线 Bar 的午夜时间戳仍视为该交易日有效。
    ///
    /// 返回交易日数量，保持既有调用方的计数语义；需要契约版本与来源时用
    /// [`AshareRuleConfig::apply_calendar_json_with_report`]。
    pub fn apply_calendar_json(&mut self, payload: &str) -> Result<usize, String> {
        self.apply_calendar_json_with_report(payload)
            .map(|report| report.trading_days)
    }

    /// 与 [`AshareRuleConfig::apply_calendar_json`] 共用解析路径，额外登记
    /// `schema_version`、`source` 和 `calendar_id`，让交易日历的血缘可以进入
    /// 质量报告而不是退化成一份无身份的文件。
    pub fn apply_calendar_json_with_report(
        &mut self,
        payload: &str,
    ) -> Result<AshareCalendarLoadReport, String> {
        let document: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| format!("交易日历 JSON 无效: {error}"))?;
        let object = document
            .as_object()
            .ok_or_else(|| "交易日历必须是对象".to_string())?;
        let schema_version = json_schema_version(object, "交易日历")?;
        if schema_version > ASHARE_SCHEMA_VERSION {
            return Err(format!(
                "不支持的交易日历 schema_version={schema_version}（本构建支持 0 旧格式与 1..={ASHARE_SCHEMA_VERSION}）"
            ));
        }
        let mut source = String::new();
        if schema_version >= 1 {
            reject_unknown_fields(object, &ASHARE_CALENDAR_FIELDS, "交易日历")?;
            let missing: Vec<&str> = ASHARE_CALENDAR_FIELDS
                .iter()
                .copied()
                .filter(|key| !object.contains_key(*key))
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "交易日历 schema_version={schema_version} 缺少字段: {missing:?}"
                ));
            }
            source = object
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            if source.is_empty() {
                return Err(format!(
                    "交易日历 schema_version={schema_version} 要求非空 source"
                ));
            }
        }
        let calendar_id = object
            .get("calendar_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let days = object
            .get("trading_days")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "交易日历必须包含 trading_days 数组".to_string())?;
        if days.is_empty() {
            return Err("交易日历 trading_days 不能为空".into());
        }
        let mut trading_days = Vec::with_capacity(days.len());
        for (index, value) in days.iter().enumerate() {
            let text = value
                .as_str()
                .ok_or_else(|| format!("交易日历 trading_days[{index}] 必须是日期字符串"))?;
            trading_days.push(date_to_shanghai_midnight_ms(text)?);
        }
        trading_days.sort_unstable();
        trading_days.dedup();
        let mut session_windows = Vec::new();
        if let Some(sessions) = object.get("sessions").and_then(serde_json::Value::as_array) {
            for (day_index, day) in trading_days.iter().enumerate() {
                for (session_index, session) in sessions.iter().enumerate() {
                    let values = session.as_array().ok_or_else(|| {
                        format!("交易日历 sessions[{session_index}] 必须是 [start,end]")
                    })?;
                    if values.len() != 2 {
                        return Err(format!(
                            "交易日历 sessions[{session_index}] 必须包含两个时间"
                        ));
                    }
                    let start = values[0].as_str().ok_or_else(|| {
                        format!("交易日历 sessions[{session_index}][0] 必须是时间字符串")
                    })?;
                    let end = values[1].as_str().ok_or_else(|| {
                        format!("交易日历 sessions[{session_index}][1] 必须是时间字符串")
                    })?;
                    let start_ms = time_of_day_ms(start).map_err(|error| {
                        format!("交易日历 trading_days[{day_index}] session 非法: {error}")
                    })?;
                    let end_ms = time_of_day_ms(end).map_err(|error| {
                        format!("交易日历 trading_days[{day_index}] session 非法: {error}")
                    })?;
                    if start_ms >= end_ms {
                        return Err(format!(
                            "交易日历 sessions[{session_index}] 必须满足 start < end"
                        ));
                    }
                    session_windows
                        .push([day.saturating_add(start_ms), day.saturating_add(end_ms)]);
                }
            }
        }
        self.trading_days = trading_days;
        self.session_windows = session_windows;
        self.validate()?;
        Ok(AshareCalendarLoadReport {
            schema_version,
            source,
            calendar_id,
            trading_days: self.trading_days.len(),
            sessions: self.session_windows.len(),
        })
    }

    /// 将统一数据层公司行为转换成当前 A 股撮合/账本可以安全处理的规则事件。
    ///
    /// 现金分红、送股和转增可以映射到现有 Ledger；配股、增发、回购、可转债
    /// 等需要权利/资金/独立标的账本，必须显式返回错误，禁止降级成拆股。
    pub fn corporate_actions_from_data(
        instrument: &str,
        actions: &[qx_data::CorporateAction],
    ) -> Result<Vec<AshareCorporateActionEvent>, String> {
        if instrument.trim().is_empty() {
            return Err("A 股公司行为转换缺少 instrument".into());
        }
        let mut events = Vec::with_capacity(actions.len());
        for action in actions {
            if action.instrument != instrument {
                return Err(format!(
                    "公司行为 instrument 不匹配: expected={instrument} actual={}",
                    action.instrument
                ));
            }
            let mut event = AshareCorporateActionEvent {
                ts: action.timestamp,
                announcement_ts: action.published_at,
                record_ts: None,
                payment_ts: None,
                subscription_start_ts: None,
                subscription_end_ts: None,
                action_type: AshareCorporateActionType::CashDividend,
                cash_dividend_raw: 0,
                split_num: 1,
                split_den: 1,
                rights_issue_price_raw: 0,
                rights_issue_ratio_num: 0,
                rights_issue_ratio_den: 1,
                issue_price_raw: 0,
                conversion_price_raw: 0,
                conversion_ratio_num: 0,
                conversion_ratio_den: 1,
                rights_instrument: None,
                subscription_qty_raw: 0,
                rights_expiry_qty_raw: 0,
                repurchase_qty_raw: 0,
                repurchase_price_raw: 0,
                convertible_bond_instrument: None,
                conversion_target_instrument: None,
                conversion_qty_raw: 0,
                conversion_target_qty_raw: 0,
                interest_per_bond_raw: 0,
                settlement_qty_raw: 0,
                settlement_price_raw: 0,
                issuer_total_shares_raw: 0,
                issuer_free_float_shares_raw: None,
                source: action.source.clone(),
                // 统一数据层的 published_at 就是 PIT 可见时间，必须一路带到
                // 撮合层，否则 `as_of` 只能看到除权日而漏掉公告时间。
                published_at_ms: action.published_at,
                raw_payload: None,
            };
            match &action.action_type {
                qx_data::CorporateActionType::Dividend => {
                    event.action_type = AshareCorporateActionType::CashDividend;
                    event.cash_dividend_raw = action.value_raw;
                }
                qx_data::CorporateActionType::BonusShare => {
                    event.action_type = AshareCorporateActionType::BonusShare;
                    event.split_num = action.ratio_num;
                    event.split_den = action.ratio_den;
                }
                qx_data::CorporateActionType::CapitalTransfer => {
                    event.action_type = AshareCorporateActionType::CapitalTransfer;
                    event.split_num = action.ratio_num;
                    event.split_den = action.ratio_den;
                }
                qx_data::CorporateActionType::ConvertibleBondIssue => {
                    event.action_type = AshareCorporateActionType::ConvertibleBondIssue;
                    event.convertible_bond_instrument = Some(action.instrument.clone());
                    event.issue_price_raw = action.price_raw;
                    event.subscription_qty_raw = action.value_raw;
                }
                qx_data::CorporateActionType::ConvertibleBondInterest => {
                    event.action_type = AshareCorporateActionType::ConvertibleBondInterest;
                    event.convertible_bond_instrument = Some(action.instrument.clone());
                    event.interest_per_bond_raw = action.value_raw;
                }
                qx_data::CorporateActionType::ConvertibleBondRedemption => {
                    event.action_type = AshareCorporateActionType::ConvertibleBondRedemption;
                    event.convertible_bond_instrument = Some(action.instrument.clone());
                    event.settlement_qty_raw = action.secondary_value_raw;
                    event.settlement_price_raw = action.price_raw;
                }
                qx_data::CorporateActionType::ConvertibleBondCall => {
                    event.action_type = AshareCorporateActionType::ConvertibleBondCall;
                    event.convertible_bond_instrument = Some(action.instrument.clone());
                    event.settlement_qty_raw = action.secondary_value_raw;
                    event.settlement_price_raw = action.price_raw;
                }
                qx_data::CorporateActionType::ConvertibleBondPut => {
                    event.action_type = AshareCorporateActionType::ConvertibleBondPut;
                    event.convertible_bond_instrument = Some(action.instrument.clone());
                    event.settlement_qty_raw = action.secondary_value_raw;
                    event.settlement_price_raw = action.price_raw;
                }
                qx_data::CorporateActionType::CapitalChange => {
                    event.action_type = AshareCorporateActionType::CapitalChange;
                    // qx-data 的 CapitalChange 约定 value_raw 为绝对总股本，
                    // secondary_value_raw 为可选绝对流通股本。
                    event.issuer_total_shares_raw = action.value_raw;
                    event.issuer_free_float_shares_raw =
                        (action.secondary_value_raw > 0).then_some(action.secondary_value_raw);
                }
                unsupported => {
                    return Err(format!(
                        "A 股公司行为 {:?} 需要专用权利/资金/独立标的账本，当前拒绝转换",
                        unsupported
                    ));
                }
            }
            events.push(event);
        }
        events.sort_by_key(|event| event.ts);
        Ok(events)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.lot_size <= 0 || self.price_tick <= 0 || self.min_commission < 0 {
            return Err("A 股 lot_size/price_tick 必须为正，min_commission 不能为负".into());
        }
        if self.limit_up_bp <= 0
            || self.limit_down_bp <= 0
            || self.limit_up_bp > 10_000
            || self.limit_down_bp > 10_000
        {
            return Err("A 股涨跌停基点必须在 1..=10000".into());
        }
        if self.commission_bp < 0 || self.stamp_duty_bp < 0 || self.transfer_fee_bp < 0 {
            return Err("A 股费用基点不能为负".into());
        }
        if self.trading_timestamps.windows(2).any(|w| w[0] >= w[1])
            || self.trading_days.windows(2).any(|w| w[0] >= w[1])
            || self.halted_timestamps.windows(2).any(|w| w[0] >= w[1])
        {
            return Err("A 股交易日/停牌时间戳必须严格递增".into());
        }
        if self
            .session_windows
            .iter()
            .any(|window| window[0] >= window[1])
        {
            return Err("A 股交易时段必须使用 [start,end) 且 start < end".into());
        }
        if self.previous_close_raw.values().any(|value| *value <= 0) {
            return Err("A 股前收盘价必须为正".into());
        }
        for event in &self.corporate_actions {
            if event.announcement_ts == Some(0)
                || event.record_ts == Some(0)
                || event.payment_ts == Some(0)
                || event.subscription_start_ts == Some(0)
                || event.subscription_end_ts == Some(0)
            {
                return Err("A 股公司行为日期时间戳不能为零".into());
            }
            if event
                .record_ts
                .is_some_and(|record_ts| record_ts > event.ts)
            {
                return Err("配股 record_ts 不能晚于除权日 ts".into());
            }
            if event
                .subscription_start_ts
                .is_some_and(|start_ts| start_ts < event.ts)
            {
                return Err("配股 subscription_start_ts 不能早于除权日 ts".into());
            }
            if event
                .subscription_end_ts
                .is_some_and(|end_ts| end_ts < event.subscription_start_ts.unwrap_or(event.ts))
            {
                return Err("配股 subscription_end_ts 不能早于认购开始日".into());
            }
            if event
                .payment_ts
                .is_some_and(|payment_ts| payment_ts < event.ts)
            {
                return Err("公司行为 payment_ts 不能早于除权日 ts".into());
            }
        }
        if self.corporate_actions.windows(2).any(|w| w[0].ts > w[1].ts)
            || self.corporate_actions.iter().any(|event| {
                let common_invalid = event.ts == 0
                    || event.cash_dividend_raw < 0
                    || event.split_num <= 0
                    || event.split_den <= 0
                    || event.rights_issue_price_raw < 0
                    || event.rights_issue_ratio_num < 0
                    || event.rights_issue_ratio_den <= 0
                    || event.issue_price_raw < 0
                    || event.conversion_price_raw < 0
                    || event.conversion_ratio_num < 0
                    || event.conversion_ratio_den <= 0
                    || event.interest_per_bond_raw < 0
                    || event.settlement_qty_raw < 0
                    || event.settlement_price_raw < 0
                    || event.issuer_total_shares_raw < 0
                    || event
                        .issuer_free_float_shares_raw
                        .is_some_and(|value| value < 0);
                let instruction_invalid = match event.action_type {
                    AshareCorporateActionType::RightsIssue => {
                        event
                            .rights_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event.rights_issue_price_raw <= 0
                            || event.rights_issue_ratio_num <= 0
                            || event.subscription_qty_raw < 0
                    }
                    AshareCorporateActionType::RightsIssueExpiry => {
                        event
                            .rights_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event.rights_expiry_qty_raw <= 0
                    }
                    AshareCorporateActionType::NewShareIssue => {
                        event.issue_price_raw <= 0 || event.subscription_qty_raw <= 0
                    }
                    AshareCorporateActionType::Repurchase => {
                        event.repurchase_qty_raw <= 0 || event.repurchase_price_raw <= 0
                    }
                    AshareCorporateActionType::ConvertibleBondIssue => {
                        event
                            .convertible_bond_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event.issue_price_raw <= 0
                            || event.subscription_qty_raw <= 0
                    }
                    AshareCorporateActionType::ConvertibleBondInterest => {
                        event
                            .convertible_bond_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event.interest_per_bond_raw <= 0
                    }
                    AshareCorporateActionType::ConvertibleBondRedemption
                    | AshareCorporateActionType::ConvertibleBondPut => {
                        event
                            .convertible_bond_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event.settlement_qty_raw <= 0
                            || event.settlement_price_raw <= 0
                    }
                    AshareCorporateActionType::ConvertibleBondCall => {
                        event
                            .convertible_bond_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event.settlement_price_raw <= 0
                    }
                    AshareCorporateActionType::ConvertibleBondConversion => {
                        event
                            .convertible_bond_instrument
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .is_empty()
                            || event
                                .conversion_target_instrument
                                .as_deref()
                                .unwrap_or("")
                                .trim()
                                .is_empty()
                            || event.conversion_qty_raw <= 0
                            || event.conversion_target_qty_raw <= 0
                            || event.conversion_price_raw <= 0
                    }
                    AshareCorporateActionType::CapitalChange => {
                        event.issuer_total_shares_raw <= 0
                            || event
                                .issuer_free_float_shares_raw
                                .is_some_and(|free_float| {
                                    free_float <= 0 || free_float > event.issuer_total_shares_raw
                                })
                    }
                    _ => false,
                };
                common_invalid || instruction_invalid
            })
        {
            return Err("A 股公司行为参数非法或缺少显式参与事实".into());
        }
        Ok(())
    }

    /// 返回当前回测账本已实现的账户级公司行为。发行人层面的可转债发行、
    /// 停牌和资本变更不会被伪造为账户持仓变化，仍由专门数据事实处理。
    pub fn corporate_action_supported_by_ledger(action_type: AshareCorporateActionType) -> bool {
        matches!(
            action_type,
            AshareCorporateActionType::CashDividend
                | AshareCorporateActionType::BonusShare
                | AshareCorporateActionType::CapitalTransfer
                | AshareCorporateActionType::RightsIssue
                | AshareCorporateActionType::RightsIssueExpiry
                | AshareCorporateActionType::NewShareIssue
                | AshareCorporateActionType::Repurchase
                | AshareCorporateActionType::ConvertibleBondIssue
                | AshareCorporateActionType::ConvertibleBondInterest
                | AshareCorporateActionType::ConvertibleBondRedemption
                | AshareCorporateActionType::ConvertibleBondCall
                | AshareCorporateActionType::ConvertibleBondPut
                | AshareCorporateActionType::ConvertibleBondConversion
                | AshareCorporateActionType::Unknown
        )
    }

    /// 提取指定标的在本次规则快照中的发行人股本事实。
    pub fn issuer_capital_snapshots(
        &self,
        instrument: &str,
        until_ts: Option<u64>,
    ) -> Result<Vec<AshareIssuerCapitalSnapshot>, String> {
        if instrument.trim().is_empty() {
            return Err("发行人股本快照缺少 instrument".into());
        }
        self.corporate_actions
            .iter()
            .filter(|event| {
                event.action_type == AshareCorporateActionType::CapitalChange
                    && until_ts.is_none_or(|until| event.ts <= until)
            })
            .map(|event| {
                if event.issuer_total_shares_raw <= 0 {
                    return Err("资本变更缺少有效 issuer_total_shares_raw".into());
                }
                if event
                    .issuer_free_float_shares_raw
                    .is_some_and(|free_float| {
                        free_float <= 0 || free_float > event.issuer_total_shares_raw
                    })
                {
                    return Err("资本变更 issuer_free_float_shares_raw 超出总股本".into());
                }
                Ok(AshareIssuerCapitalSnapshot {
                    instrument: instrument.to_owned(),
                    effective_ts: event.ts,
                    total_shares_raw: event.issuer_total_shares_raw,
                    free_float_shares_raw: event.issuer_free_float_shares_raw,
                    source: event.source.clone(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
