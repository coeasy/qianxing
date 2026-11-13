//! Full-curve exact equity, return, drawdown, and trade metric verification for depth reports.

use super::*;

type EquityVerification = (
    VerifiedEquity,
    Vec<(u64, i128)>,
    Vec<(u64, qx_core::ExactDecimal)>,
);

pub(super) struct VerifiedEquity {
    pub(super) total: u64,
    pub(super) initial_equity_raw: i128,
    pub(super) final_equity_raw: i128,
    pub(super) initial_equity_exact: qx_core::ExactDecimal,
    pub(super) final_equity_exact: qx_core::ExactDecimal,
    pub(super) return_amount_exact: qx_core::ExactDecimal,
    pub(super) max_drawdown_exact: qx_core::ExactDecimal,
    pub(super) return_bps: i32,
    pub(super) max_drawdown_bps: u32,
}

pub(super) fn verify_equity(
    value: &Value,
    exact_value: Option<&Value>,
    spec: &MultiMarketDepthBacktestSpec,
    outcome: &MultiMarketDepthBacktestOutcome,
) -> Result<EquityVerification, AppError> {
    let rows = value
        .as_array()
        .filter(|rows| {
            !rows.is_empty() && rows.len() as u64 <= crate::artifact_store::MAX_ARTIFACT_ROWS
        })
        .ok_or_else(|| conflict("多市场深度权益曲线为空或超过读取上限"))?;
    if rows.len() as u64 != outcome.equity_points {
        return Err(conflict("多市场深度权益点数与结果摘要不一致"));
    }
    let parsed = rows
        .iter()
        .map(|row| {
            let pair = row
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| conflict("多市场深度权益行必须包含时间和权益"))?;
            let ts = pair[0]
                .as_u64()
                .or_else(|| pair[0].as_str().and_then(|text| text.parse().ok()))
                .filter(|ts| *ts > 0)
                .ok_or_else(|| conflict("多市场深度权益时间无效"))?;
            let equity = raw_i128(&pair[1]).ok_or_else(|| conflict("多市场深度权益金额无效"))?;
            Ok((ts, equity))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    if parsed.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(conflict("多市场深度权益时间必须严格递增"));
    }
    let initial = parsed.first().map(|row| row.1).unwrap_or_default();
    let final_equity = parsed.last().map(|row| row.1).unwrap_or_default();
    if initial != spec.initial_cash_raw || final_equity != outcome.final_equity_raw {
        return Err(conflict("多市场深度权益曲线与初始资金/期末权益不一致"));
    }
    let exact_initial = spec
        .initial_cash_exact_value()
        .map_err(|error| conflict(format!("多市场深度精确初始资金无效: {error}")))?;
    let exact_rows = match exact_value {
        Some(value) => {
            let rows = value
                .as_array()
                .filter(|rows| rows.len() == parsed.len())
                .ok_or_else(|| conflict("多市场深度精确权益点数与 raw 曲线不一致"))?;
            rows.iter()
                .enumerate()
                .map(|(index, row)| {
                    let equity_index = row
                        .get("equity_index")
                        .and_then(Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| conflict("多市场深度精确权益 index 无效"))?;
                    let ts = row
                        .get("ts")
                        .and_then(Value::as_u64)
                        .or_else(|| {
                            row.get("ts")
                                .and_then(Value::as_str)
                                .and_then(|value| value.parse().ok())
                        })
                        .filter(|value| *value > 0)
                        .ok_or_else(|| conflict("多市场深度精确权益时间无效"))?;
                    let encoded = row
                        .get("equity_exact")
                        .and_then(Value::as_str)
                        .ok_or_else(|| conflict("多市场深度精确权益金额缺失"))?;
                    let exact = qx_core::ExactDecimal::parse(encoded)
                        .map_err(|_| conflict("多市场深度精确权益金额格式无效"))?;
                    if exact.to_string() != encoded {
                        return Err(conflict("多市场深度精确权益金额必须采用规范格式"));
                    }
                    if equity_index != index || ts != parsed[index].0 {
                        return Err(conflict("多市场深度精确权益 index/time 与 raw 曲线不一致"));
                    }
                    if spec.schema_version
                        == crate::MULTI_MARKET_DEPTH_BACKTEST_EXACT_SCHEMA_VERSION
                        && exact_cash_raw_projection(exact) != Some(parsed[index].1)
                    {
                        return Err(conflict(
                            "多市场深度 v2 精确权益 raw 投影与兼容权益曲线不一致",
                        ));
                    }
                    Ok((ts, exact))
                })
                .collect::<Result<Vec<_>, AppError>>()?
        }
        None => parsed
            .iter()
            .map(|(ts, raw)| {
                qx_core::ExactDecimal::from_parts(*raw, 9)
                    .map(|value| (*ts, value))
                    .map_err(|error| invariant(format!("多市场深度 raw 权益投影无效: {error}")))
            })
            .collect::<Result<Vec<_>, AppError>>()?,
    };
    let exact_curve = exact_rows
        .iter()
        .map(|(_, value)| *value)
        .collect::<Vec<_>>();
    let final_equity_exact = exact_curve.last().copied().unwrap_or(exact_initial);
    if exact_curve.first().copied() != Some(exact_initial) {
        return Err(conflict("多市场深度精确权益曲线初值与初始资金不一致"));
    }
    let return_bps =
        crate::report_analysis::exact_equity_total_return_bps_v1(exact_initial, final_equity_exact)
            .map_err(conflict)?;
    let max_drawdown_bps =
        crate::report_analysis::exact_equity_max_drawdown_bps_v1(&exact_curve).map_err(conflict)?;
    if return_bps != outcome.return_bps || max_drawdown_bps != outcome.max_drawdown_bps {
        return Err(conflict("多市场深度收益或最大回撤与权益曲线不一致"));
    }
    let return_amount_exact = final_equity_exact
        .checked_sub(exact_initial)
        .ok_or_else(|| conflict("多市场深度精确收益金额溢出"))?;
    let mut exact_peak = exact_initial;
    let mut max_drawdown_exact = qx_core::ExactDecimal::ZERO;
    for equity in &exact_curve {
        if exact_peak
            .checked_cmp(*equity)
            .is_some_and(|ordering| ordering.is_lt())
        {
            exact_peak = *equity;
        }
        let drawdown = exact_peak
            .checked_sub(*equity)
            .ok_or_else(|| conflict("多市场深度精确回撤金额溢出"))?;
        if drawdown
            .checked_cmp(max_drawdown_exact)
            .is_some_and(|ordering| ordering.is_gt())
        {
            max_drawdown_exact = drawdown;
        }
    }
    Ok((
        VerifiedEquity {
            total: parsed.len() as u64,
            initial_equity_raw: initial,
            final_equity_raw: final_equity,
            initial_equity_exact: exact_initial,
            final_equity_exact,
            return_amount_exact,
            max_drawdown_exact,
            return_bps,
            max_drawdown_bps,
        },
        parsed,
        exact_rows,
    ))
}

pub(super) fn verify_exact_equity_metrics(
    artifact: &Value,
    verified: &VerifiedEquity,
) -> Result<(), AppError> {
    let equity = artifact
        .get("metrics_exact")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少精确权益指标"))?;
    let expected = [
        ("initial", verified.initial_equity_exact.to_string()),
        ("final", verified.final_equity_exact.to_string()),
        ("return", verified.return_amount_exact.to_string()),
        ("return_bps", verified.return_bps.to_string()),
        ("max_drawdown", verified.max_drawdown_exact.to_string()),
        ("max_drawdown_bps", verified.max_drawdown_bps.to_string()),
    ];
    for (field, expected) in expected {
        let actual = equity
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| conflict(format!("多市场深度精确权益指标缺少 {field}")))?;
        if actual != expected {
            return Err(conflict(format!(
                "多市场深度精确权益指标 {field} 与权益曲线重算结果不一致"
            )));
        }
    }
    Ok(())
}

fn exact_cash_raw_projection(amount: qx_core::ExactDecimal) -> Option<i128> {
    amount
        .checked_mul_at_scale(
            qx_core::ExactDecimal::ONE,
            9,
            qx_core::DecimalRounding::TowardZero,
        )
        .and_then(|projected| projected.to_raw(9).ok())
}

pub(super) fn verify_exact_trade_metrics(
    artifact: &Value,
    fill_rows: &[Value],
    spec: &MultiMarketDepthBacktestSpec,
) -> Result<Value, AppError> {
    let metrics = artifact
        .get("metrics_exact")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少精确交易指标"))?;
    let mut fees_by_currency = BTreeMap::<String, qx_core::ExactDecimal>::new();
    let mut turnover = qx_core::ExactDecimal::ZERO;
    let mut fees_raw = 0_i128;
    let mut turnover_raw = 0_i128;
    let market_specs = spec
        .markets
        .iter()
        .map(|market| {
            let market_spec = market.market_spec.clone();
            Ok((market.instrument.clone(), market_spec))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    let ledger_entries: Vec<qx_core::LedgerEntry> = serde_json::from_value(
        artifact
            .get("ledger_entries")
            .cloned()
            .ok_or_else(|| conflict("多市场深度 v4 产物缺少账簿 entries"))?,
    )
    .map_err(|error| conflict(format!("多市场深度账簿 entries 无效: {error}")))?;
    let mut instrument_by_order = BTreeMap::<u64, String>::new();
    for entry in &ledger_entries {
        if entry.kind == qx_core::LedgerEntryKind::TradePosition {
            if let (Some(order_id), Some(instrument)) = (entry.order_id, entry.instrument.as_ref())
            {
                instrument_by_order.insert(order_id, instrument.to_string());
            }
        }
    }

    for row in fill_rows {
        let fill: qx_core::Fill = serde_json::from_value(row.clone())
            .map_err(|error| conflict(format!("多市场深度成交格式无效: {error}")))?;
        fill.validate_exact_amounts()
            .map_err(|error| conflict(format!("多市场深度成交金额无效: {error}")))?;
        let instrument_value = instrument_by_order
            .get(&fill.order_id)
            .ok_or_else(|| conflict("多市场深度成交找不到对应账簿订单标的"))?;
        let market_spec = market_specs
            .get(instrument_value)
            .ok_or_else(|| conflict("多市场深度成交引用了未配置标的"))?;
        let exact_turnover = market_spec
            .notional_exact(fill.quantity_decimal(), fill.price_decimal())
            .map_err(|error| conflict(format!("多市场深度成交额重算失败: {error}")))?;
        turnover = turnover
            .checked_add(exact_turnover)
            .ok_or_else(|| conflict("多市场深度精确成交额累计溢出"))?;
        turnover_raw = turnover_raw
            .checked_add(exact_raw_projection(exact_turnover)?)
            .ok_or_else(|| conflict("多市场深度成交额兼容投影累计溢出"))?;

        if !fill.exact_fees.is_empty() {
            for fee in &fill.exact_fees {
                let currency = fee
                    .currency
                    .as_deref()
                    .unwrap_or(&spec.settlement_currency)
                    .trim()
                    .to_ascii_uppercase();
                if currency.is_empty() {
                    return Err(conflict("多市场深度精确手续费缺少币种"));
                }
                let total = fees_by_currency
                    .entry(currency.clone())
                    .or_insert(qx_core::ExactDecimal::ZERO);
                *total = total
                    .checked_add(fee.amount)
                    .ok_or_else(|| conflict("多市场深度精确手续费累计溢出"))?;
                if currency == spec.settlement_currency {
                    fees_raw = fees_raw
                        .checked_add(exact_raw_projection(fee.amount)?)
                        .ok_or_else(|| conflict("多市场深度手续费兼容投影累计溢出"))?;
                }
            }
        } else {
            for fee in fill
                .fee_charges()
                .map_err(|error| conflict(format!("多市场深度手续费无效: {error}")))?
            {
                let currency = fee
                    .currency
                    .as_deref()
                    .unwrap_or(&spec.settlement_currency)
                    .trim()
                    .to_ascii_uppercase();
                let amount = qx_core::ExactDecimal::from_parts(fee.amount.raw(), 9)
                    .map_err(|error| conflict(format!("多市场深度手续费金额无效: {error}")))?;
                let total = fees_by_currency
                    .entry(currency)
                    .or_insert(qx_core::ExactDecimal::ZERO);
                *total = total
                    .checked_add(amount)
                    .ok_or_else(|| conflict("多市场深度手续费累计溢出"))?;
            }
            fees_raw = fees_raw
                .checked_add(fill.fee.raw())
                .ok_or_else(|| conflict("多市场深度手续费兼容投影累计溢出"))?;
        }
    }

    let reported_turnover = metrics
        .get("turnover")
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少成交额"))?;
    let turnover_amount = reported_turnover
        .get("amount")
        .and_then(Value::as_str)
        .ok_or_else(|| conflict("多市场深度精确成交额格式无效"))?;
    let turnover_currency = reported_turnover
        .get("currency")
        .and_then(Value::as_str)
        .ok_or_else(|| conflict("多市场深度精确成交额缺少币种"))?;
    let reported_turnover_exact = qx_core::ExactDecimal::parse(turnover_amount)
        .map_err(|_| conflict("多市场深度精确成交额格式无效"))?;
    if reported_turnover_exact != turnover || turnover_currency != spec.settlement_currency {
        return Err(conflict("多市场深度精确成交额与逐笔成交/结算币种不一致"));
    }

    let reported_fees = metrics
        .get("fees_by_currency")
        .and_then(Value::as_object)
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少按币种手续费"))?;
    let normalized_reported_fees = reported_fees
        .iter()
        .map(|(currency, value)| {
            let encoded = value
                .as_str()
                .ok_or_else(|| conflict("多市场深度手续费金额必须是精确十进制字符串"))?;
            let amount = qx_core::ExactDecimal::parse(encoded)
                .map_err(|_| conflict("多市场深度手续费金额格式无效"))?;
            if amount.to_string() != encoded {
                return Err(conflict("多市场深度手续费金额必须采用规范格式"));
            }
            Ok((currency.to_ascii_uppercase(), amount))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    if normalized_reported_fees != fees_by_currency {
        return Err(conflict("多市场深度按币种手续费与成交明细不一致"));
    }

    let raw_projection = metrics
        .get("legacy_raw_projection")
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少兼容指标投影"))?;
    let reported_fees_raw = raw_projection
        .get("fees_raw")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i128>().ok())
        .ok_or_else(|| conflict("多市场深度 fees_raw 投影无效"))?;
    let reported_turnover_raw = raw_projection
        .get("turnover_raw")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i128>().ok())
        .ok_or_else(|| conflict("多市场深度 turnover_raw 投影无效"))?;
    if reported_fees_raw != fees_raw || reported_turnover_raw != turnover_raw {
        return Err(conflict("多市场深度兼容指标投影与逐笔成交不一致"));
    }
    let input_precision = metrics
        .get("input_precision")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| conflict("多市场深度 v4 产物缺少精度口径说明"))?;

    let mut ledger_fees = BTreeMap::<String, qx_core::ExactDecimal>::new();
    for entry in &ledger_entries {
        if entry.kind != qx_core::LedgerEntryKind::Fee {
            continue;
        }
        let amount = entry
            .exact_amounts
            .map(|values| values.amount)
            .or_else(|| qx_core::ExactDecimal::from_parts(entry.amount.raw(), 9).ok())
            .ok_or_else(|| conflict("多市场深度账簿手续费金额无效"))?;
        let positive_fee = amount
            .checked_neg()
            .ok_or_else(|| conflict("多市场深度账簿手续费取反溢出"))?;
        let total = ledger_fees
            .entry(entry.currency.to_ascii_uppercase())
            .or_insert(qx_core::ExactDecimal::ZERO);
        *total = total
            .checked_add(positive_fee)
            .ok_or_else(|| conflict("多市场深度账簿手续费累计溢出"))?;
    }
    if ledger_fees != fees_by_currency {
        return Err(conflict("多市场深度账簿手续费与成交明细不一致"));
    }

    Ok(serde_json::json!({
        "turnover": { "amount": turnover.to_string(), "currency": turnover_currency },
        "fees_by_currency": fees_by_currency.iter().map(|(currency, amount)| (currency.clone(), amount.to_string())).collect::<BTreeMap<_, _>>(),
        "legacy_raw_projection": { "fees_raw": fees_raw.to_string(), "turnover_raw": turnover_raw.to_string() },
        "input_precision": input_precision,
    }))
}

fn exact_raw_projection(value: qx_core::ExactDecimal) -> Result<i128, AppError> {
    value
        .checked_mul_at_scale(
            qx_core::ExactDecimal::ONE,
            9,
            qx_core::DecimalRounding::TowardZero,
        )
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| conflict("多市场深度精确指标 raw 投影溢出"))
}

fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|text| text.parse::<i128>().ok()))
}
