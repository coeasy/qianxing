"""Lookahead and startup-history diagnostics for a versioned Python strategy."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import subprocess
import sys
from bisect import bisect_left, bisect_right
from pathlib import Path
from typing import Any, Sequence

from qianxing_bridge.strategy import StrategyInput

DECISION_FIELDS = ("target_qty", "confidence", "priority", "intents", "signal_id", "expires_at")
DEFAULT_MAX_LOOKAHEAD_CHECKS = 200
DEFAULT_HANDLER_TIMEOUT_SECONDS = 10
MAX_LOOKAHEAD_CHECKS = 1_000
MAX_HANDLER_TIMEOUT_SECONDS = 120
MAX_PIT_FACTS = 20_000
MAX_PIT_VERSIONS_PER_FACT = 100
MAX_PIT_FACT_DECISION_CHECKS = 5_000_000
MAX_PIT_EXAMPLES_PER_STATUS = 25
MAX_ADJUSTMENT_RELATION_CHECKS = 5_000_000
PRICE_ADJUSTMENT_BASES = frozenset({"raw", "forward_adjusted", "backward_adjusted", "unknown"})
PRICE_BAR_FIELDS = frozenset(
    f"bar.{column}{suffix}"
    for column in ("open", "high", "low", "close")
    for suffix in ("", "_raw")
)


def _decision_view(response: dict[str, Any]) -> dict[str, Any]:
    output = response.get("output")
    if response.get("ok") is not True or not isinstance(output, dict):
        raise ValueError(f"strategy worker rejected request: {response.get('error', 'invalid response')}")
    return {key: output.get(key) for key in DECISION_FIELDS}


def _call_strategy(
    strategy: str, request: StrategyInput, timeout_seconds: int,
    worker_command: Sequence[str] | None = None,
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    """Run one isolated request through Python or an external JSONL worker."""
    command = list(worker_command) if worker_command else [
        sys.executable, "-m", "qianxing_strategy.worker", "--module", strategy,
        "--include-diagnostics",
    ]
    process = subprocess.run(
        command,
        input=request.to_json() + "\n",
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout_seconds,
        check=False,
    )
    if process.returncode != 0:
        raise ValueError(
            f"strategy worker exited {process.returncode}: {process.stderr[-1000:]}"
        )
    lines = [line for line in process.stdout.splitlines() if line.strip()]
    if len(lines) != 1:
        raise ValueError(f"strategy worker returned {len(lines)} response lines; expected one")
    value = json.loads(lines[0])
    if not isinstance(value, dict):
        raise ValueError("strategy worker response must be a JSON object")
    state = value.get("diagnostic_state")
    if state is not None and not isinstance(state, dict):
        raise ValueError("strategy diagnostic_state must be a JSON object or null")
    return _decision_view(value), state


def _changed_state_fields(
    baseline: dict[str, Any] | None, counterfactual: dict[str, Any] | None,
) -> list[str] | None:
    if baseline is None and counterfactual is None:
        return None
    if baseline is None or counterfactual is None:
        return ["__availability__"]
    return sorted(
        key for key in baseline.keys() | counterfactual.keys()
        if baseline.get(key) != counterfactual.get(key)
    )


def _evenly_spaced_indices(total: int, limit: int) -> list[int]:
    if total <= 0 or limit <= 0:
        return []
    if total <= limit:
        return list(range(total))
    if limit == 1:
        return [total - 1]
    return [round(index * (total - 1) / (limit - 1)) for index in range(limit)]


def _clone_request(request: StrategyInput, bars: dict[str, Any], as_of: int) -> StrategyInput:
    value = request.to_dict()
    value["as_of"] = as_of
    value["bars"] = bars
    return StrategyInput.from_dict(value)


def _mutate_future_bars(bars: dict[str, Any], first_future_index: int) -> dict[str, Any]:
    changed = {key: list(value) if isinstance(value, list) else value for key, value in bars.items()}
    for column in ("open_raw", "high_raw", "low_raw", "close_raw"):
        changed[column] = [
            item
            if index < first_future_index
            else item * 2 if index % 2 else item // 2
            for index, item in enumerate(changed[column])
        ]
    changed["volume_raw"] = [
        item if index < first_future_index else item + 1
        for index, item in enumerate(changed["volume_raw"])
    ]
    return changed


def _future_perturbations(
    bars: dict[str, Any], first_future_index: int
) -> list[tuple[str, dict[str, Any]]]:
    """Return deterministic, OHLC-valid counterfactuals that probe different inputs."""
    mixed = _mutate_future_bars(bars, first_future_index)
    prices_up = {key: list(value) if isinstance(value, list) else value for key, value in bars.items()}
    prices_down = {key: list(value) if isinstance(value, list) else value for key, value in bars.items()}
    for column in ("open_raw", "high_raw", "low_raw", "close_raw"):
        prices_up[column] = [
            item if index < first_future_index else item * 3
            for index, item in enumerate(prices_up[column])
        ]
        prices_down[column] = [
            item if index < first_future_index else max(1, item // 3)
            for index, item in enumerate(prices_down[column])
        ]
    volume_zero = {key: list(value) if isinstance(value, list) else value for key, value in bars.items()}
    volume_zero["volume_raw"] = [
        item if index < first_future_index else 0
        for index, item in enumerate(volume_zero["volume_raw"])
    ]
    return [
        ("mixed_ohlcv", mixed),
        ("prices_up", prices_up),
        ("prices_down", prices_down),
        ("future_volume_zero", volume_zero),
    ]


def _changed_decision_fields(
    baseline: dict[str, Any], counterfactual: dict[str, Any]
) -> list[str]:
    return [field for field in DECISION_FIELDS if baseline.get(field) != counterfactual.get(field)]


def _lookahead_check(
    request: StrategyInput,
    strategy: str,
    max_checks: int,
    timeout_seconds: int,
    worker_command: Sequence[str] | None = None,
) -> dict[str, Any]:
    if request.bars is None or len(request.bars.ts) < 2:
        return {
            "status": "insufficient_history",
            "checks": [],
            "reason": "lookahead analysis requires at least two bars",
        }
    source = request.bars.to_dict()
    decision_indices = list(range(len(request.bars.ts) - 1))
    if len(decision_indices) > max_checks:
        stride = math.ceil(len(decision_indices) / max_checks)
        decision_indices = decision_indices[::stride]
    checks: list[dict[str, Any]] = []
    for index in decision_indices:
        # At index i the engine's decision time is the next bar timestamp; OHLCV at
        # and after that timestamp must not affect the strategy's decision.
        decision_as_of = request.bars.ts[index + 1]
        baseline, baseline_state = _call_strategy(
            strategy, _clone_request(request, source, decision_as_of), timeout_seconds, worker_command
        )
        for profile, perturbed in _future_perturbations(source, index + 1):
            counterfactual, counterfactual_state = _call_strategy(
                strategy,
                _clone_request(request, perturbed, decision_as_of),
                timeout_seconds,
                worker_command,
            )
            changed_fields = _changed_decision_fields(baseline, counterfactual)
            changed_state_fields = _changed_state_fields(baseline_state, counterfactual_state)
            checks.append(
                {
                    "visible_history_end_index": index,
                    "decision_as_of": decision_as_of,
                    "perturbation_profile": profile,
                    "future_perturbation_changed_decision": bool(changed_fields),
                    "changed_decision_fields": changed_fields,
                    "intermediate_state_changed": bool(changed_state_fields),
                    "intermediate_state_changed_fields": changed_state_fields,
                    "baseline_decision": baseline,
                    "counterfactual_decision": counterfactual,
                    "baseline_intermediate_state": baseline_state,
                    "counterfactual_intermediate_state": counterfactual_state,
                }
            )
    flagged = sum(item["future_perturbation_changed_decision"] for item in checks)
    intermediate_flagged = sum(item["intermediate_state_changed"] for item in checks)
    return {
        "status": "potential_lookahead_detected" if flagged or intermediate_flagged else "no_change_detected",
        "checks_run": len(checks),
        "decision_points_checked": len(decision_indices),
        "checks_sampled": len(decision_indices) < len(request.bars.ts) - 1,
        "flagged_checks": flagged,
        "intermediate_state_flagged_checks": intermediate_flagged,
        "intermediate_state_available": any(
            item["baseline_intermediate_state"] is not None
            or item["counterfactual_intermediate_state"] is not None
            for item in checks
        ),
        "perturbation_profiles": ["mixed_ohlcv", "prices_up", "prices_down", "future_volume_zero"],
        "checks": checks,
        "interpretation": (
            "A changed decision, exported intermediate state, or diagnostic-state availability "
            "after modifying future OHLCV is evidence of potential lookahead. "
            "No changed decision is not proof that all future-data paths are absent."
        ),
    }


def _startup_check(
    request: StrategyInput,
    strategy: str,
    startup_windows: list[int],
    max_checks: int,
    timeout_seconds: int,
    worker_command: Sequence[str] | None = None,
) -> dict[str, Any]:
    if request.bars is None:
        return {
            "status": "insufficient_history",
            "windows": [],
            "trajectory": {"status": "insufficient_history", "decision_points_available": 0,
                           "decision_points_checked": 0, "decision_points_sampled": False,
                           "decision_points_eligible": 0,
                           "decision_points_excluded_insufficient_reference_history": 0,
                           "worker_replays": 0, "changed_windows_bars": [],
                           "changed_fields_by_window": {},
                           "intermediate_state_changed_windows_bars": [],
                           "intermediate_state_changed_fields_by_window": {},
                           "first_divergence": None},
        }
    source = request.bars.to_dict()
    visible_rows = sum(ts < request.as_of for ts in request.bars.ts)
    excluded_future_rows = len(request.bars.ts) - visible_rows
    if visible_rows == 0:
        return {
            "status": "insufficient_history",
            "decision_as_of": request.as_of,
            "visible_bars": 0,
            "excluded_future_bars": excluded_future_rows,
            "windows": [],
            "trajectory": {"status": "insufficient_history", "decision_points_available": 0,
                           "decision_points_checked": 0, "decision_points_sampled": False,
                           "decision_points_eligible": 0,
                           "decision_points_excluded_insufficient_reference_history": 0,
                           "worker_replays": 0, "changed_windows_bars": [],
                           "changed_fields_by_window": {},
                           "intermediate_state_changed_windows_bars": [],
                           "intermediate_state_changed_fields_by_window": {},
                           "first_divergence": None},
        }
    visible_source = {
        key: (value[:visible_rows] if isinstance(value, list) else value)
        for key, value in source.items()
    }
    windows: list[dict[str, Any]] = []
    for requested in startup_windows:
        count = min(requested, visible_rows)
        first = visible_rows - count
        suffix = {
            key: (value[first:] if isinstance(value, list) and key != "source" else value)
            for key, value in visible_source.items()
        }
        decision, intermediate_state = _call_strategy(
            strategy, _clone_request(request, suffix, request.as_of), timeout_seconds, worker_command
        )
        windows.append(
            {
                "requested_bars": requested,
                "available_bars": count,
                "history_truncated": count < visible_rows,
                "requested_window_satisfied": requested <= visible_rows,
                "decision": decision,
                "intermediate_state": intermediate_state,
            }
        )
    reference = windows[-1]["decision"] if windows else None
    reference_state = windows[-1]["intermediate_state"] if windows else None
    for row in windows:
        row["changed_decision_fields"] = _changed_decision_fields(
            row["decision"], reference or {}
        )
        row["intermediate_state_changed_fields"] = _changed_state_fields(
            row["intermediate_state"], reference_state
        )
    trajectory = _startup_trajectory_check(
        request,
        strategy,
        startup_windows,
        windows,
        max_checks,
        timeout_seconds,
        worker_command,
    )
    changed = [row["requested_bars"] for row in windows if row["changed_decision_fields"]]
    state_changed = [row["requested_bars"] for row in windows if row["intermediate_state_changed_fields"]]
    changed = sorted(set(changed).union(trajectory["changed_windows_bars"]))
    state_changed = sorted(
        set(state_changed).union(trajectory["intermediate_state_changed_windows_bars"])
    )
    reference_requested = windows[-1]["requested_bars"] if windows else None
    reference_satisfied = reference_requested is not None and reference_requested <= visible_rows
    if changed or state_changed:
        status = "startup_sensitive"
    elif not reference_satisfied:
        status = "insufficient_reference_history"
    elif trajectory["status"] != "no_change_detected":
        status = trajectory["status"]
    else:
        status = "stable_across_requested_windows"
    decision_fields_by_window: dict[str, set[str]] = {}
    state_fields_by_window: dict[str, set[str]] = {}
    for row in windows:
        decision_fields_by_window.setdefault(str(row["requested_bars"]), set()).update(
            row["changed_decision_fields"] or []
        )
        state_fields_by_window.setdefault(str(row["requested_bars"]), set()).update(
            row["intermediate_state_changed_fields"] or []
        )
    for requested, fields in trajectory["changed_fields_by_window"].items():
        decision_fields_by_window.setdefault(requested, set()).update(fields)
    for requested, fields in trajectory["intermediate_state_changed_fields_by_window"].items():
        state_fields_by_window.setdefault(requested, set()).update(fields)
    return {
        "status": status,
        "decision_as_of": request.as_of,
        "visible_bars": visible_rows,
        "excluded_future_bars": excluded_future_rows,
        "reference_available_bars": windows[-1]["available_bars"] if windows else None,
        "reference_history_complete": reference_satisfied,
        "reference_window_bars": windows[-1]["requested_bars"] if windows else None,
        "changed_windows_bars": changed,
        "changed_fields_by_window": {
            requested: sorted(fields) for requested, fields in sorted(decision_fields_by_window.items())
            if fields
        },
        "intermediate_state_available": any(row["intermediate_state"] is not None for row in windows),
        "intermediate_state_changed_windows_bars": state_changed,
        "intermediate_state_changed_fields_by_window": {
            requested: sorted(fields) for requested, fields in sorted(state_fields_by_window.items())
            if fields
        },
        "trajectory": trajectory,
        "windows": windows,
        "interpretation": (
            "Different decisions or exported intermediate states across startup windows identify warmup sensitivity; "
            "the bounded trajectory compares matching historical decision times as well as the final point. "
            "Only bars strictly before each decision time are replayed; sampled points and an unsatisfied "
            "reference window cannot establish stability for the unchecked history."
        ),
    }


def _startup_trajectory_check(
    request: StrategyInput,
    strategy: str,
    startup_windows: list[int],
    terminal_windows: list[dict[str, Any]],
    max_checks: int,
    timeout_seconds: int,
    worker_command: Sequence[str] | None = None,
) -> dict[str, Any]:
    """Compare startup-window decisions at matching history points with a bounded replay budget."""
    if request.bars is None:
        return {
            "status": "insufficient_history", "decision_points_available": 0,
            "decision_points_checked": 0, "decision_points_sampled": False,
            "decision_points_eligible": 0,
            "decision_points_excluded_insufficient_reference_history": 0,
            "worker_replays": 0, "changed_windows_bars": [],
            "changed_fields_by_window": {}, "intermediate_state_changed_windows_bars": [],
            "intermediate_state_changed_fields_by_window": {}, "first_divergence": None,
            "divergences": [],
        }
    visible_times = [timestamp for timestamp in request.bars.ts if timestamp < request.as_of]
    available_points = len(visible_times)
    base = {
        "decision_points_available": available_points,
        "decision_points_checked": 0,
        "decision_points_sampled": False,
        "decision_points_eligible": 0,
        "decision_points_excluded_insufficient_reference_history": 0,
        "worker_replays": 0,
        "changed_windows_bars": [],
        "changed_fields_by_window": {},
        "intermediate_state_changed_windows_bars": [],
        "intermediate_state_changed_fields_by_window": {},
        "first_divergence": None,
        "divergences": [],
    }
    if available_points == 0:
        return {"status": "insufficient_history", **base}
    # A boundary at the next bar timestamp makes the prior bar visible under `bar.ts < as_of`.
    all_decision_times = [*visible_times[1:], request.as_of]
    reference_window_bars = startup_windows[-1]
    eligible_decision_times = [
        decision_at
        for decision_at in all_decision_times
        if bisect_left(request.bars.ts, decision_at) >= reference_window_bars
    ]
    eligible_points = len(eligible_decision_times)
    excluded_points = len(all_decision_times) - eligible_points
    base.update(
        decision_points_eligible=eligible_points,
        decision_points_excluded_insufficient_reference_history=excluded_points,
    )
    if len(startup_windows) < 2:
        return {"status": "insufficient_comparison_windows", **base}
    if eligible_points == 0:
        return {"status": "insufficient_reference_history", **base}
    points_per_window_set = max_checks // len(startup_windows)
    if points_per_window_set <= 0:
        return {"status": "insufficient_check_budget", **base}
    sample_indices = _evenly_spaced_indices(eligible_points, points_per_window_set)
    decision_times = [eligible_decision_times[index] for index in sample_indices]
    source = request.bars.to_dict()
    terminal_by_window = {row["requested_bars"]: row for row in terminal_windows}
    changed_fields_by_window: dict[int, set[str]] = {}
    state_fields_by_window: dict[int, set[str]] = {}
    first_divergence: dict[str, Any] | None = None
    divergences: list[dict[str, Any]] = []
    worker_replays = 0
    checked_points = 0
    for decision_at in decision_times:
        available_at_point = bisect_left(request.bars.ts, decision_at)
        if available_at_point <= 0:
            continue
        point_rows: list[dict[str, Any]] = []
        for requested in startup_windows:
            count = min(requested, available_at_point)
            first = available_at_point - count
            suffix = {
                key: (value[first:available_at_point] if isinstance(value, list) and key != "source" else value)
                for key, value in source.items()
            }
            terminal = terminal_by_window.get(requested) if decision_at == request.as_of else None
            if terminal is not None:
                decision = terminal["decision"]
                intermediate_state = terminal["intermediate_state"]
            else:
                decision, intermediate_state = _call_strategy(
                    strategy,
                    _clone_request(request, suffix, decision_at),
                    timeout_seconds,
                    worker_command,
                )
                worker_replays += 1
            point_rows.append({
                "requested_bars": requested,
                "decision": decision,
                "intermediate_state": intermediate_state,
            })
        checked_points += 1
        reference = point_rows[-1]
        for row in point_rows[:-1]:
            decision_fields = sorted(
                field for field in DECISION_FIELDS
                if row["decision"].get(field) != reference["decision"].get(field)
            )
            state_fields = _changed_state_fields(
                row["intermediate_state"], reference["intermediate_state"]
            ) or []
            if decision_fields:
                changed_fields_by_window.setdefault(row["requested_bars"], set()).update(decision_fields)
            if state_fields:
                state_fields_by_window.setdefault(row["requested_bars"], set()).update(state_fields)
            if decision_fields or state_fields:
                divergence = {
                    "decision_at": decision_at,
                    "requested_bars": row["requested_bars"],
                    "reference_window_bars": reference["requested_bars"],
                    "changed_decision_fields": decision_fields,
                    "intermediate_state_changed_fields": state_fields,
                }
                divergences.append(divergence)
                if first_divergence is None:
                    first_divergence = divergence

    return {
        "status": "startup_sensitive" if first_divergence else "no_change_detected",
        "decision_points_available": available_points,
        "decision_points_checked": checked_points,
        "decision_points_sampled": checked_points < eligible_points,
        "decision_points_eligible": eligible_points,
        "decision_points_excluded_insufficient_reference_history": excluded_points,
        "worker_replays": worker_replays,
        "changed_windows_bars": sorted(changed_fields_by_window),
        "changed_fields_by_window": {
            str(window): sorted(fields) for window, fields in sorted(changed_fields_by_window.items())
        },
        "intermediate_state_changed_windows_bars": sorted(state_fields_by_window),
        "intermediate_state_changed_fields_by_window": {
            str(window): sorted(fields) for window, fields in sorted(state_fields_by_window.items())
        },
        "first_divergence": first_divergence,
        "divergences": divergences,
    }


def _validate_pit_ledger(ledger: dict[str, Any]) -> tuple[list[dict[str, Any]], str]:
    """Validate a versioned, provider-neutral point-in-time fact history."""
    allowed = {"schema_version", "kind", "source", "facts", "price_adjustment_basis"}
    unknown = sorted(str(key) for key in ledger if key not in allowed)
    if unknown:
        raise ValueError(f"PIT ledger contains unknown key(s): {', '.join(unknown)}")
    if (
        type(ledger.get("schema_version")) is not int
        or ledger.get("schema_version") != 1
        or ledger.get("kind") != "qianxing-pit-ledger"
    ):
        raise ValueError("PIT ledger must use schema_version 1 and kind qianxing-pit-ledger")
    source = ledger.get("source")
    if not isinstance(source, str) or not source.strip():
        raise ValueError("PIT ledger source must be non-empty")
    adjustment_basis = ledger.get("price_adjustment_basis", "unknown")
    if not isinstance(adjustment_basis, str) or adjustment_basis not in PRICE_ADJUSTMENT_BASES:
        raise ValueError(
            "PIT ledger price_adjustment_basis must be raw, forward_adjusted, "
            "backward_adjusted, or unknown"
        )
    facts = ledger.get("facts")
    if not isinstance(facts, list) or not facts:
        raise ValueError("PIT ledger facts must be a non-empty array")
    if len(facts) > MAX_PIT_FACTS:
        raise ValueError(f"PIT ledger exceeds the {MAX_PIT_FACTS} fact limit")

    fact_keys = {"fact_id", "entity_id", "field", "effective_at", "versions"}
    version_keys = {"available_at", "revision", "value_fingerprint"}
    seen_fact_ids: set[str] = set()
    normalized: list[dict[str, Any]] = []
    for index, fact in enumerate(facts):
        if not isinstance(fact, dict):
            raise ValueError(f"PIT ledger facts[{index}] must be an object")
        extra = sorted(str(key) for key in fact if key not in fact_keys)
        missing = sorted(fact_keys.difference(fact))
        if extra or missing:
            detail = []
            if missing:
                detail.append(f"missing {', '.join(missing)}")
            if extra:
                detail.append(f"unknown {', '.join(extra)}")
            raise ValueError(f"PIT ledger facts[{index}] has {'; '.join(detail)}")
        fact_id, entity_id, field_name = fact["fact_id"], fact["entity_id"], fact["field"]
        for value, name in ((fact_id, "fact_id"), (entity_id, "entity_id"), (field_name, "field")):
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"PIT ledger facts[{index}].{name} must be non-empty")
        if fact_id in seen_fact_ids:
            raise ValueError(f"PIT ledger contains duplicate fact_id {fact_id!r}")
        seen_fact_ids.add(fact_id)
        effective_at = fact["effective_at"]
        if type(effective_at) is not int or effective_at <= 0:
            raise ValueError(f"PIT ledger facts[{index}].effective_at must be a positive integer timestamp")
        versions = fact["versions"]
        if not isinstance(versions, list) or not versions:
            raise ValueError(f"PIT ledger facts[{index}].versions must be a non-empty array")
        if len(versions) > MAX_PIT_VERSIONS_PER_FACT:
            raise ValueError(
                f"PIT ledger facts[{index}] exceeds the {MAX_PIT_VERSIONS_PER_FACT} version limit"
            )
        normalized_versions: list[dict[str, Any]] = []
        seen_revisions: set[str] = set()
        previous_known_at = 0
        for version_index, version in enumerate(versions):
            if not isinstance(version, dict):
                raise ValueError(f"PIT ledger facts[{index}].versions[{version_index}] must be an object")
            extra_version_keys = sorted(str(key) for key in version if key not in version_keys)
            missing_version_keys = sorted(version_keys.difference(version))
            if extra_version_keys or missing_version_keys:
                detail = []
                if missing_version_keys:
                    detail.append(f"missing {', '.join(missing_version_keys)}")
                if extra_version_keys:
                    detail.append(f"unknown {', '.join(extra_version_keys)}")
                raise ValueError(
                    f"PIT ledger facts[{index}].versions[{version_index}] has {'; '.join(detail)}"
                )
            available_at = version["available_at"]
            if available_at is not None and (type(available_at) is not int or available_at <= 0):
                raise ValueError(
                    f"PIT ledger facts[{index}].versions[{version_index}].available_at "
                    "must be null or a positive integer timestamp"
                )
            revision = version["revision"]
            value_fingerprint = version["value_fingerprint"]
            for value, name in ((revision, "revision"), (value_fingerprint, "value_fingerprint")):
                if not isinstance(value, str) or not value.strip():
                    raise ValueError(
                        f"PIT ledger facts[{index}].versions[{version_index}].{name} must be non-empty"
                    )
            if revision in seen_revisions:
                raise ValueError(f"PIT ledger facts[{index}] contains duplicate revision {revision!r}")
            seen_revisions.add(revision)
            if available_at is not None:
                if available_at <= previous_known_at:
                    raise ValueError(
                        f"PIT ledger facts[{index}].versions availability timestamps "
                        "must be strictly increasing"
                    )
                previous_known_at = available_at
            normalized_versions.append(
                {
                    "available_at": available_at,
                    "revision": revision,
                    "value_fingerprint": value_fingerprint,
                }
            )
        normalized.append(
            {
                "fact_id": fact_id,
                "entity_id": entity_id,
                "field": field_name,
                "effective_at": effective_at,
                "versions": normalized_versions,
            }
        )
    canonical = json.dumps(ledger, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return normalized, hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _adjustment_chain_check(
    facts: list[dict[str, Any]],
    decision_times: list[int],
    adjustment_basis: str,
) -> dict[str, Any]:
    """Correlate historical price revisions with later-effective corporate actions.

    The relation is intentionally reported as temporal correlation, never as proof that an
    action caused the revision. A declared backward-adjusted basis plus a price revision that
    spans pre-action decisions is stronger evidence of a possible lookahead path.
    """
    actions_by_entity: dict[str, list[dict[str, Any]]] = {}
    action_availability: dict[str, tuple[int | None, bool]] = {}
    price_facts: list[dict[str, Any]] = []
    for fact in facts:
        if fact["field"].startswith("corporate_action."):
            actions_by_entity.setdefault(fact["entity_id"], []).append(fact)
            known_times = [
                version["available_at"] for version in fact["versions"]
                if version["available_at"] is not None
            ]
            action_availability[fact["fact_id"]] = (
                min(known_times) if known_times else None,
                any(version["available_at"] is None for version in fact["versions"]),
            )
        elif fact["field"] in PRICE_BAR_FIELDS:
            price_facts.append(fact)
    for actions in actions_by_entity.values():
        actions.sort(key=lambda fact: (fact["effective_at"], fact["fact_id"]))

    pair_checks = 0
    price_revisions = 0
    candidate_count = 0
    back_adjustment_candidate_count = 0
    examples: list[dict[str, Any]] = []
    for bar_fact in price_facts:
        versions = bar_fact["versions"]
        if len(versions) < 2:
            continue
        actions = actions_by_entity.get(bar_fact["entity_id"], [])
        action_times = [action["effective_at"] for action in actions]
        previous = versions[0]
        for revision in versions[1:]:
            changed = revision["value_fingerprint"] != previous["value_fingerprint"]
            previous = revision
            if not changed:
                continue
            price_revisions += 1
            revised_at = revision["available_at"]
            if revised_at is None or not actions:
                continue
            affected_decisions = (
                bisect_left(decision_times, revised_at)
                - bisect_right(decision_times, bar_fact["effective_at"])
            )
            if affected_decisions <= 0:
                continue
            first_action = bisect_right(action_times, bar_fact["effective_at"])
            after_revision = bisect_right(action_times, revised_at)
            for action in actions[first_action:after_revision]:
                pair_checks += 1
                if pair_checks > MAX_ADJUSTMENT_RELATION_CHECKS:
                    raise ValueError(
                        "corporate-action adjustment correlation exceeds the diagnostic limit; "
                        "reduce PIT facts or decision points"
                    )
                first_known_at, unknown_action_time = action_availability[action["fact_id"]]
                known_by_revision = first_known_at is not None and first_known_at <= revised_at
                if not known_by_revision and not unknown_action_time:
                    continue
                action_decisions = (
                    bisect_left(decision_times, action["effective_at"])
                    - bisect_right(decision_times, bar_fact["effective_at"])
                )
                pre_action_points = max(0, min(affected_decisions, action_decisions))
                known_action_decisions = 0
                if first_known_at is not None:
                    known_start = max(
                        bisect_right(decision_times, bar_fact["effective_at"]),
                        bisect_left(decision_times, first_known_at),
                    )
                    known_end = bisect_left(
                        decision_times, min(action["effective_at"], revised_at)
                    )
                    known_action_decisions = max(0, known_end - known_start)
                candidate = {
                        "price_fact_id": bar_fact["fact_id"],
                        "price_entity_id": bar_fact["entity_id"],
                        "price_field": bar_fact["field"],
                        "price_effective_at": bar_fact["effective_at"],
                        "price_revision": revision["revision"],
                        "price_revision_available_at": revised_at,
                        "action_fact_id": action["fact_id"],
                        "action_field": action["field"],
                        "action_effective_at": action["effective_at"],
                        "price_value_changed": True,
                        "affected_decision_points": affected_decisions,
                        "pre_action_decision_points": pre_action_points,
                        "action_known_at_pre_action_decisions": known_action_decisions,
                        "action_availability_unknown": unknown_action_time,
                        "action_available_by_price_revision": known_by_revision,
                        "causal_attribution": "temporal_correlation_only",
                    }
                candidate_count += 1
                potential_back_adjustment = (
                    adjustment_basis == "backward_adjusted"
                    and pre_action_points > known_action_decisions
                    and known_by_revision
                    and not unknown_action_time
                )
                if potential_back_adjustment:
                    back_adjustment_candidate_count += 1
                if len(examples) < MAX_PIT_EXAMPLES_PER_STATUS:
                    candidate["potential_back_adjustment_lookahead"] = potential_back_adjustment
                    examples.append(candidate)

    has_actions = any(actions_by_entity.values())
    if not has_actions:
        status = "corporate_action_history_missing"
    elif not price_revisions:
        status = "price_revision_history_missing"
    elif back_adjustment_candidate_count:
        status = "potential_back_adjustment_lookahead"
    elif candidate_count:
        status = "historical_price_revision_correlates_with_action"
    elif adjustment_basis == "unknown":
        status = "adjustment_basis_unknown"
    else:
        status = "no_linked_adjustment_revision_detected"
    return {
        "status": status,
        "price_adjustment_basis": adjustment_basis,
        "basis_declared": adjustment_basis != "unknown",
        "correlation_only": True,
        "price_revision_count": price_revisions,
        "action_price_pairs_checked": pair_checks,
        "candidate_count": candidate_count,
        "potential_back_adjustment_lookahead_count": back_adjustment_candidate_count,
        "examples": examples,
        "interpretation": (
            "A historical price value changed after a later-effective corporate-action fact. "
            "This is a time-linked candidate, not proof that the action caused the adjustment. "
            "Without authoritative provider adjustment policy and publication timestamps, the "
            "chain cannot be declared point-in-time safe."
        ),
    }


def _pit_visibility_check(
    request: StrategyInput,
    ledger: dict[str, Any],
    max_decision_points: int,
) -> dict[str, Any]:
    facts, ledger_fingerprint = _validate_pit_ledger(ledger)
    assert request.bars is not None
    visible_times = [timestamp for timestamp in request.bars.ts if timestamp < request.as_of]
    if len(visible_times) > max_decision_points:
        stride = math.ceil(len(visible_times) / max_decision_points)
        checked_times = visible_times[::stride][:max_decision_points]
        if checked_times[-1] != visible_times[-1]:
            checked_times[-1] = visible_times[-1]
    else:
        checked_times = visible_times
    checks = len(checked_times) * len(facts)
    if checks > MAX_PIT_FACT_DECISION_CHECKS:
        raise ValueError(
            "PIT ledger workload exceeds the diagnostic limit; reduce facts or use fewer decision points"
        )

    counts = {
        "future_value_unavailable": 0,
        "revised_value_after_decision": 0,
        "availability_unknown": 0,
    }
    examples: dict[str, list[dict[str, Any]]] = {key: [] for key in counts}
    affected_fact_ids: dict[str, set[str]] = {key: set() for key in counts}
    for fact in facts:
        versions = fact["versions"]
        unknown_availability = any(version["available_at"] is None for version in versions)
        known_versions = [version for version in versions if version["available_at"] is not None]
        known_times = [version["available_at"] for version in known_versions]
        final = versions[-1]
        for decision_time in checked_times:
            if unknown_availability:
                status = "availability_unknown"
            else:
                available_index = bisect_right(known_times, decision_time)
                if not available_index:
                    status = "future_value_unavailable"
                elif (
                    final["available_at"] > decision_time
                    and final["value_fingerprint"] != known_versions[available_index - 1]["value_fingerprint"]
                ):
                    status = "revised_value_after_decision"
                else:
                    continue
            counts[status] += 1
            affected_fact_ids[status].add(fact["fact_id"])
            if len(examples[status]) < MAX_PIT_EXAMPLES_PER_STATUS:
                examples[status].append(
                    {
                        "fact_id": fact["fact_id"],
                        "entity_id": fact["entity_id"],
                        "field": fact["field"],
                        "decision_at": decision_time,
                        "effective_at": fact["effective_at"],
                        "version_count": len(versions),
                        "available_at": [version["available_at"] for version in versions],
                    }
                )
    risk_count = counts["future_value_unavailable"] + counts["revised_value_after_decision"]
    status = (
        "potential_pit_bias_detected" if risk_count else
        "visibility_unknown" if counts["availability_unknown"] else
        "no_pit_bias_detected"
    )
    return {
        "status": status,
        "ledger_source": ledger["source"],
        "ledger_fingerprint": ledger_fingerprint,
        "fact_count": len(facts),
        "decision_points_available": len(visible_times),
        "decision_points_checked": len(checked_times),
        "decision_points_sampled": len(checked_times) < len(visible_times),
        "fact_decision_checks": checks,
        "affected_fact_counts": {
            key: len(value) for key, value in affected_fact_ids.items()
        },
        "affected_fact_decision_counts": counts,
        "examples": examples,
        "adjustment_chain": _adjustment_chain_check(
            facts,
            checked_times,
            ledger.get("price_adjustment_basis", "unknown"),
        ),
        "interpretation": (
            "The final data snapshot contains values that were unavailable or materially revised "
            "after one or more historical decision times. Rebuild each decision from the latest "
            "version available by that time. Null availability timestamps remain unknown and do "
            "not count as evidence of safety."
        ),
    }


def diagnose(
    request: StrategyInput,
    strategy: str,
    startup_windows: list[int],
    max_checks: int = DEFAULT_MAX_LOOKAHEAD_CHECKS,
    timeout_seconds: int = DEFAULT_HANDLER_TIMEOUT_SECONDS,
    worker_command: Sequence[str] | None = None,
    pit_ledger: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if not worker_command and not (strategy or "").strip():
        raise ValueError("strategy module or .py path must be non-empty")
    if not startup_windows or any(window <= 0 for window in startup_windows):
        raise ValueError("startup windows must be positive integers")
    if len(startup_windows) != len(set(startup_windows)):
        raise ValueError("startup windows must be unique")
    if not 0 < max_checks <= MAX_LOOKAHEAD_CHECKS:
        raise ValueError(f"max checks must be in 1..={MAX_LOOKAHEAD_CHECKS}")
    if not 0 < timeout_seconds <= MAX_HANDLER_TIMEOUT_SECONDS:
        raise ValueError(f"timeout must be in 1..={MAX_HANDLER_TIMEOUT_SECONDS} seconds")
    if request.bars is None:
        raise ValueError("diagnostics require a bar input")
    visible_count = sum(ts < request.as_of for ts in request.bars.ts)
    if visible_count == 0:
        raise ValueError("diagnostics require at least one completed bar earlier than as_of")
    startup_windows = sorted(startup_windows)
    data_visibility = (
        None if pit_ledger is None else _pit_visibility_check(request, pit_ledger, max_checks)
    )
    report = {
        "schema_version": 1,
        "kind": "qianxing-strategy-bias-diagnostics",
        "strategy": strategy if not worker_command else None,
        "worker_kind": "external_jsonl" if worker_command else "python_module",
        "strategy_id": request.strategy_id,
        "strategy_version": request.strategy_version,
        "data_fingerprint": request.data_fingerprint,
        "instrument": request.instrument,
        "as_of": request.as_of,
        "input_bars": len(request.bars.ts),
        "lookahead": _lookahead_check(request, strategy, max_checks, timeout_seconds, worker_command),
        "visible_bars_at_as_of": visible_count,
        "startup": _startup_check(
            request, strategy, startup_windows, max_checks, timeout_seconds, worker_command
        ),
    }
    if data_visibility is not None:
        report["data_visibility"] = data_visibility
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path, help="versioned StrategyInput JSON")
    parser.add_argument(
        "--pit-ledger", type=Path,
        help="optional versioned PIT fact history with effective_at, available_at and value fingerprints",
    )
    parser.add_argument("--strategy", help="Python module or strategy .py file")
    parser.add_argument("--worker-executable", type=Path, help="external JSONL strategy worker executable (for example a C++ worker)")
    parser.add_argument("--worker-arg", action="append", default=[], help="argument passed to --worker-executable; repeat as needed")
    parser.add_argument(
        "--startup-windows",
        default="20,50,100",
        help="comma-separated warmup history lengths; the largest is the reference",
    )
    parser.add_argument("--max-lookahead-checks", type=int, default=DEFAULT_MAX_LOOKAHEAD_CHECKS)
    parser.add_argument("--timeout-seconds", type=int, default=DEFAULT_HANDLER_TIMEOUT_SECONDS)
    parser.add_argument("--out", type=Path, help="optional JSON report path")
    args = parser.parse_args()
    try:
        if bool(args.strategy) == bool(args.worker_executable):
            raise ValueError("provide exactly one of --strategy or --worker-executable")
        worker_command = None
        if args.worker_executable:
            if not args.worker_executable.is_file():
                raise ValueError(f"worker executable does not exist: {args.worker_executable}")
            worker_command = [str(args.worker_executable), *args.worker_arg]
        raw = json.loads(args.input.read_text(encoding="utf-8"))
        if not isinstance(raw, dict):
            raise ValueError("input JSON must be an object")
        request = StrategyInput.from_dict(raw)
        pit_ledger = None
        if args.pit_ledger:
            ledger_value = json.loads(args.pit_ledger.read_text(encoding="utf-8"))
            if not isinstance(ledger_value, dict):
                raise ValueError("PIT ledger JSON must be an object")
            pit_ledger = ledger_value
        windows = [int(value.strip()) for value in args.startup_windows.split(",")]
        report = diagnose(
            request,
            args.strategy,
            windows,
            max_checks=args.max_lookahead_checks,
            timeout_seconds=args.timeout_seconds,
            worker_command=worker_command,
            pit_ledger=pit_ledger,
        )
        encoded = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
        if args.out:
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.write_text(encoded, encoding="utf-8")
        print(encoded, end="")
        return 0
    except (OSError, json.JSONDecodeError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(2, f"strategy diagnostics failed: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
