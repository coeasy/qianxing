//! Application boundary for caller supplied single-instrument Bar adapters.

use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::cases::{backtest_risk_gate, bind_execution, bind_margin};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{
    BacktestDatasetInput, BacktestFillModel, BacktestMarginConfig, BacktestRiskRules, BacktestSpec,
    BuiltinStrategySpec, MIN_BACKTEST_BARS,
};
use qx_core::{InstrumentId, Money, ReplayVerifier};
use qx_guanxing::Bar;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, StrategyContext};
use qx_xingban::{
    AshareRuleConfig, BacktestConfig, BacktestEngine, BacktestReport, BarStrategy, DataTier,
    ExecutionCostRules, NativeBarStrategy, ProtectiveExitPolicy,
};
use std::collections::BTreeMap;

/// Replay-verified report plus audit metadata captured from the exact execution policy.
pub struct StrategyBacktestOutcome {
    pub report: BacktestReport,
    pub risk_rule_set_version: String,
}

/// Fully resolved runtime bindings required by the single-strategy Bar use case.
pub struct StrategyBacktestBindings {
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
    pub seed: u64,
    pub output_dir: String,
    pub account_id: String,
    pub market_spec: Option<qx_core::TradingInstrumentSpec>,
    pub margin_model: Option<BacktestMarginConfig>,
    pub execution_cost_rules: Option<ExecutionCostRules>,
    pub ashare_rules: Option<AshareRuleConfig>,
    pub risk_rules: Option<BacktestRiskRules>,
    pub allow_short: bool,
    pub fill_model: BacktestFillModel,
    pub max_bar_volume_participation_bps: Option<u16>,
}

/// Resolved strategy identity and account state used to build a validated native context.
pub struct StrategyContextBindings {
    pub strategy_id: String,
    pub strategy_version: String,
    pub account_id: String,
    pub venue_id: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub positions: BTreeMap<String, i128>,
    pub cash: BTreeMap<String, i128>,
    pub available_margin_raw: Option<i128>,
    pub risk_state: String,
}

/// Construct and validate the strategy context shared by native strategy hosts.
pub fn build_strategy_context(
    bindings: StrategyContextBindings,
) -> Result<StrategyContext, AppError> {
    let context = StrategyContext {
        strategy_id: bindings.strategy_id,
        strategy_version: bindings.strategy_version,
        account_id: bindings.account_id,
        venue_id: bindings.venue_id,
        data_fingerprint: bindings.data_fingerprint,
        as_of: bindings.as_of,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: bindings.positions,
        cash: bindings.cash,
        available_margin_raw: bindings.available_margin_raw,
        risk_state: bindings.risk_state,
    };
    context.validate().map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("策略上下文无效: {error}"),
        )
    })?;
    Ok(context)
}

/// Resolved account and data identity used to create a single-run native strategy context.
/// The facade remains responsible for resolving runtime configuration and verifying input data.
pub struct NativeStrategyBacktestContextBindings {
    pub account_id: String,
    pub venue_id: String,
    pub data_fingerprint: String,
    pub as_of: u64,
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
}

/// Assemble the stable application spec from facade-resolved runtime bindings.
pub fn assemble_strategy_backtest_spec(
    dataset: BacktestDatasetInput,
    strategy: BuiltinStrategySpec,
    bindings: StrategyBacktestBindings,
) -> BacktestSpec {
    let mut spec = BacktestSpec::new(
        dataset,
        bindings.settlement_currency,
        bindings.initial_cash_raw,
        bindings.seed,
        bindings.output_dir,
        strategy,
    );
    spec.account_id = Some(bindings.account_id);
    spec.market_spec = bindings.market_spec;
    spec.margin_model = bindings.margin_model;
    spec.execution_cost_rules = bindings.execution_cost_rules;
    spec.ashare_rules = bindings.ashare_rules;
    spec.risk_rules = bindings.risk_rules;
    spec.allow_short = Some(bindings.allow_short);
    spec.fill_model = Some(bindings.fill_model);
    spec.max_bar_volume_participation_bps = bindings.max_bar_volume_participation_bps;
    spec
}

/// Build and initialize a Rust built-in strategy behind the application boundary.
pub fn initialize_native_builtin_strategy(
    strategy_config: BuiltinStrategyConfig,
    strategy_context: StrategyContext,
) -> Result<impl BarStrategy, AppError> {
    let strategy = BuiltinStrategy::new(strategy_config).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("内置策略配置无效: {error:?}"),
        )
    })?;
    let mut native = NativeBarStrategy::new(strategy, strategy_context);
    native
        .initialize()
        .map_err(|error| AppError::from_qx_error(&error))?;
    Ok(native)
}

/// Build the standard flat-position context and initialize one native built-in strategy.
/// Keeping this projection here makes the CLI and other facades share the same initial account
/// semantics instead of independently inventing position, cash, margin, and risk-state fields.
pub fn initialize_native_builtin_strategy_for_backtest(
    strategy_config: BuiltinStrategyConfig,
    bindings: NativeStrategyBacktestContextBindings,
) -> Result<impl BarStrategy, AppError> {
    if bindings.initial_cash_raw < 0 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "单策略回测初始现金不能为负数",
        ));
    }
    let context = build_strategy_context(StrategyContextBindings {
        strategy_id: strategy_config.strategy_id.clone(),
        strategy_version: strategy_config.strategy_version.clone(),
        account_id: bindings.account_id,
        venue_id: bindings.venue_id,
        data_fingerprint: bindings.data_fingerprint,
        as_of: bindings.as_of,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(bindings.settlement_currency, bindings.initial_cash_raw)]),
        available_margin_raw: Some(bindings.initial_cash_raw),
        risk_state: "backtest-verified".into(),
    })?;
    initialize_native_builtin_strategy(strategy_config, context)
}

/// Run a runtime-configured native strategy as one application use case.
///
/// The facade still resolves files, account identity, and verified data provenance. The
/// application owns conversion to its stable strategy spec, creation of the flat backtest
/// context, strategy initialization, engine execution, and replay verification.
pub fn run_native_builtin_strategy_backtest(
    spec: &BacktestSpec,
    bars: &[Bar],
    strategy_config: BuiltinStrategyConfig,
    context_bindings: NativeStrategyBacktestContextBindings,
    context: &RunContext,
) -> Result<BacktestReport, AppError> {
    run_native_builtin_strategy_backtest_with_outcome(
        spec,
        bars,
        strategy_config,
        context_bindings,
        context,
    )
    .map(|outcome| outcome.report)
}

/// Execute a native strategy and include audit metadata from the actual risk gate.
pub fn run_native_builtin_strategy_backtest_with_outcome(
    spec: &BacktestSpec,
    bars: &[Bar],
    strategy_config: BuiltinStrategyConfig,
    context_bindings: NativeStrategyBacktestContextBindings,
    context: &RunContext,
) -> Result<StrategyBacktestOutcome, AppError> {
    let mut resolved_spec = spec.clone();
    resolved_spec.strategy = BuiltinStrategySpec::from_strategy_config(&strategy_config)?;
    let mut strategy =
        initialize_native_builtin_strategy_for_backtest(strategy_config, context_bindings)?;
    run_bar_backtest_with_outcome(&resolved_spec, bars, &mut strategy, context)
}

/// Execute a caller supplied Bar adapter through the shared application boundary.
///
/// Runtime-config driven callers (such as the CLI's Python/C ABI strategy route) bind their
/// venue and execution configuration at the facade, while engine execution and replay
/// verification remain owned by qx-app. This entry point intentionally does not publish
/// qx-app artifacts: callers with an established artifact contract may persist the verified
/// report through that contract.
pub fn run_bar_backtest_with_strategy(
    spec: &BacktestSpec,
    bars: &[Bar],
    strategy: &mut dyn BarStrategy,
    context: &RunContext,
) -> Result<BacktestReport, AppError> {
    run_bar_backtest_with_outcome(spec, bars, strategy, context).map(|outcome| outcome.report)
}

/// Execute a caller supplied Bar adapter and return audit metadata from the actual execution.
pub fn run_bar_backtest_with_outcome(
    spec: &BacktestSpec,
    bars: &[Bar],
    strategy: &mut dyn BarStrategy,
    context: &RunContext,
) -> Result<StrategyBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Bar 回测引擎执行")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        let instrument = InstrumentId::parse(&spec.instrument).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec.instrument 非法: {}", spec.instrument),
            )
        })?;
        if bars.len() < MIN_BACKTEST_BARS {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "样本过短：{} 根 Bar，Bar 回测下限 {} 根",
                    bars.len(),
                    MIN_BACKTEST_BARS
                ),
            ));
        }
        if bars.windows(2).any(|window| window[0].ts >= window[1].ts) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Bar 时间戳必须严格递增",
            ));
        }
        if spec.market_spec.as_ref().is_some_and(|market| {
            bars.iter().any(|bar| {
                bar.ts < market.valid_from || market.valid_to.is_some_and(|until| bar.ts >= until)
            })
        }) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Bar 数据时间超出 BacktestSpec.market_spec 的有效期",
            ));
        }
        let risk = backtest_risk_gate(spec);
        let risk_rule_set_version = risk.rule_set().version().to_string();
        let execution = bind_execution(spec)?;
        let config = BacktestConfig {
            instrument,
            instrument_spec: spec.market_spec.clone(),
            account_id: spec.account_id.as_deref().unwrap_or("app").to_string(),
            currency: spec.settlement_currency.clone(),
            initial_cash: Money::from_raw(spec.initial_cash_raw),
            multiplier: 1,
            fill: execution.fill,
            fee: execution.fee,
            data_tier: DataTier::Bar,
            latency: execution.latency,
            margin: bind_margin(spec),
            seed: spec.seed,
            risk,
            virtual_trading: execution.virtual_trading,
        };
        let engine = match spec.protective_exit {
            Some(policy) => BacktestEngine::new(config)
                .with_protective_exits(ProtectiveExitPolicy {
                    stop_loss_bps: policy.stop_loss_bps,
                    take_profit_bps: policy.take_profit_bps,
                    trailing_stop_bps: policy.trailing_stop_bps,
                })
                .map_err(|error| AppError::from_qx_error(&error))?,
            None => BacktestEngine::new(config),
        };
        let report = engine
            .run(bars, strategy)
            .map_err(|error| AppError::from_qx_error(&error))?;
        let replay =
            ReplayVerifier::verify(report.event_log.events(), &report.ledger).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("回测事件日志未通过重放校验: {error}"),
                )
            })?;
        if replay.log_digest != report.result_hash() {
            return Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                format!(
                    "结果哈希与重放哈希不一致: result={:016x} replay={:016x}",
                    report.result_hash(),
                    replay.log_digest
                ),
            ));
        }
        if report.equity.len() != bars.len() {
            return Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                format!(
                    "权益曲线点数 {} 与样本根数 {} 不一致",
                    report.equity.len(),
                    bars.len()
                ),
            ));
        }
        Ok(StrategyBacktestOutcome {
            report,
            risk_rule_set_version,
        })
    })
    .map_err(|error| attach(error, &correlation_id))
}
