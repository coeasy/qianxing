use crate::{
    run_multi_account_depth_backtest, AppErrorCategory, CallerCapability,
    MultiAccountDepthBacktestSpec, RunContext,
};
use std::path::Path;

fn spec(root: &Path) -> MultiAccountDepthBacktestSpec {
    let mut first = super::depth_backtest::spec("depth-account-a", root, "l1", 1);
    first.account_id = Some("account-a".into());
    first.output_dir = root.join("account-a").to_string_lossy().into_owned();
    let mut second = super::depth_backtest::spec("depth-account-b", root, "l1", 1);
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
    let root = super::fixtures::scratch("multi-account-depth");
    let batch = spec(&root);
    let context = RunContext::new(CallerCapability::Research, "depth-batch");
    let result = run_multi_account_depth_backtest(&batch, &context).expect("depth batch runs");
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
    assert!(!partial.summary.complete);
    assert_eq!(partial.summary.accounts_succeeded, 1);
    assert_eq!(partial.summary.accounts_failed, 1);
    assert!(partial.runs.iter().any(|run| run.error_json.is_some()));
}

#[test]
fn depth_batch_rejects_duplicate_accounts_and_mixed_settlement_currency() {
    let root = super::fixtures::scratch("multi-account-depth-validation");
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
