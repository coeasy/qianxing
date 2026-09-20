//! A 股交易规则与 T+1 结算状态：可交易时段、涨跌停、整手与卖单校验、封板判定。
//!
//! 从 `ashare.rs` 纯搬家拆出：`AshareRuleConfig` 的公司行为/日历装载与 PIT 闸门
//! 留在父模块，这里是下单/成交前的市场约束侧。

use super::*;

impl AshareRuleConfig {
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
