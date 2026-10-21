//! Runtime-to-application bindings shared by backtest entry points.

pub use qx_app::artifact_store::{
    BacktestAccountBase, BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE,
    BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE, BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
    BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE, BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE,
};
pub use qx_app::ExecutionCostBinding;
pub use qx_runtime::{
    account_principal_note, reject_split_account_principal, DEFAULT_BACKTEST_INITIAL_CASH,
};

use qx_core::Money;
use qx_runtime::{BacktestPrincipal, BacktestPrincipalSource, RuntimeConfig};
use qx_xingban::ExecutionCostRules;
use qx_zhenlu::RiskGate;
use std::path::{Path, PathBuf};
mod builtin_snapshot;
pub mod single_strategy;
pub mod strategy_batch;
pub use builtin_snapshot::{
    prepare_builtin_backtest_snapshot, BuiltinBacktestSnapshotInput,
    PreparedBuiltinBacktestSnapshot,
};

/// Resolve runtime cost configuration once and project it into the shared application binding.
/// A relative rule path is anchored at the runtime file; defaults retain their configured source.
pub fn execution_cost_binding_from_config(
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
) -> Result<ExecutionCostBinding, String> {
    let Some(configured) = config.strategy.cost_rules_path.as_deref() else {
        return Ok(if runtime_config_path.is_some() {
            ExecutionCostBinding::runtime_config_default()
        } else {
            ExecutionCostBinding::builtin_default()
        });
    };
    let rules_path = match runtime_config_path {
        Some(base) => qx_runtime::resolve_runtime_relative_path(base, configured),
        None => PathBuf::from(configured),
    };
    let rules = ExecutionCostRules::load(&rules_path)
        .map_err(|error| format!("读取执行成本规则失败 {}: {error}", rules_path.display()))?;
    Ok(ExecutionCostBinding::from_rules_file(
        rules,
        rules_path.to_string_lossy().into_owned(),
    ))
}

/// Validate a configured cost rule using the same loader and path base as execution binding.
pub fn cost_rules_problem(runtime_config_path: &Path, configured: &str) -> Option<String> {
    let rules_path = qx_runtime::resolve_runtime_relative_path(runtime_config_path, configured);
    if !rules_path.is_file() {
        return Some(format!(
            "文件不存在: {} (configured={configured})",
            rules_path.display()
        ));
    }
    ExecutionCostRules::load(&rules_path)
        .err()
        .map(|error| format!("内容非法: {error}"))
}

pub fn default_execution_cost_binding() -> ExecutionCostBinding {
    ExecutionCostBinding::builtin_default()
}

/// Inputs already resolved by a facade for constructing the shared depth-backtest spec.
/// Runtime declarations and provenance are projected here once for all facades.
pub struct DepthBacktestSpecBindingInput<'a> {
    pub tier: &'a str,
    pub kind: qx_strategy::BuiltinStrategyKind,
    pub frame: &'a qx_xingban::DepthFrame,
    pub frame_path: &'a Path,
    pub market_spec: Option<qx_core::TradingInstrumentSpec>,
    pub market_spec_path: Option<&'a Path>,
    pub market_spec_version: &'a str,
    pub quantity: i64,
    pub fee_bps: Option<i64>,
    pub latency_snapshots: u64,
    pub market_impact_bps: i64,
    pub root: &'a Path,
    pub runtime: Option<&'a RuntimeConfig>,
    pub runtime_config_path: Option<&'a Path>,
    pub quality_manifest_path: Option<&'a Path>,
}

pub fn depth_backtest_spec_from_runtime(
    input: DepthBacktestSpecBindingInput<'_>,
) -> Result<qx_app::DepthBacktestSpec, String> {
    use qx_app::{BuiltinStrategySpec, DepthBacktestSpec, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION};
    use qx_core::Quantity;
    use qx_strategy::BuiltinStrategyConfig;
    let account_base = match input.runtime {
        Some(config) => account_base_from_config(config)?,
        None => backtest_initial_cash(None)?,
    };
    let risk_binding = backtest_risk_binding_from_config(input.runtime, false);
    let mut strategy_config = BuiltinStrategyConfig::new(
        input.kind,
        format!("builtin-{}", input.kind.name()),
        input.frame.instrument.clone(),
        Quantity::from_i64(input.quantity),
    )?;
    let config_label = input
        .runtime_config_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "runtime config".into());
    let signal_provenance = qx_runtime::apply_builtin_signal_overrides_from_runtime(
        &mut strategy_config,
        input.runtime.map(|runtime| &runtime.strategy),
        &config_label,
    )?;
    let strategy_spec = BuiltinStrategySpec {
        kind: input.kind,
        strategy_id: strategy_config.strategy_id,
        quantity_raw: strategy_config.quantity.raw(),
        fast_window: strategy_config.fast_window,
        slow_window: strategy_config.slow_window,
        period: strategy_config.period,
        threshold_bps: strategy_config.threshold_bps,
        target_notional_raw: None,
        target_available_margin_bps: None,
        target_volatility_bps: None,
        target_kelly_fraction_bps: None,
        kelly_lookback_bars: None,
    };
    let (execution_cost_rules, execution_cost_source) = match input.runtime {
        Some(config) => {
            let costs = execution_cost_binding_from_config(config, input.runtime_config_path)?;
            (Some(costs.rules.clone()), Some(costs.source()))
        }
        None => (None, None),
    };
    let (declared_ashare_rules, declared_fill_model, declared_bar_volume_participation_bps) =
        match input.runtime {
            Some(config) => {
                let strategy = &config.strategy;
                let ashare = strategy
                    .ashare_rules_path
                    .as_deref()
                    .or(strategy.ashare_actions_path.as_deref())
                    .or(strategy.ashare_calendar_path.as_deref());
                (
                    ashare.map(str::to_owned),
                    strategy.fill_model.clone(),
                    strategy.max_bar_volume_participation_bps,
                )
            }
            None => (None, None, None),
        };
    Ok(DepthBacktestSpec {
        schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        run_id: format!(
            "book-{}-{}-{}",
            input.tier,
            input.kind.name(),
            input.frame.instrument
        ),
        manifest_run_id: Some(format!(
            "depth-backtest:{}:{}:{}",
            input.tier,
            input.kind.name(),
            input.frame.instrument
        )),
        depth_path: input.frame_path.to_string_lossy().into_owned(),
        tier: input.tier.to_string(),
        settlement_currency: qx_core::resolve_settlement_currency(input.market_spec.as_ref()),
        initial_cash_raw: account_base.cash.raw(),
        fee_bps: input.fee_bps.map(i128::from),
        latency_snapshots: input.latency_snapshots,
        queue_position_bps: 0,
        queue_position_model: qx_xingban::QueuePositionModel::default(),
        market_impact_bps: i128::from(input.market_impact_bps),
        output_dir: input.root.to_string_lossy().into_owned(),
        strategy: strategy_spec,
        market_spec: input.market_spec,
        order_policy: None,
        market_making: None,
        account_id: Some("main".to_string()),
        portfolio: None,
        market_spec_path: input
            .market_spec_path
            .map(|path| path.to_string_lossy().into_owned()),
        market_spec_source: Some(input.market_spec_version.to_string()),
        account_base_source: Some(account_base.source.to_string()),
        signal_source: Some(signal_provenance.source.to_string()),
        signal_declared_unused: Some(signal_provenance.declared_unused),
        execution_cost_rules,
        execution_cost_source,
        risk_rules: app_backtest_risk_rules(risk_binding.rules.as_ref()),
        allow_short: Some(risk_binding.allow_short),
        declared_ashare_rules,
        declared_fill_model,
        declared_bar_volume_participation_bps,
        quality_manifest_path: input
            .quality_manifest_path
            .map(|path| path.to_string_lossy().into_owned()),
        runtime_config_path: input
            .runtime_config_path
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

pub struct MultiBuiltinBacktestBindingInput<'a> {
    pub kind: qx_strategy::BuiltinStrategyKind,
    pub frames: [&'a qx_datastruct::BarFrame; 2],
    pub bars: [&'a [qx_guanxing::Bar]; 2],
    pub market_specs: [Option<&'a qx_core::TradingInstrumentSpec>; 2],
    pub margin_models: [Option<&'a qx_app::BacktestMarginConfig>; 2],
    pub quantity: i64,
    pub funding_bps: i64,
    pub runtime: Option<&'a RuntimeConfig>,
    pub runtime_config_path: Option<&'a Path>,
}

pub struct MultiBuiltinBacktestRuntimeBindings<'a> {
    pub app_inputs: qx_app::MultiBuiltinBacktestInputs<'a>,
    pub primary_cash: Money,
    pub reference_cash: Money,
    pub risk_rule_set_version: String,
    pub risk_source: &'static str,
    pub cost_source: String,
    pub fill_model_name: &'static str,
    pub fill_model_source: &'static str,
    pub signal_note: String,
}

pub fn multi_builtin_backtest_bindings_from_runtime(
    input: MultiBuiltinBacktestBindingInput<'_>,
) -> Result<MultiBuiltinBacktestRuntimeBindings<'_>, String> {
    let costs = match input.runtime {
        Some(config) => execution_cost_binding_from_config(config, input.runtime_config_path)?,
        None => default_execution_cost_binding(),
    };
    let primary_cash = qx_app::multi_leg_leg_cash_reserve(
        input.quantity,
        input.bars[0],
        costs.rules.taker_bp,
        "主",
    )
    .map_err(|error| error.to_string())?;
    let reference_cash = qx_app::multi_leg_leg_cash_reserve(
        input.quantity,
        input.bars[1],
        costs.rules.taker_bp,
        "对冲",
    )
    .map_err(|error| error.to_string())?;
    let declared_initial_cash_override_raw = initial_cash_override_from_config(input.runtime)?;
    let risk_binding = backtest_risk_binding_from_config(input.runtime, true);
    if let Some(config) = input.runtime {
        let declares_ashare_rules = |strategy: &qx_runtime::StrategyRuntimeConfig| {
            strategy.ashare_rules_path.is_some()
                || strategy.ashare_actions_path.is_some()
                || strategy.ashare_calendar_path.is_some()
        };
        let declared_by = std::iter::once(("strategy".to_string(), &config.strategy))
            .chain(config.strategies.iter().map(|strategy| {
                (
                    format!(
                        "strategy[{}]",
                        strategy.id.as_deref().unwrap_or("<missing-id>")
                    ),
                    strategy,
                )
            }))
            .find(|(_, strategy)| declares_ashare_rules(strategy))
            .map(|(label, _)| label);
        if let Some(label) = declared_by {
            return Err(format!(
                "backtest multi-builtin 不接受 {label}.ashare_rules_path（含 ashare_actions_path / ashare_calendar_path）：一份 strategy 段无法同时描述两条腿所属标的的交易制度。需要 A 股规则口径请改用 strategy backtest 或 backtest builtin"
            ));
        }
    }
    let fill_configured = input
        .runtime
        .and_then(|config| config.strategy.fill_model.as_deref());
    let primary_fill = qx_app::bar_fill_model(fill_configured, input.market_specs[0])?;
    // Each leg must validate its own price tick for one-tick slippage, even though both legs use
    // the same fill model, name and provenance.
    let _reference_fill = qx_app::bar_fill_model(fill_configured, input.market_specs[1])?;
    let mut strategy_config = qx_strategy::BuiltinStrategyConfig::new_multi_leg(
        input.kind,
        format!("builtin-{}-multi", input.kind.name()),
        input.frames[0].instrument.clone(),
        input.frames[1].instrument.clone(),
        qx_core::Quantity::from_i64(input.quantity),
    )
    .map_err(|error| format!("多腿内置策略配置无效: {error}"))?;
    let signal_provenance = qx_runtime::apply_builtin_signal_overrides_from_runtime(
        &mut strategy_config,
        input.runtime.map(|config| &config.strategy),
        input
            .runtime_config_path
            .map(|path| path.display().to_string())
            .as_deref()
            .unwrap_or("runtime config"),
    )?;
    let signal_note = qx_app::artifact_store::BacktestSignalParams::from_builtin_config(
        &strategy_config,
        &signal_provenance,
    )
    .display_note();
    let strategy = qx_app::BuiltinStrategySpec::from_multi_leg_strategy_config(&strategy_config)
        .map_err(|error| error.to_string())?;
    let volume_participation_bps = input
        .runtime
        .and_then(|config| config.strategy.max_bar_volume_participation_bps);
    let risk_rule_set_version = risk_binding.gate().rule_set().version().to_string();
    let cost_source = costs.source();
    let app_inputs = qx_app::MultiBuiltinBacktestInputs {
        strategy,
        primary_frame: input.frames[0],
        reference_frame: input.frames[1],
        initial_cash_raw: [primary_cash.raw(), reference_cash.raw()],
        declared_initial_cash_override_raw,
        market_specs: input.market_specs,
        margin_models: input.margin_models,
        funding_bps: input.funding_bps,
        declared_product: input.runtime.and_then(|config| config.strategy.product),
        execution_cost_rules: costs.rules,
        risk_rules: app_backtest_risk_rules(risk_binding.rules.as_ref()),
        allow_short: risk_binding.allow_short,
        fill_model: fill_configured.map(|_| primary_fill.model),
        max_bar_volume_participation_bps: volume_participation_bps,
    };
    Ok(MultiBuiltinBacktestRuntimeBindings {
        app_inputs,
        primary_cash,
        reference_cash,
        risk_rule_set_version,
        risk_source: risk_binding.source(),
        cost_source,
        fill_model_name: primary_fill.name,
        fill_model_source: primary_fill.source,
        signal_note,
    })
}

/// Return an explicit single-account cash override only when runtime configuration declared one.
/// Multi-leg backtests use independently sized leg accounts, so they must reject configured cash
/// instead of silently accepting and discarding it.
pub fn initial_cash_override_from_config(
    config: Option<&RuntimeConfig>,
) -> Result<Option<i128>, String> {
    let base = match config {
        Some(config) => account_base_from_config(config)?,
        None => backtest_initial_cash(None)?,
    };
    if !matches!(
        base.source,
        BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE | BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE
    ) {
        return Ok(None);
    }
    Ok(Some(base.cash.raw()))
}

#[derive(Debug)]
pub struct BacktestRiskBinding {
    pub rules: Option<qx_runtime::RiskRulesConfig>,
    pub allow_short: bool,
    pub from_config: bool,
}

impl BacktestRiskBinding {
    pub fn gate(&self) -> RiskGate {
        crate::risk_host::strategy_risk_gate(self.rules.as_ref(), self.allow_short)
    }

    pub fn source(&self) -> &'static str {
        if self.from_config {
            "runtime-config"
        } else {
            "conservative-default"
        }
    }
}

fn project_principal(principal: BacktestPrincipal) -> BacktestAccountBase {
    let source = match principal.source {
        BacktestPrincipalSource::BuiltinDefault => BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        BacktestPrincipalSource::StrategyConfig => BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE,
        BacktestPrincipalSource::AccountRegistry => BACKTEST_ACCOUNT_BASE_REGISTRY_SOURCE,
        BacktestPrincipalSource::StrategyAndPaper => BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE,
    };
    BacktestAccountBase {
        cash: Money::from_raw(principal.initial_cash_raw),
        source,
    }
}

pub fn backtest_initial_cash(declared: Option<i128>) -> Result<BacktestAccountBase, String> {
    qx_runtime::backtest_initial_cash(declared).map(project_principal)
}

pub fn account_base_from_config(config: &RuntimeConfig) -> Result<BacktestAccountBase, String> {
    qx_runtime::account_base_from_config(config).map(project_principal)
}

pub fn app_backtest_risk_rules(
    rules: Option<&qx_runtime::RiskRulesConfig>,
) -> Option<qx_app::BacktestRiskRules> {
    rules.map(|rules| qx_app::BacktestRiskRules {
        version: rules.version.clone(),
        max_qty_raw: rules.max_qty_raw,
        max_notional_raw: rules.max_notional_raw,
        no_short: rules.no_short,
    })
}

pub fn backtest_risk_binding_from_config(
    config: Option<&RuntimeConfig>,
    default_allow_short: bool,
) -> BacktestRiskBinding {
    let Some(config) = config else {
        return BacktestRiskBinding {
            rules: None,
            allow_short: default_allow_short,
            from_config: false,
        };
    };
    let margin_mode = crate::risk_host::config_margin_mode(config);
    BacktestRiskBinding {
        rules: config.strategy.risk_rules.clone(),
        allow_short: crate::risk_host::strategy_allows_short(config, margin_mode),
        from_config: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn principal_projection_preserves_runtime_amount_and_provenance() {
        let principal = backtest_initial_cash(Some(Money::from_i64(250_000).raw()))
            .expect("positive configured principal is valid");
        assert_eq!(principal.cash, Money::from_i64(250_000));
        assert_eq!(principal.source, BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE);

        let default = backtest_initial_cash(None).expect("default principal is valid");
        assert_eq!(default.cash, Money::from_i64(DEFAULT_BACKTEST_INITIAL_CASH));
        assert_eq!(default.source, BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE);
    }

    #[test]
    fn risk_projection_preserves_version_limits_and_short_policy() {
        let rules = qx_runtime::RiskRulesConfig {
            version: "risk-v2".into(),
            max_qty_raw: Some(17),
            max_notional_raw: Some(29),
            no_short: true,
        };
        let app_rules = app_backtest_risk_rules(Some(&rules)).expect("rules are present");
        assert_eq!(app_rules.version, rules.version);
        assert_eq!(app_rules.max_qty_raw, rules.max_qty_raw);
        assert_eq!(app_rules.max_notional_raw, rules.max_notional_raw);
        assert_eq!(app_rules.no_short, rules.no_short);
        assert!(app_backtest_risk_rules(None).is_none());
    }

    #[test]
    fn execution_cost_defaults_preserve_their_configuration_provenance() {
        let config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config is valid");
        let configured =
            execution_cost_binding_from_config(&config, Some(Path::new("runtime.json")))
                .expect("runtime-config default binding is valid");
        let builtin = execution_cost_binding_from_config(&config, None)
            .expect("builtin default binding is valid");

        assert_eq!(configured.source(), "runtime-config-default");
        assert_eq!(builtin.source(), "builtin-default");
        assert_eq!(default_execution_cost_binding().source(), "builtin-default");
    }

    #[test]
    fn depth_spec_binding_projects_runtime_account_and_signal_once() {
        let runtime = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config is valid");
        let frame = qx_xingban::DepthFrame::from_json(include_str!(
            "../../../deploy/qianxing.depth-frame.l1.example.json"
        ))
        .expect("L1 depth example is valid");
        let spec = depth_backtest_spec_from_runtime(DepthBacktestSpecBindingInput {
            tier: "l1",
            kind: qx_strategy::BuiltinStrategyKind::SmaCross,
            frame: &frame,
            frame_path: Path::new("depth.json"),
            market_spec: None,
            market_spec_path: None,
            market_spec_version: "market-spec-default-v1",
            quantity: 1,
            fee_bps: None,
            latency_snapshots: 0,
            market_impact_bps: 0,
            root: Path::new("runs"),
            runtime: Some(&runtime),
            runtime_config_path: Some(Path::new("runtime.json")),
            quality_manifest_path: None,
        })
        .expect("runtime binding should produce a depth spec");

        assert_eq!(spec.depth_path, "depth.json");
        assert_eq!(spec.account_id.as_deref(), Some("main"));
        assert_eq!(
            spec.account_base_source.as_deref(),
            Some(BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE)
        );
        assert_eq!(
            spec.market_spec_source.as_deref(),
            Some("market-spec-default-v1")
        );
        assert_eq!(spec.runtime_config_path.as_deref(), Some("runtime.json"));
        assert!(spec.signal_source.is_some());
    }

    #[test]
    fn multi_builtin_binding_builds_the_strategy_from_the_runtime_snapshot() {
        let mut runtime = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config is valid");
        runtime.strategy.builtin_fast_window = Some(4);
        runtime.strategy.builtin_slow_window = Some(8);
        let primary = qx_datastruct::BarFrame::from_json(include_str!(
            "../../../deploy/qianxing.bar-frame.pairs-primary.example.json"
        ))
        .expect("primary BarFrame is valid");
        let reference = qx_datastruct::BarFrame::from_json(include_str!(
            "../../../deploy/qianxing.bar-frame.pairs-reference.example.json"
        ))
        .expect("reference BarFrame is valid");
        let primary_bars: Vec<qx_guanxing::Bar> = (&primary).into();
        let reference_bars: Vec<qx_guanxing::Bar> = (&reference).into();

        let bindings =
            multi_builtin_backtest_bindings_from_runtime(MultiBuiltinBacktestBindingInput {
                kind: qx_strategy::BuiltinStrategyKind::PairsArbitrage,
                frames: [&primary, &reference],
                bars: [&primary_bars, &reference_bars],
                market_specs: [None, None],
                margin_models: [None, None],
                quantity: 1,
                funding_bps: 0,
                runtime: Some(&runtime),
                runtime_config_path: None,
            })
            .expect("runtime binding should produce multi-leg app inputs");

        assert_eq!(
            bindings.app_inputs.strategy.kind,
            qx_strategy::BuiltinStrategyKind::PairsArbitrage
        );
        assert_eq!(bindings.app_inputs.strategy.fast_window, 4);
        assert!(bindings.signal_note.contains("fast_window=4"));
    }
}
