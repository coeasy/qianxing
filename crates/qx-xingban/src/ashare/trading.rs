//! A 股交易规则与 T+1 结算状态：可交易时段、涨跌停、整手与卖单校验、封板判定。
//!
//! 从 `ashare.rs` 纯搬家拆出：`AshareRuleConfig` 的公司行为/日历装载与 PIT 闸门
//! 留在父模块，这里是下单/成交前的市场约束侧。

use super::*;

/// 未声明涨跌停带时的 serde 默认。0 是"按板块推导"的哨兵，生效值见
/// [`Self::effective_limit_up_bp`]。这里曾直接返回 1_000，于是 `board: ChiNext` 而省略
/// `limit_*_bp` 的规则会按 ±10% 封板——比真实的 ±20% 窄，合法成交被当成涨停拒掉。
pub(crate) fn unspecified_limit_bp() -> i64 {
    0
}

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

    /// 涨跌停的锚：**上一交易日**的收盘价，不是上一根 Bar 的收盘价。
    ///
    /// 日线数据上两者重合，分钟线上"上一根 5 分钟的收价"与昨收差着一整个交易日 —— 拿它当锚
    /// 会把 ±10% 的板窄化成"最近 5 分钟 ±10%"，于是涨停封死的分钟线照样成交、跌停封死的照样卖。
    /// 因此这里向前找到第一根跨日的 Bar：行情按时间升序进来时，它就是上一交易日的最后一根。
    ///
    /// 找不到（该标的第一个交易日）时返回 `None`，`blocks_fill` 因此不判板 —— 宁可不判，也不能
    /// 拿当天的价格当昨收，那会把板算成 ±0%。
    ///
    /// `previous_close_raw` 按当前 Bar 的 ts 覆盖这一推导，它是数据侧手工指定锚点的出口。
    /// 没有覆盖时，除权除息日的锚由**已装载的公司行为**折算（[`Self::ex_rights_reference`]）；
    /// 当日没有可折算的事实时，这里给的就是**不复权**的原始昨收。
    pub fn previous_close(&self, bars: &[Bar], index: usize) -> Option<i128> {
        let current = bars.get(index)?;
        if let Some(close) = self.previous_close_raw.get(&current.ts).copied() {
            return Some(close);
        }
        let day = Self::day_key(current.ts);
        let raw = bars[..index]
            .iter()
            .rev()
            .find(|bar| Self::day_key(bar.ts) != day)
            .map(|bar| bar.close)?;
        Some(self.ex_rights_reference(raw, current.ts))
    }

    /// 除权除息参考价（沪深口径）：
    /// `(昨收 + 配股价×配股比例 − 每股现金红利) ÷ (1 + 送转比例 + 配股比例)`，再落到最小报价单位。
    ///
    /// 折算只读账本会记账的那批事实：现金红利与送转股比例取
    /// [`crate::backtest::is_cash_dividend_action`] 覆盖的动作（与 `apply_corporate_action`
    /// 的入账口径同一份名单），配股取 `RightsIssue` 的价格与比例。因此板锚不会和现金流
    /// 因为"同一个事件两种读法"而分叉。
    ///
    /// 当日没有相关事件、或数据把参考价压到非正（脏数据）时，原样返回昨收 —— 宁可不折算，
    /// 也不能把板算成 ±0%。
    fn ex_rights_reference(&self, previous_close: i128, anchor_ts: u64) -> i128 {
        let ex_date = Self::day_key(anchor_ts)
            .saturating_mul(DAY_MS)
            .saturating_sub(SHANGHAI_OFFSET_MS);
        let mut cash_out = 0_i128;
        let mut cash_in = 0_i128;
        // 定点 SCALE 的"1 股折算成多少股"，即 1 + 送转比例 + 配股比例。
        let mut share_factor = SCALE;
        for event in self
            .corporate_actions
            .iter()
            .filter(|event| event.ts == ex_date)
        {
            if crate::backtest::is_cash_dividend_action(event.action_type) {
                cash_out = cash_out.saturating_add(event.cash_dividend_raw);
                if event.split_num != event.split_den {
                    share_factor = share_factor.saturating_add(
                        event
                            .split_num
                            .saturating_sub(event.split_den)
                            .saturating_mul(SCALE)
                            .saturating_div(event.split_den),
                    );
                }
            } else if event.action_type == AshareCorporateActionType::RightsIssue
                && event.rights_issue_ratio_num > 0
            {
                let ratio_scaled = event
                    .rights_issue_ratio_num
                    .saturating_mul(SCALE)
                    .saturating_div(event.rights_issue_ratio_den);
                share_factor = share_factor.saturating_add(ratio_scaled);
                cash_in = cash_in.saturating_add(
                    event.rights_issue_price_raw.saturating_mul(ratio_scaled) / SCALE,
                );
            }
        }
        if cash_out == 0 && cash_in == 0 && share_factor == SCALE {
            return previous_close;
        }
        let numerator = previous_close
            .saturating_add(cash_in)
            .saturating_sub(cash_out);
        if numerator <= 0 || share_factor <= 0 {
            return previous_close;
        }
        let adjusted = numerator.saturating_mul(SCALE).saturating_div(share_factor);
        (adjusted + self.price_tick / 2) / self.price_tick * self.price_tick
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
