//! A 股交易规则与 T+1 结算状态：可交易时段、涨跌停、整手与卖单校验、封板判定。
//!
//! 从 `ashare.rs` 纯搬家拆出：`AshareRuleConfig` 的公司行为/日历装载与 PIT 闸门
//! 留在父模块，这里是下单/成交前的市场约束侧。

use super::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
#[path = "trading/previous_close.rs"]
mod previous_close;

/// Timestamp index for A-share actions used by the Bar loop. Each action is stored once per
/// trigger timestamp, in source order, so the engine can preserve its original event ordering.
pub(crate) struct AshareCorporateActionSchedule {
    by_timestamp: HashMap<u64, Vec<usize>>,
    bar_timestamp_by_action_timestamp: HashMap<u64, u64>,
    explicit_rights_expiries: BTreeMap<u64, BTreeSet<String>>,
}

impl AshareCorporateActionSchedule {
    #[cfg(test)]
    pub(crate) fn new(actions: &[AshareCorporateActionEvent]) -> Self {
        Self::build(actions, None)
    }

    pub(crate) fn for_bars(actions: &[AshareCorporateActionEvent], bars: &[Bar]) -> Self {
        Self::build(actions, Some(bars))
    }

    pub(crate) fn has_events_at(&self, ts: u64) -> bool {
        self.by_timestamp.contains_key(&ts)
    }

    fn build(actions: &[AshareCorporateActionEvent], bars: Option<&[Bar]>) -> Self {
        let mut explicit_rights_expiries = BTreeMap::<u64, BTreeSet<String>>::new();
        let mut by_timestamp = HashMap::<u64, Vec<usize>>::new();
        let mut bar_timestamp_by_action_timestamp = HashMap::new();
        for (index, event) in actions.iter().enumerate() {
            let mut timestamps = [None; 5];
            let mut timestamp_count = 0;
            let mut scheduled_bar_timestamps = [None; 5];
            let mut scheduled_bar_timestamp_count = 0;
            for ts in [
                Some(event.ts),
                event.record_ts,
                event.payment_ts,
                event.subscription_start_ts,
                event.subscription_end_ts,
            ]
            .into_iter()
            .flatten()
            {
                if !timestamps[..timestamp_count].contains(&Some(ts)) {
                    timestamps[timestamp_count] = Some(ts);
                    timestamp_count += 1;
                }
            }
            for ts in timestamps.into_iter().take(timestamp_count).flatten() {
                let bar_ts = match bars {
                    Some(bars) => Self::first_bar_on_action_day(bars, ts),
                    None => Some(ts),
                };
                let Some(bar_ts) = bar_ts else {
                    continue;
                };
                bar_timestamp_by_action_timestamp
                    .entry(ts)
                    .or_insert(bar_ts);
                if !scheduled_bar_timestamps[..scheduled_bar_timestamp_count]
                    .contains(&Some(bar_ts))
                {
                    scheduled_bar_timestamps[scheduled_bar_timestamp_count] = Some(bar_ts);
                    scheduled_bar_timestamp_count += 1;
                    by_timestamp.entry(bar_ts).or_default().push(index);
                }
            }
            if event.action_type == AshareCorporateActionType::RightsIssueExpiry {
                if let (Some(bar_ts), Some(instrument)) = (
                    bar_timestamp_by_action_timestamp.get(&event.ts).copied(),
                    event.rights_instrument.as_ref(),
                ) {
                    explicit_rights_expiries
                        .entry(bar_ts)
                        .or_default()
                        .insert(instrument.clone());
                }
            }
        }
        Self {
            by_timestamp,
            bar_timestamp_by_action_timestamp,
            explicit_rights_expiries,
        }
    }

    fn first_bar_on_action_day(bars: &[Bar], action_ts: u64) -> Option<u64> {
        let index = bars.partition_point(|bar| bar.ts < action_ts);
        let bar = bars.get(index)?;
        (AshareRuleConfig::day_key(bar.ts) == AshareRuleConfig::day_key(action_ts))
            .then_some(bar.ts)
    }

    pub(crate) fn trigger_ts(&self, action_ts: Option<u64>) -> Option<u64> {
        action_ts.and_then(|ts| self.bar_timestamp_by_action_timestamp.get(&ts).copied())
    }

    pub(crate) fn events_at<'a>(
        &'a self,
        actions: &'a [AshareCorporateActionEvent],
        ts: u64,
    ) -> impl Iterator<Item = &'a AshareCorporateActionEvent> + 'a {
        self.by_timestamp
            .get(&ts)
            .into_iter()
            .flatten()
            .map(move |index| &actions[*index])
    }

    pub(crate) fn has_explicit_rights_expiry(
        &self,
        ts: u64,
        rights_instrument: Option<&str>,
    ) -> bool {
        rights_instrument.is_some_and(|instrument| {
            self.explicit_rights_expiries
                .get(&ts)
                .is_some_and(|instruments| instruments.contains(instrument))
        })
    }
}

/// Per-run union of calendar sessions, sorted and merged for logarithmic membership checks.
pub(crate) struct AshareSessionIndex {
    windows: Vec<[u64; 2]>,
}

/// 未声明涨跌停带时的 serde 默认。0 是"按板块推导"的哨兵，生效值见
/// [`Self::effective_limit_up_bp`]。这里曾直接返回 1_000，于是 `board: ChiNext` 而省略
/// `limit_*_bp` 的规则会按 ±10% 封板——比真实的 ±20% 窄，合法成交被当成涨停拒掉。
pub(crate) fn unspecified_limit_bp() -> i64 {
    0
}

impl AshareRuleConfig {
    pub fn is_trading(&self, ts: u64) -> bool {
        let in_session = self
            .session_windows
            .iter()
            .any(|window| window[0] <= ts && ts < window[1]);
        self.is_trading_with_session(ts, in_session)
    }

    fn is_trading_with_session(&self, ts: u64, in_session: bool) -> bool {
        let day_start = Self::day_key(ts)
            .saturating_mul(DAY_MS)
            .saturating_sub(SHANGHAI_OFFSET_MS);
        (self.trading_timestamps.is_empty() || self.trading_timestamps.binary_search(&ts).is_ok())
            && (self.trading_days.is_empty() || self.trading_days.binary_search(&day_start).is_ok())
            && (self.session_windows.is_empty() || ts == day_start || in_session)
            && self.halted_timestamps.binary_search(&ts).is_err()
    }

    pub(crate) fn bar_restrictions(
        &self,
        sessions: &AshareSessionIndex,
        bar: &Bar,
        previous_close: Option<i128>,
    ) -> (bool, bool, bool) {
        let trading = self.is_trading_with_session(bar.ts, sessions.contains(bar.ts));
        let block_buy = if self.session_windows.is_empty() {
            self.blocks_fill(Side::Buy, bar, previous_close)
        } else {
            self.blocks_fill_with_trading(Side::Buy, bar, previous_close, trading)
        };
        (
            trading,
            block_buy,
            self.blocks_fill_with_trading(Side::Sell, bar, previous_close, trading),
        )
    }

    pub fn day_key(ts: u64) -> u64 {
        ts.saturating_add(SHANGHAI_OFFSET_MS) / DAY_MS
    }

    /// 生效涨停带：配置未声明（0）时按板块表推导，声明值优先。
    pub fn effective_limit_up_bp(&self) -> i64 {
        if self.limit_up_bp == 0 {
            self.board.default_limit_bp()
        } else {
            self.limit_up_bp
        }
    }

    /// 生效跌停带，口径同 [`Self::effective_limit_up_bp`]。
    pub fn effective_limit_down_bp(&self) -> i64 {
        if self.limit_down_bp == 0 {
            self.board.default_limit_bp()
        } else {
            self.limit_down_bp
        }
    }

    pub fn limits(&self, previous_close: i128) -> (i128, i128) {
        let up = previous_close.saturating_mul(i128::from(10_000 + self.effective_limit_up_bp()))
            / 10_000;
        let down = previous_close
            .saturating_mul(i128::from(10_000 - self.effective_limit_down_bp()))
            / 10_000;
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
        self.blocks_fill_with_trading(side, bar, previous_close, self.is_trading(bar.ts))
    }

    fn blocks_fill_with_trading(
        &self,
        side: Side,
        bar: &Bar,
        previous_close: Option<i128>,
        trading: bool,
    ) -> bool {
        if !trading {
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
            self.effective_limit_up_bp(),
            self.effective_limit_down_bp(),
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

#[cfg(test)]
mod corporate_action_schedule_tests {
    use super::*;

    #[test]
    fn date_only_actions_resolve_to_the_first_bar_of_the_same_shanghai_day() {
        let midnight = super::super::date_to_shanghai_midnight_ms("2024-06-03").unwrap();
        let first_bar_ts = midnight + 9 * 60 * 60 * 1_000 + 30 * 60 * 1_000;
        let later_bar_ts = first_bar_ts + 60_000;
        let actions = [AshareCorporateActionEvent {
            ts: midnight,
            record_ts: Some(midnight),
            action_type: AshareCorporateActionType::CashDividend,
            ..AshareCorporateActionEvent::default()
        }];
        let bars = [
            Bar::new(first_bar_ts, 1, 1, 1, 1, 1),
            Bar::new(later_bar_ts, 1, 1, 1, 1, 1),
        ];
        let schedule = AshareCorporateActionSchedule::for_bars(&actions, &bars);

        assert_eq!(schedule.trigger_ts(Some(midnight)), Some(first_bar_ts));
        assert_eq!(schedule.events_at(&actions, first_bar_ts).count(), 1);
        assert_eq!(schedule.events_at(&actions, later_bar_ts).count(), 0);
        assert_eq!(
            schedule.trigger_ts(Some(midnight + 10 * 60 * 60 * 1_000)),
            None,
            "an intraday action after the final bar must not be pulled backward"
        );
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
