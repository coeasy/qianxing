"""Host-side bridge for a persistent Python strategy worker in OpenSandbox."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import io
import json
import os
import shlex
import sys
import time
import uuid
import zipfile
from datetime import timedelta
from pathlib import Path, PurePosixPath
from typing import Any

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
_MAX_SESSION_TTL_SECONDS = 86_400
_REMOTE_ROOT = "/workspace/qianxing"
_REMOTE_QUEUE_ROOT = _REMOTE_ROOT + "/runtime"


def _required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"required host setting {name} is missing")
    return value


def _bounded_int(name: str, default: int, minimum: int, maximum: int) -> int:
    raw = os.environ.get(name, str(default))
    try:
        value = int(raw)
    except ValueError as error:
        raise ValueError(f"{name} must be an integer") from error
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be from {minimum} through {maximum}")
    return value


def _redacted_error(error: BaseException) -> str:
    message = str(error)
    api_key = os.environ.get("OPEN_SANDBOX_API_KEY", "")
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


def _guest_command(
    *,
    entrypoint: str,
    spec: dict[str, Any],
    has_spec: bool,
    account_id: str | None,
    venue_id: str | None,
    artifact_sha256: str | None,
    ready_file: str,
) -> str:
    values = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "PYTHONPATH": "/workspace/strategy",
        "QX_STRATEGY_STDOUT_TO_STDERR": "1",
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
    assignments = " ".join(
        shlex.quote(f"{key}={value}") for key, value in values.items()
    )
    return (
        f"env -i {assignments} python -u -m qianxing_strategy.opensandbox_guest "
        f"--module {shlex.quote(entrypoint)} "
        f"--ready-file {shlex.quote(ready_file)} "
        f"--queue-root {shlex.quote(_REMOTE_QUEUE_ROOT)}"
    )


class OpenSandboxPythonStrategyWorker:
    """One OpenSandbox instance and one persistent strategy process per host worker."""

    def __init__(self, module: str) -> None:
        self.domain = _required_env("OPEN_SANDBOX_DOMAIN")
        self.image = _require_digest_pinned_image(
            _required_env("QX_OPENSANDBOX_IMAGE"), "QX_OPENSANDBOX_IMAGE"
        )
        self.api_key = os.environ.get("OPEN_SANDBOX_API_KEY", "").strip() or None
        self.timeout_ms = int(os.environ.get("QX_STRATEGY_PYTHON_TIMEOUT_MS", "30000"))
        if not 15_000 <= self.timeout_ms <= 60_000:
            raise ValueError(
                "OpenSandbox strategy mode requires QX_STRATEGY_PYTHON_TIMEOUT_MS "
                "from 15000 through 60000 for file and command API round trips"
            )
        raw_ttl = os.environ.get("QX_OPENSANDBOX_SESSION_TTL_SECONDS", "3600")
        try:
            self.session_ttl_seconds = int(raw_ttl)
        except ValueError as error:
            raise ValueError(
                "QX_OPENSANDBOX_SESSION_TTL_SECONDS must be an integer"
            ) from error
        minimum_ttl = max(60, (self.timeout_ms + 999) // 1000 + 30)
        if not minimum_ttl <= self.session_ttl_seconds <= _MAX_SESSION_TTL_SECONDS:
            raise ValueError(
                "QX_OPENSANDBOX_SESSION_TTL_SECONDS must be at least the strategy "
                f"timeout plus 30 seconds ({minimum_ttl}) and no more than 86400"
            )
        self.cpus = _bounded_int("QX_OPENSANDBOX_CPUS", 1, 1, 64)
        self.memory_mib = _bounded_int(
            "QX_OPENSANDBOX_MEMORY_MIB", 2048, 512, 65_536
        )

        strategy_path = Path(module).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                "OpenSandbox strategy mode requires strategy.python_module to resolve to a .py file"
            )
        spec, has_spec = _strategy_spec()
        package_version = importlib.metadata.version("qianxing")
        self.job_id = str(uuid.uuid4())
        self.bundle, self.manifest = _bundle_remote_inputs(
            strategy_path,
            spec,
            package_version=package_version,
            job_id=self.job_id,
        )
        expected_artifact = os.environ.get("QX_STRATEGY_ARTIFACT_SHA256", "").strip()
        actual_artifact = hashlib.sha256(strategy_path.read_bytes()).hexdigest()
        if expected_artifact:
            if actual_artifact.lower() != expected_artifact.lower():
                raise ValueError(
                    "strategy source changed after host verification; refusing OpenSandbox upload"
                )

        try:
            from opensandbox import SandboxSync
            from opensandbox.config import ConnectionConfigSync
            from opensandbox.models.execd import RunCommandOpts
            from opensandbox.models.filesystem import WriteEntry
            from opensandbox.models.sandboxes import NetworkPolicy
        except ImportError as error:
            raise RuntimeError(
                "OpenSandbox strategy mode needs `pip install 'qianxing[sandbox-opensandbox]'`"
            ) from error

        self._sandbox_type = SandboxSync
        self._connection_config_type = ConnectionConfigSync
        self._run_command_opts_type = RunCommandOpts
        self._write_entry_type = WriteEntry
        self._network_policy_type = NetworkPolicy
        self.package_version = package_version
        self.spec = spec
        self.has_spec = has_spec
        self.account_id = os.environ.get("QX_STRATEGY_ACCOUNT_ID")
        self.venue_id = os.environ.get("QX_STRATEGY_VENUE_ID")
        self.artifact_sha256 = expected_artifact or None
        self._audit = SandboxExecutionAudit.from_environment(
            provider="opensandbox",
            job_id=self.job_id,
            package_version=package_version,
            entry_sha256=actual_artifact,
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
        )
        self.sandbox = None
        self.execution_id: str | None = None
        self._session_id: str | None = None
        self._closed = False
        self._session_started_at = 0.0
        self._processed_requests = 0
        self._request_sequence = 0

    def _command_opts(self, seconds: float) -> Any:
        return self._run_command_opts_type(timeout=timedelta(seconds=max(1, seconds)))

    def _start_session(self) -> None:
        session_id = str(uuid.uuid4())
        self._session_id = session_id
        self._audit.emit(session_id=session_id, state="create_requested")
        config = self._connection_config_type(
            domain=self.domain,
            api_key=self.api_key,
            request_timeout=timedelta(seconds=self.timeout_ms / 1000 + 30),
            use_server_proxy=True,
        )
        try:
            self.sandbox = self._sandbox_type.create(
                self.image,
                connection_config=config,
                timeout=timedelta(seconds=self.session_ttl_seconds),
                resource={"cpu": str(self.cpus), "memory": f"{self.memory_mib}Mi"},
                network_policy=self._network_policy_type(defaultAction="deny", egress=[]),
                metadata={
                    "qianxing_job_id": self.job_id,
                    "qianxing_session_id": session_id,
                    "qianxing_strategy_sha256": self.manifest["strategy_transfer_sha256"],
                    "qianxing_spec_sha256": self.manifest["input_spec_sha256"],
                    "qianxing_resource_cpu": str(self.cpus),
                    "qianxing_resource_memory_mib": str(self.memory_mib),
                    "qianxing_image_ref": self.image,
                },
            )
            self._session_started_at = time.monotonic()
            self._audit.emit(session_id=session_id, state="created")
            self._stage_inputs()
            self._check_remote_package()
            ready_file = f"{_REMOTE_ROOT}/.ready-{session_id}"
            command = _guest_command(
                entrypoint=self.manifest["strategy_entrypoint"],
                spec=self.spec,
                has_spec=self.has_spec,
                account_id=self.account_id,
                venue_id=self.venue_id,
                artifact_sha256=self.artifact_sha256,
                ready_file=ready_file,
            )
            execution = self.sandbox.commands.run(
                command,
                opts=self._run_command_opts_type(
                    background=True,
                    timeout=timedelta(seconds=self.session_ttl_seconds),
                ),
            )
            self.execution_id = getattr(execution, "id", None)
            if not self.execution_id:
                raise RuntimeError(
                    "OpenSandbox did not return the guest worker execution id"
                )
            self._wait_for_file(ready_file, timeout_seconds=30)
            self._audit.emit(session_id=session_id, state="ready")
        except Exception as error:
            self._audit.emit_best_effort(
                session_id=session_id,
                state="create_failed",
                failure_code=audit_failure_code(error),
            )
            self._stop_session(graceful=False)
            raise

    def _stage_inputs(self) -> None:
        assert self.sandbox is not None
        with zipfile.ZipFile(io.BytesIO(self.bundle)) as archive:
            members = [member for member in archive.infolist() if not member.is_dir()]
            paths: list[str] = []
            entries = []
            for member in members:
                relative = PurePosixPath(member.filename)
                if (
                    relative.is_absolute()
                    or not relative.parts
                    or ".." in relative.parts
                    or "\\" in member.filename
                ):
                    raise ValueError("strategy bundle contains an unsafe remote path")
                paths.append(str(PurePosixPath("/workspace") / relative.parent))
                entries.append(
                    self._write_entry_type(
                        path="/workspace/" + relative.as_posix(),
                        data=archive.read(member),
                        mode=0o600,
                    )
                )
            if not entries:
                raise ValueError("strategy bundle is empty")
            mkdir = "mkdir -p " + " ".join(
                shlex.quote(path) for path in sorted(set(paths))
            )
            result = self.sandbox.commands.run(mkdir, opts=self._command_opts(30))
            if getattr(result, "exit_code", 0) != 0:
                raise RuntimeError(
                    "OpenSandbox could not create the strategy bundle directories"
                )
            self.sandbox.files.write_files(entries)
            queue_result = self.sandbox.commands.run(
                "mkdir -p "
                + shlex.quote(_REMOTE_QUEUE_ROOT + "/requests")
                + " "
                + shlex.quote(_REMOTE_QUEUE_ROOT + "/responses"),
                opts=self._command_opts(30),
            )
            if getattr(queue_result, "exit_code", 0) != 0:
                raise RuntimeError(
                    "OpenSandbox could not create strategy request queues"
                )

    def _check_remote_package(self) -> None:
        assert self.sandbox is not None
        script = _input_preflight_script(self.package_version)
        command = "env -i PATH=/usr/local/bin:/usr/bin:/bin python -c " + shlex.quote(
            script
        )
        result = self.sandbox.commands.run(command, opts=self._command_opts(30))
        if getattr(result, "exit_code", 0) != 0:
            logs = getattr(result, "logs", None)
            stderr_items = getattr(logs, "stderr", []) or []
            stderr = "".join(getattr(item, "text", "") for item in stderr_items)[
                -_MAX_REMOTE_LOG_BYTES:
            ]
            raise RuntimeError(
                "OpenSandbox strategy bundle/package preflight failed; the image must "
                f"contain Qianxing {self.package_version}: {stderr}"
            )

    def _wait_for_file(
        self, path: str, *, timeout_seconds: float, sandbox: Any | None = None
    ) -> None:
        sandbox = sandbox or self.sandbox
        if sandbox is None:
            raise RuntimeError("OpenSandbox strategy worker is not running")
        wait_script = f"""
import pathlib
import sys
import time

path = pathlib.Path({path!r})
deadline = time.monotonic() + {max(0.1, timeout_seconds)!r}
while not path.is_file():
    if time.monotonic() >= deadline:
        sys.stderr.write('timed out waiting for strategy response file\\n')
        raise SystemExit(124)
    time.sleep(0.02)
if path.stat().st_size > {_MAX_REMOTE_LINE_BYTES}:
    sys.stderr.write('strategy response file exceeded the size limit\\n')
    raise SystemExit(125)
""".strip()
        command = "env -i PATH=/usr/local/bin:/usr/bin:/bin python -c " + shlex.quote(
            wait_script
        )
        result = sandbox.commands.run(
            command,
            opts=self._command_opts(timeout_seconds + 5),
        )
        exit_code = getattr(result, "exit_code", 0)
        if exit_code == 124:
            raise TimeoutError(
                f"OpenSandbox strategy worker did not publish {Path(path).name} before timeout"
            )
        if exit_code != 0:
            logs = getattr(result, "logs", None)
            stderr_items = getattr(logs, "stderr", []) or []
            stderr = "".join(getattr(item, "text", "") for item in stderr_items)[
                -_MAX_REMOTE_LOG_BYTES:
            ]
            raise RuntimeError(
                "OpenSandbox strategy response wait failed"
                + (f": {stderr}" if stderr else "")
            )

    def _check_guest_status(self) -> None:
        if self.sandbox is None or not self.execution_id:
            raise RuntimeError("OpenSandbox strategy worker is not running")
        status = self.sandbox.commands.get_command_status(self.execution_id)
        if getattr(status, "running", True) is False:
            exit_code = getattr(status, "exit_code", None)
            error = getattr(status, "error", None)
            raise RuntimeError(
                "OpenSandbox strategy worker exited unexpectedly"
                + (f" with exit code {exit_code}" if exit_code is not None else "")
                + (f": {error}" if error else "")
            )

    def request(self, payload: str) -> str:
        if self._closed:
            raise RuntimeError("OpenSandbox strategy worker is closed")
        if len(payload.encode("utf-8")) > _MAX_REMOTE_LINE_BYTES:
            raise ValueError(
                "strategy request exceeded the OpenSandbox bridge size limit"
            )
        strategy_id, strategy_version = request_identity(payload)
        if self.sandbox is None:
            self._start_session()
        elif self._session_needs_rotation():
            checkpoint_available = self._request_contains_checkpoint(payload)
            if self._processed_requests and not checkpoint_available:
                raise RuntimeError(
                    "OpenSandbox session lease is near expiry and the request has no "
                    "persisted strategy checkpoint; refusing a state-losing restart"
                )
            self._stop_session(graceful=True)
            try:
                self._start_session()
            except Exception:
                self._closed = True
                raise
        if not self._has_request_budget():
            if self._session_id is not None:
                self._audit.emit_best_effort(
                    session_id=self._session_id,
                    state="request_failed",
                    failure_code="RequestBudgetUnavailable",
                    request_sequence=self._processed_requests + 1,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=request_data_fingerprint(payload),
                )
            raise TimeoutError(
                "OpenSandbox lease does not have enough time for the configured request; "
                "refusing to send the strategy event"
            )

        self._request_sequence += 1
        request_id = f"{self._request_sequence:020d}-{uuid.uuid4().hex}"
        request_path = f"{_REMOTE_QUEUE_ROOT}/requests/{request_id}.json"
        response_path = f"{_REMOTE_QUEUE_ROOT}/responses/{request_id}.json"
        envelope = json.dumps(
            {"request_id": request_id, "payload": payload},
            ensure_ascii=False,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
        if len(envelope) > _MAX_REMOTE_LINE_BYTES:
            raise ValueError(
                "encoded strategy request exceeded the OpenSandbox bridge size limit"
            )
        assert self.sandbox is not None
        self.sandbox.files.write_files(
            [self._write_entry_type(path=request_path, data=envelope, mode=0o600)]
        )
        try:
            self._wait_for_file(
                response_path,
                timeout_seconds=self.timeout_ms / 1000 - 0.25,
            )
            response = self.sandbox.files.read_bytes(response_path)
            if not isinstance(response, bytes):
                raise TypeError("OpenSandbox returned non-binary strategy response")
            if len(response) > _MAX_REMOTE_LINE_BYTES:
                raise RuntimeError(
                    "OpenSandbox strategy response exceeded the size limit"
                )
            self.sandbox.files.delete_files([request_path, response_path])
            response_text = response.decode("utf-8")
            if self._session_id is not None:
                self._audit.emit(
                    session_id=self._session_id,
                    state="request_completed",
                    request_sequence=self._processed_requests + 1,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=request_data_fingerprint(payload),
                    response_sha256=hashlib.sha256(response).hexdigest(),
                )
            self._processed_requests += 1
            return response_text
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
            self._stop_session(graceful=False)
            self._closed = True
            raise RuntimeError(
                "OpenSandbox strategy request failed; the host will fail closed without local fallback: "
                + _redacted_error(error)
            ) from error
        self._stop_session(graceful=False)
        self._closed = True
        raise TimeoutError(
            "OpenSandbox strategy response timed out; the host will fail closed without local fallback"
        )

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
        execution_id, self.execution_id = self.execution_id, None
        if sandbox is None:
            return
        if session_id is not None:
            self._audit.emit_best_effort(session_id=session_id, state="destroy_requested")
        if graceful and execution_id:
            request_id = "stop-" + uuid.uuid4().hex
            request_path = f"{_REMOTE_QUEUE_ROOT}/requests/{request_id}.json"
            response_path = f"{_REMOTE_QUEUE_ROOT}/responses/{request_id}.json"
            try:
                sandbox.files.write_files(
                    [
                        self._write_entry_type(
                            path=request_path,
                            data=json.dumps(
                                {"request_id": request_id, "close": True},
                                separators=(",", ":"),
                            ).encode("utf-8"),
                            mode=0o600,
                        )
                    ]
                )
                self._wait_for_file(response_path, timeout_seconds=5, sandbox=sandbox)
            except Exception as error:  # noqa: BLE001 - destroy remains the final cleanup bound.
                sys.stderr.write(
                    f"[OpenSandbox Python Strategy] graceful shutdown failed: {_redacted_error(error)}\n"
                )
                sys.stderr.flush()
        try:
            sandbox.destroy()
            if session_id is not None:
                self._audit.emit_best_effort(session_id=session_id, state="destroyed")
        except Exception as error:  # noqa: BLE001 - provider TTL bounds orphan lifetime.
            if session_id is not None:
                self._audit.emit_best_effort(
                    session_id=session_id,
                    state="destroy_failed",
                    failure_code=audit_failure_code(error),
                )
            sys.stderr.write(
                f"[OpenSandbox Python Strategy] sandbox cleanup failed: {_redacted_error(error)}\n"
            )
            sys.stderr.flush()

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._stop_session(graceful=True)


def _serve_jsonl(worker: OpenSandboxPythonStrategyWorker) -> None:
    for line in sys.stdin:
        if line.strip():
            sys.stdout.write(worker.request(line) + "\n")
            sys.stdout.flush()


def _serve_framed(worker: OpenSandboxPythonStrategyWorker) -> None:
    stream = sys.stdin.buffer
    output = sys.stdout.buffer
    while True:
        frame = read_frame(stream)
        if frame is None:
            return
        kind, sequence, payload = frame
        if kind != REQUEST:
            raise ValueError(
                f"OpenSandbox bridge expected request frame, got kind={kind}"
            )
        response = worker.request(payload.decode("utf-8"))
        output.write(encode_frame(RESPONSE, sequence, response.encode("utf-8")))
        output.flush()


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Qianxing persistent OpenSandbox strategy bridge"
    )
    parser.add_argument("--module", required=True, help="local .py strategy file")
    parser.add_argument("--protocol", choices=("jsonl", "framed_json"), default="jsonl")
    args = parser.parse_args()
    worker: OpenSandboxPythonStrategyWorker | None = None
    try:
        worker = OpenSandboxPythonStrategyWorker(args.module)
        if args.protocol == "framed_json":
            _serve_framed(worker)
        else:
            _serve_jsonl(worker)
        return 0
    except Exception as error:  # noqa: BLE001 - provider and strategy startup share this process boundary.
        sys.stderr.write(f"[OpenSandbox Python Strategy] {_redacted_error(error)}\n")
        sys.stderr.flush()
        return 1
    finally:
        if worker is not None:
            worker.close()


if __name__ == "__main__":
    raise SystemExit(main())
