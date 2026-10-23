mod common;

use common::{scratch, write_frame};
use qx_app::{
    run_multi_account_joint_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    JointBacktestAccount, MultiAccountJointBacktestSpec, MultiMarketBacktestMarket,
    PortfolioStrategySpec, RunContext,
};
use qx_strategy::BuiltinStrategyKind;
use serde_json::Value;

const SCALE: i128 = 1_000_000_000;

fn spec(root: &std::path::Path, run_id: &str) -> MultiAccountJointBacktestSpec {
    let markets = [
        ("okx", "BTC-USDT.OKX"),
        ("binance", "ETHUSDT.BINANCE"),
        ("bitget", "PEPEUSDT.BITGET"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (exchange_id, instrument))| {
        let bars_path = write_frame(root, &format!("market-{index}.json"), instrument, 80);
        let mut strategy = BuiltinStrategySpec::new(
            BuiltinStrategyKind::SmaCross,
            format!("{}-sma", exchange_id),
        );
        strategy.fast_window = 2;
        strategy.slow_window = 3;
        JointBacktestAccount {
            exchange_id: exchange_id.into(),
            account_id: "primary".into(),
            initial_cash_raw: 200_000 * SCALE,
            markets: vec![MultiMarketBacktestMarket {
                instrument: instrument.into(),
                bars_path,
                dataset_manifest_path: None,
                priority: 0,
                market_spec: None,
                fee_schedule: None,
                order_policy: None,
                margin_model: None,
                funding_bps: 0,
                funding_events: Vec::new(),
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 50_000 * SCALE,
                    strategy,
                }],
            }],
        }
    })
    .collect();
    MultiAccountJointBacktestSpec {
        schema_version: 1,
        run_id: run_id.into(),
        settlement_currency: "USDT".into(),
        seed: 7,
        output_dir: root.join("out").to_string_lossy().into_owned(),
        accounts: markets,
    }
}

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn joint_run_combines_three_exchanges_and_keeps_accounts_isolated() {
    let root = scratch("joint-multi-exchange");
    let spec = spec(&root, "joint-multi-exchange");
    spec.validate()
        .expect("three venue account groups validate");
    let outcome = run_multi_account_joint_backtest(&spec, &research("joint-multi-exchange"))
        .expect("joint exchange backtest");

    assert_eq!(outcome.accounts_total, 3);
    assert_eq!(outcome.markets_total, 3);
    assert_eq!(outcome.initial_cash_raw, 600_000 * SCALE);
    assert_eq!(outcome.accounts.len(), 3);
    assert!(outcome
        .accounts
        .iter()
        .all(|account| account.account_id == "primary"));
    assert_eq!(
        outcome
            .accounts
            .iter()
            .map(|account| account.exchange_id.as_str())
            .collect::<Vec<_>>(),
        vec!["binance", "bitget", "okx"]
    );
    assert!(outcome.successful_fills > 0);

    let equity: Vec<(u64, i128)> = serde_json::from_str(
        &std::fs::read_to_string(&outcome.equity_path).expect("joint equity file exists"),
    )
    .expect("joint equity JSON");
    assert_eq!(equity.len(), 80);
    assert_eq!(equity.last().unwrap().1, outcome.final_equity_raw);

    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifact_path).expect("joint artifact exists"),
    )
    .expect("joint artifact JSON");
    assert_eq!(
        artifact["aggregation"],
        "sum_of_independent_exchange_account_equity_no_cross_account_margin_netting"
    );
    assert_eq!(
        artifact.pointer("/outcome/accounts_total"),
        Some(&Value::from(3))
    );

    let repeated = run_multi_account_joint_backtest(&spec, &research("joint-multi-exchange"))
        .expect("same joint run is idempotent");
    assert_eq!(outcome.spec_fingerprint, repeated.spec_fingerprint);
    assert_eq!(outcome.final_equity_raw, repeated.final_equity_raw);
}

#[test]
fn joint_accounts_may_overlap_markets_but_each_account_must_match_its_exchange() {
    let root = scratch("joint-multi-exchange-invalid");
    let mut overlap = spec(&root, "joint-overlapping-instrument");
    overlap.accounts[1].exchange_id = "okx".into();
    overlap.accounts[1].account_id = "secondary".into();
    overlap.accounts[1].markets[0].instrument = overlap.accounts[0].markets[0].instrument.clone();
    overlap.accounts[1].markets[0].bars_path = write_frame(
        &root,
        "overlap-market.json",
        &overlap.accounts[0].markets[0].instrument,
        80,
    );
    overlap
        .validate()
        .expect("isolated accounts may trade the same instrument");
    let outcome = run_multi_account_joint_backtest(&overlap, &research("joint-overlapping-run"))
        .expect("overlapping isolated accounts run independently");
    assert_eq!(outcome.accounts_total, 3);
    assert_eq!(outcome.markets_total, 3);

    let mut duplicate_within_account = overlap.clone();
    let same_market = duplicate_within_account.accounts[0].markets[0].clone();
    duplicate_within_account.accounts[0]
        .markets
        .push(same_market);
    assert_eq!(
        duplicate_within_account.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut mismatch = spec(&root, "joint-venue-mismatch");
    mismatch.accounts[0].exchange_id = "binance".into();
    assert_eq!(
        mismatch.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
