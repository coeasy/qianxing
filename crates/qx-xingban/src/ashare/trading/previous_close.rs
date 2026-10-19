//! 上一交易日收盘锚、除权除息参考价与逐 Bar 昨收序列。

use super::*;

#[derive(Clone, Copy, Default)]
struct ExRightsAdjustment {
    cash_out: i128,
    cash_in: i128,
    share_factor: i128,
}

impl ExRightsAdjustment {
    fn include(&mut self, other: Self) {
        self.cash_out = self.cash_out.saturating_add(other.cash_out);
        self.cash_in = self.cash_in.saturating_add(other.cash_in);
        self.share_factor = self.share_factor.saturating_add(other.share_factor);
    }
}

fn ex_rights_adjustment_for_event(event: &AshareCorporateActionEvent) -> ExRightsAdjustment {
    let mut adjustment = ExRightsAdjustment::default();
    if crate::backtest::is_cash_dividend_action(event.action_type) {
        adjustment.cash_out = event.cash_dividend_raw;
        if event.split_num != event.split_den {
            adjustment.share_factor = event
                .split_num
                .saturating_sub(event.split_den)
                .saturating_mul(SCALE)
                .saturating_div(event.split_den);
        }
    } else if event.action_type == AshareCorporateActionType::RightsIssue
        && event.rights_issue_ratio_num > 0
    {
        let ratio_scaled = event
            .rights_issue_ratio_num
            .saturating_mul(SCALE)
            .saturating_div(event.rights_issue_ratio_den);
        adjustment.share_factor = ratio_scaled;
        adjustment.cash_in = event.rights_issue_price_raw.saturating_mul(ratio_scaled) / SCALE;
    }
    adjustment
}

impl AshareSessionIndex {
    pub(crate) fn new(rules: Option<&AshareRuleConfig>) -> Self {
        let mut windows = rules.map_or_else(Vec::new, |rules| rules.session_windows.clone());
        windows.sort_unstable_by_key(|window| window[0]);
        let mut merged: Vec<[u64; 2]> = Vec::with_capacity(windows.len());
        for window in windows {
            if let Some(previous) = merged
                .last_mut()
                .filter(|previous| window[0] <= previous[1])
            {
                previous[1] = previous[1].max(window[1]);
            } else {
                merged.push(window);
            }
        }
        Self { windows: merged }
    }

    pub(crate) fn contains(&self, ts: u64) -> bool {
        let after = self.windows.partition_point(|window| window[0] <= ts);
        after
            .checked_sub(1)
            .is_some_and(|index| ts < self.windows[index][1])
    }
}

impl AshareRuleConfig {
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

    /// 为一段按时间排序的行情一次性计算昨收锚点。
    ///
    /// `previous_close` 适合零散查询；在回测循环里逐根调用会对每根分钟 Bar 反向扫描到上个
    /// 交易日，使长日内序列重复读取同一段历史。这里仅向前遍历行情一次并追踪交易日末收，
    /// 另对显式覆盖表按时间戳查询；覆盖与除权折算语义和单点查询一致。
    pub fn previous_close_series(&self, bars: &[Bar]) -> Vec<Option<i128>> {
        // Index construction only pays off when enough history and action rows amortize it.
        // Release measurements show the daily scan remains cheaper for short or sparse runs.
        if self.corporate_actions.len() < 1_024 || bars.len() < 8_000 {
            return self.previous_close_series_daily_scan(bars);
        }

        // Build the ex-date adjustment once. The former per-day call to
        // `ex_rights_reference` scanned every corporate action, making the
        // preparation pass O(days * actions) for long A-share histories.
        let mut per_event = self
            .corporate_actions
            .iter()
            .map(|event| (event.ts, ex_rights_adjustment_for_event(event)))
            .collect::<Vec<_>>();
        // Validation normally guarantees this order. Stable sorting also preserves the public
        // projection's behavior for callers that use it directly with an unvalidated snapshot.
        per_event.sort_by_key(|(ts, _)| *ts);
        let mut adjustments = Vec::<(u64, ExRightsAdjustment)>::with_capacity(per_event.len());
        for (ts, contribution) in per_event {
            if let Some((_, adjustment)) =
                adjustments.last_mut().filter(|(last_ts, _)| *last_ts == ts)
            {
                adjustment.include(contribution);
            } else {
                let mut adjustment = ExRightsAdjustment {
                    share_factor: SCALE,
                    ..ExRightsAdjustment::default()
                };
                adjustment.include(contribution);
                adjustments.push((ts, adjustment));
            }
        }

        let mut output = Vec::with_capacity(bars.len());
        let mut current_day = None;
        let mut adjusted_previous_close = None;
        let mut last_close = None;
        let mut adjustment_index = 0;

        for bar in bars {
            let day = Self::day_key(bar.ts);
            if current_day != Some(day) {
                adjusted_previous_close = current_day.and(last_close).map(|raw| {
                    let ex_date = day
                        .saturating_mul(DAY_MS)
                        .saturating_sub(SHANGHAI_OFFSET_MS);
                    while adjustments
                        .get(adjustment_index)
                        .is_some_and(|(ts, _)| *ts < ex_date)
                    {
                        adjustment_index += 1;
                    }
                    let adjustment = adjustments
                        .get(adjustment_index)
                        .filter(|(ts, _)| *ts == ex_date)
                        .map(|(_, adjustment)| *adjustment)
                        .unwrap_or_default();
                    self.apply_ex_rights_adjustment(raw, adjustment)
                });
                current_day = Some(day);
            }

            let close = self
                .previous_close_raw
                .get(&bar.ts)
                .copied()
                .or(adjusted_previous_close);
            output.push(close);
            last_close = Some(bar.close);
        }
        output
    }

    fn previous_close_series_daily_scan(&self, bars: &[Bar]) -> Vec<Option<i128>> {
        let mut output = Vec::with_capacity(bars.len());
        let mut current_day = None;
        let mut adjusted_previous_close = None;
        let mut last_close = None;
        for bar in bars {
            let day = Self::day_key(bar.ts);
            if current_day != Some(day) {
                adjusted_previous_close = current_day
                    .and(last_close)
                    .map(|raw| self.ex_rights_reference(raw, bar.ts));
                current_day = Some(day);
            }
            output.push(
                self.previous_close_raw
                    .get(&bar.ts)
                    .copied()
                    .or(adjusted_previous_close),
            );
            last_close = Some(bar.close);
        }
        output
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
        let mut adjustment = ExRightsAdjustment {
            share_factor: SCALE,
            ..ExRightsAdjustment::default()
        };
        for event in self
            .corporate_actions
            .iter()
            .filter(|event| event.ts == ex_date)
        {
            adjustment.include(ex_rights_adjustment_for_event(event));
        }
        self.apply_ex_rights_adjustment(previous_close, adjustment)
    }

    fn apply_ex_rights_adjustment(
        &self,
        previous_close: i128,
        adjustment: ExRightsAdjustment,
    ) -> i128 {
        if adjustment.cash_out == 0 && adjustment.cash_in == 0 && adjustment.share_factor == SCALE {
            return previous_close;
        }
        let numerator = previous_close
            .saturating_add(adjustment.cash_in)
            .saturating_sub(adjustment.cash_out);
        if numerator <= 0 || adjustment.share_factor <= 0 {
            return previous_close;
        }
        let adjusted = numerator
            .saturating_mul(SCALE)
            .saturating_div(adjustment.share_factor);
        (adjusted + self.price_tick / 2) / self.price_tick * self.price_tick
    }
}
