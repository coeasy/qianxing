# Qianxing Python SDK

`qianxing` is the Python entry point for the Qianxing quantitative research and trading platform. The wheel bundles a Rust native extension; using the installed SDK does not require a Rust toolchain.

The extension uses PyO3's Python 3.10 stable ABI. Release builds publish one `cp310-abi3` wheel per supported OS and CPU architecture, usable on CPython 3.10 and newer; native platform binaries still require separate Windows, macOS, and Linux wheels.

## Current SDK surface

The public `qianxing` namespace exposes the shared Rust application use cases for local Bar dataset validation, deterministic Bar backtests, L1 Tick and L2 OrderBook backtests, two-leg Bar backtests, artifact verification, deterministic comparison of completed runs, bounded Bar parameter-grid experiments, and cooperative `RunHandle` lifecycle for Bar/Tick/OrderBook/multi-leg/parameter runs. Multi-leg requests can declare both normalized market specs, shared default maker/taker fees with per-leg overrides, funding basis points, and the shared fill model. Matching, risk, accounting, replay verification, cancellation boundaries, and experiment execution stay in Rust; Python only provides typed request/result objects.

`PaperClient` and `LiveClient` submit the versioned Rust `TradeControlSpec` to a running qx-api service. Rust authenticates the operator, validates the command in qx-app, persists and enqueues it through the existing ControlPlane, then the configured Paper or venue worker executes it. Live requires an authenticated API policy; the Python SDK does not execute trades itself. A client TLS context can be supplied for mTLS deployments.

The application SDK accepts normalized `TradingInstrumentSpec` documents; it does not currently import CCXT market snapshots or model per-venue maker/taker fee schedules. `one_tick_slippage` requires a real `price_tick` in both market specs. The depth application contract accepts Rust built-in strategies; non-zero queue-position assumptions are rejected because those strategies issue market orders.

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

Longer runs can use the same Rust engine with a cooperative handle:

```python
from qianxing import start_backtest

handle = start_backtest(spec)
if handle.wait(timeout_ms=30_000) == "running":
    handle.cancel()
state = handle.wait()
if state == "succeeded":
    outcome = handle.result()
```

Paper and Live command submission use the Rust server's existing execution path:

```python
from qianxing import LiveClient, TradeCommand

client = LiveClient(
    "https://127.0.0.1:8443",
    headers={"Authorization": "Bearer <deployment-issued-token>"},
)
receipt = client.submit(TradeCommand(
    command_id=1001,
    request_id="strategy-pause-1001",
    operator_id="operator-1",
    reason="operator maintenance",
    kind="PauseStrategy",
    target="strategy-1",
    payload={},
))
```

`headers` are forwarded to the deployment's HTTP authentication adapter, which must resolve them to an authenticated operator. The `operator_id` in the command is intent metadata and never grants authority by itself. Use `ssl_context` when the deployment requires mutual TLS.

The installed `qianxing` command exposes `doctor`, `validate-dataset`, `backtest`, `depth-backtest`, `verify`, `compare-runs`, and `run-experiment` for the same SDK use cases. Research commands accept the versioned JSON document produced by the application contract.

Amounts and quantities use the engine's fixed-point `*_raw` units. Install optional integrations only when needed, for example `pip install 'qianxing[ccxt]'` or `pip install 'qianxing[a-share]'`.

See the [repository documentation](https://github.com/coeasy/qianxing) for strategy contracts, CLI usage, release artifacts, maturity evidence, and the supported operating boundaries.
