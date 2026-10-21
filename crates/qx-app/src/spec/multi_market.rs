//! Same-account backtests across three or more independently sampled markets.

use super::{is_identity_token, MakerTakerFeeSchedule, PortfolioStrategySpec};
use crate::error::{AppError, AppErrorCategory};
use qx_core::{InstrumentId, TradingInstrumentSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MULTI_MARKET_BACKTEST_SCHEMA_VERSION: u32 = 1;
pub const MIN_MULTI_MARKETS: usize = 3;
pub const MAX_MULTI_MARKETS: usize = 32;
pub const MAX_STRATEGIES_PER_MARKET: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketBacktestMarket {
    pub instrument: String,
    pub bars_path: String,
    /// Optional DatasetManifestV2 quality evidence for this BarFrame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_manifest_path: Option<String>,
    /// Stable order for same-timestamp market events; instrument id breaks ties.
    pub priority: u32,
    pub market_spec: Option<TradingInstrumentSpec>,
    pub fee_schedule: Option<MakerTakerFeeSchedule>,
    pub strategies: Vec<PortfolioStrategySpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub account_id: String,
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
    pub seed: u64,
    pub output_dir: String,
    pub markets: Vec<MultiMarketBacktestMarket>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiMarketBacktestOutcome {
    pub run_id: String,
    pub account_id: String,
    pub spec_fingerprint: String,
    pub data_fingerprint: String,
    pub shared_result_hash: String,
    pub markets_total: usize,
    pub strategies_total: usize,
    pub successful_fills: u64,
    pub equity_points: u64,
    pub final_equity_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub artifact_path: String,
}

impl MultiMarketBacktestOutcome {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiMarketBacktestOutcome 序列化失败: {error}"),
            )
        })
    }
}

impl MultiMarketBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_MARKET_BACKTEST_SCHEMA_VERSION {
            return Err(invalid(format!(
                "MultiMarketBacktestSpec schema_version={} 本版本只认 {}",
                self.schema_version, MULTI_MARKET_BACKTEST_SCHEMA_VERSION
            )));
        }
        if !is_identity_token(&self.run_id) || !is_identity_token(&self.account_id) {
            return Err(invalid(
                "多市场回测 run_id/account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if self.settlement_currency.trim().is_empty() || self.output_dir.trim().is_empty() {
            return Err(invalid(
                "多市场回测 settlement_currency 与 output_dir 不能为空",
            ));
        }
        if self.initial_cash_raw <= 0 {
            return Err(invalid("多市场回测 initial_cash_raw 必须为正数"));
        }
        if !(MIN_MULTI_MARKETS..=MAX_MULTI_MARKETS).contains(&self.markets.len()) {
            return Err(invalid(format!(
                "多市场回测 markets 数量必须在 {MIN_MULTI_MARKETS}..={MAX_MULTI_MARKETS}"
            )));
        }
        let currency = self.settlement_currency.to_ascii_uppercase();
        if currency != self.settlement_currency {
            return Err(invalid("settlement_currency 必须使用大写规范代码"));
        }
        let mut instruments = BTreeSet::new();
        let mut strategy_ids = BTreeSet::new();
        let mut reserved = 0_i128;
        for market in &self.markets {
            let instrument = InstrumentId::parse(&market.instrument)
                .filter(|parsed| parsed.to_string() == market.instrument)
                .ok_or_else(|| {
                    invalid(format!(
                        "市场标的不是规范 InstrumentId: {}",
                        market.instrument
                    ))
                })?;
            if !instruments.insert(instrument.clone()) {
                return Err(invalid(format!("多市场回测标的重复: {instrument}")));
            }
            if market.bars_path.trim().is_empty() {
                return Err(invalid(format!("市场 {instrument} bars_path 不能为空")));
            }
            if market
                .dataset_manifest_path
                .as_deref()
                .is_some_and(|path| path.trim().is_empty())
            {
                return Err(invalid(format!(
                    "市场 {instrument} dataset_manifest_path 若在场就不能为空"
                )));
            }
            if market.strategies.is_empty() || market.strategies.len() > MAX_STRATEGIES_PER_MARKET {
                return Err(invalid(format!(
                    "市场 {instrument} 策略数必须在 1..={MAX_STRATEGIES_PER_MARKET}"
                )));
            }
            if let Some(contract) = &market.market_spec {
                contract
                    .validate()
                    .map_err(|error| AppError::from_qx_error(&error))?;
                if contract.instrument != instrument || contract.settlement_currency != currency {
                    return Err(invalid(format!(
                        "市场 {instrument} 合约标的或结算币与组合规格不一致"
                    )));
                }
                if contract.product == qx_core::TradingProduct::Option {
                    return Err(invalid("多市场 Bar 回测暂不支持期权合约"));
                }
                if market.fee_schedule.is_none() {
                    return Err(invalid(format!(
                        "市场 {instrument} 显式 market_spec 时必须显式提供 fee_schedule"
                    )));
                }
            }
            if let Some(fees) = market.fee_schedule {
                fees.validate()
                    .map_err(|error| AppError::from_qx_error(&error))?;
            }
            for member in &market.strategies {
                member.strategy.validate()?;
                if member.strategy.kind.needs_reference_leg() {
                    return Err(invalid(format!(
                        "市场 {instrument} 的双腿策略必须使用多腿回测入口"
                    )));
                }
                if !strategy_ids.insert(member.strategy.strategy_id.as_str()) {
                    return Err(invalid(format!(
                        "多市场回测 strategy_id 重复: {}",
                        member.strategy.strategy_id
                    )));
                }
                if member.reserved_capital_raw <= 0 {
                    return Err(invalid(format!(
                        "策略 {} reserved_capital_raw 必须为正数",
                        member.strategy.strategy_id
                    )));
                }
                reserved = reserved
                    .checked_add(member.reserved_capital_raw)
                    .ok_or_else(|| invalid("多市场策略预算求和溢出"))?;
            }
        }
        if reserved > self.initial_cash_raw {
            return Err(invalid("多市场策略预算合计不得超过账户初始资金"));
        }
        Ok(())
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("MultiMarketBacktestSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiMarketBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
