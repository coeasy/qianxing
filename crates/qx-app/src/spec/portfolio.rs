//! Same-account Bar portfolio specification and fail-closed validation.

use super::BuiltinStrategySpec;
use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortfolioStrategySpec {
    /// Same-account scheduling priority; lower values decide first.
    pub priority: i32,
    pub reserved_capital_raw: i128,
    pub strategy: BuiltinStrategySpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortfolioBacktestConfig {
    /// Budget and priority for `BacktestSpec.strategy`, the primary member.
    pub primary_priority: i32,
    pub primary_reserved_capital_raw: i128,
    /// Additional strategies share the primary strategy's account and ledger.
    pub additional_strategies: Vec<PortfolioStrategySpec>,
}

impl PortfolioBacktestConfig {
    pub(super) fn validate(
        &self,
        primary: &BuiltinStrategySpec,
        initial_cash_raw: i128,
    ) -> Result<(), AppError> {
        if self.additional_strategies.is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.portfolio 至少需要一个附属策略；单策略请省略 portfolio",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut reserved = self.primary_reserved_capital_raw;
        if reserved <= 0 || !ids.insert(primary.strategy_id.as_str()) {
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
            if member.reserved_capital_raw <= 0
                || !ids.insert(member.strategy.strategy_id.as_str())
                || member.strategy.kind.needs_reference_leg()
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "组合附属策略必须是唯一单腿策略且预算为正",
                ));
            }
            member.strategy.validate()?;
            reserved = reserved
                .checked_add(member.reserved_capital_raw)
                .ok_or_else(|| AppError::new(AppErrorCategory::InvalidInput, "组合预算求和溢出"))?;
        }
        if reserved > initial_cash_raw {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "组合策略预算合计不得超过回测账户初始资金",
            ));
        }
        Ok(())
    }
}
