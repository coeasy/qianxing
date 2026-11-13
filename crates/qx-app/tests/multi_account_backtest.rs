mod common;

use common::{backtest_spec, scratch, write_frame};
use qx_app::{
    read_multi_account_report_v1, run_backtest, run_multi_account_backtest,
    verify_multi_account_backtest_result, verify_run, AppErrorCategory, CallerCapability,
    MultiAccountBacktestSpec, MultiAccountResearchStatusV1, RunContext,
};

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
    assert_eq!(result.schema_version, 2);
    assert!(result.summary.complete);
    assert_eq!(result.summary.accounts_succeeded, 2);
    assert_eq!(result.summary.accounts_failed, 0);
    assert!(result.summary.successful_fills > 0);
    assert!(result.summary.weighted_return_bps.is_some());
    let manifest = result
        .run_manifest
        .as_ref()
        .expect("successful batch carries a parent RunManifest");
    assert_eq!(manifest.run_id, result.batch_id);
    assert_eq!(manifest.config_hash, result.spec_fingerprint);
    let canonical: serde_json::Value =
        serde_json::from_str(manifest.canonical_run_spec_json.as_deref().unwrap()).unwrap();
    assert_eq!(canonical["run_type"], "multi_account_backtest");
    assert_eq!(canonical["datasets"].as_array().unwrap().len(), 2);
    assert!(!canonical["execution_model"]
        .get("cross_account_margin_netting")
        .and_then(serde_json::Value::as_bool)
        .unwrap());
    verify_multi_account_backtest_result(&result)
        .expect("parent identity binds child runs and aggregate summary");
    let report = read_multi_account_report_v1(
        &serde_json::to_value(&result).unwrap(),
        &research("batch-two-markets"),
    )
    .expect("shared report reads and verifies every Bar child");
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.run_id, result.batch_id);
    assert_eq!(report.summary.accounts_succeeded, 2);
    assert!(report.verification.verified);
    assert!(report.accounts.iter().all(|account| {
        account.status == MultiAccountResearchStatusV1::Succeeded
            && account
                .report
                .as_ref()
                .is_some_and(|report| report.verification.verified)
    }));
    let mut altered = result.clone();
    altered.summary.successful_fills += 1;
    assert!(verify_multi_account_backtest_result(&altered).is_err());
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
fn changing_another_accounts_capital_does_not_change_this_accounts_run() {
    let root = scratch("multi-account-capital-isolation");
    let mut batch = two_account_spec(&root);
    // Keep account-b safely representable while changing its buying power by
    // two orders of magnitude. Account-a's execution must remain identical.
    let account_a = batch
        .runs
        .iter()
        .find(|run| run.account_id.as_deref() == Some("account-btc"))
        .unwrap()
        .clone();
    let context = research("capital-isolation");
    let baseline = run_multi_account_backtest(&batch, &context).unwrap();
    let baseline_a = baseline
        .runs
        .iter()
        .find(|run| run.account_id == "account-btc")
        .unwrap()
        .outcome
        .as_ref()
        .unwrap();

    let other = batch
        .runs
        .iter_mut()
        .find(|run| run.account_id.as_deref() == Some("account-eth"))
        .unwrap();
    other.initial_cash_raw = other.initial_cash_raw.checked_mul(100).unwrap();
    let changed = run_multi_account_backtest(&batch, &context).unwrap();
    let changed_a = changed
        .runs
        .iter()
        .find(|run| run.account_id == "account-btc")
        .unwrap()
        .outcome
        .as_ref()
        .unwrap();

    // Compare the account execution identity and metrics, excluding artifact
    // paths, which are deployment details and are not financial semantics.
    assert_eq!(baseline_a.result_hash, changed_a.result_hash);
    assert_eq!(baseline_a.fills, changed_a.fills);
    assert_eq!(baseline_a.return_bps, changed_a.return_bps);
    assert_eq!(baseline_a.max_drawdown_bps, changed_a.max_drawdown_bps);

    let mut solo = account_a;
    solo.output_dir = root.join("solo-account-a").to_string_lossy().into_owned();
    let solo_context = research("capital-isolation-account-btc");
    let solo_outcome = run_backtest(&solo, &solo_context).unwrap();
    assert_eq!(baseline_a.result_hash, solo_outcome.result_hash);
    assert_eq!(baseline_a.fills, solo_outcome.fills);
    println!(
        "MULTI_ACCOUNT_ISOLATION_OBSERVED account=account-btc other_account_capital_multiplier=100 fills={} result_hash={} isolated_from_solo=true",
        baseline_a.fills, baseline_a.result_hash
    );
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
    assert!(
        result.run_manifest.is_some(),
        "partial batch keeps parent identity"
    );
    verify_multi_account_backtest_result(&result)
        .expect("partial batch identity binds success and failure status");
    let report = read_multi_account_report_v1(
        &serde_json::to_value(&result).unwrap(),
        &research("batch-partial"),
    )
    .expect("shared report preserves partial account success");
    assert_eq!(report.summary.accounts_failed, 1);
    assert_eq!(
        report
            .accounts
            .iter()
            .filter(|account| account.status == MultiAccountResearchStatusV1::Failed)
            .count(),
        1
    );
}

#[test]
fn all_failed_batch_has_no_dataset_run_manifest_and_still_verifies_as_a_failure_result() {
    let root = scratch("multi-account-all-failed");
    let mut spec = two_account_spec(&root);
    for run in &mut spec.runs {
        run.bars_path = root
            .join(format!(
                "missing-{}.json",
                run.account_id.as_deref().unwrap()
            ))
            .to_string_lossy()
            .into_owned();
    }
    let result = run_multi_account_backtest(&spec, &research("batch-all-failed"))
        .expect("each account failure is retained in the batch result");
    assert_eq!(result.summary.accounts_succeeded, 0);
    assert_eq!(result.summary.accounts_failed, 2);
    assert!(result.run_manifest.is_none());
    verify_multi_account_backtest_result(&result)
        .expect("an all-failed batch makes no dataset-backed run claim");
    let report = read_multi_account_report_v1(
        &serde_json::to_value(&result).unwrap(),
        &research("batch-all-failed"),
    )
    .expect("shared report retains an all-failed batch without inventing child outcomes");
    assert!(!report.summary.complete);
    assert_eq!(report.summary.accounts_failed, 2);
    assert!(report.accounts.iter().all(|account| {
        account.status == MultiAccountResearchStatusV1::Failed && account.report.is_none()
    }));
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

#[test]
fn multi_account_spec_rejects_unrepresentable_total_initial_cash_before_execution() {
    let root = scratch("multi-account-cash-overflow");
    let mut spec = two_account_spec(&root);
    spec.runs[0].initial_cash_raw = i128::MAX;
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
