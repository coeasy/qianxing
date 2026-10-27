# Qianxing Python SDK

`qianxing` is the Python entry point for the Qianxing quantitative research and trading platform. The wheel bundles a Rust native extension; using the installed SDK does not require a Rust toolchain.

The extension uses PyO3's Python 3.10 stable ABI. Release builds publish one `cp310-abi3` wheel per supported OS and CPU architecture, usable on CPython 3.10 and newer; native platform binaries still require separate Windows, macOS, and Linux wheels.

## Current SDK surface

The public `qianxing` namespace exposes the shared Rust application use cases for local Bar dataset validation, deterministic Bar backtests, L1 Tick and L2 OrderBook backtests, two-leg Bar backtests, independent multi-account Bar/Depth batches, shared-account multi-market Bar portfolios, artifact verification, deterministic comparison of completed runs, and bounded Bar, depth, multi-leg, and multi-market parameter-grid experiments. Multi-market experiments vary one identified strategy slot while preserving the other slots and shared account; candidates reuse Rust risk, OMS, ledger, event, replay, and performance paths, have bounded grid sizes, stable ranking, per-candidate artifacts, and synchronous or cancellable `RunHandle` entrypoints. Python-authored strategy callbacks are supported for Bar, Tick/OrderBook, and two-leg/multi-market Bar workflows; experiment factories receive each resolved candidate spec and return a fresh strategy object. Local handles expose sequenced Rust worker lifecycle events through `events(after_seq=0)`; these currently cover run start, cancellation request and terminal state, not durable order/fill streams. L2 requests can select the built-in double-sided market-making path with tick-aligned quotes, inventory skew, bounded quote size, and cancel/replace routed through the same matcher, risk checks, OMS, ledger, and replay verifier. Multi-leg requests can declare both normalized market specs, shared default maker/taker fees with per-leg overrides, funding basis points, and the shared fill model. Matching, risk, accounting, replay verification, cancellation boundaries, and experiment execution stay in Rust; Python only supplies typed request/result objects and strategy callbacks.

`list_builtin_strategies()` reads the Rust-owned catalog in local mode; `AppClient.list_builtin_strategies()` and its async counterpart fetch the same catalog from `/app/strategies` for remote strategy selection.

For same-account Bar and Tick/OrderBook portfolios, `run_backtest_with_strategy_slots` / `start_backtest_with_strategy_slots` and `run_depth_backtest_with_strategy_slots` / `start_depth_backtest_with_strategy_slots` accept multiple identified Rust/Python callbacks in declared portfolio slots; any remaining slots use built-in Rust strategies. All slots share one Rust ledger, deterministic scheduler, risk checks, OMS, matching, fill attribution, and replay verification. Each callback must use the default `BuiltinStrategySpec` parameter values because its own decisions replace that built-in signal. The ordered portfolio spec controls priority; strategy artifact IDs, versions and SHA-256 digests are sorted by slot ID and bound into the run identity. L1 callbacks receive Tick events; L2 callbacks receive OrderBook events. Remote callback serialization remains unsupported.

Portfolio budgets accept an optional exact decimal alongside the legacy raw value. Encode exact values as `{"coefficient": "12", "scale": 10}` for `1.2e-9`; set the matching raw field to `1`, its toward-zero projection at the existing 1e-9 scale. `PortfolioStrategySpec.reserved_capital_exact` applies to child strategies, and `PortfolioBacktestConfig.primary_reserved_capital_exact` applies to the primary strategy. Omitting the exact field preserves the raw-only behavior. Rust validates the projection and uses the exact amount for allocation totals and order/fill budget checks.

For local two-leg parameter experiments, `run_multi_leg_experiment_with_strategy` and `start_multi_leg_experiment_with_strategy` accept a `strategy_factory(candidate_spec_dict)`. Rust invokes it once per candidate, and each returned strategy instance receives that candidate's parameterized spec. The strategy artifact ID, version and source SHA-256 are part of the experiment fingerprint and each candidate run identity. This local callback cannot be sent through the HTTP API; remote experiments continue to use the built-in strategy selection in the serialized spec.

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

The optional `qianxing_ccxt` package can register a configured CCXT historical client directly into this same Rust registry. It adapts OHLCV into versioned Bar payloads; provider choice, failover, lineage, frame validation, manifest generation, and backtesting remain on the shared Rust path:

```python
from qianxing_ccxt import register_ccxt_provider

# `client` is an already configured qianxing_ccxt.CcxtExchangeClient;
# `query` is the ProviderQuery requested by the backtest.
register_ccxt_provider(registry, binance_capability, client)
bars = registry.fetch_bar_frame(query)
```

This adapter is historical only: it rejects realtime queries and limits each response to 100,000 bars. CCXT OHLCV history does not provide original receive-time evidence, so the adapter records the local fetch time; Rust's `as_of` validation consequently prevents old point-in-time requests from being represented as if the exchange had delivered the history then. Configure one capability and client per exchange to use Rust's deterministic provider ordering and failover. This does not certify live market data or execution against a real venue.

`ProviderQuery` 也可声明来源授权范围与实时数据要求。`as_of`、Provider 行情时间戳和 `max_staleness_ms` 使用同一毫秒时钟；Rust 注册表会按 `auth_scope` 过滤候选、拒绝不支持实时流的来源，并将请求新鲜度上限与 Provider 的 `freshness_seconds` 取更严格值。旧查询默认保持历史数据语义：

```python
live_query = ProviderQuery(
    kind="Quote", asset_class="crypto", frequency="tick",
    start=1_000, end=1_000, as_of=1_000,
    instrument_set=("BTCUSDT.BINANCE",),
    required_auth_scope="account:read", require_realtime=True,
    max_staleness_ms=250,
)
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

For starting capital that needs more than 9 decimal places, use the versioned scale-aware form.
The coefficient is decimal text, so JSON and JavaScript never round an `i128`; `initial_cash_raw`
must be its toward-zero 1e-9 projection and is kept for older readers:

```python
spec = BacktestSpec(
    run_id="exact-capital-001",
    instrument="BTCUSDT.BINANCE",
    bars_path="deploy/qianxing.bar-frame.example.json",
    settlement_currency="USDT",
    initial_cash_raw=100_000_000_000_000,
    initial_cash_exact={"coefficient": "1000000000000001", "scale": 10},
    schema_version=2,
    output_dir="runs",
    strategy=BuiltinStrategySpec(
        kind="sma_cross", strategy_id="exact-capital-sma", fast_window=5, slow_window=20
    ),
)
outcome = run_backtest(spec)
```

The run ledger stores the exact deposit for deterministic replay. Summary `metrics_exact.equity`
reports exact initial/final equity, return and drawdown; `initial_cash_raw`, `final_equity_raw`,
and legacy basis-point fields remain compatibility projections. Schema v1 continues to accept only
`initial_cash_raw`; use [`backtest-spec-v2`](../schemas/backtest-spec-v2.json) for this form.

Run a quick, bounded comparison across the classic single-instrument strategies with the same
frozen BarFrame. Strategy candidates are canonically ordered by kind and ranking execute in Rust;
`strategy_kinds` is optional and keeps old experiment JSON unchanged when omitted. Two-leg arbitrage kinds must use the multi-leg
experiment contract. When a `split_plan` is present, Rust selects both the strategy kind and its
numeric parameters using only the training window, then reports the selected kind and evaluates it
on the held-out test window. Walk-forward folds repeat this selection independently.

```python
from qianxing import ExperimentParameterSpace, RunExperimentSpec, list_builtin_strategies, run_experiment

single_leg_kinds = tuple(
    item.name for item in list_builtin_strategies() if not item.requires_reference_leg
)
selection = RunExperimentSpec(
    experiment_id="classic-strategy-scan-001",
    base=spec,
    parameter_space=(),
    strategy_kinds=single_leg_kinds,
)
result = run_experiment(selection)
for candidate in result.candidates:
    print(candidate.strategy_kind, candidate.outcome.return_bps if candidate.outcome else candidate.error)
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

The installed `qianxing` command exposes `doctor`, `validate-dataset`, `backtest`, `depth-backtest`, `verify`, `compare-runs`, `run-strategy`, `search-strategy`, `inspect-strategy`, `diagnose-strategy`, `run-experiment`, `run-depth-experiment`, `run-multi-leg-experiment`, and `research` for local Rust-backed SDK use cases. The optional `remote-research` command submits a batch to E2B (the default), Microsandbox, or OpenSandbox; research commands accept the versioned JSON document produced by the application contract.

### Run or search a strategy from a Python file

Bar strategy files may implement the standard event hook `on_bar(context, bar)` or the Backtrader-style `prenext(context, bar)`, `nextstart(context, bar)`, and `next(context, bar)` lifecycle. Export `STRATEGY_CLASS` or `create_strategy(spec)` and declare `STRATEGY_MANIFEST`, then use the local `qianxing run-strategy` or `qianxing research` commands below. With the classic hooks, `history_requirements()` or the manifest sets warmup: `prenext` updates state while decisions are suppressed, `nextstart` runs once on the first eligible Bar and defaults to `next`, and later eligible Bars call `next`. Existing classes that override `on_bar` or `on_event` keep their current callback path. Local execution needs no sandbox SDK or server credentials.

The older module-function form (`on_event(request)` / `on_decision(request)`) remains convenient for local backtests and research. A persistent `strategy-worker` also needs a restart-safe state contract: export both `snapshot_state() -> dict` and `restore_state(state)`, or set `STRATEGY_STATELESS = True` when each decision depends only on its request. Stateful snapshots must be string-keyed JSON objects with finite values; the complete checkpoint is limited to 64 KiB. Do not store credentials in the snapshot. The worker sends an initial bootstrap checkpoint and only advances the durable event cursor together with the returned checkpoint. An undeclared module function is rejected before its first persistent callback, while one-shot local backtest/research continues to accept it.

Install the wheel once, then pass the strategy file and a versioned spec directly to the command. The file exports `STRATEGY_CLASS` (a `BaseStrategy` subclass) or `create_strategy(spec: dict)` and returns a fresh strategy instance. A class constructor may accept the complete run spec or no arguments; each run/search candidate gets a new instance. Strategy callbacks make decisions; Rust still performs risk checks, orders, fills, accounting, and replay verification. The checked-in commands below run from the repository root, use strategy files under `python/examples/`, and resolve market data under `deploy/`.

```console
pip install qianxing
qianxing doctor
qianxing run-strategy ./my_strategy.py ./backtest-spec.json
qianxing run-strategy ./python/examples/strategy_file_tick.py ./python/examples/strategy-file-tick-backtest.example.json
qianxing run-strategy ./my_strategy.py ./multi-market-backtest-spec.json
qianxing run-strategy ./my_strategy.py ./multi-market-backtest-spec.json --strategy-id my-okx-slot
qianxing run-strategy ./python/examples/strategy_file_sma.py ./python/examples/strategy-file-multi-market-backtest.example.json
qianxing run-strategy ./python/examples/strategy_file_nextstyle.py ./python/examples/strategy-file-nextstyle-backtest.example.json
qianxing inspect-strategy ./python/examples/strategy_file_sma.py --out ./runs/file-sma-manifest.json
qianxing search-strategy ./my_strategy.py ./run-experiment-spec.json --summary
qianxing search-strategy ./python/examples/strategy_file_tick.py ./python/examples/strategy-file-tick-search.example.json --summary
qianxing search-strategy ./my_strategy.py ./run-experiment-spec.json --summary --html
qianxing search-strategy ./my_strategy.py ./run-experiment-spec.json --html ./reports/research.html
qianxing research ./my_strategy.py ./run-experiment-spec.json
qianxing research ./my_strategy.py ./run-experiment-spec.json --diagnostic-candidate-limit 16 --html ./reports/research.html
qianxing diagnose-strategy ./my_strategy.py ./run-experiment-spec.json --startup-windows 20,50,100 --out ./reports/strategy-diagnostics.json
qianxing diagnose-strategy ./my_strategy.py ./multi-market-backtest-spec.json --strategy-id my-okx-slot --startup-windows 20,50,100 --out ./reports/multi-market-diagnostics.json
qianxing search-strategy ./my_strategy.py ./run-experiment-spec.json --summary --html ./reports/research.html --diagnostics ./reports/strategy-diagnostics.json
qianxing search-strategy ./python/examples/strategy_file_sma.py ./python/examples/strategy-file-multi-market-search.example.json --summary
qianxing research ./python/examples/strategy_file_sma.py ./python/examples/strategy-file-search.example.json
```

`search-strategy` reuses the bounded Rust experiment runners. For single-market Bar experiments, its default output is the complete versioned report; `--summary` shows ranked candidates, baselines, cost sensitivity, parameter-neighborhood assessment, and out-of-sample folds. Add a `split_plan` before treating a ranking as evidence of strategy effectiveness; without one, the command warns and reports only in-sample results. For multi-market experiments, use a `RunMultiMarketExperimentSpec` with `base.markets` and `target_strategy_id`. `parameter_space` searches the Rust engine's strategy dimensions (`fast_window`, `slow_window`, `period`, `threshold_bps`, and sizing fields); `callback_parameter_space` accepts arbitrary named integer parameters for the Python strategy factory. Add a `search` plan to use deterministic random, TPE, KDE-TPE, or Pareto-TPE proposals within a discrete grid or stepped integer ranges. Adaptive search requires `split_plan`; proposals and selection read training results only. Set `fold_local_search: true` to search independently on each walk-forward training fold; otherwise the first fold's candidate set is evaluated on later folds. Candidate budgets are capped at 256 and the combined training/OOS execution count at 4,096. Early pruning, robustness subwindow objectives, and neighborhood reports are not yet supported by the multi-market adaptive report and are rejected explicitly. Each trial receives a fresh strategy object, and the report records proposal type, seed, training metrics, fold selections, and candidate failures.

`research` is the guided preset over `search-strategy`: it requires an explicit experiment spec, parameter space, and held-out `split_plan`. The local and remote CLI paths call `qx-app::prepare_quick_research_spec_v1` for the shared gate and normalization. For single-market `RunExperimentSpec`, it adds `buy-and-hold` when no effectiveness baseline is supplied. It then runs the Rust search and OOS evaluation, future-data/warmup diagnostics, compact ranking, and interactive HTML report. Candidate diagnostics default to the first 16 materialized candidates; remaining candidates stay explicitly unverified, and `--diagnostic-candidate-limit` sets another bound. `STRATEGY_CLASS` diagnostics replay the standard class lifecycle, including `history_requirements()` and `prenext/nextstart/next`, using Bars visible at each point-in-time boundary. Callback and warmup-declaration failures notify `on_error_v1` with the stage when available, while preserving the original failure. `RunMultiMarketExperimentSpec` uses its strict schema and OOS folds but does not yet provide a cross-market benchmark, so its ranking is not a benchmark-relative effectiveness result. This shortcut cannot present an in-sample-only ranking as a complete effectiveness review. Use `search-strategy` when you need an in-sample ranking, the full result JSON, or a separately generated base-spec diagnostic.

`inspect-strategy` imports the file but does not call `create_strategy`; `STRATEGY_MANIFEST` declares `schema_version`, strategy ID/version, typed parameter ranges, instrument requirements, data level, warmup bars, and local dependencies. The command emits the normalized [`python-strategy-manifest-v1`](../schemas/python-strategy-manifest-v1.json) with both entry-file and strategy-bundle SHA-256 identities. A complete Bar manifest is checked against the selected market slot: it checks `asset_class`, product/venue requirements, requires `market_spec` when declared, and reads usable row counts through Rust dataset validation. A complete depth manifest must match `tick` to `tier=l1` or `orderbook` to `tier=l2`; product and venue requirements are checked against `market_spec`, while the Rust depth engine parses and validates the actual frame during the run. `DepthBacktestSpec` does not currently declare `asset_class`, so depth strategies must leave `asset_classes` empty. A single custom depth run must leave `BuiltinStrategySpec` knobs at their core defaults; `RunDepthExperimentSpec` can vary the declared numeric knobs because the fresh Python factory consumes each candidate. `warmup_bars` counts Bars only: for Tick/OrderBook strategies set it to zero and maintain event-count warmup in the strategy itself. `search-strategy` accepts `RunDepthExperimentSpec` and creates a fresh strategy object for every bounded candidate; that result is an in-sample ranking because depth experiment specs do not yet include train/OOS splits. Interactive depth reports and future-data candidate diagnostics remain unavailable.

Declared parameter defaults are copied into the selected strategy configuration before preflight and every candidate factory call. A module may export `validate_config(config)`: it receives a read-only mapping of resolved declared parameters, must return `None`, and can reject invalid combinations by raising an exception; it must remain deterministic and must not read market data, account state, or exchange services. The validator runs before a single-run data preflight and before each search candidate factory call. A Bar run needs at least `warmup_bars + 1` validated Bars; search also needs at least that many training Bars. At runtime the manifest warmup is bound to each fresh Bar callback instance: the first `warmup_bars - 1` Bars update strategy state while their order intents are suppressed. When the Nth Bar becomes visible, `on_warmup_complete(context, instrument, observed_bars)` runs before that Bar hook, allowing its decision to use the required history; resulting orders can fill no earlier than the following Bar. A strategy declaration that conflicts with the manifest fails before execution. A strategy without `STRATEGY_MANIFEST` remains `completeness=partial` and preserves legacy behavior; code hashes still bind its source, but unknown parameter or warmup requirements are not inferred.

### Run research in a remote sandbox

Local `run-strategy`, `search-strategy` and `research` remain direct local execution. `remote-research` uses the same explicit OOS split and 16-candidate diagnostic cap as guided local `research`; it adds `buy-and-hold` for single-market specs when omitted, defaults to E2B, and also supports explicit Microsandbox and OpenSandbox selection. Microsandbox defaults to its local microVM backend; `--backend cloud` is a separate explicit choice. Multi-market research does not currently calculate a cross-market baseline. Remote research never switches providers or falls back to local execution automatically. Build the chosen template/image with the same Qianxing version and every pinned third-party strategy dependency already installed. Remote providers transfer only the strategy's statically discovered `.py` imports, declared `.py` `STRATEGY_DEPENDENCIES`, the research JSON and referenced input data, then return bounded artifacts. OpenSandbox creation explicitly requests deny-all network egress; a server that cannot enforce that policy fails the job. Non-Python strategy assets must be referenced as research input files. Do not put exchange credentials in strategy source or research input; provider API keys stay on the submitting host. This command is for research jobs, not a live strategy event stream.

```console
pip install "qianxing[sandbox-e2b]"
```

For a self-hosted OpenSandbox service, install its optional SDK separately:

```console
pip install "qianxing[sandbox-opensandbox]"
```

For a Microsandbox batch or persistent worker, install the optional SDK extra:

```console
pip install "qianxing[sandbox-microsandbox]"
```

Build a template once from the same release version as the local SDK:

```python
from e2b import Template

template = Template().from_python_image("3.12").run_cmd(
    "pip install qianxing==0.3.0"
)
Template.build(template, name="qianxing-0-2-2", cpu_count=2, memory_mb=2048)
```

Then submit an experiment. The E2B API key is read from `E2B_API_KEY`; choose the prebuilt template with `QX_E2B_TEMPLATE` or `--template`. The host must set `QX_E2B_EXPECTED_CPU_COUNT` and `QX_E2B_EXPECTED_MEMORY_MB` to the template's actual resources; the command reads the created sandbox's resource info and fails before uploading strategy files if the values are missing, unreadable, or different. E2B is the default provider.

```powershell
$env:E2B_API_KEY = "<your E2B API key>"
$env:QX_E2B_TEMPLATE = "qianxing-0-3-0"
$env:QX_E2B_EXPECTED_CPU_COUNT = "2"
$env:QX_E2B_EXPECTED_MEMORY_MB = "2048"
qianxing remote-research ./my_strategy.py ./run-experiment-spec.json --output-dir ./runs/remote-001
```

For a self-hosted Microsandbox local microVM, install its optional extra and pin a ready image:

```powershell
pip install "qianxing[sandbox-microsandbox]"
$env:QX_MICROSANDBOX_IMAGE = "registry.example.internal/qianxing@sha256:<64-hex-digest>"
qianxing remote-research ./my_strategy.py ./run-experiment-spec.json --provider microsandbox --backend local --image $env:QX_MICROSANDBOX_IMAGE --output-dir ./runs/remote-ms-001
```

On Windows, Microsandbox's local runtime currently requires Windows 11 x64/ARM64 with Windows Hypervisor Platform (WHP) enabled; upstream describes Windows support as preview. Confirm hardware virtualization is enabled in firmware, then enable WHP from an elevated PowerShell and restart Windows when prompted:

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All -NoRestart
Restart-Computer
```

After restart, verify the host with `msb doctor`, then run a no-network local microVM smoke using a Python OCI image:

```powershell
python tools/microsandbox_local_smoke.py --image python:3.12-slim
```

The smoke prints `QIANXING_MICROSANDBOX_LOCAL_OK` only after Python has run inside the microVM. Pulling the image requires host network access; the guest itself is created with network access disabled. The smoke validates the Microsandbox host runtime, while a Qianxing strategy worker additionally needs the digest-pinned image configured above with the matching Qianxing package and strategy dependencies. See the [Microsandbox Python SDK requirements](https://github.com/superradcompany/microsandbox/blob/main/sdk/python/README.md).

For OpenSandbox, provide its service domain, an image containing the matching Qianxing wheel and pinned strategy dependencies, and optionally its API key:

```powershell
$env:OPEN_SANDBOX_DOMAIN = "sandbox.example.internal:8080"
$env:OPEN_SANDBOX_API_KEY = "<service API key>"
$env:QX_OPENSANDBOX_IMAGE = "registry.example.internal/qianxing@sha256:<64-hex-digest>"
$env:QX_OPENSANDBOX_CPUS = "2"
$env:QX_OPENSANDBOX_MEMORY_MIB = "2048"
qianxing remote-research ./my_strategy.py ./run-experiment-spec.json --provider opensandbox --cpus 2 --memory-mib 2048 --output-dir ./runs/remote-osb-001
```

The command requires a new local output directory. Input bundles are limited to 128 MiB; returned archives are limited to 128 MiB compressed, 256 MiB expanded and 20,000 files. A timeout, provider error or nonzero research result fails closed and never falls back to another provider or local strategy execution. Each remote-research job writes a host-side audit JSONL under `~/.qianxing/strategy-sandbox-audit/`; set `QX_REMOTE_RESEARCH_AUDIT_DIR` to move it. The result JSON includes the local `audit_log` path. The path and audit contents are never uploaded to the sandbox. E2B records observed resources after matching them to the configured template policy; OpenSandbox records requested resource limits, which still need confirmation from the connected server. These offline tests do not constitute connected-service, capacity, isolation or billing acceptance.

### Run a persistent strategy worker in a remote sandbox

Local strategy work remains the default: `run-strategy`, `search-strategy`, `research`, and the Rust `strategy-worker` execute Python directly in the local process environment and do not require a sandbox SDK. For a multi-user server, use an isolated provider when strategies are untrusted or tenant isolation is required. Provider selection is a host deployment setting; a strategy cannot turn the sandbox off. E2B is the recommended first provider for a server strategy worker; set both `QX_PYTHON_STRATEGY_EXECUTION_MODE=e2b` and `QX_PYTHON_STRATEGY_SANDBOX_REQUIRED=true` on the worker host. The second setting makes a missing or `local` provider a startup error, while leaving it unset preserves the local development default. Microsandbox local microVM is an alternative for self-hosted installations; its VM runs on the same host as the Qianxing worker, so deploy the worker on the target server. Its current local platform documentation lists macOS, Linux, and Windows, but validate the pinned Python SDK version and target host's virtualization support before deployment; the Qianxing integration has not had connected microVM acceptance on those hosts. The local backend does not turn an arbitrary URL into a remote control plane. OpenSandbox is an explicit self-hosted service alternative. The bridge fails closed and does not retry locally or silently switch providers after a sandbox failure. The separate `remote-research` batch command also defaults to E2B and supports explicit Microsandbox and OpenSandbox selection. These provider integrations have not received connected-service isolation or capacity acceptance.

Persistent Rust-hosted strategy execution creates a fresh host-owned JSONL audit path under `storage.data_dir/strategy-sandbox-audit/`; the separate `remote-research` command writes one under `~/.qianxing/strategy-sandbox-audit/` by default. Both use the same [`python-strategy-sandbox-audit-v1`](../schemas/python-strategy-sandbox-audit-v1.json) records for create/ready/failure/cleanup and completed strategy requests. Records bind the host worker/job and strategy identity, source/transfer bundle/spec digests, package version, data fingerprint, response digest, runtime reference, resource limits, network policy and lifecycle state; provider keys and strategy payload contents are excluded. The per-file SHA-256 chain detects edits within the recorded sequence, but is not a signature or a tamper-proof audit store. Remote research fails before provider creation if it cannot write the first audit record. Provider-accepted resource limits and isolation still require connected-service verification.

Before an E2B, Microsandbox, or OpenSandbox session reaches its renewal window, the next strategy request can rotate to a fresh session only when that request carries the last host-persisted checkpoint. This works for `BaseStrategy` and module functions that implement the state contract above. Missing checkpoints fail closed; no stateful module is silently restarted from its initial memory.

For E2B, install the optional SDK on the worker host and build the selected template with the exact Qianxing version and every third-party Python dependency imported by the strategy:

```powershell
pip install "qianxing[sandbox-e2b]"
$env:QX_PYTHON_STRATEGY_EXECUTION_MODE = "e2b"
$env:QX_PYTHON_STRATEGY_SANDBOX_REQUIRED = "true"
$env:QX_E2B_TEMPLATE = "qianxing-0-2-2"
$env:QX_E2B_EXPECTED_CPU_COUNT = "2"
$env:QX_E2B_EXPECTED_MEMORY_MB = "2048"
$env:E2B_API_KEY = "<host-side E2B API key>"
$env:QX_E2B_SESSION_TTL_SECONDS = "3600" # at least strategy timeout + 30s; maximum 86400s
qx-cli strategy-worker .\runtime.json <worker-id>
```

Build the E2B template with the same CPU and memory values. E2B allocates these resources when the template is built; the worker checks the created sandbox's reported CPU and memory before uploading or starting strategy code, and stops it if either value is missing or differs from the host policy. Choose these values from measured strategy workload and concurrency requirements; the values above are only a small development example, not a production capacity recommendation.

For OpenSandbox, install its optional SDK and point the host bridge at an existing service and prebuilt image with matching Qianxing and strategy dependencies:

```powershell
pip install "qianxing[sandbox-opensandbox]"
$env:QX_PYTHON_STRATEGY_EXECUTION_MODE = "opensandbox"
$env:QX_PYTHON_STRATEGY_SANDBOX_REQUIRED = "true"
$env:OPEN_SANDBOX_DOMAIN = "sandbox.example.internal:8080"
$env:OPEN_SANDBOX_API_KEY = "<optional host-side service key>"
$env:QX_OPENSANDBOX_IMAGE = "registry.example.internal/qianxing-runtime@sha256:<64-hex-digest>"
$env:QX_OPENSANDBOX_CPUS = "2"
$env:QX_OPENSANDBOX_MEMORY_MIB = "2048"
$env:QX_OPENSANDBOX_SESSION_TTL_SECONDS = "3600"
qx-cli strategy-worker .\runtime.json <worker-id>
```

Both the persistent strategy worker and `remote-research --provider opensandbox` send explicit OpenSandbox CPU and memory limits (`cpu` and `Mi` units). Defaults are 1 CPU and 2048 MiB; host settings or CLI flags may choose 1–64 CPU and 512–65536 MiB. The values are also attached as Qianxing metadata for operational inspection. The server must enforce the requested limits; local mock tests do not prove that a connected service applied them.

OpenSandbox and Microsandbox self-hosted worker images must use an immutable OCI reference of the form `repository@sha256:<64-hex-digest>`; mutable tags such as `:latest` are rejected before connecting to the provider. This pins the selected image content but does not attest who built or signed it.

Microsandbox worker resources are configured with `QX_MICROSANDBOX_CPUS` and `QX_MICROSANDBOX_MEMORY_MIB` (defaults: 1 CPU and 1024 MiB; supported bounds: 1–64 CPU and 512–65536 MiB). Those values are passed to microVM creation, and invalid values fail before a sandbox session starts.

For a self-hosted strategy server that prefers local microVM isolation, install the optional SDK and use a prebuilt image with the matching Qianxing release and strategy dependencies. The Microsandbox local backend is the default when Microsandbox execution is selected. Its current local platform documentation lists macOS, Linux, and Windows; confirm that the pinned SDK and target host support the required native hypervisor before deployment. This creates the isolation microVM on the strategy server and does not require a Cloud API key:

```bash
pip install "qianxing[sandbox-microsandbox]"
export QX_PYTHON_STRATEGY_EXECUTION_MODE=microsandbox
export QX_PYTHON_STRATEGY_SANDBOX_REQUIRED=true
export QX_MICROSANDBOX_BACKEND=local
export QX_MICROSANDBOX_IMAGE=registry.example.internal/qianxing-runtime@sha256:<64-hex-digest>
export QX_MICROSANDBOX_CPUS=2
export QX_MICROSANDBOX_MEMORY_MIB=2048
export QX_MICROSANDBOX_SESSION_TTL_SECONDS=3600
qx-cli strategy-worker ./runtime.json worker-01
```

Microsandbox Cloud is an explicit alternative: set `QX_MICROSANDBOX_BACKEND=cloud` and provide a host-side API key or configured profile. Cloud access is currently by request/private beta, and BYOC deployment is an evaluation path; confirm account access and operating limits before sizing production. `QX_MICROSANDBOX_URL` only selects a configured SDK endpoint; it does not turn an arbitrary server URL into a self-hosted Microsandbox control plane. Selecting `QX_PYTHON_STRATEGY_EXECUTION_MODE=microsandbox` is the opt-in to sandboxing; leaving execution mode unset still runs the trusted local strategy directly without a sandbox.

#### Remote Docker alternative: `llm-sandbox`

E2B remains the recommended first choice for hosted, multi-tenant strategy execution. If you operate a remote Docker host with mutual TLS, the optional `llm-sandbox` adapter can run a persistent Python strategy session there. This adapter is specifically configured for a remote Docker daemon; it does not turn the local default into a sandbox and does not provide a microVM boundary.

Install the optional adapter on the strategy worker host, configure the runtime strategy with `python_timeout_ms` of at least `15000`, then start the worker with:

```bash
pip install "qianxing[sandbox-llm]"
export QX_PYTHON_STRATEGY_EXECUTION_MODE=llm-sandbox
export QX_PYTHON_STRATEGY_SANDBOX_REQUIRED=true
export QX_LLMSANDBOX_DOCKER_HOST=tcp://docker.example.internal:2376
export QX_LLMSANDBOX_DOCKER_CA_CERT=/etc/qianxing/docker/ca.pem
export QX_LLMSANDBOX_DOCKER_CLIENT_CERT=/etc/qianxing/docker/client.pem
export QX_LLMSANDBOX_DOCKER_CLIENT_KEY=/etc/qianxing/docker/client-key.pem
export QX_LLMSANDBOX_IMAGE=registry.example.internal/qianxing-runtime@sha256:<64-hex-digest>
export QX_LLMSANDBOX_EXPECTED_CPU_COUNT=2
export QX_LLMSANDBOX_EXPECTED_MEMORY_MB=2048
export QX_LLMSANDBOX_SESSION_TTL_SECONDS=3600
qx-cli strategy-worker ./runtime.json worker-01
```

The image must contain the same `qianxing` package version as the host and IPython, plus any strategy dependencies. The remote container has no network. Before uploading strategy code, the adapter verifies the pinned digest's resolved image ID and checks Docker's applied CPU/memory/pid limits, deny-all network, no added Linux capabilities, non-root UID, read-only root filesystem, and bounded writable tmpfs mounts. TLS paths and the Docker endpoint remain in the host bridge; they are never placed in the strategy container. The worker fails closed if Docker inspection does not match the requested policy. Test against the actual remote Docker Engine before using it for uploaded strategies; for hostile multi-tenant code, prefer E2B or a microVM-backed deployment until that acceptance is complete.

All providers require a file-backed `.py` `python_module`, support `jsonl` and `framed_json`, and reject shared-memory transports. A remote sandbox is created lazily for the first event and runs one persistent strategy process so `BaseStrategy` lifecycle and in-memory state are not reset between callbacks. Runtime class strategies support `data_level="bar"`, `"tick"`, `"orderbook"`, and `"timer"`. OrderBook callbacks are replayed from the durable Runtime EventLog with checkpoint/cursor deduplication and exact price/quantity sidecars. Timer mode delivers one `on_timer(context, {"name": "scheduler", "ts": as_of})` in the first worker poll of each `strategy.live_timeframe` bucket. The time bucket is part of the idempotency fingerprint, and stale jobs from an older bucket are rejected. Timer mode is independent of Bar snapshots, but a configured snapshot remains available through the point-in-time `context["data"]` portal; `warmup_bars` must be zero. Class strategies restore their persisted checkpoint when a session rotates; module-level handlers have no durable state contract and stop before lease expiry rather than restart with lost state. Only strategy source, statically discovered Python imports, strategy configuration and supported local input files are transferred. The bundle and Qianxing package version are checked before strategy startup. Provider keys remain on the host; guest commands use a clean environment. All remote configurations request deny-all network egress, and strategies continue returning intents through the host Rust Risk/OMS/execution path. Credential-like strategy configuration fields, obvious inline source credentials, common credential files and private keys are rejected.

To use Timer mode, set both `strategy.python_strategy_spec.data_level` and the strategy manifest to `timer`, set `strategy.live_enabled=true`, and choose `strategy.live_timeframe` as the interval. `python/examples/strategy_file_timer.py` is a no-order Runtime starter; set `python_module` to that file and keep `warmup_bars=0`. A `bars_snapshot_path` is optional and only supplies point-in-time history to the callback.

Set `strategy.python_timeout_ms` in `runtime.json` to at least 5000 for E2B and Microsandbox, and 15000 for OpenSandbox and `llm-sandbox`, whose file, command, TLS Docker, and interactive-kernel round trips need a larger budget. The selected provider session TTL must be at least that timeout plus 30 seconds; maximum TTL is 86400 seconds. The Rust strategy job lease includes the request timeout and bounded sandbox bootstrap time, so a slow provider start does not let another worker claim the same queued run.

Keep provider settings on server worker hosts, not in strategy configuration. One sandbox is created per Python strategy worker, so high strategy counts need resource and cost sizing. Microsandbox requires a compatible microVM host for its local backend or an enabled Cloud account/endpoint for its cloud backend; OpenSandbox additionally requires support for sandbox creation, background commands, filesystem operations and the requested deny-all egress policy. The `llm-sandbox` path depends on the remote Docker Engine actually applying its TLS-authenticated network, resource, identity, tmpfs and read-only filesystem settings. The source integrations are not connected-provider acceptance: provider authentication, isolation enforcement, capacity, cost, session recovery and end-to-end order behavior still need deployment acceptance.

Add `--html` to write a self-contained offline HTML research view; when no path follows the flag, the report is written beside the result artifact as `*.report.html`. The report includes sortable/filterable candidate rows, a return/drawdown comparison chart, CSV export, saved walk-forward folds and search proposals, plus any baselines, cost sensitivity, neighborhood, and data-quality fields already present in the result. For up to 12 top-ranked candidates it loads the saved single-market run summary, RunManifest, equity CSV, and fills CSV only after matching run/data/result identities and checking each artifact digest from RunRecord; the report then exposes selectable raw fixed-point equity curves, account/instrument/model facts, fill fees, and a fills CSV download. When fills use schema v2, the table and download retain `qty_exact`, `price_exact`, and per-currency `exact_fees_json`; multi-market Fill coefficient/scale values are also projected to exact decimal strings after sidecar/event equality checks. A changed or missing referenced artifact stops report generation rather than silently displaying stale detail. Candidates beyond the cap and candidates without linked run files are counted in the report. Artifact loading is limited to 8 MiB per experiment and run, 65,536 equity points per run, and 20,000 fills per run. It does not recalculate trading returns or drawdown in the browser. CSV export escapes spreadsheet formula prefixes.

For multi-market Bar experiments, the HTML writer verifies each candidate's saved run artifact, Rust-validated BarFrame identities, candidate/outcome/RunManifest data identity equality and exact `input_components`, equity and fills sidecars, event fills, and per-instrument equity-contribution conservation before showing shared-account curves and attribution. It does not recompute the composed market fingerprint or independently replay the EventLog; use `qx-cli report` for Rust fingerprint and replay verification. The shared verified Bar/Depth core is available through `qianxing.reports.read_multi_account_report_v1`; interactive multi-account attribution, Tick/L2 report attachments, and cross-currency valuation remain unsupported. Single-market and multi-market Python callback diagnostics can be attached after checking strategy identity, original spec digest and Rust-revalidated data identities; for multi-market input this compares every instrument's Bar fingerprint and row count. A report without `split_plan` carries a visible in-sample-only warning.

`diagnose-strategy` accepts a single-market Bar `BacktestSpec`, its `RunExperimentSpec`, a `MultiMarketBacktestSpec`, or its `RunMultiMarketExperimentSpec` (experiment documents diagnose their `base` spec). For multi-market input, select the strategy slot with `--strategy-id` or `target_strategy_id`; all declared BarFrames are validated through Rust, with a 65,536-Bar aggregate diagnostic limit. Each sampled probe rebuilds the selected `create_strategy(spec)` callback in a fresh process, replays only that slot's visible target-market events, and applies the same future perturbation profile to every declared peer-market Bar file made available through the factory context. Startup windows truncate all peer files to the matching time interval. The diagnostic report records strategy bundle/spec/data identity and accepts an optional provider-neutral `--pit-ledger`; for multi-market runs, PIT checks currently use target-market decision times only. It fixes positions/cash/margin during diagnostic replays and does not simulate fills, order updates, other strategy-slot event streams, or account-state transitions; external file/network reads outside declared Bar files, hidden process state, and unsampled points remain outside coverage. `search-strategy --diagnostics` attaches separately generated single-market or multi-market JSON after matching strategy version, source bundle, original BacktestSpec digest, target instrument/slot and Rust-validated data fingerprint(s); that sidecar diagnoses the base spec only. Use `search-strategy --diagnose-candidates` or `research` to generate distinct diagnostics bound to each materialized search candidate and fold. A no-change result is not proof of no possible lookahead.

For the typed SDK, build arbitrary callback dimensions with `CallbackParameterSpace("risk_scale", (2, 4))` and pass them through `RunMultiMarketExperimentSpec.callback_parameter_space`. A direct strategy factory receives `(candidate, parameters)` for each candidate. Existing one-argument factories remain supported when using the engine's fixed `parameter_space` dimensions.

### Event hooks for SDK callback strategies

For callback-based SDK runs, `qianxing_strategy.BaseStrategy` is an optional typed helper. A strategy only needs to override the market hook it uses; `on_event` remains the wire entry point and dispatches Bar, Tick, order-book and timer inputs to `on_bar`, `on_tick`, `on_order_book` and `on_timer`.

The common optional hooks are `on_init(context)`, `history_requirements()`, `on_warmup_complete(context, instrument, observed_bars)`, `on_start(context)`, the market hook, `on_order(context, update)`, `on_trade(context, fill)`, `on_account(context, snapshot)`, `on_positions_snapshot(context, snapshot)`, `on_position(context, position)`, `on_error_v1(error: StrategyErrorV1)`, `diagnostic_state()` and `on_stop(context=None, reason="host_stop")`. Legacy `on_error(context, error)` and module-function `on_error(request, error)` hooks still receive the original Python exception; `on_error_v1` receives the shared Rust/C/C++ DTO with stage, strategy identity, timestamp, instrument and bounded message. Both observers run independently, and their failures do not replace the original callback error. `StrategyErrorV1` is exported from `qianxing_strategy`. Legacy `on_stop()` overrides remain supported. In local Rust-backed callbacks and the Python Runtime class worker, error observers run before a callback failure crosses the host boundary; the Runtime worker rolls strategy state back afterward. Rust defines a versioned stop context and the C ABI has an optional v2 JSON callback while retaining the v1 callback. Bar and Tick/L2 backtest engines pass `completed`, `cancelled`, or `failed` plus the final context through their Python callback adapter, native adapters, strategy slots, and portfolio wrappers; direct calls to legacy `_qx_on_stop()` still map to `host_stop`. A Runtime strategy is a persistent session across durable `JobRun` event batches: clean worker EOF delivers `shutdown`, a worker-level frame/stream failure delivers `failed`, and request decode/callback errors return per-request errors without ending the session (decoded callback failures invoke the error observers and roll state back). `on_stop` runs once when that session ends, not after every queue batch; `completed`/`cancelled` are finite backtest outcomes. Warmup is per instrument and host-managed: early Bars update strategy state while their intents are suppressed; the Bar that completes warmup can produce an eligible decision. `on_start` does not mean every instrument is ready.

Bar strategies can call `self.history(context, length=30)` for the latest visible Bars or `self.current(context, field="close_raw")` for one latest raw value. For indicator calculations, `self.history_decimal(context, length=30, field="close")`, `self.current_decimal(context, field="close")`, and `self.bar_decimal(bar, "close")` return exact `Decimal` values: source `exact_ohlcv` is preferred, with raw-only inputs decoded at the core 1e-9 scale. This avoids binary floats and prevents sub-nanounit prices from collapsing to zero in the legacy raw projection. All history helpers are capped at the callback's `context["as_of"]`; multi-market callbacks only expose Bars whose timestamps are no later than that decision time. Lifecycle hooks before the first market event (`on_init`, `on_start`, `on_restore`, and zero-Bar warmup completion) receive an empty market history, so a full frozen run snapshot cannot seed future values into strategy state. `length` is explicit and capped at 10,000 rows. This data portal is available to local `BaseStrategy` backtests and the Python Runtime class worker. `history_requirements()` remains a separate warmup declaration: querying 30 rows does not itself suppress the first 29 decisions.

For account context, `self.cash_balance(context, "USDT")` and `self.available_margin(context)` return `Decimal` values and prefer the input v9 exact sidecars when the ledger or venue supplied them. They fall back to the legacy 1e-9 raw values when the source had no higher precision; neither helper converts through binary float.

Execution feedback is split by fact type. `on_order_status_update(context, update)` dispatches committed OMS status changes to `on_order`. `on_fill_update(context, update)` receives a committed fill with `event_seq` and event time; the default projects to the legacy fill-only `on_order_update` and then `on_trade`, so existing strategies remain compatible. Use `event_seq` to deduplicate a replayed fill. `on_trade` means one committed fill, not every order transition, and strategy code must not book that fill a second time. The versioned account-update input can dispatch a complete account snapshot to `on_account`, or a complete position set first to `on_positions_snapshot` and then to `on_position` for each row; empty position sets are delivered too. These are observation callbacks and do not change the host ledger. Error observers cannot turn a failed callback into a successful run. `diagnostic_state()` is a bounded read-only diagnostic snapshot, not restart persistence. Returning `None` from a market hook means “no intents for this event.”

```python
from qianxing_strategy import BaseStrategy

class MyStrategy(BaseStrategy):
    def history_requirements(self):
        return {"warmup_bars": 30}

    def on_bar(self, context, bar):
        history = self.history(context, length=30)
        if len(history) < 30:
            return None
        if should_enter(bar, history):
            return self.decision(
                context,
                "Bar",
                bar,
                intents=[self.buy(bar["instrument"], qty_raw=1_000_000_000)],
            )
        if should_exit(bar):
            intent = self.close_position(context, bar["instrument"])
            if intent is not None:
                return self.decision(context, "Bar", bar, intents=[intent])
        return None

    def on_trade(self, context, trade):
        # Called once per committed fill; do not book the fill again here.
        return None

    def on_order(self, context, update):
        # Called for a committed OMS order-state transition.
        return None
```

The compatibility fill contract is [`strategy-update-v1.json`](../schemas/strategy-update-v1.json), with Rust/Python fixture [`strategy-update-fill-v1.json`](../crates/qx-strategy/tests/fixtures/strategy-update-fill-v1.json). Sequenced fill updates use [`strategy-fill-update-v2.json`](../schemas/strategy-fill-update-v2.json), with fixture [`strategy-fill-update-v2.json`](../crates/qx-strategy/tests/fixtures/strategy-fill-update-v2.json); committed order states use [`strategy-order-status-update-v1.json`](../schemas/strategy-order-status-update-v1.json). Exact order/fill amounts remain scale-aware when present.

`buy()` / `sell()` / `short()` / `cover()` / `close_position()` only build intents and never send orders. Intent IDs are generated per strategy instance unless supplied explicitly. `position_qty(context, instrument)` prefers the exact signed position and falls back to the core 1e-9 raw scale. `close_position()` creates a reduce-only order for a net position; hedge-mode long/short legs must specify their position policy explicitly. `short()` builds a sell-side entry intent, while `cover()` builds a reduce-only buy intent. For hedge mode, pass `policy={"position_mode": "hedge", "position_side": "short", "margin_mode": "cross", "leverage": 3}` to address the short leg; Rust still validates product support, leverage, lot size, minimum quantity/notional and account capability. Use `qty_raw`/`limit_price_raw` for the core 1e-9 scale, or pass decimal strings such as `qty_exact="0.0000000001"` and `limit_price_exact="0.00000005"`; floats are rejected. `quote_budget="25.5"` builds a standalone market-buy budget intent.

Local strategy-file commands execute trusted Python on the local machine without sandbox setup. Depending on the entry point, the callback may run in the CLI process or a local strategy-worker subprocess; neither is a security boundary. A strategy file can export `create_strategy(spec)` or `STRATEGY_CLASS`; direct classes may take the normalized run spec or use a no-argument constructor. Override the Bar/Tick/OrderBook hook you need. The Tick example above can also be tuned locally with `search-strategy`; replace the example's `depth_path` with a valid L1/L2 frame first. Treat strategy files as code with the same local permissions as the user running them. Multi-user servers and uploaded strategies should require a separately isolated sandbox host through deployment policy; that host must enforce resource, filesystem, network and credential boundaries before being advertised as sandbox support.

Pass an instance or factory to `run_backtest_with_strategy`; every decision still goes through Rust validation, risk, OMS, matching, accounting and replay. The same `create_strategy(spec)` file entry works with Bar and depth CLI runs; the returned object can use `BaseStrategy` hooks or implement `on_event(context, event)` directly.

### Run a `BaseStrategy` in the Rust strategy worker

Runtime strategy configurations can opt into the class adapter by setting both `python_module` and `python_strategy_spec`. Without `python_strategy_spec`, the existing module-level `on_event(request)` / legacy `on_decision(request)` worker contract is unchanged. Class mode uses the module's `create_strategy(spec)` factory, or a `STRATEGY_CLASS` constructor that accepts the normalized spec or no arguments when no factory is exported; it requires the result to inherit `BaseStrategy`.

```json
{
  "strategy": {
    "python_module": "python/examples/strategy_file_sma.py",
    "python_strategy_spec": {
      "strategy": {"quantity_raw": 1000000},
      "parameters": {"fast_window": 5, "slow_window": 20}
    }
  }
}
```

The class adapter invokes `on_init`, `on_start`, per-instrument warmup notifications, committed order/fill/account updates, then `on_stop` on graceful worker exit. Runtime class workers support Bar, explicit L1 Tick, and durable L2 OrderBook input; `data_level="timer"` receives one `on_timer` event per configured live timeframe bucket. Timer is a live scheduler input, not a synthetic Bar, and timer-only strategies are rejected by local Bar/depth backtest commands. OrderBook events are stored in the Runtime EventLog and replayed through Strategy input v8, including exact price/quantity fields. A full `STRATEGY_MANIFEST` binds parameter defaults and validates types/ranges before the strategy is created; optional `validate_config(config)` handles deterministic cross-parameter checks. Strategy files can import helper modules from their own directory.

Each request replays only new Bars up to `as_of`. Status and fill updates are dispatched in `event_seq` order, with order status before a fill at the same sequence. Use the sequence to make strategy-side updates idempotent. Returning `None` means no new intents and preserves the current exact position; it does not request a close. Warmup Bars may update local indicators, but their intents are suppressed until that instrument is ready. All returned intents still pass through Rust validation, risk, OMS, precision rules and execution.

For `BaseStrategy` class workers, `snapshot_state()` / `restore_state()` persist JSON-compatible strategy state together with the EventLog cursor and adapter checkpoint; restore validates strategy identity and version before replay. Delivery remains at-least-once, so callbacks should be deterministic and idempotent. External side effects outside returned intents cannot be rolled back. Use `diagnostic_state()` for bounded diagnostics, not persistence.

### Read a verified joint multi-exchange report

After `qianxing app multi-account-joint-backtest` produces an outcome JSON, use the same Rust report reader from the CLI or Python SDK. It verifies the parent and child run identities, replays each child's event log, checks child equity/fill artifacts, and recomputes aggregate equity and fills before returning bounded samples:

```console
qianxing app read-multi-account-joint-report ./runs/joint-outcome.json
```

```python
import json
from qianxing.reports import read_multi_account_joint_report_v1

with open("runs/joint-outcome.json", encoding="utf-8") as stream:
    outcome = json.load(stream)
report = read_multi_account_joint_report_v1(outcome)
assert report["verification"]["verified"]
for account in report["accounts"]:
    print(account["exchange_id"], account["account_id"], account["return_bps"])
```

Event rows can be paged independently by exchange and account; the same account ID may appear at multiple venues without cursor collision:

```python
report = read_multi_account_joint_report_v1(
    outcome,
    joint_account_event_pages={
        "okx": {"primary": {"offset": 0, "limit": 50}},
        "bitget": {"primary": {"offset": 100, "limit": 50}},
    },
)
```

The equivalent CLI/API request wraps the outcome with `joint_account_event_pages`. The API returns each account's `previous_offset` and `next_offset`; the Web console uses those cursors to page the selected account. Each page request replays and verifies all member logs before returning the requested rows. No global cross-exchange event timeline or cursor is produced because independent account logs do not establish one causal order. The report is for research comparison; it does not imply cross-account margin netting, shared buying power, or live execution readiness.

To save a self-contained offline HTML view, install the Python package with its Rust extension and call the writer from `qianxing.reports`:

```python
from qianxing.reports import write_multi_account_joint_report_html

path = write_multi_account_joint_report_html(
    outcome,
    "reports/joint-account.html",
    joint_account_event_pages={
        "okx": {"primary": {"offset": 0, "limit": 50}},
        "bitget": {"primary": {"offset": 0, "limit": 50}},
    },
)
print(path)
```

The writer calls the Rust-verified report reader before rendering. The page compares account return/drawdown, plots the bounded joint-equity sample with exact raw-value hover, filters account rows, and lets you export the included account and fill samples as CSV. Event rows remain independently scoped and paged per venue/account; the HTML does not invent a global event order, cross-currency conversion, or shared margin. To inspect another event page, regenerate the report with the next offsets.

`run-strategy` also accepts a `MultiMarketBacktestSpec` with a top-level `markets` array. It creates a fresh Python strategy instance for each declared strategy slot and routes those callbacks through the shared Rust portfolio ledger and event timeline. The factory receives the full spec plus top-level `market`, `strategy`, and `strategy_slot` values for the slot being created, so one strategy file can configure different market slots. Use `--strategy-id <id>` to replace only one slot; without it, all declared slots use the Python callback. Each slot has independent Python state. Multi-market callback runs share the Rust account/risk/OMS state; each slot or search candidate gets a fresh Python object, while cross-market shared Python callback state is not exposed yet.

The multi-market sample runs `examples/strategy_file_sma.py` against Binance and OKX BarFrames with one shared simulated account. It is a class-based `BaseStrategy` example: `STRATEGY_CLASS` exposes the class-level manifest, `history_requirements()` declares warmup, `history()` reads only Bars visible at the callback `as_of`, and `on_bar()` builds intents through the helper API. It is a local backtest example; it does not connect to either exchange. Run it from the repository root so the relative dataset paths resolve.

The runnable examples in this repository are `examples/strategy_file_sma.py`, `examples/strategy-file-backtest.example.json`, `examples/strategy-file-multi-market-backtest.example.json`, `examples/strategy-file-search.example.json`, and `examples/strategy-file-multi-market-search.example.json`. In the multi-market search sample, Python replaces only the Binance target slot; the OKX slot remains the configured built-in strategy. It includes a two-fold walk-forward split and a seeded KDE-TPE search over stepped `fast_window`/`slow_window` fields, which match the strict Rust strategy spec. From the repository root, run `qianxing search-strategy python/examples/strategy_file_sma.py python/examples/strategy-file-multi-market-search.example.json --summary` to see the proposal history, training ranking, and each fold's held-out metrics. Each run identity binds a versioned SHA-256 strategy-bundle digest: the main file and eagerly imported local Python modules are included automatically. Declare lazily imported local helpers as `STRATEGY_DEPENDENCIES = ("indicators.py", "helpers/risk.py")`; paths must stay under the strategy file's directory and point to `.py` files.

### Query trading state from Python

`AppClient` wraps the Rust account read models as well as research workflows. Pass both `account_id` and `venue_id` to scope a snapshot, balance, position, order, or reconciliation query; the SDK encodes the query parameters and rejects an incomplete pair before sending the request. Ledger reads accept `account_id`. Portfolio reads combine accounts by configured `portfolio_id` while preserving currency-specific values; an optional `reporting_currency` asks the service for its configured indicative conversion.

```python
from qianxing import AppClient

client = AppClient("https://127.0.0.1:8443", headers={"Authorization": "Bearer …"})
positions = client.get_account_positions(account_id="okx-demo", venue_id="okx")
orders = client.get_account_orders(account_id="binance-demo", venue_id="binance")
activity = client.get_account_activity(
    account_id="binance-demo", venue_id="binance", limit=50
)
portfolio = client.get_account_portfolio("crypto-demo", reporting_currency="USDT")
```

The account activity report combines one scoped snapshot with bounded position/order/fill/transfer rows and a retained account-event page. Use `after` or `before` to page that account's projection log; it does not claim to cover the venue's complete history. Portfolio reporting preserves native per-currency totals and does not imply cross-account margin netting or joint buying power. The read endpoints use the runtime's configured authentication policy; keep API credentials outside strategy files.

Amounts and quantities use the engine's fixed-point `*_raw` units. Install optional integrations only when needed, for example `pip install 'qianxing[ccxt]'` or `pip install 'qianxing[a-share]'`.

See the [repository documentation](https://github.com/coeasy/qianxing) for strategy contracts, CLI usage, release artifacts, maturity evidence, and the supported operating boundaries.

### Exact MultiMarket account principal

`MultiMarketBacktestSpec` v1 continues to accept `initial_cash_raw`. Use schema v2 and `initial_cash_exact: {"coefficient": "15", "scale": 10}` for sub-nano principal. `initial_cash_raw` must equal the toward-zero 1e-9 projection and may be zero for a positive exact amount below that quantum. Exact strategy budgets are checked against the exact account principal; Rust persists exact shared equity and report verification requires the exact equity sidecar.

### Exact MultiMarket Depth account principal

`MultiMarketDepthBacktestSpec` also preserves the v1 raw contract and supports v2 exact principal. Set `schema_version=2`, `initial_cash_exact={"coefficient":"13","scale":10}`, and the matching `initial_cash_raw=1` for `0.0000000013`. The SDK validates positivity and the toward-zero projection. A positive sub-nano principal may project to raw 0; the shared ledger and research report retain its exact value and equity curve.
