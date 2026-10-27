//! 上一交易日收盘锚、除权除息参考价与逐 Bar 昨收序列。

use super::*;

#[derive(Clone, Copy, Default)]
struct ExRightsAdjustment {
    cash_out: i128,
    cash_in: i128,
    share_factor: i128,
}

impl ExRightsAdjustment {
    fn include(&mut self, other: Self) -> Result<(), String> {
        let cash_out = self
            .cash_out
            .checked_add(other.cash_out)
            .ok_or_else(|| "除权除息现金红利合计溢出".to_string())?;
        let cash_in = self
            .cash_in
            .checked_add(other.cash_in)
            .ok_or_else(|| "除权除息配股现金合计溢出".to_string())?;
        let share_factor = self
            .share_factor
            .checked_add(other.share_factor)
            .ok_or_else(|| "除权除息股份比例合计溢出".to_string())?;
        *self = Self {
            cash_out,
            cash_in,
            share_factor,
        };
        Ok(())
    }
}

fn ex_rights_adjustment_for_event(
    event: &AshareCorporateActionEvent,
) -> Result<ExRightsAdjustment, String> {
    let mut adjustment = ExRightsAdjustment::default();
    if crate::backtest::is_cash_dividend_action(event.action_type) {
        adjustment.cash_out = event.cash_dividend_raw;
        if event.split_num != event.split_den {
            let difference = event
                .split_num
                .checked_sub(event.split_den)
                .ok_or_else(|| "除权除息股份比例差值溢出".to_string())?;
            adjustment.share_factor = if difference < 0 {
                checked_nonnegative_mul_div_floor(
                    difference
                        .checked_neg()
                        .ok_or_else(|| "除权除息股份比例取绝对值溢出".to_string())?,
                    SCALE,
                    event.split_den,
                )?
                .checked_neg()
                .ok_or_else(|| "除权除息股份比例取负值溢出".to_string())?
            } else {
                checked_nonnegative_mul_div_floor(difference, SCALE, event.split_den)?
            };
        }
    } else if event.action_type == AshareCorporateActionType::RightsIssue
        && event.rights_issue_ratio_num > 0
    {
        let ratio_scaled = checked_nonnegative_mul_div_floor(
            event.rights_issue_ratio_num,
            SCALE,
            event.rights_issue_ratio_den,
        )?;
        adjustment.share_factor = ratio_scaled;
        adjustment.cash_in =
            checked_nonnegative_mul_div_floor(event.rights_issue_price_raw, ratio_scaled, SCALE)?;
    }
    Ok(adjustment)
}

fn checked_nonnegative_mul_div_floor(
    value: i128,
    numerator: i128,
    denominator: i128,
) -> Result<i128, String> {
    if value < 0 || numerator < 0 || denominator <= 0 {
        return Err("除权除息比例计算输入非法".into());
    }
    let quotient = value / denominator;
    let remainder = value % denominator;
    let whole = quotient
        .checked_mul(numerator)
        .ok_or_else(|| "除权除息比例计算溢出".to_string())?;
    let partial = remainder
        .checked_mul(numerator)
        .ok_or_else(|| "除权除息比例余数计算溢出".to_string())?
        / denominator;
    whole
        .checked_add(partial)
        .ok_or_else(|| "除权除息比例累计溢出".into())
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
    pub fn previous_close(&self, bars: &[Bar], index: usize) -> Result<Option<i128>, String> {
        let Some(current) = bars.get(index) else {
            return Ok(None);
        };
        if let Some(close) = self.previous_close_raw.get(&current.ts).copied() {
            return Ok(Some(close));
        }
        let day = Self::day_key(current.ts);
        let Some(raw) = bars[..index]
            .iter()
            .rev()
            .find(|bar| Self::day_key(bar.ts) != day)
            .map(|bar| bar.close)
        else {
            return Ok(None);
        };
        Ok(Some(self.ex_rights_reference(raw, current.ts)?))
    }

    /// 为一段按时间排序的行情一次性计算昨收锚点。
    ///
    /// `previous_close` 适合零散查询；在回测循环里逐根调用会对每根分钟 Bar 反向扫描到上个
    /// 交易日，使长日内序列重复读取同一段历史。这里仅向前遍历行情一次并追踪交易日末收，
    /// 另对显式覆盖表按时间戳查询；覆盖与除权折算语义和单点查询一致。
    pub fn previous_close_series(&self, bars: &[Bar]) -> Result<Vec<Option<i128>>, String> {
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
            .map(|event| Ok((event.ts, ex_rights_adjustment_for_event(event)?)))
            .collect::<Result<Vec<_>, String>>()?;
        // Validation normally guarantees this order. Stable sorting also preserves the public
        // projection's behavior for callers that use it directly with an unvalidated snapshot.
        per_event.sort_by_key(|(ts, _)| *ts);
        let mut adjustments = Vec::<(u64, ExRightsAdjustment)>::with_capacity(per_event.len());
        for (ts, contribution) in per_event {
            if let Some((_, adjustment)) =
                adjustments.last_mut().filter(|(last_ts, _)| *last_ts == ts)
            {
                adjustment.include(contribution)?;
            } else {
                let mut adjustment = ExRightsAdjustment {
                    share_factor: SCALE,
                    ..ExRightsAdjustment::default()
                };
                adjustment.include(contribution)?;
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
                adjusted_previous_close = current_day
                    .and(last_close)
                    .map(|raw| {
                        let ex_date = day
                            .checked_mul(DAY_MS)
                            .and_then(|midnight| midnight.checked_sub(SHANGHAI_OFFSET_MS))
                            .ok_or_else(|| "除权日时间戳转换溢出".to_string())?;
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
                    })
                    .transpose()?;
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
        Ok(output)
    }

    fn previous_close_series_daily_scan(&self, bars: &[Bar]) -> Result<Vec<Option<i128>>, String> {
        let mut output = Vec::with_capacity(bars.len());
        let mut current_day = None;
        let mut adjusted_previous_close = None;
        let mut last_close = None;
        for bar in bars {
            let day = Self::day_key(bar.ts);
            if current_day != Some(day) {
                adjusted_previous_close = if current_day.is_some() {
                    last_close
                        .map(|raw| self.ex_rights_reference(raw, bar.ts))
                        .transpose()?
                } else {
                    None
                };
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
        Ok(output)
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
    fn ex_rights_reference(&self, previous_close: i128, anchor_ts: u64) -> Result<i128, String> {
        let ex_date = Self::day_key(anchor_ts)
            .checked_mul(DAY_MS)
            .and_then(|midnight| midnight.checked_sub(SHANGHAI_OFFSET_MS))
            .ok_or_else(|| "除权日时间戳转换溢出".to_string())?;
        let mut adjustment = ExRightsAdjustment {
            share_factor: SCALE,
            ..ExRightsAdjustment::default()
        };
        for event in self
            .corporate_actions
            .iter()
            .filter(|event| event.ts == ex_date)
        {
            adjustment.include(ex_rights_adjustment_for_event(event)?)?;
        }
        self.apply_ex_rights_adjustment(previous_close, adjustment)
    }

    fn apply_ex_rights_adjustment(
        &self,
        previous_close: i128,
        adjustment: ExRightsAdjustment,
    ) -> Result<i128, String> {
        if adjustment.cash_out == 0 && adjustment.cash_in == 0 && adjustment.share_factor == SCALE {
            return Ok(previous_close);
        }
        let numerator = previous_close
            .checked_add(adjustment.cash_in)
            .and_then(|value| value.checked_sub(adjustment.cash_out))
            .ok_or_else(|| "除权除息参考价现金流合计溢出".to_string())?;
        if numerator <= 0 {
            return Err("除权除息参考价必须为正数".into());
        }
        if adjustment.share_factor <= 0 {
            return Err("除权除息股份比例必须为正数".into());
        }
        if self.price_tick <= 0 {
            return Err("除权除息价格 tick 必须为正数".into());
        }
        let adjusted =
            checked_nonnegative_mul_div_floor(numerator, SCALE, adjustment.share_factor)?;
        let rounded_units = adjusted
            .checked_add(self.price_tick / 2)
            .ok_or_else(|| "除权除息参考价 tick 舍入溢出".to_string())?
            / self.price_tick;
        rounded_units
            .checked_mul(self.price_tick)
            .ok_or_else(|| "除权除息参考价 tick 对齐溢出".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_previous_close_series_rejects_corporate_action_sum_overflow() {
        let event = |cash_dividend_raw: i128| {
            serde_json::from_str::<AshareCorporateActionEvent>(&format!(
                r#"{{"ts":1,"action_type":"cash_dividend","cash_dividend_raw":{cash_dividend_raw}}}"#
            ))
            .unwrap()
        };
        let mut rules = AshareRuleConfig::default();
        rules.corporate_actions = (0..1_024)
            .map(|index| match index {
                0 => event(i128::MAX),
                1 => event(1),
                _ => event(0),
            })
            .collect();
        let bar = Bar::new(1, SCALE, SCALE, SCALE, SCALE, 1);
        let bars = vec![bar; 8_000];

        let error = rules.previous_close_series(&bars).unwrap_err();
        assert!(error.contains("现金红利合计溢出"), "{error}");
    }
}
