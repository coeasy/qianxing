//! Versioned contract for a deterministic two-leg Bar backtest.

use super::{is_identity_token, BuiltinStrategySpec};
use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

pub const MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

/// Both input BarFrames must have identical timestamps and distinct instruments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiLegBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub primary_bars_path: String,
    pub reference_bars_path: String,
    pub settlement_currency: String,
    pub primary_initial_cash_raw: i128,
    pub reference_initial_cash_raw: i128,
    pub seed: u64,
    pub output_dir: String,
    pub strategy: BuiltinStrategySpec,
}

impl MultiLegBacktestSpec {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: impl Into<String>,
        primary_bars_path: impl Into<String>,
        reference_bars_path: impl Into<String>,
        settlement_currency: impl Into<String>,
        primary_initial_cash_raw: i128,
        reference_initial_cash_raw: i128,
        seed: u64,
        output_dir: impl Into<String>,
        strategy: BuiltinStrategySpec,
    ) -> Self {
        Self {
            schema_version: MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: run_id.into(),
            primary_bars_path: primary_bars_path.into(),
            reference_bars_path: reference_bars_path.into(),
            settlement_currency: settlement_currency.into(),
            primary_initial_cash_raw,
            reference_initial_cash_raw,
            seed,
            output_dir: output_dir.into(),
            strategy,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "MultiLegBacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "MultiLegBacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        for (field, value) in [
            ("primary_bars_path", &self.primary_bars_path),
            ("reference_bars_path", &self.reference_bars_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("MultiLegBacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if self.primary_bars_path == self.reference_bars_path {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿 BarFrame 必须来自两份独立输入",
            ));
        }
        if self.primary_initial_cash_raw <= 0 || self.reference_initial_cash_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿两侧 initial_cash_raw 都必须为正数",
            ));
        }
        if !self.strategy.kind.needs_reference_leg() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿回测只接受需要 reference leg 的内置套利策略",
            ));
        }
        self.strategy.validate()
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiLegBacktestSpec JSON 无效: {error}"),
            )
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiLegBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

/// Stable outcome shared by Rust, Python, and HTTP callers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegBacktestOutcome {
    pub run_id: String,
    pub primary_instrument: String,
    pub reference_instrument: String,
    pub primary_result_hash: String,
    pub reference_result_hash: String,
    pub combined_return_bps: i32,
    pub primary_fills: u64,
    pub reference_fills: u64,
    pub attribution_artifact: String,
}

impl MultiLegBacktestOutcome {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiLegBacktestOutcome 序列化失败: {error}"),
            )
        })
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiLegBacktestOutcome JSON 无效: {error}"),
            )
        })
    }
}
