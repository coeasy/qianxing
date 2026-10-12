use super::{is_identity_token, AppError, AppErrorCategory};
use qx_zhenlu::portfolio::{RiskParityAsset, RiskParityPlan};
use serde::{Deserialize, Serialize};

pub const RISK_PARITY_SPEC_SCHEMA_VERSION: u32 = 1;
pub const RISK_PARITY_RESULT_SCHEMA_VERSION: u32 = 1;

/// Explicitly frozen risk inputs for a long-only equal-risk-contribution rebalance plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskParityAssetSpec {
    pub instrument: String,
    pub mark_price_raw: i128,
    pub returns_bps: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskParitySpec {
    pub schema_version: u32,
    pub portfolio_id: String,
    pub available_cash_raw: i128,
    pub target_gross_notional_raw: i128,
    pub current_positions: std::collections::BTreeMap<String, i128>,
    pub assets: Vec<RiskParityAssetSpec>,
    pub max_turnover_bps: u32,
    pub min_trade_notional_raw: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskParityResult {
    pub schema_version: u32,
    pub portfolio_id: String,
    pub plan_hash: String,
    pub plan: RiskParityPlan,
}

impl RiskParitySpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("RiskParitySpec JSON 无效: {error}"),
            )
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RiskParitySpec 序列化失败: {error}"),
            )
        })
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != RISK_PARITY_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "RiskParitySpec schema_version={} 本版本只认 {}",
                    self.schema_version, RISK_PARITY_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.portfolio_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "RiskParitySpec.portfolio_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        Ok(())
    }

    pub(crate) fn into_domain(
        self,
    ) -> Result<(String, qx_zhenlu::portfolio::RiskParityInput), AppError> {
        self.validate()?;
        let mut assets = Vec::with_capacity(self.assets.len());
        for asset in self.assets {
            let instrument = qx_core::InstrumentId::parse(&asset.instrument).ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("RiskParitySpec instrument 非法: {}", asset.instrument),
                )
            })?;
            assets.push(RiskParityAsset {
                instrument,
                mark_price_raw: asset.mark_price_raw,
                returns_bps: asset.returns_bps,
            });
        }
        Ok((
            self.portfolio_id,
            qx_zhenlu::portfolio::RiskParityInput {
                available_cash_raw: self.available_cash_raw,
                target_gross_notional_raw: self.target_gross_notional_raw,
                current_positions: self.current_positions,
                assets,
                max_turnover_bps: self.max_turnover_bps,
                min_trade_notional_raw: self.min_trade_notional_raw,
            },
        ))
    }
}

impl RiskParityResult {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RiskParityResult 序列化失败: {error}"),
            )
        })
    }
}
