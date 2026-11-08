"""Project-first local strategy workflow and read-only server deployment plans."""

from __future__ import annotations

import ast
import hashlib
import json
import copy
import importlib.util
import inspect
import re
from pathlib import Path
from pathlib import PurePosixPath
import subprocess
import sys
import threading
from types import MappingProxyType
from typing import Any, Mapping

from .process_tree import run_process_tree
from qianxing_strategy.manifest import (
    apply_parameter_defaults,
    build_module_manifest,
    declared_config_validator,
    resolve_strategy_parameters,
)

PROJECT_FILE = "qianxing.strategy.json"
PROJECT_SCHEMA_VERSION = 1
PROJECT_KIND = "qianxing-strategy-project"
_PROJECT_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
_REQUIRED = {
    "schema_version",
    "kind",
    "project_id",
    "strategy_file",
    "input_spec",
}
_OPTIONAL = {"strategy_id", "strategy_version", "runtime_config", "worker_id", "data"}
_STRATEGY_IMPORT_LOCK = threading.RLock()
_STRATEGY_IMPORT_ROOTS: set[Path] = set()


def _module_is_under_strategy_root(module: object, roots: set[Path]) -> bool:
    module_path = getattr(module, "__file__", None)
    if not isinstance(module_path, str) or not module_path.endswith(".py"):
        return False
    try:
        resolved = Path(module_path).resolve(strict=True)
    except OSError:
        return False
    for root in roots:
        try:
            resolved.relative_to(root)
        except ValueError:
            continue
        return True
    return False


def _json_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ValueError(f"cannot read {label} {str(path)!r}: {error}") from error
    except json.JSONDecodeError as error:
        raise ValueError(f"{label} JSON is invalid at line {error.lineno}: {error.msg}") from error
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def _relative_or_absolute(root: Path, raw: str | None, label: str) -> str | None:
    if raw is None:
        return None
    source = Path(raw).expanduser().resolve(strict=True)
    try:
        return source.relative_to(root).as_posix()
    except ValueError:
        return str(source)


def _resolve(root: Path, raw: str, label: str) -> Path:
    path = Path(raw).expanduser()
    resolved = (root / path).resolve() if not path.is_absolute() else path.resolve()
    if not resolved.is_file():
        raise ValueError(f"{label} does not exist or is not a file: {resolved}")
    return resolved


_INPUT_PATH_KEYS = {
    "path", "file", "file_path", "input_path", "source_path", "data_path",
    "bars_path", "ticks_path", "trades_path", "book_path", "csv_path",
    "depth_path", "quality_manifest_path", "dataset_snapshot_path",
}


def _resolve_input_paths(root: Path, spec: dict[str, Any]) -> tuple[dict[str, Any], list[str]]:
    """Return a copy with project-relative data references made absolute."""
    resolved_spec = copy.deepcopy(spec)
    failures: list[str] = []

    def visit(value: Any) -> None:
        if isinstance(value, dict):
            for key, item in value.items():
                if key in _INPUT_PATH_KEYS and isinstance(item, str):
                    candidate = Path(item).expanduser()
                    resolved = candidate.resolve() if candidate.is_absolute() else (root / candidate).resolve()
                    if not resolved.is_file():
                        failures.append(str(resolved))
                    value[key] = str(resolved)
                else:
                    visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)

    visit(resolved_spec)
    return resolved_spec, failures


_DEMO_STRATEGY = '''"""Offline learning example, not an investment recommendation."""
from qianxing_strategy import BaseStrategy

STRATEGY_ID = "demo-sma-cross"
STRATEGY_VERSION = "0.1.0"
FAST, SLOW = 5, 20
STRATEGY_MANIFEST = {
    "schema_version": 1, "parameters": {}, "data_level": "bar",
    "warmup_bars": SLOW + 1,
    "instrument_requirements": {"requires_explicit_spec": False},
}


class DemoSmaCross(BaseStrategy):
    def on_bar(self, context, bar):
        instrument = bar["instrument"]
        rows = self.history(context, instrument, length=SLOW + 1, fields=("close_raw",))
        intents = []
        if len(rows) == SLOW + 1:
            closes = [row["close_raw"] for row in rows]
            previous = closes[:-1]
            before = sum(previous[-FAST:]) * SLOW - sum(previous[-SLOW:]) * FAST
            current = sum(closes[-FAST:]) * SLOW - sum(closes[-SLOW:]) * FAST
            position = self.position_qty(context, instrument)
            if before <= 0 < current and position == 0:
                available = context["available_margin_raw"]
                quantity = max(0, available) * 9500 // (10000 * closes[-1]) * 10 ** 9
                if quantity:
                    intents.append(self.buy(instrument, qty_raw=quantity))
            elif before >= 0 > current and position > 0:
                intent = self.close_position(context, instrument)
                if intent is not None:
                    intents.append(intent)
        return self.decision(context, "Bar", bar, intents=intents)


STRATEGY_CLASS = DemoSmaCross
'''


def _write_sample_dataset(directory: Path, *, instrument: str | None, rows: int) -> dict[str, Any]:
    from .sample_data import SAMPLE_INSTRUMENT, write_sample_bundle

    return write_sample_bundle(
        directory, instrument=instrument or SAMPLE_INSTRUMENT, rows=rows
    )


def _write_fetched_dataset(
    directory: Path,
    *,
    source: str,
    instrument: str,
    start: str,
    end: str,
    frequency: str,
    adjustment: str,
) -> dict[str, Any]:
    """真实取数：走 qianxing_ashare 的同一条 fetch 路径，并登记血缘。"""
    from qianxing_ashare import AshareQuery, fetch_bars

    from . import dataset_bundle

    result = fetch_bars(
        AshareQuery(instrument, start, end, frequency, adjustment), source_name=source
    )
    frame = result.frame
    context = dataset_bundle.source_context(
        frame.instrument,
        frequency=frequency,
        adjustment=adjustment,
        gaps=dataset_bundle.frame_gaps(frame, frequency),
    )
    manifest = result.manifest
    return dataset_bundle.write_dataset_bundle(
        directory,
        frame,
        context,
        {
            "is_sample": False,
            "source": manifest.provider,
            "source_kind": "provider",
            "provider": manifest.provider,
            "provider_version": manifest.provider_version,
            "source_hash": manifest.source_hash,
            "received_at": manifest.received_at,
            "frequency": manifest.frequency,
            "adjustment": manifest.adjustment,
            "start": manifest.start,
            "end": manifest.end,
            "cached": result.cached,
        },
        dataset_id=f"ashare-{frame.instrument}-{frequency}",
        version="1",
    )


def _write_demo_inputs(
    root: Path,
    *,
    source: str | None,
    instrument: str | None,
    start: str | None,
    end: str | None,
    cash: str | int | float,
    currency: str | None,
    frequency: str,
    adjustment: str,
    rows: int,
    seed: int,
    force: bool,
) -> dict[str, Any]:
    """写 ``data/`` 数据集包与 ``input.json``，返回血缘摘要。"""
    from . import run_spec
    from qianxing_ashare.sources import resolve_source

    selected = resolve_source(explicit=source, default="sample")
    sample = selected.name == "sample"
    initial_cash_raw = run_spec.parse_cash(cash)
    if type(seed) is not int or not 0 <= seed < 2 ** 64:
        raise ValueError("seed must be an unsigned 64-bit integer")
    if sample and (start or end or frequency != "daily" or adjustment != "none"):
        raise ValueError("sample uses its fixed daily, unadjusted date range; omit date/frequency/adjustment flags")
    if sample and (type(rows) is not int or rows < 22):
        raise ValueError("demo needs at least 22 rows for warmup and one trading decision")
    if not sample and not (instrument and start and end):
        raise ValueError("--source requires --instrument/--start/--end")
    from .sample_data import SAMPLE_INSTRUMENT
    if not sample:
        from qianxing_ashare import normalize_instrument
        instrument = normalize_instrument(instrument)
    settlement = currency or run_spec.settlement_currency(instrument or SAMPLE_INSTRUMENT)
    data_dir = root / "data"
    if data_dir.exists() and any(data_dir.iterdir()) and not force:
        raise ValueError(f"data directory is not empty: {data_dir}; use --force to replace it")
    if (root / "input.json").exists() and not force:
        raise ValueError("input.json already exists; use --force to replace it")
    if sample:
        bundle = _write_sample_dataset(data_dir, instrument=instrument, rows=rows)
    else:
        bundle = _write_fetched_dataset(
            data_dir,
            source=selected.name,
            instrument=instrument or "",
            start=start or "",
            end=end or "",
            frequency=frequency,
            adjustment=adjustment,
        )
    spec = run_spec.compose_run_spec(
        instrument=bundle["instrument"],
        bars_path=data_dir / bundle["bars_path"],
        dataset_snapshot_path=data_dir / bundle["snapshot_path"],
        output_dir=root / "runs" / "artifacts",
        initial_cash_raw=initial_cash_raw,
        strategy_config={
            "kind": "sma_cross",
            "strategy_id": "demo-sma-cross",
            "fast_window": 5,
            "slow_window": 20,
            "quantity_raw": 10 ** 9,
        },
        run_id="demo" if sample else "fetch",
        settlement_currency=settlement,
        seed=seed,
    )
    for field in ("bars_path", "dataset_snapshot_path", "output_dir"):
        spec[field] = Path(spec[field]).relative_to(root).as_posix()
    run_spec.write_run_spec(root, spec)
    return {
        "is_sample": bundle["is_sample"],
        "source": bundle.get("source"),
        "instrument": bundle["instrument"],
        "rows": bundle["rows"],
        "license": bundle.get("license"),
        "data_directory": str(data_dir),
        "input_spec": "input.json",
        "dataset_snapshot": str(data_dir / bundle["snapshot_path"]),
    }


def init_strategy_project(
    directory: str | Path,
    *,
    strategy_file: str | None = None,
    input_spec: str | None = None,
    runtime_config: str | None = None,
    worker_id: str | None = None,
    force: bool = False,
    demo: bool = False,
    source: str | None = None,
    instrument: str | None = None,
    start: str | None = None,
    end: str | None = None,
    cash: str | int | float = "100000",
    currency: str | None = None,
    frequency: str = "daily",
    adjustment: str = "none",
    rows: int = 600,
    seed: int = 20261030,
) -> dict[str, Any]:
    """Create a small project scaffold; optional input paths are linked, not copied.

    ``demo=True``（或给了 ``source``）时额外写一份内置样例/真实取数的数据集包与
    ``input.json``，使 ``project doctor`` 与 ``project run`` 建完即可通过（方案 D2/D14）。
    """
    demo_mode = demo or source is not None
    if demo_mode and (input_spec or strategy_file):
        raise ValueError("--demo/--source generate their own strategy and input; omit --input-spec/--strategy-file")
    root = Path(directory).expanduser().resolve()
    if root.exists() and not root.is_dir():
        raise ValueError(f"project path is not a directory: {root}")
    root.mkdir(parents=True, exist_ok=True)
    manifest_path = root / PROJECT_FILE
    if manifest_path.exists() and not force:
        raise ValueError(f"project manifest already exists: {manifest_path}; use --force to replace it")

    strategies = root / "strategies"
    strategies.mkdir(exist_ok=True)
    template_path = strategies / "main.py"
    if demo_mode and template_path.exists() and not force:
        raise ValueError(f"strategy file already exists: {template_path}; use --force to replace it")
    if not demo_mode and not template_path.exists():
        template_path.write_text(
            '''"""Starter strategy for qianxing run-strategy."""\n\nfrom qianxing_strategy import BaseStrategy\n\n\nclass Strategy(BaseStrategy):\n    STRATEGY_ID = "my-strategy"\n    STRATEGY_VERSION = "0.1.0"\n\n    def on_bar(self, context, bar):\n        # Return no order until your signal and sizing rules are ready.\n        return self.decision(context, "Bar", bar)\n\n\nSTRATEGY_CLASS = Strategy\n''',
            encoding="utf-8",
            newline="\n",
        )

    strategy_value = _relative_or_absolute(root, strategy_file, "strategy file") if strategy_file else "strategies/main.py"
    input_value = _relative_or_absolute(root, input_spec, "input spec") if input_spec else None
    runtime_value = _relative_or_absolute(root, runtime_config, "runtime config") if runtime_config else None
    project_id = root.name or "qianxing-strategy"
    if not _PROJECT_ID.fullmatch(project_id):
        project_id = re.sub(r"[^A-Za-z0-9._-]+", "-", project_id).strip("-.")[:128]
        if not project_id or not project_id[0].isalnum():
            project_id = f"qianxing-{project_id or 'strategy'}"

    data = None
    if demo_mode:
        data = _write_demo_inputs(
            root,
            source=source,
            instrument=instrument,
            start=start,
            end=end,
            cash=cash,
            currency=currency,
            frequency=frequency,
            adjustment=adjustment,
            rows=rows,
            seed=seed,
            force=force,
        )
        input_value = data["input_spec"]
        template_path.write_text(_DEMO_STRATEGY, encoding="utf-8", newline="\n")

    manifest = {
        "schema_version": PROJECT_SCHEMA_VERSION,
        "kind": PROJECT_KIND,
        "project_id": project_id,
        "strategy_file": strategy_value,
        "input_spec": input_value,
    }
    if worker_id is not None:
        manifest["worker_id"] = worker_id
    if runtime_value is not None:
        manifest["runtime_config"] = runtime_value
    if data is not None:
        manifest["data"] = {"source": data["source"]}
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    ignore_path = root / ".gitignore"
    if not ignore_path.exists():
        ignore_path.write_text(".qianxing/\nruns/\n__pycache__/\n", encoding="utf-8", newline="\n")
    result = {"status": "created", "project_root": str(root), "manifest": str(manifest_path), "strategy_file": strategy_value, "input_spec": input_value}
    if data is not None:
        result["data"] = data
    return result


def load_strategy_project(path: str | Path) -> tuple[Path, dict[str, Any]]:
    requested = Path(path).expanduser().resolve()
    root = requested.parent if requested.is_file() else requested
    manifest_path = requested if requested.is_file() else root / PROJECT_FILE
    manifest = _json_object(manifest_path, "strategy project manifest")
    unknown = set(manifest) - _REQUIRED - _OPTIONAL
    missing = _REQUIRED - set(manifest)
    if unknown or missing:
        raise ValueError(
            "strategy project manifest fields are invalid"
            + (f"; missing={sorted(missing)}" if missing else "")
            + (f"; unknown={sorted(unknown)}" if unknown else "")
        )
    from .app import validate_strategy_project_v1

    manifest = validate_strategy_project_v1(manifest)
    if manifest.get("schema_version") != PROJECT_SCHEMA_VERSION or manifest.get("kind") != PROJECT_KIND:
        raise ValueError("unsupported strategy project manifest kind or schema_version")
    for field in ("strategy_file", "input_spec"):
        value = manifest.get(field)
        if value is not None and (not isinstance(value, str) or not value.strip()):
            raise ValueError(f"{field} must be a non-empty path or null")
    for field in ("strategy_id", "strategy_version", "runtime_config", "worker_id"):
        value = manifest.get(field)
        if value is not None and (not isinstance(value, str) or not value.strip()):
            raise ValueError(f"{field} must be non-empty text when provided")
    data_source = (manifest.get("data") or {}).get("source")
    if data_source is not None and data_source.strip().lower() != "auto":
        from qianxing_ashare.sources import source
        source(data_source)
    return root, manifest


def show_strategy_project(path: str | Path) -> dict[str, Any]:
    """Show project metadata and resolved file locations without executing code."""
    requested = Path(path).expanduser().resolve()
    root, manifest = load_strategy_project(path)
    resolved_paths: dict[str, dict[str, Any] | None] = {}
    for field in ("strategy_file", "input_spec", "runtime_config"):
        raw = manifest.get(field)
        if raw is None:
            resolved_paths[field] = None
            continue
        candidate = Path(raw).expanduser()
        resolved = (root / candidate).resolve() if not candidate.is_absolute() else candidate.resolve()
        resolved_paths[field] = {"path": str(resolved), "exists": resolved.is_file()}
    lock_path = root / "requirements.lock"
    if lock_path.exists() or lock_path.is_symlink():
        try:
            from .project_runs import _project_dependency_lock_path

            resolved_lock = _project_dependency_lock_path(root)
            resolved_paths["dependency_lock"] = {
                "path": str(resolved_lock),
                "exists": True,
                "sha256": hashlib.sha256(resolved_lock.read_bytes()).hexdigest(),
            }
        except (OSError, ValueError) as error:
            resolved_paths["dependency_lock"] = {"path": str(lock_path), "exists": False}
            raise ValueError(f"project requirements.lock is invalid: {error}") from error
    else:
        resolved_paths["dependency_lock"] = None
    from .dataset_bundle import project_provenance
    input_path = resolved_paths["input_spec"]
    data = project_provenance(root, _json_object(Path(input_path["path"]), "input spec")) if (
        input_path and input_path["exists"]
    ) else []
    return {
        "status": "loaded",
        "project_root": str(root),
        "manifest_path": str(requested if requested.is_file() else root / PROJECT_FILE),
        "manifest": manifest,
        "resolved_paths": resolved_paths,
        "datasets": data,
    }


def static_project_doctor(path: str | Path) -> dict[str, Any]:
    """Inspect project metadata and local files without importing user code."""
    root, manifest = load_strategy_project(path)
    issues: list[dict[str, str]] = []
    strategy_path: Path | None = None
    strategy_value = manifest.get("strategy_file")
    if isinstance(strategy_value, str) and strategy_value.strip():
        candidate = Path(strategy_value).expanduser()
        strategy_path = (root / candidate).resolve() if not candidate.is_absolute() else candidate.resolve()
    if strategy_path is None or not strategy_path.is_file():
        issues.append({"code": "STRATEGY_FILE_MISSING", "message": "strategy_file is not a readable local file"})
        strategy_path = None
    else:
        try:
            ast.parse(strategy_path.read_text(encoding="utf-8"), filename=str(strategy_path))
        except (OSError, UnicodeError, SyntaxError) as error:
            issues.append({"code": "STRATEGY_SOURCE_INVALID", "message": str(error)})

    input_value = manifest.get("input_spec")
    input_path: Path | None = None
    if not input_value:
        issues.append({"code": "INPUT_SPEC_MISSING", "message": "input_spec is not configured"})
    else:
        try:
            input_path = _resolve(root, input_value, "input spec")
            spec = _json_object(input_path, "strategy input spec")
            _resolved, failures = _resolve_input_paths(root, spec)
            if failures:
                issues.append({
                    "code": "INPUT_FILE_MISSING",
                    "message": "input spec references missing local file(s): " + ", ".join(failures[:8]),
                })
        except (OSError, TypeError, ValueError) as error:
            issues.append({"code": "INPUT_SPEC_INVALID", "message": str(error)})

    runtime_path: Path | None = None
    if manifest.get("runtime_config"):
        try:
            runtime_path = _resolve(root, manifest["runtime_config"], "runtime config")
            _json_object(runtime_path, "runtime config")
        except (OSError, TypeError, ValueError) as error:
            issues.append({"code": "RUNTIME_CONFIG_INVALID", "message": str(error)})

    dependency_lock_path: Path | None = None
    dependency_lock_sha256: str | None = None
    dependency_lock_requirements: list[dict[str, Any]] | None = None
    dependency_lock_requirements_sha256: str | None = None
    wheelhouse_path = root / "wheelhouse"
    wheelhouse_exists = wheelhouse_path.is_dir()
    hash_pinned_distributions: list[str] = []
    try:
        from .project_runs import (
            _dependency_lock_bindings_sha256,
            _installed_distribution_versions,
            _parse_dependency_lock,
            _project_dependency_lock_path,
        )

        dependency_lock_path = _project_dependency_lock_path(root)
        if dependency_lock_path is not None:
            lock_bytes = dependency_lock_path.read_bytes()
            dependency_lock_sha256 = hashlib.sha256(lock_bytes).hexdigest()
            requirements = _parse_dependency_lock(lock_bytes)
            installed = _installed_distribution_versions()
            dependency_lock_requirements = [
                {**requirement, "installed_version": installed.get(requirement["name"])}
                for requirement in requirements
            ]
            hash_pinned_distributions = [
                requirement["name"] for requirement in requirements if requirement["wheel_hashes"]
            ]
            dependency_lock_requirements_sha256 = _dependency_lock_bindings_sha256(requirements)
            mismatches = [
                binding for binding in dependency_lock_requirements
                if binding["version"] != binding["installed_version"]
            ]
            if mismatches:
                issues.append({
                    "code": "DEPENDENCY_LOCK_ENVIRONMENT_MISMATCH",
                    "message": "requirements.lock pins do not match installed distributions: "
                    + ", ".join(
                        f"{item['name']}={item['version']} (installed {item['installed_version'] or 'missing'})"
                        for item in mismatches[:8]
                    ),
                })
            if hash_pinned_distributions and not wheelhouse_exists:
                issues.append({
                    "code": "DEPENDENCY_WHEELHOUSE_MISSING",
                    "message": "hash-pinned requirements require a project-root wheelhouse/ directory",
                })
    except (OSError, ValueError) as error:
        issues.append({"code": "DEPENDENCY_LOCK_INVALID", "message": str(error)})

    from .app import doctor

    return {
        "status": "ready" if not issues else "blocked",
        "profile": "local",
        "project_id": manifest["project_id"],
        "project_root": str(root),
        "user_code_executed": False,
        "execution_boundary": "static_metadata_and_syntax_only",
        "checks": {
            "strategy_file": {"path": str(strategy_path) if strategy_path else None,
                              "exists": strategy_path is not None,
                              "syntax_checked": strategy_path is not None},
            "input_spec": {"path": str(input_path) if input_path else None, "exists": input_path is not None},
            "runtime_config": {"path": str(runtime_path) if runtime_path else None,
                               "configured": manifest.get("runtime_config") is not None,
                               "valid": runtime_path is not None or manifest.get("runtime_config") is None},
            "dependency_lock": {
                "path": str(dependency_lock_path) if dependency_lock_path else None,
                "configured": dependency_lock_path is not None,
                "sha256": dependency_lock_sha256,
                "requirements": dependency_lock_requirements,
                "requirements_sha256": dependency_lock_requirements_sha256,
                "wheelhouse_path": str(wheelhouse_path) if hash_pinned_distributions else None,
                "wheelhouse_exists": wheelhouse_exists if hash_pinned_distributions else None,
                "hash_pinned_distributions": hash_pinned_distributions,
            },
            "environment": doctor(),
        },
        "issues": issues,
        "dynamic_check": "not_run; pass --dynamic to execute the strategy preflight in a timed subprocess",
    }


def dynamic_project_doctor(path: str | Path, *, timeout_seconds: int = 30) -> dict[str, Any]:
    """Run the existing strategy preflight in a bounded child process."""
    if timeout_seconds < 1 or timeout_seconds > 300:
        raise ValueError("dynamic doctor timeout must be between 1 and 300 seconds")
    root, _manifest = load_strategy_project(path)
    requested = Path(path).expanduser().resolve()
    project_path = requested if requested.is_file() else root
    sdk_source_root = Path(__file__).resolve().parents[1]
    code = (
        "import json, sys; "
        "sys.path = [entry for entry in sys.path if entry and entry != sys.argv[1]]; "
        "sys.path.insert(0, sys.argv[1]); "
        "from qianxing.strategy_project import local_project_preflight; "
        "print(json.dumps(local_project_preflight(sys.argv[2]), ensure_ascii=False))"
    )
    try:
        completed = run_process_tree(
            [sys.executable, "-c", code, str(sdk_source_root), str(project_path)],
            cwd=str(root), timeout_seconds=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        return {
            "status": "timeout", "project_id": _manifest["project_id"],
            "timeout_seconds": timeout_seconds, "user_code_executed": True,
            "error": str(error),
        }
    return {
        "status": "ready" if completed.returncode == 0 else "failed",
        "project_id": _manifest["project_id"],
        "returncode": completed.returncode,
        "user_code_executed": True,
        "execution_boundary": "trusted_local_python_subprocess",
        "stdout": completed.stdout[-65536:],
        "stderr": completed.stderr[-16384:],
    }


def local_project_preflight(
    path: str | Path, *, input_spec_override: str | Path | None = None,
    strategy_file_override: str | Path | None = None, explain: bool = False,
) -> dict[str, Any]:
    """Resolve every input relative to the project, including Rust dataset reads."""
    return _local_project_preflight(
        path, input_spec_override=input_spec_override,
        strategy_file_override=strategy_file_override, explain=explain,
    )


def _strategy_bundle_sha256(
    source_path: Path, source_bytes: bytes, module: object
) -> str:
    """Hash the strategy file and its declared/eager local Python dependencies."""
    files = _strategy_bundle_files(source_path, source_bytes, module)
    digest = hashlib.sha256()
    digest.update(b"qianxing-strategy-bundle-v1\0")
    for relative, content in sorted(files.items()):
        path_bytes = relative.encode("utf-8")
        digest.update(len(path_bytes).to_bytes(4, "big"))
        digest.update(path_bytes)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def _strategy_bundle_files(
    source_path: Path, source_bytes: bytes, module: object
) -> dict[str, bytes]:
    """Return validated strategy sources using paths relative to the strategy directory."""
    from .app import DataUnavailableError, InvalidInputError

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
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    for dependency in declared:
        if not isinstance(dependency, str) or not dependency.strip():
            message = "STRATEGY_DEPENDENCIES entries must be non-empty strings"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        portable = PurePosixPath(dependency.replace("\\", "/"))
        if portable.is_absolute() or ".." in portable.parts or portable.suffix.lower() != ".py":
            message = f"invalid strategy dependency path: {dependency!r}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        add_file(root.joinpath(*portable.parts))
    loaded_modules = getattr(module, "__qianxing_project_modules__", ())
    if not isinstance(loaded_modules, tuple):
        message = "strategy project import manifest is invalid"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    for loaded_module in loaded_modules:
        module_path = getattr(loaded_module, "__file__", None)
        if not isinstance(module_path, str) or not module_path.endswith(".py"):
            continue
        try:
            relative = Path(module_path).resolve(strict=True).relative_to(root)
        except (OSError, ValueError):
            continue
        if relative.suffix.lower() == ".py":
            add_file(root / relative)
    return files


def load_strategy_file(path: str | Path):
    """Load a user strategy module without routing through the command-line interface."""
    from .app import DataUnavailableError, InvalidInputError

    source_path = Path(path).expanduser().resolve()
    if source_path.suffix.lower() != ".py":
        message = f"strategy file must be a .py file: {source_path}"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    try:
        source_bytes = source_path.read_bytes()
    except OSError as exc:
        message = f"cannot read strategy file {str(source_path)!r}: {exc}"
        raise DataUnavailableError(message, payload={"category": "DATA_UNAVAILABLE", "message": message}) from exc
    source_digest = hashlib.sha256(source_bytes).hexdigest()
    module_identity = hashlib.sha256(
        str(source_path).encode("utf-8") + b"\0" + source_bytes
    ).hexdigest()
    module_name = f"_qianxing_strategy_{module_identity[:24]}"
    module_spec = importlib.util.spec_from_file_location(module_name, source_path)
    if module_spec is None or module_spec.loader is None:
        message = f"cannot load Python strategy file: {source_path}"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    module = importlib.util.module_from_spec(module_spec)
    parent = str(source_path.parent)
    project_root = source_path.parent.resolve()
    with _STRATEGY_IMPORT_LOCK:
        _STRATEGY_IMPORT_ROOTS.add(project_root)
        for name, loaded_module in tuple(sys.modules.items()):
            if _module_is_under_strategy_root(loaded_module, _STRATEGY_IMPORT_ROOTS):
                sys.modules.pop(name, None)
        original_path = list(sys.path)
        sys.modules[module_name] = module
        sys.path.insert(0, parent)
        try:
            module_spec.loader.exec_module(module)
        except Exception as exc:
            message = f"loading Python strategy file failed: {type(exc).__name__}: {exc}"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
        finally:
            sys.path[:] = original_path
            project_modules = tuple(
                loaded_module
                for loaded_module in tuple(sys.modules.values())
                if _module_is_under_strategy_root(loaded_module, {project_root})
            )
            if module not in project_modules:
                project_modules = (module, *project_modules)
            module.__qianxing_project_modules__ = project_modules
            for name, loaded_module in tuple(sys.modules.items()):
                if _module_is_under_strategy_root(loaded_module, {project_root}):
                    sys.modules.pop(name, None)
    factory = getattr(module, "create_strategy", None)
    strategy_class = getattr(module, "STRATEGY_CLASS", None)
    if factory is None and strategy_class is not None:
        from qianxing_strategy import BaseStrategy

        if not isinstance(strategy_class, type) or not issubclass(strategy_class, BaseStrategy):
            message = "STRATEGY_CLASS must be a qianxing_strategy.BaseStrategy subclass"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
        try:
            constructor = inspect.signature(strategy_class)
        except (TypeError, ValueError) as exc:
            message = "cannot inspect STRATEGY_CLASS constructor; export create_strategy(spec: dict) instead"
            raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
        try:
            constructor.bind({})
            accepts_spec = True
        except TypeError:
            accepts_spec = False
            try:
                constructor.bind()
            except TypeError as exc:
                message = "STRATEGY_CLASS constructor must accept the strategy spec or no arguments"
                raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message}) from exc
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
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    return module, factory, _strategy_bundle_sha256(source_path, source_bytes, module)


def validate_strategy_config(
    module: object,
    manifest: Mapping[str, Any],
    execution_spec: Mapping[str, Any],
    target_strategy_id: str | None,
) -> None:
    """Run the strategy's optional pure config validator against declared parameters."""
    validator = declared_config_validator(module)
    if validator is None:
        return
    if manifest.get("completeness") != "complete":
        raise ValueError("validate_config requires a complete strategy manifest")
    config = resolve_strategy_parameters(
        execution_spec, manifest, target_strategy_id=target_strategy_id
    )
    try:
        result = validator(MappingProxyType(copy.deepcopy(config)))
    except Exception as exc:
        raise ValueError(
            f"validate_config(config) rejected strategy parameters: {type(exc).__name__}: {exc}"
        ) from exc
    if result is not None:
        raise ValueError("validate_config(config) must return None; raise ValueError to reject config")


def strategy_config_targets(
    execution_spec: Mapping[str, Any], target_strategy_id: str | None
) -> list[str | None]:
    """Return strategy slots that must pass a module-level configuration validator."""
    base = execution_spec.get("base")
    if isinstance(base, Mapping):
        return strategy_config_targets(base, target_strategy_id)
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


def _local_project_preflight(
    path: str | Path, *, input_spec_override: str | Path | None = None,
    strategy_file_override: str | Path | None = None, explain: bool = False,
) -> dict[str, Any]:
    """Resolve local project files and delegate semantic validation to Rust preflight."""
    from .app import validate_strategy_execution_preflight_v1

    root, manifest = load_strategy_project(path)
    strategy_file_value = manifest["strategy_file"]
    if not strategy_file_value:
        raise ValueError("strategy_file is required for local execution")
    strategy_file = (
        Path(strategy_file_override).expanduser().resolve(strict=True)
        if strategy_file_override is not None
        else _resolve(root, strategy_file_value, "strategy file")
    )
    module, _factory, source_sha256 = load_strategy_file(strategy_file)
    module_manifest = build_module_manifest(module, strategy_file, source_sha256)
    input_value = manifest["input_spec"]
    if not input_value:
        raise ValueError("input_spec is required; set it to a versioned BacktestSpec JSON path")
    input_path = (
        Path(input_spec_override).expanduser().resolve(strict=True)
        if input_spec_override is not None
        else _resolve(root, input_value, "input spec")
    )
    if not input_path.is_file():
        raise ValueError(f"input spec does not exist or is not a file: {input_path}")
    raw_spec = _json_object(input_path, "strategy input spec")
    selected_id = manifest.get("strategy_id") or module_manifest.get("strategy_id")
    spec = apply_parameter_defaults(raw_spec, module_manifest, target_strategy_id=selected_id)
    # The native validator resolves data files relative to its process cwd. Make
    # project-relative paths explicit so preflight is safe under concurrent calls
    # and does not need to mutate the embedding application's cwd.
    resolved_spec, path_failures = _resolve_input_paths(root, spec)
    if path_failures:
        raise ValueError("input spec references missing local file(s): " + ", ".join(path_failures[:8]))
    for target_id in strategy_config_targets(spec, selected_id):
        validate_strategy_config(module, module_manifest, spec, target_id)
    preflight_result = validate_strategy_execution_preflight_v1(
        module_manifest,
        resolved_spec,
        purpose="run",
        target_strategy_id=selected_id,
        require_dataset_snapshots=True,
    )
    host_capabilities = preflight_result.get("host_capabilities")
    if not isinstance(host_capabilities, dict):
        raise ValueError(
            "native extension does not report host capabilities; rebuild or reinstall "
            "the qianxing wheel matching this Python SDK"
        )
    from .dataset_bundle import project_provenance
    result = {
        "status": "ready",
        "profile": "local",
        "datasets": project_provenance(root, resolved_spec),
        "project_id": manifest["project_id"],
        "project_root": str(root),
        "strategy_file": str(strategy_file),
        "strategy_source_sha256": module_manifest["source_sha256"],
        "strategy_bundle_sha256": module_manifest["bundle_sha256"],
        "strategy_id": selected_id,
        "strategy_version": manifest.get("strategy_version") or module_manifest.get("strategy_version"),
        "input_spec": str(input_path),
        "input_spec_sha256": hashlib.sha256(input_path.read_bytes()).hexdigest(),
        "data_level": module_manifest.get("data_level"),
        "warmup_bars": module_manifest.get("warmup_bars"),
        "checked_markets": preflight_result.get("checked_markets", []),
        "host_capabilities": host_capabilities,
        "execution_boundary": "trusted_local_python",
        "credential_access": "not_provided_by_local_project_runner",
    }
    if explain:
        from .project_explain import explain_local_configuration
        result["configuration"] = explain_local_configuration(
            root=root, manifest=manifest, raw_spec=raw_spec, module_manifest=module_manifest,
            selected_id=selected_id, resolved_spec=resolved_spec, strategy_file=strategy_file,
            input_path=input_path, input_spec_override=input_spec_override,
            strategy_file_override=strategy_file_override,
            manifest_path=Path(path).expanduser().resolve() if Path(path).expanduser().is_file() else root / PROJECT_FILE,
        )
        result["data_selection"] = _explain_data_selection(root, manifest)
    return result


def _explain_data_selection(root: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    from qianxing_ashare.sources import AshareProviderError, resolve_data_config

    try:
        config = resolve_data_config(manifest=manifest.get("data"), project_root=root)
    except AshareProviderError as error:
        return {"status": "unavailable", "scope": "future_fetch_only",
                "error": json.loads(error.to_json())}
    return {"status": "resolved", "scope": "future_fetch_only",
            "configuration": config["explanation"]}


def server_project_plan(
    path: str | Path,
    *,
    runtime_config: str | None = None,
    worker_id: str | None = None,
    explain: bool = False,
) -> dict[str, Any]:
    """Build a read-only static plan; it never starts workers or submits orders."""
    root, manifest = load_strategy_project(path)
    runtime_value = runtime_config or manifest.get("runtime_config")
    if not runtime_value:
        raise ValueError("server plan requires --runtime-config or runtime_config in the project manifest")
    runtime_path = _resolve(root, runtime_value, "runtime config")
    runtime = _json_object(runtime_path, "runtime config")
    from .app import plan_strategy_runtime_v1

    binding = plan_strategy_runtime_v1(runtime, worker_id or manifest.get("worker_id"))
    worker = binding["worker"]
    strategy_config = binding["strategy"]
    host_fields = ("python_module", "external_executable", "c_abi_library", "builtin_strategy")
    configured_hosts = [field for field in host_fields if strategy_config.get(field) is not None]
    if len(configured_hosts) > 1:
        raise ValueError("server runtime config must select exactly one strategy host adapter")
    if not configured_hosts:
        # Rust also supports the original configured target-position strategy.
        configured_hosts = ["target_position"]
    worker_instrument_scope = worker.get("symbols", [])
    strategy_instrument = strategy_config.get("instrument")
    if (
        isinstance(worker_instrument_scope, list)
        and worker_instrument_scope
        and isinstance(strategy_instrument, str)
        and strategy_instrument not in worker_instrument_scope
    ):
        raise ValueError(
            f"strategy instrument {strategy_instrument!r} is outside worker {worker.get('id')!r} symbol scope"
        )
    for scope_field in ("account_id", "venue_id"):
        worker_scope = worker.get(scope_field)
        strategy_scope = strategy_config.get(scope_field)
        if worker_scope and strategy_scope and worker_scope != strategy_scope:
            raise ValueError(
                f"strategy {scope_field}={strategy_scope!r} does not match worker scope {worker_scope!r}"
            )

    source_binding: dict[str, Any] = {"status": "not_checked"}
    strategy_file_value = manifest.get("strategy_file")
    if configured_hosts[0] != "python_module":
        source_binding = {"status": "not_applicable", "reason": "runtime artifact is not a Python source file"}
    elif strategy_file_value:
        strategy_file = _resolve(root, strategy_file_value, "strategy file")
        source_digest = hashlib.sha256(strategy_file.read_bytes()).hexdigest()
        expected_digest = strategy_config.get("strategy_artifact_sha256")
        if expected_digest is not None and (
            not isinstance(expected_digest, str)
            or re.fullmatch(r"[0-9a-fA-F]{64}", expected_digest) is None
        ):
            raise ValueError("strategy_artifact_sha256 must contain 64 hexadecimal characters")
        # Planning never imports user code. A bundle hash includes imported modules and
        # must be checked by the runtime artifact verifier, rather than guessed here.
        source_binding = {
            "status": (
                "unbound" if expected_digest is None else
                "source_matched" if expected_digest.lower() == source_digest else
                "bundle_verification_required"
            ),
            "strategy_file": str(strategy_file),
            "source_sha256": source_digest,
            "runtime_expected_sha256": expected_digest,
            "user_code_executed": False,
        }
    for field, runtime_field in (("strategy_id", "id"), ("strategy_version", "version")):
        expected = manifest.get(field)
        configured = strategy_config.get(runtime_field)
        if expected is not None and configured is not None and expected != configured:
            raise ValueError(f"project {field}={expected!r} does not match runtime {runtime_field}={configured!r}")
    capability_negotiation = binding.get("capability_negotiation", {})
    configured_requirement_status = capability_negotiation.get(
        "configured_requirement_status", {}
    )
    blocking_gates = [
        "runtime-check and live-check have not run",
        "strategy artifact bundle verification has not run",
        "host capability negotiation has not completed",
        "credential and sandbox provider availability are not observed",
        "venue/account reconciliation has not run",
        "worker health and checkpoint restore have not been observed",
    ]
    if configured_requirement_status.get("status") == "unsupported":
        blocking_gates.append(
            "configured strategy requirements are unsupported by the selected host"
        )
    elif configured_requirement_status.get("status") == "deferred_to_verified_library_load":
        blocking_gates.append(
            "C ABI requirements await verified library export inspection at worker startup"
        )
    result = {
        "status": "static_plan_only",
        "profile": "server",
        "project_id": manifest["project_id"],
        "runtime_config": str(runtime_path),
        "runtime_config_sha256": hashlib.sha256(runtime_path.read_bytes()).hexdigest(),
        "environment": runtime.get("environment"),
        "worker": {"id": worker.get("id"), "role": worker.get("role"), "enabled": worker.get("enabled")},
        "strategy_host": configured_hosts[0],
        "strategy_artifact_binding": source_binding,
        "runtime_capabilities": {
            "persistent_worker": True,
            "checkpoint": "adapter_and_strategy_contract_dependent",
            "startup_reconciliation": "required_before_live_trading",
            "isolation": "must_be_configured_for_untrusted_code",
        },
        "host_capabilities": binding.get("host_capabilities"),
        "capability_negotiation": capability_negotiation,
        "readiness": {
            "state": "not_evaluated",
            "trading_enabled": False,
            "blocking_gates": blocking_gates,
        },
        "strategy": {
            "id": strategy_config.get("id"),
            "version": strategy_config.get("version"),
            "artifact_sha256": strategy_config.get("strategy_artifact_sha256"),
            "account_id": strategy_config.get("account_id"),
            "venue_id": strategy_config.get("venue_id"),
            "instrument": strategy_config.get("instrument"),
        },
        "worker_scope": {"account_id": worker.get("account_id"), "venue_id": worker.get("venue_id"), "symbols": worker.get("symbols", [])},
        "external_checks": ["qx-cli runtime-check", "qx-cli live-check", "venue capability evidence", "credential and sandbox provider availability", "worker startup and reconciliation"],
        "worker_command": ["qx-cli", "strategy-worker", str(runtime_path), str(worker.get("id"))],
        "side_effects": "none",
    }
    if explain:
        from .project_explain import explain_server_configuration
        result["configuration"] = explain_server_configuration(
            root=root, manifest=manifest, runtime=runtime, binding=binding,
            runtime_path=runtime_path, runtime_config=runtime_config, worker_id=worker_id,
            manifest_path=Path(path).expanduser().resolve() if Path(path).expanduser().is_file() else root / PROJECT_FILE,
        )
        result["data_selection"] = _explain_data_selection(root, manifest)
    return result


def project_deployments(
    path: str | Path, api_url: str, *, worker_id: str | None = None, ssl_context: Any = None,
) -> dict[str, Any]:
    """Read observed server state without importing strategy code or enabling execution."""
    from .app import AppClient
    _root, manifest = load_strategy_project(path)
    selected_worker = worker_id if worker_id is not None else manifest.get("worker_id")
    client = AppClient(api_url, ssl_context=ssl_context)
    return {
        "project_id": manifest["project_id"], "profile": "server",
        "worker_id": selected_worker,
        "deployments": client.get_strategy_deployments(selected_worker),
        "side_effects": "none",
    }


__all__ = [
    "PROJECT_FILE",
    "init_strategy_project",
    "load_strategy_project",
    "show_strategy_project",
    "local_project_preflight",
    "server_project_plan",
    "project_deployments",
]
