use crate::{DataTier, OrderBookExecutionDescriptor, OrderBookExecutionModel, RunManifestIdentity};
use qx_core::{EventLog, Fill, Fnv1a, Ledger, RunManifest, TradingInstrumentSpec};
use serde::{Deserialize, Serialize};

pub struct OrderBookBacktestReport {
    pub fills: Vec<Fill>,
    pub execution_attempts: Vec<crate::ExecutionAttempt>,
    pub ledger: Ledger,
    pub event_log: EventLog,
    pub pending_orders: usize,
    /// 与 Bar 回测同形的统一产物：每个快照结束后的账户权益、净持仓与时间。
    pub equity: Vec<i128>,
    pub positions: Vec<i128>,
    pub snapshot_ts: Vec<u64>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub initial_equity_raw: i128,
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
            model.queue_position_model.descriptor(model.queue_position_bps),
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
        };
        manifest.validate()?;
        Ok(manifest)
    }
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
