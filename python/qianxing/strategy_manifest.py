"""Versioned metadata contract for file-based Python strategies."""

from __future__ import annotations

import copy
import hashlib
import math
from pathlib import Path, PurePosixPath
from types import MappingProxyType
from typing import Any, Callable, Mapping


_MANIFEST_KEYS = {
    "schema_version",
    "strategy_id",
    "strategy_version",
    "parameters",
    "instrument_requirements",
    "data_level",
    "warmup_bars",
    "dependencies",
}
_PARAMETER_TYPES = {"integer", "number", "string", "boolean"}
_DATA_LEVELS = {"bar", "tick", "orderbook", "timer"}


def _declared_strategy_class(module: object) -> type | None:
    strategy_class = getattr(module, "STRATEGY_CLASS", None)
    if strategy_class is None:
        return None
    if not isinstance(strategy_class, type):
        raise ValueError("STRATEGY_CLASS must be a class")
    return strategy_class


def declared_config_validator(
    module: object,
) -> Callable[[Mapping[str, Any]], Any] | None:
    """Resolve the single optional config validator exported by a strategy module/class."""
    strategy_class = _declared_strategy_class(module)
    module_validator = getattr(module, "validate_config", None)
    class_validator = (
        getattr(strategy_class, "validate_config", None)
        if strategy_class is not None
        else None
    )
    if module_validator is not None and class_validator is not None:
        raise ValueError(
            "declare validate_config either on the module or STRATEGY_CLASS, not both"
        )
    validator = module_validator if module_validator is not None else class_validator
    if validator is not None and not callable(validator):
        owner = "strategy module" if module_validator is not None else "STRATEGY_CLASS"
        raise ValueError(f"{owner} validate_config must be callable")
    return validator


def _is_number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _validate_parameter_value(name: str, definition: Mapping[str, Any], value: object) -> None:
    parameter_type = definition["type"]
    valid_type = {
        "integer": lambda item: isinstance(item, int) and not isinstance(item, bool),
        "number": _is_number,
        "string": lambda item: isinstance(item, str),
        "boolean": lambda item: isinstance(item, bool),
    }[parameter_type]
    if not valid_type(value):
        raise ValueError(f"parameter {name!r} must have type {parameter_type}")
    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError(f"parameter {name!r} must be finite")
    minimum = definition.get("minimum")
    maximum = definition.get("maximum")
    if minimum is not None and value < minimum:
        raise ValueError(f"parameter {name!r} is below minimum {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"parameter {name!r} is above maximum {maximum}")
    choices = definition.get("choices")
    if choices is not None and value not in choices:
        raise ValueError(f"parameter {name!r} is not one of its declared choices")


def normalize_manifest(
    raw: object,
    *,
    strategy_id: str,
    strategy_version: str,
    dependencies: object,
) -> dict[str, Any]:
    """Validate an optional module ``STRATEGY_MANIFEST`` and return normalized metadata.

    Missing declarations remain visible as ``completeness=partial``. They are never
    silently promoted into a complete parameter or warmup contract.
    """
    if dependencies is None:
        declared_dependencies = ()
    elif isinstance(dependencies, (tuple, list)):
        declared_dependencies = dependencies
    else:
        raise ValueError("STRATEGY_DEPENDENCIES must be a tuple or list")
    if any(not isinstance(item, str) for item in declared_dependencies):
        raise ValueError("STRATEGY_DEPENDENCIES must contain relative .py paths")
    if len(set(declared_dependencies)) != len(declared_dependencies):
        raise ValueError("STRATEGY_DEPENDENCIES must not contain duplicates")
    if any(
        not isinstance(item, str)
        or not item.strip()
        or PurePosixPath(item.replace("\\", "/")).is_absolute()
        or ".." in PurePosixPath(item.replace("\\", "/")).parts
        or ":" in PurePosixPath(item.replace("\\", "/")).parts[0]
        or PurePosixPath(item.replace("\\", "/")).suffix.lower() != ".py"
        for item in declared_dependencies
    ):
        raise ValueError("STRATEGY_DEPENDENCIES must contain relative .py paths")

    if raw is None:
        return {
            "schema_version": 1,
            "kind": "qianxing-python-strategy-manifest",
            "strategy_id": strategy_id,
            "strategy_version": strategy_version,
            "entrypoint": "create_strategy(spec)",
            "parameters": {},
            "instrument_requirements": None,
            "data_level": None,
            "warmup_bars": None,
            "dependencies": list(declared_dependencies),
            "completeness": "partial",
        }

    if not isinstance(raw, dict):
        raise ValueError("STRATEGY_MANIFEST must be an object")
    unknown = sorted(set(raw) - _MANIFEST_KEYS)
    if unknown:
        raise ValueError(f"STRATEGY_MANIFEST has unknown fields: {unknown}")
    if (
        not isinstance(raw.get("schema_version"), int)
        or isinstance(raw.get("schema_version"), bool)
        or raw.get("schema_version") != 1
    ):
        raise ValueError("STRATEGY_MANIFEST.schema_version must equal 1")

    declared_id = raw.get("strategy_id", strategy_id)
    declared_version = raw.get("strategy_version", strategy_version)
    if not isinstance(declared_id, str) or not declared_id.strip():
        raise ValueError("STRATEGY_MANIFEST.strategy_id must be a non-empty string")
    if not isinstance(declared_version, str) or not declared_version.strip():
        raise ValueError("STRATEGY_MANIFEST.strategy_version must be a non-empty string")
    if declared_id != strategy_id:
        raise ValueError("STRATEGY_MANIFEST.strategy_id must match STRATEGY_ID")
    if declared_version != strategy_version:
        raise ValueError("STRATEGY_MANIFEST.strategy_version must match STRATEGY_VERSION")

    parameters = raw.get("parameters")
    if not isinstance(parameters, dict):
        raise ValueError("STRATEGY_MANIFEST.parameters must be an object")
    normalized_parameters: dict[str, dict[str, Any]] = {}
    for name, definition in sorted(parameters.items()):
        if not isinstance(name, str) or not name.strip() or not isinstance(definition, dict):
            raise ValueError("parameter definitions need non-empty names and object values")
        allowed_definition_keys = {
            "type", "required", "default", "minimum", "maximum", "choices"
        }
        unknown_definition_keys = sorted(set(definition) - allowed_definition_keys)
        if unknown_definition_keys:
            raise ValueError(
                f"parameter {name!r} has unknown fields: {unknown_definition_keys}"
            )
        parameter_type = definition.get("type")
        if parameter_type not in _PARAMETER_TYPES:
            raise ValueError(
                f"parameter {name!r}.type must be one of {sorted(_PARAMETER_TYPES)}"
            )
        required = definition.get("required", False)
        if not isinstance(required, bool):
            raise ValueError(f"parameter {name!r}.required must be boolean")
        normalized = {"type": parameter_type, "required": required}
        for bound in ("minimum", "maximum"):
            if bound in definition:
                if parameter_type not in {"integer", "number"}:
                    raise ValueError(f"parameter {name!r}.{bound} requires a numeric type")
                bound_value = definition[bound]
                if not _is_number(bound_value) or not math.isfinite(float(bound_value)):
                    raise ValueError(f"parameter {name!r}.{bound} must be a finite number")
                if parameter_type == "integer" and not isinstance(bound_value, int):
                    raise ValueError(f"integer parameter {name!r}.{bound} must be an integer")
                normalized[bound] = bound_value
        if (
            "minimum" in normalized
            and "maximum" in normalized
            and normalized["minimum"] > normalized["maximum"]
        ):
            raise ValueError(f"parameter {name!r}.minimum exceeds maximum")
        if "choices" in definition:
            choices = definition["choices"]
            if not isinstance(choices, list) or not choices:
                raise ValueError(f"parameter {name!r}.choices must be a non-empty array")
            for choice in choices:
                _validate_parameter_value(name, normalized, choice)
            normalized["choices"] = choices
        if "default" in definition:
            _validate_parameter_value(name, normalized, definition["default"])
            normalized["default"] = definition["default"]
        if required and "default" not in normalized and not any(
            key in normalized for key in ("minimum", "maximum", "choices")
        ):
            raise ValueError(
                f"required parameter {name!r} needs a default, bound, or choices"
            )
        normalized_parameters[name] = normalized

    requirements = raw.get("instrument_requirements")
    if not isinstance(requirements, dict):
        raise ValueError("STRATEGY_MANIFEST.instrument_requirements must be an object")
    allowed_requirement_keys = {
        "asset_classes", "products", "venues", "requires_explicit_spec"
    }
    unknown_requirement_keys = sorted(set(requirements) - allowed_requirement_keys)
    if unknown_requirement_keys:
        raise ValueError(
            f"instrument_requirements has unknown fields: {unknown_requirement_keys}"
        )
    normalized_requirements: dict[str, Any] = {}
    for key in ("asset_classes", "products", "venues"):
        values = requirements.get(key, [])
        if not isinstance(values, list) or any(
            not isinstance(item, str) or not item.strip() for item in values
        ):
            raise ValueError(f"instrument_requirements.{key} must be an array of non-empty strings")
        normalized_requirements[key] = sorted(set(values))
    requires_explicit_spec = requirements.get("requires_explicit_spec")
    if not isinstance(requires_explicit_spec, bool):
        raise ValueError("instrument_requirements.requires_explicit_spec must be boolean")
    normalized_requirements["requires_explicit_spec"] = requires_explicit_spec
    data_level = raw.get("data_level")
    if data_level not in _DATA_LEVELS:
        raise ValueError(f"STRATEGY_MANIFEST.data_level must be one of {sorted(_DATA_LEVELS)}")
    warmup_bars = raw.get("warmup_bars")
    if not isinstance(warmup_bars, int) or isinstance(warmup_bars, bool) or warmup_bars < 0:
        raise ValueError("STRATEGY_MANIFEST.warmup_bars must be a non-negative integer")
    manifest_dependencies = raw.get("dependencies", list(declared_dependencies))
    if not isinstance(manifest_dependencies, list) or any(
        not isinstance(item, str)
        for item in manifest_dependencies
    ) or len(set(manifest_dependencies)) != len(manifest_dependencies) or set(
        manifest_dependencies
    ) != set(declared_dependencies):
        raise ValueError(
            "STRATEGY_MANIFEST.dependencies must match STRATEGY_DEPENDENCIES"
        )

    return {
        "schema_version": 1,
        "kind": "qianxing-python-strategy-manifest",
        "strategy_id": declared_id,
        "strategy_version": declared_version,
        "entrypoint": "create_strategy(spec)",
        "parameters": normalized_parameters,
        "instrument_requirements": normalized_requirements,
        "data_level": data_level,
        "warmup_bars": warmup_bars,
        "dependencies": sorted(manifest_dependencies),
        "completeness": "complete",
    }


def bind_code_identity(
    manifest: Mapping[str, Any], strategy_path: Path, bundle_sha256: str
) -> dict[str, Any]:
    source_sha256 = hashlib.sha256(strategy_path.read_bytes()).hexdigest()
    return {
        **manifest,
        "source_sha256": source_sha256,
        "bundle_sha256": bundle_sha256,
    }


def apply_parameter_defaults(
    execution_spec: Mapping[str, Any],
    manifest: Mapping[str, Any],
    *,
    target_strategy_id: str | None = None,
    candidate_parameters: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Return an execution spec with declared parameter defaults bound to strategy slots.

    Defaults are applied only to declared strategy parameters. The caller's object is never
    mutated, and unrelated strategy slots are left untouched when a target identity is known.
    This is used both before preflight and for each materialized search candidate.
    """
    if not isinstance(execution_spec, Mapping):
        raise TypeError("strategy execution spec must be a mapping")
    normalized = copy.deepcopy(dict(execution_spec))
    definitions = manifest.get("parameters", {})
    if not isinstance(definitions, Mapping):
        raise ValueError("strategy manifest parameters must be a mapping")
    defaults = {
        name: definition["default"]
        for name, definition in definitions.items()
        if isinstance(name, str)
        and isinstance(definition, Mapping)
        and "default" in definition
    }
    overrides = {
        name: value
        for name, value in (candidate_parameters or {}).items()
        if name in definitions
    }

    def bind(config: object, *, values: Mapping[str, Any] | None = None) -> None:
        if not isinstance(config, dict):
            return
        identity = config.get("strategy_id")
        if (
            target_strategy_id is not None
            and isinstance(identity, str)
            and identity != target_strategy_id
        ):
            return
        for name, value in defaults.items():
            config.setdefault(name, copy.deepcopy(value))
        for name, value in (values or {}).items():
            if name in definitions:
                config[name] = copy.deepcopy(value)

    def visit(value: object) -> None:
        if not isinstance(value, dict):
            return
        base = value.get("base")
        if isinstance(base, dict):
            visit(base)
        parameter_values = value.get("parameters")
        if not isinstance(parameter_values, Mapping):
            parameter_values = {}
        parameter_values = {**parameter_values, **overrides}
        bind(value.get("strategy"), values=parameter_values)
        bind(value.get("parameters"), values=overrides)
        slot = value.get("strategy_slot")
        if isinstance(slot, dict):
            bind(slot.get("strategy"), values=parameter_values)
        markets = value.get("markets")
        if isinstance(markets, list):
            for market in markets:
                if not isinstance(market, dict):
                    continue
                slots = market.get("strategies")
                if not isinstance(slots, list):
                    continue
                for portfolio_slot in slots:
                    if isinstance(portfolio_slot, dict):
                        bind(portfolio_slot.get("strategy"), values=parameter_values)

    visit(normalized)
    return normalized


def resolve_strategy_parameters(
    execution_spec: Mapping[str, Any],
    manifest: Mapping[str, Any],
    *,
    target_strategy_id: str | None = None,
) -> dict[str, Any]:
    """Resolve declared defaults and configured values for pure cross-parameter validation."""
    definitions = manifest.get("parameters", {})
    if not isinstance(definitions, Mapping):
        raise ValueError("strategy manifest parameters must be a mapping")
    resolved = {
        name: copy.deepcopy(definition["default"])
        for name, definition in definitions.items()
        if isinstance(name, str)
        and isinstance(definition, Mapping)
        and "default" in definition
    }
    names = set(definitions)

    def merge(config: object) -> None:
        if not isinstance(config, Mapping):
            return
        identity = config.get("strategy_id")
        if (
            target_strategy_id is not None
            and isinstance(identity, str)
            and identity != target_strategy_id
        ):
            return
        for name in names:
            if name in config:
                resolved[name] = copy.deepcopy(config[name])

    def visit(value: object) -> None:
        if not isinstance(value, Mapping):
            return
        base = value.get("base")
        if isinstance(base, Mapping):
            visit(base)
        merge(value.get("strategy"))
        merge(value.get("parameters"))
        slot = value.get("strategy_slot")
        if isinstance(slot, Mapping):
            merge(slot.get("strategy"))
        markets = value.get("markets")
        if isinstance(markets, list):
            for market in markets:
                if not isinstance(market, Mapping):
                    continue
                slots = market.get("strategies")
                if not isinstance(slots, list):
                    continue
                for portfolio_slot in slots:
                    if isinstance(portfolio_slot, Mapping):
                        merge(portfolio_slot.get("strategy"))

    visit(execution_spec)
    return resolved


def prepare_runtime_strategy_spec(
    module: object,
    execution_spec: Mapping[str, Any],
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    """Bind defaults and validate parameters before a process strategy starts.

    Runtime class strategies reuse the file strategy manifest contract. A
    warmup-only declaration remains supported for compatibility with early
    ``BaseStrategy`` class adapters.
    """
    if not isinstance(execution_spec, Mapping):
        raise TypeError("strategy execution spec must be an object")

    strategy_class = _declared_strategy_class(module)
    raw_manifest = getattr(module, "STRATEGY_MANIFEST", None)
    class_manifest = (
        getattr(strategy_class, "STRATEGY_MANIFEST", None)
        if strategy_class is not None
        else None
    )
    if raw_manifest is not None and class_manifest is not None and raw_manifest != class_manifest:
        raise ValueError(
            "module STRATEGY_MANIFEST conflicts with STRATEGY_CLASS.STRATEGY_MANIFEST"
        )
    if raw_manifest is None:
        raw_manifest = class_manifest

    validator = declared_config_validator(module)
    if raw_manifest is None:
        if validator is not None:
            raise ValueError("validate_config requires a complete strategy manifest")
        return copy.deepcopy(dict(execution_spec)), None

    if isinstance(raw_manifest, Mapping) and set(raw_manifest) == {"warmup_bars"}:
        warmup_bars = raw_manifest["warmup_bars"]
        if (
            isinstance(warmup_bars, bool)
            or not isinstance(warmup_bars, int)
            or warmup_bars < 0
        ):
            raise ValueError("STRATEGY_MANIFEST.warmup_bars must be a non-negative integer")
        if validator is not None:
            raise ValueError("validate_config requires a complete strategy manifest")
        return copy.deepcopy(dict(execution_spec)), {"warmup_bars": warmup_bars}

    module_id = getattr(module, "STRATEGY_ID", None) or getattr(
        strategy_class, "STRATEGY_ID", None
    )
    module_version = getattr(module, "STRATEGY_VERSION", None) or getattr(
        strategy_class, "STRATEGY_VERSION", None
    )
    fallback_id = "runtime-strategy"
    module_path = getattr(module, "__file__", None)
    if isinstance(module_path, str) and module_path.strip():
        fallback_id = Path(module_path).stem
    if isinstance(raw_manifest, Mapping):
        module_id = module_id or raw_manifest.get("strategy_id")
        module_version = module_version or raw_manifest.get("strategy_version")
    strategy_id = str(module_id or fallback_id)
    strategy_version = str(module_version or "local")
    manifest = normalize_manifest(
        dict(raw_manifest) if isinstance(raw_manifest, Mapping) else raw_manifest,
        strategy_id=strategy_id,
        strategy_version=strategy_version,
        dependencies=getattr(module, "STRATEGY_DEPENDENCIES", ()),
    )

    bound_spec = apply_parameter_defaults(execution_spec, manifest)
    parameters = resolve_strategy_parameters(bound_spec, manifest)
    for name, definition in manifest["parameters"].items():
        if name not in parameters:
            if definition.get("required", False):
                raise ValueError(f"required strategy parameter {name!r} is missing")
            continue
        _validate_parameter_value(name, definition, parameters[name])
    if validator is not None:
        if manifest.get("completeness") != "complete":
            raise ValueError("validate_config requires a complete strategy manifest")
        try:
            result = validator(MappingProxyType(copy.deepcopy(parameters)))
        except Exception as exc:
            raise ValueError(
                "validate_config(config) rejected strategy parameters: "
                f"{type(exc).__name__}: {exc}"
            ) from exc
        if result is not None:
            raise ValueError(
                "validate_config(config) must return None; raise ValueError to reject config"
            )
    return bound_spec, manifest


def build_module_manifest(module: object, strategy_path: Path, bundle_sha256: str) -> dict[str, Any]:
    """Create a deterministic public manifest from a loaded user strategy module."""
    raw = getattr(module, "STRATEGY_MANIFEST", None)
    strategy_class = _declared_strategy_class(module)
    class_manifest = (
        getattr(strategy_class, "STRATEGY_MANIFEST", None)
        if strategy_class is not None
        else None
    )
    if raw is not None and class_manifest is not None and raw != class_manifest:
        raise ValueError(
            "module STRATEGY_MANIFEST conflicts with STRATEGY_CLASS.STRATEGY_MANIFEST"
        )
    if raw is None:
        raw = class_manifest
    # Validate duplicate validator declarations during inspect as well as execution.
    declared_config_validator(module)
    module_id = getattr(module, "STRATEGY_ID", None) or getattr(
        strategy_class, "STRATEGY_ID", None
    )
    module_version = getattr(module, "STRATEGY_VERSION", None) or getattr(
        strategy_class, "STRATEGY_VERSION", None
    )
    fallback_id = strategy_path.stem
    if isinstance(raw, dict):
        fallback_id = raw.get("strategy_id", fallback_id)
        module_version = module_version or raw.get("strategy_version")
    strategy_id = module_id or fallback_id
    strategy_version = module_version or "local"
    normalized = normalize_manifest(
        raw,
        strategy_id=str(strategy_id),
        strategy_version=str(strategy_version),
        dependencies=getattr(module, "STRATEGY_DEPENDENCIES", ()),
    )
    return bind_code_identity(normalized, strategy_path, bundle_sha256)
