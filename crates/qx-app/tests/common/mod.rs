//! Public-API integration fixtures for qx-app use cases.

use qx_app::{
    BacktestDatasetInput, BacktestSpec, BuiltinStrategySpec, DatasetSpec, DepthBacktestSpec,
    ExperimentParameterSpace, RunExperimentSpec, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
};
use qx_core::{
    InstrumentId, MarginMode, OrderPolicy, PositionMode, PositionSide, TradingInstrumentSpec,
    TradingProduct,
};
use qx_strategy::BuiltinStrategyKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const SCALE: i128 = 1_000_000_000;

pub fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("qx-app integration tests live below the workspace root")
        .join("target")
        .join(format!(
            "qx-app-integration-tmp/{label}-{}-{serial}",
            std::process::id()
        ));
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("remove stale integration test directory");
    }
    std::fs::create_dir_all(&root).expect("create integration test directory");
    root
}

#[allow(dead_code)]
pub fn frame_json(instrument: &str, bars: usize) -> String {
    let mut ts = Vec::new();
    let mut open = Vec::new();
    let mut high = Vec::new();
    let mut low = Vec::new();
    let mut close = Vec::new();
    let mut volume = Vec::new();
    for index in 0..bars {
        let phase = (index % 20) as i128;
        let price = if phase < 10 {
            100 + phase * 2
        } else {
            100 + (20 - phase) * 2
        };
        ts.push(1_000 + (index as u64) * 1_000);
        open.push(price * SCALE);
        high.push((price + 1) * SCALE);
        low.push((price - 1) * SCALE);
        close.push(price * SCALE);
        volume.push(SCALE);
    }
    serde_json::json!({
        "schema_version": 1,
        "instrument": instrument,
        "source": "qx-app-test-v1",
        "ts": ts,
        "open_raw": open,
        "high_raw": high,
        "low_raw": low,
        "close_raw": close,
        "volume_raw": volume,
    })
    .to_string()
}

#[allow(dead_code)]
pub fn write_frame(root: &Path, name: &str, instrument: &str, bars: usize) -> String {
    let path = root.join(name);
    std::fs::write(&path, frame_json(instrument, bars)).expect("write BarFrame fixture");
    path.to_string_lossy().into_owned()
}

pub fn backtest_spec(run_id: &str, bars_path: &str, output_dir: &Path) -> BacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "app-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    BacktestSpec::new(
        BacktestDatasetInput {
            run_id: run_id.into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: bars_path.into(),
        },
        "USDT",
        100_000 * SCALE,
        20261010,
        output_dir.to_string_lossy().into_owned(),
        strategy,
    )
}

#[allow(dead_code)]
pub fn dataset_spec(dataset_id: &str, bars_path: &str) -> DatasetSpec {
    DatasetSpec::new(dataset_id, bars_path)
}

#[allow(dead_code)]
pub fn experiment_spec(
    experiment_id: &str,
    bars_path: &str,
    output_dir: &Path,
    fast_windows: Vec<i128>,
) -> RunExperimentSpec {
    RunExperimentSpec {
        schema_version: 1,
        experiment_id: experiment_id.to_string(),
        base: backtest_spec("unused-base-run", bars_path, output_dir),
        parameter_space: vec![ExperimentParameterSpace {
            name: "fast_window".into(),
            values: fast_windows,
        }],
        strategy_kinds: Vec::new(),
        split_plan: None,
        baselines: Vec::new(),
        search: None,
        cost_sensitivity: Vec::new(),
    }
}

#[allow(dead_code)]
pub fn depth_frame_json(level_count: usize) -> String {
    let snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            };
            let levels = |offset: i128| {
                (0..level_count)
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
    serde_json::json!({
        "schema_version": 1,
        "source": "qx-app-depth-test",
        "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
        "snapshots": snapshots
    })
    .to_string()
}

#[allow(dead_code)]
pub fn write_depth_frame(root: &Path, name: &str, level_count: usize) -> String {
    let path = root.join(name);
    std::fs::write(&path, depth_frame_json(level_count)).expect("write depth fixture");
    path.to_string_lossy().into_owned()
}

#[allow(dead_code)]
pub fn depth_spec(run_id: &str, root: &Path, tier: &str, level_count: usize) -> DepthBacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "depth-sma");
    strategy.quantity_raw = SCALE;
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    DepthBacktestSpec {
        schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        run_id: run_id.into(),
        depth_path: write_depth_frame(root, "depth.json", level_count),
        tier: tier.into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 1_000_000 * SCALE,
        fee_bps: Some(0),
        latency_snapshots: 0,
        queue_position_bps: 0,
        queue_position_model: qx_xingban::QueuePositionModel::Disabled,
        market_impact_bps: 0,
        funding_bps: 0,
        funding_events: Vec::new(),
        output_dir: root.join("runs").to_string_lossy().into_owned(),
        strategy,
        market_spec: None,
        order_policy: None,
        market_making: None,
        account_id: None,
        portfolio: None,
        manifest_run_id: None,
        execution_cost_rules: None,
        execution_cost_source: None,
        risk_rules: None,
        allow_short: None,
        declared_ashare_rules: None,
        declared_fill_model: None,
        declared_bar_volume_participation_bps: None,
        quality_manifest_path: None,
        runtime_config_path: None,
        market_spec_path: None,
        market_spec_source: None,
        account_base_source: None,
        signal_source: None,
        signal_declared_unused: None,
    }
}

#[allow(dead_code)]
pub fn perpetual_market_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1_000,
        valid_to: Some(49_000),
        exact_rules: None,
    }
}

#[allow(dead_code)]
pub fn cross_margin_order_policy() -> OrderPolicy {
    OrderPolicy {
        reduce_only: false,
        position_side: PositionSide::Net,
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        leverage: 2,
        post_only: false,
    }
}
