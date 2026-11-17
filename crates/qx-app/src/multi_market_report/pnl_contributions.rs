//! Verification of per-instrument equity attribution against report curves and replayed ledger.

use super::{
    invalid, raw_i128, MultiMarketBacktestSpec, VerifiedMultiMarketPnlContributions,
    VerifiedPnlContributionSample, MAX_MULTI_MARKET_PNL_SAMPLES,
};
use crate::AppError;
use qx_core::{
    DecimalRounding, ExactDecimal, ExactValuationContext, InstrumentId, Ledger, Price,
    TradingInstrumentSpec, ValuationContext,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub fn verify_multi_market_pnl_contributions_v1(
    sidecar: &Value,
    equity_rows: &[Value],
    spec: &MultiMarketBacktestSpec,
    ledger: &Ledger,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    final_market_marks: &BTreeMap<InstrumentId, Price>,
) -> Result<VerifiedMultiMarketPnlContributions, AppError> {
    verify_multi_market_pnl_contributions_with_exact_v1(
        sidecar,
        equity_rows,
        None,
        spec,
        ledger,
        market_specs,
        final_market_marks,
    )
}

pub fn verify_multi_market_pnl_contributions_with_exact_v1(
    sidecar: &Value,
    equity_rows: &[Value],
    exact_equity_rows: Option<&[Value]>,
    spec: &MultiMarketBacktestSpec,
    ledger: &Ledger,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    final_market_marks: &BTreeMap<InstrumentId, Price>,
) -> Result<VerifiedMultiMarketPnlContributions, AppError> {
    if equity_rows.is_empty() {
        return Err(invalid("共享权益曲线为空，不能校验逐标的权益贡献"));
    }
    let total_equity_points =
        u64::try_from(equity_rows.len()).map_err(|_| invalid("共享权益点数超出 u64 范围"))?;
    if exact_equity_rows.is_some_and(|rows| rows.len() != equity_rows.len()) {
        return Err(invalid("exact 权益 sidecar 点数与 raw 权益曲线不一致"));
    }
    let exact_equity_values = exact_equity_rows
        .map(|rows| {
            rows.iter()
                .zip(equity_rows)
                .enumerate()
                .map(|(index, (row, raw_row))| {
                    let fields = row
                        .as_object()
                        .ok_or_else(|| invalid("exact 权益采样点必须是对象"))?;
                    let equity_index = fields
                        .get("equity_index")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| invalid("exact 权益索引无效"))?;
                    let ts = fields
                        .get("ts")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| invalid("exact 权益时间戳无效"))?;
                    let amount = fields
                        .get("equity_exact")
                        .and_then(Value::as_str)
                        .map(ExactDecimal::parse)
                        .transpose()
                        .map_err(|error| invalid(format!("exact 权益金额无效: {error}")))?
                        .ok_or_else(|| invalid("exact 权益金额缺失"))?;
                    if fields.len() != 3
                        || equity_index != index as u64
                        || raw_row.get(0).and_then(Value::as_u64) != Some(ts)
                    {
                        return Err(invalid("exact 权益 sidecar 索引或时间轴无效"));
                    }
                    Ok(amount)
                })
                .collect::<Result<Vec<_>, AppError>>()
        })
        .transpose()?;
    let last_equity_index = total_equity_points - 1;
    let expected_instruments = spec
        .markets
        .iter()
        .map(|market| market.instrument.clone())
        .collect::<BTreeSet<_>>();
    if expected_instruments.len() != spec.markets.len() {
        return Err(invalid("多市场规格包含重复标的，不能校验权益贡献"));
    }
    let sidecar = sidecar
        .as_object()
        .ok_or_else(|| invalid("逐标的权益贡献必须是对象"))?;
    let sidecar_instruments = sidecar.keys().cloned().collect::<BTreeSet<_>>();
    if sidecar_instruments != expected_instruments {
        return Err(invalid("逐标的权益贡献标的集合与市场输入不一致"));
    }

    let mut curves = BTreeMap::<String, Vec<VerifiedPnlContributionSample>>::new();
    for instrument in &expected_instruments {
        let rows = sidecar
            .get(instrument)
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("{instrument} 权益贡献必须是数组")))?;
        if rows.is_empty() || rows.len() > MAX_MULTI_MARKET_PNL_SAMPLES {
            return Err(invalid(format!("{instrument} 权益贡献采样点数无效")));
        }
        let mut curve = Vec::with_capacity(rows.len());
        for row in rows {
            let equity_index = row
                .get("equity_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(format!("{instrument} 权益贡献索引无效")))?;
            let ts = row
                .get("ts")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid(format!("{instrument} 权益贡献时间戳无效")))?;
            let pnl_contribution_raw = row
                .get("pnl_contribution_raw")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i128>().ok())
                .ok_or_else(|| invalid(format!("{instrument} 权益贡献金额无效")))?;
            let pnl_contribution_exact = row
                .get("pnl_contribution_exact")
                .and_then(Value::as_str)
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| invalid(format!("{instrument} 精确权益贡献金额无效: {error}")))?;
            let expected_field_count = if pnl_contribution_exact.is_some() {
                4
            } else {
                3
            };
            if row.as_object().is_none_or(|fields| {
                fields.len() != expected_field_count
                    || !fields.contains_key("equity_index")
                    || !fields.contains_key("ts")
                    || !fields.contains_key("pnl_contribution_raw")
                    || fields.contains_key("pnl_contribution_exact")
                        != pnl_contribution_exact.is_some()
                    || (exact_equity_rows.is_some() && pnl_contribution_exact.is_none())
                    || (exact_equity_rows.is_none() && pnl_contribution_exact.is_some())
            }) {
                return Err(invalid(format!("{instrument} 权益贡献点字段不受支持")));
            }
            if curve
                .last()
                .is_some_and(|previous: &VerifiedPnlContributionSample| {
                    equity_index <= previous.equity_index
                })
            {
                return Err(invalid(format!("{instrument} 权益贡献索引未严格递增")));
            }
            curve.push(VerifiedPnlContributionSample {
                equity_index,
                ts,
                pnl_contribution_raw,
                pnl_contribution_exact,
            });
        }
        if curve.first().map(|sample| sample.equity_index) != Some(0)
            || curve.last().map(|sample| sample.equity_index) != Some(last_equity_index)
        {
            return Err(invalid(format!(
                "{instrument} 权益贡献采样没有覆盖权益曲线首尾"
            )));
        }
        curves.insert(instrument.clone(), curve);
    }

    let sample_count = curves
        .values()
        .next()
        .map(Vec::len)
        .ok_or_else(|| invalid("逐标的权益贡献没有曲线"))?;
    if curves.values().any(|curve| curve.len() != sample_count) {
        return Err(invalid("逐标的权益贡献采样索引集合不一致"));
    }
    let initial_cash_exact = spec.initial_cash_exact_value()?;
    if let Some(exact_values) = exact_equity_values.as_ref() {
        if exact_values.first().copied() != Some(initial_cash_exact) {
            return Err(invalid("逐标的权益贡献 exact 曲线首点与初始资金不一致"));
        }
        for (equity_row, exact_value) in equity_rows.iter().zip(exact_values) {
            let projected_raw = exact_value
                .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                .and_then(|projected| projected.to_raw(9).ok());
            if equity_row.get(1).and_then(raw_i128) != projected_raw {
                return Err(invalid("逐标的 exact 权益与 raw 权益投影不一致"));
            }
        }
    } else if spec.initial_cash_exact.is_some() {
        return Err(invalid(
            "MultiMarketBacktestSpec v2 复核必须提供 exact 权益 sidecar",
        ));
    }

    for sample_index in 0..sample_count {
        let first_curve_sample = curves
            .values()
            .next()
            .and_then(|curve| curve.get(sample_index))
            .ok_or_else(|| invalid("逐标的权益贡献采样点缺失"))?;
        let equity_index = first_curve_sample.equity_index;
        let ts = first_curve_sample.ts;
        let equity_row = equity_rows
            .get(usize::try_from(equity_index).map_err(|_| invalid("权益索引超出范围"))?)
            .ok_or_else(|| invalid("权益贡献索引超出共享权益曲线"))?;
        if equity_row.get(0).and_then(Value::as_u64) != Some(ts) {
            return Err(invalid("逐标的权益贡献时间戳与共享权益曲线不一致"));
        }

        let mut contribution_sum = 0_i128;
        let mut exact_contribution_sum = exact_equity_rows.map(|_| initial_cash_exact);
        for (instrument, curve) in &curves {
            let sample = curve[sample_index];
            if sample.equity_index != equity_index || sample.ts != ts {
                return Err(invalid("各标的权益贡献采样轴不一致"));
            }
            contribution_sum = contribution_sum
                .checked_add(sample.pnl_contribution_raw)
                .ok_or_else(|| invalid("逐标的权益贡献求和溢出"))?;
            if let Some(exact_sum) = exact_contribution_sum.as_mut() {
                let exact_contribution = sample
                    .pnl_contribution_exact
                    .ok_or_else(|| invalid("exact 权益 sidecar 缺少逐标的精确贡献"))?;
                *exact_sum = exact_sum
                    .checked_add(exact_contribution)
                    .ok_or_else(|| invalid("逐标的精确权益贡献求和溢出"))?;
            }
            if sample_index + 1 == sample_count {
                let instrument_id = InstrumentId::parse(instrument)
                    .ok_or_else(|| invalid(format!("无效标的标识: {instrument}")))?;
                let instrument_spec = market_specs
                    .get(&instrument_id)
                    .ok_or_else(|| invalid(format!("{instrument} 缺少有效市场规格")))?;
                let mark = final_market_marks
                    .get(&instrument_id)
                    .ok_or_else(|| invalid(format!("{instrument} 缺少末尾行情标记")))?;
                let recomputed = ledger
                    .instrument_equity_contribution(
                        &spec.account_id,
                        &instrument_id,
                        Some(mark),
                        &spec.settlement_currency,
                        instrument_spec,
                        &BTreeMap::new(),
                    )
                    .map_err(|error| {
                        invalid(format!("{instrument} 期末盈亏贡献复算失败: {error}"))
                    })?;
                if recomputed != sample.pnl_contribution_raw {
                    return Err(invalid(format!(
                        "{instrument} 期末盈亏贡献与重放账簿估值不一致"
                    )));
                }
                if let Some(expected_exact) = sample.pnl_contribution_exact {
                    let mut exact_cash = BTreeMap::<String, ExactDecimal>::new();
                    for entry in ledger.entries().iter().filter(|entry| {
                        entry.account_id == spec.account_id
                            && entry.instrument.as_ref() == Some(&instrument_id)
                    }) {
                        let amount = match entry.exact_amounts {
                            Some(amounts) => amounts.amount,
                            None => ExactDecimal::from_parts(entry.amount.raw(), 9)
                                .map_err(|error| invalid(error.to_owned()))?,
                        };
                        let total = exact_cash
                            .entry(entry.currency.clone())
                            .or_insert(ExactDecimal::ZERO);
                        *total = total
                            .checked_add(amount)
                            .ok_or_else(|| invalid("精确标的现金归因累计溢出"))?;
                    }
                    let context = ValuationContext::spot(
                        &spec.account_id,
                        final_market_marks,
                        &spec.settlement_currency,
                    )
                    .with_spec(Some(instrument_spec));
                    let exact_recomputed = ledger
                        .instrument_equity_contribution_from_exact_cash(&context, &exact_cash)
                        .map_err(|error| {
                            invalid(format!("{instrument} 期末精确贡献复算失败: {error}"))
                        })?;
                    if exact_recomputed != expected_exact {
                        return Err(invalid(format!(
                            "{instrument} 期末精确贡献与重放账簿估值不一致"
                        )));
                    }
                }
            }
        }

        let reconstructed_equity = spec
            .initial_cash_raw
            .checked_add(contribution_sum)
            .ok_or_else(|| invalid("共享权益归因重建溢出"))?;
        let expected_equity = equity_row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| invalid("权益贡献校验目标无效"))?;
        if reconstructed_equity != expected_equity {
            return Err(invalid(format!(
                "第 {equity_index} 个采样点的逐标的贡献与共享账户权益不守恒"
            )));
        }
        if let (Some(exact_values), Some(exact_sum)) =
            (exact_equity_values.as_ref(), exact_contribution_sum)
        {
            let exact_row_value = *exact_values
                .get(usize::try_from(equity_index).map_err(|_| invalid("exact 权益索引超出范围"))?)
                .ok_or_else(|| invalid("exact 权益采样索引越界"))?;
            if exact_sum != exact_row_value {
                return Err(invalid(format!(
                    "第 {equity_index} 个采样点的逐标的 exact 贡献与 exact 共享权益不守恒"
                )));
            }
            if sample_index + 1 == sample_count {
                let valuation = ValuationContext::spot(
                    &spec.account_id,
                    final_market_marks,
                    &spec.settlement_currency,
                )
                .with_market_specs(market_specs);
                let exact_total = ledger
                    .valuate_exact(&ExactValuationContext::new(valuation))
                    .map_err(|error| invalid(format!("期末 exact 权益复算失败: {error}")))?;
                if exact_total != exact_row_value {
                    return Err(invalid("期末 exact 权益与重放账簿估值不一致"));
                }
            }
        }
    }

    Ok(VerifiedMultiMarketPnlContributions {
        total_equity_points,
        curves,
    })
}
