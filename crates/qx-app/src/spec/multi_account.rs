//! Deterministic batch backtests over isolated accounts.

use super::{BacktestOutcome, BacktestSpec};
use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION: u32 = 1;
pub const MAX_MULTI_ACCOUNT_RUNS: usize = 32;

/// A batch of independent account backtests. Every member retains its own
/// ledger and run artifacts; this does not net cash, positions, or risk across
/// accounts. Cross-currency aggregation is rejected until an FX contract is
/// included in the run identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiAccountBacktestSpec {
    pub schema_version: u32,
    pub batch_id: String,
    pub runs: Vec<BacktestSpec>,
}

impl MultiAccountBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "MultiAccountBacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION
                ),
            ));
        }
        if !super::is_identity_token(&self.batch_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "MultiAccountBacktestSpec.batch_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if !(2..=MAX_MULTI_ACCOUNT_RUNS).contains(&self.runs.len()) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiAccountBacktestSpec.runs 数量必须在 2..={MAX_MULTI_ACCOUNT_RUNS}"),
            ));
        }

        let mut accounts = BTreeSet::new();
        let mut run_ids = BTreeSet::new();
        let mut settlement_currency: Option<&str> = None;
        for run in &self.runs {
            run.validate()?;
            let account_id = run.account_id.as_deref().ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "多账户回测的每个 BacktestSpec 都必须显式声明 account_id",
                )
            })?;
            if !accounts.insert(account_id) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("多账户回测 account_id 重复: {account_id}"),
                ));
            }
            if !run_ids.insert(run.run_id.as_str()) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("多账户回测 run_id 重复: {}", run.run_id),
                ));
            }
            match settlement_currency {
                Some(currency) if currency != run.settlement_currency => {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "多账户回测成员必须使用相同 settlement_currency；跨币种汇总需要显式 FX 数据与时点",
                    ));
                }
                None => settlement_currency = Some(&run.settlement_currency),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiAccountBacktestSpec JSON 无效: {error}"),
            )
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiAccountBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

/// One isolated account's result. Errors are kept per account so one bad
/// dataset does not erase successful, independently verifiable runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountRunResult {
    pub account_id: String,
    pub outcome: Option<BacktestOutcome>,
    pub error_json: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountBacktestSummary {
    pub accounts_total: usize,
    pub accounts_succeeded: usize,
    pub accounts_failed: usize,
    pub successful_initial_cash_raw: i128,
    pub successful_fills: u64,
    /// Initial-cash-weighted return across successful account runs only.
    /// `complete=false` means this is a partial research result.
    pub weighted_return_bps: Option<i32>,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountBacktestResult {
    pub batch_id: String,
    pub settlement_currency: String,
    pub spec_fingerprint: String,
    pub runs: Vec<MultiAccountRunResult>,
    pub summary: MultiAccountBacktestSummary,
}

impl MultiAccountBacktestResult {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiAccountBacktestResult 序列化失败: {error}"),
            )
        })
    }
}
