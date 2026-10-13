//! Bounded independent-account batches for the shared Tick/OrderBook kernel.

use super::{DepthBacktestOutcome, DepthBacktestSpec, MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION};
use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A multi-account batch whose members run the existing depth backtest use case independently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiAccountDepthBacktestSpec {
    pub schema_version: u32,
    pub batch_id: String,
    pub runs: Vec<DepthBacktestSpec>,
}

impl MultiAccountDepthBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION {
            return Err(invalid(format!(
                "MultiAccountDepthBacktestSpec schema_version={} 本版本只认 {}",
                self.schema_version, MULTI_ACCOUNT_BACKTEST_SCHEMA_VERSION
            )));
        }
        if !super::is_identity_token(&self.batch_id) {
            return Err(invalid(
                "MultiAccountDepthBacktestSpec.batch_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if !(2..=super::MAX_MULTI_ACCOUNT_RUNS).contains(&self.runs.len()) {
            return Err(invalid(format!(
                "MultiAccountDepthBacktestSpec.runs 数量必须在 2..={}",
                super::MAX_MULTI_ACCOUNT_RUNS
            )));
        }

        let mut accounts = BTreeSet::new();
        let mut run_ids = BTreeSet::new();
        let mut settlement_currency: Option<&str> = None;
        for run in &self.runs {
            run.validate()?;
            let account_id = run.account_id.as_deref().ok_or_else(|| {
                invalid("多账户深度回测的每个 DepthBacktestSpec 都必须显式声明 account_id")
            })?;
            if !accounts.insert(account_id) {
                return Err(invalid(format!(
                    "多账户深度回测 account_id 重复: {account_id}"
                )));
            }
            if !run_ids.insert(run.run_id.as_str()) {
                return Err(invalid(format!(
                    "多账户深度回测 run_id 重复: {}",
                    run.run_id
                )));
            }
            match settlement_currency {
                Some(currency) if currency != run.settlement_currency => {
                    return Err(invalid(
                        "多账户深度回测成员必须使用相同 settlement_currency；跨币种汇总需要显式 FX 数据与时点",
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
            invalid(format!("MultiAccountDepthBacktestSpec JSON 无效: {error}"))
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiAccountDepthBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountDepthRunResult {
    pub account_id: String,
    pub outcome: Option<DepthBacktestOutcome>,
    pub error_json: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountDepthBacktestSummary {
    pub accounts_total: usize,
    pub accounts_succeeded: usize,
    pub accounts_failed: usize,
    pub successful_initial_cash_raw: i128,
    pub successful_fills: u64,
    pub weighted_return_bps: Option<i32>,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiAccountDepthBacktestResult {
    pub batch_id: String,
    pub settlement_currency: String,
    pub spec_fingerprint: String,
    pub runs: Vec<MultiAccountDepthRunResult>,
    pub summary: MultiAccountDepthBacktestSummary,
}

impl MultiAccountDepthBacktestResult {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiAccountDepthBacktestResult 序列化失败: {error}"),
            )
        })
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiAccountDepthBacktestResult JSON 无效: {error}"),
            )
        })
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
