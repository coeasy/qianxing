//! Strict versioned contract for deterministic Tick and OrderBook backtests.

use super::{
    is_identity_token, supports_fixed_notional_sizing, BacktestArtifacts, BuiltinStrategySpec,
    PortfolioBacktestConfig,
};
use crate::error::{AppError, AppErrorCategory};
use qx_core::{OrderPolicy, TradingInstrumentSpec};
use qx_xingban::MarketMakingConfig;
use serde::{Deserialize, Serialize};

/// Schema for deterministic L1/L2 market-data backtests.
pub const DEPTH_BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

/// L1 frames execute through the Rust Tick engine; L2 frames use the Rust book engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub depth_path: String,
    /// Exactly `l1` or `l2`; this determines the Rust matching engine.
    pub tier: String,
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
    pub fee_bps: i128,
    pub latency_snapshots: u64,
    pub queue_position_bps: i128,
    pub market_impact_bps: i128,
    pub output_dir: String,
    pub strategy: BuiltinStrategySpec,
    /// Optional normalized market contract. Missing preserves legacy spot semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_spec: Option<TradingInstrumentSpec>,
    /// Required for derivative products so the Rust engine can apply margin/position rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_policy: Option<OrderPolicy>,
    /// Optional L2 two-sided quote strategy executed by the shared Rust book engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_making: Option<MarketMakingConfig>,
    /// Ledger/account ownership for this run. Omitted legacy specs use `app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Same-account strategy portfolio over the shared Tick/OrderBook ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<PortfolioBacktestConfig>,
}

impl DepthBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DEPTH_BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DepthBacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if self
            .account_id
            .as_deref()
            .is_some_and(|account_id| !is_identity_token(account_id))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        for (field, value) in [
            ("depth_path", &self.depth_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthBacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if !matches!(self.tier.as_str(), "l1" | "l2") {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.tier 仅支持 l1 或 l2",
            ));
        }
        if let Some(market_making) = self.market_making {
            if self.portfolio.is_some() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "DepthBacktestSpec 暂不允许把 market_making 与策略组合混用",
                ));
            }
            if self.tier != "l2" {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_making 仅支持 L2 订单簿回测",
                ));
            }
            market_making
                .validate()
                .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
            if self
                .market_spec
                .as_ref()
                .is_some_and(|market| market.price_tick != market_making.tick_size_raw)
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_making.tick_size_raw 必须与 market_spec.price_tick 一致",
                ));
            }
            if strategy_has_sizing(&self.strategy) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_making 使用 quote_qty_raw 控制报价数量，不接受策略仓位 sizing 参数",
                ));
            }
        }
        if self.initial_cash_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.initial_cash_raw 必须为正",
            ));
        }
        for (field, value) in [
            ("fee_bps", self.fee_bps),
            ("queue_position_bps", self.queue_position_bps),
            ("market_impact_bps", self.market_impact_bps),
        ] {
            if !(0..=10_000).contains(&value) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthBacktestSpec.{field} 必须在 0..=10000 内"),
                ));
            }
        }
        // The current facade accepts built-ins, which submit market orders.
        // Queue position only affects limit fills; rejecting it avoids a silent no-op.
        if self.queue_position_bps != 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "queue_position_bps 仅影响限价单；当前内置策略只提交市价单，必须为 0",
            ));
        }
        match (&self.market_spec, self.order_policy) {
            (Some(market), policy) => {
                market.validate().map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("DepthBacktestSpec.market_spec 非法: {error:?}"),
                    )
                })?;
                if market.settlement_currency != self.settlement_currency {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "DepthBacktestSpec.market_spec settlement_currency 与请求不一致",
                    ));
                }
                if self.strategy.target_notional_raw.is_some()
                    && !supports_fixed_notional_sizing(market)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "target_notional_raw 当前只支持现货的一单位线性名义金额；衍生品需先配置合约专用 sizing",
                    ));
                }
                if market.product.supports_leverage() && policy.is_none() {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "Tick/OrderBook 衍生品回测必须显式提供 OrderPolicy",
                    ));
                }
                if let Some(policy) = policy {
                    if policy.reduce_only || policy.post_only {
                        return Err(AppError::new(
                            AppErrorCategory::InvalidInput,
                            "DepthBacktestSpec 的内置策略只提交开仓市价意图；OrderPolicy.reduce_only/post_only 必须为 false",
                        ));
                    }
                    policy.validate_for(market).map_err(|error| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            format!("DepthBacktestSpec.order_policy 非法: {error:?}"),
                        )
                    })?;
                }
            }
            (None, Some(_)) => {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "DepthBacktestSpec.order_policy 需要对应的 market_spec",
                ));
            }
            (None, None) => {}
        }
        self.strategy.validate()?;
        if let Some(portfolio) = &self.portfolio {
            portfolio.validate(&self.strategy, self.initial_cash_raw)?;
            if self
                .order_policy
                .is_some_and(|policy| policy.position_mode != qx_core::PositionMode::OneWay)
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Tick/OrderBook 策略组合目前要求 OneWay 持仓模式",
                ));
            }
        }
        let strategies = std::iter::once(&self.strategy).chain(
            self.portfolio
                .iter()
                .flat_map(|portfolio| portfolio.additional_strategies.iter())
                .map(|member| &member.strategy),
        );
        for strategy in strategies {
            if self
                .market_spec
                .as_ref()
                .is_some_and(|market| market.product.supports_leverage())
                && (strategy.target_available_margin_bps.is_some()
                    || strategy.target_volatility_bps.is_some()
                    || strategy.target_kelly_fraction_bps.is_some())
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Tick/OrderBook 保证金 sizing 需要已认证的衍生品 free-margin 快照；当前仅现货现金 sizing 可用",
                ));
            }
            if strategy.target_notional_raw.is_some()
                && self
                    .market_spec
                    .as_ref()
                    .is_some_and(|market| !supports_fixed_notional_sizing(market))
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "target_notional_raw 当前只支持现货的一单位线性名义金额；衍生品需先配置合约专用 sizing",
                ));
            }
        }
        Ok(())
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DepthBacktestSpec JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DepthBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

fn strategy_has_sizing(strategy: &BuiltinStrategySpec) -> bool {
    strategy.target_notional_raw.is_some()
        || strategy.target_available_margin_bps.is_some()
        || strategy.target_volatility_bps.is_some()
        || strategy.target_kelly_fraction_bps.is_some()
}

/// Stable result for an L1 Tick or L2 order-book run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthBacktestOutcome {
    pub run_id: String,
    pub instrument: String,
    pub tier: String,
    pub result_hash: String,
    pub data_fingerprint: String,
    pub fills: u64,
    pub equity_points: u64,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub artifacts: BacktestArtifacts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl DepthBacktestOutcome {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DepthBacktestOutcome JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DepthBacktestOutcome 序列化失败: {error}"),
            )
        })
    }
}
