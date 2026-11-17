"""Application service for executing a Python strategy against a local backtest spec."""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
from typing import Any, Callable

from qianxing_strategy.base import bind_strategy_warmup

from .app import (
    InvalidInputError,
    run_backtest_with_strategy,
    run_depth_backtest_with_strategy,
    run_multi_market_backtest_with_strategy_slots,
    start_backtest_with_strategy,
    start_depth_backtest_with_strategy,
    start_multi_market_backtest_with_strategy_slots,
    validate_strategy_execution_preflight_v1,
)
from .project_runs import prepare_project_input, wait_project_run
from .strategy_project import (
    apply_parameter_defaults,
    build_module_manifest,
    load_strategy_file,
    strategy_config_targets,
    validate_strategy_config,
)


# Snapshot references belong to preflight, not the strict engine spec.
_PROJECT_EXECUTION_ONLY_KEYS = ("dataset_snapshot_path",)


def _drop_project_execution_keys(payload: dict[str, Any]) -> dict[str, Any]:
    value = {
        key: item for key, item in payload.items() if key not in _PROJECT_EXECUTION_ONLY_KEYS
    }
    for container in ("base",):
        if isinstance(value.get(container), dict):
            value[container] = _drop_project_execution_keys(value[container])
    markets = value.get("markets")
    if isinstance(markets, list):
        value["markets"] = [
            _drop_project_execution_keys(market) if isinstance(market, dict) else market
            for market in markets
        ]
    return value


def _strategy_identity(
    module: object,
    strategy_file: str | Path,
    payload: Mapping[str, Any],
    manifest: Mapping[str, Any],
    *,
    strategy_id: str | None,
    strategy_version: str | None,
) -> tuple[str, str]:
    path = Path(strategy_file)
    base = payload.get("base") if isinstance(payload.get("base"), Mapping) else payload
    strategy = base.get("strategy") if isinstance(base.get("strategy"), Mapping) else {}
    configured_id = payload.get("target_strategy_id") or strategy.get("strategy_id")
    resolved_id = (
        strategy_id
        or configured_id
        or getattr(module, "STRATEGY_ID", None)
        or manifest.get("strategy_id")
        or path.stem
    )
    if configured_id and str(resolved_id) != str(configured_id):
        message = (
            "strategy ID must match the primary strategy slot in the spec "
            f"({configured_id!r})"
        )
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    resolved_version = (
        strategy_version
        or getattr(module, "STRATEGY_VERSION", None)
        or manifest.get("strategy_version")
        or "local"
    )
    return str(resolved_id), str(resolved_version)


def _bind_manifest_config_factory(
    factory: Callable[..., object],
    module: object,
    manifest: Mapping[str, Any],
    strategy_id: str | None,
) -> Callable[..., object]:
    def create(spec: Mapping[str, Any], *args: object, **kwargs: object) -> object:
        if kwargs:
            raise ValueError("create_strategy(spec) does not accept keyword arguments")
        if len(args) > 1 or (args and not isinstance(args[0], Mapping)):
            raise ValueError("candidate parameters must be provided as one mapping")
        effective_id = strategy_id
        if effective_id is None:
            selected = spec.get("strategy")
            selected_id = selected.get("strategy_id") if isinstance(selected, Mapping) else None
            if isinstance(selected_id, str) and selected_id.strip():
                effective_id = selected_id
        bound_spec = apply_parameter_defaults(
            spec,
            manifest,
            target_strategy_id=effective_id,
            candidate_parameters=args[0] if args else None,
        )
        validate_strategy_config(module, manifest, bound_spec, effective_id)
        return factory(bound_spec)

    return create


def _bind_warmup_factory(
    factory: Callable[..., object], warmup_bars: int
) -> Callable[..., object]:
    def create(*args: object, **kwargs: object) -> object:
        strategy = factory(*args, **kwargs)
        if strategy is None:
            return None
        return bind_strategy_warmup(
            strategy, warmup_bars, defer_history_requirements=True
        )

    return create


def _multi_market_bindings(
    payload: dict[str, Any],
    factory: Callable[[dict[str, Any]], object],
    *,
    strategy_id: str | None,
    strategy_version: str,
    source_sha256: str,
) -> list[dict[str, object]]:
    markets = payload.get("markets")
    if not isinstance(markets, list) or not markets:
        message = "multi-market strategy input must contain a non-empty markets array"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    slots: list[tuple[str, dict, dict]] = []
    seen: set[str] = set()
    for market_index, market in enumerate(markets):
        if not isinstance(market, dict) or not isinstance(market.get("strategies"), list):
            message = f"markets[{market_index}].strategies must be an array"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        for slot_index, slot in enumerate(market["strategies"]):
            strategy = slot.get("strategy") if isinstance(slot, dict) else None
            slot_id = strategy.get("strategy_id") if isinstance(strategy, dict) else None
            if not isinstance(slot_id, str) or not slot_id.strip():
                message = f"markets[{market_index}].strategies[{slot_index}] must declare strategy.strategy_id"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            if slot_id in seen:
                message = f"multi-market strategy slot ID is duplicated: {slot_id}"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            seen.add(slot_id)
            slots.append((slot_id, market, slot))
    selected = [slot for slot in slots if strategy_id is None or slot[0] == strategy_id]
    if not selected:
        message = (
            f"strategy ID {strategy_id!r} does not match a declared multi-market strategy slot"
            if strategy_id
            else "multi-market spec contains no strategy slots"
        )
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    bindings = []
    for slot_id, market, slot in selected:
        factory_payload = {
            **payload,
            "market": market,
            "strategy": slot["strategy"],
            "strategy_slot": slot,
        }
        try:
            strategy = factory(factory_payload)
        except Exception as exc:
            message = f"create_strategy(spec) failed for slot {slot_id!r}: {type(exc).__name__}: {exc}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
        if strategy is None:
            message = f"create_strategy(spec) returned None for slot {slot_id!r}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        bindings.append({
            "strategy_id": slot_id,
            "strategy_version": strategy_version,
            "source_sha256": source_sha256,
            "strategy": strategy,
        })
    return bindings


def run_strategy_file(
    payload: object,
    strategy_file: str | Path,
    *,
    strategy_id: str | None = None,
    strategy_version: str | None = None,
    preflight: bool = True,
    expected_bundle_sha256: str | None = None,
    _loaded_strategy: tuple[Any, Callable[..., Any], str] | None = None,
) -> Any:
    """Load, validate, instantiate, and run a Python strategy using shared Rust use cases."""
    if not isinstance(payload, dict):
        message = "strategy input JSON must be an object"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    module, factory, source_sha256 = _loaded_strategy or load_strategy_file(strategy_file)
    if expected_bundle_sha256 and source_sha256 != expected_bundle_sha256:
        message = "strategy bundle changed after project preflight"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    try:
        strategy_manifest = build_module_manifest(
            module, Path(strategy_file).expanduser().resolve(), source_sha256
        )
        selected_id = strategy_id
        if selected_id is None and not isinstance(payload.get("markets"), list):
            selected_id = strategy_manifest.get("strategy_id")
        payload = apply_parameter_defaults(
            payload, strategy_manifest, target_strategy_id=selected_id
        )
        for target_id in strategy_config_targets(payload, selected_id):
            validate_strategy_config(module, strategy_manifest, payload, target_id)
        factory = _bind_manifest_config_factory(
            factory, module, strategy_manifest, selected_id
        )
        if preflight:
            validate_strategy_execution_preflight_v1(
                strategy_manifest,
                payload,
                purpose="run",
                target_strategy_id=selected_id,
            )
        if strategy_manifest.get("warmup_bars") is not None:
            factory = _bind_warmup_factory(factory, strategy_manifest["warmup_bars"])
    except (OSError, ValueError) as exc:
        raise InvalidInputError(
            f"strategy manifest preflight failed: {exc}",
            payload={"category": "INVALID_INPUT", "message": str(exc)},
        ) from exc

    payload = prepare_project_input(_drop_project_execution_keys(payload))

    is_depth_spec = (
        payload.get("schema_version") == 1
        and isinstance(payload.get("depth_path"), str)
        and payload.get("tier") in {"l1", "l2"}
    )
    if is_depth_spec:
        expected_level = "tick" if payload["tier"] == "l1" else "orderbook"
        declared_level = strategy_manifest.get("data_level")
        if declared_level not in (None, expected_level):
            message = (
                f"strategy manifest data_level={declared_level!r} does not match "
                f"depth tier={payload['tier']!r} (expected {expected_level!r})"
            )
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        resolved_id, resolved_version = _strategy_identity(
            module, strategy_file, payload, strategy_manifest,
            strategy_id=strategy_id, strategy_version=strategy_version,
        )
        try:
            strategy = factory(payload)
        except Exception as exc:
            message = f"create_strategy(spec) failed: {type(exc).__name__}: {exc}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
        if strategy is None:
            message = "create_strategy(spec) returned None"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        return wait_project_run(
            start_depth_backtest_with_strategy,
            run_depth_backtest_with_strategy,
            payload,
            strategy,
            strategy_id=resolved_id,
            strategy_version=resolved_version,
            source_sha256=source_sha256,
        )
    if isinstance(payload.get("markets"), list):
        resolved_version = (
            strategy_version
            or getattr(module, "STRATEGY_VERSION", None)
            or strategy_manifest.get("strategy_version")
            or "local"
        )
        bindings = _multi_market_bindings(
            payload,
            factory,
            strategy_id=strategy_id,
            strategy_version=str(resolved_version),
            source_sha256=source_sha256,
        )
        return wait_project_run(
            start_multi_market_backtest_with_strategy_slots,
            run_multi_market_backtest_with_strategy_slots,
            payload,
            bindings,
        )
    resolved_id, resolved_version = _strategy_identity(
        module, strategy_file, payload, strategy_manifest,
        strategy_id=strategy_id, strategy_version=strategy_version,
    )
    try:
        strategy = factory(payload)
    except Exception as exc:
        message = f"create_strategy(spec) failed: {type(exc).__name__}: {exc}"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
    if strategy is None:
        message = "create_strategy(spec) returned None"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    return wait_project_run(
        start_backtest_with_strategy,
        run_backtest_with_strategy,
        payload,
        strategy,
        strategy_id=resolved_id,
        strategy_version=resolved_version,
        source_sha256=source_sha256,
    )


def run_project_strategy(
    root: Path,
    manifest: Mapping[str, Any],
    input_path: Path,
    run_plan: Mapping[str, Any],
) -> Any:
    """Execute a project snapshot after its immutable local preflight has succeeded."""
    from .project_runs import record_project_result
    from .strategy_project import _json_object

    payload = _json_object(input_path, "strategy input spec")
    result = run_strategy_file(
        payload,
        run_plan["strategy_file"],
        strategy_id=manifest.get("strategy_id"),
        strategy_version=manifest.get("strategy_version"),
        preflight=False,
        expected_bundle_sha256=run_plan.get("strategy_bundle_sha256"),
    )
    from dataclasses import asdict, is_dataclass

    record_project_result(asdict(result) if is_dataclass(result) else result)
    return result
