use crate::{
    app_backtest_risk_rules, backtest_settlement_currency, BacktestRiskBinding,
    BuiltinStrategyConfig, BuiltinStrategyKind, ExecutionCostBinding,
};
use qx_core::{Money, TradingInstrumentSpec};
use qx_datastruct::BarFrame;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct AppLegInput<'a> {
    pub kind: BuiltinStrategyKind,
    pub funding_bps: i64,
    pub primary_frame: &'a BarFrame,
    pub reference_frame: &'a BarFrame,
    pub primary_path: &'a Path,
    pub reference_path: &'a Path,
    pub primary_spec: Option<&'a TradingInstrumentSpec>,
    pub reference_spec: Option<&'a TradingInstrumentSpec>,
    pub primary_margin_model: Option<&'a qx_app::BacktestMarginConfig>,
    pub reference_margin_model: Option<&'a qx_app::BacktestMarginConfig>,
    pub primary_cash: Money,
    pub reference_cash: Money,
    pub strategy_config: &'a BuiltinStrategyConfig,
    pub costs: &'a ExecutionCostBinding,
    pub risk: &'a BacktestRiskBinding,
    pub fill_model: Option<qx_app::BacktestFillModel>,
    pub max_bar_volume_participation_bps: Option<u16>,
}

pub(super) struct AppLegRuns {
    pub run: qx_app::MultiBuiltinBacktestRun,
    pub risk_rule_versions: Vec<String>,
}

struct LegSpecInput<'a> {
    run_id: &'a str,
    frame: &'a BarFrame,
    path: &'a Path,
    cash: Money,
    market: Option<&'a TradingInstrumentSpec>,
    margin_model: Option<&'a qx_app::BacktestMarginConfig>,
    account_id: &'a str,
    output_dir: &'a str,
    app: &'a AppLegInput<'a>,
}

struct ScratchOutput {
    path: PathBuf,
    serial: usize,
}

impl ScratchOutput {
    fn create() -> Result<Self, String> {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "qx-cli manifest 必须位于 workspace 根目录下".to_string())?
            .join("target/qx-cli-multibuiltin-app")
            .join(format!("{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("创建 qx-app 多腿临时产物目录失败: {error}"))?;
        Ok(Self { path: root, serial })
    }
}

impl Drop for ScratchOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(super) fn run(input: AppLegInput<'_>) -> Result<AppLegRuns, String> {
    let scratch = ScratchOutput::create()?;
    let primary_run_id = format!(
        "multi-builtin-{}-primary-{}",
        input.kind.name(),
        scratch.serial
    );
    let reference_run_id = format!(
        "multi-builtin-{}-reference-{}",
        input.kind.name(),
        scratch.serial
    );
    let mut strategy = qx_app::BuiltinStrategySpec::new(
        input.kind,
        format!("builtin-{}-multi", input.kind.name()),
    );
    strategy.quantity_raw = input.strategy_config.quantity.raw();
    strategy.fast_window = input.strategy_config.fast_window;
    strategy.slow_window = input.strategy_config.slow_window;
    strategy.period = input.strategy_config.period;
    strategy.threshold_bps = input.strategy_config.threshold_bps;
    let primary_output = scratch.path.join("primary");
    let primary_output_text = primary_output.to_string_lossy();
    let primary = leg_spec(LegSpecInput {
        run_id: &primary_run_id,
        frame: input.primary_frame,
        path: input.primary_path,
        cash: input.primary_cash,
        market: input.primary_spec,
        margin_model: input.primary_margin_model,
        account_id: "multi-leg-primary",
        output_dir: &primary_output_text,
        app: &input,
    })?;
    let reference_output = scratch.path.join("reference");
    let reference_output_text = reference_output.to_string_lossy();
    let reference = leg_spec(LegSpecInput {
        run_id: &reference_run_id,
        frame: input.reference_frame,
        path: input.reference_path,
        cash: input.reference_cash,
        market: input.reference_spec,
        margin_model: input.reference_margin_model,
        account_id: "multi-leg-reference",
        output_dir: &reference_output_text,
        app: &input,
    })?;
    let run = qx_app::run_multi_builtin_backtest(
        &qx_app::MultiBuiltinBacktestSpec {
            schema_version: qx_app::MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
            strategy,
            primary,
            reference,
            funding_bps: input.funding_bps,
        },
        &qx_app::RunContext::new(qx_app::CallerCapability::Research, &primary_run_id)
            .with_code_commit(crate::build_identity::BUILD_REVISION),
    )
    .map_err(|error| format!("qx-app 独立账本多腿回测失败: {}", error.to_json()))?;
    let risk_rule_versions = vec![
        risk_summary(&run.primary.outcome.artifacts.summary)?,
        risk_summary(&run.reference.outcome.artifacts.summary)?,
    ];
    Ok(AppLegRuns {
        run,
        risk_rule_versions,
    })
}

fn leg_spec(input: LegSpecInput<'_>) -> Result<qx_app::BacktestSpec, String> {
    let mut placeholder = qx_app::BuiltinStrategySpec::new(
        BuiltinStrategyKind::SmaCross,
        format!("{}-scheduled", input.run_id),
    );
    placeholder.quantity_raw = input.app.strategy_config.quantity.raw();
    let mut spec = qx_app::BacktestSpec::new(
        qx_app::BacktestDatasetInput {
            run_id: input.run_id.into(),
            instrument: input.frame.instrument.to_string(),
            bars_path: input.path.to_string_lossy().into_owned(),
        },
        backtest_settlement_currency(input.market),
        input.cash.raw(),
        20260914,
        input.output_dir,
        placeholder,
    );
    spec.account_id = Some(input.account_id.into());
    spec.market_spec = input.market.cloned();
    spec.margin_model = input.margin_model.cloned();
    spec.execution_cost_rules = Some(input.app.costs.rules.clone());
    spec.risk_rules = app_backtest_risk_rules(input.app.risk.rules.as_ref());
    spec.allow_short = Some(input.app.risk.allow_short);
    spec.fill_model = input.app.fill_model;
    spec.max_bar_volume_participation_bps = input.app.max_bar_volume_participation_bps;
    Ok(spec)
}

fn risk_summary(path: &str) -> Result<String, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 qx-app 风控摘要失败 {path}: {error}"))?;
    let summary: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("解析 qx-app 风控摘要失败 {path}: {error}"))?;
    summary["risk_rule_set_version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("qx-app 风控摘要缺少 risk_rule_set_version: {path}"))
}
