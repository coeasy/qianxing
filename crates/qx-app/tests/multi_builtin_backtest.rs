mod common;

use common::scratch;
use qx_app::{
    run_multi_builtin_backtest, BacktestDatasetInput, BacktestSpec, BuiltinStrategySpec,
    CallerCapability, MultiBuiltinBacktestSpec, RunContext, MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
};
use qx_strategy::BuiltinStrategyKind;
use std::path::{Path, PathBuf};

fn deploy_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
}

fn leg_spec(
    run_id: &str,
    instrument: &str,
    bars_path: &Path,
    output_dir: &Path,
    initial_cash_raw: i128,
    account_id: &str,
) -> BacktestSpec {
    let mut spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: run_id.into(),
            instrument: instrument.into(),
            bars_path: bars_path.to_string_lossy().into_owned(),
        },
        "USDT",
        initial_cash_raw,
        20260914,
        output_dir.to_string_lossy().into_owned(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, format!("{run_id}-scheduled")),
    );
    spec.account_id = Some(account_id.into());
    // The historical multi-builtin chain allows the short leg, including when a market spec is
    // omitted and the market therefore follows the spot multiplier-1 compatibility rule.
    spec.allow_short = Some(true);
    spec
}

#[test]
fn app_independent_multi_builtin_matches_legacy_default_leg_hashes() {
    let deploy = deploy_root();
    let primary_path = deploy.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference_path = deploy.join("qianxing.bar-frame.pairs-reference.example.json");
    let root = scratch("independent-multi-builtin");
    let mut strategy = BuiltinStrategySpec::new(
        BuiltinStrategyKind::PairsArbitrage,
        "builtin-pairs_arbitrage-multi",
    );
    strategy.quantity_raw = 1_000_000_000;
    let spec = MultiBuiltinBacktestSpec {
        schema_version: MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
        strategy,
        primary: leg_spec(
            "multi-builtin-primary",
            "BTCUSDT.BINANCE",
            &primary_path,
            &root.join("primary"),
            126_064_000_000_000,
            "multi-leg-primary",
        ),
        reference: leg_spec(
            "multi-builtin-reference",
            "ETHUSDT.BINANCE",
            &reference_path,
            &root.join("reference"),
            100_000_000_000_000,
            "multi-leg-reference",
        ),
        funding_bps: 0,
    };
    let context = RunContext::new(CallerCapability::Research, "multi-builtin-parity");
    let result = run_multi_builtin_backtest(&spec, &context).expect("run app multi-builtin");
    assert_eq!(
        format!("{:016x}", result.primary.report.result_hash()),
        "4e40f0d5e8e79687"
    );
    assert_eq!(
        format!("{:016x}", result.reference.report.result_hash()),
        "4674f7667f4ead02"
    );
    assert_eq!(result.primary.report.fills.len(), 3);
    assert_eq!(result.reference.report.fills.len(), 3);
    std::fs::remove_dir_all(root).expect("remove independent multi-builtin artifacts");
}
