//! Strict versioned contract for deterministic Tick and OrderBook backtests.

use super::{is_identity_token, BacktestArtifacts, BuiltinStrategySpec};
use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

/// Schema for deterministic L1/L2 market-data backtests.
pub const DEPTH_BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

/// L1 frames execute through the Rust Tick engine; L2 frames use the Rust book engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub depth_path: String,
    /// Exactly `l1` or `l2`; this determines the Rust matching engine.
    pub tier: String,
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
    pub fee_bps: i128,
    pub latency_snapshots: u64,
    pub queue_position_bps: i128,
    pub market_impact_bps: i128,
    pub output_dir: String,
    pub strategy: BuiltinStrategySpec,
}

impl DepthBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DEPTH_BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DepthBacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        for (field, value) in [
            ("depth_path", &self.depth_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthBacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if !matches!(self.tier.as_str(), "l1" | "l2") {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.tier 仅支持 l1 或 l2",
            ));
        }
        if self.initial_cash_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.initial_cash_raw 必须为正",
            ));
        }
        for (field, value) in [
            ("fee_bps", self.fee_bps),
            ("queue_position_bps", self.queue_position_bps),
            ("market_impact_bps", self.market_impact_bps),
        ] {
            if !(0..=10_000).contains(&value) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthBacktestSpec.{field} 必须在 0..=10000 内"),
                ));
            }
        }
        // The current facade accepts built-ins, which submit market orders.
        // Queue position only affects limit fills; rejecting it avoids a silent no-op.
        if self.queue_position_bps != 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "queue_position_bps 仅影响限价单；当前内置策略只提交市价单，必须为 0",
            ));
        }
        self.strategy.validate()
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DepthBacktestSpec JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DepthBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

/// Stable result for an L1 Tick or L2 order-book run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthBacktestOutcome {
    pub run_id: String,
    pub instrument: String,
    pub tier: String,
    pub result_hash: String,
    pub data_fingerprint: String,
    pub fills: u64,
    pub equity_points: u64,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub artifacts: BacktestArtifacts,
}

impl DepthBacktestOutcome {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DepthBacktestOutcome JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DepthBacktestOutcome 序列化失败: {error}"),
            )
        })
    }
}
