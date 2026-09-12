# Qianxing Strategy API v1

Strategy API v1 is the only supported boundary between a strategy and the trading runtime.

## Direction of data

```text
Runtime → StrategyContext + MarketEvent
Strategy → StrategyDecision(intents[])
Runtime → RiskGate → OMS → ExecutionPort
```

Strategies never receive credentials, Venue clients, mutable Ledger handles, or control-plane handles.

## Numeric and identity rules

- Amounts, prices and quantities use signed fixed-point raw integers.
- `request_id`, `strategy_id`, `strategy_version`, `signal_id` and `intent_id` are mandatory.
- An `intent_id` is unique within the strategy account and must be safe to retry.
- Every output has an expiry. Expired decisions are rejected before OMS registration.
- Runtime validates the data fingerprint and `as_of` boundary before invoking a strategy.
- Rust, C++ and Python must produce equivalent `StrategyDecision` values for the same replay input.

## Compatibility

The existing `target_qty` field remains a compatibility path for legacy rebalance strategies. New
strategies should return `intents[]`; the runtime will eventually make `target_qty` opt-in legacy mode.

