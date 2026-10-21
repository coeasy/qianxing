mod common;

use common::{depth_spec as spec, perpetual_market_spec, scratch};
use qx_app::{
    run_depth_backtest, verify_depth_run, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    PortfolioBacktestConfig, PortfolioStrategySpec, RunContext,
};
use qx_core::{MarginMode, OrderPolicy, PositionMode, PositionSide};
use qx_strategy::BuiltinStrategyKind;
use std::path::Path;

const SCALE: i128 = 1_000_000_000;

fn write_kelly_depth_frame(root: &Path, name: &str, levels: usize) -> String {
    let snapshots = (0..48)
        .map(|index| {
            let mid = 100 + index * 2 + if index % 2 == 0 { 0 } else { 5 };
            let levels = |offset: i128| {
                (0..levels)
                    .map(|level| {
                        serde_json::json!({
                            "price": (mid + offset * (level as i128 + 1)) * SCALE,
                            "qty": 100 * SCALE
                        })
                    })
                    .collect::<Vec<_>>()
            };
            serde_json::json!({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1000 + index as u64 * 1000,
                "sequence": index as u64 + 1,
                "bids": levels(-1),
                "asks": levels(1)
            })
        })
        .collect::<Vec<_>>();
    let path = root.join(name);
    std::fs::write(
        &path,
        serde_json::json!({
            "schema_version": 1,
            "source": "qx-app-kelly-depth-test",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots
        })
        .to_string(),
    )
    .expect("write Kelly depth fixture");
    path.to_string_lossy().into_owned()
}

#[test]
fn l1_and_l2_builtin_kelly_sizing_runs_through_live_cash_snapshots() {
    let root = scratch("app-depth-kelly-sizing");
    for tier in ["l1", "l2"] {
        let run_id = format!("depth-kelly-{tier}");
        let mut request = spec(&run_id, &root, tier, if tier == "l1" { 1 } else { 2 });
        request.depth_path = write_kelly_depth_frame(
            &root,
            &format!("{run_id}.json"),
            if tier == "l1" { 1 } else { 2 },
        );
        request.strategy.kind = BuiltinStrategyKind::Momentum;
        request.strategy.period = 2;
        request.strategy.threshold_bps = 1;
        request.strategy.target_kelly_fraction_bps = Some(5_000);
        request.strategy.kelly_lookback_bars = Some(30);
        let context = RunContext::new(CallerCapability::Research, &run_id);
        let first = run_depth_backtest(&request, &context).expect("Kelly depth run");
        let replay = verify_depth_run(&first, &context).expect("verify Kelly artifacts");
        assert!(replay.verified, "{:?}", replay.mismatches);
        assert_eq!(first.equity_points, 48);
        assert!(first.fills > 0, "Kelly sample should trade on {tier}");
        assert_eq!(
            first.result_hash,
            run_depth_backtest(&request, &context)
                .expect("deterministic Kelly rerun")
                .result_hash
        );
    }
}

#[test]
fn depth_spot_cash_fraction_and_portfolio_member_sizing_are_supported() {
    let root = scratch("app-depth-cash-sizing");
    let mut request = spec("depth-cash-fraction", &root, "l1", 1);
    request.strategy.target_available_margin_bps = Some(2_500);
    request.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: request.initial_cash_raw / 2,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: request.initial_cash_raw / 2,
            strategy: {
                let mut strategy =
                    BuiltinStrategySpec::new(BuiltinStrategyKind::Momentum, "depth-momentum-kelly");
                strategy.fast_window = 2;
                strategy.slow_window = 3;
                strategy.target_kelly_fraction_bps = Some(5_000);
                strategy.kelly_lookback_bars = Some(30);
                strategy
            },
        }],
    });
    let outcome = run_depth_backtest(
        &request,
        &RunContext::new(CallerCapability::Research, "depth-cash-fraction"),
    )
    .expect("cash-fraction primary and Kelly portfolio member");
    assert!(outcome.fills > 0);
    assert!(
        verify_depth_run(
            &outcome,
            &RunContext::new(CallerCapability::Research, "verify-depth-cash-fraction")
        )
        .unwrap()
        .verified
    );
}

#[test]
fn depth_market_spec_and_order_policy_fail_closed_on_missing_or_out_of_range_contracts() {
    let root = scratch("app-depth-product-guards");
    let mut request = spec("depth-perpetual-guards", &root, "l1", 1);
    request.market_spec = Some(perpetual_market_spec());
    request.strategy.target_notional_raw = Some(100 * SCALE);
    assert_eq!(
        request.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    request.strategy.target_notional_raw = None;
    assert_eq!(
        request.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    request.order_policy = Some(OrderPolicy {
        reduce_only: false,
        position_side: PositionSide::Net,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        leverage: 2,
        post_only: false,
    });
    request.strategy.target_kelly_fraction_bps = Some(5_000);
    request.strategy.kelly_lookback_bars = Some(30);
    assert_eq!(
        request.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    request.strategy.target_kelly_fraction_bps = None;
    request.strategy.kelly_lookback_bars = None;
    request.market_spec.as_mut().unwrap().valid_to = Some(47_000);
    assert_eq!(
        run_depth_backtest(
            &request,
            &RunContext::new(CallerCapability::Research, "depth-perpetual-guards"),
        )
        .unwrap_err()
        .category(),
        AppErrorCategory::InvalidInput
    );
}
