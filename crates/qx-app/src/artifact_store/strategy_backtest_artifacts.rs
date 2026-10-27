use super::{
    persist_backtest_artifacts, BacktestAccountBase, BacktestArtifacts, BacktestInputProvenance,
    BacktestSampleTimestamps, BacktestSignalParams,
};
use qx_core::InstrumentId;
use qx_xingban::BacktestReport;
use std::path::{Path, PathBuf};

pub struct StrategyBacktestArtifactsInput<'a> {
    pub runtime_config_path: Option<&'a Path>,
    pub market_spec_path: Option<&'a Path>,
    pub strategy_id: &'a str,
    pub instrument: &'a InstrumentId,
    pub bars: &'a [qx_guanxing::Bar],
    pub risk_rule_set_version: &'a str,
    pub cost_source: &'a str,
    pub fill_model: (&'static str, &'static str),
    pub account_base: BacktestAccountBase,
    pub input: BacktestInputProvenance,
    pub signal: Option<BacktestSignalParams>,
    pub dataset_quality: Option<&'a serde_json::Value>,
}

pub struct StrategyBacktestArtifactsResult {
    pub summary_path: PathBuf,
    pub equity_path: PathBuf,
    pub fills_path: PathBuf,
    pub rejections: Vec<(String, usize)>,
}

pub fn persist_strategy_backtest_artifacts(
    run_manifest_path: &Path,
    report: &BacktestReport,
    input: StrategyBacktestArtifactsInput<'_>,
) -> Result<StrategyBacktestArtifactsResult, String> {
    let rejections = crate::trade_facts::rejection_facts(&report.event_log);
    let summary_extra = serde_json::json!({
        "metrics_exact": report.exact_metrics_json(),
    });
    let (summary_path, equity_path, fills_path) = persist_backtest_artifacts(
        run_manifest_path,
        &BacktestArtifacts {
            runtime_config_path: input.runtime_config_path,
            account_id: None,
            portfolio: None,
            summary_extra: Some(&summary_extra),
            market_spec_path: input.market_spec_path,
            strategy_id: input.strategy_id,
            instrument: input.instrument,
            sample_unit: "bar",
            samples: input.bars.len(),
            sample_ts: BacktestSampleTimestamps::Bars(input.bars),
            equity: &report.equity,
            equity_exact: Some(&report.equity_exact),
            positions: &report.positions,
            fills: &report.fills,
            execution_attempts: &report.execution_attempts,
            clock_start: report.clock_start,
            clock_end: report.clock_end,
            input_data_hash: report.input_data_hash,
            result_hash: report.result_hash(),
            event_log: &report.event_log,
            ledger: &report.ledger,
            issuer_capital: &report.issuer_capital_snapshots,
            issuer_capital_at_end: report.issuer_capital_at(report.clock_end),
            return_bps: report.return_bps,
            max_drawdown_bps: report.max_drawdown_bps,
            fees_raw: report.fees_raw,
            turnover_raw: report.turnover_raw,
            final_equity_raw: report.final_equity(),
            assumptions: &report.assumptions,
            model_descriptors: &report.model_descriptors,
            risk_rule_set_version: input.risk_rule_set_version,
            risk_rule_source: "runtime-config",
            cost_source: input.cost_source,
            fill_model: Some(input.fill_model),
            fill_model_details: Some(&report.fill_model_descriptor),
            execution_model_details: None,
            account_base: input.account_base,
            matching_kernel: crate::cases::kernels::BAR_MATCHING_KERNEL,
            rejections: &rejections,
            input: input.input,
            signal: input.signal,
            dataset_quality: input.dataset_quality,
        },
    )?;
    Ok(StrategyBacktestArtifactsResult {
        summary_path,
        equity_path,
        fills_path,
        rejections,
    })
}
