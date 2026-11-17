"""Experiment candidate configuration and callback bias diagnostics."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
from typing import Any, Callable, Mapping, Sequence

from .strategy_diagnostics import (
    _multi_market_diagnostic_request,
    _strategy_diagnostic_request,
)


def _experiment_candidate_rows(document: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Return every materialized primary and fold-local candidate with its search scope."""
    rows: list[dict[str, Any]] = []
    for candidate in document.get("candidates", ()):
        if isinstance(candidate, dict):
            rows.append({"scope": "primary", "fold": None, "candidate": candidate})
    search = document.get("search")
    if isinstance(search, dict):
        for fold_index, fold in enumerate(search.get("fold_searches", ()), start=1):
            if not isinstance(fold, dict):
                continue
            fold_number = fold.get("fold", fold_index)
            for candidate in fold.get("candidates", ()):
                if isinstance(candidate, dict):
                    rows.append(
                        {"scope": "fold_local", "fold": fold_number, "candidate": candidate}
                    )
    return rows

def _candidate_diagnostic_spec(
    base_spec: Mapping[str, Any],
    candidate: Mapping[str, Any],
    experiment_spec: Mapping[str, Any],
    strategy_id: str,
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    """Rebuild the candidate settings consumed by create_strategy(spec)."""
    candidate_spec = copy.deepcopy(dict(base_spec))
    raw_parameters = candidate.get("parameters", {})
    if not isinstance(raw_parameters, dict) or any(
        not isinstance(name, str) or type(value) is not int
        for name, value in raw_parameters.items()
    ):
        raise TypeError("candidate parameters must be an object of integer values")
    parameters = dict(raw_parameters)
    kind = candidate.get("strategy_kind")
    if kind is not None and not isinstance(kind, str):
        raise TypeError("candidate strategy_kind must be a string or null")

    markets = candidate_spec.get("markets")
    if isinstance(markets, list):
        target: dict[str, Any] | None = None
        target_slot: dict[str, Any] | None = None
        for market in markets:
            if not isinstance(market, dict):
                continue
            for slot in market.get("strategies", ()):
                strategy = slot.get("strategy") if isinstance(slot, dict) else None
                if isinstance(strategy, dict) and strategy.get("strategy_id") == strategy_id:
                    if target is not None:
                        raise ValueError(f"strategy slot {strategy_id!r} is ambiguous")
                    target, target_slot = market, slot
        if target is None or target_slot is None:
            raise ValueError(f"strategy slot {strategy_id!r} is missing from the experiment base")
        strategy = target_slot.get("strategy")
        if not isinstance(strategy, dict):
            raise TypeError("target strategy slot has no strategy object")
        if kind is not None:
            strategy["kind"] = kind
        builtin_names = {
            row.get("name")
            for row in experiment_spec.get("parameter_space", ())
            if isinstance(row, dict) and isinstance(row.get("name"), str)
        }
        for name, value in parameters.items():
            if name in builtin_names:
                strategy[name] = value
        factory_context = {
            "market": target,
            "strategy": strategy,
            "strategy_slot": target_slot,
            "parameters": parameters,
        }
        return candidate_spec, factory_context

    strategy = candidate_spec.get("strategy")
    if not isinstance(strategy, dict):
        raise TypeError("candidate BacktestSpec has no strategy object")
    if kind is not None:
        strategy["kind"] = kind
    for name, value in parameters.items():
        strategy[name] = value
    return candidate_spec, None


def diagnose_experiment_candidates(
    experiment_spec: Mapping[str, Any],
    experiment_document: Mapping[str, Any],
    strategy_file: str | Path,
    *,
    strategy_id: str,
    strategy_version: str,
    strategy_bundle_sha256: str,
    startup_windows: Sequence[int],
    max_checks: int,
    timeout_seconds: int,
    candidate_limit: int | None = None,
    candidate_rows_builder: Callable[
        [Mapping[str, Any]], list[dict[str, Any]]
    ] | None = None,
    candidate_spec_builder: Callable[
        ..., tuple[dict[str, Any], dict[str, Any] | None]
    ] | None = None,
    multi_market_request_builder: Callable[..., Any] | None = None,
    strategy_request_builder: Callable[..., Any] | None = None,
) -> dict[str, Any]:
    """Attach an independently replayed bias diagnostic to each materialized candidate."""
    candidate_rows_builder = candidate_rows_builder or _experiment_candidate_rows
    candidate_spec_builder = candidate_spec_builder or _candidate_diagnostic_spec
    multi_market_request_builder = (
        multi_market_request_builder or _multi_market_diagnostic_request
    )
    strategy_request_builder = strategy_request_builder or _strategy_diagnostic_request
    base_spec = experiment_spec.get("base")
    if not isinstance(base_spec, dict):
        raise TypeError("candidate diagnostics require an experiment base object")
    rows = candidate_rows_builder(experiment_document)
    if candidate_limit is not None and candidate_limit <= 0:
        raise ValueError("diagnostic candidate limit must be positive")

    from qianxing_strategy.diagnostics import (
        MAX_HANDLER_TIMEOUT_SECONDS,
        MAX_LOOKAHEAD_CHECKS,
        diagnose_callback_strategy,
    )

    if (
        not startup_windows
        or any(type(window) is not int or window <= 0 for window in startup_windows)
        or len(set(startup_windows)) != len(startup_windows)
    ):
        raise ValueError("diagnostic startup windows must be unique positive integers")
    if not 0 < max_checks <= MAX_LOOKAHEAD_CHECKS:
        raise ValueError(f"diagnostic max checks must be in 1..={MAX_LOOKAHEAD_CHECKS}")
    if not 0 < timeout_seconds <= MAX_HANDLER_TIMEOUT_SECONDS:
        raise ValueError(
            f"diagnostic timeout must be in 1..={MAX_HANDLER_TIMEOUT_SECONDS} seconds"
        )

    base_spec_sha256 = hashlib.sha256(
        json.dumps(
            base_spec,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    ).hexdigest()
    items: list[dict[str, Any]] = []
    diagnosed = 0
    failures = 0
    unavailable = 0
    limited = 0
    diagnostic_attempts = 0
    for row in rows:
        candidate = row["candidate"]
        parameters = candidate.get("parameters", {})
        item: dict[str, Any] = {
            "scope": row["scope"],
            "fold": row["fold"],
            "ordinal": candidate.get("ordinal"),
            "strategy_kind": candidate.get("strategy_kind"),
            "parameters": parameters,
        }
        if candidate.get("outcome") is None:
            if candidate.get("pruning") is not None:
                item.update(
                    {"status": "candidate_pruned", "reason": "candidate was pruned before a full outcome"}
                )
            else:
                item.update(
                    {
                        "status": "candidate_failed",
                        "reason": candidate.get("error") or "candidate produced no outcome",
                    }
                )
            unavailable += 1
            items.append(item)
            continue
        if candidate_limit is not None and diagnostic_attempts >= candidate_limit:
            item.update(
                {
                    "status": "not_run_candidate_limit",
                    "reason": f"diagnostic candidate limit {candidate_limit} reached",
                }
            )
            limited += 1
            items.append(item)
            continue

        diagnostic_attempts += 1
        try:
            candidate_spec, factory_context = candidate_spec_builder(
                base_spec, candidate, experiment_spec, strategy_id
            )
            identity_material = {
                "base_spec_sha256": base_spec_sha256,
                "input_data_fingerprint": experiment_document.get("input_data_fingerprint"),
                "strategy_bundle_sha256": strategy_bundle_sha256,
                "scope": row["scope"],
                "fold": row["fold"],
                "ordinal": candidate.get("ordinal"),
                "strategy_kind": candidate.get("strategy_kind"),
                "parameters": parameters,
            }
            item["candidate_fingerprint"] = hashlib.sha256(
                json.dumps(
                    identity_material,
                    ensure_ascii=False,
                    sort_keys=True,
                    separators=(",", ":"),
                    allow_nan=False,
                ).encode("utf-8")
            ).hexdigest()
            if isinstance(candidate_spec.get("markets"), list):
                (
                    request,
                    strategy_config,
                    resolved_context,
                    peer_frames,
                    market_inputs,
                ) = multi_market_request_builder(
                    candidate_spec, strategy_id, strategy_version
                )
                if factory_context is None:
                    raise ValueError("multi-market candidate diagnostic has no factory context")
                resolved_context["parameters"] = factory_context["parameters"]
                diagnostic = diagnose_callback_strategy(
                    request,
                    strategy_file,
                    candidate_spec,
                    strategy_bundle_sha256=strategy_bundle_sha256,
                    startup_windows=list(startup_windows),
                    max_checks=max_checks,
                    timeout_seconds=timeout_seconds,
                    diagnostic_market_bars=peer_frames,
                    factory_context=resolved_context,
                )
                diagnostic["backtest_strategy_config"] = strategy_config
                diagnostic["multi_market"] = {
                    "target_strategy_id": strategy_id,
                    "market_count": len(candidate_spec["markets"]),
                    "peer_market_bars_included": len(peer_frames),
                    "peer_market_instruments": sorted(peer_frames),
                    "market_inputs": market_inputs,
                    "point_in_time_ledger_scope": "target_market_only",
                }
            else:
                request = strategy_request_builder(
                    candidate_spec, strategy_id, strategy_version
                )
                diagnostic = diagnose_callback_strategy(
                    request,
                    strategy_file,
                    candidate_spec,
                    strategy_bundle_sha256=strategy_bundle_sha256,
                    startup_windows=list(startup_windows),
                    max_checks=max_checks,
                    timeout_seconds=timeout_seconds,
                )
                diagnostic["backtest_strategy_config"] = candidate_spec.get("strategy")
            diagnostic["candidate_fingerprint"] = item["candidate_fingerprint"]
            diagnostic["candidate_parameters"] = parameters
            diagnostic["candidate_scope"] = row["scope"]
            diagnostic["candidate_fold"] = row["fold"]
            item.update({"status": "diagnosed", "diagnostics": diagnostic})
            diagnosed += 1
        except Exception as exc:  # noqa: BLE001 - isolate user strategy failures per candidate.
            item.update(
                {
                    "status": "diagnostic_failed",
                    "reason": f"{type(exc).__name__}: {exc}",
                }
            )
            failures += 1
        items.append(item)

    coverage = (
        "no_candidates"
        if not rows
        else "complete"
        if diagnosed == len(rows)
        else "no_runnable_candidates"
        if diagnosed == 0 and failures == 0 and limited == 0
        else "partial"
    )
    return {
        "schema_version": 1,
        "kind": "qianxing-candidate-bias-diagnostics",
        "experiment_id": experiment_document.get("experiment_id"),
        "strategy_id": strategy_id,
        "strategy_version": strategy_version,
        "strategy_bundle_sha256": strategy_bundle_sha256,
        "base_spec_sha256": base_spec_sha256,
        "input_data_fingerprint": experiment_document.get("input_data_fingerprint"),
        "diagnostic_data_scope": "full_declared_input_for_each_candidate_configuration",
        "parameters": {
            "startup_windows": list(startup_windows),
            "max_lookahead_checks": max_checks,
            "timeout_seconds": timeout_seconds,
            "candidate_limit": candidate_limit,
        },
        "candidate_count": len(rows),
        "diagnosed_count": diagnosed,
        "diagnostic_failure_count": failures,
        "unavailable_candidate_count": unavailable,
        "not_run_candidate_count": limited,
        "coverage_status": coverage,
        "items": items,
    }
