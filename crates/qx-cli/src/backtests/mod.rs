//! 回测编排：策略回测、深度档回测、多标的多腿回测与产物落盘。
//!
//! 撮合内核来自 qx-xingban，本模块负责输入解析、风控门同源注入与运行清单/产物持久化。

use super::*;

/// Bar 回测内核装配：`backtest builtin`、`backtest multi-builtin` 的腿级回测与
/// `strategy backtest` 共用同一份乘数、成交、延迟与数据档位口径；
/// 各入口只覆盖真正会分叉的费用、保证金、风控门与随机种子。
pub(crate) struct BarBacktestAssembly {
    pub(crate) instrument: InstrumentId,
    pub(crate) instrument_spec: Option<TradingInstrumentSpec>,
    pub(crate) account_id: String,
    pub(crate) currency: String,
    pub(crate) initial_cash: Money,
    pub(crate) margin: Box<dyn MarginRule>,
    pub(crate) fee: Box<dyn FeeModel>,
    pub(crate) risk: RiskGate,
    pub(crate) virtual_trading: VirtualTradingConfig,
    pub(crate) seed: u64,
}

impl BarBacktestAssembly {
    pub(crate) fn new(instrument: &InstrumentId, account_id: impl Into<String>, seed: u64) -> Self {
        Self {
            instrument: instrument.clone(),
            instrument_spec: None,
            account_id: account_id.into(),
            currency: "USDT".into(),
            initial_cash: Money::from_i64(100_000),
            margin: Box::new(NoMargin),
            fee: Box::new(MakerTakerFeeModel {
                maker_bp: 2,
                taker_bp: 5,
            }),
            risk: strategy_risk_gate(None, false),
            virtual_trading: VirtualTradingConfig::default(),
            seed,
        }
    }

    pub(crate) fn into_config(self) -> BacktestConfig {
        BacktestConfig {
            instrument: self.instrument,
            instrument_spec: self.instrument_spec,
            account_id: self.account_id,
            currency: self.currency,
            initial_cash: self.initial_cash,
            multiplier: 1,
            fill: Box::new(NextBarOpenFillModel),
            fee: self.fee,
            data_tier: DataTier::Bar,
            latency: Box::new(ZeroLatency),
            margin: self.margin,
            seed: self.seed,
            risk: self.risk,
            virtual_trading: self.virtual_trading,
        }
    }
}

/// market spec JSON → `(合约规格, 保证金规则)`；三条 Bar 回测链读取同一份口径。
pub(crate) fn market_spec_with_margin(
    instrument: &InstrumentId,
    spec_path: Option<&Path>,
    label: &str,
) -> Result<(Option<TradingInstrumentSpec>, Box<dyn MarginRule>), String> {
    let Some(spec_path) = spec_path else {
        return Ok((None, Box::new(NoMargin)));
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
    let margin = ccxt_margin_rule_from_market(&market);
    Ok((Some(ccxt_market_to_spec(instrument, &market)?), margin))
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
mod artifacts;
pub(crate) use artifacts::*;

mod depth;
pub(crate) use depth::*;

mod fast_backtest;
pub(crate) use fast_backtest::*;

mod kernels;
pub(crate) use kernels::*;

mod multi_builtin;
pub(crate) use multi_builtin::*;

mod risk_binding;
pub(crate) use risk_binding::*;

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
