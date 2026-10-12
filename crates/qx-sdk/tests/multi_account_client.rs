use qx_sdk::{
    BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client,
    MultiAccountBacktestSpec, RunStatus,
};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-sdk has a repository root")
        .to_path_buf()
}

fn multi_account_spec(run_id: &str, account_id: &str, output: &Path) -> BacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, run_id);
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    let bars = repo_root().join("deploy/qianxing.bar-frame.example.json");
    let mut spec = BacktestSpec::new(
        run_id,
        "BTCUSDT.BINANCE",
        bars.to_string_lossy(),
        "USDT",
        100_000 * 1_000_000_000,
        20261010,
        output.to_string_lossy(),
        strategy,
    );
    spec.account_id = Some(account_id.to_owned());
    spec
}

#[test]
fn independent_consumer_runs_sync_and_async_multi_account_batches() {
    let root = std::env::temp_dir().join(format!("qx-sdk-multi-account-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create isolated artifact root");
    let batch = MultiAccountBacktestSpec {
        schema_version: 1,
        batch_id: "sdk-batch".into(),
        runs: vec![
            multi_account_spec("sdk-batch-a", "account-a", &root.join("account-a")),
            multi_account_spec("sdk-batch-b", "account-b", &root.join("account-b")),
        ],
    };
    let client = Client::new(CallerCapability::Research, "sdk-batch");
    let result = client
        .run_multi_account_backtest(&batch)
        .expect("run batch through the stable Rust SDK");
    assert_eq!(result.summary.accounts_succeeded, 2);
    assert_eq!(result.summary.accounts_failed, 0);
    assert!(result.summary.successful_fills > 0);

    let mut handle = client.start_multi_account_backtest(batch);
    assert_eq!(handle.run_id(), "sdk-batch");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let async_result = handle
        .try_take_result()
        .expect("terminal result is available")
        .expect("batch succeeds");
    assert_eq!(async_result.summary, result.summary);
    assert_eq!(async_result.spec_fingerprint, result.spec_fingerprint);
}
