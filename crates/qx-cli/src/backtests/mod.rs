//! 回测编排：策略回测、深度档回测、多标的多腿回测与产物落盘。
//!
//! 撮合内核来自 qx-xingban，本模块负责输入解析、风控门同源注入与运行清单/产物持久化。

use super::*;

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
    ) -> Self {
        Self {
            instrument: instrument.clone(),
            instrument_spec: None,
            account_id: account_id.into(),
            initial_cash,
            margin: Box::new(NoMargin),
            fill: fill.fill,
            fee: costs.fee_model(),
            latency: costs.latency_model(),
            risk: strategy_risk_gate(None, false),
            virtual_trading: VirtualTradingConfig::default(),
            seed,
        }
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

/// market spec JSON → `MarketSpecLoad`；三条 Bar 回测链读取同一份口径。
///
/// 结构体本身住在 `market_spec.rs`（它同时是保证金规则的形状来源），这里只负责"读文件 +
/// 报错带上是哪条链"，好让 mod.rs 的顶层条目不再长一份。
pub(crate) fn market_spec_with_margin(
    instrument: &InstrumentId,
    spec_path: Option<&Path>,
    label: &str,
) -> Result<MarketSpecLoad, String> {
    let Some(spec_path) = spec_path else {
        return Ok(MarketSpecLoad {
            spec: None,
            margin: Box::new(NoMargin),
            source: DEFAULT_INSTRUMENT_SPEC_VERSION,
        });
    };
    let payload = std::fs::read_to_string(spec_path).map_err(|error| {
        format!(
            "读取{label} market spec 失败 {}: {error}",
            spec_path.display()
        )
    })?;
    let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "{label} market spec JSON 无效 {}: {error}",
            spec_path.display()
        )
    })?;
    let source = market_spec_source_label(&market);
    let margin = ccxt_margin_rule_from_market(&market);
    let spec = market_spec_from_value(instrument, &market)
        .map_err(|error| format!("{label} market spec {}: {error}", spec_path.display()))?;
    Ok(MarketSpecLoad {
        spec: Some(spec),
        margin,
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

mod ashare_binding;
pub(crate) use ashare_binding::*;

mod config_declarations;
pub(crate) use config_declarations::*;

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

mod risk_binding;
pub(crate) use risk_binding::*;

mod signal_binding;
pub(crate) use signal_binding::*;

mod single_strategy;
pub(crate) use single_strategy::*;

mod strategy_backtest;
pub(crate) use strategy_backtest::*;

pub(crate) struct ScheduledTargetStrategy {
    instrument: InstrumentId,
    targets: BTreeMap<u64, i128>,
    policy: Option<OrderPolicy>,
    account_id: String,
}

impl BarStrategy for ScheduledTargetStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if instrument != &self.instrument {
            return None;
        }
        let visible_ts = history.last()?.ts;
        let target = self
            .targets
            .range(..=visible_ts)
            .next_back()
            .map(|(_, target)| *target)
            .unwrap_or(0);
        let delta = target.checked_sub(position)?;
        if delta == 0 {
            return None;
        }
        Some(Order {
            client_id: 0,
            instrument: self.instrument.clone(),
            side: if delta > 0 { Side::Buy } else { Side::Sell },
            qty: Quantity::from_raw(delta.checked_abs()?),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: self.account_id.clone(),
            trace: None,
            policy: self.policy,
        })
    }
}

pub(crate) fn read_bar_frame_for_multi_backtest(
    path: &Path,
    label: &str,
) -> Result<BarFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取{label} BarFrame 失败 {}: {error}", path.display()))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("{label} BarFrame 校验失败 {}: {error:?}", path.display()))
}
