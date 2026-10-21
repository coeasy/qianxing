//! Runtime-to-BacktestSpec bindings for single-strategy Bar backtests.

use super::{
    account_base_from_config, app_backtest_risk_rules, backtest_risk_binding_from_config,
    execution_cost_binding_from_config,
};
use crate::backtest_bindings::BacktestAccountBase;
use qx_runtime::RuntimeConfig;
use std::path::Path;

pub struct SingleStrategyBacktestBindingInput<'a> {
    pub config: &'a RuntimeConfig,
    pub runtime_config_path: Option<&'a Path>,
    pub frame: &'a qx_datastruct::BarFrame,
    pub input_path: String,
    pub strategy_id: &'a str,
    pub instrument_spec: Option<qx_core::TradingInstrumentSpec>,
    pub margin_model: Option<qx_app::BacktestMarginConfig>,
    pub instrument_spec_version: &'a str,
    pub ashare_rules: Option<qx_xingban::AshareRuleConfig>,
    pub ashare_rules_path: Option<&'a str>,
}

pub struct SingleStrategyBacktestRuntimeBindings {
    pub app_spec: qx_app::BacktestSpec,
    pub account_base: BacktestAccountBase,
    pub cost_source: String,
    pub fill_model_name: &'static str,
    pub fill_model_source: &'static str,
    pub builtin_execution: Option<qx_app::StrategyBacktestExecution>,
    pub builtin_signal: Option<qx_app::artifact_store::BacktestSignalParams>,
    pub builtin_signal_note: Option<String>,
}

/// Resolve runtime-owned execution policy and project one immutable application spec.
/// The facade still owns file reads and construction of arbitrary strategy adapters.
pub fn single_strategy_backtest_bindings_from_runtime(
    input: SingleStrategyBacktestBindingInput<'_>,
) -> Result<SingleStrategyBacktestRuntimeBindings, String> {
    let account_id = input
        .config
        .strategy
        .account_id
        .clone()
        .unwrap_or_else(|| "backtest".into());
    let account_base = account_base_from_config(input.config)?;
    let risk_binding = backtest_risk_binding_from_config(Some(input.config), false);
    let costs = execution_cost_binding_from_config(input.config, input.runtime_config_path)?;
    let fill = qx_app::bar_fill_model(
        input.config.strategy.fill_model.as_deref(),
        input.instrument_spec.as_ref(),
    )?;
    let cost_source = costs.source_with_ashare_rules(input.ashare_rules_path);
    let strategy = qx_app::BuiltinStrategySpec::new(
        qx_strategy::BuiltinStrategyKind::SmaCross,
        input.strategy_id.to_string(),
    );
    let app_spec = qx_app::assemble_strategy_backtest_spec(
        qx_app::BacktestDatasetInput {
            run_id: "strategy-backtest".into(),
            instrument: input.frame.instrument.to_string(),
            bars_path: input.input_path,
        },
        strategy,
        qx_app::StrategyBacktestBindings {
            settlement_currency: input
                .instrument_spec
                .as_ref()
                .map(|spec| spec.settlement_currency.clone())
                .unwrap_or_else(|| qx_core::resolve_settlement_currency(None)),
            initial_cash_raw: account_base.cash.raw(),
            seed: 20260911,
            output_dir: ".".into(),
            account_id: account_id.clone(),
            market_spec: input.instrument_spec.clone(),
            margin_model: input.margin_model,
            execution_cost_rules: Some(costs.rules),
            ashare_rules: input.ashare_rules,
            risk_rules: app_backtest_risk_rules(risk_binding.rules.as_ref()),
            allow_short: risk_binding.allow_short,
            fill_model: fill.model,
            max_bar_volume_participation_bps: input
                .config
                .strategy
                .max_bar_volume_participation_bps,
        },
    );

    let (builtin_execution, builtin_signal, builtin_signal_note) =
        if input.config.strategy.builtin_strategy.is_some() {
            if let Some(configured) = input.config.strategy.instrument.as_deref() {
                let configured = qx_core::InstrumentId::parse(configured)
                    .ok_or_else(|| format!("Strategy instrument 非法: {configured}"))?;
                if configured != input.frame.instrument {
                    return Err(format!(
                        "内置策略回测 instrument 不一致: strategy={} frame={}",
                        configured, input.frame.instrument
                    ));
                }
            }
            let builtin_config = qx_runtime::builtin_strategy_config_from_runtime(
                &input.config.strategy,
                &input.frame.instrument,
            )
            .map_err(|error| match input.runtime_config_path {
                Some(path) => format!("{error}；runtime_config={}", path.display()),
                None => error,
            })?;
            let provenance =
                qx_runtime::builtin_signal_provenance(builtin_config.kind, &input.config.strategy);
            let signal = qx_app::artifact_store::BacktestSignalParams::from_builtin_config(
                &builtin_config,
                &provenance,
            );
            let note = signal.display_note();
            (
                Some(qx_app::StrategyBacktestExecution::NativeBuiltin {
                    strategy: Box::new(builtin_config),
                    venue_id: input.config.strategy.venue_id.clone(),
                }),
                Some(signal),
                Some(note),
            )
        } else {
            (None, None, None)
        };

    Ok(SingleStrategyBacktestRuntimeBindings {
        app_spec,
        account_base,
        cost_source,
        fill_model_name: fill.name,
        fill_model_source: fill.source,
        builtin_execution,
        builtin_signal,
        builtin_signal_note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest_bindings::BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE;

    #[test]
    fn runtime_binding_projects_builtin_strategy_and_execution_policy() {
        let mut config = RuntimeConfig::from_json(include_str!(
            "../../../../deploy/qianxing.runtime.example.json"
        ))
        .expect("runtime example config is valid");
        config.strategy.builtin_strategy = Some("sma_cross".into());
        let frame = qx_datastruct::BarFrame::from_json(include_str!(
            "../../../../deploy/qianxing.bar-frame.example.json"
        ))
        .expect("BarFrame example is valid");

        let bindings =
            single_strategy_backtest_bindings_from_runtime(SingleStrategyBacktestBindingInput {
                config: &config,
                runtime_config_path: Some(Path::new("runtime.json")),
                frame: &frame,
                input_path: "bars.json".into(),
                strategy_id: "sma_cross",
                instrument_spec: None,
                margin_model: None,
                instrument_spec_version: "market-spec-default-v1",
                ashare_rules: None,
                ashare_rules_path: None,
            })
            .expect("runtime bindings should produce an application spec");

        assert_eq!(bindings.app_spec.bars_path, "bars.json");
        assert_eq!(bindings.app_spec.strategy.strategy_id, "sma_cross");
        assert_eq!(
            bindings.account_base.source,
            BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE
        );
        assert_eq!(bindings.cost_source, "runtime-config-default");
        assert!(bindings.builtin_execution.is_some());
        assert!(bindings.builtin_signal.is_some());
        assert!(bindings.builtin_signal_note.is_some());
    }
}
