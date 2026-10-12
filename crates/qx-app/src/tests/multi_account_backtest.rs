use crate::cases::{run_multi_account_backtest, verify_run};
use crate::context::{CallerCapability, RunContext};
use crate::error::AppErrorCategory;
use crate::spec::MultiAccountBacktestSpec;
use crate::tests::fixtures::{backtest_spec, scratch, write_frame};

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

fn two_account_spec(root: &std::path::Path) -> MultiAccountBacktestSpec {
    let btc = write_frame(root, "btc.json", "BTCUSDT.BINANCE", 80);
    let eth = write_frame(root, "eth.json", "ETHUSDT.BINANCE", 80);
    let mut first = backtest_spec("account-run-btc", &btc, &root.join("out"));
    first.account_id = Some("account-btc".into());
    let mut second = backtest_spec("account-run-eth", &eth, &root.join("out"));
    second.instrument = "ETHUSDT.BINANCE".into();
    second.account_id = Some("account-eth".into());
    second.strategy.strategy_id = "eth-sma".into();
    MultiAccountBacktestSpec {
        schema_version: 1,
        batch_id: "batch-two-markets".into(),
        runs: vec![second, first],
    }
}

#[test]
fn independent_accounts_run_different_markets_and_keep_verifiable_ledgers() {
    let root = scratch("multi-account-backtest");
    let spec = two_account_spec(&root);
    let result = run_multi_account_backtest(&spec, &research("batch-two-markets"))
        .expect("independent multi-account research runs");

    assert_eq!(result.runs.len(), 2);
    assert!(result.summary.complete);
    assert_eq!(result.summary.accounts_succeeded, 2);
    assert_eq!(result.summary.accounts_failed, 0);
    assert!(result.summary.successful_fills > 0);
    assert!(result.summary.weighted_return_bps.is_some());
    assert_eq!(result.runs[0].account_id, "account-btc");
    assert_eq!(result.runs[1].account_id, "account-eth");
    assert_eq!(
        result.runs[0].outcome.as_ref().unwrap().instrument,
        "BTCUSDT.BINANCE"
    );
    assert_eq!(
        result.runs[1].outcome.as_ref().unwrap().instrument,
        "ETHUSDT.BINANCE"
    );
    assert_ne!(
        result.runs[0].outcome.as_ref().unwrap().data_fingerprint,
        result.runs[1].outcome.as_ref().unwrap().data_fingerprint
    );
    for account_run in &result.runs {
        verify_run(
            account_run.outcome.as_ref().unwrap(),
            &research(&account_run.outcome.as_ref().unwrap().run_id),
        )
        .expect("each account run verifies independently");
    }
    let repeated = run_multi_account_backtest(&spec, &research("batch-two-markets"))
        .expect("idempotent batch repeat");
    assert_eq!(result, repeated);
}

#[test]
fn one_account_failure_is_reported_without_erasing_successful_runs() {
    let root = scratch("multi-account-partial");
    let mut spec = two_account_spec(&root);
    spec.runs[0].bars_path = root.join("missing.json").to_string_lossy().into_owned();

    let result = run_multi_account_backtest(&spec, &research("batch-partial"))
        .expect("member failures are isolated");
    assert!(!result.summary.complete);
    assert_eq!(result.summary.accounts_succeeded, 1);
    assert_eq!(result.summary.accounts_failed, 1);
    assert!(result.summary.weighted_return_bps.is_some());
    assert!(result.runs.iter().any(|run| run.outcome.is_some()));
    assert!(result.runs.iter().any(|run| run.error_json.is_some()));
}

#[test]
fn multi_account_spec_rejects_duplicate_accounts_and_mixed_settlement_currencies() {
    let root = scratch("multi-account-invalid");
    let mut spec = two_account_spec(&root);
    spec.runs[1].account_id = spec.runs[0].account_id.clone();
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    spec = two_account_spec(&root);
    spec.runs[1].settlement_currency = "USD".into();
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
