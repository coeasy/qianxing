//! 应用层稳定输入/输出类型（T2-1）。
//!
//! 这里的类型是**应用层的公共面**：CLI、Python SDK、HTTP API 三个入口都只经它们
//! 进出用例，所以它们必须比任何单个入口活得久。三条纪律：
//!
//! 1. **带版本**：每个 spec 有 `schema_version`，不匹配就 [`AppErrorCategory::InvalidInput`]，
//!    不做"尽力而为"的字段猜测。
//! 2. **严格**：`deny_unknown_fields`。多一个键就是拼错了字段名，静默忽略等于让
//!    用户以为设上了。
//! 3. **定点原值显式**：金额/数量走 `*_raw`（i128 定点），与仓内 wire 风格一致；
//!    应用层不接受浮点，也不替调用方做单位换算。
//!
//! 还有第 4 条只对身份字段成立：`run_id`/`dataset_id` 必须落在 `[A-Za-z0-9._-]` 里。
//! 它们会变成产物**文件名**的一部分，也会出现在三个入口的 URL/命令行里；放行 `/`、`\`
//! 或 `..` 等于让调用方决定往哪写文件。这条限制写在 `validate()` 里而不是"写文件时
//! 顺手 sanitize"：sanitize 会把两个不同的 run_id 静默映射到同一个文件，
//! 而**拒绝**会让调用方当场知道自己的身份串不合法。

use crate::error::{AppError, AppErrorCategory};
use qx_core::{MakerTakerFeeSchedule, TradingInstrumentSpec};
use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};

/// `DatasetSpec` 的 schema 版本。改形状必须抬它，否则旧调用方会静默按新语义读。
pub const DATASET_SPEC_SCHEMA_VERSION: u32 = 1;

/// `BacktestSpec` 的 schema 版本。
pub const BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

/// Bar 回测对样本量的下限：少于这个数，"指标"不是指标而是噪声。
/// 它是一道**档位闸**而不是礼貌检查：样本不足时正确行为是
/// [`AppErrorCategory::FidelityInsufficient`]，不是给一个看起来像结论的数。
pub const MIN_BACKTEST_BARS: usize = 2;

mod validation;
use validation::is_identity_token;

/// Fixed quote-notional sizing currently assumes one linear base unit per quantity unit.
/// Derivative contracts need product-specific notional math and must not fall through to it.
pub(super) fn supports_fixed_notional_sizing(spec: &qx_core::TradingInstrumentSpec) -> bool {
    spec.product == qx_core::TradingProduct::Spot
        && spec.contract_size == qx_core::SCALE
        && !spec.inverse
}

/// `ValidateDataset` 的输入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetSpec {
    pub schema_version: u32,
    /// 数据集身份。会随裁决一起回来，供调用方对齐注册表。
    pub dataset_id: String,
    /// BarFrame JSON 的路径。相对路径按进程当前目录解析（与仓内其余入口同口径）。
    pub bars_path: String,
}

impl DatasetSpec {
    /// 构造一份当前版本的 spec。
    pub fn new(dataset_id: impl Into<String>, bars_path: impl Into<String>) -> Self {
        Self {
            schema_version: DATASET_SPEC_SCHEMA_VERSION,
            dataset_id: dataset_id.into(),
            bars_path: bars_path.into(),
        }
    }

    /// 形状自检。**只查"这份 spec 自己站不站得住"**，不碰磁盘。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DATASET_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DatasetSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DATASET_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.dataset_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DatasetSpec.dataset_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符，实际 {:?}",
                    self.dataset_id
                ),
            ));
        }
        if self.bars_path.trim().is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DatasetSpec.bars_path 不能为空",
            ));
        }
        Ok(())
    }

    /// 从 JSON 读入（严格：多余键即拒）。
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DatasetSpec JSON 无效: {error}"),
            )
        })
    }
}

/// 内置策略的声明（应用层面）。Bar 与 Tick/OrderBook 入口共用同一套策略旋钮；
/// 深度入口把每个可见盘口快照折成一个价格观测。费用/撮合/保证金口径由回测 spec 决定。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinStrategySpec {
    pub kind: BuiltinStrategyKind,
    pub strategy_id: String,
    /// 每笔下单数量（定点原值）。
    pub quantity_raw: i128,
    /// 可选固定名义金额 sizing（结算币/报价币 raw 单位）；为 None 时使用 quantity_raw。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_notional_raw: Option<i128>,
    /// 可选的当前可用资金比例（基点）；内核按决策时可见的 available margin 计算数量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_available_margin_bps: Option<u32>,
    /// Optional ATR-normalized risk budget (basis points of current available margin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_volatility_bps: Option<u32>,
    /// Fractional Kelly multiplier in basis points, applied to the prior-return estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kelly_fraction_bps: Option<u32>,
    /// Prior close observations for Kelly sizing (30..=1000); each Tick/OrderBook snapshot adds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kelly_lookback_bars: Option<usize>,
    pub fast_window: usize,
    pub slow_window: usize,
    /// Indicator period used by RSI/channel/volatility strategies.
    #[serde(default = "default_strategy_period")]
    pub period: usize,
    /// Signal threshold in basis points, where used by the selected strategy.
    #[serde(default = "default_strategy_threshold_bps")]
    pub threshold_bps: i128,
}

fn default_strategy_period() -> usize {
    14
}

fn default_strategy_threshold_bps() -> i128 {
    100
}

impl BuiltinStrategySpec {
    pub fn new(kind: BuiltinStrategyKind, strategy_id: impl Into<String>) -> Self {
        Self {
            kind,
            strategy_id: strategy_id.into(),
            quantity_raw: qx_core::SCALE,
            target_notional_raw: None,
            target_available_margin_bps: None,
            target_volatility_bps: None,
            target_kelly_fraction_bps: None,
            kelly_lookback_bars: None,
            fast_window: 5,
            slow_window: 20,
            period: default_strategy_period(),
            threshold_bps: default_strategy_threshold_bps(),
        }
    }

    /// 形状自检与领域策略旋钮清单保持一致；不生效的旋钮不阻断本次运行。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.strategy_id.trim().is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.strategy_id 不能为空",
            ));
        }
        if self.quantity_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.quantity_raw 必须为正",
            ));
        }
        if self.target_notional_raw.is_some_and(|value| value <= 0) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.target_notional_raw 必须为正",
            ));
        }
        if self
            .target_available_margin_bps
            .is_some_and(|bps| bps == 0 || bps > 10_000)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.target_available_margin_bps 必须在 1..=10000",
            ));
        }
        if self
            .target_volatility_bps
            .is_some_and(|bps| bps == 0 || bps > 10_000)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.target_volatility_bps 必须在 1..=10000",
            ));
        }
        if self
            .target_kelly_fraction_bps
            .is_some_and(|bps| bps == 0 || bps > 10_000)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.target_kelly_fraction_bps 必须在 1..=10000",
            ));
        }
        match (self.target_kelly_fraction_bps, self.kelly_lookback_bars) {
            (Some(_), Some(lookback)) if (30..=1_000).contains(&lookback) => {}
            (Some(_), None) => {}
            (Some(_), Some(_)) => {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Kelly sizing 必须配置 30..=1000 根 kelly_lookback_bars",
                ))
            }
            (None, Some(_)) => {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "kelly_lookback_bars 只能与 Kelly sizing 一起配置",
                ))
            }
            (None, None) => {}
        }
        if [
            self.target_notional_raw.is_some(),
            self.target_available_margin_bps.is_some(),
            self.target_volatility_bps.is_some(),
            self.target_kelly_fraction_bps.is_some(),
        ]
        .into_iter()
        .filter(|enabled| *enabled)
        .count()
            > 1
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "固定名义金额、可用资金比例、波动率预算与 Kelly sizing 互斥",
            ));
        }
        use qx_strategy::builtin_signal::BuiltinSignalKnob::{
            FastWindow, Period, SlowWindow, ThresholdBps,
        };
        let uses = |knob| self.kind.uses_signal_knob(knob);
        if (uses(FastWindow) || uses(SlowWindow))
            && (self.fast_window == 0
                || self.slow_window == 0
                || self.fast_window >= self.slow_window)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec 快慢窗口必须为正且快线短于慢线",
            ));
        }
        if uses(Period) && self.period < 2 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.period 至少为 2",
            ));
        }
        if uses(ThresholdBps) && self.threshold_bps < 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.threshold_bps 不得为负",
            ));
        }
        Ok(())
    }
}

/// `RunBacktest` 的输入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestSpec {
    pub schema_version: u32,
    /// 运行身份。同时是**correlation id**：报错、产物、日志三处用它对齐。
    pub run_id: String,
    pub instrument: String,
    pub bars_path: String,
    /// 结算币种。它决定现金腿落在哪本账簿，所以必填——不给默认值。
    pub settlement_currency: String,
    /// 本金（定点原值）。V11 Q72 之后这是必答题：藏在默认值里会让整条收益率
    /// 压在一个调用方毫不知情的常数上。
    pub initial_cash_raw: i128,
    pub seed: u64,
    /// 产物目录。四份产物（`*.run.json` / `*.summary.json` / `*.equity.csv` / `*.fills.csv`）
    /// 都落在它下面。
    pub output_dir: String,
    /// Ledger/account ownership for this run. Omitted legacy specs use `app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Frozen venue market contract used by the shared Bar matching, risk, margin and ledger path.
    /// Omitted legacy specs keep their historical compatibility semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_spec: Option<TradingInstrumentSpec>,
    /// Frozen maker/taker schedule used by the shared Rust matching and accounting path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_schedule: Option<MakerTakerFeeSchedule>,
    pub strategy: BuiltinStrategySpec,
    /// Optional deterministic same-account strategy basket. Omitted specs keep
    /// their historical single-strategy behavior and JSON shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<PortfolioBacktestConfig>,
    /// Optional position-level protective exits, evaluated on completed bars
    /// and routed through the same OMS/risk/matching path as strategy orders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protective_exit: Option<ProtectiveExitSpec>,
}
impl BacktestSpec {
    /// 构造一份当前版本的 spec（其余字段按调用方给的填）。
    #[allow(clippy::too_many_arguments)] // 与产物身份逐格对应，合并成参数包会让必答项变成可选。
    pub fn new(
        run_id: impl Into<String>,
        instrument: impl Into<String>,
        bars_path: impl Into<String>,
        settlement_currency: impl Into<String>,
        initial_cash_raw: i128,
        seed: u64,
        output_dir: impl Into<String>,
        strategy: BuiltinStrategySpec,
    ) -> Self {
        Self {
            schema_version: BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: run_id.into(),
            instrument: instrument.into(),
            bars_path: bars_path.into(),
            settlement_currency: settlement_currency.into(),
            initial_cash_raw,
            seed,
            output_dir: output_dir.into(),
            account_id: None,
            market_spec: None,
            fee_schedule: None,
            strategy,
            portfolio: None,
            protective_exit: None,
        }
    }
    /// 形状自检。**只查"这份 spec 自己站不站得住"**，不碰磁盘。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "BacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        for (field, value) in [
            ("instrument", &self.instrument),
            ("bars_path", &self.bars_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("BacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "BacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符（它会变成产物文件名），实际 {:?}",
                    self.run_id
                ),
            ));
        }
        if self
            .account_id
            .as_deref()
            .is_some_and(|account_id| !is_identity_token(account_id))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if qx_core::InstrumentId::parse(&self.instrument).is_none() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec.instrument 非法: {}", self.instrument),
            ));
        }
        if self.initial_cash_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.initial_cash_raw 必须为正",
            ));
        }
        if let Some(market) = &self.market_spec {
            market
                .validate()
                .map_err(|error| AppError::from_qx_error(&error))?;
            if market.product == qx_core::TradingProduct::Option {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Bar 回测暂不接收期权规格：行权价、到期日与认购/认沽身份尚未建模",
                ));
            }
            if qx_core::InstrumentId::parse(&self.instrument).as_ref() != Some(&market.instrument) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "BacktestSpec.market_spec.instrument 必须与 BacktestSpec.instrument 一致",
                ));
            }
            if market.settlement_currency != self.settlement_currency {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "BacktestSpec.market_spec.settlement_currency 必须与 settlement_currency 一致",
                ));
            }
            if self.fee_schedule.is_none() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "指定 market_spec 时必须显式提供 fee_schedule，禁止静默套用默认费率",
                ));
            }
        }
        if let Some(fees) = self.fee_schedule {
            fees.validate()
                .map_err(|error| AppError::from_qx_error(&error))?;
        }
        if let Some(policy) = self.protective_exit {
            policy.validate()?;
        }
        self.strategy.validate()?;
        if let Some(portfolio) = &self.portfolio {
            if self.protective_exit.is_some() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "策略组合暂不支持账户净持仓级 protective_exit，避免退出成交无法归属到策略持仓",
                ));
            }
            portfolio.validate(&self.strategy, self.initial_cash_raw)?;
        }
        Ok(())
    }

    /// 从 JSON 读入（严格：多余键即拒）。
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec JSON 无效: {error}"),
            )
        })
    }

    /// 稳定 JSON 形态（规范化输出，供三个入口逐字节比对）。
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("BacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

mod depth;
pub use depth::{DepthBacktestOutcome, DepthBacktestSpec, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION};
mod multi_leg;
pub use multi_leg::{
    MultiLegBacktestOutcome, MultiLegBacktestSpec, MultiLegFillModel,
    MULTI_LEG_BACKTEST_SPEC_SCHEMA_VERSION,
};
mod multi_account;
pub use multi_account::*;
pub mod multi_market;
mod multi_market_depth;
pub use multi_market_depth::*;
mod results;
pub use results::{BacktestArtifacts, BacktestOutcome, DatasetVerdict, VerificationResult};
mod risk_parity;
pub use risk_parity::{
    RiskParityAssetSpec, RiskParityResult, RiskParitySpec, RISK_PARITY_RESULT_SCHEMA_VERSION,
    RISK_PARITY_SPEC_SCHEMA_VERSION,
};
mod algo_execution;
pub use algo_execution::{
    AlgoExecutionMethod, AlgoExecutionPlan, AlgoExecutionSpec, ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
};
mod protective_exit;
pub use protective_exit::ProtectiveExitSpec;
mod portfolio;
pub use portfolio::{PortfolioBacktestConfig, PortfolioStrategySpec};
mod capital_allocation;
pub use capital_allocation::{
    CapitalAllocationAccount, CapitalAllocationRequest, CapitalAllocationResult,
    CapitalAllocationSpec, CAPITAL_ALLOCATION_SPEC_SCHEMA_VERSION,
};
