"""Provider-neutral, host-side audit records for persistent strategy sandboxes."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import threading
import time
from pathlib import Path
from typing import Any, Mapping

_ZERO_SHA256 = "0" * 64
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_PROVIDERS = {"e2b", "llm-sandbox", "microsandbox", "opensandbox"}
_STATES = {
    "create_requested",
    "created",
    "ready",
    "create_failed",
    "request_completed",
    "request_failed",
    "destroy_requested",
    "destroyed",
    "destroy_failed",
}
_MAX_RECORD_BYTES = 16 * 1024


def _sha256_hex(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _safe_digest(value: str | None, field: str) -> str | None:
    if value is None or value == "":
        return None
    normalized = value.lower()
    if not _SHA256_RE.fullmatch(normalized):
        raise ValueError(f"sandbox audit {field} must be a lowercase SHA-256 digest")
    return normalized


class SandboxExecutionAudit:
    """Append a versioned, secret-free lifecycle stream to a host-owned file.

    The caller supplies a fresh audit path for one bridge process. Each line is
    hash chained to its predecessor so accidental edits and truncation are
    detectable. This is an operational record, not a signed or tamper-proof log.
    """

    def __init__(
        self,
        *,
        provider: str,
        job_id: str,
        package_version: str,
        entry_sha256: str,
        bundle_sha256: str,
        spec_sha256: str,
        resource_limits: Mapping[str, int],
        network_policy: str,
        runtime_ref: str,
        backend: str | None = None,
        host_worker_id: str | None = None,
        path: str | os.PathLike[str] | None = None,
    ) -> None:
        if provider not in _PROVIDERS:
            raise ValueError("sandbox audit provider is unsupported")
        if not isinstance(job_id, str) or not job_id or len(job_id) > 64:
            raise ValueError("sandbox audit job_id is invalid")
        if network_policy != "deny_all":
            raise ValueError("strategy sandbox audit requires network_policy=deny_all")
        if not isinstance(runtime_ref, str) or not runtime_ref or len(runtime_ref) > 512:
            raise ValueError("sandbox audit runtime_ref is invalid")
        if backend is not None and (not isinstance(backend, str) or len(backend) > 64):
            raise ValueError("sandbox audit backend is invalid")
        if host_worker_id is not None and (
            not isinstance(host_worker_id, str)
            or not host_worker_id
            or len(host_worker_id.encode("utf-8")) > 256
        ):
            raise ValueError("sandbox audit host_worker_id is invalid")
        if not isinstance(package_version, str) or not package_version or len(package_version) > 64:
            raise ValueError("sandbox audit package_version is invalid")

        limits = dict(resource_limits)
        required_limits = {
            "cpu_count",
            "memory_bytes",
            "request_timeout_ms",
            "session_ttl_seconds",
            "max_request_bytes",
            "max_response_bytes",
        }
        if set(limits) != required_limits or any(
            type(value) is not int or value <= 0 for value in limits.values()
        ):
            raise ValueError("sandbox audit resource_limits are incomplete or invalid")

        self.provider = provider
        self.job_id = job_id
        self.package_version = package_version
        self.entry_sha256 = _safe_digest(entry_sha256, "entry_sha256")
        self.bundle_sha256 = _safe_digest(bundle_sha256, "bundle_sha256")
        self.spec_sha256 = _safe_digest(spec_sha256, "spec_sha256")
        self.resource_limits = limits
        self.network_policy = network_policy
        self.runtime_ref = runtime_ref
        self.backend = backend
        self.host_worker_id = host_worker_id
        self.path = Path(path) if path is not None else None
        self._sequence = 0
        self._previous_sha256 = _ZERO_SHA256
        self._lock = threading.Lock()

        required = os.environ.get("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED", "").strip().lower()
        if required in {"1", "true", "yes"} and self.path is None:
            raise RuntimeError(
                "QX_STRATEGY_SANDBOX_AUDIT_REQUIRED is enabled but no host audit path was supplied"
            )
        if required not in {"", "0", "false", "no", "1", "true", "yes"}:
            raise ValueError("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED must be a boolean")

    @classmethod
    def from_environment(cls, **kwargs: Any) -> "SandboxExecutionAudit":
        kwargs.setdefault("host_worker_id", os.environ.get("QX_STRATEGY_HOST_WORKER_ID"))
        return cls(path=os.environ.get("QX_STRATEGY_SANDBOX_AUDIT_PATH"), **kwargs)

    def emit(
        self,
        *,
        session_id: str,
        state: str,
        failure_code: str | None = None,
        request_sequence: int | None = None,
        strategy_id: str | None = None,
        strategy_version: str | None = None,
        data_fingerprint: str | None = None,
        response_sha256: str | None = None,
    ) -> None:
        if state not in _STATES:
            raise ValueError("sandbox audit lifecycle state is unsupported")
        if not isinstance(session_id, str) or not session_id or len(session_id) > 64:
            raise ValueError("sandbox audit session_id is invalid")
        if failure_code is not None:
            if not isinstance(failure_code, str) or not re.fullmatch(
                r"[A-Za-z0-9_.-]{1,96}", failure_code
            ):
                raise ValueError("sandbox audit failure_code is invalid")
        if request_sequence is not None and (
            type(request_sequence) is not int or request_sequence <= 0
        ):
            raise ValueError("sandbox audit request_sequence must be positive")
        for value, field in (
            (strategy_id, "strategy_id"),
            (strategy_version, "strategy_version"),
        ):
            if value is not None and (
                not isinstance(value, str)
                or not value
                or len(value.encode("utf-8")) > 256
            ):
                raise ValueError(f"sandbox audit {field} is invalid")
        if data_fingerprint is not None and (
            not isinstance(data_fingerprint, str)
            or not data_fingerprint
            or len(data_fingerprint.encode("utf-8")) > 256
        ):
            raise ValueError("sandbox audit data_fingerprint is invalid")
        response_sha256 = _safe_digest(response_sha256, "response_sha256")

        with self._lock:
            if self.path is None:
                return
            self._sequence += 1
            record: dict[str, Any] = {
                "schema_version": 1,
                "record_type": "strategy_sandbox_lifecycle",
                "record_sequence": self._sequence,
                "occurred_at_ms": time.time_ns() // 1_000_000,
                "provider": self.provider,
                "job_id": self.job_id,
                "host_worker_id": self.host_worker_id,
                "session_id": session_id,
                "lifecycle_state": state,
                "package_version": self.package_version,
                "entry_sha256": self.entry_sha256,
                "bundle_sha256": self.bundle_sha256,
                "spec_sha256": self.spec_sha256,
                "runtime_ref": self.runtime_ref,
                "backend": self.backend,
                "resource_limits": self.resource_limits,
                "network_policy": self.network_policy,
                "failure_code": failure_code,
                "request_sequence": request_sequence,
                "strategy_id": strategy_id,
                "strategy_version": strategy_version,
                "data_fingerprint": data_fingerprint,
                "response_sha256": response_sha256,
                "previous_record_sha256": self._previous_sha256,
            }
            canonical = json.dumps(
                record, sort_keys=True, separators=(",", ":"), ensure_ascii=True
            ).encode("utf-8")
            record_sha256 = _sha256_hex(canonical)
            record["record_sha256"] = record_sha256
            line = json.dumps(
                record, sort_keys=True, separators=(",", ":"), ensure_ascii=True
            ).encode("utf-8") + b"\n"
            if len(line) > _MAX_RECORD_BYTES:
                raise ValueError("sandbox audit record exceeds 16 KiB")
            self.path.parent.mkdir(parents=True, exist_ok=True)
            flags = os.O_CREAT | os.O_APPEND | os.O_WRONLY
            if hasattr(os, "O_BINARY"):
                flags |= os.O_BINARY
            descriptor = os.open(self.path, flags, 0o600)
            try:
                written = os.write(descriptor, line)
                if written != len(line):
                    raise OSError("sandbox audit record was only partially appended")
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
            self._previous_sha256 = record_sha256

    def emit_best_effort(self, **kwargs: Any) -> None:
        """Attempt a cleanup/failure record without ever blocking provider cleanup."""
        try:
            self.emit(**kwargs)
        except Exception as error:  # noqa: BLE001 - cleanup must proceed if audit storage failed.
            sys.stderr.write(
                "[Qianxing Sandbox Audit] lifecycle append failed: "
                + type(error).__name__
                + "\n"
            )
            sys.stderr.flush()


def request_data_fingerprint(payload: str) -> str | None:
    try:
        value = json.loads(payload)
    except json.JSONDecodeError:
        return None
    fingerprint = value.get("data_fingerprint") if isinstance(value, dict) else None
    if isinstance(fingerprint, str) and 0 < len(fingerprint.encode("utf-8")) <= 256:
        return fingerprint
    return None


def request_identity(payload: str) -> tuple[str | None, str | None]:
    try:
        value = json.loads(payload)
    except json.JSONDecodeError:
        return None, None
    if not isinstance(value, dict):
        return None, None
    strategy_id = value.get("strategy_id")
    strategy_version = value.get("strategy_version")
    if not isinstance(strategy_id, str) or not strategy_id or len(strategy_id.encode("utf-8")) > 256:
        strategy_id = None
    if not isinstance(strategy_version, str) or not strategy_version or len(strategy_version.encode("utf-8")) > 256:
        strategy_version = None
    return strategy_id, strategy_version


def failure_code(error: BaseException) -> str:
    """Return a bounded error class label, never the provider error text or secrets."""
    name = type(error).__name__
    return name if re.fullmatch(r"[A-Za-z0-9_.-]{1,96}", name) else "ProviderError"
