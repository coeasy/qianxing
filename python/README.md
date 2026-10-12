# Qianxing Python SDK

`qianxing` is the Python entry point for the Qianxing quantitative research and trading platform. The wheel bundles a Rust native extension; using the installed SDK does not require a Rust toolchain.

The extension uses PyO3's Python 3.10 stable ABI. Release builds publish one `cp310-abi3` wheel per supported OS and CPU architecture, usable on CPython 3.10 and newer; native platform binaries still require separate Windows, macOS, and Linux wheels.

## Current SDK surface

The public `qianxing` namespace exposes the shared Rust application use cases for local Bar dataset validation, deterministic Bar backtests, L1 Tick and L2 OrderBook backtests, two-leg Bar backtests, artifact verification, deterministic comparison of completed runs, bounded Bar and multi-leg parameter-grid experiments, and cooperative `RunHandle` lifecycle for Bar/Tick/OrderBook/multi-leg/experiment runs. Local handles expose sequenced Rust worker lifecycle events through `events(after_seq=0)`; these currently cover run start, cancellation request and terminal state, not durable order/fill streams. L2 requests can select the built-in double-sided market-making path with tick-aligned quotes, inventory skew, bounded quote size, and cancel/replace routed through the same matcher, risk checks, OMS, ledger, and replay verifier. Multi-leg requests can declare both normalized market specs, shared default maker/taker fees with per-leg overrides, funding basis points, and the shared fill model. Matching, risk, accounting, replay verification, cancellation boundaries, and experiment execution stay in Rust; Python only provides typed request/result objects.

`AppClient` exposes the same use cases over a running qx-api service, including remote start/status/cancel/wait/result for every supported run kind. Its `RemoteRunHandle.wait()` uses a finite timeout by default. The service stores run snapshots below its configured artifact root: completed results survive API restarts, while jobs interrupted by a process exit recover as failed with an explicit error because worker threads cannot resume across processes.

`PaperClient` and `LiveClient` submit the versioned Rust `TradeControlSpec` to a running qx-api service. Rust authenticates the operator, validates the command in qx-app, persists and enqueues it through the existing ControlPlane, then the configured Paper or venue worker executes it. Live requires an authenticated API policy; the Python SDK does not execute trades itself. A client TLS context can be supplied for mTLS deployments.

The application SDK accepts normalized `TradingInstrumentSpec` documents for multi-leg Bar and Tick/OrderBook runs; depth runs check the contract identity and validity interval against the input and require an explicit `OrderPolicy` for derivatives. A Bar `BacktestSpec` with a market spec must also include an explicit `MakerTakerFeeSchedule`, which is consumed by the shared Rust fee model and recorded in run identity and summary. CCXT market snapshots are not imported automatically, and time-tiered/rebate/tax schedules are not modeled. `one_tick_slippage` requires a real `price_tick` in both multi-leg market specs. The L2 market-making strategy checks its configured tick size against the market spec when present. Queue-position simulation is rejected by the current built-in strategy facade, as are reduce-only and post-only policies; post-only fill behavior is not yet modeled by the shared order-book matcher.

## Rust-backed Python data providers

`qianxing.data.ProviderRegistry` accepts Python adapters while Rust retains deterministic provider selection, failover, result lineage hashing, and Bar/Quote payload validation. An adapter implements `fetch(query_json: str) -> result_json: str`; `ProviderResponse.to_json()` asks the Rust extension to build the raw-record and result fingerprints, so adapters do not implement the hashing algorithm. Raise `ProviderAdapterError` for retryable source failures, `ProviderManualInterventionError` for failures that must stop failover, and `ProviderPermanentError` for invalid or unsupported requests.

For a single-instrument strict Bar query, `registry.fetch_bar_frame(query)` returns the shared Rust BarFrame as a dictionary. `registry.run_backtest(spec, query)` freezes that canonical frame at `spec.bars_path` and executes the existing Rust backtest use case. When dataset identity is required, `fetch_bar_dataset(query, dataset_id, dataset_version, interval_ms, timezone)` also returns Rust-generated `DatasetManifestV2`; `run_backtest_with_manifest(...)` freezes the frame and manifest, validates the dataset through Rust, then runs the same Rust backtest. The interval is explicit so missing-bar counts are not guessed from a frequency label. For Quote queries, each payload must be a versioned `OrderBookSnapshot` JSON row with matching event timestamp and instrument; Rust assembles and validates the L1/L2 `DepthFrame`, and `registry.run_depth_backtest(spec, query, tier)` freezes it before invoking the shared Rust Tick/OrderBook use case. All workflows reuse identical frozen files and reject different content at those paths. This adapter surface does not certify an external source and does not yet provide streaming subscriptions, paging, or incremental order-book delta merging.

```python
from qianxing.data import (
    BAR_JSON_CONTENT_TYPE,
    ProviderCapability,
    ProviderPayloadRow,
    ProviderQuery,
    ProviderRegistry,
    ProviderResponse,
)

class LocalBars:
    def fetch(self, query_json: str) -> str:
        # Decode query_json and retrieve the requested rows from your data source.
        rows = (ProviderPayloadRow(1, 1, "BTCUSDT.BINANCE", BAR_JSON_CONTENT_TYPE,
                                  '{"ts":1,"open":100,"high":101,"low":99,"close":100,"volume":5}'),)
        return ProviderResponse("local-bars", "v1", "request-1", 1, rows).to_json()

registry = ProviderRegistry()
registry.register(ProviderCapability(
    provider_id="local-bars", version="v1", data_kinds=("Bar",),
    asset_classes=("crypto",), frequencies=("1m",), auth_scope="local",
    rate_limit_per_second=10, freshness_seconds=60, historical_start=0,
    historical_end=2**63, realtime=False, priority=0, quality_score=100,
    cost_score=0,
), LocalBars())
```

For deterministic L2 market-making research, add a `MarketMakingSpec` to `DepthBacktestSpec`; all parameters are fixed-point raw values and are included in the run configuration hash:

```python
from qianxing import BuiltinStrategySpec, DepthBacktestSpec, MarketMakingSpec, run_depth_backtest

spec = DepthBacktestSpec(
    run_id="maker-l2-001",
    depth_path="books.json",
    tier="l2",
    settlement_currency="USDT",
    initial_cash_raw=100_000_000_000_000,
    output_dir="runs/maker-l2-001",
    strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="maker-l2"),
    market_making=MarketMakingSpec(
        spread_bps=50,
        quote_qty_raw=1_000_000_000,
        max_inventory_raw=5_000_000_000,
        tick_size_raw=10_000_000,
        inventory_skew_bps=25,
    ),
)
outcome = run_depth_backtest(spec)
```

## Example

Query the built-in catalog from Rust, then select a single-instrument strategy without maintaining a second Python list. The catalog marks strategies that require a reference leg so callers can route those to a multi-leg workflow instead of a single-Bar run:

```python
from qianxing.core import list_builtin_strategies

for strategy in list_builtin_strategies():
    print(strategy.name, strategy.description, strategy.signal_knobs)

selected = next(item for item in list_builtin_strategies() if item.name == "ema_cross")
if selected.requires_reference_leg:
    raise ValueError("this strategy requires the multi-leg backtest workflow")
```

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

For server-managed runs, use the same typed spec with `AppClient`; the API executes the exact same Rust use case and the returned remote handle polls the durable run registry:

```python
from qianxing import AppClient

client = AppClient("https://localhost:8443", headers={"Authorization": "Bearer …"})
remote = client.start_backtest(spec)
state = remote.wait(timeout=300)
if state == "succeeded":
    outcome = remote.result()
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
import ssl
from qianxing import LiveClient, TradeCommand

tls = ssl.create_default_context(cafile="api-ca.pem")
tls.load_cert_chain("operator.crt", "operator.key")
client = LiveClient(
    "https://127.0.0.1:8443",
    ssl_context=tls,
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

`ssl_context` supplies the client certificate for the built-in mTLS identity mapping. `headers` are available for deployments with an HTTP authentication adapter. The `operator_id` in the command is intent metadata and never grants authority by itself.

The installed `qianxing` command exposes `doctor`, `validate-dataset`, `backtest`, `depth-backtest`, `verify`, `compare-runs`, `run-experiment`, `run-depth-experiment`, and `run-multi-leg-experiment` for the same Rust-backed SDK use cases. Research commands accept the versioned JSON document produced by the application contract.

Amounts and quantities use the engine's fixed-point `*_raw` units. Install optional integrations only when needed, for example `pip install 'qianxing[ccxt]'` or `pip install 'qianxing[a-share]'`.

See the [repository documentation](https://github.com/coeasy/qianxing) for strategy contracts, CLI usage, release artifacts, maturity evidence, and the supported operating boundaries.
