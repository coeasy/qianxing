use crate::{DataTier, OrderBookExecutionDescriptor, OrderBookExecutionModel, RunManifestIdentity};
use qx_core::{
    DecimalRounding, EventLog, ExactDecimal, Fill, Fnv1a, Ledger, RunManifest,
    TradingInstrumentSpec,
};
use serde::{Deserialize, Serialize};

pub struct OrderBookBacktestReport {
    pub fills: Vec<Fill>,
    pub execution_attempts: Vec<crate::ExecutionAttempt>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub pending_orders: usize,
    /// 与 Bar 回测同形的统一产物：每个快照结束后的账户权益、净持仓与时间。
    pub equity: Vec<i128>,
    /// Scale-aware marked equity parallel to the legacy raw curve.
    pub equity_exact: Vec<ExactDecimal>,
    pub positions: Vec<i128>,
    pub snapshot_ts: Vec<u64>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    /// Exact native-currency fee totals. `fees_raw` remains the legacy 1e-9 projection.
    pub fees_exact_by_currency: BTreeMap<String, ExactDecimal>,
    /// Exact traded notional in the configured quote/settlement currency.
    pub turnover_exact: ExactDecimal,
    pub turnover_currency: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub initial_equity_raw: i128,
    pub initial_equity_exact: ExactDecimal,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
    pub data_tier: DataTier,
    pub risk_rule_set_version: String,
    pub execution_model_descriptor: OrderBookExecutionDescriptor,
    pub model_descriptors: Vec<String>,
    pub assumptions: Vec<String>,
}

use std::collections::{BTreeMap, BTreeSet};

pub(super) fn execution_model_descriptors(
    data_tier: DataTier,
    model: &OrderBookExecutionModel,
    risk_rule_set_version: &str,
    instrument_spec: Option<&TradingInstrumentSpec>,
) -> Vec<String> {
    vec![
        format!("data_tier={data_tier:?}"),
        format!(
            "orderbook-matching@v2 fee_bps={} latency_snapshots={} queue_position_bps={} {} market_impact_bps={}",
            model.fee_bps,
            model.latency_snapshots,
            model.queue_position_bps,
            model
                .queue_position_model
                .descriptor(model.queue_position_bps),
            model.market_impact_bps
        ),
        format!("risk_rule_set={risk_rule_set_version}"),
        match instrument_spec {
            Some(spec) => format!("ledger=spec:{:?}", spec.product),
            None => "ledger=cash-spot".to_string(),
        },
    ]
}

/// Paired limit-order fill-rate change for reports over identical order-book inputs.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueuePositionSensitivityReport {
    pub schema_version: u32,
    pub input_data_hash: u64,
    pub baseline_result_hash: String,
    pub estimated_result_hash: String,
    pub baseline_limit_order_fill_rate_bps: u32,
    pub estimated_limit_order_fill_rate_bps: u32,
    pub delta_bps: i32,
    pub baseline_model_descriptor: String,
    pub estimated_model_descriptor: String,
    pub descriptor: String,
}

impl OrderBookBacktestReport {
    /// Share of submitted limit orders that received at least one fill, in basis points.
    pub fn limit_order_fill_rate_bps(&self) -> Option<u32> {
        let submitted = self
            .event_log
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                qx_core::EventKind::OrderSubmitted { order } if order.limit.is_some() => {
                    Some(order.client_id)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if submitted.is_empty() {
            return None;
        }
        let filled = self
            .fills
            .iter()
            .map(|fill| fill.order_id)
            .collect::<BTreeSet<_>>();
        let filled_count = submitted.intersection(&filled).count() as u128;
        Some(((filled_count * 10_000) / submitted.len() as u128) as u32)
    }

    pub(crate) fn append_limit_order_fill_rate_descriptor(&mut self) {
        let value = self
            .limit_order_fill_rate_bps()
            .map_or_else(|| "unavailable".to_string(), |rate| rate.to_string());
        self.model_descriptors
            .push(format!("limit_order_fill_rate_bps={value}"));
    }

    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn final_equity(&self) -> i128 {
        *self.equity.last().unwrap_or(&self.initial_equity_raw)
    }

    pub fn final_equity_exact(&self) -> ExactDecimal {
        self.equity_exact
            .last()
            .copied()
            .unwrap_or(self.initial_equity_exact)
    }

    /// Scale-aware equity facts used by reports and downstream research readers.
    pub fn exact_metrics_json(&self) -> Result<serde_json::Value, String> {
        let mut metrics = serde_json::json!({
            "equity": exact_equity_metrics_json(self.initial_equity_exact, &self.equity_exact)?,
            "turnover": {
                "amount": self.turnover_exact.to_string(),
                "currency": self.turnover_currency,
            },
            "fees_by_currency": self.fees_exact_by_currency.iter().map(|(currency, amount)| {
                (currency.clone(), amount.to_string())
            }).collect::<BTreeMap<_, _>>(),
            "legacy_raw_projection": {
                "fees_raw": self.fees_raw.to_string(),
                "turnover_raw": self.turnover_raw.to_string(),
            },
        });
        let mut funding_by_currency = BTreeMap::<String, ExactDecimal>::new();
        let mut funding_count = 0_u64;
        for entry in self
            .ledger
            .entries()
            .iter()
            .filter(|entry| entry.kind == qx_core::LedgerEntryKind::Funding)
        {
            funding_count = funding_count
                .checked_add(1)
                .ok_or_else(|| "资金费结算计数溢出".to_string())?;
            let amount = entry
                .exact_amounts
                .map_or_else(
                    || ExactDecimal::from_parts(entry.amount.raw(), 9),
                    |exact| Ok(exact.amount),
                )
                .map_err(|error| error.to_string())?;
            let total = funding_by_currency
                .entry(entry.currency.clone())
                .or_insert(ExactDecimal::ZERO);
            *total = total
                .checked_add(amount)
                .ok_or_else(|| "资金费现金流累计溢出".to_string())?;
        }
        metrics["funding"] = serde_json::json!({
            "settlement_count": funding_count,
            "cashflow_by_currency": funding_by_currency
                .iter()
                .map(|(currency, amount)| (currency.clone(), amount.to_string()))
                .collect::<BTreeMap<_, _>>(),
        });
        metrics["input_precision"] = serde_json::json!(
            "DepthFrame v2 exact path covers spec-bound single-market Spot and linear/inverse derivative quantity orders, exact risk checks, Spot cash/inventory reservations, derivative aggregate margin/fee projections, funding cashflows, fills, fees, and ledger when queue estimation is disabled. Shared multi-market exact risk/reservations are not enabled; v1 inputs retain the 1e-9 grid."
        );
        Ok(metrics)
    }

    /// Build the same run fingerprint contract used by bar backtests.
    pub fn run_manifest(
        &self,
        identity: RunManifestIdentity<'_>,
        data_fingerprint: &str,
    ) -> Result<RunManifest, String> {
        if data_fingerprint.trim().is_empty() {
            return Err("RunManifest data_fingerprint 不能为空".into());
        }
        let mut model_hash = Fnv1a::new();
        for descriptor in &self.model_descriptors {
            model_hash.write_text(descriptor);
        }
        let manifest = RunManifest {
            run_id: identity.run_id.into(),
            code_commit: identity.code_commit.into(),
            config_hash: identity.config_hash.into(),
            data_fingerprint: data_fingerprint.into(),
            input_components: BTreeMap::new(),
            clock_start: self.clock_start,
            clock_end: self.clock_end,
            global_seed: 0,
            determinism_mode: true,
            result_hash: format!("{:016x}", self.result_hash()),
            strategy_version: identity.strategy_version.into(),
            instrument_spec_version: identity.instrument_spec_version.into(),
            model_fingerprint: format!("{:016x}", model_hash.finish()),
            input_event_hash: format!("{:016x}", self.input_data_hash),
            output_event_hash: format!("{:016x}", self.result_hash()),
            runtime_version: identity.runtime_version.into(),
            canonical_run_spec_json: None,
            canonical_run_spec_fingerprint: None,
        };
        manifest.validate()?;
        Ok(manifest)
    }
}

pub(super) fn accumulate_exact_fees(
    totals: &mut BTreeMap<String, ExactDecimal>,
    fill: &Fill,
    default_currency: &str,
) -> Result<(), String> {
    if !fill.exact_fees.is_empty() {
        if fill.fee.raw() != 0 || !fill.fees.is_empty() {
            return Err("同一精确成交不能混用 raw fees 与 exact_fees".into());
        }
        for charge in &fill.exact_fees {
            if charge.amount.is_negative() {
                return Err("精确手续费不能为负数".into());
            }
            if charge.amount.is_zero() {
                continue;
            }
            let currency = charge
                .currency
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "非零精确手续费必须声明币种".to_string())?
                .to_ascii_uppercase();
            add_exact_fee(totals, currency, charge.amount)?;
        }
        return Ok(());
    }

    for charge in fill.fee_charges()? {
        if charge.amount.raw() == 0 {
            continue;
        }
        let currency = charge
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(default_currency)
            .to_ascii_uppercase();
        let amount =
            ExactDecimal::from_parts(charge.amount.raw(), 9).map_err(|error| error.to_string())?;
        add_exact_fee(totals, currency, amount)?;
    }
    Ok(())
}

fn add_exact_fee(
    totals: &mut BTreeMap<String, ExactDecimal>,
    currency: String,
    amount: ExactDecimal,
) -> Result<(), String> {
    let current = totals.entry(currency).or_insert(ExactDecimal::ZERO);
    *current = current
        .checked_add(amount)
        .ok_or_else(|| "按币种汇总手续费溢出".to_string())?;
    Ok(())
}

pub(super) fn exact_equity_metrics_json(
    initial: ExactDecimal,
    curve: &[ExactDecimal],
) -> Result<serde_json::Value, String> {
    let metrics = exact_equity_metrics(initial, curve)?;
    Ok(serde_json::json!({
        "initial": metrics.initial.to_string(),
        "final": metrics.final_equity.to_string(),
        "return": metrics.return_amount.to_string(),
        "return_bps": metrics.return_bps.map(|value| value.to_string()),
        "max_drawdown": metrics.max_drawdown.to_string(),
        "max_drawdown_bps": metrics.max_drawdown_bps.to_string(),
    }))
}

pub(super) struct ExactEquityMetrics {
    pub initial: ExactDecimal,
    pub final_equity: ExactDecimal,
    pub return_amount: ExactDecimal,
    pub return_bps: Option<i128>,
    pub max_drawdown: ExactDecimal,
    pub max_drawdown_bps: i128,
}

pub(super) fn exact_equity_metrics(
    initial: ExactDecimal,
    curve: &[ExactDecimal],
) -> Result<ExactEquityMetrics, String> {
    let final_equity = curve.last().copied().unwrap_or(initial);
    let return_amount = final_equity
        .checked_sub(initial)
        .ok_or_else(|| "精确深度回测收益计算溢出".to_string())?;
    let return_bps = exact_ratio_bps(return_amount, initial);
    let mut peak = initial;
    let mut max_drawdown = ExactDecimal::ZERO;
    let mut max_drawdown_bps = 0_i128;
    for equity in curve {
        if peak
            .checked_cmp(*equity)
            .is_some_and(|ordering| ordering.is_lt())
        {
            peak = *equity;
        }
        let drawdown = peak
            .checked_sub(*equity)
            .ok_or_else(|| "精确深度回撤计算溢出".to_string())?;
        if drawdown
            .checked_cmp(max_drawdown)
            .is_some_and(|ordering| ordering.is_gt())
        {
            max_drawdown = drawdown;
        }
        if peak.is_positive() {
            max_drawdown_bps = max_drawdown_bps.max(exact_ratio_bps(drawdown, peak).unwrap_or(0));
        }
    }
    Ok(ExactEquityMetrics {
        initial,
        final_equity,
        return_amount,
        return_bps,
        max_drawdown,
        max_drawdown_bps,
    })
}

fn exact_ratio_bps(numerator: ExactDecimal, denominator: ExactDecimal) -> Option<i128> {
    if !denominator.is_positive() {
        return None;
    }
    numerator
        .checked_mul_at_scale(
            ExactDecimal::from_parts(10_000, 0).ok()?,
            ExactDecimal::MAX_SCALE,
            DecimalRounding::TowardZero,
        )?
        .checked_div_at_scale(denominator, 0, DecimalRounding::TowardZero)
        .map(ExactDecimal::coefficient)
}

impl QueuePositionSensitivityReport {
    /// Compare a disabled baseline and an explicitly enabled FIFO estimate over the same snapshots.
    pub fn compare(
        baseline: &OrderBookBacktestReport,
        estimated: &OrderBookBacktestReport,
    ) -> Result<Self, String> {
        if baseline.input_data_hash != estimated.input_data_hash {
            return Err("queue-position sensitivity requires identical input_data_hash".into());
        }
        let baseline_model_descriptor = baseline
            .model_descriptors
            .iter()
            .find(|value| value.contains("queue_position=disabled"))
            .ok_or_else(|| {
                "baseline report does not declare queue_position=disabled".to_string()
            })?;
        let estimated_model_descriptor = estimated
            .model_descriptors
            .iter()
            .find(|value| {
                value.contains("queue_position=fifo_volume_estimate@v1[")
                    || value.contains("queue_position=fifo_volume_estimate@v2[")
            })
            .ok_or_else(|| "estimated report does not declare the FIFO volume model".to_string())?;
        let baseline_matching = matching_model_parts(baseline_model_descriptor)?;
        let estimated_matching = matching_model_parts(estimated_model_descriptor)?;
        if baseline_matching.0 != estimated_matching.0
            || baseline_matching.1 != estimated_matching.1
        {
            return Err(
                "queue-position sensitivity requires identical matching parameters apart from the queue model".into(),
            );
        }
        let comparable_descriptors = |report: &OrderBookBacktestReport| {
            report
                .model_descriptors
                .iter()
                .filter(|value| {
                    !value.starts_with("orderbook-matching@")
                        && !value.starts_with("limit_order_fill_rate_bps=")
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        if comparable_descriptors(baseline) != comparable_descriptors(estimated) {
            return Err(
                "queue-position sensitivity requires identical risk, ledger, and data-tier descriptors".into(),
            );
        }
        let baseline_rate = baseline
            .limit_order_fill_rate_bps()
            .ok_or_else(|| "baseline report has no submitted limit orders".to_string())?;
        let estimated_rate = estimated
            .limit_order_fill_rate_bps()
            .ok_or_else(|| "estimated report has no submitted limit orders".to_string())?;
        let delta_bps = estimated_rate as i32 - baseline_rate as i32;
        let descriptor = format!(
            "queue_position_fill_rate_sensitivity@v1[baseline_bps={baseline_rate};estimated_bps={estimated_rate};delta_bps={delta_bps}]"
        );
        Ok(Self {
            schema_version: 1,
            input_data_hash: baseline.input_data_hash,
            baseline_result_hash: format!("{:016x}", baseline.result_hash()),
            estimated_result_hash: format!("{:016x}", estimated.result_hash()),
            baseline_limit_order_fill_rate_bps: baseline_rate,
            estimated_limit_order_fill_rate_bps: estimated_rate,
            delta_bps,
            baseline_model_descriptor: baseline_model_descriptor.clone(),
            estimated_model_descriptor: estimated_model_descriptor.clone(),
            descriptor,
        })
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| error.to_string())
    }
}

fn matching_model_parts(descriptor: &str) -> Result<(String, String), String> {
    let (before_queue, queue_and_after) = descriptor
        .split_once(" queue_position=")
        .ok_or_else(|| "matching descriptor is missing queue_position".to_string())?;
    let after_queue = if let Some(rest) = queue_and_after.strip_prefix("disabled") {
        rest
    } else if let Some(rest) = queue_and_after
        .strip_prefix("fifo_volume_estimate@v1[")
        .or_else(|| queue_and_after.strip_prefix("fifo_volume_estimate@v2["))
    {
        rest.split_once(']')
            .ok_or_else(|| "FIFO queue descriptor is incomplete".to_string())?
            .1
    } else {
        return Err("matching descriptor uses an unsupported queue model".into());
    };
    Ok((before_queue.to_string(), after_queue.trim().to_string()))
}

#[cfg(test)]
mod exact_metrics_tests {
    use super::exact_equity_metrics_json;
    use qx_core::ExactDecimal;

    #[test]
    fn exact_depth_return_and_drawdown_keep_sub_nano_changes() {
        let initial = ExactDecimal::parse("0.0000000019").unwrap();
        let curve = [
            ExactDecimal::parse("0.0000000020").unwrap(),
            ExactDecimal::parse("0.0000000019").unwrap(),
            ExactDecimal::parse("0.0000000020").unwrap(),
        ];
        let metrics = exact_equity_metrics_json(initial, &curve).unwrap();
        assert_eq!(metrics["return_bps"], "526");
        assert_eq!(metrics["max_drawdown_bps"], "500");
        assert_eq!(metrics["final"], "0.000000002");
    }
}
