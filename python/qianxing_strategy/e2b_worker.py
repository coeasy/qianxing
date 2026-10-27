"""Host-side bridge for a persistent Python strategy worker in E2B.

The bridge owns the E2B API key on the host and forwards only strategy requests
and responses. The sandbox receives the strategy bundle and strategy spec, but
never exchange credentials or the host's E2B API key.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import io
import json
import os
import queue
import shlex
import sys
import threading
import time
import uuid
import zipfile
from pathlib import Path
from typing import Any

from qianxing.sandbox_execution import _bundle_remote_inputs, _input_preflight_script

from .frame import REQUEST, RESPONSE, encode_frame, read_frame
from .sandbox_audit import (
    SandboxExecutionAudit,
    failure_code as audit_failure_code,
    request_data_fingerprint,
    request_identity,
)

_MAX_REMOTE_LINE_BYTES = 16 * 1024 * 1024
_MAX_REMOTE_LOG_BYTES = 8_000
_MAX_SESSION_TTL_SECONDS = 86_400


class _RemoteOutput:
    """Turn E2B stdout chunks into bounded, ordered JSONL responses."""

    def __init__(self) -> None:
        self.responses: queue.Queue[str | Exception] = queue.Queue(maxsize=1)
        self._buffer = ""
        self._lock = threading.Lock()
        self._request_active = False
        self._response_seen = False
        self._fatal_error: RuntimeError | None = None

    def begin_request(self) -> None:
        with self._lock:
            if self._fatal_error is not None:
                raise self._fatal_error
            if not self.responses.empty():
                self._fail_locked("remote strategy output contains a stale response")
                raise self._fatal_error
            if self._request_active:
                self._fail_locked("remote strategy bridge received a nested request")
                raise self._fatal_error
            self._request_active = True
            self._response_seen = False

    def finish_request(self) -> RuntimeError | None:
        with self._lock:
            self._request_active = False
            if self._buffer:
                self._buffer = ""
                self._fail_locked(
                    "remote strategy worker emitted an incomplete protocol line"
                )
            return self._fatal_error

    def fail(self, message: str) -> None:
        with self._lock:
            self._fail_locked(message)

    def _fail_locked(self, message: str) -> None:
        if self._fatal_error is None:
            self._fatal_error = RuntimeError(message)
        try:
            self.responses.put_nowait(self._fatal_error)
        except queue.Full:
            # The active request will observe the fatal state after consuming its
            # first line. Never block the SDK output pump or grow an attacker-sized queue.
            pass

    def on_stdout(self, chunk: str | bytes) -> None:
        text = (
            chunk.decode("utf-8", errors="replace")
            if isinstance(chunk, bytes)
            else chunk
        )
        with self._lock:
            if (
                len(text.encode("utf-8", errors="replace"))
                + len(self._buffer.encode("utf-8", errors="replace"))
                > _MAX_REMOTE_LINE_BYTES
            ):
                self._buffer = ""
                self._fail_locked(
                    f"remote strategy output exceeded {_MAX_REMOTE_LINE_BYTES} bytes"
                )
                return
            self._buffer += text
            while "\n" in self._buffer:
                line, self._buffer = self._buffer.split("\n", 1)
                if not line:
                    self._fail_locked(
                        "remote strategy worker emitted an empty protocol line"
                    )
                elif not self._request_active:
                    self._fail_locked(
                        "remote strategy worker emitted unsolicited stdout"
                    )
                elif self._response_seen:
                    self._fail_locked(
                        "remote strategy worker emitted multiple responses for one request"
                    )
                elif self._fatal_error is None:
                    self._response_seen = True
                    try:
                        self.responses.put_nowait(line)
                    except queue.Full:
                        self._fail_locked("remote strategy response queue is full")
            if len(self._buffer.encode("utf-8")) > _MAX_REMOTE_LINE_BYTES:
                self._buffer = ""
                self._fail_locked(
                    f"remote strategy response exceeded {_MAX_REMOTE_LINE_BYTES} bytes"
                )

    def remote_process_ended(self, detail: str) -> None:
        with self._lock:
            if self._buffer:
                self._buffer = ""
                self._fail_locked(
                    "remote strategy worker ended with an incomplete protocol line"
                )
            self._fail_locked(detail)

    @staticmethod
    def on_stderr(chunk: str | bytes) -> None:
        text = (
            chunk.decode("utf-8", errors="replace")
            if isinstance(chunk, bytes)
            else chunk
        )
        if text:
            excerpt = text.encode("utf-8", errors="replace")[-_MAX_REMOTE_LOG_BYTES:]
            sys.stderr.write(
                "[E2B Python Strategy] " + excerpt.decode("utf-8", errors="replace")
            )
            sys.stderr.flush()


def _required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"required host setting {name} is missing")
    return value


def _required_positive_int_env(name: str) -> int:
    raw = _required_env(name)
    try:
        value = int(raw)
    except ValueError as error:
        raise ValueError(f"{name} must be a positive integer") from error
    if value <= 0:
        raise ValueError(f"{name} must be a positive integer")
    return value


def _redacted_error(error: BaseException) -> str:
    message = str(error)
    api_key = os.environ.get("E2B_API_KEY", "")
    return message.replace(api_key, "<redacted>") if api_key else message


def _strategy_spec() -> tuple[dict[str, Any], bool]:
    raw = os.environ.get("QX_STRATEGY_SPEC_JSON")
    if raw is None:
        return {}, False
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"QX_STRATEGY_SPEC_JSON is invalid JSON: {error}") from error
    if not isinstance(value, dict):
        raise TypeError("QX_STRATEGY_SPEC_JSON must be a JSON object")
    return value, True


def _remote_worker_command(
    *,
    entrypoint: str,
    spec: dict[str, Any],
    has_spec: bool,
    account_id: str | None,
    venue_id: str | None,
    artifact_sha256: str | None,
    ready_file: str,
) -> str:
    # `env -i` prevents template-level environment variables (including secrets)
    # from becoming visible to uploaded strategy code.
    values = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "PYTHONPATH": "/workspace/strategy",
    }
    if has_spec:
        values["QX_STRATEGY_SPEC_JSON"] = json.dumps(
            spec, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        )
    if account_id:
        values["QX_STRATEGY_ACCOUNT_ID"] = account_id
    if venue_id:
        values["QX_STRATEGY_VENUE_ID"] = venue_id
    if artifact_sha256:
        values["QX_STRATEGY_ARTIFACT_SHA256"] = artifact_sha256
    values["QX_STRATEGY_STDOUT_TO_STDERR"] = "1"
    assignments = " ".join(
        shlex.quote(f"{key}={value}") for key, value in values.items()
    )
    return (
        f"env -i {assignments} python -u -m qianxing_strategy.worker "
        f"--module {shlex.quote(entrypoint)} --protocol jsonl "
        f"--ready-file {shlex.quote(ready_file)}"
    )


class E2BPythonStrategyWorker:
    """One E2B sandbox and one persistent strategy process per host worker."""

    def __init__(self, module: str) -> None:
        template = _required_env("QX_E2B_TEMPLATE")
        self.expected_cpu_count = _required_positive_int_env(
            "QX_E2B_EXPECTED_CPU_COUNT"
        )
        self.expected_memory_mb = _required_positive_int_env(
            "QX_E2B_EXPECTED_MEMORY_MB"
        )
        # The SDK reads this only in the host process. It is never included in
        # the command environment passed to the remote sandbox.
        _required_env("E2B_API_KEY")
        self.timeout_ms = int(os.environ.get("QX_STRATEGY_PYTHON_TIMEOUT_MS", "30000"))
        if not 5_000 <= self.timeout_ms <= 60_000:
            raise ValueError(
                "E2B strategy mode requires QX_STRATEGY_PYTHON_TIMEOUT_MS from "
                "5000 through 60000"
            )
        raw_ttl = os.environ.get("QX_E2B_SESSION_TTL_SECONDS", "3600")
        try:
            self.session_ttl_seconds = int(raw_ttl)
        except ValueError as error:
            raise ValueError("QX_E2B_SESSION_TTL_SECONDS must be an integer") from error
        minimum_ttl = max(60, (self.timeout_ms + 999) // 1000 + 30)
        if not minimum_ttl <= self.session_ttl_seconds <= _MAX_SESSION_TTL_SECONDS:
            raise ValueError(
                "QX_E2B_SESSION_TTL_SECONDS must be at least the strategy timeout "
                f"plus 30 seconds ({minimum_ttl}) and no more than 86400"
            )

        strategy_path = Path(module).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                "E2B strategy mode requires strategy.python_module to resolve to a .py file"
            )
        spec, has_spec = _strategy_spec()
        package_version = importlib.metadata.version("qianxing")
        job_id = str(uuid.uuid4())
        bundle, manifest = _bundle_remote_inputs(
            strategy_path,
            spec,
            package_version=package_version,
            job_id=job_id,
        )
        expected_artifact = os.environ.get("QX_STRATEGY_ARTIFACT_SHA256", "").strip()
        actual_artifact = hashlib.sha256(strategy_path.read_bytes()).hexdigest()
        if expected_artifact:
            if actual_artifact.lower() != expected_artifact.lower():
                raise ValueError(
                    "strategy source changed after host verification; refusing E2B upload"
                )

        try:
            from e2b import Sandbox
        except ImportError as error:
            raise RuntimeError(
                "E2B strategy mode needs `pip install 'qianxing[sandbox-e2b]'`"
            ) from error

        self._sandbox_type = Sandbox
        self._template = template
        self._job_id = job_id
        self._package_version = package_version
        self._bundle = bundle
        self._manifest = manifest
        self._spec = spec
        self._has_spec = has_spec
        self._audit = SandboxExecutionAudit.from_environment(
            provider="e2b",
            job_id=job_id,
            package_version=package_version,
            entry_sha256=actual_artifact,
            bundle_sha256=manifest["strategy_transfer_sha256"],
            spec_sha256=manifest["input_spec_sha256"],
            resource_limits={
                "cpu_count": self.expected_cpu_count,
                "memory_bytes": self.expected_memory_mb * 1_000_000,
                "request_timeout_ms": self.timeout_ms,
                "session_ttl_seconds": self.session_ttl_seconds,
                "max_request_bytes": _MAX_REMOTE_LINE_BYTES,
                "max_response_bytes": _MAX_REMOTE_LINE_BYTES,
            },
            network_policy="deny_all",
            runtime_ref=template,
        )
        self._account_id = os.environ.get("QX_STRATEGY_ACCOUNT_ID")
        self._venue_id = os.environ.get("QX_STRATEGY_VENUE_ID")
        self._artifact_sha256 = expected_artifact or None
        self.sandbox = None
        self.handle = None
        self.output = _RemoteOutput()
        self._reader_thread: threading.Thread | None = None
        self._closed = False
        self._session_started_at = 0.0
        self._processed_requests = 0
        self._session_id: str | None = None

    def _start_session(self) -> None:
        session_id = str(uuid.uuid4())
        self._session_id = session_id
        self._audit.emit(session_id=session_id, state="create_requested")
        try:
            self.sandbox = self._sandbox_type.create(
                self._template,
                timeout=self.session_ttl_seconds,
                allow_internet_access=False,
                metadata={
                    "qianxing_job_id": self._job_id,
                    "qianxing_session_id": session_id,
                    "qianxing_strategy_sha256": self._manifest["strategy_transfer_sha256"],
                    "qianxing_spec_sha256": self._manifest["input_spec_sha256"],
                    "qianxing_expected_cpu_count": str(self.expected_cpu_count),
                    "qianxing_expected_memory_mb": str(self.expected_memory_mb),
                },
            )
            self._session_started_at = time.monotonic()
            self._audit.emit(session_id=session_id, state="created")
            get_info = getattr(self.sandbox, "get_info", None)
            if not callable(get_info):
                raise RuntimeError(
                    "E2B SDK does not expose sandbox CPU and memory inspection"
                )
            info = get_info()
            actual_cpu_count = getattr(info, "cpu_count", None)
            actual_memory_mb = getattr(info, "memory_mb", None)
            if (
                type(actual_cpu_count) is not int
                or type(actual_memory_mb) is not int
                or actual_cpu_count <= 0
                or actual_memory_mb <= 0
            ):
                raise RuntimeError(
                    "E2B sandbox did not report verifiable CPU and memory limits"
                )
            if (
                actual_cpu_count != self.expected_cpu_count
                or actual_memory_mb != self.expected_memory_mb
            ):
                raise RuntimeError(
                    "E2B sandbox resource limits do not match the host policy: "
                    f"expected cpu={self.expected_cpu_count}, memory_mb="
                    f"{self.expected_memory_mb}; got cpu={actual_cpu_count}, "
                    f"memory_mb={actual_memory_mb}"
                )
            with zipfile.ZipFile(io.BytesIO(self._bundle)) as archive:
                files = [member for member in archive.infolist() if not member.is_dir()]
                directories = sorted(
                    {
                        str(Path(member.filename).parent).replace("\\", "/")
                        for member in files
                    }
                )
                mkdir = "mkdir -p " + " ".join(
                    shlex.quote(path) for path in directories
                )
                self.sandbox.commands.run(mkdir, timeout=15)
                for member in files:
                    self.sandbox.files.write(
                        "/workspace/" + member.filename,
                        archive.read(member),
                    )
            self._check_remote_package(self._package_version)
            command = _remote_worker_command(
                entrypoint=self._manifest["strategy_entrypoint"],
                spec=self._spec,
                has_spec=self._has_spec,
                account_id=self._account_id,
                venue_id=self._venue_id,
                artifact_sha256=self._artifact_sha256,
                ready_file=f"/workspace/strategy/.ready-{session_id}",
            )
            self.output = _RemoteOutput()
            self.handle = self.sandbox.commands.run(
                command,
                background=True,
                stdin=True,
                timeout=0,
            )
            output = self.output
            self._reader_thread = threading.Thread(
                target=self._consume_remote_output,
                args=(self.handle, output),
                name="qianxing-e2b-output",
                daemon=True,
            )
            self._reader_thread.start()
            self._wait_for_ready(f"/workspace/strategy/.ready-{session_id}")
            self._audit.emit(session_id=session_id, state="ready")
        except Exception as error:
            self._audit.emit_best_effort(
                session_id=session_id,
                state="create_failed",
                failure_code=audit_failure_code(error),
            )
            self._stop_session(graceful=False)
            raise

    def _check_remote_package(self, expected_version: str) -> None:
        script = _input_preflight_script(expected_version)
        command = "env -i PATH=/usr/local/bin:/usr/bin:/bin python -c " + shlex.quote(
            script
        )
        result = self.sandbox.commands.run(command, timeout=30)
        if getattr(result, "exit_code", 0) != 0:
            stderr = str(getattr(result, "stderr", ""))[-_MAX_REMOTE_LOG_BYTES:]
            raise RuntimeError(
                "E2B remote bundle or package preflight failed; the template must "
                f"contain Qianxing {expected_version}: {stderr}"
            )

    @staticmethod
    def _consume_remote_output(handle: Any, output: _RemoteOutput) -> None:
        try:
            result = handle.wait(
                on_stdout=output.on_stdout,
                on_stderr=output.on_stderr,
            )
            exit_code = getattr(result, "exit_code", None)
            output.remote_process_ended(
                "E2B strategy worker exited unexpectedly"
                + (f" with exit code {exit_code}" if exit_code is not None else "")
            )
        except Exception as error:  # noqa: BLE001 - SDK stream failures must wake the request waiter.
            output.remote_process_ended(
                "E2B strategy output stream failed: " + _redacted_error(error)
            )

    def _wait_for_ready(self, marker: str) -> None:
        script = f"""
import pathlib
import sys
import time

path = pathlib.Path({marker!r})
deadline = time.monotonic() + 30
while not path.is_file():
    if time.monotonic() >= deadline:
        sys.exit(78)
    time.sleep(0.1)
""".strip()
        command = "env -i PATH=/usr/local/bin:/usr/bin:/bin python -c " + shlex.quote(
            script
        )
        result = self.sandbox.commands.run(command, timeout=35)
        if getattr(result, "exit_code", 0) != 0:
            raise RuntimeError(
                "E2B Python strategy worker did not initialize within 30 seconds; "
                "the remote worker was stopped and the strategy was not run"
            )

    def request(self, payload: str) -> str:
        if self._closed:
            raise RuntimeError("E2B strategy worker is closed")
        if self.handle is None:
            self._start_session()
        elif self._session_needs_rotation():
            checkpoint_available = self._request_contains_checkpoint(payload)
            if self._processed_requests and not checkpoint_available:
                raise RuntimeError(
                    "E2B session lease is near expiry and the request has no persisted "
                    "strategy checkpoint; refusing a state-losing restart"
                )
            self._stop_session(graceful=True)
            try:
                self._start_session()
            except Exception:
                self._closed = True
                raise
        if not self._has_request_budget():
            raise TimeoutError(
                "E2B sandbox lease does not have enough time for the configured "
                "strategy request; refusing to send an event"
            )
        strategy_id, strategy_version = request_identity(payload)
        self.output.begin_request()
        try:
            self.handle.send_stdin(payload.rstrip("\r\n") + "\n")
            response = self.output.responses.get(
                timeout=max(0.1, self.timeout_ms / 1000 - 0.25)
            )
        except queue.Empty as error:
            self.output.finish_request()
            if self._session_id is not None:
                self._audit.emit_best_effort(
                    session_id=self._session_id,
                    state="request_failed",
                    failure_code=audit_failure_code(error),
                    request_sequence=self._processed_requests + 1,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=request_data_fingerprint(payload),
                )
            raise TimeoutError(
                "E2B strategy response timed out; the host will fail closed without local fallback"
            ) from error
        except Exception as error:
            self.output.finish_request()
            if self._session_id is not None:
                self._audit.emit_best_effort(
                    session_id=self._session_id,
                    state="request_failed",
                    failure_code=audit_failure_code(error),
                    request_sequence=self._processed_requests + 1,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=request_data_fingerprint(payload),
                )
            raise
        fatal_error = self.output.finish_request()
        if fatal_error is not None:
            raise fatal_error
        if isinstance(response, Exception):
            raise response
        if self._session_id is not None:
            self._audit.emit(
                session_id=self._session_id,
                state="request_completed",
                request_sequence=self._processed_requests + 1,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=request_data_fingerprint(payload),
                response_sha256=hashlib.sha256(response.encode("utf-8")).hexdigest(),
            )
        self._processed_requests += 1
        return response

    def _session_needs_rotation(self) -> bool:
        renew_before = min(60, max(1, self.session_ttl_seconds // 4))
        elapsed = time.monotonic() - self._session_started_at
        return elapsed >= self.session_ttl_seconds - renew_before

    def _has_request_budget(self) -> bool:
        remaining = self.session_ttl_seconds - (
            time.monotonic() - self._session_started_at
        )
        return remaining >= self.timeout_ms / 1000 + 5

    @staticmethod
    def _request_contains_checkpoint(payload: str) -> bool:
        try:
            decoded = json.loads(payload)
        except json.JSONDecodeError:
            return False
        return isinstance(decoded, dict) and isinstance(
            decoded.get("strategy_checkpoint"), dict
        )

    def _stop_session(self, *, graceful: bool) -> None:
        sandbox, self.sandbox = self.sandbox, None
        session_id, self._session_id = self._session_id, None
        handle, self.handle = self.handle, None
        reader_thread, self._reader_thread = self._reader_thread, None
        force_process_stop = not graceful or reader_thread is None
        if handle is not None:
            try:
                handle.close_stdin()
            except Exception as error:  # noqa: BLE001 - SDK cleanup can fail after provider disconnect.
                sys.stderr.write(
                    f"[E2B Python Strategy] closing remote stdin failed: {_redacted_error(error)}\n"
                )
            if graceful and reader_thread is not None:
                reader_thread.join(timeout=5)
                force_process_stop = reader_thread.is_alive()
            if force_process_stop:
                try:
                    handle.kill()
                except Exception as error:  # noqa: BLE001 - the sandbox lease remains the final cleanup bound.
                    sys.stderr.write(
                        f"[E2B Python Strategy] stopping remote worker failed: {_redacted_error(error)}\n"
                    )
        if sandbox is not None:
            if session_id is not None:
                self._audit.emit_best_effort(session_id=session_id, state="destroy_requested")
            try:
                sandbox.kill()
                if session_id is not None:
                    self._audit.emit_best_effort(session_id=session_id, state="destroyed")
            except Exception as error:  # noqa: BLE001 - provider errors are surfaced; TTL bounds orphan lifetime.
                if session_id is not None:
                    self._audit.emit_best_effort(
                        session_id=session_id,
                        state="destroy_failed",
                        failure_code=audit_failure_code(error),
                    )
                sys.stderr.write(
                    f"[E2B Python Strategy] sandbox cleanup failed: {_redacted_error(error)}\n"
                )
                sys.stderr.flush()
        if (
            reader_thread is not None
            and reader_thread is not threading.current_thread()
            and reader_thread.is_alive()
        ):
            reader_thread.join(timeout=1)

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        # Let the guest worker observe EOF and invoke BaseStrategy.on_stop;
        # the bounded join escalates to kill when it does not exit promptly.
        self._stop_session(graceful=True)


def _serve_jsonl(worker: E2BPythonStrategyWorker) -> None:
    for line in sys.stdin:
        if line.strip():
            sys.stdout.write(worker.request(line) + "\n")
            sys.stdout.flush()


def _serve_framed(worker: E2BPythonStrategyWorker) -> None:
    stream = sys.stdin.buffer
    output = sys.stdout.buffer
    while True:
        frame = read_frame(stream)
        if frame is None:
            return
        kind, sequence, payload = frame
        if kind != REQUEST:
            raise ValueError(f"E2B bridge expected request frame, got kind={kind}")
        response = worker.request(payload.decode("utf-8"))
        output.write(encode_frame(RESPONSE, sequence, response.encode("utf-8")))
        output.flush()


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Qianxing persistent E2B strategy bridge"
    )
    parser.add_argument("--module", required=True, help="local .py strategy file")
    parser.add_argument("--protocol", choices=("jsonl", "framed_json"), default="jsonl")
    args = parser.parse_args()
    worker: E2BPythonStrategyWorker | None = None
    try:
        worker = E2BPythonStrategyWorker(args.module)
        if args.protocol == "framed_json":
            _serve_framed(worker)
        else:
            _serve_jsonl(worker)
        return 0
    except Exception as error:  # noqa: BLE001 - provider and strategy startup share this process boundary.
        sys.stderr.write(f"[E2B Python Strategy] {_redacted_error(error)}\n")
        sys.stderr.flush()
        return 1
    finally:
        if worker is not None:
            worker.close()


if __name__ == "__main__":
    raise SystemExit(main())
