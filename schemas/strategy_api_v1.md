# Qianxing Strategy API v1

Strategy API v1 is the only supported boundary between a strategy process (Rust / Python / C++)
and the trading runtime. The machine-readable shape lives in
`strategy_api_v1.schema.json`; the authority behind both is
`qx_runtime::{StrategyContractInput, StrategyContractOutput, StrategyContractIntent}`. One
resident pin test keeps the three aligned field by field, and a second one nails only the two
facts the first does not cover:
`crates/qx-runtime/tests/strategy_api_schema_contract.rs` (the key set the writer emits, the
required set the reader cannot default, the contract version, the closed word vocabulary - all
four measured through `to_json_for` / `from_json_for`, i.e. the production serializer and the
production reader) and `crates/qx-runtime/tests/strategy_contract_schema.rs` (unknown-key
rejection and the integer-only wire form of the fixed-point fields).

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
amount), `available_margin_raw` (null when the runtime could not compute it), `risk_state`,
`research_targets` (instrument -> raw target), `bars` (null unless the run declares bar
history). `strategy_version` rides on the input only - an output echoes `request_id`,
`strategy_id` and `instrument` back instead. The reference reader is
`python/qianxing_bridge/strategy.py`; there is no machine-readable schema for this direction
yet, so what a Python strategy can see is exactly what the Rust writer emits.

Before a strategy is invoked, the runtime validates the input it is about to hand over:
identity text and `risk_state` non-empty, `as_of != 0`, a non-empty `data_fingerprint`,
every instrument parseable, and when bars ride along, equal column lengths with strictly
increasing timestamps. Both encoders run that check - `to_json` and
`encode_strategy_columnar_input` - so a strategy cannot be handed an input the runtime would
not accept from itself.

## Output: `StrategyContractOutput`

Machine-readable form: [`strategy_api_v1.schema.json`](strategy_api_v1.schema.json). Its
`required` list equals the non-`#[serde(default)]` fields of the Rust structs, and its
`additionalProperties: false` equals their `#[serde(deny_unknown_fields)]` -
`crates/qx-runtime/tests/strategy_api_schema_contract.rs` fails if either key list drifts, and
`crates/qx-runtime/tests/strategy_contract_schema.rs` fails if an unknown key is dropped rather
than rejected. Neither side can therefore silently gain or lose a key.

- Mandatory output fields: `schema_version` (must equal the contract version), `request_id`,
  `strategy_id`, `signal_id`, `instrument`, `target_qty`, `confidence`, `priority`,
  `expires_at`. `intents` may be omitted.
- Mandatory intent fields: `intent_id`, `instrument`, `side`, `qty_raw`. `limit_price_raw`,
  `reduce_only`, `post_only`, `position_side`, `margin_mode`, `position_mode` and `leverage`
  default when absent.

Rules that the schema alone cannot express, enforced by
`StrategyContractOutput::validate_for`:

- `schema_version` must equal `STRATEGY_CONTRACT_SCHEMA_VERSION` (the same constant the
  schema pins as `const`), and `request_id` / `strategy_id` / `instrument` must match the
  input this output answers.
- `signal_id != 0`; `expires_at == 0` means "no expiry", otherwise it must not be earlier
  than the input's `as_of`.
- `intent_id` is unique within one output and non-zero; `qty_raw > 0`; `limit_price_raw`,
  when present, is positive; `leverage`, when present, is non-zero (`u32`, so zero is the
  only value that is not positive).
- `side` accepts `buy`/`sell` case-insensitively and the order builder lowercases it, as it
  lowercases `position_side`. `margin_mode` and `position_mode` are validated
  case-insensitively but matched against the lowercase literals when the order is built, so
  `margin_mode: "ISOLATED"` passes validation and is then refused outright - author in
  lowercase. The schema states the canonical lowercase forms of all four words, which is
  the stricter direction: a payload that validates against the schema always reaches the
  runtime intact, while `side: "BUY"` parses at the runtime yet is not schema-valid.

### What an intent's identity becomes

`intent_id` is only a per-output ordinal while `client_order_id` and `command_id` are
account-wide unique keys, so the runtime folds the durable identity from the pair
(`request_id`, `intent_id`) in `strategy_order_identity` - `request_id` being this round's
run id - instead of reusing the ordinal. Replaying one round lands on the same identity; a
later round yields a new one. `trace.intent_id` keeps the strategy's value, so attribution
is unaffected.

## Numeric rules

Quantities, prices, cash and confidence are signed fixed-point raw integers (core scale
`1e9`, `qx_core::SCALE`) carried as Rust `i128`. On the wire they are **JSON integers only**,
including magnitudes beyond `i64`; the decimal-string form used by account-snapshot
artifacts is rejected here as `invalid number`. The pin test asserts both halves, so
switching this channel to strings has to move the schema, the Rust structs and the SDKs
together.

## Language coverage

All intent fields are reachable from the Rust SDK and from the Python SDK. The Python
`StrategyIntent` in `python/qianxing_bridge/strategy.py` declares `margin_mode`,
`position_mode` and `leverage`, and its `to_dict` emits 11 of the 11 intent fields. Its
`validate` holds the same closed word sets and the same non-zero `leverage` rule that
`StrategyContractOutput::validate_for` applies. The C ABI is the narrow surface:
`qx_order_intent` in `cpp/include/qianxing_strategy.h` stops at `position_side`, so a C++
strategy cannot express those three and its leg falls back to the runtime's own
`config.strategy` product/position/margin settings. In-process Rust strategies carry them as
a typed `OrderPolicy` rather than as words, and `from_native_decision` maps that policy onto
the lowercase word forms above.

Unknown keys in an output are rejected rather than dropped on both live paths: Rust via
`deny_unknown_fields` on `StrategyContractOutput` / `StrategyContractIntent`, the Python SDK
via `_reject_unknown_keys`, so a mistyped optional key fails loudly instead of silently
taking the default. The input direction has no such gate yet - `StrategyInput.from_dict`
reads the keys it knows and ignores the rest.

## Compatibility

`target_qty` remains the legacy single-target path: with `intents` empty the runtime turns
it into a rebalance plan through `build_rebalance_plan`, using the same target-position ->
quantity-delta derivation as backtest, paper and live. New strategies should return
`intents[]`. A non-empty `intents[]` is never re-derived into a target position - the same
function refuses it, because the orders already express the delta.
