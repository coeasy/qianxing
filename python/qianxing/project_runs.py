"""Project run receipts and cooperative cancellation over the Rust RunHandle.

Receipts describe local execution, never server deployment or trading readiness.
One writer owns each receipt; cancellation is a separate, idempotent marker.
"""
from __future__ import annotations

import base64
import binascii
import contextlib
import csv
import hashlib
import io
import json
import os
import platform
import re
import subprocess
import sys
import sysconfig
import threading
import uuid
import zipfile
from contextvars import ContextVar
from datetime import datetime, timezone
from importlib import machinery as importlib_machinery
from importlib import metadata as importlib_metadata
from pathlib import Path
from typing import Any, Callable

_ACTIVE: ContextVar["ProjectRun | None"] = ContextVar("project_run", default=None)
_RUNTIME_IMPORT_CAPTURE: ContextVar[dict[str, Any] | None] = ContextVar(
    "project_run_import_capture", default=None
)
_RUNTIME_IMPORT_HOOK_INSTALLED = False
_RUNTIME_IMPORT_HOOK_LOCK = threading.Lock()
_RUNTIME_PATH_FINDER = None
_RUN_ID = re.compile(r"^[0-9a-f]{32}$")
_TERMINAL = {"succeeded", "failed", "cancelled"}
_KNOWN_STATUSES = _TERMINAL | {"preparing", "running", "cancelling"}
_DEPENDENCY_LOCK_NAME = "requirements.lock"
_DEPENDENCY_LOCK_MAX_BYTES = 8 * 1024 * 1024
_RUNTIME_IMPORT_MAX_FILES = 50_000
_RUNTIME_IMPORT_MAX_OBSERVATIONS = 50_000
_RUNTIME_IMPORT_MAX_BYTES = 2 * 1024 * 1024 * 1024
_DEPENDENCY_WHEELHOUSE_MAX_FILES = 10_000
_DEPENDENCY_WHEELHOUSE_MAX_BYTES = 2 * 1024 * 1024 * 1024
_LOCK_PIN = re.compile(
    r"^([A-Za-z0-9][A-Za-z0-9._-]*)(?:\[[A-Za-z0-9_,.-]+\])?==([^\s;#]+)"
    r"(?:\s+--hash=sha256:[0-9a-fA-F]{64})*$"
)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _strategy_runtime_environment(
    project_dependency_lock_sha256: str | None = None,
    runtime_import_files_sha256: str | None = None,
) -> dict[str, Any]:
    """Identify the Python environment that supplies strategy runtime dependencies."""
    distributions = sorted(
        (str(distribution.metadata.get("Name", "")).lower(), distribution.version)
        for distribution in importlib_metadata.distributions()
        if distribution.metadata.get("Name")
    )
    encoded = json.dumps(distributions, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    identity = {
        "python_version": sys.version,
        "implementation": sys.implementation.name,
        "cache_tag": sys.implementation.cache_tag,
        "executable": str(Path(sys.executable).resolve()),
        "platform": platform.platform(),
        "distributions": distributions,
    }
    if project_dependency_lock_sha256 is not None:
        identity["project_dependency_lock_sha256"] = project_dependency_lock_sha256
    if runtime_import_files_sha256 is not None:
        identity["runtime_import_files_sha256"] = runtime_import_files_sha256
    identity["sha256"] = hashlib.sha256(
        json.dumps(identity, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    identity["distributions_sha256"] = hashlib.sha256(encoded).hexdigest()
    return identity


def _runtime_import_files_sha256(
    files: list[dict[str, Any]], imports: list[dict[str, Any]] | None = None,
) -> str:
    payload: Any = files if imports is None else {"files": files, "imports": imports}
    encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _record_runtime_import_event(event: str, args: tuple[Any, ...]) -> None:
    """Retain bounded import origins, including modules later removed from sys.modules."""
    if event != "import":
        return
    capture = _RUNTIME_IMPORT_CAPTURE.get()
    if capture is None:
        return
    try:
        name = args[0] if args and isinstance(args[0], str) else None
        origin = args[1] if len(args) > 1 and isinstance(args[1], str) else None
        if not name:
            return
        observations: set[tuple[str, str | None]] = capture["observations"]
        item = (name, origin)
        if item in observations:
            return
        if len(observations) >= _RUNTIME_IMPORT_MAX_OBSERVATIONS:
            capture["overflow"] = True
            return
        observations.add(item)
    except Exception:
        # Audit hooks must not change import behavior; inventory failure is reported
        # from the bounded capture state after strategy execution.
        capture["capture_error"] = True


class _RuntimePathFinder:
    """Capture resolved PathFinder origins that the audit event reports as None."""

    def find_spec(self, fullname: str, path: Any = None, target: Any = None):
        spec = importlib_machinery.PathFinder.find_spec(fullname, path, target)
        if spec is not None and isinstance(spec.origin, str):
            _record_runtime_import_event("import", (fullname, spec.origin))
        return spec

    def invalidate_caches(self) -> None:
        importlib_machinery.PathFinder.invalidate_caches()

    def __getattr__(self, name: str) -> Any:
        # `importlib.metadata` finds distributions through the `find_distributions`
        # attribute of each sys.meta_path finder, so delegation keeps distribution
        # fingerprints populated. It also lets the finder be skipped when Python
        # 3.13 drops the helper while the importlib_metadata backport is loaded.
        return getattr(importlib_machinery.PathFinder, name)


def _ensure_runtime_import_hook() -> None:
    global _RUNTIME_IMPORT_HOOK_INSTALLED, _RUNTIME_PATH_FINDER
    if _RUNTIME_IMPORT_HOOK_INSTALLED:
        return
    with _RUNTIME_IMPORT_HOOK_LOCK:
        if not _RUNTIME_IMPORT_HOOK_INSTALLED:
            _RUNTIME_PATH_FINDER = _RuntimePathFinder()
            for index, finder in enumerate(sys.meta_path):
                if finder is importlib_machinery.PathFinder:
                    sys.meta_path[index] = _RUNTIME_PATH_FINDER
                    break
            sys.addaudithook(_record_runtime_import_event)
            _RUNTIME_IMPORT_HOOK_INSTALLED = True


def _import_container_path(raw_path: str) -> Path | None:
    """Resolve a source path inside a ZIP/wheel/egg to its containing archive."""
    match = re.search(r"(?i)\.(?:zip|whl|egg)[/\\]", raw_path)
    if match is None:
        return None
    container = Path(raw_path[: match.end() - 1])
    try:
        resolved = container.resolve(strict=True)
        return resolved if resolved.is_file() else None
    except OSError:
        return None


def _wheel_distribution_record_map(wheel_record: dict[str, Any]) -> dict[str, str]:
    snapshot_path = Path(wheel_record["snapshot"])
    with zipfile.ZipFile(snapshot_path) as wheel:
        record_names = [
            name for name in wheel.namelist()
            if name.endswith(".dist-info/RECORD") and "/" in name
        ]
        if len(record_names) != 1:
            raise ValueError(f"wheel must contain exactly one dist-info/RECORD: {snapshot_path.name}")
        info = wheel.getinfo(record_names[0])
        if info.file_size > 16 * 1024 * 1024:
            raise ValueError(f"wheel RECORD exceeds 16 MiB: {snapshot_path.name}")
        try:
            record_text = wheel.read(record_names[0]).decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError(f"wheel RECORD is not UTF-8: {snapshot_path.name}") from error
    result: dict[str, str] = {}
    try:
        rows = list(csv.reader(io.StringIO(record_text, newline="")))
    except csv.Error as error:
        raise ValueError(f"wheel RECORD is malformed: {snapshot_path.name}: {error}") from error
    for row in rows:
        if len(row) != 3 or not row[1].startswith("sha256="):
            continue
        member = row[0].replace("\\", "/")
        parts = member.split("/")
        if len(parts) >= 3 and parts[0].endswith(".data") and parts[1] in {"purelib", "platlib"}:
            member = "/".join(parts[2:])
        try:
            result[member] = base64.urlsafe_b64decode(
                row[1][len("sha256="):] + "=" * (-len(row[1][len("sha256="):]) % 4)
            ).hex()
        except (ValueError, binascii.Error) as error:
            raise ValueError(f"wheel RECORD has an invalid hash: {snapshot_path.name}:{row[0]}") from error
    return result


def _relative_module_file_path(module_names: set[str], path: Path) -> str | None:
    for module_name in sorted(module_names):
        parts = module_name.split(".")
        if not all(part and part.isidentifier() for part in parts):
            continue
        if path.name == "__init__.py":
            return "/".join([*parts, "__init__.py"])
        if len(parts) == 1:
            return path.name
        return "/".join([*parts[:-1], path.name])
    return None


def _loaded_runtime_import_files(
    root: Path,
    requirements: list[dict[str, Any]] | None = None,
    wheels: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Hash regular Python/native files belonging to loaded non-stdlib modules."""
    project_root = root.resolve()
    paths = sysconfig.get_paths()
    stdlib_root = Path(paths["stdlib"]).resolve()
    package_roots = {
        Path(value).resolve()
        for key in ("purelib", "platlib")
        if isinstance((value := paths.get(key)), str) and value
    }

    def is_under(path: Path, parent: Path) -> bool:
        try:
            path.relative_to(parent)
            return True
        except ValueError:
            return False

    package_distributions = importlib_metadata.packages_distributions()
    installed_distributions = tuple(importlib_metadata.distributions())
    module_paths: dict[Path, set[str]] = {}
    for module_name, module in tuple(sys.modules.items()):
        module_file = getattr(module, "__file__", None)
        cached_file = getattr(module, "__cached__", None)
        for raw_path in (module_file, cached_file):
            if not isinstance(raw_path, str) or not raw_path or raw_path.startswith("<"):
                continue
            container = _import_container_path(raw_path)
            if container is not None:
                module_paths.setdefault(container, set()).add(module_name)
                continue
            candidate = Path(raw_path)
            if candidate.suffix.lower() not in {
                ".py", ".pyc", ".pyo", ".so", ".pyd", ".dll", ".dylib",
                ".zip", ".whl", ".egg",
            }:
                continue
            try:
                resolved = candidate.resolve(strict=True)
                if not resolved.is_file():
                    continue
            except OSError:
                continue
            in_package_root = any(is_under(resolved, package_root) for package_root in package_roots)
            if (
                is_under(resolved, project_root)
                and not in_package_root
                and resolved.suffix.lower() == ".py"
            ):
                # Python source files have their own frozen strategy-bundle manifest.
                continue
            if is_under(resolved, stdlib_root) and not in_package_root:
                continue
            module_paths.setdefault(resolved, set()).add(module_name)

    capture = _RUNTIME_IMPORT_CAPTURE.get()
    if capture is not None and capture.get("capture_error"):
        raise ValueError("runtime import observations could not be recorded")
    if capture is not None and capture.get("overflow"):
        raise ValueError("runtime import observations exceeded the 50,000-entry limit")
    raw_imports = sorted(
        capture.get("observations", ()),
        key=lambda item: (item[0], item[1] or ""),
    ) if capture is not None else []
    import_records: list[dict[str, Any]] = []
    for module_name, origin in raw_imports:
        container = _import_container_path(origin) if origin is not None else None
        if container is not None:
            module_paths.setdefault(container, set()).add(module_name)
            continue
        if origin is None:
            continue
        candidate = Path(origin)
        if candidate.suffix.lower() not in {
            ".py", ".pyc", ".pyo", ".so", ".pyd", ".dll", ".dylib",
            ".zip", ".whl", ".egg",
        }:
            continue
        try:
            resolved = candidate.resolve(strict=True)
            if resolved.is_file():
                in_package_root = any(
                    is_under(resolved, package_root) for package_root in package_roots
                )
                if (
                    is_under(resolved, project_root)
                    and not in_package_root
                    and resolved.suffix.lower() == ".py"
                ):
                    continue
                if is_under(resolved, stdlib_root) and not in_package_root:
                    continue
                module_paths.setdefault(resolved, set()).add(module_name)
        except OSError:
            # Keep the observation below even when the imported module's source
            # disappeared before the run inventory was assembled.
            pass

    if len(module_paths) > _RUNTIME_IMPORT_MAX_FILES:
        raise ValueError("loaded runtime contains too many module files to fingerprint")
    locked_requirements = {
        requirement["name"]: requirement
        for requirement in (requirements or [])
        if isinstance(requirement, dict) and isinstance(requirement.get("name"), str)
    }
    wheel_records = wheels or []
    wheel_maps = {
        wheel["sha256"]: _wheel_distribution_record_map(wheel)
        for wheel in wheel_records
        if isinstance(wheel, dict) and isinstance(wheel.get("snapshot"), str)
    }
    distribution_owners: dict[Path, list[dict[str, Any]]] = {}
    for distribution in installed_distributions:
        try:
            distribution_root = Path(distribution.locate_file("")).resolve()
            relevant_paths = [path for path in module_paths if is_under(path, distribution_root)]
            if not relevant_paths:
                continue
            record_entries = {
                str(package_path).replace("\\", "/"): package_path
                for package_path in (distribution.files or ())
            }
            distribution_name = str(distribution.metadata.get("Name", "")).lower()
            distribution_version = distribution.version
            for path in relevant_paths:
                relative_path = path.relative_to(distribution_root).as_posix()
                package_path = record_entries.get(relative_path)
                if package_path is None:
                    continue
                recorded_hash = getattr(package_path, "hash", None)
                record_sha256 = None
                if recorded_hash is not None and recorded_hash.mode == "sha256":
                    encoded_hash = recorded_hash.value
                    try:
                        record_sha256 = base64.urlsafe_b64decode(
                            encoded_hash + "=" * (-len(encoded_hash) % 4)
                        ).hex()
                    except (ValueError, binascii.Error):
                        raise ValueError(
                            f"invalid SHA-256 RECORD entry for {distribution_name}: {relative_path}"
                        )
                distribution_owners.setdefault(path, []).append({
                    "name": distribution_name,
                    "version": distribution_version,
                    "relative_path": relative_path,
                    "record_sha256": record_sha256,
                    "wheel_sha256": None,
                })
        except (OSError, ValueError) as error:
            raise ValueError(f"cannot inspect installed distribution RECORD: {error}") from error

    files: list[dict[str, Any]] = []
    total_bytes = 0
    for path, module_names in sorted(module_paths.items(), key=lambda item: str(item[0])):
        size = path.stat().st_size
        total_bytes += size
        if total_bytes > _RUNTIME_IMPORT_MAX_BYTES:
            raise ValueError("loaded runtime module files exceed the 2 GiB fingerprint limit")
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        file_sha256 = digest.hexdigest()
        owners = sorted(
            distribution_owners.get(path, []),
            key=lambda owner: (owner["name"], owner["version"]),
        )
        package_names: set[str] = set()
        for module_name in module_names:
            top_level = module_name.split(".", 1)[0]
            package_names.update(
                _normalize_distribution_name(name)
                for name in package_distributions.get(top_level, [])
                if isinstance(name, str)
            )
            normalized_top_level = _normalize_distribution_name(top_level)
            if normalized_top_level in locked_requirements:
                package_names.add(normalized_top_level)
        if path.suffix.lower() not in {".pyc", ".zip", ".whl", ".egg"} and not owners:
            for package_name in sorted(package_names):
                requirement = locked_requirements.get(package_name)
                if not requirement or not requirement.get("wheel_hashes"):
                    continue
                relative_path = _relative_module_file_path(module_names, path)
                if relative_path is None:
                    raise ValueError(f"cannot map loaded module to its locked wheel: {path}")
                wheel_matches = [
                    wheel
                    for wheel in wheel_records
                    if wheel.get("name") == package_name
                    and wheel.get("version") == requirement["version"]
                    and wheel.get("sha256") in requirement["wheel_hashes"]
                    and wheel_maps.get(wheel.get("sha256"), {}).get(relative_path) == file_sha256
                ]
                if not wheel_matches:
                    raise ValueError(
                        "loaded runtime module is not bound to an installed distribution or locked wheel: "
                        f"{package_name} ({relative_path})"
                    )
                wheel_sha256 = sorted(wheel["sha256"] for wheel in wheel_matches)[0]
                owners.append({
                    "name": package_name,
                    "version": requirement["version"],
                    "relative_path": relative_path,
                    "record_sha256": None,
                    "wheel_sha256": wheel_sha256,
                })
        for owner in owners:
            if owner["record_sha256"] is not None and owner["record_sha256"] != file_sha256:
                raise ValueError(
                    "loaded runtime module does not match its installed distribution RECORD: "
                    f"{owner['name']} {owner['version']} ({path})"
                )
            requirement = locked_requirements.get(owner["name"])
            if requirement and requirement.get("wheel_hashes") and path.suffix.lower() != ".pyc":
                wheel_matches = [
                    wheel
                    for wheel in wheel_records
                    if wheel.get("name") == owner["name"]
                    and wheel.get("version") == owner["version"]
                    and wheel.get("sha256") in requirement["wheel_hashes"]
                    and wheel_maps.get(wheel.get("sha256"), {}).get(owner["relative_path"]) == file_sha256
                ]
                if not wheel_matches:
                    raise ValueError(
                        "loaded runtime module does not match any locked wheel RECORD: "
                        f"{owner['name']} {owner['version']} ({owner['relative_path']})"
                    )
                owner["wheel_sha256"] = sorted(wheel["sha256"] for wheel in wheel_matches)[0]
        files.append({
            "module_names": sorted(module_names),
            "path": str(path),
            "sha256": file_sha256,
            "bytes": size,
            "distributions": owners,
        })
    captured_file_names = {
        name for module_names in module_paths.values() for name in module_names
    }
    captured_file_prefixes = set(captured_file_names)
    for name in captured_file_names:
        parts = name.split(".")
        captured_file_prefixes.update(
            ".".join(parts[:index]) for index in range(1, len(parts))
        )
    for module_name, origin in raw_imports:
        top_level = module_name.split(".", 1)[0]
        if top_level in sys.stdlib_module_names:
            coverage = "stdlib"
        elif origin is not None and origin.startswith("<"):
            coverage = "opaque" if module_name in sys.modules else "attempt"
        elif origin is None and module_name in captured_file_prefixes:
            coverage = "namespace"
        elif origin is None:
            module = sys.modules.get(module_name)
            if module is None:
                # The audit event runs before import resolution, so a no-origin
                # event absent at completion may be a rejected optional import.
                coverage = "attempt"
            else:
                loader = getattr(getattr(module, "__spec__", None), "loader", None)
                coverage = (
                    "namespace"
                    if loader is None or type(loader).__name__ == "NamespaceLoader"
                    else "opaque"
                )
        elif _import_container_path(origin) is not None:
            coverage = "container"
        else:
            try:
                origin_path = Path(origin).resolve(strict=True)
            except OSError:
                coverage = "unresolved"
            else:
                if origin_path in module_paths:
                    coverage = "file"
                elif (
                    is_under(origin_path, project_root)
                    and origin_path.suffix.lower() == ".py"
                ):
                    coverage = "strategy_bundle"
                else:
                    coverage = "unresolved"
        import_records.append({
            "module_name": module_name,
            "origin": origin,
            "coverage": coverage,
        })
    return {
        "schema_version": 2,
        "files": files,
        "imports": import_records,
        "imports_overflow": bool(capture and capture.get("overflow")),
        "sha256": _runtime_import_files_sha256(files, import_records),
        "file_count": len(files),
        "bytes": total_bytes,
    }


def _verify_runtime_import_files(
    snapshot: Any, wheels: list[dict[str, Any]] | None = None,
) -> str:
    if (
        not isinstance(snapshot, dict)
        or snapshot.get("schema_version") not in {1, 2}
        or not isinstance(snapshot.get("files"), list)
        or not isinstance(snapshot.get("sha256"), str)
        or not re.fullmatch(r"[0-9a-f]{64}", snapshot["sha256"])
        or not isinstance(snapshot.get("file_count"), int)
        or isinstance(snapshot.get("file_count"), bool)
        or not isinstance(snapshot.get("bytes"), int)
        or isinstance(snapshot.get("bytes"), bool)
    ):
        return "invalid_record"
    files = snapshot["files"]
    imports = snapshot.get("imports", [])
    if snapshot["schema_version"] == 2:
        if (
            not isinstance(imports, list)
            or not isinstance(snapshot.get("imports_overflow"), bool)
        ):
            return "invalid_record"
        seen_imports: set[tuple[str, str | None]] = set()
        for item in imports:
            if (
                not isinstance(item, dict)
                or not isinstance(item.get("module_name"), str)
                or not item["module_name"]
                or (
                    item.get("origin") is not None
                    and not isinstance(item.get("origin"), str)
                )
                or item.get("coverage") not in {
                    "file", "container", "strategy_bundle", "stdlib", "namespace",
                    "attempt", "unresolved", "opaque"
                }
            ):
                return "invalid_record"
            identity = (item["module_name"], item.get("origin"))
            if identity in seen_imports:
                return "invalid_record"
            seen_imports.add(identity)
    if len(files) > _RUNTIME_IMPORT_MAX_FILES:
        return "oversized"
    total_bytes = 0
    canonical: list[dict[str, Any]] = []
    seen_paths: set[str] = set()
    for record in files:
        if (
            not isinstance(record, dict)
            or not isinstance(record.get("module_names"), list)
            or not all(isinstance(name, str) for name in record["module_names"])
            or not isinstance(record.get("path"), str)
            or not record["path"]
            or record["path"] in seen_paths
            or not isinstance(record.get("sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"])
            or not isinstance(record.get("bytes"), int)
            or isinstance(record.get("bytes"), bool)
            or record["bytes"] < 0
            or not isinstance(record.get("distributions"), list)
        ):
            return "invalid_record"
        for owner in record["distributions"]:
            if (
                not isinstance(owner, dict)
                or not isinstance(owner.get("name"), str)
                or not owner["name"]
                or not isinstance(owner.get("version"), str)
                or not owner["version"]
                or (
                    owner.get("relative_path") is not None
                    and not isinstance(owner.get("relative_path"), str)
                )
                or (
                    owner.get("record_sha256") is not None
                    and (
                        not isinstance(owner.get("record_sha256"), str)
                        or not re.fullmatch(r"[0-9a-f]{64}", owner["record_sha256"])
                    )
                )
                or (
                    owner.get("wheel_sha256") is not None
                    and (
                        not isinstance(owner.get("wheel_sha256"), str)
                        or not re.fullmatch(r"[0-9a-f]{64}", owner["wheel_sha256"])
                    )
                )
            ):
                return "invalid_record"
        seen_paths.add(record["path"])
        try:
            path = Path(record["path"])
            size = path.stat().st_size
            total_bytes += size
            if total_bytes > _RUNTIME_IMPORT_MAX_BYTES:
                return "oversized"
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
        except FileNotFoundError:
            return "missing"
        except OSError:
            return "unreadable"
        if size != record["bytes"] or digest.hexdigest() != record["sha256"]:
            return "changed"
        if any(
            owner.get("record_sha256") is not None
            and owner["record_sha256"] != record["sha256"]
            for owner in record["distributions"]
        ):
            return "record_hash_mismatch"
        for owner in record["distributions"]:
            wheel_sha256 = owner.get("wheel_sha256")
            if wheel_sha256 is None:
                continue
            matching_wheels = [
                wheel for wheel in (wheels or [])
                if wheel.get("sha256") == wheel_sha256
                and wheel.get("name") == owner.get("name")
                and wheel.get("version") == owner.get("version")
            ]
            if not matching_wheels:
                return "wheel_record_mismatch"
            try:
                if not any(
                    _wheel_distribution_record_map(wheel).get(owner.get("relative_path"))
                    == record["sha256"]
                    for wheel in matching_wheels
                ):
                    return "wheel_record_mismatch"
            except (OSError, ValueError, zipfile.BadZipFile):
                return "wheel_unreadable"
        canonical.append(record)
    canonical.sort(key=lambda item: item["path"])
    canonical_imports = sorted(
        imports,
        key=lambda item: (item["module_name"], item.get("origin") or ""),
    ) if snapshot["schema_version"] == 2 else None
    expected_digest = (
        _runtime_import_files_sha256(canonical, canonical_imports)
        if canonical_imports is not None
        else _runtime_import_files_sha256(canonical)
    )
    if expected_digest != snapshot["sha256"]:
        return "manifest_mismatch"
    if snapshot.get("file_count") != len(files) or snapshot.get("bytes") != total_bytes:
        return "manifest_mismatch"
    if snapshot.get("schema_version") == 2:
        if snapshot.get("imports_overflow"):
            return "imports_overflow"
        if any(item["coverage"] in {"unresolved", "opaque"} for item in imports):
            return "unresolved_imports"
    return "matched"


def _write(path: Path, value: Any) -> None:
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        with temporary.open("w", encoding="utf-8", newline="\n") as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2, allow_nan=False)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _directory(root: Path, run_id: str) -> Path:
    if not isinstance(run_id, str) or not _RUN_ID.fullmatch(run_id):
        raise ValueError("project run ID must be 32 lowercase hexadecimal characters")
    directory = root / ".qianxing" / "runs" / run_id
    if not directory.is_dir():
        raise ValueError(f"project run does not exist: {run_id}")
    return directory


def _process_identity(pid: int) -> dict[str, str] | None:
    """Return a host/process-start identity where the OS exposes one."""
    identity = {"host": platform.node(), "platform": platform.system()}
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        class FileTime(ctypes.Structure):
            _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.GetProcessTimes.argtypes = [
            wintypes.HANDLE, ctypes.POINTER(FileTime), ctypes.POINTER(FileTime),
            ctypes.POINTER(FileTime), ctypes.POINTER(FileTime),
        ]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x1000, False, pid)
        if not handle:
            return None
        try:
            created = FileTime()
            exited, kernel_time, user_time = FileTime(), FileTime(), FileTime()
            if not kernel.GetProcessTimes(
                handle, ctypes.byref(created), ctypes.byref(exited),
                ctypes.byref(kernel_time), ctypes.byref(user_time),
            ):
                return None
            identity["process_start"] = f"{created.high:08x}{created.low:08x}"
            return identity
        finally:
            kernel.CloseHandle(handle)
    if platform.system() == "Linux":
        try:
            stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
            fields = stat[stat.rfind(")") + 2 :].split()
            identity["process_start"] = fields[19]
            identity["boot_id"] = Path("/proc/sys/kernel/random/boot_id").read_text(
                encoding="ascii"
            ).strip()
            return identity
        except (OSError, IndexError):
            return None
    if platform.system() == "Darwin":
        try:
            completed = subprocess.run(
                ["ps", "-o", "lstart=", "-p", str(pid)],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=2,
                check=False,
            )
            process_start = completed.stdout.strip()
            if completed.returncode != 0 or not process_start:
                return None
            identity["process_start"] = process_start
            identity["process_start_source"] = "ps_lstart"
            identity["process_start_precision"] = "one_second"
            return identity
        except (OSError, subprocess.TimeoutExpired):
            return None
    return None


class ProjectRunCancelled(Exception):
    pass


class ProjectRun:
    def __init__(self, root: Path, project_id: str):
        self.root = root
        self.run_id = uuid.uuid4().hex
        self.directory = root / ".qianxing" / "runs" / self.run_id
        self.directory.mkdir(parents=True)
        self.record = {
            "schema_version": 1, "kind": "qianxing-project-run",
            "project_run_id": self.run_id, "project_id": project_id,
            "profile": "local", "status": "preparing", "pid": os.getpid(),
            "owner_process_identity": _process_identity(os.getpid()),
            "started_at": _now(), "updated_at": _now(), "artifacts": {},
        }
        self.save()

    def save(self) -> None:
        self.record["updated_at"] = _now()
        _write(self.directory / "run.json", self.record)

    def event(self, value: Any) -> None:
        with (self.directory / "events.jsonl").open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(value, ensure_ascii=False, allow_nan=False) + "\n")

    def wait(self, handle: Any) -> Any:
        self.record.update(status="running", engine_run_id=handle.run_id)
        self.save()
        cursor = 0
        cancelling = False
        while True:
            try:
                if (self.directory / "cancel.request").exists() and not cancelling:
                    handle.cancel()
                    cancelling = True
                    self.record["status"] = "cancelling"
                    self.save()
                status = handle.wait(100)
            except KeyboardInterrupt:
                handle.cancel()
                cancelling = True
                self.record["status"] = "cancelling"
                self.save()
                continue
            for event in handle.events(cursor):
                from dataclasses import asdict, is_dataclass
                value = asdict(event) if is_dataclass(event) else event
                self.event(value)
                cursor = max(cursor, int(value["seq"]))
            if status in _TERMINAL:
                # result() preserves structured engine errors on failure.
                result = handle.result()
                if status == "cancelled":
                    raise ProjectRunCancelled()
                if result is None:
                    raise RuntimeError(f"Rust run ended as {status} without an outcome")
                return result

    def result(self, document: Any) -> None:
        _write(self.directory / "result.json", document)
        artifacts = document.get("artifacts", {}) if isinstance(document, dict) else {}
        indexed = {}
        issues: dict[str, str] = {}
        if not isinstance(artifacts, dict):
            artifacts = {}
            issues["artifacts"] = "invalid_manifest"
        for name, raw in artifacts.items():
            if not isinstance(raw, str):
                issues[str(name)] = "invalid_path"
                continue
            path = Path(raw)
            path = path if path.is_absolute() else self.root / path
            try:
                if not path.is_file():
                    issues[str(name)] = "missing"
                    continue
                digest = hashlib.sha256()
                with path.open("rb") as stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                        digest.update(chunk)
                indexed[name] = {"path": str(path.resolve()), "sha256": digest.hexdigest(), "bytes": path.stat().st_size}
            except OSError:
                issues[str(name)] = "unreadable"
        self.record["artifacts"] = indexed
        self.record["artifact_integrity"] = {
            "status": "complete" if artifacts and not issues else "incomplete",
            "declared_count": len(artifacts),
            "indexed_count": len(indexed),
            "issues": issues,
        }
        self.record["result_path"] = str(self.directory / "result.json")
        self.save()


_OUTPUT_LOG_LIMIT = 4 * 1024 * 1024
_OUTPUT_TRUNCATION_NOTICE = b"\n[output truncated at 4194304 bytes]\n"
_OUTPUT_CAPTURE_LOCK = threading.Lock()
_OUTPUT_CAPTURE_ACTIVE = False


class _BoundedOutputLog:
    """Tee one process stream into a size-capped run log while keeping console output."""

    def __init__(self, original: Any, handle: Any, limit: int):
        self._original = original
        self._handle = handle
        self._limit = limit
        self.written = 0
        self.truncated = False

    def write(self, text: Any) -> Any:
        result = self._original.write(text)
        data = text.encode("utf-8", "replace") if isinstance(text, str) else bytes(text)
        room = self._limit - self.written
        if room > 0:
            self._handle.write(data[:room])
            self.written += min(room, len(data))
        if len(data) > room and not self.truncated:
            self.truncated = True
            self._handle.write(_OUTPUT_TRUNCATION_NOTICE)
        return result

    def flush(self) -> None:
        self._original.flush()
        self._handle.flush()

    def __getattr__(self, name: str) -> Any:
        return getattr(self._original, name)


@contextlib.contextmanager
def _capture_project_output(directory: Path, records: dict[str, dict]):
    """Record bounded stdout/stderr for the engine phase; concurrent runs skip capture."""
    global _OUTPUT_CAPTURE_ACTIVE
    with _OUTPUT_CAPTURE_LOCK:
        if _OUTPUT_CAPTURE_ACTIVE:
            yield
            return
        _OUTPUT_CAPTURE_ACTIVE = True
    sinks: dict[str, tuple[Any, _BoundedOutputLog]] = {}
    try:
        for name in ("stdout", "stderr"):
            handle = (directory / f"{name}.log").open("wb")
            sink = _BoundedOutputLog(getattr(sys, name), handle, _OUTPUT_LOG_LIMIT)
            sinks[name] = (handle, sink)
            setattr(sys, name, sink)
        yield
    finally:
        for name, (handle, sink) in sinks.items():
            setattr(sys, name, sink._original)
            handle.close()
            path = directory / f"{name}.log"
            records[name] = {
                "path": str(path),
                "bytes": path.stat().st_size,
                "sha256": _file_sha256(path),
                "truncated": sink.truncated,
            }
        with _OUTPUT_CAPTURE_LOCK:
            _OUTPUT_CAPTURE_ACTIVE = False


def run_project(path: str | Path, callback: Callable[[Path, dict], int]) -> int:
    from .strategy_project import load_strategy_project
    root, manifest = load_strategy_project(path)
    run = ProjectRun(root, manifest["project_id"])
    print(f"Project run: {run.run_id}", file=sys.stderr)
    token = _ACTIVE.set(run)
    import_token = _RUNTIME_IMPORT_CAPTURE.set({
        "observations": set(), "overflow": False, "capture_error": False,
    })
    output_logs: dict[str, dict] = {}
    try:
        _ensure_runtime_import_hook()
        snapshot_project_dependency_lock(root)
        with _capture_project_output(run.directory, output_logs):
            code = callback(root, manifest)
        if run.record.get("strategy_snapshots"):
            try:
                dependency_lock = run.record.get("dependency_lock")
                run.record["runtime_import_files_completed"] = _loaded_runtime_import_files(
                    root,
                    dependency_lock.get("requirements", []) if isinstance(dependency_lock, dict) else [],
                    run.record.get("dependency_wheels", []),
                )
            except (OSError, ValueError, RuntimeError) as error:
                run.record["runtime_import_files_error"] = str(error)[:2048]
                if code == 0:
                    run.record.update(
                        status="failed",
                        error={
                            "category": "RUNTIME_IMPORT_FINGERPRINT_INCOMPLETE",
                            "message": "loaded runtime module files could not be fingerprinted",
                        },
                    )
                    return 1
        if code == 0:
            run.record["status"] = "succeeded"
            verification = _verify_project_artifact_record(run.record)
            if not verification["verified"]:
                run.record.update(
                    status="failed",
                    error={
                        "category": "ARTIFACT_INTEGRITY_INCOMPLETE",
                        "message": "successful execution did not produce a complete, verifiable artifact set",
                        "checks": verification["artifacts"],
                    },
                )
                return 1
        else:
            run.record["status"] = "failed"
        return code
    except (ProjectRunCancelled, KeyboardInterrupt):
        run.record["status"] = "cancelled"
        return 130
    except BaseException as error:
        run.record.update(
            status="failed",
            error=project_error_envelope(error, stage="execute", run_id=run.run_id),
        )
        raise
    finally:
        if (
            run.record.get("strategy_snapshots")
            and "runtime_import_files_completed" not in run.record
            and "runtime_import_files_error" not in run.record
        ):
            try:
                dependency_lock = run.record.get("dependency_lock")
                run.record["runtime_import_files_completed"] = _loaded_runtime_import_files(
                    root,
                    dependency_lock.get("requirements", []) if isinstance(dependency_lock, dict) else [],
                    run.record.get("dependency_wheels", []),
                )
            except (OSError, ValueError, RuntimeError) as error:
                run.record["runtime_import_files_error"] = str(error)[:2048]
        if output_logs:
            run.record["output_logs"] = output_logs
        run.record["finished_at"] = _now()
        try:
            run.save()
        finally:
            _RUNTIME_IMPORT_CAPTURE.reset(import_token)
            _ACTIVE.reset(token)


def _project_dependency_lock_path(root: Path) -> Path | None:
    project_root = root.resolve(strict=True)
    candidate = project_root / _DEPENDENCY_LOCK_NAME
    if not candidate.exists() and not candidate.is_symlink():
        return None
    try:
        resolved = candidate.resolve(strict=True)
        resolved.relative_to(project_root)
    except (OSError, ValueError) as error:
        raise ValueError(
            f"project {_DEPENDENCY_LOCK_NAME} must resolve to a file inside the project root"
        ) from error
    if not resolved.is_file():
        raise ValueError(f"project {_DEPENDENCY_LOCK_NAME} is not a regular file")
    if resolved.stat().st_size > _DEPENDENCY_LOCK_MAX_BYTES:
        raise ValueError(f"project {_DEPENDENCY_LOCK_NAME} exceeds the 8 MiB limit")
    return resolved


def _normalize_distribution_name(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _parse_dependency_lock(lock_bytes: bytes) -> list[dict[str, Any]]:
    try:
        source = lock_bytes.decode("utf-8-sig")
    except UnicodeDecodeError as error:
        raise ValueError("requirements.lock must be UTF-8") from error
    logical_lines: list[str] = []
    pending = ""
    for physical_line in source.splitlines():
        line = physical_line.strip()
        if not line or line.startswith("#"):
            continue
        line = re.split(r"\s+#", line, maxsplit=1)[0].strip()
        if not line:
            continue
        if line.endswith("\\"):
            pending += line[:-1].strip() + " "
            continue
        logical_lines.append((pending + line).strip())
        pending = ""
    if pending:
        raise ValueError("requirements.lock ends with an incomplete line continuation")
    requirements: dict[str, dict[str, Any]] = {}
    for line in logical_lines:
        if any(
            line.startswith(option + " ") or line.startswith(option + "=")
            for option in ("--index-url", "--extra-index-url", "--trusted-host", "--find-links")
        ):
            continue
        match = _LOCK_PIN.fullmatch(line)
        if not match:
            raise ValueError(
                "requirements.lock must use exact name==version pins (optional SHA-256 hashes); "
                f"unsupported line: {line[:160]}"
            )
        name = _normalize_distribution_name(match.group(1))
        version = match.group(2)
        wheel_hashes = sorted(set(
            value.lower()
            for value in re.findall(r"--hash=sha256:([0-9a-fA-F]{64})", line)
        ))
        previous = requirements.get(name)
        if previous is not None and previous["version"] != version:
            raise ValueError(f"requirements.lock pins {name} to multiple versions")
        if previous is not None:
            wheel_hashes = sorted(set(previous["wheel_hashes"]) | set(wheel_hashes))
        requirements[name] = {"version": version, "wheel_hashes": wheel_hashes}
    return [
        {"name": name, **value}
        for name, value in sorted(requirements.items())
    ]


def _installed_distribution_versions() -> dict[str, str]:
    versions: dict[str, str] = {}
    for distribution in importlib_metadata.distributions():
        raw_name = distribution.metadata.get("Name")
        if not raw_name:
            continue
        name = _normalize_distribution_name(str(raw_name))
        version = distribution.version
        if name in versions and versions[name] != version:
            raise ValueError(f"multiple installed distributions claim the name {name}")
        versions[name] = version
    return versions


def _dependency_lock_bindings_sha256(requirements: list[dict[str, Any]]) -> str:
    encoded = json.dumps(requirements, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _wheel_metadata(path: Path) -> tuple[str, str]:
    try:
        with zipfile.ZipFile(path) as wheel:
            metadata_names = [
                name for name in wheel.namelist()
                if name.endswith(".dist-info/METADATA") and "/" in name
            ]
            if len(metadata_names) != 1:
                raise ValueError("wheel must contain exactly one dist-info/METADATA")
            info = wheel.getinfo(metadata_names[0])
            if info.file_size > 1024 * 1024:
                raise ValueError("wheel METADATA exceeds 1 MiB")
            from email.parser import BytesParser

            metadata = BytesParser().parsebytes(wheel.read(metadata_names[0]))
            name, version = metadata.get("Name"), metadata.get("Version")
            if not name or not version:
                raise ValueError("wheel metadata is missing Name or Version")
            return _normalize_distribution_name(name), version
    except (OSError, zipfile.BadZipFile) as error:
        raise ValueError(f"cannot read wheel metadata from {path.name}: {error}") from error


def _copy_wheel_snapshot(source: Path, destination: Path) -> dict[str, Any]:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(destination.name + "." + uuid.uuid4().hex + ".tmp")
    digest = hashlib.sha256()
    byte_count = 0
    try:
        with source.open("rb") as input_stream, temporary.open("xb") as output_stream:
            for chunk in iter(lambda: input_stream.read(1024 * 1024), b""):
                byte_count += len(chunk)
                if byte_count > _DEPENDENCY_WHEELHOUSE_MAX_BYTES:
                    raise ValueError("a dependency wheel exceeds the 2 GiB size limit")
                digest.update(chunk)
                output_stream.write(chunk)
            output_stream.flush()
            os.fsync(output_stream.fileno())
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
    return {
        "snapshot": str(destination.resolve()),
        "sha256": digest.hexdigest(),
        "bytes": byte_count,
    }


def _snapshot_locked_wheels(
    root: Path, run_directory: Path, requirements: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    pinned = {
        requirement["name"]: requirement
        for requirement in requirements
        if requirement.get("wheel_hashes")
    }
    if not pinned:
        return []
    wheelhouse = root / "wheelhouse"
    try:
        wheelhouse_resolved = wheelhouse.resolve(strict=True)
        wheelhouse_resolved.relative_to(root.resolve(strict=True))
    except (OSError, ValueError) as error:
        raise ValueError("hash-pinned requirements.lock entries require a project-root wheelhouse/") from error
    if not wheelhouse_resolved.is_dir():
        raise ValueError("project wheelhouse must be a directory")
    candidates = sorted(wheelhouse_resolved.rglob("*.whl"), key=lambda path: str(path).lower())
    if len(candidates) > _DEPENDENCY_WHEELHOUSE_MAX_FILES:
        raise ValueError("project wheelhouse exceeds the 10000-wheel limit")
    matched: dict[tuple[str, str], list[tuple[Path, str, int]]] = {}
    for candidate in candidates:
        try:
            resolved = candidate.resolve(strict=True)
            resolved.relative_to(wheelhouse_resolved)
            if not resolved.is_file():
                raise ValueError("wheelhouse entries must be regular files")
            size = resolved.stat().st_size
            if size > _DEPENDENCY_WHEELHOUSE_MAX_BYTES:
                raise ValueError(f"dependency wheel is too large: {resolved.name}")
            name, version = _wheel_metadata(resolved)
            requirement = pinned.get(name)
            if requirement is None or requirement["version"] != version:
                continue
            digest = hashlib.sha256()
            with resolved.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            wheel_sha256 = digest.hexdigest()
            if wheel_sha256 not in requirement["wheel_hashes"]:
                continue
            matched.setdefault((name, version), []).append((resolved, wheel_sha256, size))
        except OSError as error:
            raise ValueError(f"cannot inspect dependency wheel {candidate}: {error}") from error
    for name, requirement in pinned.items():
        if not matched.get((name, requirement["version"])):
            raise ValueError(
                f"wheelhouse has no {name}=={requirement['version']} wheel matching requirements.lock SHA-256"
            )
    snapshots: list[dict[str, Any]] = []
    total_bytes = 0
    snapshotted_hashes: set[str] = set()
    created_paths: list[Path] = []
    destination_root = run_directory / "dependency-wheelhouse"
    try:
        for (name, version), wheels in sorted(matched.items()):
            for source, wheel_sha256, size in wheels:
                if wheel_sha256 in snapshotted_hashes:
                    continue
                snapshotted_hashes.add(wheel_sha256)
                total_bytes += size
                if total_bytes > _DEPENDENCY_WHEELHOUSE_MAX_BYTES:
                    raise ValueError("matching dependency wheels exceed the 2 GiB archive limit")
                destination = destination_root / f"{wheel_sha256}.whl"
                copied = _copy_wheel_snapshot(source, destination)
                created_paths.append(destination)
                if copied["sha256"] != wheel_sha256:
                    raise ValueError(f"dependency wheel changed while snapshotting: {source.name}")
                snapshots.append({
                    "name": name,
                    "version": version,
                    "source": str(source),
                    "source_name": source.name,
                    **copied,
                })
    except BaseException:
        for created in created_paths:
            created.unlink(missing_ok=True)
        raise
    return snapshots


def _dependency_wheelhouse_sha256(wheels: list[dict[str, Any]]) -> str:
    identity = [
        {key: wheel[key] for key in ("name", "version", "sha256", "bytes")}
        for wheel in sorted(wheels, key=lambda item: (item["name"], item["version"], item["sha256"]))
    ]
    encoded = json.dumps(identity, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _verify_dependency_wheels(record: dict[str, Any]) -> str:
    dependency_lock = record.get("dependency_lock")
    if not isinstance(dependency_lock, dict) or "requirements" not in dependency_lock:
        return "matched"
    requirements = dependency_lock.get("requirements")
    wheels = record.get("dependency_wheels")
    if not isinstance(requirements, list):
        return "invalid_lock_requirements"
    requirements_by_name: dict[str, dict[str, Any]] = {}
    for item in requirements:
        if (
            not isinstance(item, dict)
            or not isinstance(item.get("name"), str)
            or not isinstance(item.get("version"), str)
            or not isinstance(item.get("wheel_hashes"), list)
            or not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) for value in item["wheel_hashes"])
        ):
            return "invalid_lock_requirements"
        requirements_by_name[item["name"]] = item
    hash_pinned = {name: item for name, item in requirements_by_name.items() if item["wheel_hashes"]}
    if wheels is None and not hash_pinned:
        return "matched"
    if not isinstance(wheels, list) or len(wheels) > _DEPENDENCY_WHEELHOUSE_MAX_FILES:
        return "invalid_record"
    total_bytes = 0
    seen_hashes: set[str] = set()
    wheel_pairs: set[tuple[str, str]] = set()
    for wheel in wheels:
        if (
            not isinstance(wheel, dict)
            or not isinstance(wheel.get("name"), str)
            or not isinstance(wheel.get("version"), str)
            or not isinstance(wheel.get("snapshot"), str)
            or not isinstance(wheel.get("sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", wheel["sha256"])
            or not isinstance(wheel.get("bytes"), int)
            or isinstance(wheel.get("bytes"), bool)
            or wheel["bytes"] < 0
        ):
            return "invalid_record"
        if wheel["sha256"] in seen_hashes:
            return "duplicate_wheel"
        seen_hashes.add(wheel["sha256"])
        requirement = requirements_by_name.get(wheel["name"])
        if (
            requirement is None
            or requirement["version"] != wheel["version"]
            or wheel["sha256"] not in requirement["wheel_hashes"]
        ):
            return "wheel_not_locked"
        try:
            path = Path(wheel["snapshot"])
            size = path.stat().st_size
            total_bytes += size
            if total_bytes > _DEPENDENCY_WHEELHOUSE_MAX_BYTES:
                return "oversized"
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            if size != wheel["bytes"] or digest.hexdigest() != wheel["sha256"]:
                return "changed"
            if _wheel_metadata(path) != (wheel["name"], wheel["version"]):
                return "metadata_mismatch"
            _wheel_distribution_record_map(wheel)
        except FileNotFoundError:
            return "missing"
        except (OSError, ValueError, zipfile.BadZipFile):
            return "unreadable"
        wheel_pairs.add((wheel["name"], wheel["version"]))
    if any((name, item["version"]) not in wheel_pairs for name, item in hash_pinned.items()):
        return "missing_locked_wheel"
    expected_wheelhouse = _dependency_wheelhouse_sha256(wheels)
    if dependency_lock.get("wheelhouse_sha256") != expected_wheelhouse:
        return "manifest_mismatch"
    plan = record.get("plan")
    if isinstance(plan, dict) and plan.get("project_dependency_wheelhouse_sha256") != expected_wheelhouse:
        return "plan_mismatch"
    return "matched"


def snapshot_project_dependency_lock(root: Path) -> dict[str, Any] | None:
    """Freeze the optional pip requirements lock before project code starts running."""
    run = _ACTIVE.get()
    if run is None:
        raise RuntimeError("dependency lock snapshots require an active managed project run")
    source = _project_dependency_lock_path(root)
    if source is None:
        return None
    destination = run.directory / "requirements.lock.snapshot"
    temporary = destination.with_name(destination.name + "." + uuid.uuid4().hex + ".tmp")
    digest = hashlib.sha256()
    byte_count = 0
    try:
        with source.open("rb") as input_stream, temporary.open("xb") as output_stream:
            for chunk in iter(lambda: input_stream.read(1024 * 1024), b""):
                byte_count += len(chunk)
                if byte_count > _DEPENDENCY_LOCK_MAX_BYTES:
                    raise ValueError(f"project {_DEPENDENCY_LOCK_NAME} exceeds the 8 MiB limit")
                digest.update(chunk)
                output_stream.write(chunk)
            output_stream.flush()
            os.fsync(output_stream.fileno())
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
    record = {
        "path": str(source),
        "snapshot": str(destination.resolve()),
        "sha256": digest.hexdigest(),
        "bytes": byte_count,
    }
    run.record["dependency_lock"] = record
    run.save()
    requirements = _parse_dependency_lock(destination.read_bytes())
    installed = _installed_distribution_versions()
    bindings = [
        {**requirement, "installed_version": installed.get(requirement["name"])}
        for requirement in requirements
    ]
    record["requirements"] = bindings
    record["requirements_sha256"] = _dependency_lock_bindings_sha256(requirements)
    wheels = _snapshot_locked_wheels(root, run.directory, requirements)
    record["wheelhouse_sha256"] = _dependency_wheelhouse_sha256(wheels)
    run.record["dependency_wheels"] = wheels
    run.save()
    for binding in bindings:
        if binding["installed_version"] != binding["version"]:
            raise ValueError(
                "requirements.lock does not match the active Python environment: "
                f"{binding['name']} requires {binding['version']}, "
                f"installed {binding['installed_version'] or 'missing'}"
            )
    run.record["dependency_lock"] = record
    return record


def snapshot_project_input(root: Path, input_path: Path) -> Path:
    """Freeze a project input and its referenced files into the managed run.

    Paths are resolved from the project root, then copied once by content hash.
    The returned spec is self-contained and safe to pass to the engine without
    changing the embedding process's working directory.
    """
    run = _ACTIVE.get()
    if run is None:
        raise RuntimeError("input snapshots require an active managed project run")
    from .strategy_project import _INPUT_PATH_KEYS, _resolve_input_paths

    input_bytes = input_path.read_bytes()
    try:
        spec = json.loads(input_bytes.decode("utf-8"))
    except UnicodeDecodeError as error:
        raise ValueError(f"strategy input spec is not UTF-8: {error}") from error
    except json.JSONDecodeError as error:
        raise ValueError(f"strategy input spec JSON is invalid at line {error.lineno}: {error.msg}") from error
    if not isinstance(spec, dict):
        raise ValueError("strategy input spec must be a JSON object")
    resolved, failures = _resolve_input_paths(root, spec)
    if failures:
        raise ValueError("input spec references missing local file(s): " + ", ".join(failures[:8]))

    snapshots = run.directory / "input-files"
    snapshots.mkdir(exist_ok=True)
    copied: dict[str, str] = {}
    source_records: dict[str, dict[str, Any]] = {}

    def freeze(value: Any) -> None:
        if isinstance(value, dict):
            for key, item in value.items():
                if key in _INPUT_PATH_KEYS and isinstance(item, str):
                    source = Path(item)
                    if not source.is_absolute():
                        source = root / source
                    source = source.resolve(strict=True)
                    destination_text = None
                    digest_state = hashlib.sha256()
                    temporary = snapshots / f".{uuid.uuid4().hex}.tmp"
                    try:
                        with source.open("rb") as source_stream, temporary.open("xb") as output_stream:
                            for chunk in iter(lambda: source_stream.read(1024 * 1024), b""):
                                digest_state.update(chunk)
                                output_stream.write(chunk)
                            output_stream.flush()
                            os.fsync(output_stream.fileno())
                        digest = digest_state.hexdigest()
                        destination_text = copied.get(digest)
                        suffix = source.suffix[:32]
                        destination = snapshots / f"{digest}{suffix}"
                        if destination_text is None:
                            if destination.exists():
                                temporary.unlink(missing_ok=True)
                            else:
                                os.replace(temporary, destination)
                            destination_text = str(destination.resolve())
                            copied[digest] = destination_text
                            source_records[digest] = {
                                "sha256": digest,
                                "source": str(source),
                                "snapshot": destination_text,
                                "bytes": destination.stat().st_size,
                            }
                        else:
                            temporary.unlink(missing_ok=True)
                    finally:
                        temporary.unlink(missing_ok=True)
                    value[key] = destination_text
                else:
                    freeze(item)
        elif isinstance(value, list):
            for item in value:
                freeze(item)

    freeze(resolved)
    destination = run.directory / "preflight-input.json"
    _write(destination, resolved)
    run.record["input_spec_source"] = str(input_path.resolve())
    run.record["input_spec_sha256"] = hashlib.sha256(input_bytes).hexdigest()
    run.record["input_snapshots"] = source_records
    run.record["preflight_input_path"] = str(destination.resolve())
    run.record["preflight_input_sha256"] = hashlib.sha256(destination.read_bytes()).hexdigest()
    run.save()
    return destination


def snapshot_project_strategy(root: Path, strategy_path: Path) -> Path:
    """Freeze a strategy file and its local Python dependencies into this run."""
    run = _ACTIVE.get()
    if run is None:
        raise RuntimeError("strategy snapshots require an active managed project run")
    from .strategy_project import _strategy_bundle_files, load_strategy_file

    source = strategy_path.expanduser().resolve(strict=True)
    module, _factory, expected_bundle = load_strategy_file(source)
    files = _strategy_bundle_files(source, source.read_bytes(), module)
    # Loading the source to discover eager imports may populate sys.modules with
    # project-local modules. Drop those entries so later imports use the snapshot.
    source_root = source.parent.resolve()
    for name, loaded_module in tuple(sys.modules.items()):
        module_path = getattr(loaded_module, "__file__", None)
        if not isinstance(module_path, str) or not module_path.endswith(".py"):
            continue
        try:
            Path(module_path).resolve(strict=True).relative_to(source_root)
        except (OSError, ValueError):
            continue
        sys.modules.pop(name, None)
    destination_root = run.directory / "strategy-bundle"
    records: dict[str, dict[str, Any]] = {}
    for relative, content in sorted(files.items()):
        relative_path = Path(*relative.split("/"))
        destination = destination_root / relative_path
        destination.parent.mkdir(parents=True, exist_ok=True)
        temporary = destination.with_name(destination.name + "." + uuid.uuid4().hex + ".tmp")
        try:
            with temporary.open("xb") as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, destination)
        finally:
            temporary.unlink(missing_ok=True)
        records[relative] = {
            "source": str((source.parent / relative_path).resolve()),
            "snapshot": str(destination.resolve()),
            "sha256": hashlib.sha256(content).hexdigest(),
            "bytes": len(content),
        }
    digest = hashlib.sha256()
    digest.update(b"qianxing-strategy-bundle-v1\0")
    for relative, content in sorted(files.items()):
        encoded_path = relative.encode("utf-8")
        digest.update(len(encoded_path).to_bytes(4, "big"))
        digest.update(encoded_path)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    actual_bundle = digest.hexdigest()
    if actual_bundle != expected_bundle:
        raise ValueError("strategy bundle changed while its run snapshot was being created")
    snapshot_main = destination_root / source.name
    if not snapshot_main.is_file():
        raise ValueError("strategy bundle snapshot is missing its main source file")
    run.record["strategy_bundle_source"] = str(source)
    run.record["strategy_bundle_sha256"] = expected_bundle
    run.record["strategy_snapshots"] = records
    run.record["strategy_file_snapshot"] = str(snapshot_main.resolve())
    dependency_lock = run.record.get("dependency_lock")
    runtime_import_files = _loaded_runtime_import_files(
        root,
        dependency_lock.get("requirements", []) if isinstance(dependency_lock, dict) else [],
        run.record.get("dependency_wheels", []),
    )
    run.record["runtime_import_files"] = runtime_import_files
    dependency_lock_sha256 = (
        dependency_lock.get("sha256") if isinstance(dependency_lock, dict) else None
    )
    run.record["strategy_runtime_environment"] = _strategy_runtime_environment(
        dependency_lock_sha256,
        runtime_import_files["sha256"],
    )
    run.save()
    return snapshot_main


def wait_project_run(start: Callable, synchronous: Callable, *args: Any, **kwargs: Any) -> Any:
    run = _ACTIVE.get()
    if run is None:
        return synchronous(*args, **kwargs)
    if (run.directory / "cancel.request").exists():
        raise ProjectRunCancelled()
    handle = start(*args, **kwargs)
    try:
        return run.wait(handle)
    except BaseException:
        handle.cancel()
        raise


_ERROR_CATEGORY_BY_TYPE = {
    "ValueError": "INVALID_INPUT",
    "TypeError": "INVALID_INPUT",
    "FileNotFoundError": "DATA_UNAVAILABLE",
    "PermissionError": "PERMISSION_DENIED",
    "TimeoutError": "TIMEOUT",
    "OSError": "IO_ERROR",
}
_RETRYABLE_CATEGORIES = frozenset({"DATA_UNAVAILABLE", "TIMEOUT", "IO_ERROR", "RUNTIME_UNAVAILABLE"})
_ERROR_REMEDIATION = {
    "INVALID_INPUT": "fix the reported input, then rerun `qianxing project doctor <project>`",
    "DATA_UNAVAILABLE": "restore the referenced files, then rerun `qianxing project doctor <project>`",
    "PERMISSION_DENIED": "run these paths as a user that may read and write them",
    "CONFLICT": "inspect the existing run or deployment state before retrying",
    "TIMEOUT": "rerun the command; if it repeats, raise the configured timeout",
    "IO_ERROR": "free disk space or fix path permissions, then rerun the command",
    "REPORT_FAILED": "inspect the run's report.log, then rerun `qianxing project report <project> <run_id>`",
    "INTERNAL_INVARIANT": "rerun `qianxing project doctor <project> --dynamic` and report the project_run_id",
}


def project_error_envelope(
    error: Any, *, stage: str, run_id: str | None = None, correlation_id: str | None = None,
) -> dict[str, Any]:
    """Project §8.3 envelope; keeps the v1 `type`/`message` keys readable."""
    payload = error if isinstance(error, dict) else getattr(error, "payload", None) or {}
    category = str(
        payload.get("category")
        or payload.get("code")
        or getattr(error, "code", None)
        or _ERROR_CATEGORY_BY_TYPE.get(type(error).__name__, "PROJECT_ERROR")
    )
    cause = str(
        payload.get("message")
        or payload.get("cause")
        or ("" if isinstance(error, dict) else str(error))
        or category
    )[:4096]
    retry = payload.get("safe_to_retry")
    if not isinstance(retry, bool):
        retry = payload.get("retry")
    action = payload.get("action")
    # Rust/SDK payloads carry `action` as a code such as FIX_INPUT, so only a
    # sentence-shaped action can double as remediation text.
    remediation = action if isinstance(action, str) and " " in action.strip() else None
    envelope = dict(payload)
    envelope.update({
        "code": category,
        "category": category,
        "retryable": retry if isinstance(retry, bool) else category in _RETRYABLE_CATEGORIES,
        "correlation_id": correlation_id or payload.get("correlation_id") or run_id or uuid.uuid4().hex,
        "stage": payload.get("stage") or stage,
        "project_run_id": run_id if run_id is not None else payload.get("project_run_id"),
        "cause": cause,
        "remediation": remediation or _ERROR_REMEDIATION.get(
            category, "inspect the project run receipt and report.log"
        ),
    })
    envelope.setdefault("type", "ProjectError" if isinstance(error, dict) else type(error).__name__)
    envelope.setdefault("message", cause)
    return envelope


def record_project_error(error: Any) -> None:
    run = _ACTIVE.get()
    if run is not None:
        run.record["error"] = project_error_envelope(error, stage="execute", run_id=run.run_id)
        run.save()


def record_project_plan(plan: dict) -> None:
    run = _ACTIVE.get()
    if run is not None:
        if not isinstance(plan, dict):
            raise ValueError("managed project preflight plan must be an object")
        plan = dict(plan)
        dependency_lock = run.record.get("dependency_lock")
        if isinstance(dependency_lock, dict):
            expected_lock = dependency_lock.get("sha256")
            existing_lock = plan.get("project_dependency_lock_sha256")
            if existing_lock is not None and existing_lock != expected_lock:
                raise ValueError("preflight plan does not bind to the frozen dependency lock")
            plan["project_dependency_lock_sha256"] = expected_lock
            requirements_sha256 = dependency_lock.get("requirements_sha256")
            if isinstance(requirements_sha256, str):
                existing_requirements = plan.get("project_dependency_lock_requirements_sha256")
                if existing_requirements is not None and existing_requirements != requirements_sha256:
                    raise ValueError("preflight plan does not bind to the locked distribution versions")
                plan["project_dependency_lock_requirements_sha256"] = requirements_sha256
            wheelhouse_sha256 = dependency_lock.get("wheelhouse_sha256")
            if isinstance(wheelhouse_sha256, str):
                existing_wheelhouse = plan.get("project_dependency_wheelhouse_sha256")
                if existing_wheelhouse is not None and existing_wheelhouse != wheelhouse_sha256:
                    raise ValueError("preflight plan does not bind to the locked dependency wheelhouse")
                plan["project_dependency_wheelhouse_sha256"] = wheelhouse_sha256
        runtime_import_files = run.record.get("runtime_import_files")
        if isinstance(runtime_import_files, dict):
            expected_runtime_files = runtime_import_files.get("sha256")
            existing_runtime_files = plan.get("runtime_import_files_sha256")
            if existing_runtime_files is not None and existing_runtime_files != expected_runtime_files:
                raise ValueError("preflight plan does not bind to the loaded runtime module files")
            plan["runtime_import_files_sha256"] = expected_runtime_files
        if run.record.get("strategy_snapshots"):
            expected_strategy = run.record.get("strategy_file_snapshot")
            planned_strategy = plan.get("strategy_file")
            if (
                not isinstance(expected_strategy, str)
                or not isinstance(planned_strategy, str)
                or Path(planned_strategy).resolve() != Path(expected_strategy).resolve()
                or plan.get("strategy_bundle_sha256") != run.record.get("strategy_bundle_sha256")
            ):
                raise ValueError("preflight plan does not bind to the frozen strategy bundle")
            expected_input = run.record.get("preflight_input_path")
            planned_input = plan.get("input_spec")
            if (
                not isinstance(expected_input, str)
                or not isinstance(planned_input, str)
                or Path(planned_input).resolve() != Path(expected_input).resolve()
                or plan.get("input_spec_sha256") != run.record.get("preflight_input_sha256")
            ):
                raise ValueError("preflight plan does not bind to the frozen input snapshot")
        run.record["plan"] = plan
        run.save()


def prepare_project_input(payload: dict) -> dict:
    """Isolate managed run output without changing the user's input file."""
    run = _ACTIVE.get()
    if run is None:
        return payload
    value = dict(payload)
    value["output_dir"] = str(run.directory / "artifacts")
    execution_input_path = run.directory / "execution-input.json"
    _write(execution_input_path, value)
    run.record["execution_input_path"] = str(execution_input_path.resolve())
    run.record["execution_input_sha256"] = hashlib.sha256(
        execution_input_path.read_bytes()
    ).hexdigest()
    run.save()
    return value


def _process_alive(pid: int, expected_identity: Any = None) -> bool | None:
    if os.name == "nt":
        # os.kill(pid, 0) is not a portable liveness probe on Windows.
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x1000, False, pid)
        if not handle:
            return False if ctypes.get_last_error() == 87 else None
        try:
            code = wintypes.DWORD()
            alive = code.value == 259 if kernel.GetExitCodeProcess(handle, ctypes.byref(code)) else None
        finally:
            kernel.CloseHandle(handle)
        if alive and isinstance(expected_identity, dict):
            actual_identity = _process_identity(pid)
            if actual_identity is None:
                return None
            return actual_identity == expected_identity
        return alive
    try:
        os.kill(pid, 0)
        if isinstance(expected_identity, dict):
            actual_identity = _process_identity(pid)
            return None if actual_identity is None else actual_identity == expected_identity
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return None


def record_project_result(document: Any) -> None:
    run = _ACTIVE.get()
    if run is not None:
        run.result(document)


def project_run_status(root: Path, run_id: str) -> dict:
    directory = _directory(root, run_id)
    try:
        record = json.loads((directory / "run.json").read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {
            "project_run_id": run_id,
            "status": "corrupt",
            "error": {"category": "RUN_RECEIPT_MISSING", "message": "run.json is missing"},
            "artifacts": {},
        }
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return {
            "project_run_id": run_id,
            "status": "corrupt",
            "error": {"category": "RUN_RECEIPT_UNREADABLE", "message": str(error)[:4096]},
            "artifacts": {},
        }
    if not isinstance(record, dict):
        return {
            "project_run_id": run_id,
            "status": "corrupt",
            "error": {"category": "RUN_RECEIPT_INVALID", "message": "run.json must contain an object"},
            "artifacts": {},
        }
    if (
        record.get("project_run_id") != run_id
        or record.get("status") not in _KNOWN_STATUSES
        or not isinstance(record.get("pid"), int)
        or not isinstance(record.get("artifacts"), dict)
    ):
        return {
            "project_run_id": run_id,
            "status": "corrupt",
            "error": {"category": "RUN_RECEIPT_INVALID", "message": "run.json has invalid identity, status, PID, or artifacts"},
            "artifacts": {},
        }
    record["cancellation_requested"] = (directory / "cancel.request").exists()
    # A receipt after an unclean process exit is not evidence of success.
    if record["status"] not in _TERMINAL:
        alive = _process_alive(record["pid"], record.get("owner_process_identity"))
        record["owner_process_alive"] = alive
        if alive is False:
            record["status"] = "interrupted"
        elif alive is None:
            record["status"] = "unknown"
    return record


def project_runs(root: Path, limit: int = 100) -> list[dict]:
    if limit < 1 or limit > 1000:
        raise ValueError("run list limit must be between 1 and 1000")
    base = root / ".qianxing" / "runs"
    if not base.exists():
        return []
    records = [project_run_status(root, path.name) for path in base.iterdir()
               if path.is_dir() and _RUN_ID.fullmatch(path.name)]
    return sorted(records, key=lambda record: str(record.get("started_at", "")), reverse=True)[:limit]


def cancel_project_run(root: Path, run_id: str) -> dict:
    status = project_run_status(root, run_id)
    if status["status"] in {"preparing", "running", "cancelling"}:
        _write(_directory(root, run_id) / "cancel.request", {"requested_at": _now()})
    return project_run_status(root, run_id)


def project_run_events(root: Path, run_id: str, tail: int = 100) -> list[dict]:
    from collections import deque
    if tail < 1 or tail > 10000:
        raise ValueError("event tail must be between 1 and 10000")
    path = _directory(root, run_id) / "events.jsonl"
    if not path.exists():
        return []
    with path.open(encoding="utf-8") as stream:
        lines = list(deque(stream, maxlen=tail))
    # A concurrent writer can leave a final, incomplete line; retry on the next poll.
    return [json.loads(line) for line in lines if line.endswith("\n")]


def verify_project_artifacts(root: Path, run_id: str) -> dict:
    """Check recorded bytes; this does not replace Rust EventLog replay verification."""
    record = project_run_status(root, run_id)
    return _verify_project_artifact_record(record)


def _verify_project_artifact_record(record: dict) -> dict:
    """Validate one already-loaded receipt, including manifest reconciliation and source bytes."""
    checks = {}
    integrity = record.get("artifact_integrity")
    if not isinstance(integrity, dict):
        integrity = {}
    artifacts = record.get("artifacts")
    if not isinstance(artifacts, dict):
        artifacts = {}
        checks["artifacts"] = "invalid_manifest"
    if record.get("status") == "corrupt":
        receipt_error = record.get("error")
        checks["run_receipt"] = (
            receipt_error.get("category", "invalid_record")
            if isinstance(receipt_error, dict)
            else "invalid_record"
        )
    if artifacts:
        if integrity.get("status") != "complete":
            checks["artifact_manifest"] = "incomplete"
        if integrity.get("declared_count") != len(artifacts) or integrity.get("indexed_count") != len(artifacts):
            checks["artifact_manifest"] = "count_mismatch"
    else:
        checks["artifact_manifest"] = "no_required_artifacts"
    for name, artifact in artifacts.items():
        if not isinstance(artifact, dict) or not isinstance(artifact.get("path"), str) or not isinstance(artifact.get("sha256"), str):
            checks[str(name)] = "invalid_record"
            continue
        path = Path(artifact["path"])
        try:
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            checks[name] = "matched" if digest.hexdigest() == artifact["sha256"] else "changed"
        except FileNotFoundError:
            checks[name] = "missing"
        except OSError:
            checks[name] = "unreadable"
    input_snapshots = record.get("input_snapshots", {})
    if not isinstance(input_snapshots, dict):
        checks["input_snapshots"] = "invalid_record"
        input_snapshots = {}
    for digest, snapshot in input_snapshots.items():
        if not isinstance(snapshot, dict) or not isinstance(snapshot.get("snapshot"), str):
            checks[f"input:{digest}"] = "invalid_record"
            continue
        path = Path(snapshot["snapshot"])
        try:
            hasher = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    hasher.update(chunk)
            checks[f"input:{digest}"] = "matched" if hasher.hexdigest() == snapshot.get("sha256") else "changed"
        except FileNotFoundError:
            checks[f"input:{digest}"] = "missing"
        except OSError:
            checks[f"input:{digest}"] = "unreadable"
    strategy_snapshots = record.get("strategy_snapshots", {})
    if not isinstance(strategy_snapshots, dict):
        checks["strategy_snapshots"] = "invalid_record"
        strategy_snapshots = {}
    for relative, snapshot in strategy_snapshots.items():
        if not isinstance(snapshot, dict) or not isinstance(snapshot.get("snapshot"), str):
            checks[f"strategy:{relative}"] = "invalid_record"
            continue
        try:
            actual = hashlib.sha256(Path(snapshot["snapshot"]).read_bytes()).hexdigest()
            checks[f"strategy:{relative}"] = "matched" if actual == snapshot.get("sha256") else "changed"
        except FileNotFoundError:
            checks[f"strategy:{relative}"] = "missing"
        except OSError:
            checks[f"strategy:{relative}"] = "unreadable"
    strategy_records = record.get("strategy_snapshots")
    expected_bundle = record.get("strategy_bundle_sha256")
    if strategy_records is not None or expected_bundle is not None:
        plan = record.get("plan")
        if not isinstance(strategy_records, dict) or not isinstance(expected_bundle, str):
            checks["strategy_bundle"] = "invalid_record"
        else:
            bundle = hashlib.sha256()
            bundle.update(b"qianxing-strategy-bundle-v1\0")
            bundle_error = None
            for relative, snapshot in sorted(strategy_records.items()):
                if not isinstance(snapshot, dict) or not isinstance(snapshot.get("snapshot"), str):
                    bundle_error = "invalid_record"
                    break
                try:
                    content = Path(snapshot["snapshot"]).read_bytes()
                except FileNotFoundError:
                    bundle_error = "missing"
                    break
                except OSError:
                    bundle_error = "unreadable"
                    break
                encoded_path = relative.encode("utf-8")
                bundle.update(len(encoded_path).to_bytes(4, "big"))
                bundle.update(encoded_path)
                bundle.update(len(content).to_bytes(8, "big"))
                bundle.update(content)
            if bundle_error is not None:
                checks["strategy_bundle"] = bundle_error
            elif not strategy_records:
                checks["strategy_bundle"] = "invalid_record"
            elif isinstance(plan, dict) and plan.get("strategy_bundle_sha256") != expected_bundle:
                checks["strategy_bundle"] = "plan_mismatch"
            else:
                checks["strategy_bundle"] = "matched" if bundle.hexdigest() == expected_bundle else "changed"
    runtime_environment = record.get("strategy_runtime_environment")
    if runtime_environment is not None:
        expected_environment = (
            runtime_environment.get("sha256") if isinstance(runtime_environment, dict) else None
        )
        if not isinstance(expected_environment, str):
            checks["strategy_runtime_environment"] = "invalid_record"
        else:
            try:
                dependency_lock = record.get("dependency_lock")
                dependency_lock_sha256 = (
                    dependency_lock.get("sha256") if isinstance(dependency_lock, dict) else None
                )
                runtime_import_files = record.get("runtime_import_files")
                runtime_import_files_sha256 = (
                    runtime_import_files.get("sha256")
                    if isinstance(runtime_import_files, dict)
                    else None
                )
                if runtime_environment.get("project_dependency_lock_sha256") != dependency_lock_sha256:
                    checks["strategy_runtime_environment"] = "dependency_lock_mismatch"
                    actual_environment = None
                elif runtime_environment.get("runtime_import_files_sha256") != runtime_import_files_sha256:
                    checks["strategy_runtime_environment"] = "runtime_import_files_mismatch"
                    actual_environment = None
                else:
                    actual_environment = _strategy_runtime_environment(
                        dependency_lock_sha256, runtime_import_files_sha256
                    )
                checks["strategy_runtime_environment"] = (
                    "matched"
                    if actual_environment is not None
                    and actual_environment["sha256"] == expected_environment
                    else checks.get("strategy_runtime_environment", "changed")
                )
            except (OSError, ValueError, RuntimeError):
                checks["strategy_runtime_environment"] = "unreadable"
    dependency_wheelhouse_check = _verify_dependency_wheels(record)
    if dependency_wheelhouse_check != "matched":
        checks["dependency_wheelhouse"] = dependency_wheelhouse_check
    dependency_wheels = record.get("dependency_wheels")
    if not isinstance(dependency_wheels, list):
        dependency_wheels = []
    for field, check_name in (
        ("runtime_import_files", "runtime_import_files"),
        ("runtime_import_files_completed", "runtime_import_files_completed"),
    ):
        snapshot = record.get(field)
        if snapshot is None:
            if field == "runtime_import_files" and record.get("strategy_runtime_environment") is not None:
                checks[check_name] = "missing"
            elif field == "runtime_import_files_completed" and record.get("runtime_import_files") is not None:
                checks[check_name] = (
                    "capture_failed" if record.get("runtime_import_files_error") else "missing"
                )
            continue
        result = _verify_runtime_import_files(snapshot, dependency_wheels)
        checks[check_name] = result
        plan = record.get("plan")
        if field == "runtime_import_files" and isinstance(plan, dict):
            if plan.get("runtime_import_files_sha256") != snapshot.get("sha256"):
                checks["runtime_import_files_plan"] = "plan_mismatch"
        runtime_error = record.get("runtime_import_files_error")
        if runtime_error:
            checks[check_name] = "capture_failed"
    dependency_lock = record.get("dependency_lock")
    if dependency_lock is not None:
        if (
            not isinstance(dependency_lock, dict)
            or not isinstance(dependency_lock.get("snapshot"), str)
            or not isinstance(dependency_lock.get("sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", dependency_lock["sha256"])
            or not isinstance(dependency_lock.get("bytes"), int)
            or isinstance(dependency_lock.get("bytes"), bool)
        ):
            checks["dependency_lock"] = "invalid_record"
        else:
            try:
                snapshot_path = Path(dependency_lock["snapshot"])
                lock_bytes = snapshot_path.stat().st_size
                if lock_bytes > _DEPENDENCY_LOCK_MAX_BYTES:
                    checks["dependency_lock"] = "oversized"
                else:
                    lock_content = snapshot_path.read_bytes()
                    lock_digest = hashlib.sha256(lock_content).hexdigest()
                    checks["dependency_lock"] = (
                        "matched"
                        if lock_digest == dependency_lock["sha256"]
                        and lock_bytes == dependency_lock["bytes"]
                        else "changed"
                    )
                    if "requirements" in dependency_lock:
                        try:
                            requirements = _parse_dependency_lock(lock_content)
                            installed = _installed_distribution_versions()
                            bindings = [
                                {
                                    **requirement,
                                    "installed_version": installed.get(requirement["name"]),
                                }
                                for requirement in requirements
                            ]
                            if (
                                bindings != dependency_lock.get("requirements")
                                or _dependency_lock_bindings_sha256(requirements)
                                != dependency_lock.get("requirements_sha256")
                            ):
                                checks["dependency_lock_requirements"] = "manifest_mismatch"
                            elif any(
                                binding["installed_version"] != binding["version"]
                                for binding in bindings
                            ):
                                checks["dependency_lock_requirements"] = "installed_version_changed"
                            else:
                                checks["dependency_lock_requirements"] = "matched"
                        except (OSError, ValueError, UnicodeError):
                            checks["dependency_lock_requirements"] = "invalid_or_unreadable"
                plan = record.get("plan")
                if isinstance(plan, dict) and plan.get("project_dependency_lock_sha256") != dependency_lock["sha256"]:
                    checks["dependency_lock_plan"] = "plan_mismatch"
                if isinstance(plan, dict) and isinstance(dependency_lock.get("requirements_sha256"), str):
                    if plan.get("project_dependency_lock_requirements_sha256") != dependency_lock["requirements_sha256"]:
                        checks["dependency_lock_requirements_plan"] = "plan_mismatch"
            except FileNotFoundError:
                checks["dependency_lock"] = "missing"
            except OSError:
                checks["dependency_lock"] = "unreadable"
    for label, path_key, digest_key in (
        ("preflight_input", "preflight_input_path", "preflight_input_sha256"),
        ("execution_input", "execution_input_path", "execution_input_sha256"),
    ):
        path_value = record.get(path_key)
        expected = record.get(digest_key)
        if path_value is None and expected is None:
            continue
        if not isinstance(path_value, str) or not isinstance(expected, str):
            checks[label] = "invalid_record"
            continue
        try:
            digest = hashlib.sha256(Path(path_value).read_bytes()).hexdigest()
            checks[label] = "matched" if digest == expected else "changed"
        except FileNotFoundError:
            checks[label] = "missing"
        except OSError:
            checks[label] = "unreadable"
    issues = integrity.get("issues")
    if isinstance(issues, dict):
        for name, issue in issues.items():
            checks.setdefault(name, issue)
    return {"project_run_id": record.get("project_run_id"), "verification": "recorded_artifact_bytes",
            "verified": record.get("status") == "succeeded" and bool(artifacts)
            and all(value == "matched" for value in checks.values()),
            "integrity_status": integrity.get("status", "unknown"),
            "artifacts": checks}


def _project_report_protected_paths(root: Path, run_directory: Path, record: dict) -> set[Path]:
    """Resolve project inputs and run evidence before allowing a report output path."""
    protected = {
        (run_directory / name).resolve()
        for name in ("run.json", "result.json", "events.jsonl", "cancel.request",
                     "preflight-input.json", "execution-input.json", "report.log",
                     "report-status.json", "requirements.lock.snapshot")
    }
    for artifact in record.get("artifacts", {}).values():
        if isinstance(artifact, dict) and isinstance(artifact.get("path"), str):
            protected.add(Path(artifact["path"]).expanduser().resolve())
    dependency_lock = record.get("dependency_lock")
    if isinstance(dependency_lock, dict):
        for key in ("path", "snapshot"):
            value = dependency_lock.get(key)
            if isinstance(value, str):
                protected.add(Path(value).expanduser().resolve())
    for artifact in record.get("input_snapshots", {}).values():
        if isinstance(artifact, dict) and isinstance(artifact.get("snapshot"), str):
            protected.add(Path(artifact["snapshot"]).expanduser().resolve())
    for artifact in record.get("strategy_snapshots", {}).values():
        if isinstance(artifact, dict) and isinstance(artifact.get("snapshot"), str):
            protected.add(Path(artifact["snapshot"]).expanduser().resolve())
    for field in ("runtime_import_files", "runtime_import_files_completed"):
        snapshot = record.get(field)
        if isinstance(snapshot, dict):
            for item in snapshot.get("files", []):
                if isinstance(item, dict) and isinstance(item.get("path"), str):
                    protected.add(Path(item["path"]).expanduser().resolve())
    for wheel in record.get("dependency_wheels", []):
        if isinstance(wheel, dict):
            for key in ("source", "snapshot"):
                if isinstance(wheel.get(key), str):
                    protected.add(Path(wheel[key]).expanduser().resolve())

    def add_file(raw: Any) -> None:
        if isinstance(raw, str) and raw.strip():
            candidate = Path(raw).expanduser()
            if not candidate.is_absolute():
                candidate = root / candidate
            if candidate.exists():
                protected.add(candidate.resolve())

    def scan(value: Any) -> None:
        if isinstance(value, dict):
            for key, item in value.items():
                normalized = key.lower()
                if normalized in {"strategy_file", "input_spec", "runtime_config"} or normalized.endswith(("_path", "_file")):
                    add_file(item)
                scan(item)
        elif isinstance(value, list):
            for item in value:
                scan(item)

    for name in ("qianxing.strategy.json",):
        manifest_path = root / name
        if manifest_path.is_file():
            try:
                scan(json.loads(manifest_path.read_text(encoding="utf-8")))
            except (OSError, json.JSONDecodeError):
                # A report remains readable if the project manifest was later removed.
                pass
    project_manifest = root / "qianxing.strategy.json"
    if project_manifest.is_file():
        try:
            manifest = json.loads(project_manifest.read_text(encoding="utf-8"))
            raw_spec = manifest.get("input_spec") if isinstance(manifest, dict) else None
            if isinstance(raw_spec, str):
                spec_path = Path(raw_spec)
                spec_path = spec_path if spec_path.is_absolute() else root / spec_path
                if spec_path.is_file():
                    scan(json.loads(spec_path.read_text(encoding="utf-8")))
        except (OSError, json.JSONDecodeError):
            pass
    plan = record.get("plan")
    if isinstance(plan, dict):
        scan(plan)
    return protected


_REPORT_FORMATS = ("html", "xlsx", "json")


def _report_formats(value: Any) -> tuple[str, ...]:
    raw = (value,) if isinstance(value, str) else tuple(value or ())
    selected: list[str] = []
    for item in raw:
        for part in str(item).split(","):
            name = part.strip().lower()
            if not name:
                continue
            if name not in _REPORT_FORMATS:
                raise ValueError(
                    f"unsupported report format {name!r}; choose html, xlsx, or json"
                )
            if name not in selected:
                selected.append(name)
    if not selected:
        raise ValueError("at least one report format is required")
    return tuple(selected)


def _report_output_paths(
    root: Path, directory: Path, output: str | None, formats: tuple[str, ...],
) -> dict[str, Path]:
    raw = Path(output).expanduser() if output else directory / "report.html"
    target = raw if raw.is_absolute() else root / raw
    target = target.resolve()
    stem = target.with_suffix("") if target.suffix else target
    candidates = {
        "html": target,
        "xlsx": stem.with_suffix(".xlsx"),
        "json": stem.with_suffix(".json"),
    }
    return {name: candidates[name] for name in formats}


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def generate_project_report(
    root: Path, run_id: str, *, executable: str = "qx-cli", output: str | None = None,
    formats: Any = ("html",),
) -> dict:
    """Delegate a completed run's verified report to Rust in the requested formats."""
    import subprocess
    selected = _report_formats(formats)
    status = project_run_status(root, run_id)
    integrity = verify_project_artifacts(root, run_id)
    if not integrity["verified"]:
        raise ValueError("project artifacts are missing, changed, or the run did not succeed")
    strategy_bundle_sha256 = integrity["artifacts"].get("strategy_bundle")
    runtime_identity = status.get("strategy_runtime_environment")
    strategy_runtime_environment_sha256 = (
        runtime_identity.get("sha256") if isinstance(runtime_identity, dict) else None
    )
    dependency_lock_identity = status.get("dependency_lock")
    dependency_lock_sha256 = (
        dependency_lock_identity.get("sha256")
        if isinstance(dependency_lock_identity, dict)
        else None
    )
    dependency_wheelhouse_sha256 = (
        dependency_lock_identity.get("wheelhouse_sha256")
        if isinstance(dependency_lock_identity, dict)
        else None
    )
    runtime_imports = status.get("runtime_import_files_completed")
    if not isinstance(runtime_imports, dict):
        runtime_imports = status.get("runtime_import_files")
    runtime_import_files_sha256 = (
        runtime_imports.get("sha256") if isinstance(runtime_imports, dict) else None
    )
    summary = status["artifacts"].get("summary")
    if not summary:
        raise ValueError("this run has no indexed summary; use its outcome-specific report entry point")
    directory = _directory(root, run_id)
    targets = _report_output_paths(root, directory, output, selected)
    if len(set(targets.values())) != len(targets):
        raise ValueError("requested report formats resolve to the same output path")
    protected = _project_report_protected_paths(root.resolve(), directory.resolve(), status)
    for path in targets.values():
        if path in protected:
            raise ValueError("report output aliases a project input or run evidence file")
    command = [executable, "report", summary["path"]]
    if "html" in targets:
        command += ["--html", "-o", str(targets["html"])]
    if "xlsx" in targets:
        command += ["--xlsx", str(targets["xlsx"])]
    if "json" in targets:
        command += ["--json"]
    identity = {
        "project_run_id": run_id,
        "formats": list(selected),
        "report_paths": {name: str(path) for name, path in targets.items()},
        "strategy_bundle_sha256": strategy_bundle_sha256,
        "strategy_runtime_environment_sha256": strategy_runtime_environment_sha256,
        "project_dependency_lock_sha256": dependency_lock_sha256,
        "project_dependency_wheelhouse_sha256": dependency_wheelhouse_sha256,
        "runtime_import_files_sha256": runtime_import_files_sha256,
        "generated_at": _now(),
    }
    try:
        process = subprocess.run(
            command, cwd=root, capture_output=True, encoding="utf-8", errors="replace", timeout=300,
        )
    except FileNotFoundError as error:
        raise ValueError("qx-cli was not found; install the matching CLI or pass --cli PATH") from error
    except subprocess.TimeoutExpired as error:
        failure = project_error_envelope(
            {"category": "TIMEOUT", "message": "Rust report generation exceeded the 300 second timeout"},
            stage="report", run_id=run_id,
        )
        _write(directory / "report-status.json", {
            **identity, "status": "failed", "reason": "report_timeout",
            "report_artifacts": {name: {"path": str(path), "present": False, "sha256": None, "bytes": None}
                                 for name, path in targets.items()},
            "report_sha256": None, "error": failure,
        })
        raise ValueError("Rust report generation exceeded the 300 second timeout") from error
    (directory / "report.log").write_text(process.stdout + "\n" + process.stderr, encoding="utf-8")
    # The renderer is a separate process. Recheck every indexed engine artifact
    # after it exits so report generation cannot silently alter run evidence.
    integrity_after = verify_project_artifacts(root, run_id)
    bundle_after = integrity_after["artifacts"].get("strategy_bundle")
    failure_reason: str | None = None
    if "json" in targets:
        try:
            projection = json.loads((process.stdout or "").strip() or "null")
        except ValueError:
            projection = None
        if not isinstance(projection, dict):
            failure_reason = "report_json_invalid"
        else:
            json_target = targets["json"]
            staging = json_target.with_name(json_target.name + ".tmp")
            try:
                json_target.parent.mkdir(parents=True, exist_ok=True)
                staging.write_text(
                    json.dumps(projection, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
                )
                os.replace(staging, json_target)
            except OSError:
                failure_reason = "report_json_write_failed"
                try:
                    staging.unlink()
                except OSError:
                    pass
    report_artifacts: dict[str, dict[str, Any]] = {}
    for name, path in targets.items():
        entry: dict[str, Any] = {"path": str(path), "present": False, "sha256": None, "bytes": None}
        try:
            if path.is_file() and path.stat().st_size > 0:
                entry["bytes"] = path.stat().st_size
                entry["sha256"] = _file_sha256(path)
                entry["present"] = True
        except OSError:
            failure_reason = failure_reason or f"report_{name}_unreadable"
        if not entry["present"] and failure_reason is None:
            failure_reason = f"report_{name}_missing"
        report_artifacts[name] = entry
    succeeded = (
        process.returncode == 0
        and failure_reason is None
        and integrity_after["verified"]
        and bundle_after == strategy_bundle_sha256
    )
    result = {
        **identity,
        "status": "succeeded" if succeeded else "failed",
        "report_path": str(next(iter(targets.values()))),
        "report_artifacts": report_artifacts,
        "exit_code": process.returncode,
        "source_artifacts_verified_after_render": integrity_after["verified"],
        "report_sha256": report_artifacts.get("html", {}).get("sha256"),
        "log_path": str(directory / "report.log"),
    }
    if not succeeded:
        result["reason"] = failure_reason or "report_verification_failed"
        result["error"] = project_error_envelope(
            {
                "category": "REPORT_FAILED",
                "message": (
                    "Rust report generation failed or source evidence changed during rendering"
                    if failure_reason is None else f"report output failed: {failure_reason}"
                ),
                "reason": result["reason"],
            },
            stage="report", run_id=run_id,
        )
    _write(directory / "report-status.json", result)
    if not succeeded:
        remediation = result["error"]["remediation"]
        raise ValueError(
            f"Rust report generation failed; inspect {directory / 'report.log'}; next: {remediation}"
        )
    return result
