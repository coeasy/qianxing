//! Same-account Bar portfolio specification and fail-closed validation.

use super::BuiltinStrategySpec;
use crate::error::{AppError, AppErrorCategory};
use qx_core::{DecimalRounding, ExactDecimal};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortfolioStrategySpec {
    /// Same-account scheduling priority; lower values decide first.
    pub priority: i32,
    pub reserved_capital_raw: i128,
    /// Scale-aware budget. The raw field remains its toward-zero 1e-9 projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_capital_exact: Option<ExactDecimal>,
    pub strategy: BuiltinStrategySpec,
}

impl PortfolioStrategySpec {
    pub fn reserved_capital(&self) -> Result<ExactDecimal, String> {
        match self.reserved_capital_exact {
            Some(amount) => {
                let projected = amount
                    .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                    .and_then(|value| value.to_raw(9).ok());
                if !amount.is_positive() || projected != Some(self.reserved_capital_raw) {
                    return Err(format!(
                        "策略 {} reserved_capital_exact 必须为正且 raw 字段必须等于向零投影",
                        self.strategy.strategy_id
                    ));
                }
                Ok(amount)
            }
            None if self.reserved_capital_raw > 0 => {
                ExactDecimal::from_parts(self.reserved_capital_raw, 9)
                    .map_err(|_| "组合策略预算无法转换为 ExactDecimal".to_string())
            }
            None => Err(format!(
                "策略 {} reserved_capital_raw 必须为正，或提供 reserved_capital_exact",
                self.strategy.strategy_id
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortfolioBacktestConfig {
    /// Budget and priority for `BacktestSpec.strategy`, the primary member.
    pub primary_priority: i32,
    pub primary_reserved_capital_raw: i128,
    /// Scale-aware primary strategy budget. The raw field remains its toward-zero projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_reserved_capital_exact: Option<ExactDecimal>,
    /// Additional strategies share the primary strategy's account and ledger.
    pub additional_strategies: Vec<PortfolioStrategySpec>,
}

impl PortfolioBacktestConfig {
    pub fn primary_reserved_capital(&self) -> Result<ExactDecimal, String> {
        match self.primary_reserved_capital_exact {
            Some(amount) => {
                let projected = amount
                    .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
                    .and_then(|value| value.to_raw(9).ok());
                if !amount.is_positive() || projected != Some(self.primary_reserved_capital_raw) {
                    return Err(
                        "primary_reserved_capital_exact 必须为正且 raw 字段必须等于向零投影".into(),
                    );
                }
                Ok(amount)
            }
            None if self.primary_reserved_capital_raw > 0 => {
                ExactDecimal::from_parts(self.primary_reserved_capital_raw, 9)
                    .map_err(|_| "组合主策略预算无法转换为 ExactDecimal".to_string())
            }
            None => Err(
                "primary_reserved_capital_raw 必须为正，或提供 primary_reserved_capital_exact"
                    .into(),
            ),
        }
    }
}

impl PortfolioBacktestConfig {
    pub(super) fn validate(
        &self,
        primary: &BuiltinStrategySpec,
        initial_cash_raw: i128,
        initial_cash_exact: Option<ExactDecimal>,
    ) -> Result<(), AppError> {
        if self.additional_strategies.is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "组合回测规格至少需要一个附属策略；单策略请省略 portfolio",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut reserved = self
            .primary_reserved_capital()
            .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
        if !ids.insert(primary.strategy_id.as_str()) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "组合主策略预算必须为正且 strategy_id 必须唯一",
            ));
        }
        if primary.kind.needs_reference_leg() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "双腿策略须使用多腿回测规格，不能放入单标的策略组合",
            ));
        }
        for member in &self.additional_strategies {
            let member_budget = member
                .reserved_capital()
                .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
            if !ids.insert(member.strategy.strategy_id.as_str())
                || member.strategy.kind.needs_reference_leg()
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "组合附属策略必须是唯一单腿策略且预算为正",
                ));
            }
            member.strategy.validate()?;
            reserved = reserved.checked_add(member_budget).ok_or_else(|| {
                AppError::new(AppErrorCategory::InvalidInput, "组合精确预算求和溢出")
            })?;
        }
        let initial_cash = match initial_cash_exact {
            Some(amount) => amount,
            None => ExactDecimal::from_parts(initial_cash_raw, 9)
                .map_err(|_| AppError::new(AppErrorCategory::InvalidInput, "回测初始资金溢出"))?,
        };
        if reserved
            .checked_cmp(initial_cash)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "组合策略预算合计不得超过回测账户初始资金",
            ));
        }
        Ok(())
    }
}
