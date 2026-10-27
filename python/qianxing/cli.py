"""Console entry point for the Python SDK's shared Rust application workflows."""

from __future__ import annotations

import argparse
import copy
import hashlib
import inspect
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import asdict, is_dataclass
from pathlib import Path, PurePosixPath
from types import MappingProxyType
from typing import Any, Callable, Mapping, Sequence

from .app import (
    DataUnavailableError,
    InvalidInputError,
    QianxingError,
    compare_runs,
    doctor,
    run_backtest,
    run_backtest_with_strategy,
    run_depth_backtest,
    run_depth_backtest_with_strategy,
    run_depth_experiment,
    run_depth_experiment_with_strategy,
    run_experiment,
    run_experiment_with_strategy,
    _prepare_quick_research_spec as prepare_quick_research_spec,
    run_multi_leg_experiment,
    run_multi_market_backtest_with_strategy_slots,
    run_multi_market_experiment,
    run_multi_market_experiment_with_strategy,
    read_run_report_v1,
    validate_dataset,
    validate_strategy_execution_preflight_v1,
    verify_run,
    verify_depth_run,
)
from .reports.experiment_html import (
    load_verified_candidate_details,
    load_verified_multi_market_candidate_details,
    write_experiment_html,
)
from .strategy_manifest import (
    apply_parameter_defaults,
    build_module_manifest,
    declared_config_validator,
    resolve_strategy_parameters,
)
from qianxing_strategy.base import bind_strategy_warmup


def _document(value: object) -> object:
    return asdict(value) if is_dataclass(value) else value


def _referenced_input_paths(value: object) -> list[str]:
    """Collect common local data-file references from a versioned input document."""
    path_keys = {
        "path",
        "file",
        "file_path",
        "input_path",
        "source_path",
        "data_path",
        "bars_path",
        "ticks_path",
        "trades_path",
        "book_path",
        "csv_path",
    }
    found: list[str] = []

    def visit(item: object) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                if key in path_keys and isinstance(child, str):
                    found.append(child)
                else:
                    visit(child)
        elif isinstance(item, (list, tuple)):
            for child in item:
                visit(child)

    visit(value)
    return found


def _strategy_diagnostic_request(
    spec: dict[str, object], strategy_id: str, strategy_version: str
):
    from qianxing_bridge.strategy import StrategyInput

    bars_path = spec.get("bars_path")
    if not isinstance(bars_path, str) or not bars_path.strip():
        raise InvalidInputError(
            "diagnose-strategy requires BacktestSpec.bars_path",
            payload={"category": "INVALID_INPUT", "message": "diagnose-strategy requires BacktestSpec.bars_path"},
        )
    path = Path(bars_path).expanduser().resolve()
    try:
        frame_bytes = path.read_bytes()
        frame = json.loads(frame_bytes)
    except OSError as exc:
        message = f"cannot read diagnostic BarFrame {str(path)!r}: {exc}"
        raise DataUnavailableError(
            message, payload={"category": "DATA_UNAVAILABLE", "message": message}
        ) from exc
    except json.JSONDecodeError as exc:
        message = f"diagnostic BarFrame JSON is invalid: {exc}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        ) from exc
    if not isinstance(frame, dict):
        message = "diagnostic BarFrame must be a JSON object"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    instrument = spec.get("instrument")
    if frame.get("instrument") != instrument:
        message = "BarFrame.instrument must match BacktestSpec.instrument"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    timestamps = frame.get("ts")
    if not isinstance(timestamps, list) or not timestamps or any(type(ts) is not int or ts <= 0 for ts in timestamps):
        message = "diagnostic BarFrame.ts must contain positive integer timestamps"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    from qianxing_strategy.diagnostics import MAX_CALLBACK_DIAGNOSTIC_BARS

    if len(timestamps) > MAX_CALLBACK_DIAGNOSTIC_BARS:
        message = f"callback diagnostics accept at most {MAX_CALLBACK_DIAGNOSTIC_BARS} bars"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    if timestamps[-1] >= (1 << 64) - 1:
        message = "last Bar timestamp leaves no representable diagnostic as_of boundary"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    verdict = validate_dataset(
        {
            "schema_version": 1,
            "dataset_id": str(frame.get("source") or path.stem),
            "bars_path": str(path),
        }
    )
    if not verdict.usable or verdict.instrument != instrument or verdict.rows != len(timestamps):
        message = "BarFrame failed Rust dataset validation or does not match the BacktestSpec"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    settlement = spec.get("settlement_currency")
    initial_cash = spec.get("initial_cash_raw")
    if not isinstance(settlement, str) or not settlement.strip() or type(initial_cash) is not int:
        message = "BacktestSpec settlement_currency and initial_cash_raw are required for diagnostics"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    return StrategyInput.from_dict(
        {
            "schema_version": 1,
            "request_id": f"{spec.get('run_id') or 'backtest'}:strategy-diagnostic",
            "strategy_id": strategy_id,
            "strategy_version": strategy_version,
            "data_fingerprint": verdict.content_fingerprint,
            "as_of": timestamps[-1] + 1,
            "instrument": instrument,
            "positions": {},
            "cash": {settlement: initial_cash},
            "available_margin_raw": None,
            "risk_state": "diagnostic_fixed_initial_context",
            "research_targets": {},
            # Dataset BarFrames carry identity metadata alongside OHLCV columns.
            # StrategyInput.bars is a separate strict wire contract, so pass only
            # its declared fields after the dataset itself has been validated.
            "bars": {key: value for key, value in frame.items() if key != "instrument"},
        }
    )


def _multi_market_diagnostic_request(
    spec: dict[str, object], strategy_id: str, strategy_version: str
):
    """Build one strategy-slot diagnostic input with Rust-validated peer BarFrames."""
    from qianxing_bridge.strategy import StrategyInput, StrategyBars
    from qianxing_strategy.diagnostics import MAX_CALLBACK_DIAGNOSTIC_BARS

    markets = spec.get("markets")
    if not isinstance(markets, list) or not markets:
        raise InvalidInputError("multi-market diagnostics require a non-empty markets array")
    settlement = spec.get("settlement_currency")
    initial_cash = spec.get("initial_cash_raw")
    if not isinstance(settlement, str) or not settlement.strip() or type(initial_cash) is not int:
        raise InvalidInputError(
            "multi-market diagnostics require settlement_currency and initial_cash_raw"
        )

    target: tuple[dict, dict] | None = None
    seen_slots: set[str] = set()
    for market_index, market in enumerate(markets):
        if not isinstance(market, dict) or not isinstance(market.get("strategies"), list):
            raise InvalidInputError(f"markets[{market_index}].strategies must be an array")
        for slot in market["strategies"]:
            strategy = slot.get("strategy") if isinstance(slot, dict) else None
            slot_id = strategy.get("strategy_id") if isinstance(strategy, dict) else None
            if not isinstance(slot_id, str) or not slot_id.strip() or slot_id in seen_slots:
                raise InvalidInputError("multi-market strategy slots need unique strategy IDs")
            seen_slots.add(slot_id)
            if slot_id == strategy_id:
                target = (market, slot)
    if target is None:
        raise InvalidInputError(
            f"strategy ID {strategy_id!r} does not match a declared multi-market strategy slot"
        )

    target_market, target_slot = target
    target_instrument = target_market.get("instrument")
    if not isinstance(target_instrument, str) or not target_instrument.strip():
        raise InvalidInputError("target market instrument must be a non-empty string")
    bars_path = target_market.get("bars_path")
    if not isinstance(bars_path, str) or not bars_path.strip():
        raise InvalidInputError("target market requires bars_path")
    request = _strategy_diagnostic_request(
        {
            "run_id": spec.get("run_id"),
            "instrument": target_instrument,
            "bars_path": bars_path,
            "settlement_currency": settlement,
            "initial_cash_raw": initial_cash,
            "account_id": spec.get("account_id"),
            "strategy": target_slot.get("strategy"),
        },
        strategy_id,
        strategy_version,
    )

    market_bars: dict[str, StrategyBars] = {}
    market_inputs: list[dict[str, object]] = []
    target_as_of = request.as_of
    total_rows = 0
    for market_index, market in enumerate(markets):
        instrument = market.get("instrument") if isinstance(market, dict) else None
        path_value = market.get("bars_path") if isinstance(market, dict) else None
        if not isinstance(instrument, str) or not instrument.strip():
            raise InvalidInputError(f"markets[{market_index}].instrument must be a non-empty string")
        if not isinstance(path_value, str) or not path_value.strip():
            raise InvalidInputError(f"markets[{market_index}].bars_path must be a non-empty string")
        path = Path(path_value).expanduser().resolve()
        try:
            frame = json.loads(path.read_text(encoding="utf-8"))
        except OSError as exc:
            raise DataUnavailableError(f"cannot read BarFrame {str(path)!r}: {exc}") from exc
        except json.JSONDecodeError as exc:
            raise InvalidInputError(f"BarFrame JSON is invalid for {instrument}: {exc}") from exc
        if not isinstance(frame, dict) or frame.get("instrument") != instrument:
            raise InvalidInputError(f"BarFrame.instrument must match market {instrument}")
        timestamps = frame.get("ts")
        if not isinstance(timestamps, list) or not timestamps:
            raise InvalidInputError(f"BarFrame for {instrument} must contain timestamps")
        if len(timestamps) > MAX_CALLBACK_DIAGNOSTIC_BARS:
            raise InvalidInputError(
                f"BarFrame for {instrument} exceeds the {MAX_CALLBACK_DIAGNOSTIC_BARS}-bar diagnostic limit"
            )
        verdict = validate_dataset(
            {"schema_version": 1, "dataset_id": f"diagnostic-{market_index}", "bars_path": str(path)}
        )
        if not verdict.usable or verdict.instrument != instrument or verdict.rows != len(timestamps):
            raise InvalidInputError(f"BarFrame failed Rust dataset validation for {instrument}")
        market_inputs.append(
            {
                "instrument": instrument,
                "content_fingerprint": verdict.content_fingerprint,
                "rows": len(timestamps),
            }
        )
        total_rows += len(timestamps)
        if total_rows > MAX_CALLBACK_DIAGNOSTIC_BARS:
            raise InvalidInputError(
                f"multi-market callback diagnostics accept at most {MAX_CALLBACK_DIAGNOSTIC_BARS} total Bars"
            )
        if instrument == target_instrument:
            continue
        raw_bars = {key: value for key, value in frame.items() if key != "instrument"}
        bars = StrategyBars.from_dict(raw_bars)
        visible_count = sum(ts < target_as_of for ts in bars.ts)
        if visible_count:
            clipped = {
                key: (value[:visible_count] if isinstance(value, list) else value)
                for key, value in bars.to_dict().items()
            }
            market_bars[instrument] = StrategyBars.from_dict(clipped)

    fingerprint_material = json.dumps(
        sorted(
            (item["instrument"], item["content_fingerprint"])
            for item in market_inputs
        ),
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    request_value = request.to_dict()
    request_value["data_fingerprint"] = "multi-market-sha256:" + hashlib.sha256(
        fingerprint_material
    ).hexdigest()
    strategy_config = target_slot.get("strategy")
    factory_context = {
        "market": target_market,
        "strategy": strategy_config,
        "strategy_slot": target_slot,
    }
    return (
        StrategyInput.from_dict(request_value),
        strategy_config,
        factory_context,
        market_bars,
        sorted(market_inputs, key=lambda item: str(item["instrument"])),
    )


def _write_protected_json(path: Path, document: object, protected_paths: Sequence[str | Path]) -> Path:
    target = path.expanduser().resolve(strict=False)
    target_key = os.path.normcase(str(target))
    for source in protected_paths:
        source_key = os.path.normcase(str(Path(source).expanduser().resolve(strict=False)))
        if source_key == target_key:
            message = f"diagnostic output would overwrite an input file: {source}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    target.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            dir=target.parent,
            prefix=f".{target.name}.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, target)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    return target


def _read_json(path: str) -> object:
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except OSError as exc:
        message = f"cannot read JSON input {path!r}: {exc}"
        raise DataUnavailableError(
            message, payload={"category": "DATA_UNAVAILABLE", "message": message}
        ) from exc
    except json.JSONDecodeError as exc:
        message = f"invalid JSON input {path!r}: {exc}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        ) from exc


def _strategy_bundle_sha256(source_path: Path, source_bytes: bytes, module: object) -> str:
    """Hash the strategy file and Python dependencies imported from its directory."""
    root = source_path.parent.resolve()
    files: dict[str, bytes] = {}

    def add_file(path: Path, content: bytes | None = None) -> None:
        try:
            resolved = path.resolve(strict=True)
        except OSError as exc:
            message = f"cannot read declared strategy dependency {str(path)!r}: {exc}"
            raise DataUnavailableError(
                message, payload={"category": "DATA_UNAVAILABLE", "message": message}
            ) from exc
        try:
            relative = resolved.relative_to(root)
        except ValueError as exc:
            message = f"strategy dependency must remain under {root}: {path}"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            ) from exc
        if not resolved.is_file() or resolved.suffix.lower() != ".py":
            message = f"strategy dependency must be a Python source file: {path}"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            )
        try:
            payload = content if content is not None else resolved.read_bytes()
        except OSError as exc:
            message = f"cannot read strategy dependency {str(resolved)!r}: {exc}"
            raise DataUnavailableError(
                message, payload={"category": "DATA_UNAVAILABLE", "message": message}
            ) from exc
        files[relative.as_posix()] = payload

    add_file(source_path, source_bytes)

    declared = getattr(module, "STRATEGY_DEPENDENCIES", ())
    if not isinstance(declared, (tuple, list)):
        message = "STRATEGY_DEPENDENCIES must be a tuple or list of relative .py paths"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )
    for dependency in declared:
        if not isinstance(dependency, str) or not dependency.strip():
            message = "STRATEGY_DEPENDENCIES entries must be non-empty strings"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            )
        portable = PurePosixPath(dependency.replace("\\", "/"))
        if portable.is_absolute() or ".." in portable.parts or portable.suffix.lower() != ".py":
            message = f"invalid strategy dependency path: {dependency!r}"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            )
        add_file(root.joinpath(*portable.parts))

    # Capture eagerly imported local helpers automatically. Lazy imports must be listed in
    # STRATEGY_DEPENDENCIES so the identity is complete before the Rust run starts.
    for loaded_module in tuple(sys.modules.values()):
        module_path = getattr(loaded_module, "__file__", None)
        if not isinstance(module_path, str) or not module_path.endswith(".py"):
            continue
        candidate = Path(module_path)
        try:
            relative = candidate.resolve(strict=True).relative_to(root)
        except (OSError, ValueError):
            continue
        if relative.suffix.lower() == ".py":
            add_file(root / relative)

    digest = hashlib.sha256()
    digest.update(b"qianxing-strategy-bundle-v1\0")
    for relative, content in sorted(files.items()):
        path_bytes = relative.encode("utf-8")
        digest.update(len(path_bytes).to_bytes(4, "big"))
        digest.update(path_bytes)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def _load_strategy_file(path: str):
    """Load one user strategy module and return its factory, module, and source digest."""
    source_path = Path(path).expanduser().resolve()
    if source_path.suffix.lower() != ".py":
        message = f"strategy file must be a .py file: {source_path}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )
    try:
        source_bytes = source_path.read_bytes()
    except OSError as exc:
        message = f"cannot read strategy file {str(source_path)!r}: {exc}"
        raise DataUnavailableError(
            message, payload={"category": "DATA_UNAVAILABLE", "message": message}
        ) from exc

    source_digest = hashlib.sha256(source_bytes).hexdigest()
    module_name = f"_qianxing_strategy_{source_digest[:20]}"
    module_spec = importlib.util.spec_from_file_location(module_name, source_path)
    if module_spec is None or module_spec.loader is None:
        message = f"cannot load Python strategy file: {source_path}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )
    module = importlib.util.module_from_spec(module_spec)
    sys.modules[module_name] = module
    parent = str(source_path.parent)
    sys.path.insert(0, parent)
    try:
        module_spec.loader.exec_module(module)
    except Exception as exc:
        sys.modules.pop(module_name, None)
        message = f"loading Python strategy file failed: {type(exc).__name__}: {exc}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        ) from exc
    finally:
        try:
            sys.path.remove(parent)
        except ValueError:
            pass

    factory = getattr(module, "create_strategy", None)
    strategy_class = getattr(module, "STRATEGY_CLASS", None)
    if factory is None and strategy_class is not None:
        from qianxing_strategy import BaseStrategy

        if not isinstance(strategy_class, type) or not issubclass(
            strategy_class, BaseStrategy
        ):
            message = "STRATEGY_CLASS must be a qianxing_strategy.BaseStrategy subclass"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            )

        try:
            constructor = inspect.signature(strategy_class)
        except (TypeError, ValueError) as exc:
            message = (
                "cannot inspect STRATEGY_CLASS constructor; export "
                "create_strategy(spec: dict) instead"
            )
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            ) from exc

        accepts_spec = True
        try:
            constructor.bind({})
        except TypeError:
            accepts_spec = False
            try:
                constructor.bind()
            except TypeError as exc:
                message = (
                    "STRATEGY_CLASS constructor must accept the strategy spec or no arguments"
                )
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                ) from exc

        if accepts_spec:
            def factory(spec):
                return strategy_class(spec)
        else:
            def factory(_spec):
                return strategy_class()
    if not callable(factory):
        message = (
            "strategy file must export STRATEGY_CLASS (a BaseStrategy subclass) "
            "or create_strategy(spec: dict)"
        )
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )
    digest = _strategy_bundle_sha256(source_path, source_bytes, module)
    return module, factory, digest


def _strategy_identity(
    args: argparse.Namespace,
    module: object,
    strategy_file: str,
    payload: dict,
    strategy_manifest: Mapping[str, Any] | None = None,
) -> tuple[str, str]:
    path = Path(strategy_file)
    base = payload.get("base") if isinstance(payload.get("base"), dict) else payload
    strategy = base.get("strategy") if isinstance(base.get("strategy"), dict) else {}
    configured_id = payload.get("target_strategy_id") or strategy.get("strategy_id")
    strategy_id = (
        args.strategy_id
        or configured_id
        or getattr(module, "STRATEGY_ID", None)
        or (strategy_manifest or {}).get("strategy_id")
        or path.stem
    )
    if configured_id and str(strategy_id) != str(configured_id):
        message = (
            "strategy ID must match the primary strategy slot in the spec "
            f"({configured_id!r})"
        )
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )
    strategy_version = (
        args.strategy_version
        or getattr(module, "STRATEGY_VERSION", None)
        or (strategy_manifest or {}).get("strategy_version")
        or "local"
    )
    return str(strategy_id), str(strategy_version)


def _bind_manifest_warmup_factory(
    factory: Callable[..., object], warmup_bars: int
) -> Callable[..., object]:
    """Carry manifest warmup into each fresh instance and validate it at callback init."""
    def create(*args: object, **kwargs: object) -> object:
        strategy = factory(*args, **kwargs)
        if strategy is None:
            return None
        return bind_strategy_warmup(
            strategy,
            warmup_bars,
            defer_history_requirements=True,
        )

    return create


def _validate_strategy_config(
    module: object,
    manifest: Mapping[str, Any],
    execution_spec: Mapping[str, Any],
    target_strategy_id: str | None,
) -> None:
    """Run the single optional pure validator against declared parameters."""
    validator = declared_config_validator(module)
    if validator is None:
        return
    if manifest.get("completeness") != "complete":
        raise ValueError("validate_config requires a complete strategy manifest")
    config = resolve_strategy_parameters(
        execution_spec,
        manifest,
        target_strategy_id=target_strategy_id,
    )
    try:
        result = validator(MappingProxyType(copy.deepcopy(config)))
    except Exception as exc:
        raise ValueError(
            f"validate_config(config) rejected strategy parameters: {type(exc).__name__}: {exc}"
        ) from exc
    if result is not None:
        raise ValueError(
            "validate_config(config) must return None; raise ValueError to reject config"
        )


def _bind_manifest_config_factory(
    factory: Callable[..., object],
    module: object,
    manifest: Mapping[str, Any],
    target_strategy_id: str | None,
) -> Callable[..., object]:
    """Apply manifest defaults and cross-parameter validation for every factory call."""
    def create(
        execution_spec: Mapping[str, Any], *args: object, **kwargs: object
    ) -> object:
        if kwargs:
            raise ValueError("create_strategy(spec) does not accept keyword arguments")
        if len(args) > 1 or (args and not isinstance(args[0], Mapping)):
            raise ValueError("candidate parameters must be provided as one mapping")
        candidate_parameters = args[0] if args else None
        effective_target = target_strategy_id
        if effective_target is None and isinstance(execution_spec, Mapping):
            strategy = execution_spec.get("strategy")
            if isinstance(strategy, Mapping):
                identity = strategy.get("strategy_id")
                if isinstance(identity, str) and identity.strip():
                    effective_target = identity
        bound_spec = apply_parameter_defaults(
            execution_spec,
            manifest,
            target_strategy_id=effective_target,
            candidate_parameters=candidate_parameters,
        )
        _validate_strategy_config(
            module,
            manifest,
            bound_spec,
            effective_target,
        )
        return factory(bound_spec)

    return create


def _strategy_config_targets(
    execution_spec: Mapping[str, Any], target_strategy_id: str | None
) -> list[str | None]:
    """List the strategy slots that must pass module-level config validation."""
    base = execution_spec.get("base")
    if isinstance(base, Mapping):
        return _strategy_config_targets(base, target_strategy_id)
    markets = execution_spec.get("markets")
    if not isinstance(markets, list):
        return [target_strategy_id]
    identities: set[str] = set()
    for market in markets:
        if not isinstance(market, Mapping):
            continue
        slots = market.get("strategies")
        if not isinstance(slots, list):
            continue
        for slot in slots:
            strategy = slot.get("strategy") if isinstance(slot, Mapping) else None
            identity = strategy.get("strategy_id") if isinstance(strategy, Mapping) else None
            if isinstance(identity, str) and identity.strip():
                if target_strategy_id is None or identity == target_strategy_id:
                    identities.add(identity)
    return sorted(identities) if identities else [target_strategy_id]


def _multi_market_strategy_bindings(
    payload: dict,
    factory: Callable[[dict], object],
    *,
    strategy_id: str | None,
    strategy_version: str,
    source_sha256: str,
) -> list[dict[str, object]]:
    """Build fresh Python strategy instances for selected shared-account market slots."""
    markets = payload.get("markets")
    if not isinstance(markets, list) or not markets:
        message = "multi-market strategy input must contain a non-empty markets array"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )

    slots: list[tuple[str, dict, dict]] = []
    seen: set[str] = set()
    for market_index, market in enumerate(markets):
        if not isinstance(market, dict) or not isinstance(market.get("strategies"), list):
            message = f"markets[{market_index}].strategies must be an array"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            )
        for slot_index, portfolio_slot in enumerate(market["strategies"]):
            strategy = (
                portfolio_slot.get("strategy")
                if isinstance(portfolio_slot, dict)
                else None
            )
            slot_id = strategy.get("strategy_id") if isinstance(strategy, dict) else None
            if not isinstance(slot_id, str) or not slot_id.strip():
                message = (
                    f"markets[{market_index}].strategies[{slot_index}] must declare "
                    "strategy.strategy_id"
                )
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                )
            if slot_id in seen:
                message = f"multi-market strategy slot ID is duplicated: {slot_id}"
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                )
            seen.add(slot_id)
            slots.append((slot_id, market, portfolio_slot))

    selected = [slot for slot in slots if strategy_id is None or slot[0] == strategy_id]
    if not selected:
        message = (
            f"strategy ID {strategy_id!r} does not match a declared multi-market strategy slot"
            if strategy_id
            else "multi-market spec contains no strategy slots"
        )
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        )

    bindings: list[dict[str, object]] = []
    for slot_id, market, portfolio_slot in selected:
        strategy_spec = portfolio_slot["strategy"]
        factory_payload = {
            **payload,
            "market": market,
            "strategy": strategy_spec,
            "strategy_slot": portfolio_slot,
        }
        try:
            strategy = factory(factory_payload)
        except Exception as exc:
            message = (
                f"create_strategy(spec) failed for slot {slot_id!r}: "
                f"{type(exc).__name__}: {exc}"
            )
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            ) from exc
        if strategy is None:
            message = f"create_strategy(spec) returned None for slot {slot_id!r}"
            raise InvalidInputError(
                message, payload={"category": "INVALID_INPUT", "message": message}
            )
        bindings.append(
            {
                "strategy_id": slot_id,
                "strategy_version": strategy_version,
                "source_sha256": source_sha256,
                "strategy": strategy,
            }
        )
    return bindings


def _experiment_summary(result: object) -> dict[str, object]:
    """Render the long experiment report as a compact strategy review."""
    document = _document(result)
    if not isinstance(document, dict):
        return {"result": document}

    candidates = document.get("candidates", [])
    if isinstance(document.get("ranking"), (list, tuple)) and not isinstance(
        document.get("comparison"), dict
    ):
        by_ordinal = {
            candidate.get("ordinal"): candidate
            for candidate in candidates
            if isinstance(candidate, dict)
        }
        ranking = []
        for item in document.get("ranking", []):
            if not isinstance(item, dict):
                continue
            candidate = by_ordinal.get(item.get("ordinal"), {})
            ranking.append(
                {
                    **item,
                    "parameters": candidate.get("parameters", {}),
                }
            )
        out_of_sample = document.get("out_of_sample")
        fold_selections = []
        if isinstance(out_of_sample, dict):
            for fold in out_of_sample.get("folds", []):
                fold_selections.append(
                    {
                        "fold": fold.get("fold"),
                        "selected_ordinal": fold.get("selected_ordinal"),
                        "parameters": fold.get("selected_parameters", {}),
                        "in_sample_return_bps": fold.get("in_sample_return_bps"),
                        "out_of_sample_return_bps": fold.get("out_of_sample_return_bps"),
                    }
                )
        search_report = document.get("search")
        search_summary = None
        if isinstance(search_report, dict):
            search_folds = []
            for fold in search_report.get("fold_searches", []):
                if not isinstance(fold, dict):
                    continue
                trials = [
                    trial
                    for trial in fold.get("trials", [])
                    if isinstance(trial, dict)
                ]
                trials.sort(
                    key=lambda trial: (
                        trial.get("training_score_bps") is None,
                        -(trial.get("training_score_bps") or 0),
                        trial.get("ordinal", 0),
                    )
                )
                search_folds.append(
                    {
                        "fold": fold.get("fold"),
                        "seed": fold.get("seed"),
                        "top_training_trials": trials[:10],
                    }
                )
            search_summary = {
                "algorithm": search_report.get("algorithm"),
                "objective": search_report.get("objective"),
                "seed": search_report.get("seed"),
                "budget": search_report.get("budget"),
                "fold_local_search": search_report.get("fold_local_search"),
                "max_training_drawdown_bps": search_report.get(
                    "max_training_drawdown_bps"
                ),
                "fold_searches": search_folds,
            }
        return {
            "experiment_id": document.get("experiment_id"),
            "target_strategy_id": document.get("target_strategy_id"),
            "candidate_count": document.get("total_candidates"),
            "succeeded_candidates": document.get("succeeded_candidates"),
            "failed_candidates": document.get("failed_candidates"),
            "ranking_scope": (
                "first_training_fold"
                if isinstance(out_of_sample, dict)
                else "full_input_in_sample"
            ),
            "top_candidates": ranking[:10],
            "search": search_summary,
            "fold_candidate_failures": document.get("fold_candidate_failures", []),
            "out_of_sample": (
                {
                    "in_sample_return_bps": out_of_sample.get("in_sample_return_bps"),
                    "out_of_sample_return_bps": out_of_sample.get("out_of_sample_return_bps"),
                    "degradation_bps": out_of_sample.get("degradation_bps"),
                    "cross_fold_robustness": out_of_sample.get("cross_fold_robustness"),
                    "fold_selections": fold_selections,
                }
                if isinstance(out_of_sample, dict)
                else None
            ),
            **(
                {}
                if isinstance(out_of_sample, dict)
                else {"note": "multi-market grid has no time split; in-sample ranking alone does not establish out-of-sample effectiveness"}
            ),
            "artifact_path": document.get("artifact_path"),
        }

    by_run_id = {
        candidate.get("outcome", {}).get("run_id"): candidate
        for candidate in candidates
        if candidate.get("outcome") is not None
    }


    ranking = []
    comparison = document.get("comparison") or {}
    for item in comparison.get("runs", []):
        run = item.get("run", {})
        candidate = by_run_id.get(run.get("run_id"), {})
        ranking.append(
            {
                "rank": item.get("rank"),
                "ordinal": candidate.get("ordinal"),
                "parameters": candidate.get("parameters", {}),
                "return_bps": run.get("return_bps"),
                "max_drawdown_bps": run.get("max_drawdown_bps"),
                "fills": (candidate.get("outcome") or {}).get("fills"),
                "run_id": run.get("run_id"),
            }
        )

    out_of_sample = document.get("out_of_sample")
    fold_selections = []
    if isinstance(out_of_sample, dict):
        for fold in out_of_sample.get("folds", []):
            selected = next(
                (
                    candidate
                    for candidate in candidates
                    if candidate.get("ordinal") == fold.get("selected_ordinal")
                ),
                {},
            )
            fold_selections.append(
                {
                    "fold": fold.get("fold"),
                    "parameters": fold.get("selected_parameters")
                    or selected.get("parameters", {}),
                    "in_sample_return_bps": fold.get("in_sample_return_bps"),
                    "out_of_sample_return_bps": fold.get("out_of_sample_return_bps"),
                }
            )

    search = document.get("search") or {}
    neighborhood = search.get("neighborhood") or {}
    return {
        "experiment_id": document.get("experiment_id"),
        "candidate_count": document.get("total_candidates"),
        "succeeded_candidates": document.get("succeeded_candidates"),
        "failed_candidates": document.get("failed_candidates"),
        "top_candidates_by_comparison": ranking[:10],
        "out_of_sample": (
            {
                "in_sample_return_bps": out_of_sample.get("in_sample_return_bps"),
                "out_of_sample_return_bps": out_of_sample.get("out_of_sample_return_bps"),
                "degradation_bps": out_of_sample.get("degradation_bps"),
                "cross_fold_robustness": out_of_sample.get("cross_fold_robustness"),
                "fold_selections": fold_selections,
            }
            if isinstance(out_of_sample, dict)
            else None
        ),
        "baselines": document.get("baselines", []),
        "cost_sensitivity": [
            {
                "name": item.get("name"),
                "cost_robustness_passed": item.get("cost_robustness_passed"),
                "return_delta_bps": item.get("return_delta_bps"),
                "error": item.get("error"),
            }
            for item in document.get("cost_sensitivity", [])
        ],
        "parameter_neighborhood_assessment": neighborhood.get("assessment"),
        "artifact_path": document.get("artifact_path"),
    }


def _multi_market_experiment_factory(
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


def _diagnose_experiment_candidates(
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
) -> dict[str, Any]:
    """Attach an independently replayed bias diagnostic to each materialized candidate."""
    base_spec = experiment_spec.get("base")
    if not isinstance(base_spec, dict):
        raise TypeError("candidate diagnostics require an experiment base object")
    rows = _experiment_candidate_rows(experiment_document)
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
            candidate_spec, factory_context = _candidate_diagnostic_spec(
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
                ) = _multi_market_diagnostic_request(
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
                request = _strategy_diagnostic_request(
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


def _configure_console_encoding() -> None:
    """Keep JSON output valid on Windows terminals whose default code page is legacy."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if callable(reconfigure):
            try:
                reconfigure(encoding="utf-8", errors="backslashreplace")
            except (OSError, ValueError):
                pass


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="qianxing",
        description="牵星 Python SDK command line (shared Rust research workflows).",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("doctor", help="show installed SDK and native use-case availability")
    diagnose = commands.add_parser(
        "diagnose-strategy",
        help="probe a Python strategy class or factory for future-data and warmup sensitivity",
    )
    diagnose.add_argument(
        "strategy_file",
        help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
    )
    diagnose.add_argument("input", help="BacktestSpec, MultiMarketBacktestSpec, or experiment JSON file")
    diagnose.add_argument("--strategy-id", help="override the strategy ID")
    diagnose.add_argument("--strategy-version", help="override the strategy version")
    diagnose.add_argument(
        "--startup-windows",
        default="20,50,100",
        help="comma-separated history lengths used to compare warmup sensitivity",
    )
    diagnose.add_argument(
        "--max-lookahead-checks",
        type=int,
        default=32,
        help="maximum sampled decision points for future-Bar perturbations (1..1000)",
    )
    diagnose.add_argument("--timeout-seconds", type=int, default=10)
    diagnose.add_argument("--pit-ledger", type=Path, help="optional versioned point-in-time fact ledger")
    diagnose.add_argument("--out", type=Path, help="optional diagnostic JSON output path")
    inspect_strategy = commands.add_parser(
        "inspect-strategy",
        help="inspect a Python strategy's versioned metadata and code identity",
    )
    inspect_strategy.add_argument(
        "strategy_file",
        help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
    )
    inspect_strategy.add_argument("--out", type=Path, help="write the manifest JSON to this path")
    for name, help_text in (
        ("validate-dataset", "validate a versioned DatasetSpec JSON file"),
        ("backtest", "run a versioned Bar BacktestSpec JSON file"),
        ("verify", "verify artifacts described by a BacktestOutcome JSON file"),
        ("compare-runs", "rank completed runs from identical market data"),
        ("run-experiment", "run and compare a bounded Rust parameter grid"),
        ("run-depth-experiment", "run a bounded Rust Tick/OrderBook parameter grid"),
        ("run-multi-leg-experiment", "run a bounded Rust multi-leg parameter grid"),
        ("run-multi-market-experiment", "run a bounded shared-account multi-market grid"),
        ("depth-backtest", "run an L1 Tick or L2 order-book backtest"),
        ("verify-depth", "verify artifacts from an L1 Tick or L2 order-book run"),
    ):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("input", help="path to the versioned JSON document")
    for name, help_text in (
        ("run-strategy", "run a local Python strategy file on the shared Rust Bar, Tick, or OrderBook engine"),
        ("search-strategy", "search parameters for a Python strategy file on the Rust engine"),
    ):
        command = commands.add_parser(name, help=help_text)
        command.add_argument(
            "strategy_file",
            help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
        )
        command.add_argument(
            "input",
            help="BacktestSpec, DepthBacktestSpec, RunExperimentSpec, or RunDepthExperimentSpec JSON file",
        )
        command.add_argument("--strategy-id", help="override the strategy ID")
        command.add_argument("--strategy-version", help="override the strategy version")
        if name == "search-strategy":
            command.add_argument(
                "--summary",
                action="store_true",
                help="print a compact ranking and out-of-sample effectiveness review",
            )
            command.add_argument(
                "--html",
                nargs="?",
                const="",
                metavar="PATH",
                help="write an offline interactive HTML report (default: beside the result artifact)",
            )
            command.add_argument(
                "--diagnostics",
                type=Path,
                help="attach a matching diagnose-strategy JSON sidecar to the HTML report",
            )
            command.add_argument(
                "--diagnose-candidates",
                action="store_true",
                help="replay future-data and warmup diagnostics for every materialized candidate",
            )
            command.add_argument(
                "--diagnostic-startup-windows",
                nargs="+",
                type=int,
                default=[20, 50, 100],
                metavar="BARS",
                help="startup history lengths used for every candidate diagnostic",
            )
            command.add_argument(
                "--diagnostic-max-checks",
                type=int,
                default=8,
                help="maximum sampled decision points per candidate (1..1000)",
            )
            command.add_argument(
                "--diagnostic-timeout-seconds",
                type=int,
                default=10,
                help="strategy callback timeout for each candidate diagnostic",
            )
            command.add_argument(
                "--diagnostic-candidate-limit",
                type=int,
                help="optional cap; remaining candidates stay listed as not_run_candidate_limit",
            )
    research = commands.add_parser(
        "research",
        help="run strategy search, per-candidate bias diagnostics, and an interactive report",
    )
    research.add_argument(
        "strategy_file",
        help="Python file exporting STRATEGY_CLASS or create_strategy(spec)",
    )
    research.add_argument(
        "input",
        help="RunExperimentSpec with an explicit parameter space and train/OOS split_plan",
    )
    research.add_argument("--strategy-id", help="override the strategy ID")
    research.add_argument("--strategy-version", help="override the strategy version")
    research.add_argument(
        "--html",
        nargs="?",
        const="",
        default="",
        metavar="PATH",
        help="write the interactive HTML report (default: beside the experiment artifact)",
    )
    research.add_argument(
        "--diagnostic-startup-windows",
        nargs="+",
        type=int,
        default=[20, 50, 100],
        metavar="BARS",
        help="startup history lengths used for every materialized candidate",
    )
    research.add_argument(
        "--diagnostic-max-checks",
        type=int,
        default=8,
        help="maximum sampled decision points per candidate (1..1000)",
    )
    research.add_argument(
        "--diagnostic-timeout-seconds",
        type=int,
        default=10,
        help="strategy callback timeout for each candidate diagnostic",
    )
    research.add_argument(
        "--diagnostic-candidate-limit",
        type=int,
        default=16,
        help="maximum candidates to diagnose (default: 16); remaining candidates stay explicitly unverified",
    )
    remote_research = commands.add_parser(
        "remote-research",
        help="run a Python strategy research batch in an explicitly selected remote sandbox",
    )
    remote_research.add_argument(
        "strategy_file", help="Python file exporting create_strategy(spec)"
    )
    remote_research.add_argument(
        "input",
        help="RunExperimentSpec with an explicit parameter space and required train/OOS split_plan",
    )
    remote_research.add_argument(
        "--provider",
        choices=("e2b", "microsandbox", "opensandbox"),
        default="e2b",
        help="remote sandbox provider; E2B remains the default, with no automatic provider fallback",
    )
    remote_research.add_argument(
        "--template",
        default=os.environ.get("QX_E2B_TEMPLATE"),
        help="E2B template with the matching qianxing wheel and pinned strategy dependencies (or QX_E2B_TEMPLATE)",
    )
    remote_research.add_argument(
        "--domain",
        default=os.environ.get("OPEN_SANDBOX_DOMAIN"),
        help="OpenSandbox API domain (or OPEN_SANDBOX_DOMAIN)",
    )
    remote_research.add_argument(
        "--image",
        help="digest-pinned OCI image with the matching qianxing wheel and pinned strategy dependencies; provider-specific image environment variables are also supported",
    )
    remote_research.add_argument(
        "--backend",
        choices=("local", "cloud"),
        default=os.environ.get("QX_MICROSANDBOX_BACKEND", "local"),
        help="Microsandbox backend (default: local; cloud requires host credentials)",
    )
    remote_research.add_argument(
        "--cpus",
        type=int,
        help="sandbox CPU limit (1..64; defaults from the selected provider's host settings)",
    )
    remote_research.add_argument(
        "--memory-mib",
        type=int,
        help="sandbox memory limit in MiB (512..65536; defaults from the selected provider's host settings)",
    )
    remote_research.add_argument(
        "--timeout-seconds",
        type=int,
        default=900,
        help="sandbox lease and command timeout (1..86400 seconds)",
    )
    remote_research.add_argument(
        "--output-dir",
        type=Path,
        help="new local directory for downloaded result artifacts (default: <input>-<provider>-research)",
    )
    remote_research.add_argument("--strategy-id", help="override the strategy ID")
    remote_research.add_argument(
        "--strategy-version", help="override the strategy version"
    )
    remote_research.add_argument(
        "--diagnostic-startup-windows",
        nargs="+",
        type=int,
        default=[20, 50, 100],
        metavar="BARS",
        help="history lengths used for candidate warmup diagnostics",
    )
    remote_research.add_argument(
        "--diagnostic-max-checks",
        type=int,
        default=8,
        help="maximum sampled future-data checks per candidate (1..1000)",
    )
    remote_research.add_argument(
        "--diagnostic-timeout-seconds",
        type=int,
        default=10,
        help="strategy callback timeout used by each diagnostic probe",
    )
    remote_research.add_argument(
        "--diagnostic-candidate-limit",
        type=int,
        default=16,
        help="maximum candidates to diagnose (default: 16); remaining candidates stay explicitly unverified",
    )
    return parser


def _prepare_guided_research_input(payload: Any) -> dict[str, Any]:
    """Delegate guided research schema and baseline policy to the Rust app layer."""
    if not isinstance(payload, dict):
        raise InvalidInputError("qianxing research input must be a JSON object")
    return prepare_quick_research_spec(payload)


def main(argv: Sequence[str] | None = None) -> int:
    _configure_console_encoding()
    args = _parser().parse_args(argv)
    guided_research = args.command == "research"
    research_workflow = guided_research or args.command == "remote-research"
    if guided_research:
        # The guided flow is a preset over the same validated search path. Keep
        # one implementation for search, candidate identity, diagnostics, and reports.
        args.command = "search-strategy"
        args.summary = True
        args.diagnostics = None
        args.diagnose_candidates = True
    try:
        if args.command == "doctor":
            result = doctor()
        elif args.command == "inspect-strategy":
            module, _factory, bundle_sha256 = _load_strategy_file(args.strategy_file)
            try:
                strategy_manifest = build_module_manifest(
                    module, Path(args.strategy_file).expanduser().resolve(), bundle_sha256
                )
            except (OSError, ValueError) as exc:
                raise InvalidInputError(
                    f"invalid Python strategy manifest: {exc}",
                    payload={"category": "INVALID_INPUT", "message": str(exc)},
                ) from exc
            if args.out is not None:
                protected = [args.strategy_file]
                protected.extend(
                    Path(args.strategy_file).expanduser().resolve().parent / dependency
                    for dependency in strategy_manifest["dependencies"]
                )
                output_path = _write_protected_json(args.out, strategy_manifest, protected)
                print(f"Strategy manifest: {output_path}", file=sys.stderr)
            result = strategy_manifest
        else:
            payload = _read_json(args.input)
            if research_workflow:
                payload = _prepare_guided_research_input(payload)
            if args.command == "remote-research":
                if not isinstance(payload, dict):
                    raise InvalidInputError("remote research input must be a JSON object")
                try:
                    if args.provider == "e2b":
                        if not args.template:
                            raise InvalidInputError(
                                "E2B remote-research needs --template or QX_E2B_TEMPLATE"
                            )
                        from .sandbox_execution import E2BStrategySandboxProvider

                        provider = E2BStrategySandboxProvider(
                            template_id=args.template,
                            timeout_seconds=args.timeout_seconds,
                        )
                    elif args.provider == "opensandbox":
                        domain = args.domain or os.environ.get("OPEN_SANDBOX_DOMAIN")
                        if not domain:
                            raise InvalidInputError(
                                "OpenSandbox remote-research needs --domain or OPEN_SANDBOX_DOMAIN"
                            )
                        image = args.image or os.environ.get("QX_OPENSANDBOX_IMAGE")
                        if not image:
                            raise InvalidInputError(
                                "OpenSandbox remote-research needs --image or QX_OPENSANDBOX_IMAGE"
                            )
                        from .sandbox_execution import (
                            OpenSandboxStrategySandboxProvider,
                        )

                        provider = OpenSandboxStrategySandboxProvider(
                            domain=domain,
                            image=image,
                            api_key=os.environ.get("OPEN_SANDBOX_API_KEY"),
                            timeout_seconds=args.timeout_seconds,
                            cpu_count=(
                                args.cpus
                                if args.cpus is not None
                                else int(os.environ.get("QX_OPENSANDBOX_CPUS", "1"))
                            ),
                            memory_mib=(
                                args.memory_mib
                                if args.memory_mib is not None
                                else int(
                                    os.environ.get(
                                        "QX_OPENSANDBOX_MEMORY_MIB", "2048"
                                    )
                                )
                            ),
                        )
                    else:
                        image = args.image or os.environ.get("QX_MICROSANDBOX_IMAGE")
                        if not image:
                            raise InvalidInputError(
                                "Microsandbox remote-research needs --image or QX_MICROSANDBOX_IMAGE"
                            )
                        from .sandbox_execution import (
                            MicrosandboxStrategySandboxProvider,
                        )

                        provider = MicrosandboxStrategySandboxProvider(
                            image=image,
                            backend=args.backend,
                            api_key=os.environ.get("MICROSANDBOX_API_KEY"),
                            profile=os.environ.get("QX_MICROSANDBOX_PROFILE"),
                            url=os.environ.get("QX_MICROSANDBOX_URL"),
                            timeout_seconds=args.timeout_seconds,
                            cpu_count=(
                                args.cpus
                                if args.cpus is not None
                                else int(os.environ.get("QX_MICROSANDBOX_CPUS", "1"))
                            ),
                            memory_mib=(
                                args.memory_mib
                                if args.memory_mib is not None
                                else int(
                                    os.environ.get(
                                        "QX_MICROSANDBOX_MEMORY_MIB", "1024"
                                    )
                                )
                            ),
                        )
                    result = provider.run_research(
                        strategy_file=args.strategy_file,
                        input_path=args.input,
                        input_spec=payload,
                        output_dir=args.output_dir,
                        strategy_id=args.strategy_id,
                        strategy_version=args.strategy_version,
                        diagnostic_startup_windows=args.diagnostic_startup_windows,
                        diagnostic_max_checks=args.diagnostic_max_checks,
                        diagnostic_timeout_seconds=args.diagnostic_timeout_seconds,
                        diagnostic_candidate_limit=args.diagnostic_candidate_limit,
                    )
                except InvalidInputError:
                    raise
                except Exception as exc:
                    raise InvalidInputError(
                        f"remote {args.provider} research failed: {type(exc).__name__}: {exc}",
                        payload={
                            "category": "REMOTE_SANDBOX_FAILED",
                            "message": str(exc),
                        },
                    ) from exc
            elif args.command == "validate-dataset":
                result = validate_dataset(payload)
            elif args.command == "diagnose-strategy":
                if not isinstance(payload, dict):
                    message = "diagnose-strategy input must be a BacktestSpec object"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                diagnostic_spec = payload.get("base") if isinstance(payload.get("base"), dict) else payload
                module, _factory, source_sha256 = _load_strategy_file(args.strategy_file)
                try:
                    strategy_manifest = build_module_manifest(
                        module, Path(args.strategy_file).expanduser().resolve(), source_sha256
                    )
                    multi_market = isinstance(diagnostic_spec.get("markets"), list)
                    if multi_market:
                        slots = [
                            strategy.get("strategy_id")
                            for market in diagnostic_spec["markets"]
                            if isinstance(market, dict)
                            for slot in market.get("strategies", [])
                            if isinstance(slot, dict)
                            for strategy in [slot.get("strategy")]
                            if isinstance(strategy, dict)
                        ]
                        configured_id = payload.get("target_strategy_id")
                        if args.strategy_id and configured_id and args.strategy_id != configured_id:
                            raise InvalidInputError(
                                "--strategy-id must match target_strategy_id in the experiment spec"
                            )
                        strategy_id = (
                            args.strategy_id
                            or configured_id
                            or getattr(module, "STRATEGY_ID", None)
                            or strategy_manifest.get("strategy_id")
                            or (slots[0] if len(slots) == 1 else None)
                        )
                        if not isinstance(strategy_id, str) or not strategy_id.strip():
                            raise InvalidInputError(
                                "multi-market diagnostics need --strategy-id or target_strategy_id"
                            )
                        validate_strategy_execution_preflight_v1(
                            strategy_manifest,
                            diagnostic_spec,
                            purpose="diagnose",
                            target_strategy_id=strategy_id,
                        )
                        strategy_version = (
                            args.strategy_version
                            or getattr(module, "STRATEGY_VERSION", None)
                            or strategy_manifest.get("strategy_version")
                            or "local"
                        )
                        (
                            request,
                            strategy_config,
                            factory_context,
                            diagnostic_market_bars,
                            market_inputs,
                        ) = _multi_market_diagnostic_request(
                            diagnostic_spec, strategy_id, str(strategy_version)
                        )
                    else:
                        strategy_id, strategy_version = _strategy_identity(
                            args, module, args.strategy_file, payload, strategy_manifest
                        )
                        validate_strategy_execution_preflight_v1(
                            strategy_manifest,
                            diagnostic_spec,
                            purpose="diagnose",
                            target_strategy_id=None,
                        )
                        request = _strategy_diagnostic_request(
                            diagnostic_spec, strategy_id, strategy_version
                        )
                        strategy_config = diagnostic_spec.get("strategy")
                        factory_context = None
                        diagnostic_market_bars = None
                        market_inputs = None
                    windows = [
                        int(value.strip())
                        for value in args.startup_windows.split(",")
                        if value.strip()
                    ]
                except ValueError as exc:
                    raise InvalidInputError(
                        str(exc), payload={"category": "INVALID_INPUT", "message": str(exc)}
                    ) from exc
                pit_ledger = None
                if args.pit_ledger is not None:
                    try:
                        pit_ledger_value = json.loads(args.pit_ledger.read_text(encoding="utf-8"))
                    except OSError as exc:
                        message = f"cannot read PIT ledger {str(args.pit_ledger)!r}: {exc}"
                        raise DataUnavailableError(message, payload={"category": "DATA_UNAVAILABLE", "message": message}) from exc
                    except json.JSONDecodeError as exc:
                        message = f"PIT ledger JSON is invalid: {exc}"
                        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
                    if not isinstance(pit_ledger_value, dict):
                        message = "PIT ledger must be a JSON object"
                        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                    pit_ledger = pit_ledger_value
                try:
                    from qianxing_strategy.diagnostics import diagnose_callback_strategy

                    result = diagnose_callback_strategy(
                        request,
                        args.strategy_file,
                        diagnostic_spec,
                        strategy_bundle_sha256=source_sha256,
                        startup_windows=windows,
                        max_checks=args.max_lookahead_checks,
                        timeout_seconds=args.timeout_seconds,
                        pit_ledger=pit_ledger,
                        diagnostic_market_bars=diagnostic_market_bars,
                        factory_context=factory_context,
                    )
                    result["backtest_strategy_config"] = strategy_config
                    if multi_market:
                        result["multi_market"] = {
                            "target_strategy_id": strategy_id,
                            "market_count": len(diagnostic_spec["markets"]),
                            "peer_market_bars_included": len(diagnostic_market_bars or {}),
                            "peer_market_instruments": sorted(diagnostic_market_bars or {}),
                            "market_inputs": market_inputs,
                            "point_in_time_ledger_scope": (
                                "target_market_only" if pit_ledger is not None else "not_supplied"
                            ),
                        }
                except (OSError, ValueError, TimeoutError, subprocess.SubprocessError) as exc:
                    message = f"Python strategy diagnostic failed: {exc}"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
                if args.out is not None:
                    protected = [args.input, args.strategy_file, *_referenced_input_paths(diagnostic_spec)]
                    if args.pit_ledger is not None:
                        protected.append(args.pit_ledger)
                    output_path = _write_protected_json(args.out, result, protected)
                    print(f"Strategy diagnostics: {output_path}", file=sys.stderr)
            elif args.command == "backtest":
                result = run_backtest(payload)
            elif args.command == "compare-runs":
                result = compare_runs(payload)
            elif args.command == "run-experiment":
                result = run_experiment(payload)
            elif args.command == "run-depth-experiment":
                result = run_depth_experiment(payload)
            elif args.command == "run-multi-leg-experiment":
                result = run_multi_leg_experiment(payload)
            elif args.command == "run-multi-market-experiment":
                result = run_multi_market_experiment(payload)
            elif args.command == "depth-backtest":
                result = run_depth_backtest(payload)
            elif args.command == "verify-depth":
                result = verify_depth_run(payload)
            elif args.command in {"run-strategy", "search-strategy"}:
                if not isinstance(payload, dict):
                    message = "strategy input JSON must be an object"
                    raise InvalidInputError(
                        message,
                        payload={"category": "INVALID_INPUT", "message": message},
                    )
                module, factory, source_sha256 = _load_strategy_file(args.strategy_file)
                try:
                    strategy_manifest = build_module_manifest(
                        module, Path(args.strategy_file).expanduser().resolve(), source_sha256
                    )
                    selected_strategy_id = args.strategy_id
                    if selected_strategy_id is None and args.command == "search-strategy":
                        selected_strategy_id = payload.get("target_strategy_id")
                    if selected_strategy_id is None and args.command == "search-strategy":
                        selected_strategy_id = strategy_manifest.get("strategy_id")
                    if (
                        selected_strategy_id is None
                        and not isinstance(payload.get("markets"), list)
                    ):
                        selected_strategy_id = strategy_manifest.get("strategy_id")
                    payload = apply_parameter_defaults(
                        payload,
                        strategy_manifest,
                        target_strategy_id=selected_strategy_id,
                    )
                    if args.command == "run-strategy":
                        for config_target in _strategy_config_targets(
                            payload, selected_strategy_id
                        ):
                            _validate_strategy_config(
                                module,
                                strategy_manifest,
                                payload,
                                config_target,
                            )
                    factory = _bind_manifest_config_factory(
                        factory,
                        module,
                        strategy_manifest,
                        selected_strategy_id,
                    )
                    validate_strategy_execution_preflight_v1(
                        strategy_manifest,
                        payload,
                        purpose="search" if args.command == "search-strategy" else "run",
                        target_strategy_id=selected_strategy_id,
                        split_plan=(
                            payload.get("split_plan")
                            if args.command == "search-strategy"
                            and isinstance(payload.get("split_plan"), dict)
                            else None
                        ),
                    )
                    if strategy_manifest.get("warmup_bars") is not None:
                        factory = _bind_manifest_warmup_factory(
                            factory, strategy_manifest["warmup_bars"]
                        )
                except (OSError, ValueError) as exc:
                    raise InvalidInputError(
                        f"strategy manifest preflight failed: {exc}",
                        payload={"category": "INVALID_INPUT", "message": str(exc)},
                    ) from exc
                if args.command == "run-strategy":
                    is_depth_spec = (
                        payload.get("schema_version") == 1
                        and isinstance(payload.get("depth_path"), str)
                        and payload.get("tier") in {"l1", "l2"}
                    )
                    if is_depth_spec:
                        expected_data_level = "tick" if payload["tier"] == "l1" else "orderbook"
                        declared_data_level = strategy_manifest.get("data_level")
                        if declared_data_level not in (None, expected_data_level):
                            message = (
                                f"strategy manifest data_level={declared_data_level!r} does not match "
                                f"depth tier={payload['tier']!r} (expected {expected_data_level!r})"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        strategy_id, strategy_version = _strategy_identity(
                            args, module, args.strategy_file, payload, strategy_manifest
                        )
                        try:
                            strategy = factory(payload)
                        except Exception as exc:
                            message = (
                                "create_strategy(spec) failed: "
                                f"{type(exc).__name__}: {exc}"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            ) from exc
                        if strategy is None:
                            message = "create_strategy(spec) returned None"
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        result = run_depth_backtest_with_strategy(
                            payload,
                            strategy,
                            strategy_id=strategy_id,
                            strategy_version=strategy_version,
                            source_sha256=source_sha256,
                        )
                    elif isinstance(payload.get("markets"), list):
                        strategy_version = (
                            args.strategy_version
                            or getattr(module, "STRATEGY_VERSION", None)
                            or strategy_manifest.get("strategy_version")
                            or "local"
                        )
                        bindings = _multi_market_strategy_bindings(
                            payload,
                            factory,
                            strategy_id=args.strategy_id,
                            strategy_version=str(strategy_version),
                            source_sha256=source_sha256,
                        )
                        result = run_multi_market_backtest_with_strategy_slots(
                            payload, bindings
                        )
                    else:
                        strategy_id, strategy_version = _strategy_identity(
                            args, module, args.strategy_file, payload, strategy_manifest
                        )
                        try:
                            strategy = factory(payload)
                        except Exception as exc:
                            message = (
                                "create_strategy(spec) failed: "
                                f"{type(exc).__name__}: {exc}"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            ) from exc
                        if strategy is None:
                            message = "create_strategy(spec) returned None"
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        result = run_backtest_with_strategy(
                            payload,
                            strategy,
                            strategy_id=strategy_id,
                            strategy_version=strategy_version,
                            source_sha256=source_sha256,
                        )
                else:
                    strategy_id, strategy_version = _strategy_identity(
                        args, module, args.strategy_file, payload, strategy_manifest
                    )
                    depth_base = payload.get("base")
                    if (
                        isinstance(depth_base, dict)
                        and isinstance(depth_base.get("depth_path"), str)
                        and depth_base.get("tier") in {"l1", "l2"}
                    ):
                        expected_data_level = (
                            "tick" if depth_base["tier"] == "l1" else "orderbook"
                        )
                        declared_data_level = strategy_manifest.get("data_level")
                        if declared_data_level not in (None, expected_data_level):
                            message = (
                                f"strategy manifest data_level={declared_data_level!r} does not match "
                                f"depth tier={depth_base['tier']!r} (expected {expected_data_level!r})"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if payload.get("strategy_kinds"):
                            message = (
                                "strategy_kinds cannot be combined with a Python strategy file; "
                                "use parameter_space for the depth strategy"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if not payload.get("parameter_space"):
                            message = "RunDepthExperimentSpec needs parameter_space"
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if args.html is not None or args.diagnostics is not None:
                            message = (
                                "interactive HTML and diagnostic sidecars currently support Bar experiments only; "
                                "depth strategy search returns the complete ranked JSON report"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if args.diagnose_candidates:
                            message = (
                                "candidate future-data diagnostics currently support Bar experiments only"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        print(
                            "[建议] RunDepthExperimentSpec 没有训练/样本外切分；当前排名只表示该深度输入上的样本内候选排序。",
                            file=sys.stderr,
                        )
                        result = run_depth_experiment_with_strategy(
                            payload,
                            factory,
                            strategy_id=strategy_id,
                            strategy_version=strategy_version,
                            source_sha256=source_sha256,
                        )
                    elif isinstance(payload.get("base"), dict) and isinstance(
                        payload["base"].get("markets"), list
                    ):
                        if payload.get("strategy_kinds"):
                            message = (
                                "strategy_kinds cannot be combined with a Python strategy file; "
                                "use parameter_space for the target strategy slot"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        callback_space = payload.get("callback_parameter_space")
                        parameter_space = payload.get("parameter_space")
                        search = payload.get("search") or {}
                        continuous_space = (
                            search.get("continuous_parameter_space", [])
                            if isinstance(search, dict)
                            else []
                        )
                        if callback_space and parameter_space:
                            message = (
                                "use either callback_parameter_space for arbitrary Python strategy "
                                "parameters or parameter_space for Rust engine parameters, not both"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if not callback_space and not parameter_space and not continuous_space:
                            message = (
                                "multi-market search-strategy needs parameter_space or "
                                "callback_parameter_space or search.continuous_parameter_space"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if not payload.get("split_plan"):
                            print(
                                "[建议] 多市场文件策略实验未配置 split_plan；结果只有样本内排序，不能据此判断样本外有效性。",
                                file=sys.stderr,
                            )
                        result = run_multi_market_experiment_with_strategy(
                            payload,
                            _multi_market_experiment_factory(
                                payload, factory, strategy_id
                            ),
                            strategy_id=strategy_id,
                            strategy_version=strategy_version,
                            source_sha256=source_sha256,
                        )
                    else:
                        search = payload.get("search") or {}
                        if not isinstance(search, dict):
                            message = "RunExperimentSpec.search must be an object"
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if not payload.get("parameter_space") and not search.get(
                            "continuous_parameter_space"
                        ):
                            message = (
                                "search-strategy needs parameter_space or "
                                "search.continuous_parameter_space"
                            )
                            raise InvalidInputError(
                                message,
                                payload={"category": "INVALID_INPUT", "message": message},
                            )
                        if not payload.get("split_plan"):
                            print(
                                "[建议] RunExperimentSpec 未配置 split_plan；结果只有样本内排序，不能据此判断样本外有效性。",
                                file=sys.stderr,
                            )
                        result = run_experiment_with_strategy(
                            payload,
                            factory,
                            strategy_id=strategy_id,
                            strategy_version=strategy_version,
                            source_sha256=source_sha256,
                        )
            else:
                result = verify_run(payload)
        full_document = _document(result)
        if args.command == "search-strategy" and args.diagnose_candidates:
            if not isinstance(payload, dict) or not isinstance(payload.get("base"), dict):
                message = "--diagnose-candidates requires a RunExperimentSpec or RunMultiMarketExperimentSpec"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            try:
                candidate_diagnostics = _diagnose_experiment_candidates(
                    payload,
                    full_document if isinstance(full_document, dict) else {},
                    args.strategy_file,
                    strategy_id=strategy_id,
                    strategy_version=str(strategy_version),
                    strategy_bundle_sha256=source_sha256,
                    startup_windows=args.diagnostic_startup_windows,
                    max_checks=args.diagnostic_max_checks,
                    timeout_seconds=args.diagnostic_timeout_seconds,
                    candidate_limit=args.diagnostic_candidate_limit,
                )
            except (TypeError, ValueError) as exc:
                raise InvalidInputError(
                    f"candidate diagnostics configuration failed: {exc}",
                    payload={"category": "INVALID_INPUT", "message": str(exc)},
                ) from exc
            if not isinstance(full_document, dict):
                message = "search-strategy result cannot carry candidate diagnostics"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            full_document = {**full_document, "candidate_diagnostics": candidate_diagnostics}
        document = (
            _experiment_summary(full_document)
            if args.command == "search-strategy" and args.summary
            else full_document
        )
        if (
            args.command == "search-strategy"
            and args.summary
            and isinstance(full_document, dict)
            and isinstance(full_document.get("candidate_diagnostics"), dict)
            and isinstance(document, dict)
        ):
            diagnostics = full_document["candidate_diagnostics"]
            document["candidate_diagnostics"] = {
                key: diagnostics.get(key)
                for key in (
                    "coverage_status",
                    "candidate_count",
                    "diagnosed_count",
                    "diagnostic_failure_count",
                    "unavailable_candidate_count",
                    "not_run_candidate_count",
                    "items",
                )
            }
            document["candidate_diagnostics"]["items"] = [
                {
                    key: item.get(key)
                    for key in (
                        "scope",
                        "fold",
                        "ordinal",
                        "strategy_kind",
                        "parameters",
                        "candidate_fingerprint",
                        "status",
                        "reason",
                    )
                }
                for item in diagnostics.get("items", [])
                if isinstance(item, dict)
            ]
        if args.command == "search-strategy" and (args.html is not None or args.diagnostics is not None):
            if args.diagnostics is not None and args.html is None:
                message = "--diagnostics requires --html"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
            if not isinstance(full_document, dict):
                message = "search-strategy result is not an object and cannot be rendered as an HTML report"
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                )
            report_document = full_document
            if args.diagnostics is not None:
                if not isinstance(payload, dict) or not isinstance(payload.get("base"), dict):
                    message = "--diagnostics requires a RunExperimentSpec or RunMultiMarketExperimentSpec"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                try:
                    diagnostics = _read_json(str(args.diagnostics))
                except QianxingError as exc:
                    raise InvalidInputError(
                        f"cannot read diagnostics attachment: {exc}",
                        payload={"category": "INVALID_INPUT", "message": str(exc)},
                    ) from exc
                if not isinstance(diagnostics, dict):
                    message = "diagnostics attachment must be a JSON object"
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                base_spec_sha256 = hashlib.sha256(
                    json.dumps(
                        payload["base"],
                        ensure_ascii=False,
                        sort_keys=True,
                        separators=(",", ":"),
                        allow_nan=False,
                    ).encode("utf-8")
                ).hexdigest()
                expected_identity = {
                    "kind": "qianxing-strategy-bias-diagnostics",
                    "strategy": "python_create_strategy_callback",
                    "strategy_id": strategy_id,
                    "strategy_version": strategy_version,
                    "strategy_bundle_sha256": source_sha256,
                    "backtest_spec_sha256": base_spec_sha256,
                }
                multi_market_base = isinstance(payload["base"].get("markets"), list)
                if multi_market_base:
                    (
                        expected_request,
                        expected_strategy_config,
                        _expected_factory_context,
                        expected_peer_frames,
                        expected_market_inputs,
                    ) = _multi_market_diagnostic_request(
                        payload["base"], strategy_id, str(strategy_version)
                    )
                    expected_identity["data_fingerprint"] = expected_request.data_fingerprint
                    expected_identity["instrument"] = expected_request.instrument
                    expected_identity["backtest_strategy_config"] = expected_strategy_config
                    attached_multi_market = diagnostics.get("multi_market")
                    if not isinstance(attached_multi_market, dict):
                        mismatches = ["multi_market"]
                    else:
                        mismatches = [
                            key
                            for key, expected in (
                                ("target_strategy_id", strategy_id),
                                ("market_count", len(payload["base"]["markets"])),
                                ("market_inputs", expected_market_inputs),
                                (
                                    "peer_market_instruments",
                                    sorted(expected_peer_frames),
                                ),
                                (
                                    "peer_market_bars_included",
                                    len(expected_peer_frames),
                                ),
                                (
                                    "point_in_time_ledger_scope",
                                    "target_market_only"
                                    if diagnostics.get("data_visibility") is not None
                                    else "not_supplied",
                                ),
                            )
                            if attached_multi_market.get(key) != expected
                        ]
                else:
                    expected_identity["data_fingerprint"] = full_document.get(
                        "input_data_fingerprint"
                    )
                    expected_identity["instrument"] = payload["base"].get("instrument")
                    mismatches = []
                mismatches = [
                    *mismatches,
                    *(
                        key
                        for key, expected in expected_identity.items()
                        if diagnostics.get(key) != expected
                    ),
                ]
                if mismatches:
                    message = "diagnostics attachment identity does not match this experiment: " + ", ".join(mismatches)
                    raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
                report_document = {**full_document, "diagnostics": diagnostics}
            try:
                base_spec = payload.get("base") if isinstance(payload, dict) else None
                if isinstance(base_spec, dict) and isinstance(base_spec.get("markets"), list):
                    if all(
                        isinstance(market, dict) and isinstance(market.get("bars_path"), str)
                        for market in base_spec["markets"]
                    ):
                        verified_details = load_verified_multi_market_candidate_details(
                            full_document,
                            market_validator=validate_dataset,
                            base_dir=Path.cwd(),
                        )
                    else:
                        verified_details = {
                            "schema_version": 1,
                            "runs": [],
                            "unsupported_scope": "当前 Python 搜索报告仅接入共享账户多市场 Bar 产物；Tick/L2 和多账户归因尚未从其独立产物导入。",
                        }
                else:
                    verified_details = load_verified_candidate_details(
                        full_document,
                        base_dir=Path.cwd(),
                        rust_report_reader=read_run_report_v1,
                    )
                report_document = {**report_document, "research_details": verified_details}
            except (OSError, ValueError) as exc:
                message = f"cannot verify candidate run details for HTML report: {exc}"
                raise InvalidInputError(
                    message, payload={"category": "INVALID_INPUT", "message": message}
                ) from exc
            try:
                report_path = write_experiment_html(
                    report_document,
                    args.html or None,
                    protected_paths=(
                        args.input,
                        args.strategy_file,
                        *_referenced_input_paths(payload),
                    ),
                )
            except ValueError as exc:
                raise InvalidInputError(
                    str(exc), payload={"category": "INVALID_INPUT", "message": str(exc)}
                ) from exc
            except OSError as exc:
                message = f"cannot write experiment HTML report: {exc}"
                raise DataUnavailableError(
                    message, payload={"category": "DATA_UNAVAILABLE", "message": message}
                ) from exc
            print(f"Interactive HTML report: {report_path}", file=sys.stderr)
        print(json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True))
        return 0
    except QianxingError as exc:
        error = exc.payload or {"category": exc.code, "message": str(exc)}
        print(json.dumps(error, ensure_ascii=False, sort_keys=True), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
