# Qianxing Python SDK

`qianxing` is the Python entry point for the Qianxing quantitative research and trading platform. The wheel bundles a Rust native extension; using the installed SDK does not require a Rust toolchain.

The extension uses PyO3's Python 3.10 stable ABI. Release builds publish one `cp310-abi3` wheel per supported OS and CPU architecture, usable on CPython 3.10 and newer; native platform binaries still require separate Windows, macOS, and Linux wheels.

## Current SDK surface

The public `qianxing` namespace currently exposes the shared Rust application use cases for local Bar dataset validation, deterministic Bar backtests, artifact verification, and deterministic comparison of completed runs. Comparisons require the same instrument and data fingerprint, then rank by return, drawdown, and run ID. Backtest facts, fixed-point arithmetic, matching, risk, and metrics remain implemented by the Rust engine.

The wheel also exposes the existing versioned strategy contract as `qianxing.strategy`, plus the optional adapters as `qianxing.ccxt` and `qianxing.ashare`. Those connector modules preserve their own documented boundaries and are not a substitute for the not-yet-complete shared Paper/Live application workflows.

Tick and OrderBook application workflows, long-running Run handles, Paper orchestration, and Live controls are not yet exposed through this SDK facade. The repository has additional lower-level CLI, worker, connector, and strategy capabilities; their presence does not mean they are already part of the high-level Python API or approved for production trading.

## Example

```python
from qianxing import BacktestSpec, BuiltinStrategySpec, run_backtest, verify_run

spec = BacktestSpec(
    run_id="sample-001",
    instrument="BTCUSDT.BINANCE",
    bars_path="deploy/qianxing.bar-frame.example.json",
    settlement_currency="USDT",
    initial_cash_raw=100_000_000_000_000,
    output_dir="runs",
    strategy=BuiltinStrategySpec(
        kind="sma_cross", strategy_id="sma-5-20", fast_window=5, slow_window=20
    ),
)
outcome = run_backtest(spec)
assert verify_run(outcome).verified
```

The installed `qianxing` command exposes `doctor`, `validate-dataset`, `backtest`, `verify`, and `compare-runs` for the same SDK use cases. Research commands accept the versioned JSON document produced by the application contract.

Amounts and quantities use the engine's fixed-point `*_raw` units. Install optional integrations only when needed, for example `pip install 'qianxing[ccxt]'` or `pip install 'qianxing[a-share]'`.

See the [repository documentation](https://github.com/coeasy/qianxing) for strategy contracts, CLI usage, release artifacts, maturity evidence, and the supported operating boundaries.
