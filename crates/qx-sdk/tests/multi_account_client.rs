use qx_sdk::{
    BacktestDatasetInput, BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability,
    Client, DepthBacktestSpec, MultiAccountBacktestSpec, MultiAccountDepthBacktestSpec, RunStatus,
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
        BacktestDatasetInput {
            run_id: run_id.into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: bars.to_string_lossy().into_owned(),
        },
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

#[test]
fn independent_consumer_runs_sync_and_async_multi_account_depth_batches() {
    let root =
        std::env::temp_dir().join(format!("qx-sdk-multi-account-depth-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create isolated artifact root");
    let fixture = repo_root().join("deploy/qianxing.depth-frame.l1.example.json");
    let alternate = root.join("eth-okx-depth.json");
    let content = std::fs::read_to_string(&fixture).expect("read repository depth example");
    std::fs::write(
        alternate,
        content
            .replace("BTCUSDT", "ETHUSDT")
            .replace("BINANCE", "OKX"),
    )
    .expect("write alternate depth example");
    let mut depth_strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sdk-depth-sma");
    depth_strategy.quantity_raw = 1_000_000_000;
    depth_strategy.fast_window = 2;
    depth_strategy.slow_window = 3;
    let first = DepthBacktestSpec {
        schema_version: 1,
        run_id: "sdk-depth-account-a".into(),
        depth_path: fixture.to_string_lossy().into_owned(),
        tier: "l1".into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 100_000 * 1_000_000_000,
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 0,
        queue_position_model: Default::default(),
        market_impact_bps: 0,
        output_dir: root.join("account-a").to_string_lossy().into_owned(),
        strategy: depth_strategy,
        market_spec: None,
        order_policy: None,
        market_making: None,
        account_id: Some("account-a".into()),
        portfolio: None,
    };
    let mut second = first.clone();
    second.run_id = "sdk-depth-account-b".into();
    second.account_id = Some("account-b".into());
    second.depth_path = root
        .join("eth-okx-depth.json")
        .to_string_lossy()
        .into_owned();
    second.output_dir = root.join("account-b").to_string_lossy().into_owned();
    let batch = MultiAccountDepthBacktestSpec {
        schema_version: 1,
        batch_id: "sdk-depth-batch".into(),
        runs: vec![second, first],
    };
    let client = Client::new(CallerCapability::Research, "sdk-depth-batch");
    let result = client
        .run_multi_account_depth_backtest(&batch)
        .expect("run depth batch through the stable Rust SDK");
    assert_eq!(result.summary.accounts_succeeded, 2);
    assert_eq!(
        result.runs[0].outcome.as_ref().unwrap().instrument,
        "BTCUSDT.BINANCE"
    );
    assert_eq!(
        result.runs[1].outcome.as_ref().unwrap().instrument,
        "ETHUSDT.OKX"
    );
    let mut handle = client.start_multi_account_depth_backtest(batch);
    assert_eq!(handle.run_id(), "sdk-depth-batch");
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let async_result = handle
        .try_take_result()
        .expect("terminal result is available")
        .expect("batch succeeds");
    assert_eq!(async_result.summary, result.summary);
    assert_eq!(async_result.spec_fingerprint, result.spec_fingerprint);
}
