# qx-sdk

`qx-sdk` is the stable Rust facade for embedding Qianxing's deterministic research and backtest use cases. It keeps the public integration surface in one crate: consumers do not need to add `qx-app` or the internal domain crates directly.

The SDK runs locally in your Rust process. It does not fetch market data or submit exchange orders. Supply a validated BarFrame from your own data pipeline, run a read-only preflight, execute the backtest, and verify the resulting artifacts.

## Install

```toml
[dependencies]
qx-sdk = "0.4.3"
```

Install Rust 1.87 or later to use the [0.4.3 SDK release](https://crates.io/crates/qx-sdk/0.4.3) after it is published. Until then, use `qx-sdk = "0.4.2"`.

Create a consumer project and fetch the matching teaching fixture:

```bash
cargo new qx-quickstart
cd qx-quickstart
cargo add qx-sdk@0.4.3
curl -L https://raw.githubusercontent.com/coeasy/qianxing/v0.4.3/deploy/qianxing.bar-frame.example.json -o bars.json
```

On Windows PowerShell, use `Invoke-WebRequest -Uri https://raw.githubusercontent.com/coeasy/qianxing/v0.4.3/deploy/qianxing.bar-frame.example.json -OutFile bars.json` for the last command after the release is published. Put the Rust snippet below in `src/main.rs`, then run `cargo run`.

## Run your first backtest

The input is a BarFrame JSON file whose instrument matches `BacktestDatasetInput.instrument`. A checked-in 70-bar fixture is available at [`deploy/qianxing.bar-frame.example.json`](https://github.com/coeasy/qianxing/blob/v0.4.3/deploy/qianxing.bar-frame.example.json). Download it as `bars.json`, then run this from a fresh project directory after the release is published:

```rust,no_run
use qx_sdk::{
    BacktestDatasetInput, BacktestPreflightCheckStatusV1, BacktestSpec,
    BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "sma-2-3");
    strategy.fast_window = 2;
    strategy.slow_window = 3;

    let spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "first-run".into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: "bars.json".into(),
        },
        "USDT",
        100_000 * 1_000_000_000, // 100,000 USDT at the SDK's 1e-9 raw scale
        20261010,
        "runs/first-run",
        strategy,
    );
    let client = Client::new(CallerCapability::Research, "first-run");

    // Read-only: checks the dataset, instrument, strategy history, and rule window.
    let readiness = client.preflight_backtest(&spec)?;
    if !readiness.ready {
        let blocked = readiness.checks.iter()
            .filter(|check| check.status == BacktestPreflightCheckStatusV1::Blocked)
            .map(|check| format!("{}: {}", check.id, check.detail.as_deref().unwrap_or("blocked")))
            .collect::<Vec<_>>();
        return Err(format!("backtest preflight blocked: {}", blocked.join("; ")).into());
    }

    // The run re-reads and validates its inputs; preflight is not a cached approval.
    let outcome = client.run_backtest(&spec)?;
    let verification = client.verify_run(&outcome)?;
    if !verification.verified {
        return Err(format!("artifact verification failed: {:?}", verification.mismatches).into());
    }

    println!(
        "verified={} fills={} result_hash={}",
        verification.verified, outcome.fills, outcome.result_hash
    );
    Ok(())
}
```

The fixture contains synthetic teaching data, not exchange history. Its run is for integration validation only; it does not demonstrate strategy performance. Replace `bars.json` with your own BarFrame and preserve its source, time range, and market assumptions when interpreting results.

## Next steps

- `Client::start_backtest` returns a cancellable `RunHandle` for bounded waits and lifecycle events.
- `builtin_strategy_recipe_catalog()` exposes strategy parameters, data needs, and research cautions from the Rust-owned catalog.
- `RemoteApiClient` accesses a running Qianxing API through a `RemoteTransport` implementation. For remote service use, handle TLS, authentication, timeouts, and connection reuse in that transport.
- For API migration details and compatibility guidance, see the [SDK migration guide](https://github.com/coeasy/qianxing/blob/v0.4.2/docs/qx-sdk-migration.md) and [API documentation](https://docs.rs/qx-sdk).

Paper and live control commands are separate from local research. They require a configured service, authorization, and an explicitly verified venue capability; this quickstart does not enable or perform trading.
