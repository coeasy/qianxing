use qx_sdk::{
    BacktestDatasetInput, BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability,
    Client, DatasetSpec,
};
use std::{fs, path::PathBuf, time::SystemTime};

// Small illustrative dataset, not historical market data.
const SAMPLE_BARS: &str = include_str!("../bars.json");
const SCALE: i128 = 1_000_000_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let parent = PathBuf::from("runs");
    fs::create_dir_all(&parent)?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let run_dir = parent.join(format!("sample-{}-{nonce}", std::process::id()));
    // Do not reuse or overwrite a previous run directory.
    fs::create_dir(&run_dir)?;
    let bars_path = run_dir.join("bars.json");
    fs::write(&bars_path, SAMPLE_BARS)?;
    let bars = bars_path.to_string_lossy().into_owned();

    let client = Client::new(CallerCapability::Research, "rust-quickstart");
    let dataset = client.validate_dataset(&DatasetSpec::new("sample-bars", bars.clone()))?;
    if !dataset.usable {
        return Err(format!("sample data is unusable: {:?}", dataset.gaps).into());
    }
    if dataset.rows != 70 {
        return Err(format!("expected 70 sample bars, found {}", dataset.rows).into());
    }
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sma-example");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    let spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "rust-quickstart".into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: bars,
        },
        "USDT",
        100_000 * SCALE,
        20261010,
        run_dir.to_string_lossy().into_owned(),
        strategy,
    );
    let outcome = client.run_backtest(&spec)?;
    if outcome.fills != 1 {
        return Err(format!("expected 1 sample fill, found {}", outcome.fills).into());
    }
    let verified = client.verify_run(&outcome)?;
    if !verified.verified {
        return Err(format!("verification failed: {:?}", verified.mismatches).into());
    }
    println!("Illustrative data only; not historical market data.");
    println!(
        "Bars: {}; fills: {}; result hash: {}",
        dataset.rows, outcome.fills, outcome.result_hash
    );
    println!(
        "Verified: {}; output: {}",
        verified.verified,
        run_dir.display()
    );
    println!("Summary: {}", outcome.artifacts.summary);
    println!("Equity: {}", outcome.artifacts.equity);
    println!("Fills: {}", outcome.artifacts.fills);
    Ok(())
}
