"""Host bridge for a persistent Python strategy worker in Microsandbox.

The Microsandbox SDK and its API key stay in this host-side process. Strategy
code runs in a microVM with network egress disabled and only receives the
versioned Qianxing JSONL/Framed-JSON contract.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import importlib.metadata
import io
import json
import os
import sys
import time
import uuid
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlsplit

from qianxing.sandbox_execution import (
    _bundle_remote_inputs,
    _input_preflight_script,
    _require_digest_pinned_image,
)

from .frame import REQUEST, RESPONSE, encode_frame, read_frame
from .sandbox_audit import (
    SandboxExecutionAudit,
    failure_code as audit_failure_code,
    request_data_fingerprint,
    request_identity,
)

_MAX_REMOTE_LINE_BYTES = 16 * 1024 * 1024
_MAX_REMOTE_LOG_BYTES = 8_000
_MAX_REMOTE_LOG_TOTAL_BYTES = 64 * 1024
_MAX_SESSION_TTL_SECONDS = 86_400
_REMOTE_ROOT = "/workspace/strategy"


def _redacted_error(error: BaseException) -> str:
    message = str(error)
    for name in ("MICROSANDBOX_API_KEY",):
        secret = os.environ.get(name, "")
        if secret:
            message = message.replace(secret, "<redacted>")
    return message


def _required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"required host setting {name} is missing")
    return value


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


def _bounded_int(name: str, default: int, minimum: int, maximum: int) -> int:
    raw = os.environ.get(name, str(default))
    try:
        value = int(raw)
    except ValueError as error:
        raise ValueError(f"{name} must be an integer") from error
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be from {minimum} through {maximum}")
    return value


class MicrosandboxPythonStrategyWorker:
    """One Microsandbox microVM and one persistent strategy process."""

    def __init__(self, module: str) -> None:
        self.backend = os.environ.get("QX_MICROSANDBOX_BACKEND", "local").strip().lower()
        if self.backend not in {"cloud", "local"}:
            raise ValueError("QX_MICROSANDBOX_BACKEND must be cloud or local")
        self.image = _require_digest_pinned_image(
            _required_env("QX_MICROSANDBOX_IMAGE"), "QX_MICROSANDBOX_IMAGE"
        )
        self.api_key = os.environ.get("MICROSANDBOX_API_KEY", "").strip()
        self.profile = os.environ.get("QX_MICROSANDBOX_PROFILE", "").strip()
        self.url = os.environ.get("QX_MICROSANDBOX_URL", "").strip()
        if self.url:
            parsed_url = urlsplit(self.url)
            if (
                parsed_url.scheme not in {"http", "https"}
                or not parsed_url.hostname
                or parsed_url.username is not None
                or parsed_url.password is not None
                or parsed_url.query
                or parsed_url.fragment
            ):
                raise ValueError(
                    "QX_MICROSANDBOX_URL must be an HTTP(S) base URL without embedded credentials, query, or fragment"
                )
        if self.backend == "cloud" and not (self.api_key or self.profile):
            raise RuntimeError(
                "Microsandbox cloud mode requires MICROSANDBOX_API_KEY or "
                "QX_MICROSANDBOX_PROFILE"
            )
        self.timeout_ms = _bounded_int(
            "QX_STRATEGY_PYTHON_TIMEOUT_MS", 30_000, 5_000, 60_000
        )
        self.session_ttl_seconds = _bounded_int(
            "QX_MICROSANDBOX_SESSION_TTL_SECONDS",
            3_600,
            max(60, (self.timeout_ms + 999) // 1000 + 30),
            _MAX_SESSION_TTL_SECONDS,
        )
        self.cpus = _bounded_int("QX_MICROSANDBOX_CPUS", 1, 1, 64)
        self.memory_mib = _bounded_int("QX_MICROSANDBOX_MEMORY_MIB", 1024, 512, 65_536)

        strategy_path = Path(module).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                "Microsandbox strategy mode requires strategy.python_module to resolve to a .py file"
            )
        self.spec, self.has_spec = _strategy_spec()
        self.package_version = importlib.metadata.version("qianxing")
        self.job_id = str(uuid.uuid4())
        self.bundle, self.manifest = _bundle_remote_inputs(
            strategy_path,
            self.spec,
            package_version=self.package_version,
            job_id=self.job_id,
        )
        self.artifact_sha256 = os.environ.get("QX_STRATEGY_ARTIFACT_SHA256", "").strip()
        actual_artifact_sha256 = hashlib.sha256(strategy_path.read_bytes()).hexdigest()
        if self.artifact_sha256:
            if actual_artifact_sha256.lower() != self.artifact_sha256.lower():
                raise ValueError(
                    "strategy source changed after host verification; refusing Microsandbox upload"
                )
        self.account_id = os.environ.get("QX_STRATEGY_ACCOUNT_ID")
        self.venue_id = os.environ.get("QX_STRATEGY_VENUE_ID")
        self._audit = SandboxExecutionAudit.from_environment(
            provider="microsandbox",
            job_id=self.job_id,
            package_version=self.package_version,
            entry_sha256=actual_artifact_sha256,
            bundle_sha256=self.manifest["strategy_transfer_sha256"],
            spec_sha256=self.manifest["input_spec_sha256"],
            resource_limits={
                "cpu_count": self.cpus,
                "memory_bytes": self.memory_mib * 1024 * 1024,
                "request_timeout_ms": self.timeout_ms,
                "session_ttl_seconds": self.session_ttl_seconds,
                "max_request_bytes": _MAX_REMOTE_LINE_BYTES,
                "max_response_bytes": _MAX_REMOTE_LINE_BYTES,
            },
            network_policy="deny_all",
            runtime_ref=self.image,
            backend=self.backend,
        )

        self.sandbox_type: Any = None
        self.network_type: Any = None
        self.backend_kind_type: Any = None
        self.backend_scope_factory: Any = None
        self.stdin_type: Any = None
        self.event_type: Any = None
        self.sandbox: Any = None
        self.handle: Any = None
        self.stdin: Any = None
        self._output_task: asyncio.Task[None] | None = None
        self._active_response: asyncio.Future[str] | None = None
        self._output_buffer = bytearray()
        self._remote_log_bytes = 0
        self._remote_log_truncated = False
        self._fatal_error: RuntimeError | None = None
        self._session_started_at = 0.0
        self._processed_requests = 0
        self._closed = False
        self._session_id: str | None = None

    def _load_sdk(self) -> None:
        try:
            from microsandbox import (
                BackendKind,
                ExecEventType,
                Network,
                Sandbox,
                Stdin,
                backend_scope,
            )
        except ImportError as error:
            raise RuntimeError(
                "Microsandbox strategy mode needs `pip install 'qianxing[sandbox-microsandbox]'`"
            ) from error
        self.sandbox_type = Sandbox
        self.network_type = Network
        self.backend_kind_type = BackendKind
        self.backend_scope_factory = backend_scope
        self.stdin_type = Stdin
        self.event_type = ExecEventType

    def backend_scope(self) -> Any:
        self._load_sdk()
        if self.backend == "local":
            return self.backend_scope_factory(self.backend_kind_type.LOCAL)
        options: dict[str, str] = {}
        if self.profile:
            options["profile"] = self.profile
        else:
            options["api_key"] = self.api_key
        if self.url:
            options["url"] = self.url
        return self.backend_scope_factory(self.backend_kind_type.CLOUD, **options)

    async def _start_session(self) -> None:
        session_id = str(uuid.uuid4())
        self._session_id = session_id
        name = "qianxing-" + uuid.uuid4().hex[:24]
        self._audit.emit(session_id=session_id, state="create_requested")
        try:
            self.sandbox = await self.sandbox_type.create(
                name,
                image=self.image,
                cpus=self.cpus,
                memory=self.memory_mib,
                max_duration=float(self.session_ttl_seconds),
                network=self.network_type.none(),
                labels={
                    "app": "qianxing-strategy",
                    "job": self.job_id[:48],
                    "session": session_id,
                },
            )
            self._session_started_at = time.monotonic()
            self._audit.emit(session_id=session_id, state="created")
            await self._stage_inputs()
            await self._check_remote_package()
            ready_file = f"{_REMOTE_ROOT}/.ready-{uuid.uuid4().hex}"
            command_env: dict[str, str] = {
                "PATH": "/usr/local/bin:/usr/bin:/bin",
                "PYTHONPATH": "/workspace/strategy",
                "QX_STRATEGY_STDOUT_TO_STDERR": "1",
            }
            if self.has_spec:
                command_env["QX_STRATEGY_SPEC_JSON"] = json.dumps(
                    self.spec, ensure_ascii=False, separators=(",", ":"), allow_nan=False
                )
            if self.account_id:
                command_env["QX_STRATEGY_ACCOUNT_ID"] = self.account_id
            if self.venue_id:
                command_env["QX_STRATEGY_VENUE_ID"] = self.venue_id
            if self.artifact_sha256:
                command_env["QX_STRATEGY_ARTIFACT_SHA256"] = self.artifact_sha256
            env_args = ["-i", *(f"{key}={value}" for key, value in command_env.items())]
            env_args.extend(
                [
                    "python",
                    "-u",
                    "-m",
                    "qianxing_strategy.worker",
                    "--module",
                    self.manifest["strategy_entrypoint"],
                    "--protocol",
                    "jsonl",
                    "--ready-file",
                    ready_file,
                ]
            )
            self.handle = await self.sandbox.exec_stream(
                "env",
                env_args,
                cwd="/workspace/strategy",
                stdin=self.stdin_type.pipe(),
            )
            self.stdin = self.handle.take_stdin()
            if self.stdin is None:
                raise RuntimeError("Microsandbox strategy worker has no stdin pipe")
            self._output_buffer.clear()
            self._remote_log_bytes = 0
            self._remote_log_truncated = False
            self._fatal_error = None
            self._output_task = asyncio.create_task(self._pump_output())
            await self._wait_for_ready(ready_file)
            self._audit.emit(session_id=session_id, state="ready")
        except Exception as error:
            self._audit.emit_best_effort(
                session_id=session_id,
                state="create_failed",
                failure_code=audit_failure_code(error),
            )
            await self._stop_session(force=True)
            raise

    async def _stage_inputs(self) -> None:
        with zipfile.ZipFile(io.BytesIO(self.bundle)) as archive:
            files = [member for member in archive.infolist() if not member.is_dir()]
            if not files:
                raise ValueError("Microsandbox strategy bundle is empty")
            directories: set[str] = {_REMOTE_ROOT}
            entries: list[tuple[str, bytes]] = []
            for member in files:
                relative = PurePosixPath(member.filename)
                if (
                    relative.is_absolute()
                    or not relative.parts
                    or ".." in relative.parts
                    or "\\" in member.filename
                ):
                    raise ValueError("strategy bundle contains an unsafe remote path")
                target = PurePosixPath("/workspace") / relative
                directories.add(str(target.parent))
                entries.append((str(target), archive.read(member)))
            result = await self.sandbox.exec(
                "mkdir", ["-p", *sorted(directories)], timeout=30.0
            )
            if not result.success:
                raise RuntimeError("Microsandbox could not create strategy bundle directories")
            for path, content in entries:
                await self.sandbox.fs.write(path, content)

    async def _check_remote_package(self) -> None:
        script = _input_preflight_script(self.package_version)
        result = await self.sandbox.exec(
            "env",
            [
                "-i",
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "python",
                "-c",
                script,
            ],
            timeout=30.0,
        )
        if not result.success:
            stderr = result.stderr_text.encode("utf-8", errors="replace")[-_MAX_REMOTE_LOG_BYTES:]
            raise RuntimeError(
                "Microsandbox strategy bundle/package preflight failed; the image must "
                f"contain Qianxing {self.package_version}: "
                + stderr.decode("utf-8", errors="replace")
            )

    async def _wait_for_ready(self, ready_file: str) -> None:
        script = "\n".join(
            [
                "import pathlib, sys, time",
                f"path = pathlib.Path({ready_file!r})",
                "deadline = time.monotonic() + 30",
                "while not path.is_file():",
                "    if time.monotonic() >= deadline:",
                "        sys.exit(78)",
                "    time.sleep(0.05)",
            ]
        )
        result = await self.sandbox.exec(
            "env",
            ["-i", "PATH=/usr/local/bin:/usr/bin:/bin", "python", "-c", script],
            timeout=35.0,
        )
        if not result.success:
            raise RuntimeError(
                "Microsandbox Python strategy worker did not initialize within 30 seconds"
            )

    async def _pump_output(self) -> None:
        try:
            async for event in self.handle:
                if event.event_type == self.event_type.STDOUT:
                    self._consume_stdout(event.data or b"")
                elif event.event_type == self.event_type.STDERR:
                    self._write_remote_stderr(event.data or b"")
                elif event.event_type in {
                    self.event_type.FAILED,
                    self.event_type.STDIN_ERROR,
                    self.event_type.EXITED,
                }:
                    code = getattr(event, "code", None)
                    detail = getattr(event, "data", None)
                    message = "Microsandbox strategy worker exited unexpectedly"
                    if code is not None:
                        message += f" with code {code}"
                    if detail:
                        message += ": " + bytes(detail)[-_MAX_REMOTE_LOG_BYTES:].decode(
                            "utf-8", errors="replace"
                        )
                    self._set_fatal(message)
                    return
        except asyncio.CancelledError:
            raise
        except Exception as error:  # noqa: BLE001 - provider stream errors must wake the caller.
            self._set_fatal("Microsandbox output stream failed: " + _redacted_error(error))
            return
        self._set_fatal("Microsandbox strategy output stream ended unexpectedly")

    def _consume_stdout(self, chunk: bytes) -> None:
        if len(self._output_buffer) + len(chunk) > _MAX_REMOTE_LINE_BYTES:
            self._output_buffer.clear()
            self._set_fatal("Microsandbox strategy response exceeded the size limit")
            return
        self._output_buffer.extend(chunk)
        while b"\n" in self._output_buffer:
            line, _, remainder = self._output_buffer.partition(b"\n")
            self._output_buffer = bytearray(remainder)
            future = self._active_response
            if not line or future is None or future.done():
                self._set_fatal(
                    "Microsandbox strategy emitted an empty, stale, or duplicate protocol response"
                )
                return
            if len(line) > _MAX_REMOTE_LINE_BYTES:
                self._set_fatal("Microsandbox strategy response exceeded the size limit")
                return
            try:
                decoded = line.rstrip(b"\r").decode("utf-8")
            except UnicodeDecodeError as error:
                self._set_fatal("Microsandbox strategy response is not UTF-8: " + str(error))
                return
            future.set_result(decoded)

    def _write_remote_stderr(self, chunk: bytes) -> None:
        if not chunk:
            return
        remaining = _MAX_REMOTE_LOG_TOTAL_BYTES - self._remote_log_bytes
        if remaining > 0:
            excerpt = chunk[:remaining]
            self._remote_log_bytes += len(excerpt)
            sys.stderr.write(
                "[Microsandbox Python Strategy] "
                + excerpt.decode("utf-8", errors="replace")
            )
        if len(chunk) > remaining and not self._remote_log_truncated:
            self._remote_log_truncated = True
            sys.stderr.write(
                "\n[Microsandbox Python Strategy] further guest stderr was truncated\n"
            )
        sys.stderr.flush()

    def _set_fatal(self, message: str) -> None:
        if self._fatal_error is not None:
            return
        self._fatal_error = RuntimeError(message)
        pending = self._active_response
        if pending is not None and not pending.done():
            pending.set_exception(self._fatal_error)

    async def _request(self, payload: str) -> str:
        if self._closed:
            raise RuntimeError("Microsandbox strategy worker is closed")
        if len(payload.encode("utf-8")) > _MAX_REMOTE_LINE_BYTES:
            raise ValueError("strategy request exceeded the Microsandbox bridge size limit")
        strategy_id, strategy_version = request_identity(payload)
        if self.sandbox is None:
            await self._start_session()
        elif self._session_needs_rotation():
            checkpoint_available = self._request_contains_checkpoint(payload)
            if self._processed_requests and not checkpoint_available:
                raise RuntimeError(
                    "Microsandbox session lease is near expiry and the request has no "
                    "persisted strategy checkpoint; refusing a state-losing restart"
                )
            await self._stop_session(force=False)
            await self._start_session()
        if self._fatal_error is not None:
            raise self._fatal_error
        if self._active_response is not None:
            raise RuntimeError("Microsandbox strategy bridge received a nested request")

        future: asyncio.Future[str] = asyncio.get_running_loop().create_future()
        self._active_response = future
        try:
            await self.stdin.write(payload.rstrip("\r\n").encode("utf-8") + b"\n")
            response = await asyncio.wait_for(
                future, timeout=max(0.1, self.timeout_ms / 1000 - 0.25)
            )
            if self._fatal_error is not None:
                raise self._fatal_error
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
        except TimeoutError as error:
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
            await self._stop_session(force=True)
            self._closed = True
            raise TimeoutError(
                "Microsandbox strategy response timed out; failing closed without local fallback"
            ) from error
        except Exception as error:
            if self._session_id is not None:
                self._audit.emit(
                    session_id=self._session_id,
                    state="request_failed",
                    failure_code=audit_failure_code(error),
                    request_sequence=self._processed_requests + 1,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=request_data_fingerprint(payload),
                )
            await self._stop_session(force=True)
            self._closed = True
            raise RuntimeError(
                "Microsandbox strategy request failed; failing closed without local fallback: "
                + _redacted_error(error)
            ) from error
        finally:
            if self._active_response is future:
                self._active_response = None

    def _session_needs_rotation(self) -> bool:
        renew_before = min(60, max(1, self.session_ttl_seconds // 4))
        return time.monotonic() - self._session_started_at >= (
            self.session_ttl_seconds - renew_before
        )

    @staticmethod
    def _request_contains_checkpoint(payload: str) -> bool:
        try:
            value = json.loads(payload)
        except json.JSONDecodeError:
            return False
        return isinstance(value, dict) and isinstance(value.get("strategy_checkpoint"), dict)

    async def _stop_session(self, *, force: bool) -> None:
        handle, self.handle = self.handle, None
        sink, self.stdin = self.stdin, None
        sandbox, self.sandbox = self.sandbox, None
        session_id, self._session_id = self._session_id, None
        output_task, self._output_task = self._output_task, None
        force_cleanup = force
        if sink is not None:
            try:
                await sink.close()
            except Exception as error:  # noqa: BLE001 - teardown must continue.
                sys.stderr.write(
                    "[Microsandbox Python Strategy] closing stdin failed: "
                    + _redacted_error(error)
                    + "\n"
                )
        if output_task is not None and not force_cleanup and not output_task.done():
            try:
                await asyncio.wait_for(asyncio.shield(output_task), timeout=5.0)
            except TimeoutError:
                force_cleanup = True
        if handle is not None and force_cleanup:
            try:
                await handle.kill()
            except Exception as error:  # noqa: BLE001 - sandbox destroy is the outer cleanup bound.
                sys.stderr.write(
                    "[Microsandbox Python Strategy] stopping guest process failed: "
                    + _redacted_error(error)
                    + "\n"
                )
        if output_task is not None and not output_task.done():
            output_task.cancel()
            try:
                await asyncio.wait_for(output_task, timeout=1.0)
            except (asyncio.CancelledError, TimeoutError):
                pass
        if sandbox is not None:
            if session_id is not None:
                self._audit.emit_best_effort(session_id=session_id, state="destroy_requested")
            try:
                await sandbox.destroy(force=force_cleanup, timeout=10.0)
                if session_id is not None:
                    self._audit.emit_best_effort(session_id=session_id, state="destroyed")
            except Exception as error:  # noqa: BLE001 - provider lease remains a final bound.
                if session_id is not None:
                    self._audit.emit_best_effort(
                        session_id=session_id,
                        state="destroy_failed",
                        failure_code=audit_failure_code(error),
                    )
                sys.stderr.write(
                    "[Microsandbox Python Strategy] sandbox cleanup failed: "
                    + _redacted_error(error)
                    + "\n"
                )
                sys.stderr.flush()

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        # A normal host shutdown must EOF the guest worker so it can run the
        # strategy's on_stop hook. _stop_session bounds that grace period and
        # escalates to kill/destroy if the guest does not exit promptly.
        await self._stop_session(force=False)


async def _serve_jsonl(worker: MicrosandboxPythonStrategyWorker) -> None:
    while True:
        line = await asyncio.to_thread(sys.stdin.readline)
        if not line:
            return
        if line.strip():
            sys.stdout.write(await worker._request(line) + "\n")
            sys.stdout.flush()


async def _read_frame_async(stream: Any) -> Any:
    return await asyncio.to_thread(read_frame, stream)


async def _serve_framed(worker: MicrosandboxPythonStrategyWorker) -> None:
    input_stream = sys.stdin.buffer
    output_stream = sys.stdout.buffer
    while True:
        frame = await _read_frame_async(input_stream)
        if frame is None:
            return
        kind, sequence, payload = frame
        if kind != REQUEST:
            raise ValueError(f"Microsandbox bridge expected request frame, got kind={kind}")
        response = await worker._request(payload.decode("utf-8"))
        output_stream.write(encode_frame(RESPONSE, sequence, response.encode("utf-8")))
        output_stream.flush()


async def _run(module: str, protocol: str) -> int:
    worker = MicrosandboxPythonStrategyWorker(module)
    try:
        with worker.backend_scope():
            if protocol == "framed_json":
                await _serve_framed(worker)
            else:
                await _serve_jsonl(worker)
        return 0
    finally:
        await worker.close()


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Qianxing persistent Microsandbox strategy bridge"
    )
    parser.add_argument("--module", required=True, help="local .py strategy file")
    parser.add_argument("--protocol", choices=("jsonl", "framed_json"), default="jsonl")
    args = parser.parse_args()
    try:
        return asyncio.run(_run(args.module, args.protocol))
    except Exception as error:  # noqa: BLE001 - provider and strategy failures cross this boundary.
        sys.stderr.write("[Microsandbox Python Strategy] " + _redacted_error(error) + "\n")
        sys.stderr.flush()
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
