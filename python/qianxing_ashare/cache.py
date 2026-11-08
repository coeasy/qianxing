"""取数结果的内容寻址缓存（方案 D15）。

同一个查询（源 + 标的 + 频率 + 复权 + 区间 + 请求版本）第二次取数只读缓存；
``--offline`` 只读不写，未命中即报错并列出缺失键；``--refresh`` 强制重取。

缓存**不参与证据链**：运行仍以 manifest / snapshot 摘要为准，缓存只是省一次网络。
"""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
import time
from pathlib import Path
from typing import Any, Mapping

from .sources import DATA_CACHE_ENV

CACHE_SCHEMA_VERSION = 1
CACHE_RECORD_SCHEMA_VERSION = 1


def cache_root(environ: Mapping[str, str] | None = None) -> Path:
    environment = os.environ if environ is None else environ
    configured = environment.get(DATA_CACHE_ENV)
    if configured:
        return Path(configured).expanduser()
    if environment.get("XDG_CACHE_HOME"):
        return Path(environment["XDG_CACHE_HOME"]).expanduser() / "qianxing" / "data"
    if os.name == "nt" and environment.get("LOCALAPPDATA"):
        return Path(environment["LOCALAPPDATA"]) / "qianxing" / "data"
    return Path.home() / ".cache" / "qianxing" / "data"


def request_descriptor(
    source_name: str,
    *,
    instrument: str,
    start: str,
    end: str,
    frequency: str,
    adjustment: str,
    kind: str = "bars",
    as_of: str | None = None,
) -> dict[str, Any]:
    descriptor = {
        "request_schema": CACHE_SCHEMA_VERSION,
        "kind": kind,
        "source": source_name,
        "instrument": instrument,
        "start": start.replace("-", ""),
        "end": end.replace("-", ""),
    }
    if kind == "bars":
        descriptor["frequency"] = frequency
        descriptor["adjustment"] = adjustment
    if as_of:
        descriptor["as_of"] = as_of
    return descriptor


def request_key(descriptor: Mapping[str, Any]) -> str:
    canonical = json.dumps(descriptor, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def describe_request(descriptor: Mapping[str, Any]) -> str:
    parts = [
        str(descriptor["source"]),
        str(descriptor["instrument"]),
        str(descriptor.get("kind", "bars")),
    ]
    if descriptor.get("frequency"):
        parts.append(str(descriptor["frequency"]))
    if descriptor.get("adjustment"):
        parts.append(str(descriptor["adjustment"]))
    parts.append(f"{descriptor['start']}..{descriptor['end']}")
    if descriptor.get("as_of"):
        parts.append(f"as_of={descriptor['as_of']}")
    return ":".join(parts)


def entry_directory(root: Path, source_name: str, key: str) -> Path:
    return Path(root) / source_name / key[:2] / key


def _digest(payload: str) -> str:
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _bound_request(
    descriptor: Any, source_name: str, key: str, kind: str
) -> bool:
    return (
        isinstance(descriptor, dict)
        and descriptor.get("request_schema") == CACHE_SCHEMA_VERSION
        and descriptor.get("source") == source_name
        and descriptor.get("kind") == kind
        and request_key(descriptor) == key
    )


def read_cached(
    root: Path, source_name: str, key: str, kind: str = "bars"
) -> tuple[str, str] | None:
    # A single read sees one complete generation, never a payload from one writer
    # paired with another writer's manifest. Legacy split records are cache misses.
    path = entry_directory(root, source_name, key) / "record.json"
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(record, dict) or record.get("schema_version") != CACHE_RECORD_SCHEMA_VERSION:
            return None
        if record.get("request_key") != key or not _bound_request(
            record.get("request"), source_name, key, kind
        ):
            return None
        payload = record["payload_json"]
        manifest = record["manifest_json"]
        if not isinstance(payload, str) or not isinstance(manifest, str):
            return None
        if (
            _digest(payload) != record.get("payload_sha256")
            or _digest(manifest) != record.get("manifest_sha256")
        ):
            return None
        if not isinstance(json.loads(payload), dict) or not isinstance(json.loads(manifest), dict):
            return None
        return payload, manifest
    except (OSError, ValueError, KeyError, TypeError):
        return None


def write_cached(
    root: Path,
    source_name: str,
    key: str,
    descriptor: Mapping[str, Any],
    payload_json: str,
    manifest_json: str,
    kind: str = "bars",
) -> Path:
    request = dict(descriptor)
    if not _bound_request(request, source_name, key, kind):
        raise ValueError("cache request does not match source, kind or key")
    record = {
        "schema_version": CACHE_RECORD_SCHEMA_VERSION,
        "request_key": key,
        "request": request,
        "payload_json": payload_json,
        "payload_sha256": _digest(payload_json),
        "manifest_json": manifest_json,
        "manifest_sha256": _digest(manifest_json),
    }
    directory = entry_directory(root, source_name, key)
    directory.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        # Unique sibling files allow concurrent writers on Windows as well as POSIX.
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=directory, prefix=".record-", suffix=".tmp", delete=False
        ) as handle:
            temporary = Path(handle.name)
            json.dump(record, handle, ensure_ascii=False, separators=(",", ":"))
            handle.flush()
            os.fsync(handle.fileno())
        for attempt in range(5):
            try:
                os.replace(temporary, directory / "record.json")
                break
            except PermissionError:
                # Windows readers/competing renames can briefly deny replacement.
                # Never unlink the old generation; persistent failures still surface.
                if os.name != "nt" or attempt == 4:
                    raise
                time.sleep(0.01 * 2**attempt)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return directory


def clear(root: Path, source_name: str | None = None) -> None:
    target = Path(root) / source_name if source_name else Path(root)
    if not target.exists():
        return
    for path in sorted(target.rglob("*"), reverse=True):
        if path.is_file():
            path.unlink()
        elif path.is_dir() and not any(path.iterdir()):
            path.rmdir()
    if not source_name and target.exists() and not any(target.iterdir()):
        target.rmdir()
