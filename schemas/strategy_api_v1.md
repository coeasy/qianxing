# Qianxing Strategy API v1

Strategy API v1 is the only supported boundary between a strategy process (Rust / Python / C++)
and the trading runtime. The machine-readable shape lives in
`strategy_api_v1.schema.json`; the authority behind both is
`qx_runtime::{StrategyContractInput, StrategyContractOutput, StrategyContractIntent}`, and
`crates/qx-runtime/tests/strategy_api_schema_contract.rs` pins the three together field by field
(emitted key set, required set, contract version).

## Direction of data

```text
Runtime -> StrategyContractInput (read-only, fixed-point)
Strategy -> StrategyContractOutput (intents[] or legacy target_qty)
Runtime -> RiskGate -> OMS -> ExecutionPort
```

Strategies never receive credentials, Venue clients, mutable Ledger handles, or control-plane
handles. `strategy_version` rides on the input only; the output echoes `request_id`,
`strategy_id` and `instrument` back.

## Numeric and identity rules

- Amounts, prices and quantities use signed fixed-point raw integers.
- Mandatory output fields: `schema_version` (must equal the contract version), `request_id`,
  `strategy_id`, `signal_id`, `instrument`, `target_qty`, `confidence`, `priority`,
  `expires_at`. `intents` may be omitted.
- Mandatory intent fields: `intent_id`, `instrument`, `side`, `qty_raw`. `limit_price_raw`,
  `reduce_only`, `post_only`, `position_side`, `margin_mode`, `position_mode` and `leverage`
  default when absent.
- `signal_id` and every `intent_id` are non-zero, and an `intent_id` is unique within one
  output; the runtime checks uniqueness per decision, not per account.
- Because `intent_id` is only a per-output ordinal while `client_order_id` and `command_id` are
  account-wide unique keys, the runtime folds the durable identity from the pair
  (`request_id`, `intent_id`) instead of reusing the ordinal. Replaying one round lands on the
  same identity; a later round yields a new one. `trace.intent_id` keeps the strategy's value.
- `expires_at == 0` means "no expiry". A non-zero `expires_at` earlier than the input's
  `as_of` is rejected before OMS registration.
- `side` is `buy` or `sell`; `position_side` is `net` / `long` / `short`; `margin_mode` is
  `cash` / `cross` / `isolated`; `position_mode` is `one_way` / `hedge`; `leverage >= 1`.
- Runtime validates the input's identity, `as_of` and data fingerprint before invoking a
  strategy.

## Compatibility

`intents: []` (or an absent `intents`) keeps the legacy single-target path: the runtime turns
`target_qty` into a portfolio rebalance plan. A non-empty `intents[]` is never re-derived into a
target position - the runtime refuses, because the orders already express the delta.

## Known gaps (V11 R4-4)

- The reader decodes with serde and ignores unknown keys, so a misspelled optional key is
  silently dropped. `additionalProperties: false` in the schema is the authoring contract, which
  is deliberately stricter than the reader.
- The reader matches the four word fields case-insensitively, while this file lists the lowercase
  form - which is also what the writer emits. `side: "BUY"` therefore parses but is not
  schema-valid; author in lowercase.
- The Python bridge's `StrategyIntent` emits 8 of the 11 intent fields: per-leg `margin_mode`,
  `position_mode` and `leverage` are read by the runtime but not reachable from a Python
  strategy. Rust and C++ strategies can express them.
