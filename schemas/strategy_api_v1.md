# Qianxing Strategy API v1

Strategy API v1 is the only supported boundary between a strategy and the trading runtime.

## What crosses the boundary

There are two layers, and they are deliberately the same semantics in two shapes:

```text
in-process   Strategy::on_event(&StrategyContext, &MarketEvent) -> StrategyDecision
             (Rust / C ABI; `qx-strategy`, and the C header's `qx_strategy_decision`)

cross-language  Runtime  -> StrategyContractInput   (read-only snapshot of account + market)
                Strategy -> StrategyContractOutput  (target_qty legacy path, or intents[])

                both      Runtime -> Risk -> OMS -> Venue   (the only place an order becomes real)
```

`StrategyContractOutput::from_native_decision` is the bridge from the native shape to the
JSON shape, so a Rust strategy and a Python strategy that decide the same thing produce the
same orders. The JSON payloads are single-line: a subprocess strategy (Python, or any
language) is driven over stdio JSONL by `crates/qx-cli/src/strategy_host.rs`, and its worker
answers `{"ok": true, "output": {…}}` or `{"ok": false, "error": "…"}`. The
`shared_memory_json` and `shared_memory_columnar` transports carry the same payloads —
columnar mode moves bar history as fixed-width little-endian columns (`QXCB` magic) and
keeps the identity fields as canonical JSON.

Strategies never receive credentials, Venue clients, mutable Ledger handles, or
control-plane handles. `StrategyContractOutput` is an *order intent*, not an order: every
intent is rebuilt into a core `Order` by `build_strategy_order_from_contract_intent` and
then passes Risk, OMS and the Venue gate like any other order.

## Input: `StrategyContractInput`

`schema_version`, `request_id`, `strategy_id`, `strategy_version`, `data_fingerprint`,
`as_of`, `instrument`, `positions` (instrument -> raw quantity), `cash` (currency -> raw
amount), `available_margin_raw` (null when the runtime could not compute it),
`risk_state`, `research_targets` (instrument -> raw target), `bars` (null unless the run
declares bar history). The reference reader is `python/qianxing_bridge/strategy.py`; there
is no machine-readable schema for this direction yet.

## Output: `StrategyContractOutput`

Machine-readable form: [`strategy_api_v1.schema.json`](strategy_api_v1.schema.json). Its
`required` list equals the non-`#[serde(default)]` fields of the Rust structs, and its
`additionalProperties: false` equals their `#[serde(deny_unknown_fields)]` —
`crates/qx-runtime/tests/strategy_contract_schema.rs` fails if the two drift apart, so
neither side can silently gain or lose a key.

Rules that the schema alone cannot express, enforced by
`StrategyContractOutput::validate_for`:

- `schema_version` must equal `STRATEGY_CONTRACT_SCHEMA_VERSION` (the same constant the
  schema pins as `const`), and `request_id` / `strategy_id` / `instrument` must match the
  input this output answers.
- `signal_id != 0`; `expires_at == 0` means "no expiry", otherwise it must not be earlier
  than the input's `as_of`.
- `intent_id` is unique within one output and non-zero; `qty_raw > 0`;
  `limit_price_raw`, when present, is positive; `leverage`, when present, is non-zero.
- `side` accepts `buy`/`sell` case-insensitively and the runtime lowercases it, as it does
  for `position_side`, `margin_mode` and `position_mode`. The schema states the canonical
  lowercase forms, which is the stricter direction: a payload that validates against the
  schema always reaches the runtime intact.

The Python package also provides identity-bound constructors so a strategy does not have
to copy request metadata into every output by hand:

```python
StrategyOutput.for_target_position(
    request,
    signal_id=max(1, request.as_of),
    target_qty_exact="0.0000000001",
)

StrategyOutput.for_intents(
    request,
    signal_id=max(1, request.as_of),
    intents=(intent_a, intent_b),
)
```

They preserve the same wire contract and validate integer widths before serialization.
They only describe a target or intent; quantity delta, market rules, Risk and OMS remain
runtime responsibilities.

## Numeric rules

Quantities, prices, cash and confidence are signed fixed-point raw integers (core scale
`1e9`) carried as Rust `i128`. On the wire they are **JSON integers only**, including
magnitudes beyond `i64`; the decimal-string form used by account-snapshot artifacts is
rejected here as `invalid number`. The pin test asserts both halves, so switching this
channel to strings has to move the schema, the Rust structs and the SDKs together.

## Language coverage

`margin_mode`, `position_mode` and `leverage` are carried on the Rust intent and on the
Python `StrategyIntent` (`python/qianxing_bridge/strategy.py`, which validates and serializes
all three). The stable C ABI v2 `qx_order_intent` layout is unchanged. C++ plugins can expose
the optional `qx_strategy_get_order_policy_v1` symbol to return these fields by `intent_id`;
its bounded JSON payload is defined by
[`c-strategy-order-policy-v1.json`](c-strategy-order-policy-v1.json). Older plugins without
the symbol keep the v2 runtime defaults. The host rejects unsupported schema versions,
unknown fields, invalid modes, zero leverage, malformed UTF-8 and oversized payloads.
An extension object either omits all three fields to preserve the runtime defaults or
specifies `margin_mode`, `position_mode` and `leverage` together; partial overrides are
rejected because the native `OrderPolicy` contract is a complete policy value.
Unknown keys are also rejected in Rust strategy-file contracts via `deny_unknown_fields`
and in the Python SDK via `_reject_unknown_keys`, so a mistyped optional key fails loudly
instead of silently taking the default.

## Compatibility

`target_qty` remains the legacy single-target path: with `intents` empty the runtime turns
it into a rebalance plan through `build_rebalance_plan`, using the same target-position ->
quantity-delta derivation as backtest, paper and live. New strategies should return
`intents[]`.
