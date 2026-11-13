mod common;

use common::{depth_spec, scratch};
use qx_app::{
    read_multi_account_report_v1, run_multi_account_depth_backtest, AppErrorCategory,
    CallerCapability, MultiAccountDepthBacktestSpec, MultiAccountResearchStatusV1, RunContext,
};
use std::path::Path;

fn spec(root: &Path) -> MultiAccountDepthBacktestSpec {
    let mut first = depth_spec("depth-account-a", root, "l1", 1);
    first.account_id = Some("account-a".into());
    first.output_dir = root.join("account-a").to_string_lossy().into_owned();
    let mut second = depth_spec("depth-account-b", root, "l1", 1);
    second.account_id = Some("account-b".into());
    second.output_dir = root.join("account-b").to_string_lossy().into_owned();
    let depth_json = std::fs::read_to_string(&second.depth_path).expect("depth fixture exists");
    let alt_market = depth_json
        .replace("BTCUSDT", "ETHUSDT")
        .replace("BINANCE", "OKX");
    let alt_path = root.join("eth-okx-depth.json");
    std::fs::write(&alt_path, alt_market).expect("write second market fixture");
    second.depth_path = alt_path.to_string_lossy().into_owned();
    MultiAccountDepthBacktestSpec {
        schema_version: 1,
        batch_id: "depth-batch".into(),
        runs: vec![second, first],
    }
}

#[test]
fn independent_depth_accounts_are_stable_and_failures_are_isolated() {
    let root = scratch("multi-account-depth");
    let batch = spec(&root);
    let context = RunContext::new(CallerCapability::Research, "depth-batch");
    let result = run_multi_account_depth_backtest(&batch, &context).expect("depth batch runs");
    assert_eq!(result.schema_version, 2);
    assert_eq!(result.result_kind, "multi_account_depth");
    let parent_manifest = result.run_manifest.as_ref().expect("parent manifest");
    let parent_spec = qx_spec::CanonicalRunSpec::from_json(
        parent_manifest
            .canonical_run_spec_json
            .as_deref()
            .expect("parent canonical spec"),
    )
    .expect("valid parent spec");
    assert_eq!(parent_spec.run_type, "multi_account_depth_backtest");
    assert_eq!(
        parent_spec
            .execution_model
            .get("cross_account_margin_netting")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    qx_app::artifact_store::verify_multi_account_depth_backtest_result(&result)
        .expect("parent and child identities verify");
    let report = read_multi_account_report_v1(&serde_json::to_value(&result).unwrap(), &context)
        .expect("shared report reads and verifies every Depth child");
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
    assert!(result.summary.complete);
    assert_eq!(result.summary.accounts_succeeded, 2);
    assert_eq!(result.summary.accounts_failed, 0);
    assert_eq!(result.runs[0].account_id, "account-a");
    assert_eq!(result.runs[1].account_id, "account-b");
    assert_eq!(
        result.runs[0].outcome.as_ref().unwrap().instrument,
        "BTCUSDT.BINANCE"
    );
    assert_eq!(
        result.runs[1].outcome.as_ref().unwrap().instrument,
        "ETHUSDT.OKX"
    );
    assert!(result.runs.iter().all(|run| run.outcome.is_some()));
    assert!(result.summary.successful_fills > 0);
    assert_eq!(
        result,
        run_multi_account_depth_backtest(&batch, &context).unwrap()
    );

    let mut partial = spec(&root);
    partial.runs[0].depth_path = root
        .join("missing-depth.json")
        .to_string_lossy()
        .into_owned();
    let partial =
        run_multi_account_depth_backtest(&partial, &context).expect("isolated member error");
    assert_eq!(
        partial
            .run_manifest
            .as_ref()
            .map(|manifest| manifest.run_id.as_str()),
        Some("depth-batch")
    );
    qx_app::artifact_store::verify_multi_account_depth_backtest_result(&partial)
        .expect("partial parent and child identities verify");
    assert!(!partial.summary.complete);
    assert_eq!(partial.summary.accounts_succeeded, 1);
    assert_eq!(partial.summary.accounts_failed, 1);
    assert!(partial.runs.iter().any(|run| run.error_json.is_some()));
}

#[test]
fn multi_account_depth_result_verifier_rejects_summary_tampering_and_checks_all_failed() {
    let root = scratch("multi-account-depth-verify");
    let batch = spec(&root);
    let context = RunContext::new(CallerCapability::Research, "depth-batch");
    let mut result = run_multi_account_depth_backtest(&batch, &context).expect("batch runs");
    result.summary.successful_fills += 1;
    assert!(qx_app::artifact_store::verify_multi_account_depth_backtest_result(&result).is_err());

    let mut all_failed = spec(&root);
    for run in &mut all_failed.runs {
        run.depth_path = root
            .join(format!(
                "missing-{}.json",
                run.account_id.as_deref().unwrap()
            ))
            .to_string_lossy()
            .into_owned();
    }
    let failed = run_multi_account_depth_backtest(&all_failed, &context)
        .expect("account-level failures are returned");
    assert!(failed.run_manifest.is_none());
    assert!(failed.runs.iter().all(|run| run.outcome.is_none()));
    qx_app::artifact_store::verify_multi_account_depth_backtest_result(&failed)
        .expect("all-failed result is internally consistent");
    let report = read_multi_account_report_v1(&serde_json::to_value(&failed).unwrap(), &context)
        .expect("shared report retains an all-failed Depth batch without child artifacts");
    assert!(!report.summary.complete);
    assert_eq!(report.summary.accounts_failed, 2);
    assert!(report.accounts.iter().all(|account| {
        account.status == qx_app::MultiAccountResearchStatusV1::Failed && account.report.is_none()
    }));
}

#[test]
fn depth_batch_rejects_duplicate_accounts_and_mixed_settlement_currency() {
    let root = scratch("multi-account-depth-validation");
    let mut batch = spec(&root);
    batch.runs[1].account_id = batch.runs[0].account_id.clone();
    assert_eq!(
        batch.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    batch = spec(&root);
    batch.runs[1].settlement_currency = "USD".into();
    assert_eq!(
        batch.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn depth_batch_rejects_unrepresentable_total_initial_cash_before_execution() {
    let root = scratch("multi-account-depth-cash-overflow");
    let mut batch = spec(&root);
    batch.runs[0].initial_cash_raw = i128::MAX;
    assert_eq!(
        batch.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
