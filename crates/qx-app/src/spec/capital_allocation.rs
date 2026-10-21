use super::{is_identity_token, AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

pub use qx_zhenlu::portfolio::{CapitalAllocationAccount, CapitalAllocationRequest};

pub const CAPITAL_ALLOCATION_SPEC_SCHEMA_VERSION: u32 = 1;
pub const CAPITAL_ALLOCATION_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapitalAllocationSpec {
    pub schema_version: u32,
    pub portfolio_id: String,
    pub portfolio_budget_raw: i128,
    pub accounts: Vec<CapitalAllocationAccount>,
    pub requests: Vec<CapitalAllocationRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapitalAllocationResult {
    pub schema_version: u32,
    pub portfolio_id: String,
    pub allocations: Vec<qx_zhenlu::portfolio::CapitalAllocation>,
}

impl CapitalAllocationResult {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("CapitalAllocationResult 序列化失败: {error}"),
            )
        })
    }
}

impl CapitalAllocationSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("CapitalAllocationSpec JSON 无效: {error}"),
            )
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != CAPITAL_ALLOCATION_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "CapitalAllocationSpec schema_version 不受支持",
            ));
        }
        if !is_identity_token(&self.portfolio_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "portfolio_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        Ok(())
    }
}
