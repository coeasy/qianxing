//! Versioned two-leg Bar backtest contract. Input streams may have different timestamps; both legs execute through one shared account buying-power pool.

use super::{is_identity_token, BuiltinStrategySpec};
use crate::error::{AppError, AppErrorCategory};
use qx_core::{DecimalRounding, ExactDecimal, FundingRateEvent, TradingInstrumentSpec};
use serde::{Deserialize, Serialize};

pub const MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;
pub const MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiLegFillModel {
    #[default]
    NextBarOpen,
    BestPrice,
    OneTickSlippage,
}

/// Dataset identity and source files for a two-leg run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegDatasetInput {
    pub run_id: String,
    pub primary_bars_path: String,
    pub reference_bars_path: String,
}

/// Input BarFrames must be valid and use distinct instruments. Their timestamps may be unaligned;
/// callbacks are ordered by timestamp, declared leg priority, then canonical instrument id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiLegBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub primary_bars_path: String,
    pub reference_bars_path: String,
    pub settlement_currency: String,
    pub primary_initial_cash_raw: i128,
    pub reference_initial_cash_raw: i128,
    /// Version 2 scale-aware leg principals. The raw fields are toward-zero 1e-9 projections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_initial_cash_exact: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_initial_cash_exact: Option<ExactDecimal>,
    pub seed: u64,
    pub output_dir: String,
    pub strategy: BuiltinStrategySpec,
    #[serde(default)]
    pub primary_market_spec: Option<TradingInstrumentSpec>,
    #[serde(default)]
    pub reference_market_spec: Option<TradingInstrumentSpec>,
    #[serde(default = "default_maker_fee_bps")]
    pub maker_fee_bps: i64,
    #[serde(default = "default_taker_fee_bps")]
    pub taker_fee_bps: i64,
    #[serde(default)]
    pub primary_maker_fee_bps: Option<i64>,
    #[serde(default)]
    pub primary_taker_fee_bps: Option<i64>,
    #[serde(default)]
    pub reference_maker_fee_bps: Option<i64>,
    #[serde(default)]
    pub reference_taker_fee_bps: Option<i64>,
    #[serde(default)]
    /// Signed derivative funding rate in bps per default UTC 8-hour settlement.
    /// Settlements affect the shared Rust Ledger and replay before callbacks at that time.
    pub funding_bps: i64,
    /// Exchange-timestamped historical rates; event cadence optionally bounds mark staleness.
    /// A market cannot mix this series with nonzero `funding_bps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub funding_events: Vec<FundingRateEvent>,
    #[serde(default)]
    pub fill_model: MultiLegFillModel,
    /// Shared owner for both legs. Omitted derives one stable account identity from `run_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Optional DatasetManifestV2 inputs; both manifests must be supplied together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_dataset_manifest_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_dataset_manifest_path: Option<String>,
}

fn default_maker_fee_bps() -> i64 {
    qx_core::fee::DEFAULT_MAKER_BP
}

fn default_taker_fee_bps() -> i64 {
    qx_core::fee::DEFAULT_TAKER_BP
}

impl MultiLegBacktestSpec {
    pub fn new(
        dataset: MultiLegDatasetInput,
        settlement_currency: impl Into<String>,
        primary_initial_cash_raw: i128,
        reference_initial_cash_raw: i128,
        seed: u64,
        output_dir: impl Into<String>,
        strategy: BuiltinStrategySpec,
    ) -> Self {
        Self {
            schema_version: MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: dataset.run_id,
            primary_bars_path: dataset.primary_bars_path,
            reference_bars_path: dataset.reference_bars_path,
            settlement_currency: settlement_currency.into(),
            primary_initial_cash_raw,
            reference_initial_cash_raw,
            primary_initial_cash_exact: None,
            reference_initial_cash_exact: None,
            seed,
            output_dir: output_dir.into(),
            strategy,
            primary_market_spec: None,
            reference_market_spec: None,
            maker_fee_bps: default_maker_fee_bps(),
            taker_fee_bps: default_taker_fee_bps(),
            primary_maker_fee_bps: None,
            primary_taker_fee_bps: None,
            reference_maker_fee_bps: None,
            reference_taker_fee_bps: None,
            funding_bps: 0,
            funding_events: Vec::new(),
            fill_model: MultiLegFillModel::default(),
            account_id: None,
            primary_dataset_manifest_path: None,
            reference_dataset_manifest_path: None,
        }
    }

    /// Select the exact v2 principal contract while retaining raw compatibility projections.
    pub fn with_initial_cash_exact(
        mut self,
        primary: ExactDecimal,
        reference: ExactDecimal,
    ) -> Result<Self, AppError> {
        let primary_raw = exact_cash_raw_projection(primary)
            .ok_or_else(|| invalid("主腿精确初始资金无法投影到 1e-9 兼容范围"))?;
        let reference_raw = exact_cash_raw_projection(reference)
            .ok_or_else(|| invalid("参考腿精确初始资金无法投影到 1e-9 兼容范围"))?;
        self.schema_version = MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION;
        self.primary_initial_cash_raw = primary_raw;
        self.reference_initial_cash_raw = reference_raw;
        self.primary_initial_cash_exact = Some(primary);
        self.reference_initial_cash_exact = Some(reference);
        self.validate()?;
        Ok(self)
    }

    /// Return the authoritative per-leg principal values, preserving the v1 raw contract.
    pub fn initial_cash_exact_values(&self) -> Result<(ExactDecimal, ExactDecimal), AppError> {
        match (
            self.primary_initial_cash_exact,
            self.reference_initial_cash_exact,
        ) {
            (Some(primary), Some(reference)) => Ok((primary, reference)),
            (None, None) => Ok((
                ExactDecimal::from_parts(self.primary_initial_cash_raw, 9)
                    .map_err(|_| invalid("主腿初始资金溢出"))?,
                ExactDecimal::from_parts(self.reference_initial_cash_raw, 9)
                    .map_err(|_| invalid("参考腿初始资金溢出"))?,
            )),
            _ => Err(invalid("多腿精确初始资金必须为两腿成对提供")),
        }
    }

    /// Return the authoritative combined principal and its raw compatibility projection.
    pub fn combined_initial_cash(&self) -> Result<(ExactDecimal, i128), AppError> {
        let (primary, reference) = self.initial_cash_exact_values()?;
        let total = primary
            .checked_add(reference)
            .ok_or_else(|| invalid("多腿精确初始资金合计溢出"))?;
        let raw = exact_cash_raw_projection(total)
            .ok_or_else(|| invalid("多腿精确初始资金合计无法投影到 1e-9 兼容范围"))?;
        Ok((total, raw))
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if !matches!(
            self.schema_version,
            MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION | MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION
        ) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "MultiLegBacktestSpec schema_version={} 本版本只认 {} 或 {}",
                    self.schema_version,
                    MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION,
                    MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION
                ),
            ));
        }
        if (self.schema_version == MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION
            && (self.primary_initial_cash_exact.is_some()
                || self.reference_initial_cash_exact.is_some()))
            || (self.schema_version == MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION
                && (self.primary_initial_cash_exact.is_none()
                    || self.reference_initial_cash_exact.is_none()))
        {
            return Err(invalid(
                "MultiLegBacktestSpec v1 只接受两腿 initial_cash_raw；v2 必须成对提供 initial_cash_exact",
            ));
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "MultiLegBacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if self
            .account_id
            .as_deref()
            .is_some_and(|account_id| !is_identity_token(account_id))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "MultiLegBacktestSpec.account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        for (field, value) in [
            ("primary_bars_path", &self.primary_bars_path),
            ("reference_bars_path", &self.reference_bars_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("MultiLegBacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if self.primary_bars_path == self.reference_bars_path {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿 BarFrame 必须来自两份独立输入",
            ));
        }
        if self.primary_dataset_manifest_path.is_some()
            != self.reference_dataset_manifest_path.is_some()
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿回测必须同时提供 primary/reference 数据质量清单，或都不提供",
            ));
        }
        for (field, path) in [
            (
                "primary_dataset_manifest_path",
                &self.primary_dataset_manifest_path,
            ),
            (
                "reference_dataset_manifest_path",
                &self.reference_dataset_manifest_path,
            ),
        ] {
            if path.as_deref().is_some_and(|value| value.trim().is_empty()) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("MultiLegBacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if let (Some(primary), Some(reference)) = (
            self.primary_initial_cash_exact,
            self.reference_initial_cash_exact,
        ) {
            if !primary.is_positive()
                || !reference.is_positive()
                || exact_cash_raw_projection(primary) != Some(self.primary_initial_cash_raw)
                || exact_cash_raw_projection(reference) != Some(self.reference_initial_cash_raw)
            {
                return Err(invalid(
                    "多腿 exact 初始本金必须为正，且 raw 字段必须等于各腿向零投影值",
                ));
            }
            let combined = primary
                .checked_add(reference)
                .ok_or_else(|| invalid("多腿精确初始资金合计溢出"))?;
            exact_cash_raw_projection(combined)
                .ok_or_else(|| invalid("多腿精确初始资金合计无法投影到 1e-9 兼容范围"))?;
        } else {
            if self.primary_initial_cash_raw <= 0 || self.reference_initial_cash_raw <= 0 {
                return Err(invalid("多腿两侧 initial_cash_raw 都必须为正数"));
            }
            self.primary_initial_cash_raw
                .checked_add(self.reference_initial_cash_raw)
                .ok_or_else(|| invalid("多腿初始现金合计溢出 i128"))?;
        }
        let fee_rates = [
            self.maker_fee_bps,
            self.taker_fee_bps,
            self.primary_maker_fee_bps.unwrap_or(self.maker_fee_bps),
            self.primary_taker_fee_bps.unwrap_or(self.taker_fee_bps),
            self.reference_maker_fee_bps.unwrap_or(self.maker_fee_bps),
            self.reference_taker_fee_bps.unwrap_or(self.taker_fee_bps),
        ];
        if fee_rates.iter().any(|rate| !(0..=10_000).contains(rate)) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿共享费率与腿级 maker/taker 费率必须在 0..=10000",
            ));
        }
        if !(-10_000..=10_000).contains(&self.funding_bps) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿 funding_bps 必须在 -10000..=10000",
            ));
        }
        if self.funding_events.len() > 100_000 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿 funding_events 数量不得超过 100000",
            ));
        }
        let mut funding_event_ids = std::collections::BTreeSet::new();
        for event in &self.funding_events {
            if !(-10_000..=10_000).contains(&event.rate_bps)
                || event.settlement_ts == 0
                || !event.has_valid_funding_interval()
                || !funding_event_ids.insert((event.instrument.clone(), event.settlement_ts))
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "多腿 funding_events 费率、结算周期或唯一性无效",
                ));
            }
        }
        if self.strategy.target_available_margin_bps.is_some()
            || self.strategy.target_volatility_bps.is_some()
            || self.strategy.target_kelly_fraction_bps.is_some()
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿策略须为每条腿分别提供可用资金与波动率快照，暂不支持共享账户 sizing",
            ));
        }
        if self.strategy.target_notional_raw.is_some() {
            let (Some(primary), Some(reference)) = (
                self.primary_market_spec.as_ref(),
                self.reference_market_spec.as_ref(),
            ) else {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "多腿固定名义金额 sizing 必须配置两腿永续/交割 market spec",
                ));
            };
            if !matches!(
                primary.product,
                qx_core::TradingProduct::Perpetual | qx_core::TradingProduct::Future
            ) || !matches!(
                reference.product,
                qx_core::TradingProduct::Perpetual | qx_core::TradingProduct::Future
            ) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "多腿固定名义金额 sizing 当前仅支持永续/交割合约；现货空腿缺少借币/库存模型",
                ));
            }
            if primary.settlement_currency != self.settlement_currency
                || reference.settlement_currency != self.settlement_currency
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "多腿固定名义金额 sizing 要求两腿结算币种都等于 settlement_currency",
                ));
            }
            if primary.quote_currency != reference.quote_currency {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "多腿固定名义金额 sizing 要求两腿报价币种一致",
                ));
            }
            for market in [primary, reference] {
                let expected_settlement = if market.inverse {
                    &market.base_currency
                } else {
                    &market.quote_currency
                };
                if &market.settlement_currency != expected_settlement {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "多腿固定名义金额 sizing 要求线性合约以报价币结算、反向合约以基础币结算",
                    ));
                }
            }
        }
        for (label, spec) in [
            ("primary_market_spec", &self.primary_market_spec),
            ("reference_market_spec", &self.reference_market_spec),
        ] {
            if let Some(spec) = spec {
                spec.validate().map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("MultiLegBacktestSpec.{label} 非法: {error:?}"),
                    )
                })?;
            }
        }
        if !self.strategy.kind.needs_reference_leg() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "多腿回测只接受需要 reference leg 的内置套利策略",
            ));
        }
        self.strategy.validate()
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiLegBacktestSpec JSON 无效: {error}"),
            )
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiLegBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

fn exact_cash_raw_projection(amount: ExactDecimal) -> Option<i128> {
    amount
        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
        .and_then(|value| value.to_raw(9).ok())
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message.into())
}

/// Stable outcome shared by Rust, Python, and HTTP callers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegBacktestOutcome {
    pub run_id: String,
    pub primary_instrument: String,
    pub reference_instrument: String,
    pub primary_result_hash: String,
    pub reference_result_hash: String,
    pub combined_return_bps: i32,
    pub primary_fills: u64,
    pub reference_fills: u64,
    pub attribution_artifact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl MultiLegBacktestOutcome {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("MultiLegBacktestOutcome 序列化失败: {error}"),
            )
        })
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("MultiLegBacktestOutcome JSON 无效: {error}"),
            )
        })
    }
}
