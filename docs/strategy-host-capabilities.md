# Strategy host capabilities

File-backed Python strategies may declare an optional `required_capabilities` list in `STRATEGY_MANIFEST`. The shared Rust execution preflight validates the list against the actual execution input before dataset loading and before callbacks. Run, search and diagnostic callers using this preflight share the same check. Existing manifests that omit the list retain their previous behavior.

```python
STRATEGY_MANIFEST = {
    # Existing required metadata: schema_version, parameters, instrument_requirements,
    # data_level, warmup_bars, and optional strategy identity/dependencies.
    "required_capabilities": ["bar", "bar_history", "fill_updates"],
    # ...
}
```

The snippet only shows the capability declaration; the other existing manifest fields remain required for a complete declaration. Unknown names, non-array values, non-string values and duplicate requirements are rejected. Explicit requirements are checked for both complete and partial manifests. Capability selection comes from the execution input (including an experiment's `base`), rather than trusting a partial strategy declaration.

Runtime strategy configuration may also declare `strategy.required_capabilities`
for the selected execution host. The field is omitted from serialized configurations
when empty, preserving the previous config shape. Runtime validates it against the
Rust-owned host catalog before starting a process or worker. For C ABI plugins the
loader resolves the exported callbacks and checks requirements before invoking the
plugin's `qx_strategy_create_v1` constructor. Python manifest requirements remain
an additional strategy-owned check before its factory is called.

## Current local Python delivery matrix

| Capability | Bar host | Tick host | OrderBook host |
|---|---|---|---|
| `bar` | Supported | Unsupported | Unsupported |
| `tick` | Unsupported | Supported | Unsupported |
| `orderbook` | Unsupported | Unsupported | Supported |
| `bar_history` | Supported | Unsupported | Unsupported |
| `multi_instrument` | Supported | Supported | Supported |
| `fill_updates` | Supported | Supported | Supported |
| `order_status_updates` | Supported | Unsupported | Unsupported |
| `account_updates` | Unsupported | Unsupported | Unsupported |
| `position_updates` | Unsupported | Unsupported | Unsupported |
| `timer` | Unsupported | Unsupported | Unsupported |
| `durable_checkpoint` | Unsupported | Unsupported | Unsupported |

Support means the host implements the delivery path. A run still needs valid inputs and fills only generate callbacks when execution commits a fill. `bar_history` exposes only Bars visible at callback `as_of`; it does not grant arbitrary historical data access. A single local run's Python state is not a durable server checkpoint.

`multi_instrument` is supported by local Python adapters because their Bar, Tick,
and OrderBook dispatch paths preserve each event's instrument identity. Local
multi-leg built-in backtests use the separate `local_multi_builtin_rust` catalog.
Persistent server strategy workers currently process a single instrument per
job and therefore do not advertise this capability; a strategy that requires it
is rejected by those hosts before dispatch. The server built-in catalog also
omits it, including for multi-leg built-ins that are not supported by that
server host.

The Rust preflight result includes `host_capabilities` with `schema_version`, `host_id`, `profile`, and `supported`. The current host IDs are `local_python_bar`, `local_python_tick`, `local_python_orderbook`, and `local_python_unresolved`. `qianxing project doctor` and local project plans expose that same Rust result. See [host capability schema](../schemas/strategy-host-capabilities-v1.json) and [Python strategy manifest schema](../schemas/python-strategy-manifest-v1.json).

## Persistent Runtime Python class host

The Runtime class host receives a Rust-generated `StrategyHostCapabilitiesV1`
in its explicitly constructed worker environment. Local workers pass it directly;
E2B, OpenSandbox, llm-sandbox and Microsandbox bridges forward the same catalog
into their isolated guest process through an explicit environment allowlist.
It uses `profile="server"` and `host_id="runtime_python_class_<data_level>"`.
The host selects the data level from the actual `python_strategy_spec` (default
Bar), rather than the strategy manifest. Python validates catalog identity and
requirements before invoking the factory. Unknown, duplicate, malformed and
unsupported requirements fail closed.

| Runtime class capability | Bar | Tick | OrderBook | Timer |
|---|---|---|---|---|
| Market callback | `bar` | `tick` | `orderbook` | `timer` |
| `bar_history` | Supported | Not claimed | Not claimed | Not claimed |
| `fill_updates` | Supported | Supported | Supported | Supported |
| `order_status_updates` | Supported | Supported | Supported | Supported |
| `account_updates`, `position_updates` | Scoped | Scoped | Scoped | Scoped |
| `durable_checkpoint` | Supported | Supported | Supported | Supported |

Scoped account/position delivery requires both configured account and venue.
Checkpoint support describes the existing class adapter checkpoint/cursor
protocol; users must implement appropriate `snapshot_state`/`restore_state`
for their strategy state. It does not establish atomic venue submission or
exactly-once external side effects.

The persistent Runtime module-function host uses `host_id="runtime_python_module_function"`.
Its catalog is created by Rust and checked before the module handler is constructed.
It claims the market-data variants represented by the versioned `StrategyInput`
contract (`bar`, `tick`, `orderbook`, and `bar_history`), fill/order-status
delivery, and the persistent checkpoint protocol; it does not claim a timer
callback. Account and position updates are listed only when account and venue
are both configured. This describes host delivery support, not a guarantee that
an individual job contains every market-data variant.

The persistent C ABI worker records `host_id="c_abi"` from the verified loaded
plugin exports. Its catalog includes order-status, fill, account/position, and
durable-checkpoint delivery only when the corresponding plugin callback exists;
account/position delivery also requires configured account and venue scope.
Market-data levels remain unclaimed because C ABI worker configuration does not
negotiate one catalog level. A plugin may additionally export
`qx_strategy_required_capabilities_v1`, which writes a bounded
[`StrategyRequiredCapabilitiesV1`](../schemas/c-strategy-required-capabilities-v1.json)
JSON object. Rust checks that strategy-owned declaration and runtime config
requirements against the verified plugin callback catalog before invoking
`qx_strategy_create_v1`. Plugins without the optional export keep their legacy
behavior.

The built-in Rust server strategy worker records
`host_id="builtin_rust_strategy_runtime"` with `bar` and `bar_history`: its
execution path loads the configured BarFrame and invokes the strategy once per
historical Bar in chronological order. It does not claim timer, execution-update,
or durable user-state checkpoint capabilities. Each `BuiltinStrategyKind` now
declares its required market capability in Rust; the worker validates that
declaration and optional runtime config requirements against the built-in host
catalog before starting execution.

The persistent external-process server worker receives the Rust-owned
`runtime_external_process` catalog in `QX_STRATEGY_HOST_CAPABILITIES_JSON` before
launch. It describes the versioned input's Bar/history, tick, order-book,
fill/order-status, checkpoint, and explicitly scoped account/position delivery.
The local backtest process gets a separate `local_external_process` catalog with
only Bar/history. Runtime configuration may set `required_capabilities`; Rust
checks it against that catalog before starting the process.

For a nonempty requirement list, the server process launcher also sets
`QX_STRATEGY_REQUIRED_CAPABILITIES_JSON` and a private
`QX_STRATEGY_CAPABILITY_ACK_FILE` path. The process must validate its required
capabilities against the supplied catalog and atomically publish a
`StrategyHostCapabilityAckV1` before Rust returns the client to the worker.
Rust checks the acknowledgment's schema, host identity/profile, contract version,
catalog, requirement list, and ready flag. The wait is bounded by the configured
process timeout and the response is limited to 64 KiB; timeout, malformed or
mismatched acknowledgment, and unsupported requirements fail startup. An empty
requirement list preserves the legacy startup behavior. The C++ JSONL/framed/
shared-memory example implements this handshake; other executable hosts must
implement the same environment and acknowledgment contract when requirements
are configured. See the [acknowledgment schema](../schemas/strategy-host-capability-ack-v1.json)
and [capability catalog schema](../schemas/strategy-host-capabilities-v1.json).

When an external-process checkpoint is supplied, the host serializes input schema
v10 and adds `strategy_checkpoint_sha256`, computed by Rust over the canonical
checkpoint JSON. After the process restores that checkpoint, it must return
`checkpoint_restore_ack` with schema version 1 and the same digest. Rust rejects
missing, malformed, or mismatched acknowledgments before accepting the output or
committing the execution cursor. JSONL and columnar shared-memory inputs carry
the same digest. The bundled Python JSONL worker and C++ example echo the digest
only after their request handler completes; custom executables must implement
the same response contract. See [input schema v10](../schemas/strategy-api-input-v10.json)
and the [restore acknowledgment schema](../schemas/strategy-process-checkpoint-restore-ack-v1.json).

The launcher reserves a unique startup marker and waits for Python capability
validation and class construction to complete before returning the client to the
Runtime worker. A failed or hung factory cannot publish worker readiness. The
wait is bounded by the configured Python timeout; failure cleans up the marker
and child. UTF-8 interpreter mode keeps protocol and diagnostics consistent on
Windows. JSONL, framed JSON and both local shared-memory transports exercise
this startup path. `on_init`, checkpoint restoration and market callbacks still
run when the appropriate input arrives, so this startup acknowledgment does not
prove those later steps or live trading readiness.

The canonical pure-Python metadata implementation now lives in
`qianxing_strategy.manifest`. `qianxing.strategy_manifest` preserves the existing
public imports. Class worker setup no longer imports the application SDK, its
HTTP/SSL dependencies, or the native extension just to normalize metadata.

## Remaining deployment boundaries

Local file-backed Python manifests are checked against the selected local data
host during shared Rust preflight. Local `python_module` workers now receive a
Rust-owned local catalog before module handler construction, and Python checks
manifest requirements against its identity and supported set. Standalone and
server external-process launchers both pass their profile-specific catalog;
with nonempty config requirements the process must acknowledge the catalog
before first input. C ABI requirements are checked against verified plugin
exports before the constructor, and built-in requirements are checked against
the Rust Bar host before execution.

Static server plans now expose the Rust-owned catalog and the result of checking
runtime config requirements. For C ABI they report that export inspection,
including the optional strategy declaration, is deferred to verified library
load. They do not import user Python source and report manifest requirements as
`not_imported`; readiness remains blocked until worker startup performs that check.
Cross-host behavior acceptance still needs to exercise required-supported and
required-unsupported paths for every host.
Capability support does not prove feed freshness, credential validity, venue
reconciliation, checkpoint restoration, or a live-ready deployment.

No remote support is inferred from local subprocess support.

## Live warmup evidence for process and C ABI hosts

Live admission reads per-instrument warmup evidence from the last durable
checkpoint. Runtime Python class checkpoints retain `adapter_state.warmup_completed`;
external process checkpoints must put the same non-empty instrument array in
`adapter_state.warmup_completed`, and C ABI checkpoint snapshots must put it in
`strategy_state.warmup_completed`. The worker checks every instrument whose intent
would be submitted. Missing, malformed, or duplicate evidence blocks the live intent.
The built-in Rust `StrategyRuntime` is the only host whose contract has no
user-defined warmup lifecycle, so its warmup check is not applicable.

Runtime module-function workers use the same durable checkpoint envelope when
`python_strategy_spec` is omitted. They must declare `snapshot_state` plus
`restore_state`, or explicitly declare `STRATEGY_STATELESS = True`; startup fails
before the ready marker when neither form is available. A live module-function
strategy reports completed instruments with `warmup_completed_instruments()`.
The Runtime worker persists this list alongside the module checkpoint and checks
it against each candidate live intent. Missing evidence keeps the job checkpoint-only
and prevents an order from being dispatched.
