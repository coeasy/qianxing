"""Redacted plan provenance from normalized module manifests and native runtime bindings."""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Iterator

from qianxing_strategy.manifest import apply_parameter_defaults

__all__ = ["explain_local_configuration", "explain_server_configuration"]

_REDACTED = "[REDACTED]"
_MISSING = object()
_ConfigPath = tuple[str | int, ...]


def _json_path(path: _ConfigPath, prefix: str = "$") -> str:
    for key in path:
        if isinstance(key, int):
            prefix += f"[{key}]"
        elif key.isidentifier():
            prefix += "." + key
        else:
            prefix += "[" + json.dumps(key, ensure_ascii=False) + "]"
    return prefix


def _sensitive(key: str) -> bool:
    compact = "".join(char for char in key.lower() if char.isalnum())
    return any(part in compact for part in (
        "credential", "password", "passwd", "secret", "token", "apikey",
        "privatekey", "authorization", "accesskey",
    ))


def _redact(value: Any, key: str = "") -> Any:
    if _sensitive(key):
        return _REDACTED
    if isinstance(value, dict):
        return {name: _redact(item, str(name)) for name, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_redact(item) for item in value]
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return _REDACTED


def _source(kind: str, file: Path, path: _ConfigPath) -> str:
    return f"{kind}:{file}#{_json_path(path)}"


def _row(value: Any, source: str, *, key: str = "") -> dict[str, Any]:
    return {"value": _redact(value, key), "source": source, "overridden": []}


def _chain_row(observations: list[dict[str, Any]], *, key: str = "") -> dict[str, Any]:
    def safe(item):
        return {
            name: content if name == "source" else _redact(content, key)
            for name, content in item.items()
        }

    return {**safe(observations[-1]), "overridden": [safe(item) for item in observations[:-1]]}


def _project_path(root: Path, raw: str | Path) -> str:
    path = Path(raw).expanduser()
    return str((root / path).resolve() if not path.is_absolute() else path.resolve())


def _path_row(
    root: Path, manifest: dict, field: str, resolved: Path, override: str | Path | None,
    manifest_path: Path,
) -> dict[str, Any]:
    observations = []
    if manifest.get(field) is not None:
        original = str(manifest[field])
        observations.append({
            "value": _project_path(root, original), "original_value": original,
            "source": _source("project_manifest", manifest_path, (field,)),
        })
    if override is not None:
        observations.append({
            "value": str(resolved), "original_value": str(override),
            "source": "flag:--" + field.replace("_", "-"),
        })
    if not observations:
        return _row(str(resolved), f"unknown:resolved_{field}")
    # The plan has already resolved the winning path (local flags are cwd-relative).
    observations[-1]["value"] = str(resolved)
    return _chain_row(observations)


def _at(value: Any, path: _ConfigPath) -> Any:
    for key in path:
        if isinstance(value, dict) and key in value:
            value = value[key]
        elif isinstance(value, (list, tuple)) and isinstance(key, int) and 0 <= key < len(value):
            value = value[key]
        else:
            return _MISSING
    return value


def _leaves(value: Any, path: _ConfigPath = ()) -> Iterator[tuple[_ConfigPath, Any]]:
    # Stop at sensitive containers, so neither their contents nor their child keys leak.
    if path and isinstance(path[-1], str) and _sensitive(path[-1]):
        yield path, value
    elif isinstance(value, dict) and value:
        for key, item in value.items():
            yield from _leaves(item, (*path, key))
    elif isinstance(value, list) and value:
        for index, item in enumerate(value):
            yield from _leaves(item, (*path, index))
    else:
        yield path, value


def explain_local_configuration(
    *, root: Path, manifest: dict, raw_spec: dict, module_manifest: dict,
    selected_id: str | None, resolved_spec: dict, strategy_file: Path, input_path: Path,
    input_spec_override=None, strategy_file_override=None, manifest_path: Path | None = None,
) -> dict:
    """Explain the effective spec, using the real parameter binder only for tracing."""
    manifest_path = manifest_path or root / "qianxing.strategy.json"
    trace: dict[_ConfigPath, list[dict[str, Any]]] = {}
    apply_parameter_defaults(
        raw_spec, module_manifest, target_strategy_id=selected_id, parameter_trace=trace,
    )
    rows = {
        "backend": _row("local", "local_project_runner"),
        "execution_boundary": _row("trusted_local_python", "local_project_runner"),
        "strategy_file": _path_row(root, manifest, "strategy_file", strategy_file, strategy_file_override, manifest_path),
        "input_spec": _path_row(root, manifest, "input_spec", input_path, input_spec_override, manifest_path),
    }
    for field in ("strategy_id", "strategy_version"):
        observations = []
        for kind, file, values in (
            ("module_manifest", strategy_file, module_manifest),
            ("project_manifest", manifest_path, manifest),
        ):
            if values.get(field):
                observations.append({"value": values[field], "source": _source(kind, file, (field,))})
        effective = selected_id if field == "strategy_id" else (
            manifest.get(field) or module_manifest.get(field)
        )
        if not observations or observations[-1]["value"] != effective:
            observations.append({"value": effective, "source": f"unknown:selected_{field}"})
        rows[field] = _chain_row(observations)

    for path, value in _leaves(resolved_spec):
        observations = []
        for length in range(len(path), 0, -1):
            events = trace.get(path[:length])
            if events is None:
                continue
            suffix = path[length:]
            for event in events:
                original = _at(event["value"], suffix)
                if original is not _MISSING:
                    kind = event["source"]
                    file = strategy_file if kind == "module_manifest" else input_path
                    observations.append({
                        "value": original,
                        "source": _source(kind, file, (*event["path"], *suffix)),
                    })
            break
        if not observations:
            original = _at(raw_spec, path)
            if original is not _MISSING:
                observations.append({"value": original, "source": _source("input_spec", input_path, path)})
        key = str(path[-1]) if path else ""
        if not observations:
            rows[_json_path(path, "spec")] = _row(value, "unknown:resolved_spec#" + _json_path(path), key=key)
            continue
        original = observations[-1]["value"]
        # Only attribute a changed value to path resolution when the supplied result
        # is exactly that resolution. Other unexplained changes get an honest source.
        is_path = False
        if isinstance(value, str) and isinstance(original, str) and not _sensitive(key):
            try:
                is_path = Path(value).is_absolute() and value == _project_path(root, original)
            except (OSError, ValueError):
                # Arbitrary spec strings need not be valid filesystem paths.
                pass
        if value != original and not is_path:
            observations.append({"value": value, "source": "unknown:resolved_spec#" + _json_path(path)})
        row = _chain_row(observations, key=key)
        if is_path:
            row.update(value=value, original_value=original, resolution="project_relative_path")
        rows[_json_path(path, "spec")] = row
    return rows


def _selected_raw(
    runtime: dict, binding: dict,
) -> tuple[dict | None, _ConfigPath, dict | None, _ConfigPath]:
    worker_id = binding.get("worker", {}).get("id")
    workers = runtime.get("workers", [])
    matches = [
        (worker, ("workers", index)) for index, worker in enumerate(workers)
        if isinstance(worker, dict) and worker_id is not None and worker.get("id") == worker_id
    ] if isinstance(workers, list) else []
    raw_worker, worker_path = matches[0] if len(matches) == 1 else (None, ())
    strategies = runtime.get("strategies")
    if isinstance(strategies, list) and strategies:
        matches = [
            (strategy, ("strategies", index)) for index, strategy in enumerate(strategies)
            if isinstance(strategy, dict) and worker_id is not None and strategy.get("id") == worker_id
        ]
        raw_strategy, strategy_path = matches[0] if len(matches) == 1 else (None, ())
    elif strategies is None or strategies == []:
        # An absent legacy strategy object is itself defaulted by Rust.
        raw_strategy, strategy_path = runtime.get("strategy", {}), ("strategy",)
        if not isinstance(raw_strategy, dict):
            raw_strategy = None
    else:
        raw_strategy, strategy_path = None, ()
    return raw_worker, worker_path, raw_strategy, strategy_path


def _safe_server_value(field: str, value: Any) -> Any:
    # Do not expose arbitrary structures smuggled under otherwise allowlisted fields.
    if field == "enabled":
        valid = value is None or isinstance(value, bool)
    elif field == "symbols":
        valid = isinstance(value, list) and all(isinstance(item, str) for item in value)
    else:
        valid = value is None or isinstance(value, str)
    return _redact(value) if valid else _REDACTED


def _runtime_row(
    runtime_path: Path, owner: str, field: str, effective: dict, raw: dict | None,
    raw_path: _ConfigPath, *, defaulted: bool,
) -> dict[str, Any]:
    value = effective.get(field)
    source = f"unknown:native_binding#$.{owner}.{field}"
    original = _MISSING
    if field in effective and raw is not None:
        if field in raw:
            if raw[field] == value:
                source = _source("runtime_config", runtime_path, (*raw_path, field))
            else:
                original = raw[field]
        elif defaulted:
            rust_type = "StrategyRuntimeConfig" if owner == "strategy" else "WorkerConfig"
            source = f"rust_default:{rust_type}.{field}"
    row = _row(_safe_server_value(field, value), source)
    if original is not _MISSING:
        row["original_value"] = _safe_server_value(field, original)
        row["original_source"] = _source("runtime_config", runtime_path, (*raw_path, field))
    return row


def explain_server_configuration(
    *, root: Path, manifest: dict, runtime: dict, binding: dict, runtime_path: Path,
    runtime_config: str | None, worker_id: str | None, manifest_path: Path | None = None,
) -> dict:
    """Explain selectors and only allowlisted fields of a native, read-only plan."""
    manifest_path = manifest_path or root / "qianxing.strategy.json"
    rows = {"runtime_config": _path_row(root, manifest, "runtime_config", runtime_path, runtime_config or None, manifest_path)}
    environment_source = (
        _source("runtime_config", runtime_path, ("environment",))
        if "environment" in runtime else "unknown:runtime.environment"
    )
    rows["environment"] = _row(_safe_server_value("environment", runtime.get("environment")), environment_source)
    raw_worker, worker_path, raw_strategy, strategy_path = _selected_raw(runtime, binding)
    for owner, raw, path, fields in (
        ("strategy", raw_strategy, strategy_path,
         ("id", "version", "strategy_artifact_sha256", "account_id", "venue_id", "instrument")),
        ("worker", raw_worker, worker_path,
         ("id", "role", "enabled", "account_id", "venue_id", "symbols")),
    ):
        for field in fields:
            public_field = "artifact_sha256" if field == "strategy_artifact_sha256" else field
            rows[f"{owner}.{public_field}"] = _runtime_row(
                runtime_path, owner, field, binding.get(owner, {}), raw, path,
                defaulted=owner == "strategy" or field in {"account_id", "venue_id", "symbols"},
            )
    worker = rows["worker.id"]
    observations = [{"value": worker["value"], "source": worker["source"]}]
    if manifest.get("worker_id"):
        observations.append({
            "value": _safe_server_value("id", manifest["worker_id"]),
            "source": _source("project_manifest", manifest_path, ("worker_id",)),
        })
    if worker_id:
        observations.append({"value": worker_id, "source": "flag:--worker-id"})
    if observations[-1]["value"] != worker["value"]:
        observations.append({"value": worker["value"], "source": "unknown:native_binding#$.worker.id"})
    rows["worker_id"] = _chain_row(observations)
    return rows
