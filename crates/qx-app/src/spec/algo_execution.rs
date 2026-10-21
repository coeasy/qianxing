use crate::{AppError, AppErrorCategory};
use qx_zhenlu::VwapVolumeBucket;
use serde::{Deserialize, Serialize};

pub const ALGO_EXECUTION_SPEC_SCHEMA_VERSION: u32 = 1;
pub const ALGO_EXECUTION_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlgoExecutionMethod {
    Twap,
    Vwap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlgoExecutionSpec {
    pub schema_version: u32,
    pub parent_id: String,
    pub total_quantity_raw: i128,
    pub start_ts: u64,
    pub end_ts: u64,
    pub method: AlgoExecutionMethod,
    /// TWAP child count; VWAP may omit it and uses the frozen volume profile length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_count: Option<usize>,
    #[serde(default)]
    pub volume_profile: Vec<VwapVolumeBucket>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlgoExecutionPlan {
    pub schema_version: u32,
    pub method: AlgoExecutionMethod,
    pub parent_id: String,
    pub children: Vec<qx_zhenlu::AlgoChildSlice>,
}

impl AlgoExecutionSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("AlgoExecutionSpec JSON 无效: {error}"),
            )
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("序列化 AlgoExecutionSpec 失败: {error}"),
            )
        })
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != ALGO_EXECUTION_SPEC_SCHEMA_VERSION
            || self.parent_id.trim().is_empty()
            || self.total_quantity_raw <= 0
            || self.end_ts <= self.start_ts
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "算法执行规格身份、数量、时间范围或 schema_version 无效",
            ));
        }
        match self.method {
            AlgoExecutionMethod::Twap => {
                let count = self.child_count.ok_or_else(|| {
                    AppError::new(AppErrorCategory::InvalidInput, "TWAP 必须提供 child_count")
                })?;
                if count == 0
                    || count > qx_zhenlu::MAX_ALGO_CHILDREN
                    || count as i128 > self.total_quantity_raw
                    || !self.volume_profile.is_empty()
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "TWAP child_count 超限、数量不足或包含不适用的 VWAP profile",
                    ));
                }
            }
            AlgoExecutionMethod::Vwap => {
                if self.child_count.is_some()
                    || self.volume_profile.is_empty()
                    || self.volume_profile.len() > qx_zhenlu::MAX_ALGO_CHILDREN
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "VWAP 必须仅提供 1..=10000 个 volume_profile 桶",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl AlgoExecutionPlan {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("序列化 AlgoExecutionPlan 失败: {error}"),
            )
        })
    }
}
