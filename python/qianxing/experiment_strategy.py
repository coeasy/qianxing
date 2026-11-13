"""Strategy factory adapters for multi-market experiments."""

from __future__ import annotations

from typing import Callable


def build_multi_market_experiment_factory(
    experiment_spec: dict,
    factory: Callable[[dict], object],
    strategy_id: str,
) -> Callable[[dict], object]:
    """Adapt a candidate's target slot to the strategy-file create_strategy contract."""
    search = experiment_spec.get("search")
    continuous = search.get("continuous_parameter_space", []) if isinstance(search, dict) else []
    dimensions = (
        experiment_spec.get("callback_parameter_space")
        or experiment_spec.get("parameter_space", [])
        or continuous
    )
    parameter_names = [
        dimension.get("name")
        for dimension in dimensions
        if isinstance(dimension, dict) and isinstance(dimension.get("name"), str)
    ]

    def create(candidate: dict, parameters: dict | None = None) -> object:
        markets = candidate.get("markets") if isinstance(candidate, dict) else None
        if not isinstance(markets, list):
            raise ValueError("multi-market experiment candidate has no markets array")
        matches = []
        for market in markets:
            if not isinstance(market, dict):
                continue
            for slot in market.get("strategies", []):
                strategy = slot.get("strategy") if isinstance(slot, dict) else None
                if isinstance(strategy, dict) and strategy.get("strategy_id") == strategy_id:
                    matches.append((market, slot, strategy))
        if len(matches) != 1:
            raise ValueError(
                f"target strategy slot {strategy_id!r} matched {len(matches)} candidates"
            )
        market, slot, strategy = matches[0]
        resolved_parameters = (
            parameters
            if isinstance(parameters, dict)
            else {name: strategy.get(name) for name in parameter_names}
        )
        return factory(
            {
                **candidate,
                "market": market,
                "strategy": strategy,
                "strategy_slot": slot,
                "parameters": resolved_parameters,
            }
        )

    return create
