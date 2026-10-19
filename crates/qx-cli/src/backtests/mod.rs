//! 回测编排：策略回测、深度档回测、多标的多腿回测与产物落盘。
//!
//! 撮合内核来自 qx-xingban，本模块负责输入解析、风控门同源注入与运行清单/产物持久化。

pub(crate) use super::{
    config_margin_mode, execution_cost_binding, execution_cost_binding_from_config,
    market_spec_from_value, multi_leg_integrity_summary, read_example_json, read_runtime_config,
    rejection_count, rejection_facts, rejection_facts_line, render_number, strategy_allows_short,
    strategy_risk_gate, verify_dataset_bundle_binding, verify_dataset_bundle_component_bindings,
    verify_strategy_artifact, BacktestConfig, BacktestEngine, Bar, BarFrame, BuiltinStrategy,
    BuiltinStrategyConfig, BuiltinStrategyKind, ContractBarStrategy, DataTier,
    ExecutionCostBinding, FeeModel, FillModel, InstrumentId, JsonDatasetRegistry, LatencyModel,
    MarginRule, Money, NativeBarStrategy, NativeStrategyContext, NoMargin, Quantity, RiskGate,
    RunManifest, RunManifestIdentity, RuntimeConfig, TradingInstrumentSpec, TradingProduct,
    VirtualTradingConfig, DEFAULT_SETTLEMENT_CURRENCY, MULTI_LEG_KINDS,
};
use std::path::Path;

pub(crate) struct MarketSpecLoad {
    pub(crate) spec: Option<TradingInstrumentSpec>,
    pub(crate) app_margin_model: Option<qx_app::BacktestMarginConfig>,
    pub(crate) source: &'static str,
}

/// Bar 回测内核装配：`backtest builtin`、`backtest multi-builtin` 的腿级回测与
/// `strategy backtest` 共用同一份乘数、成交、延迟与数据档位口径；
/// 各入口只覆盖真正会分叉的费用、撮合模型、保证金、风控门与随机种子。
pub(crate) struct BarBacktestAssembly {
    pub(crate) instrument: InstrumentId,
    pub(crate) instrument_spec: Option<TradingInstrumentSpec>,
    pub(crate) account_id: String,
    /// 只能由 [`Self::new`] 给出：本金一旦允许装配后再覆盖，"必答题"就退回到可以在
    /// 某条链上忘了答的形状（V11 Q72）。
    initial_cash: Money,
    pub(crate) margin: Box<dyn MarginRule>,
    pub(crate) fill: Box<dyn FillModel>,
    pub(crate) fee: Box<dyn FeeModel>,
    pub(crate) latency: Box<dyn LatencyModel>,
    pub(crate) risk: RiskGate,
    pub(crate) virtual_trading: VirtualTradingConfig,
    pub(crate) seed: u64,
}

/// 回测记账币种：market spec 声明了就用它，没声明才回落 USDT。币种决定现金腿落在
/// 哪本账簿，所以只能有一处判定——入口各抄一份兜底时，CNY 标的会在 USDT 账簿上撮合，
/// 产物里的成交却标着没人声明过的币种。
pub(crate) fn backtest_settlement_currency(spec: Option<&TradingInstrumentSpec>) -> String {
    spec.map(|spec| spec.settlement_currency.clone())
        .unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.into())
}

// 没人声明本金时，单腿回测按多少记账、这条必答题怎么答：见 `account_base.rs`（V11 Q72）。

impl BarBacktestAssembly {
    /// 费用与延迟成对来自同一份 [`ExecutionCostBinding`]（V11 Q0c）：分两个入口注入
    /// 就会退回到"两条链各自挑口径"，那是 Q0a/Q0c 要消灭的形状。
    ///
    /// 撮合模型同样是必答题：`fill` 由调用方从 [`bar_fill_model`] 取回来，装配处不给默认值，
    /// 于是新增一条 Bar 回测链时"忘了回答用哪个撮合模型"过不了编译（与 Q0a 的费用入参同法）。
    ///
    /// 本金自 V11 Q72 起也是必答题（`initial_cash`）：它以前藏在下面的默认值里，两条链
    /// 因此可以在使用者毫不知情的情况下，把整条收益率与全部风控判定压在一个 100,000 的常数上。
    pub(crate) fn new(
        instrument: &InstrumentId,
        account_id: impl Into<String>,
        initial_cash: Money,
        seed: u64,
        costs: &ExecutionCostBinding,
        fill: BarFillModelBinding,
    ) -> Result<Self, String> {
        Ok(Self {
            instrument: instrument.clone(),
            instrument_spec: None,
            account_id: account_id.into(),
            initial_cash,
            margin: Box::new(NoMargin),
            fill: fill.into_fill()?,
            fee: costs.fee_model(),
            latency: costs.latency_model(),
            risk: strategy_risk_gate(None, false),
            virtual_trading: VirtualTradingConfig::default(),
            seed,
        })
    }

    pub(crate) fn into_config(self) -> BacktestConfig {
        BacktestConfig {
            currency: backtest_settlement_currency(self.instrument_spec.as_ref()),
            instrument: self.instrument,
            instrument_spec: self.instrument_spec,
            account_id: self.account_id,
            initial_cash: self.initial_cash,
            multiplier: 1,
            fill: self.fill,
            fee: self.fee,
            data_tier: DataTier::Bar,
            latency: self.latency,
            margin: self.margin,
            seed: self.seed,
            risk: self.risk,
            virtual_trading: self.virtual_trading,
        }
    }
}

/// market spec JSON → `MarketSpecLoad`；CLI 只负责文件读取与错误路径，上层解析由 qx-app 完成。
pub(crate) fn market_spec_with_margin(
    instrument: &InstrumentId,
    spec_path: Option<&Path>,
    label: &str,
) -> Result<MarketSpecLoad, String> {
    let Some(spec_path) = spec_path else {
        return Ok(MarketSpecLoad {
            spec: None,
            app_margin_model: None,
            source: crate::DEFAULT_INSTRUMENT_SPEC_VERSION,
        });
    };
    let payload = read_example_json(spec_path, &format!("{label} market spec "))?;
    let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "{label} market spec JSON 无效 {}: {error}",
            spec_path.display()
        )
    })?;
    let source = qx_app::market_spec_source_label(&market);
    let app_margin_model = Some(qx_app::backtest_margin_model_from_market(&market));
    let spec = market_spec_from_value(instrument, &market)
        .map_err(|error| format!("{label} market spec {}: {error}", spec_path.display()))?;
    Ok(MarketSpecLoad {
        spec: Some(spec),
        app_margin_model,
        source,
    })
}

/// 内置策略在 Bar 内核上跑一遍的共用执行段：初始化、撮合与失败文案只保留一份。
pub(crate) fn run_builtin_strategy_on_bars(
    config: BacktestConfig,
    strategy_config: BuiltinStrategyConfig,
    context: NativeStrategyContext,
    bars: &[Bar],
) -> Result<qx_xingban::BacktestReport, String> {
    let mut strategy = NativeBarStrategy::new(BuiltinStrategy::new(strategy_config)?, context);
    strategy
        .initialize()
        .map_err(|error| format!("初始化内置策略失败: {error:?}"))?;
    BacktestEngine::new(config)
        .run(bars, &mut strategy)
        .map_err(|error| format!("内置策略回测失败: {error:?}"))
}

mod account_base;
pub(crate) use account_base::*;

mod artifacts;
pub(crate) use artifacts::*;

mod builtin_app;
pub(crate) use builtin_app::*;

mod ashare_binding;
pub(crate) use ashare_binding::*;
mod config_declarations;
pub(crate) use config_declarations::*;

mod ccxt_builtin;
pub(crate) use ccxt_builtin::*;

mod depth;
pub(crate) use depth::*;

mod fast_backtest;
pub(crate) use fast_backtest::*;

mod fill_model;
pub(crate) use fill_model::*;

mod kernels;
pub(crate) use kernels::*;

mod leg_funding;
pub(crate) use leg_funding::*;

mod multi_builtin;
pub(crate) use multi_builtin::*;

mod multi_composition_verify;
pub(crate) use multi_composition_verify::*;

mod multi_quality;
pub(crate) use multi_quality::*;

mod multi_artifacts;
pub(crate) use multi_artifacts::*;

mod risk_binding;
pub(crate) use risk_binding::*;

mod run_record;
mod summary_listing;
pub(crate) use run_record::*;
pub(crate) use summary_listing::*;

mod signal_binding;
pub(crate) use signal_binding::*;

mod single_strategy;
pub(crate) use single_strategy::*;

mod strategy_backtest;
pub(crate) use strategy_backtest::*;

pub(crate) fn read_bar_frame_for_multi_backtest(
    path: &Path,
    label: &str,
) -> Result<BarFrame, String> {
    let payload = read_example_json(path, &format!("{label} BarFrame "))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("{label} BarFrame 校验失败 {}: {error:?}", path.display()))
}
