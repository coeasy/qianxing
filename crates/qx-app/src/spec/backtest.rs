use super::{
    AppError, AppErrorCategory, BacktestMarginConfig, BuiltinStrategySpec, PortfolioBacktestConfig,
    ProtectiveExitSpec, BACKTEST_SPEC_SCHEMA_VERSION,
};
use qx_core::{MakerTakerFeeSchedule, TradingInstrumentSpec};
use qx_xingban::{AshareRuleConfig, ExecutionCostRules};
use serde::{Deserialize, Serialize};
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
    /// 产物目录。四份核心产物（`*.run.json` / `*.summary.json` / `*.equity.csv` / `*.fills.csv`）
    /// 都落在它下面。
    pub output_dir: String,
    /// Ledger/account ownership for this run. Omitted legacy specs use `app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Frozen venue market contract used by the shared Bar matching, risk, margin and ledger path.
    /// Omitted legacy specs keep their historical compatibility semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_spec: Option<TradingInstrumentSpec>,
    /// Explicit margin semantics supplied by a venue snapshot; omitted specs keep app defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_model: Option<BacktestMarginConfig>,
    /// Frozen maker/taker schedule used by the shared Rust matching and accounting path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_schedule: Option<MakerTakerFeeSchedule>,
    /// Validated execution cost rules, including latency. Omitted legacy specs keep zero latency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_cost_rules: Option<ExecutionCostRules>,
    /// Optional preloaded A-share rule snapshot; paths and external data loading stay at the facade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ashare_rules: Option<AshareRuleConfig>,
    /// Optional OHLCV-compatible fill model; unsupported book-only models are not representable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_model: Option<BacktestFillModel>,
    /// Maximum cumulative market-volume participation for all fills on each Bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bar_volume_participation_bps: Option<u16>,
    /// Optional static order risk rules; product-derived constraints remain implicit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_rules: Option<BacktestRiskRules>,
    /// Optional short-selling permission override from a runtime product/margin policy.
    /// Omitted specs derive it from `market_spec`; missing specs retain conservative spot behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_short: Option<bool>,
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
    pub fn new(
        dataset: BacktestDatasetInput,
        settlement_currency: impl Into<String>,
        initial_cash_raw: i128,
        seed: u64,
        output_dir: impl Into<String>,
        strategy: BuiltinStrategySpec,
    ) -> Self {
        Self {
            schema_version: BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: dataset.run_id,
            instrument: dataset.instrument,
            bars_path: dataset.bars_path,
            settlement_currency: settlement_currency.into(),
            initial_cash_raw,
            seed,
            output_dir: output_dir.into(),
            account_id: None,
            market_spec: None,
            margin_model: None,
            fee_schedule: None,
            execution_cost_rules: None,
            ashare_rules: None,
            fill_model: None,
            max_bar_volume_participation_bps: None,
            risk_rules: None,
            allow_short: None,
            strategy,
            portfolio: None,
            protective_exit: None,
        }
    }
    /// 形状自检。**只查"这份 spec 自己站不站得住"**，不碰磁盘。
    pub fn validate(&self) -> Result<(), AppError> {
        super::backtest_validation::validate(self)
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

/// Declarative static risk rules for a shared Bar backtest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestRiskRules {
    #[serde(default = "default_backtest_risk_version")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_qty_raw: Option<i128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_notional_raw: Option<i128>,
    #[serde(default)]
    pub no_short: bool,
}

fn default_backtest_risk_version() -> String {
    "runtime-risk-v1".into()
}

/// Fill models whose assumptions can be expressed with OHLCV Bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacktestFillModel {
    NextBarOpen,
    BestPrice,
    OneTickSlippage,
}

/// Identity and input paths for one Bar backtest. Kept together because each field names the same
/// dataset/run boundary and must travel consistently into manifests and replay verification.
pub struct BacktestDatasetInput {
    pub run_id: String,
    pub instrument: String,
    pub bars_path: String,
}
