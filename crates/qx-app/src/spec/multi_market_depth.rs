//! Versioned same-account multi-market L2 backtest contract.

use super::{is_identity_token, BuiltinStrategySpec};
use crate::error::{AppError, AppErrorCategory};
use qx_core::{InstrumentId, OrderPolicy, TradingInstrumentSpec, TradingProduct};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MULTI_MARKET_DEPTH_BACKTEST_SCHEMA_VERSION: u32 = 1;
pub const MIN_MULTI_MARKET_DEPTH_MARKETS: usize = 2;
pub const MAX_MULTI_MARKET_DEPTH_MARKETS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketDepthBacktestMarket {
    pub instrument: String,
    pub depth_path: String,
    /// Optional market-data tier: `l1` emits Tick events and requires one bid/ask level;
    /// `l2` (the backward-compatible default) emits full OrderBook events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_tier: Option<String>,
    /// Lower values are processed first when market timestamps match.
    pub priority: u32,
    pub market_spec: TradingInstrumentSpec,
    pub fee_bps: i128,
    pub latency_snapshots: u64,
    pub queue_position_bps: i128,
    pub market_impact_bps: i128,
    pub strategy: BuiltinStrategySpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_policy: Option<OrderPolicy>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketDepthBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub account_id: String,
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
    pub output_dir: String,
    pub markets: Vec<MultiMarketDepthBacktestMarket>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiMarketDepthBacktestOutcome {
    pub run_id: String,
    pub account_id: String,
    pub spec_fingerprint: String,
    pub data_fingerprint: String,
    pub shared_result_hash: String,
    pub markets_total: usize,
    pub successful_fills: u64,
    pub pending_orders: usize,
    pub equity_points: u64,
    pub final_equity_raw: i128,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub artifact_path: String,
}

impl MultiMarketDepthBacktestOutcome {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiMarketDepthBacktestOutcome 序列化失败: {error}"),
            )
        })
    }
}

impl MultiMarketDepthBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_MARKET_DEPTH_BACKTEST_SCHEMA_VERSION {
            return Err(invalid(format!(
                "MultiMarketDepthBacktestSpec schema_version={} 本版本只认 {}",
                self.schema_version, MULTI_MARKET_DEPTH_BACKTEST_SCHEMA_VERSION
            )));
        }
        if !is_identity_token(&self.run_id) || !is_identity_token(&self.account_id) {
            return Err(invalid(
                "多市场盘口回测 run_id/account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if self.settlement_currency.trim().is_empty()
            || self.settlement_currency != self.settlement_currency.to_ascii_uppercase()
            || self.output_dir.trim().is_empty()
        {
            return Err(invalid(
                "多市场盘口回测 settlement_currency 必须为大写代码且 output_dir 不能为空",
            ));
        }
        if self.initial_cash_raw <= 0 {
            return Err(invalid("多市场盘口回测 initial_cash_raw 必须为正数"));
        }
        if !(MIN_MULTI_MARKET_DEPTH_MARKETS..=MAX_MULTI_MARKET_DEPTH_MARKETS)
            .contains(&self.markets.len())
        {
            return Err(invalid(format!(
                "多市场盘口回测 markets 数量必须在 {MIN_MULTI_MARKET_DEPTH_MARKETS}..={MAX_MULTI_MARKET_DEPTH_MARKETS}"
            )));
        }
        let mut instruments = BTreeSet::new();
        let mut strategy_ids = BTreeSet::new();
        for market in &self.markets {
            let instrument = InstrumentId::parse(&market.instrument)
                .filter(|parsed| parsed.to_string() == market.instrument)
                .ok_or_else(|| {
                    invalid(format!(
                        "盘口市场标的不是规范 InstrumentId: {}",
                        market.instrument
                    ))
                })?;
            if !instruments.insert(instrument.clone()) {
                return Err(invalid(format!("多市场盘口回测标的重复: {instrument}")));
            }
            if market.depth_path.trim().is_empty() {
                return Err(invalid(format!("市场 {instrument} depth_path 不能为空")));
            }
            if market
                .data_tier
                .as_deref()
                .is_some_and(|tier| !matches!(tier, "l1" | "l2"))
            {
                return Err(invalid(format!(
                    "市场 {instrument} data_tier 仅支持 l1 或 l2"
                )));
            }
            market
                .market_spec
                .validate()
                .map_err(|error| AppError::from_qx_error(&error))?;
            if market.market_spec.instrument != instrument
                || market.market_spec.product != TradingProduct::Spot
                || !market
                    .market_spec
                    .quote_currency
                    .eq_ignore_ascii_case(&self.settlement_currency)
                || !market
                    .market_spec
                    .settlement_currency
                    .eq_ignore_ascii_case(&self.settlement_currency)
            {
                return Err(invalid(format!(
                    "市场 {instrument} 必须是报价/结算币匹配的现货规格；多市场衍生品保证金尚未支持"
                )));
            }
            if !(0..=10_000).contains(&market.fee_bps)
                || !(0..=10_000).contains(&market.queue_position_bps)
                || !(0..=10_000).contains(&market.market_impact_bps)
            {
                return Err(invalid(format!(
                    "市场 {instrument} fee/queue/impact bps 必须在 0..=10000"
                )));
            }
            market
                .strategy
                .validate()
                .map_err(|error| invalid(error.to_string()))?;
            if market.strategy.kind.needs_reference_leg()
                || !strategy_ids.insert(market.strategy.strategy_id.as_str())
            {
                return Err(invalid(format!(
                    "市场 {instrument} 策略必须是唯一的单市场策略"
                )));
            }
            market
                .order_policy
                .unwrap_or_default()
                .validate_for(&market.market_spec)
                .map_err(|error| invalid(error.to_string()))?;
        }
        Ok(())
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("MultiMarketDepthBacktestSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiMarketDepthBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
