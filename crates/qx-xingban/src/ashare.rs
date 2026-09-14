//! A 股 Bar 回测规则：交易日、T+1、整手、涨跌停、停牌和费用参数。
//!
//! 规则与数据源解耦。数据源只提供 BarFrame；本模块决定该 Bar 是否可交易、
//! 订单是否合规以及成交后哪些仓位在当日仍不可卖出。

use qx_core::{Order, Side, SCALE};
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
    NewShareIssue,
    Repurchase,
    ConvertibleBondIssue,
    ConvertibleBondConversion,
    Suspension,
    CapitalChange,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AshareCorporateActionEvent {
    pub ts: u64,
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
    #[serde(default)]
    pub source: String,
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
            session_windows: Vec::new(),
            halted_timestamps: Vec::new(),
            previous_close_raw: BTreeMap::new(),
            corporate_actions: Vec::new(),
        }
    }
}

impl AshareRuleConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
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
        if self.corporate_actions.windows(2).any(|w| w[0].ts > w[1].ts)
            || self.corporate_actions.iter().any(|event| {
                event.ts == 0
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
            })
        {
            return Err("A 股公司行为必须按 ts 非递减，分红不能为负，拆股比例必须为正".into());
        }
        Ok(())
    }

    /// 当前 Ledger 能安全处理的公司行为只有现金分红和股份比例变动。
    /// 其他事件必须等资金、权利和独立标的账本实现后再进入回测。
    pub fn corporate_action_supported_by_ledger(action_type: AshareCorporateActionType) -> bool {
        matches!(
            action_type,
            AshareCorporateActionType::CashDividend
                | AshareCorporateActionType::BonusShare
                | AshareCorporateActionType::CapitalTransfer
                | AshareCorporateActionType::Unknown
        )
    }

    pub fn is_trading(&self, ts: u64) -> bool {
        (self.trading_timestamps.is_empty() || self.trading_timestamps.binary_search(&ts).is_ok())
            && (self.session_windows.is_empty()
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
        format!(
            "AshareRules@v1[board={:?};t_plus_one={};lot_size={};limit_up_bp={};limit_down_bp={};price_tick={};commission_bp={};min_commission={};stamp_duty_bp={};transfer_fee_bp={};calendar={};sessions={};halted={}]",
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
            self.session_windows.len(),
            self.halted_timestamps.len()
        )
    }
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
}
