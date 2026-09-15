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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

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
    /// 可选的前收盘覆盖，key 为当前 bar ts；用于真实交易日历/公司行为快照。
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
    /// 输入可以是动作数组，也可以是 `{ "actions": [...] }`。转换只负责
    /// 规范化日期、比例和字段；复杂事件仍由回测账本的支持矩阵决定，不能
    /// 因为完成了 JSON 解析就被误当成已实现的资金/权利语义。
    pub fn apply_corporate_actions_json(
        &mut self,
        instrument: &str,
        payload: &str,
    ) -> Result<usize, String> {
        if instrument.trim().is_empty() {
            return Err("A 股公司行为 JSON 缺少 instrument".into());
        }
        let document: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| format!("公司行为 JSON 无效: {error}"))?;
        let actions = document
            .as_array()
            .or_else(|| {
                document
                    .get("actions")
                    .and_then(serde_json::Value::as_array)
            })
            .ok_or_else(|| "公司行为 JSON 必须是数组或包含 actions 数组的对象".to_string())?;
        let mut converted = Vec::with_capacity(actions.len());
        let mut halted = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let object = action
                .as_object()
                .ok_or_else(|| format!("公司行为 actions[{index}] 必须是对象"))?;
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
            let action_type = object
                .get("action_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let parsed_type = parse_json_action_type(action_type).ok_or_else(|| {
                format!("公司行为 actions[{index}] action_type 非法: {action_type}")
            })?;
            if parsed_type == AshareCorporateActionType::Suspension {
                halted.push(ts);
                continue;
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
                source: object
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .into(),
            });
        }
        converted.sort_by_key(|event| event.ts);
        halted.sort_unstable();
        halted.dedup();
        // 先在候选快照上校验，失败时不污染调用方已有的规则配置。
        let mut candidate = self.clone();
        candidate.corporate_actions.extend(converted);
        candidate.corporate_actions.sort_by_key(|event| event.ts);
        candidate.halted_timestamps.extend(halted);
        candidate.halted_timestamps.sort_unstable();
        candidate.halted_timestamps.dedup();
        candidate.validate()?;
        *self = candidate;
        Ok(actions.len())
    }

    /// 将 Python AshareTradingCalendar JSON 合并到规则快照。交易日使用
    /// Asia/Shanghai 午夜时间戳；如果日历包含 sessions，则为每个交易日展开
    /// [start, end) 时段。日线 Bar 的午夜时间戳仍视为该交易日有效。
    pub fn apply_calendar_json(&mut self, payload: &str) -> Result<usize, String> {
        let document: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| format!("交易日历 JSON 无效: {error}"))?;
        let days = document
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
        if let Some(sessions) = document
            .get("sessions")
            .and_then(serde_json::Value::as_array)
        {
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
        Ok(self.trading_days.len())
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

    pub fn is_trading(&self, ts: u64) -> bool {
        let day_start = Self::day_key(ts)
            .saturating_mul(DAY_MS)
            .saturating_sub(SHANGHAI_OFFSET_MS);
        (self.trading_timestamps.is_empty() || self.trading_timestamps.binary_search(&ts).is_ok())
            && (self.trading_days.is_empty() || self.trading_days.binary_search(&day_start).is_ok())
            && (self.session_windows.is_empty()
                || ts == day_start
                || self
                    .session_windows
                    .iter()
                    .any(|window| window[0] <= ts && ts < window[1]))
            && self.halted_timestamps.binary_search(&ts).is_err()
    }

    pub fn day_key(ts: u64) -> u64 {
        ts.saturating_add(SHANGHAI_OFFSET_MS) / DAY_MS
    }

    pub fn previous_close(&self, bars: &[Bar], index: usize) -> Option<i128> {
        let ts = bars.get(index)?.ts;
        self.previous_close_raw.get(&ts).copied().or_else(|| {
            index
                .checked_sub(1)
                .and_then(|previous| bars.get(previous).map(|bar| bar.close))
        })
    }

    pub fn limits(&self, previous_close: i128) -> (i128, i128) {
        let up = previous_close.saturating_mul(i128::from(10_000 + self.limit_up_bp)) / 10_000;
        let down = previous_close.saturating_mul(i128::from(10_000 - self.limit_down_bp)) / 10_000;
        (self.align_down(up), self.align_up(down))
    }

    fn align_down(&self, value: i128) -> i128 {
        value / self.price_tick * self.price_tick
    }

    fn align_up(&self, value: i128) -> i128 {
        (value + self.price_tick - 1) / self.price_tick * self.price_tick
    }

    pub fn validate_order(
        &self,
        order: &Order,
        position: i128,
        bought_today: i128,
        ts: u64,
    ) -> Result<(), String> {
        if !self.is_trading(ts) {
            return Err("A 股当前 bar 不在可交易日或处于停牌".into());
        }
        let qty = order.qty.raw();
        if qty <= 0 {
            return Err("A 股订单数量必须为正".into());
        }
        match order.side {
            Side::Buy => {
                if qty % self.lot_size != 0 {
                    return Err("A 股买入数量必须是 100 股整手".into());
                }
            }
            Side::Sell => {
                let available = if self.t_plus_one {
                    position.saturating_sub(bought_today)
                } else {
                    position
                };
                if qty > available {
                    return Err("A 股 T+1 可卖仓位不足".into());
                }
                if !self.allow_odd_lot_sell && qty % self.lot_size != 0 {
                    return Err("A 股卖出数量必须是 100 股整手".into());
                }
            }
        }
        Ok(())
    }

    /// 只有在当前 bar 以涨停/跌停封死时阻止对应方向成交；触及但有成交路径时，
    /// 仍允许按 bar 级保守模型成交。
    pub fn blocks_fill(&self, side: Side, bar: &Bar, previous_close: Option<i128>) -> bool {
        if !self.is_trading(bar.ts) {
            return true;
        }
        let Some(previous_close) = previous_close else {
            return false;
        };
        let (up, down) = self.limits(previous_close);
        match side {
            Side::Buy => bar.open >= up && bar.high <= up && bar.low >= up,
            Side::Sell => bar.open <= down && bar.high <= down && bar.low >= down,
        }
    }

    pub fn descriptor(&self) -> String {
        let action_bytes = serde_json::to_vec(&self.corporate_actions).unwrap_or_default();
        let mut action_hash = Fnv1a::new();
        action_hash.write_bytes(&action_bytes);
        format!(
            "AshareRules@v2[board={:?};t_plus_one={};lot_size={};limit_up_bp={};limit_down_bp={};price_tick={};commission_bp={};min_commission={};stamp_duty_bp={};transfer_fee_bp={};calendar={};trading_days={};sessions={};halted={};actions={};actions_hash={:016x}]",
            self.board,
            self.t_plus_one,
            self.lot_size,
            self.limit_up_bp,
            self.limit_down_bp,
            self.price_tick,
            self.commission_bp,
            self.min_commission,
            self.stamp_duty_bp,
            self.transfer_fee_bp,
            self.trading_timestamps.len(),
            self.trading_days.len(),
            self.session_windows.len(),
            self.halted_timestamps.len(),
            self.corporate_actions.len(),
            action_hash.finish()
        )
    }
}

fn json_i128(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<i128, String> {
    let Some(value) = object.get(field) else {
        return Ok(0);
    };
    if let Some(number) = value.as_i64() {
        return Ok(i128::from(number));
    }
    if let Some(text) = value.as_str() {
        return text
            .parse::<i128>()
            .map_err(|error| format!("{field} 必须是整数: {error}"));
    }
    Err(format!("{field} 必须是整数"))
}

fn json_optional_i128(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<i128>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    json_i128(object, field).map(Some)
}

fn json_optional_date(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .ok_or_else(|| format!("{field} 必须是 YYYY-MM-DD 日期字符串"))?;
    date_to_shanghai_midnight_ms(text)
        .map(Some)
        .map_err(|error| format!("{field} 非法: {error}"))
}

fn json_i128_or(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    default: i128,
) -> Result<i128, String> {
    if object.get(field).is_none() {
        Ok(default)
    } else {
        json_i128(object, field)
    }
}

fn parse_json_action_type(value: &str) -> Option<AshareCorporateActionType> {
    Some(match value {
        "cash_dividend" | "dividend" => AshareCorporateActionType::CashDividend,
        "bonus_share" => AshareCorporateActionType::BonusShare,
        "capital_transfer" => AshareCorporateActionType::CapitalTransfer,
        "rights_issue" => AshareCorporateActionType::RightsIssue,
        "rights_issue_expiry" | "rights_expiry" => AshareCorporateActionType::RightsIssueExpiry,
        "new_share_issue" => AshareCorporateActionType::NewShareIssue,
        "repurchase" => AshareCorporateActionType::Repurchase,
        "convertible_bond_issue" => AshareCorporateActionType::ConvertibleBondIssue,
        "convertible_bond_interest" | "bond_interest" => {
            AshareCorporateActionType::ConvertibleBondInterest
        }
        "convertible_bond_redemption" | "bond_redemption" => {
            AshareCorporateActionType::ConvertibleBondRedemption
        }
        "convertible_bond_call" | "bond_call" => AshareCorporateActionType::ConvertibleBondCall,
        "convertible_bond_put" | "bond_put" => AshareCorporateActionType::ConvertibleBondPut,
        "convertible_bond_conversion" => AshareCorporateActionType::ConvertibleBondConversion,
        "suspension" => AshareCorporateActionType::Suspension,
        "capital_change" => AshareCorporateActionType::CapitalChange,
        "unknown" => AshareCorporateActionType::Unknown,
        _ => return None,
    })
}

fn date_to_shanghai_midnight_ms(value: &str) -> Result<u64, String> {
    let date = value
        .get(..10)
        .filter(|date| {
            date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')
        })
        .ok_or_else(|| "必须是 YYYY-MM-DD".to_string())?;
    let mut parts = date.split('-');
    let year = parts
        .next()
        .ok_or_else(|| "缺少年份".to_string())?
        .parse::<i64>()
        .map_err(|_| "年份非法".to_string())?;
    let month = parts
        .next()
        .ok_or_else(|| "缺少月份".to_string())?
        .parse::<i64>()
        .map_err(|_| "月份非法".to_string())?;
    let day = parts
        .next()
        .ok_or_else(|| "缺少日期".to_string())?
        .parse::<i64>()
        .map_err(|_| "日期非法".to_string())?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if !(1..=12).contains(&month) || !(1..=days_in_month).contains(&day) {
        return Err("日期超出范围".into());
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year / 400
    } else {
        (adjusted_year - 399) / 400
    };
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    let timestamp = days_since_epoch
        .checked_mul(86_400_000)
        .and_then(|value| value.checked_sub(8 * 3_600_000))
        .ok_or_else(|| "日期时间戳溢出".to_string())?;
    u64::try_from(timestamp).map_err(|_| "日期必须不早于 Unix epoch".into())
}

fn time_of_day_ms(value: &str) -> Result<u64, String> {
    let mut parts = value.split(':');
    let hour = parts
        .next()
        .ok_or_else(|| "缺少小时".to_string())?
        .parse::<u64>()
        .map_err(|_| "小时非法".to_string())?;
    let minute = parts
        .next()
        .ok_or_else(|| "缺少分钟".to_string())?
        .parse::<u64>()
        .map_err(|_| "分钟非法".to_string())?;
    let second = parts
        .next()
        .map(|value| value.parse::<u64>().map_err(|_| "秒非法".to_string()))
        .transpose()?
        .unwrap_or(0);
    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err("时间超出范围".into());
    }
    Ok((hour * 3_600 + minute * 60 + second) * 1_000)
}

#[derive(Clone, Debug, Default)]
pub struct AshareSettlementState {
    day: Option<u64>,
    bought_today: i128,
}

impl AshareSettlementState {
    pub fn prepare(&mut self, ts: u64) {
        let day = AshareRuleConfig::day_key(ts);
        if self.day != Some(day) {
            self.day = Some(day);
            self.bought_today = 0;
        }
    }

    pub fn bought_today(&self) -> i128 {
        self.bought_today
    }

    pub fn on_buy(&mut self, qty: i128) {
        self.bought_today = self.bought_today.saturating_add(qty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, OrderStatus, Quantity};

    fn order(side: Side, qty: i128) -> Order {
        Order {
            client_id: 1,
            instrument: InstrumentId::parse("000001.SZSE").unwrap(),
            side,
            qty: Quantity::from_raw(qty),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }
    }

    #[test]
    fn t_plus_one_and_lot_rules_are_enforced() {
        let rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        rules.validate().unwrap();
        let lot = rules.lot_size;
        assert!(rules
            .validate_order(&order(Side::Buy, lot), 0, 0, 1)
            .is_ok());
        assert!(rules
            .validate_order(&order(Side::Buy, lot + 1), 0, 0, 1)
            .is_err());
        assert!(rules
            .validate_order(&order(Side::Sell, lot), lot, lot, 1)
            .is_err());
        assert!(rules
            .validate_order(&order(Side::Sell, lot), lot, 0, 1 + DAY_MS)
            .is_ok());
    }

    #[test]
    fn sealed_limit_and_halt_block_the_correct_side() {
        let rules = AshareRuleConfig {
            enabled: true,
            halted_timestamps: vec![2],
            ..AshareRuleConfig::default()
        };
        rules.validate().unwrap();
        let up = 10 * SCALE;
        let up_limit = rules.limits(up).0;
        let sealed = Bar::new(1, up_limit, up_limit, up_limit, up_limit, 1);
        assert!(rules.blocks_fill(Side::Buy, &sealed, Some(up)));
        assert!(!rules.blocks_fill(Side::Sell, &sealed, Some(up)));
        assert!(!rules.is_trading(2));
    }

    #[test]
    fn supported_data_actions_convert_and_complex_actions_fail_closed() {
        let supported = vec![qx_data::CorporateAction {
            instrument: "000001.SZSE".into(),
            timestamp: 1,
            action_type: qx_data::CorporateActionType::Dividend,
            value_raw: 5,
            secondary_value_raw: 0,
            ratio_num: 1,
            ratio_den: 1,
            price_raw: 0,
            published_at: None,
            effective_at: None,
            source: "test".into(),
        }];
        let events =
            AshareRuleConfig::corporate_actions_from_data("000001.SZSE", &supported).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cash_dividend_raw, 5);
        let complex = qx_data::CorporateAction {
            instrument: "000001.SZSE".into(),
            timestamp: 2,
            action_type: qx_data::CorporateActionType::RightsIssue,
            value_raw: 0,
            secondary_value_raw: 0,
            ratio_num: 1,
            ratio_den: 10,
            price_raw: 8,
            published_at: None,
            effective_at: None,
            source: "test".into(),
        };
        assert!(AshareRuleConfig::corporate_actions_from_data("000001.SZSE", &[complex]).is_err());
    }

    #[test]
    fn python_action_json_is_converted_with_suspension_and_pit_fields() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"cash_dividend","cash_dividend_raw":100000000,"published_at":"2024-05-20T09:00:00+08:00","source":"akshare"},
            {"instrument":"000001.SZSE","ex_date":"2024-06-04","action_type":"suspension","source":"akshare"}
        ]"#;
        assert_eq!(
            rules
                .apply_corporate_actions_json("000001.SZSE", payload)
                .unwrap(),
            2
        );
        assert_eq!(rules.corporate_actions.len(), 1);
        assert_eq!(rules.corporate_actions[0].cash_dividend_raw, 100_000_000);
        assert_eq!(rules.halted_timestamps.len(), 1);
        assert!(!rules.is_trading(rules.halted_timestamps[0]));
    }

    #[test]
    fn python_action_json_rejects_invalid_ratio_and_instrument() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let wrong_instrument =
            r#"[{"instrument":"600000.SSE","ex_date":"2024-06-03","action_type":"cash_dividend"}]"#;
        assert!(rules
            .apply_corporate_actions_json("000001.SZSE", wrong_instrument)
            .is_err());
        let invalid_ratio = r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"bonus_share","share_ratio_num":0}]"#;
        assert!(rules
            .apply_corporate_actions_json("000001.SZSE", invalid_ratio)
            .is_err());
    }

    #[test]
    fn capital_change_requires_explicit_issuer_snapshot() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"capital_change",
             "issuer_total_shares_raw":"1000000000000","issuer_free_float_shares_raw":"700000000000",
             "source":"exchange"}
        ]"#;
        rules
            .apply_corporate_actions_json("000001.SZSE", payload)
            .unwrap();
        let snapshots = rules.issuer_capital_snapshots("000001.SZSE", None).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].total_shares_raw, 1_000_000_000_000);
        assert_eq!(snapshots[0].free_float_shares_raw, Some(700_000_000_000));

        let missing_total = r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"capital_change"}]"#;
        assert!(rules
            .apply_corporate_actions_json("000001.SZSE", missing_total)
            .is_err());
        assert_eq!(rules.corporate_actions.len(), 1);
    }

    #[test]
    fn capital_change_from_data_maps_absolute_supply_fields() {
        let action = qx_data::CorporateAction {
            instrument: "000001.SZSE".into(),
            timestamp: 10,
            action_type: qx_data::CorporateActionType::CapitalChange,
            value_raw: 1_000,
            secondary_value_raw: 700,
            ratio_num: 1,
            ratio_den: 1,
            price_raw: 0,
            published_at: None,
            effective_at: None,
            source: "provider".into(),
        };
        let events =
            AshareRuleConfig::corporate_actions_from_data("000001.SZSE", &[action]).unwrap();
        assert_eq!(
            events[0].action_type,
            AshareCorporateActionType::CapitalChange
        );
        assert_eq!(events[0].issuer_total_shares_raw, 1_000);
        assert_eq!(events[0].issuer_free_float_shares_raw, Some(700));
    }

    #[test]
    fn python_rights_action_json_preserves_lifecycle_dates() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"[{"instrument":"000001.SZSE","ex_date":"2024-06-03","record_date":"2024-05-31","subscription_start":"2024-06-04","subscription_end":"2024-06-07","payment_date":"2024-06-10","action_type":"rights_issue","rights_instrument":"700001.SZSE","rights_issue_price_raw":5000000000,"rights_issue_ratio_num":20,"rights_issue_ratio_den":100,"subscription_qty_raw":10,"source":"akshare"}]"#;
        rules
            .apply_corporate_actions_json("000001.SZSE", payload)
            .unwrap();
        let event = &rules.corporate_actions[0];
        assert!(event.record_ts.is_some());
        assert!(event.subscription_start_ts.unwrap() > event.ts);
        assert!(event.subscription_end_ts.unwrap() > event.subscription_start_ts.unwrap());
        assert!(event.payment_ts.unwrap() > event.ts);
    }

    #[test]
    fn python_action_json_keeps_explicit_complex_instruction_fields() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"rights_issue",
             "rights_instrument":"000001.SZSE","rights_issue_price_raw":5000000000,
             "rights_issue_ratio_num":100000000,"rights_issue_ratio_den":1000000000,
             "subscription_qty_raw":10000000000}
        ]"#;
        rules
            .apply_corporate_actions_json("000001.SZSE", payload)
            .unwrap();
        let event = &rules.corporate_actions[0];
        assert_eq!(event.rights_instrument.as_deref(), Some("000001.SZSE"));
        assert_eq!(event.subscription_qty_raw, 10_000_000_000);
        assert!(AshareRuleConfig::corporate_action_supported_by_ledger(
            AshareCorporateActionType::RightsIssue
        ));
    }

    #[test]
    fn python_action_json_accepts_explicit_rights_expiry_quantity() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-10","action_type":"rights_issue_expiry",
             "rights_instrument":"700001.SZSE","rights_expiry_qty_raw":10000000000}
        ]"#;
        rules
            .apply_corporate_actions_json("000001.SZSE", payload)
            .unwrap();
        let event = &rules.corporate_actions[0];
        assert_eq!(
            event.action_type,
            AshareCorporateActionType::RightsIssueExpiry
        );
        assert_eq!(event.rights_expiry_qty_raw, 10_000_000_000);
    }

    #[test]
    fn python_action_json_accepts_convertible_bond_lifecycle_fields() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"[
            {"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"convertible_bond_issue",
             "convertible_bond_instrument":"123001.SZSE","issue_price_raw":100000000000,
             "subscription_qty_raw":100000000000},
            {"instrument":"000001.SZSE","ex_date":"2024-06-10","record_date":"2024-06-10",
             "payment_date":"2024-06-20","action_type":"convertible_bond_interest",
             "convertible_bond_instrument":"123001.SZSE","interest_per_bond_raw":5000000000},
            {"instrument":"000001.SZSE","ex_date":"2024-06-30","payment_date":"2024-06-30",
             "action_type":"convertible_bond_redemption","convertible_bond_instrument":"123001.SZSE",
             "settlement_qty_raw":100000000000,"settlement_price_raw":105000000000},
            {"instrument":"000001.SZSE","ex_date":"2024-07-31","payment_date":"2024-07-31",
             "action_type":"convertible_bond_call","convertible_bond_instrument":"123001.SZSE",
             "settlement_price_raw":106000000000}
        ]"#;
        assert_eq!(
            rules
                .apply_corporate_actions_json("000001.SZSE", payload)
                .unwrap(),
            4
        );
        assert_eq!(
            rules.corporate_actions[0].action_type,
            AshareCorporateActionType::ConvertibleBondIssue
        );
        assert_eq!(
            rules.corporate_actions[1].interest_per_bond_raw,
            5_000_000_000
        );
        assert_eq!(
            rules.corporate_actions[2].settlement_price_raw,
            105_000_000_000
        );
        assert_eq!(
            rules.corporate_actions[3].action_type,
            AshareCorporateActionType::ConvertibleBondCall
        );
        assert_eq!(rules.corporate_actions[3].settlement_qty_raw, 0);
        assert!(AshareRuleConfig::corporate_action_supported_by_ledger(
            AshareCorporateActionType::ConvertibleBondInterest
        ));
    }

    #[test]
    fn calendar_json_controls_daily_and_intraday_trading_windows() {
        let mut rules = AshareRuleConfig {
            enabled: true,
            ..AshareRuleConfig::default()
        };
        let payload = r#"{
            "calendar_id":"cn-2024",
            "trading_days":["2024-06-03"],
            "sessions":[["09:30","11:30"],["13:00","15:00"]]
        }"#;
        assert_eq!(rules.apply_calendar_json(payload).unwrap(), 1);
        let day = rules.trading_days[0];
        assert!(rules.is_trading(day));
        assert!(rules.is_trading(day + 9 * 3_600_000 + 30 * 60_000));
        assert!(!rules.is_trading(day + 12 * 3_600_000));
        assert!(!rules.is_trading(day + DAY_MS));
    }
}
