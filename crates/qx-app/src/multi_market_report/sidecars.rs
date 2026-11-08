//! Replay-backed verification of multi-market Bar equity and fill sidecars.

use super::{invalid, raw_i128, AppError, ExactDecimal, MultiMarketBacktestSpec};
use crate::report_read::{event_fill_identity, update_fill_sequence_digest};
use qx_core::DecimalRounding;
use serde_json::Value;

/// Facts recomputed from a shared-account multi-market Bar report's sidecars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketBarSidecarsV1 {
    pub equity_points: u64,
    pub first_equity_raw: i128,
    pub final_equity_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub fill_rows: u64,
}

struct VerifiedMultiMarketBarEquityV1 {
    equity_points: u64,
    first_equity_raw: i128,
    final_equity_raw: i128,
    return_bps: i32,
    max_drawdown_bps: u32,
}

/// Recompute portfolio metrics and bind the fills sidecar to replayed Filled facts.
///
/// The caller still owns rooted file reads and EventLog replay. This use case receives the
/// already loaded facts and verifies the outcome-facing financial summary before projection.
#[allow(clippy::too_many_arguments)]
pub fn verify_multi_market_bar_sidecars_v1(
    equity: &Value,
    fills: &Value,
    events: &[Value],
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_fill_count: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    verify_multi_market_bar_sidecars_with_exact_v1(
        equity,
        None,
        fills,
        events,
        spec,
        expected_equity_points,
        expected_fill_count,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )
}

/// Verify Bar sidecars when the run carries an exact initial principal/equity curve.
#[allow(clippy::too_many_arguments)]
pub fn verify_multi_market_bar_sidecars_with_exact_v1(
    equity: &Value,
    exact_equity: Option<&Value>,
    fills: &Value,
    events: &[Value],
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_fill_count: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    let equity = verify_multi_market_bar_equity_metrics_v1(
        equity,
        exact_equity,
        spec,
        expected_equity_points,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )?;
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid("多市场成交 sidecar 必须是数组"))?;
    let event_fills: Vec<Value> = events
        .iter()
        .filter_map(|event| event.pointer("/kind/Filled/fill").cloned())
        .collect();
    let fill_count =
        u64::try_from(fill_rows.len()).map_err(|_| invalid("多市场成交行数超出 u64 范围"))?;
    if fills != &Value::Array(event_fills) || fill_count != expected_fill_count {
        return Err(invalid("多市场成交 sidecar 与已重放 Filled 事件不一致"));
    }

    Ok(VerifiedMultiMarketBarSidecarsV1 {
        equity_points: equity.equity_points,
        first_equity_raw: equity.first_equity_raw,
        final_equity_raw: equity.final_equity_raw,
        return_bps: equity.return_bps,
        max_drawdown_bps: equity.max_drawdown_bps,
        fill_rows: fill_count,
    })
}

/// Reconcile a fills sidecar with the streamed, ordered Filled facts from a verified EventLog.
#[allow(clippy::too_many_arguments)]
pub fn verify_multi_market_bar_sidecars_from_event_report_v1(
    equity: &Value,
    fills: &Value,
    expected_fill_count: u64,
    expected_fill_sequence_sha256: &str,
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
        equity,
        None,
        fills,
        expected_fill_count,
        expected_fill_sequence_sha256,
        spec,
        expected_equity_points,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )
}

/// Event-report variant of [`verify_multi_market_bar_sidecars_with_exact_v1`].
#[allow(clippy::too_many_arguments)]
pub fn verify_multi_market_bar_sidecars_from_event_report_with_exact_v1(
    equity: &Value,
    exact_equity: Option<&Value>,
    fills: &Value,
    expected_fill_count: u64,
    expected_fill_sequence_sha256: &str,
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarSidecarsV1, AppError> {
    let equity = verify_multi_market_bar_equity_metrics_v1(
        equity,
        exact_equity,
        spec,
        expected_equity_points,
        expected_final_equity_raw,
        expected_return_bps,
        expected_max_drawdown_bps,
    )?;
    let fill_count = verify_multi_market_event_fill_sidecar_v1(
        fills,
        expected_fill_count,
        expected_fill_sequence_sha256,
    )?;
    Ok(VerifiedMultiMarketBarSidecarsV1 {
        equity_points: equity.equity_points,
        first_equity_raw: equity.first_equity_raw,
        final_equity_raw: equity.final_equity_raw,
        return_bps: equity.return_bps,
        max_drawdown_bps: equity.max_drawdown_bps,
        fill_rows: fill_count,
    })
}

pub fn verify_multi_market_event_fill_sidecar_v1(
    fills: &Value,
    expected_fill_count: u64,
    expected_fill_sequence_sha256: &str,
) -> Result<u64, AppError> {
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid("多市场成交 sidecar 必须是数组"))?;
    let fill_count =
        u64::try_from(fill_rows.len()).map_err(|_| invalid("多市场成交行数超出 u64 范围"))?;
    let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
    digest.update(b"qx-event-fill-sequence-v1\0");
    for row in fill_rows {
        let fill: qx_core::Fill = serde_json::from_value(row.clone())
            .map_err(|error| invalid(format!("多市场成交 sidecar 行无效: {error}")))?;
        let normalized = serde_json::to_value(&fill)
            .map_err(|error| invalid(format!("多市场成交 sidecar 归一化失败: {error}")))?;
        if normalized != *row {
            return Err(invalid("多市场成交 sidecar 含事件事实之外的字段或非规范值"));
        }
        update_fill_sequence_digest(&mut digest, &event_fill_identity(&fill))?;
    }
    if fill_count != expected_fill_count || digest.finish_hex() != expected_fill_sequence_sha256 {
        return Err(invalid(
            "多市场成交 sidecar 与流式重放的 Filled 事件序列不一致",
        ));
    }
    Ok(fill_count)
}

/// Preserve the v1 wire contract for inline EventLog artifacts while reconciling depth fills.
/// Older reports compare the sidecar's complete JSON rows directly with embedded Filled events;
/// this helper keeps that compatibility check in the application layer.
pub fn verify_multi_market_legacy_event_fill_sidecar_v1(
    fills: &Value,
    events: &[Value],
    expected_fill_count: u64,
) -> Result<(), AppError> {
    let fill_rows = fills
        .as_array()
        .ok_or_else(|| invalid("多市场深度成交 sidecar 必须是数组"))?;
    let event_fills = events
        .iter()
        .filter_map(|event| event.pointer("/kind/Filled/fill").cloned())
        .collect::<Vec<_>>();
    let fill_count =
        u64::try_from(fill_rows.len()).map_err(|_| invalid("多市场深度成交行数溢出"))?;
    if fills != &Value::Array(event_fills) || fill_count != expected_fill_count {
        return Err(invalid("多市场深度成交 sidecar 与重放 Filled 事件不一致"));
    }
    Ok(())
}

fn verify_multi_market_bar_equity_metrics_v1(
    equity: &Value,
    exact_equity: Option<&Value>,
    spec: &MultiMarketBacktestSpec,
    expected_equity_points: u64,
    expected_final_equity_raw: i128,
    expected_return_bps: i32,
    expected_max_drawdown_bps: u32,
) -> Result<VerifiedMultiMarketBarEquityV1, AppError> {
    let equity_rows = equity
        .as_array()
        .ok_or_else(|| invalid("多市场权益曲线必须是数组"))?;
    let equity_points =
        u64::try_from(equity_rows.len()).map_err(|_| invalid("多市场权益点数超出 u64 范围"))?;
    if equity_points != expected_equity_points || equity_rows.is_empty() {
        return Err(invalid("多市场权益点数与结果摘要不一致"));
    }
    let first_equity_raw = equity_rows
        .first()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid("多市场权益首点无效"))?;
    let final_equity_raw = equity_rows
        .last()
        .and_then(|row| row.get(1))
        .and_then(raw_i128)
        .ok_or_else(|| invalid("多市场权益末点无效"))?;
    if first_equity_raw != spec.initial_cash_raw || final_equity_raw != expected_final_equity_raw {
        return Err(invalid("多市场权益曲线与初始资金/期末权益不一致"));
    }
    let initial_cash_exact = spec.initial_cash_exact_value()?;
    let (return_bps, max_drawdown_bps) = if let Some(exact_equity) = exact_equity {
        let rows = exact_equity
            .as_array()
            .ok_or_else(|| invalid("多市场 exact 权益 sidecar 必须是数组"))?;
        if rows.len() != equity_rows.len() || rows.is_empty() {
            return Err(invalid("多市场 exact 权益点数与 raw 权益曲线不一致"));
        }
        let mut exact_values = Vec::with_capacity(rows.len());
        for (index, (row, raw_row)) in rows.iter().zip(equity_rows).enumerate() {
            let object = row
                .as_object()
                .ok_or_else(|| invalid("多市场 exact 权益采样点必须是对象"))?;
            let equity_index = object
                .get("equity_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("多市场 exact 权益索引无效"))?;
            let ts = object
                .get("ts")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("多市场 exact 权益时间戳无效"))?;
            let amount = object
                .get("equity_exact")
                .and_then(Value::as_str)
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| invalid(format!("多市场 exact 权益金额无效: {error}")))?
                .ok_or_else(|| invalid("多市场 exact 权益金额缺失"))?;
            if object.len() != 3
                || equity_index != index as u64
                || raw_row.get(0).and_then(Value::as_u64) != Some(ts)
            {
                return Err(invalid("多市场 exact 权益 sidecar 索引/时间轴无效"));
            }
            let projected_raw = amount
                .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                .and_then(|projected| projected.to_raw(9).ok());
            if raw_row.get(1).and_then(raw_i128) != projected_raw {
                return Err(invalid("多市场 exact 权益与 raw 权益投影不一致"));
            }
            exact_values.push(amount);
        }
        if exact_values.first().copied() != Some(initial_cash_exact) {
            return Err(invalid("多市场 exact 权益首点与初始资金不一致"));
        }
        let return_bps = crate::report_analysis::exact_equity_total_return_bps_v1(
            initial_cash_exact,
            *exact_values
                .last()
                .ok_or_else(|| invalid("多市场 exact 权益曲线为空"))?,
        )
        .map_err(invalid)?;
        let max_drawdown_bps =
            crate::report_analysis::exact_equity_max_drawdown_bps_v1(&exact_values)
                .map_err(invalid)?;
        (return_bps, max_drawdown_bps)
    } else {
        if spec.initial_cash_exact.is_some() {
            return Err(invalid(
                "MultiMarketBacktestSpec v2 复核必须提供 exact 权益 sidecar",
            ));
        }
        let return_bps = final_equity_raw
            .checked_sub(first_equity_raw)
            .and_then(|value| value.checked_mul(10_000))
            .and_then(|value| value.checked_div(first_equity_raw))
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| invalid("多市场收益基点无法复算"))?;
        (return_bps, equity_drawdown_bps(equity_rows)?)
    };
    if return_bps != expected_return_bps || max_drawdown_bps != expected_max_drawdown_bps {
        return Err(invalid("多市场聚合收益或最大回撤与权益曲线不一致"));
    }

    Ok(VerifiedMultiMarketBarEquityV1 {
        equity_points,
        first_equity_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
    })
}

fn equity_drawdown_bps(rows: &[Value]) -> Result<u32, AppError> {
    let mut peak = 0_i128;
    let mut maximum = 0_u32;
    for row in rows {
        let value = row
            .get(1)
            .and_then(raw_i128)
            .ok_or_else(|| invalid("权益曲线包含无效金额"))?;
        peak = peak.max(value);
        if peak <= 0 || value >= peak {
            continue;
        }
        let drawdown = peak
            .checked_sub(value)
            .and_then(|loss| loss.checked_mul(10_000))
            .and_then(|loss| loss.checked_div(peak))
            .unwrap_or(10_000)
            .clamp(0, 10_000) as u32;
        maximum = maximum.max(drawdown);
    }
    Ok(maximum)
}
