"""Host bridge for a persistent strategy worker on a remote Docker host.

This provider uses llm-sandbox's interactive IPython session to keep one strategy
instance alive in a hardened container. Docker credentials and the remote Docker
client remain in this local bridge process; the guest sees only strategy inputs.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.metadata
import io
import json
import os
import re
import sys
import tarfile
import tempfile
import time
import uuid
import zipfile
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlsplit

from qianxing.sandbox_execution import _bundle_remote_inputs, _input_preflight_script

from .frame import REQUEST, RESPONSE, encode_frame, read_frame
from .sandbox_audit import (
    SandboxExecutionAudit,
    request_data_fingerprint,
    request_identity,
)
from .sandbox_audit import (
    failure_code as audit_failure_code,
)

_MAX_REMOTE_LINE_BYTES = 16 * 1024 * 1024
_MAX_REMOTE_LOG_BYTES = 8_000
_MAX_SESSION_TTL_SECONDS = 86_400
_SHA256_IMAGE = re.compile(r"^.+@sha256:[0-9a-fA-F]{64}$")
_RESPONSE_PREFIX = "__QIANXING_STRATEGY_RESPONSE__:"


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


def _redacted_error(error: BaseException) -> str:
    message = str(error)
    for name in (
        "QX_LLMSANDBOX_DOCKER_CLIENT_KEY",
        "QX_LLMSANDBOX_DOCKER_CLIENT_CERT",
        "QX_LLMSANDBOX_DOCKER_CA_CERT",
    ):
        value = os.environ.get(name, "")
        if value:
            message = message.replace(value, "<redacted-path>")
    return message


def _log_cleanup_failure(action: str, error: BaseException) -> None:
    sys.stderr.write(
        f"[llm-sandbox Python Strategy] {action}: "
        + _redacted_error(error)[-_MAX_REMOTE_LOG_BYTES:]
        + "\n"
    )
    sys.stderr.flush()


def _remote_docker_client() -> Any:
    host = _required_env("QX_LLMSANDBOX_DOCKER_HOST")
    parsed = urlsplit(host)
    if (
        parsed.scheme != "tcp"
        or not parsed.hostname
        or parsed.port is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
    ):
        raise ValueError(
            "QX_LLMSANDBOX_DOCKER_HOST must be a credential-free tcp://host:port endpoint"
        )
    ca_cert = Path(_required_env("QX_LLMSANDBOX_DOCKER_CA_CERT")).expanduser()
    client_cert = Path(_required_env("QX_LLMSANDBOX_DOCKER_CLIENT_CERT")).expanduser()
    client_key = Path(_required_env("QX_LLMSANDBOX_DOCKER_CLIENT_KEY")).expanduser()
    for path, name in (
        (ca_cert, "QX_LLMSANDBOX_DOCKER_CA_CERT"),
        (client_cert, "QX_LLMSANDBOX_DOCKER_CLIENT_CERT"),
        (client_key, "QX_LLMSANDBOX_DOCKER_CLIENT_KEY"),
    ):
        if not path.is_file():
            raise ValueError(f"{name} must name a readable TLS file")
    try:
        import docker
    except ImportError as error:
        raise RuntimeError(
            "llm-sandbox strategy mode needs `pip install 'qianxing[sandbox-llm]'`"
        ) from error
    tls = docker.tls.TLSConfig(
        client_cert=(str(client_cert), str(client_key)),
        ca_cert=str(ca_cert),
        verify=True,
    )
    return docker.DockerClient(base_url=host, tls=tls, timeout=30)


def _validate_image(image: str) -> str:
    if not _SHA256_IMAGE.fullmatch(image):
        raise ValueError(
            "QX_LLMSANDBOX_IMAGE must use repository@sha256:<64 hexadecimal characters>"
        )
    return image


def _extract_bundle_script() -> str:
    return r"""
import zipfile
from pathlib import Path, PurePosixPath
root = Path('/workspace').resolve()
bundle = root / 'qianxing-strategy-input.zip'
with zipfile.ZipFile(bundle) as archive:
    for member in archive.infolist():
        rel = PurePosixPath(member.filename)
        if rel.is_absolute() or not rel.parts or '..' in rel.parts or '\\' in member.filename or ':' in member.filename:
            raise ValueError('strategy bundle contains an unsafe path')
        target = (root / Path(*rel.parts)).resolve()
        try:
            target.relative_to(root)
        except ValueError as error:
            raise ValueError('strategy bundle path escapes workspace') from error
        if member.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(member))
""".strip()


def _guest_environment_setup(
    *,
    entrypoint: str,
    spec: dict[str, Any],
    has_spec: bool,
    account_id: str | None,
    venue_id: str | None,
    artifact_sha256: str | None,
    package_version: str,
) -> str:
    # Values are serialized as Python literals and contain only host-validated
    # identity/config data. Provider credentials never enter this guest namespace.
    env = {
        "HOME": "/tmp",
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "PYTHONPATH": "/workspace/strategy",
        "TMPDIR": "/tmp",
        "QX_STRATEGY_STDOUT_TO_STDERR": "1",
    }
    if has_spec:
        env["QX_STRATEGY_SPEC_JSON"] = json.dumps(
            spec, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        )
    if account_id:
        env["QX_STRATEGY_ACCOUNT_ID"] = account_id
    if venue_id:
        env["QX_STRATEGY_VENUE_ID"] = venue_id
    if artifact_sha256:
        env["QX_STRATEGY_ARTIFACT_SHA256"] = artifact_sha256
    setup = "\n".join(
        [
            "import argparse as _qx_argparse",
            "import contextlib as _qx_contextlib",
            "import io as _qx_io",
            "import json as _qx_json",
            "import os as _qx_os",
            "import sys as _qx_sys",
            "_qx_os.environ.clear()",
            f"_qx_os.environ.update({_python_literal(env)})",
            "if '/workspace/strategy' not in _qx_sys.path:",
            "    _qx_sys.path.insert(0, '/workspace/strategy')",
            f"if _qx_importlib_metadata_version() != {_python_literal(package_version)}:",
            "    raise RuntimeError('remote image qianxing package version mismatch')",
            "_qx_handler_setup_log = _qx_io.StringIO()",
            "with _qx_contextlib.redirect_stdout(_qx_handler_setup_log), _qx_contextlib.redirect_stderr(_qx_handler_setup_log):",
            "    from qianxing_strategy.worker import _initialize_handlers as _qx_initialize_handlers",
            "    from qianxing_strategy.worker import _handle_line as _qx_handle_line",
            "    from qianxing_strategy.worker import _close_handler as _qx_close_handler",
            "    _qx_handler, _qx_diagnostic, _qx_checkpoint_handler = _qx_initialize_handlers(",
            f"        _qx_argparse.Namespace(module={_python_literal(entrypoint)}, include_diagnostics=False),",
            "        _qx_argparse.ArgumentParser(),",
            "    )",
            "if _qx_checkpoint_handler is None:",
            "    raise RuntimeError('llm-sandbox strategy handler has no durable checkpoint hook')",
        ]
    )
    return setup


def _python_literal(value: Any) -> str:
    return repr(value)


def _copy_to_unprivileged_guest(session: Any, source: str, destination: str) -> None:
    """Copy one file with guest ownership and readable mode into the Docker container.

    llm-sandbox's generic Docker copy helper preserves the host temporary-file
    mode/owner. Its interactive kernel runs as UID 65532 here, so SDK-created
    mode-0600 command files would otherwise be unreadable. Build a small tar
    entry with explicit guest ownership instead of widening host temp-file ACLs.
    """
    source_path = Path(source)
    remote_path = PurePosixPath(destination)
    if (
        not source_path.is_file()
        or not remote_path.is_absolute()
        or not remote_path.parts
        or not remote_path.name
        or ".." in remote_path.parts
        or "\\" in destination
        or ":" in destination
    ):
        raise ValueError("llm-sandbox copy target must be a file and a safe absolute guest path")
    backend_session = getattr(session, "_backend_session", None)
    container = getattr(backend_session, "container", None)
    if container is None:
        raise RuntimeError("llm-sandbox Docker container is not open for file transfer")

    archive_buffer = io.BytesIO()
    with source_path.open("rb") as source_file, tarfile.open(
        fileobj=archive_buffer, mode="w"
    ) as archive:
        member = tarfile.TarInfo(name=remote_path.name)
        member.size = source_path.stat().st_size
        member.mode = 0o644
        member.uid = 65_532
        member.gid = 65_532
        member.uname = ""
        member.gname = ""
        archive.addfile(member, source_file)
    uploaded = container.put_archive(remote_path.parent.as_posix(), archive_buffer.getvalue())
    if not uploaded:
        raise RuntimeError("llm-sandbox Docker host rejected a strategy runtime file")


def _response_code(payload: str, marker: str) -> str:
    encoded = base64.b64encode(payload.encode("utf-8")).decode("ascii")
    return "\n".join(
        [
            "import base64 as _qx_b64",
            "import contextlib as _qx_contextlib",
            "import io as _qx_io",
            f"_qx_payload = _qx_b64.b64decode({encoded!r}).decode('utf-8')",
            "_qx_strategy_log_sink = _QxBoundedSink()",
            "with _qx_contextlib.redirect_stdout(_qx_strategy_log_sink), _qx_contextlib.redirect_stderr(_qx_strategy_log_sink):",
            "    _qx_response = _qx_handle_line(_qx_handler, _qx_payload, checkpoint_handler=_qx_checkpoint_handler)",
            f"print({marker!r} + _qx_response)",
        ]
    )


class LlmSandboxPythonStrategyWorker:
    """A remote Docker container and persistent InteractiveSandboxSession."""

    def __init__(self, module: str) -> None:
        self.image = _validate_image(_required_env("QX_LLMSANDBOX_IMAGE"))
        self.timeout_ms = _bounded_int(
            "QX_STRATEGY_PYTHON_TIMEOUT_MS", 30_000, 15_000, 60_000
        )
        self.session_ttl_seconds = _bounded_int(
            "QX_LLMSANDBOX_SESSION_TTL_SECONDS",
            3_600,
            max(60, (self.timeout_ms + 999) // 1000 + 30),
            _MAX_SESSION_TTL_SECONDS,
        )
        self.expected_cpu_count = _bounded_int(
            "QX_LLMSANDBOX_EXPECTED_CPU_COUNT", 2, 1, 64
        )
        self.expected_memory_mb = _bounded_int(
            "QX_LLMSANDBOX_EXPECTED_MEMORY_MB", 2_048, 512, 65_536
        )

        strategy_path = Path(module).expanduser().resolve(strict=True)
        if not strategy_path.is_file() or strategy_path.suffix.lower() != ".py":
            raise ValueError(
                "llm-sandbox strategy mode requires strategy.python_module to resolve to a .py file"
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
        if self.artifact_sha256 and actual_artifact_sha256.lower() != self.artifact_sha256.lower():
            raise ValueError(
                "strategy source changed after host verification; refusing llm-sandbox upload"
            )
        self.account_id = os.environ.get("QX_STRATEGY_ACCOUNT_ID")
        self.venue_id = os.environ.get("QX_STRATEGY_VENUE_ID")
        self.workspace_tmpfs_bytes = min(
            4_096_000_000,
            max(256_000_000, (self.expected_memory_mb * 1_000_000) // 2),
        )
        with zipfile.ZipFile(io.BytesIO(self.bundle)) as bundle_archive:
            expanded_bundle_bytes = sum(
                member.file_size for member in bundle_archive.infolist()
            )
        if expanded_bundle_bytes > self.workspace_tmpfs_bytes:
            raise ValueError(
                "llm-sandbox strategy bundle exceeds the isolated workspace memory limit"
            )
        self.runtime_configs = self._runtime_configs()
        self._docker_client = _remote_docker_client()
        try:
            from llm_sandbox import InteractiveSandboxSession
        except ImportError as error:
            self._docker_client.close()
            raise RuntimeError(
                "llm-sandbox strategy mode needs `pip install 'qianxing[sandbox-llm]'`"
            ) from error

        self._session_type = InteractiveSandboxSession
        self._audit = SandboxExecutionAudit.from_environment(
            provider="llm-sandbox",
            job_id=self.job_id,
            package_version=self.package_version,
            entry_sha256=actual_artifact_sha256,
            bundle_sha256=self.manifest["strategy_transfer_sha256"],
            spec_sha256=self.manifest["input_spec_sha256"],
            resource_limits={
                "cpu_count": self.expected_cpu_count,
                "memory_bytes": self.expected_memory_mb * 1_000_000,
                "request_timeout_ms": self.timeout_ms,
                "session_ttl_seconds": self.session_ttl_seconds,
                "max_request_bytes": _MAX_REMOTE_LINE_BYTES,
                "max_response_bytes": _MAX_REMOTE_LINE_BYTES,
            },
            network_policy="deny_all",
            runtime_ref=self.image,
            backend="remote-docker-tls",
        )
        self.session: Any = None
        self._closed = False
        self._session_started_at = 0.0
        self._processed_requests = 0
        self._session_id: str | None = None
        self._client_closed = False

    def _runtime_configs(self) -> dict[str, Any]:
        memory_bytes = self.expected_memory_mb * 1_000_000
        nano_cpus = self.expected_cpu_count * 1_000_000_000
        return {
            "network_mode": "none",
            "mem_limit": memory_bytes,
            "memswap_limit": memory_bytes,
            "nano_cpus": nano_cpus,
            "pids_limit": 128,
            "cap_drop": ["ALL"],
            "security_opt": ["no-new-privileges:true"],
            "privileged": False,
            "read_only": True,
            "user": "65532:65532",
            "tmpfs": {
                "/tmp": "rw,nosuid,nodev,noexec,size=134217728,uid=65532,gid=65532,mode=1777",
                "/workspace": (
                    "rw,nosuid,nodev,size="
                    f"{self.workspace_tmpfs_bytes},uid=65532,gid=65532,mode=0755"
                ),
            },
            "restart_policy": {"Name": "no"},
            "environment": {
                "HOME": "/tmp",
                "PIP_NO_INDEX": "1",
                "PYTHONUNBUFFERED": "1",
            },
            "shm_size": 67_108_864,
        }

    def _verify_runtime(self, session: Any) -> None:
        backend_session = getattr(session, "_backend_session", None)
        container = getattr(backend_session, "container", None)
        if container is None:
            raise RuntimeError("llm-sandbox did not expose the created Docker container for verification")
        reload_container = getattr(container, "reload", None)
        if callable(reload_container):
            reload_container()
        attrs = getattr(container, "attrs", None)
        if not isinstance(attrs, dict):
            raise TypeError("llm-sandbox Docker container inspection is unavailable")
        container_config = attrs.get("Config")
        docker_image = getattr(backend_session, "docker_image", None)
        image_id = getattr(docker_image, "id", None)
        image_attrs = getattr(docker_image, "attrs", None)
        repo_digests = image_attrs.get("RepoDigests") if isinstance(image_attrs, dict) else None
        backend_config = getattr(backend_session, "config", None)
        if (
            not isinstance(container_config, dict)
            or getattr(backend_config, "image", None) != self.image
            or not image_id
            or not isinstance(repo_digests, list)
            or self.image not in repo_digests
            or container_config.get("Image") not in {self.image, image_id}
            or attrs.get("Image") != image_id
        ):
            raise RuntimeError("llm-sandbox container image does not match the pinned digest reference")
        if container_config.get("User") != "65532:65532":
            raise RuntimeError("llm-sandbox strategy container must run as the unprivileged runtime user")
        host_config = attrs.get("HostConfig")
        if not isinstance(host_config, dict):
            raise TypeError("llm-sandbox Docker HostConfig is missing")
        expected = {
            "NetworkMode": "none",
            "Memory": self.expected_memory_mb * 1_000_000,
            "MemorySwap": self.expected_memory_mb * 1_000_000,
            "NanoCpus": self.expected_cpu_count * 1_000_000_000,
            "PidsLimit": 128,
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges:true"],
            "Privileged": False,
            "ReadonlyRootfs": True,
            "ShmSize": 67_108_864,
        }
        actual = {key: host_config.get(key) for key in expected}
        if actual != expected or (host_config.get("CapAdd") or []) != []:
            raise RuntimeError(
                "llm-sandbox Docker isolation settings do not match host policy: "
                + json.dumps(
                    {
                        "expected": expected,
                        "actual": actual,
                        "cap_add": host_config.get("CapAdd") or [],
                    },
                    sort_keys=True,
                )
            )
        expected_tmpfs = self.runtime_configs["tmpfs"]
        actual_tmpfs = host_config.get("Tmpfs")
        if not isinstance(actual_tmpfs, dict) or set(actual_tmpfs) != set(expected_tmpfs):
            raise RuntimeError("llm-sandbox Docker tmpfs mounts do not match host policy")
        for path, expected_options in expected_tmpfs.items():
            if set(str(actual_tmpfs[path]).split(",")) != set(expected_options.split(",")):
                raise RuntimeError(
                    f"llm-sandbox Docker tmpfs mount {path} does not match host policy"
                )

    def _start_session(self) -> None:
        session_id = str(uuid.uuid4())
        self._session_id = session_id
        self._audit.emit(session_id=session_id, state="create_requested")
        session = None
        try:
            session = self._session_type(
                backend="docker",
                client=self._docker_client,
                image=self.image,
                lang="python",
                kernel_type="ipython",
                max_memory=f"{self.expected_memory_mb}m",
                history_size=0,
                timeout=self.timeout_ms / 1000,
                session_timeout=float(self.session_ttl_seconds),
                execution_timeout=float(self.timeout_ms / 1000),
                runtime_configs=self.runtime_configs,
                workdir="/workspace",
                keep_template=True,
                commit_container=False,
            )
            # Keep uploaded bootstrap and interactive command files owned by the
            # same unprivileged UID that runs the persistent kernel.
            def copy_to_runtime(source: str, destination: str) -> None:
                _copy_to_unprivileged_guest(session, source, destination)

            session.copy_to_runtime = copy_to_runtime
            session.open()
            self.session = session
            self._session_started_at = time.monotonic()
            self._audit.emit(session_id=session_id, state="created")
            self._verify_runtime(session)
            mkdir_result = session.execute_command("mkdir -p /workspace")
            if getattr(mkdir_result, "exit_code", 0) != 0:
                raise RuntimeError("llm-sandbox could not prepare the strategy workspace")
            with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as bundle_file:
                bundle_file.write(self.bundle)
                bundle_path = Path(bundle_file.name)
            try:
                # The custom tar copier sets guest ownership/readability without
                # widening permissions on this host-side temporary bundle.
                session.copy_to_runtime(str(bundle_path), "/workspace/qianxing-strategy-input.zip")
            finally:
                bundle_path.unlink(missing_ok=True)

            setup = "\n\n".join(
                [
                    "import importlib.metadata as _qx_importlib_metadata",
                    "def _qx_importlib_metadata_version(): return _qx_importlib_metadata.version('qianxing')",
                    _extract_bundle_script(),
                    _input_preflight_script(self.package_version),
                    _guest_environment_setup(
                        entrypoint=self.manifest["strategy_entrypoint"],
                        spec=self.spec,
                        has_spec=self.has_spec,
                        account_id=self.account_id,
                        venue_id=self.venue_id,
                        artifact_sha256=self.artifact_sha256 or None,
                        package_version=self.package_version,
                    ),
                    "class _QxBoundedSink:\n"
                    "    def __init__(self): self.written = 0\n"
                    "    def write(self, value):\n"
                    "        size = len(str(value).encode('utf-8', errors='replace'))\n"
                    "        self.written = min(" + str(_MAX_REMOTE_LOG_BYTES) + ", self.written + size)\n"
                    "        return len(str(value))\n"
                    "    def flush(self): return None\n",
                ]
            )
            result = session.run(setup, timeout=self.timeout_ms / 1000)
            if getattr(result, "exit_code", None) != 0:
                stderr = str(getattr(result, "stderr", ""))[-_MAX_REMOTE_LOG_BYTES:]
                raise RuntimeError(
                    "llm-sandbox strategy image or bootstrap check failed; the image must "
                    f"contain Qianxing {self.package_version} and IPython: {stderr}"
                )
            if self.session is not session:
                raise RuntimeError("llm-sandbox session changed during startup")
            self._audit.emit(session_id=session_id, state="ready")
        except Exception as error:
            self._audit.emit_best_effort(
                session_id=session_id,
                state="create_failed",
                failure_code=audit_failure_code(error),
            )
            if session is not None:
                try:
                    session.close()
                except Exception as cleanup_error:  # noqa: BLE001 - preserve the startup error.
                    _log_cleanup_failure("session cleanup failed after startup error", cleanup_error)
            self.session = None
            self._session_id = None
            raise

    def request(self, payload: str) -> str:
        if self._closed:
            raise RuntimeError("llm-sandbox strategy worker is closed")
        if len(payload.encode("utf-8")) > _MAX_REMOTE_LINE_BYTES:
            raise ValueError("llm-sandbox strategy request exceeded the size limit")
        if self.session is None:
            self._start_session()
        elif self._session_needs_rotation():
            if self._processed_requests and not self._request_contains_checkpoint(payload):
                raise RuntimeError(
                    "llm-sandbox session lease is near expiry and the request has no persisted "
                    "strategy checkpoint; refusing a state-losing restart"
                )
            self._stop_session()
            try:
                self._start_session()
            except Exception:
                self._closed = True
                raise
        if not self._has_request_budget():
            raise TimeoutError(
                "llm-sandbox session does not have enough time for the configured strategy request"
            )

        strategy_id, strategy_version = request_identity(payload)
        request_sequence = self._processed_requests + 1
        marker = _RESPONSE_PREFIX + uuid.uuid4().hex + ":"
        try:
            result = self.session.run(
                _response_code(payload, marker), timeout=self.timeout_ms / 1000
            )
            if getattr(result, "exit_code", None) != 0:
                raise RuntimeError(
                    "llm-sandbox strategy cell failed: "
                    + str(getattr(result, "stderr", ""))[-_MAX_REMOTE_LOG_BYTES:]
                )
            stdout = getattr(result, "stdout", "")
            if not isinstance(stdout, str) or len(stdout.encode("utf-8")) > _MAX_REMOTE_LINE_BYTES:
                raise RuntimeError("llm-sandbox strategy output exceeded the size limit")
            matches = [line[len(marker) :] for line in stdout.splitlines() if line.startswith(marker)]
            if len(matches) != 1:
                raise RuntimeError("llm-sandbox strategy output did not contain exactly one response")
            response = matches[0]
            if len(response.encode("utf-8")) > _MAX_REMOTE_LINE_BYTES:
                raise RuntimeError("llm-sandbox strategy response exceeded the size limit")
        except Exception as error:
            if self._session_id is not None:
                self._audit.emit_best_effort(
                    session_id=self._session_id,
                    state="request_failed",
                    failure_code=audit_failure_code(error),
                    request_sequence=request_sequence,
                    strategy_id=strategy_id,
                    strategy_version=strategy_version,
                    data_fingerprint=request_data_fingerprint(payload),
                )
            self._stop_session()
            self._closed = True
            raise RuntimeError(
                "llm-sandbox strategy request failed; refusing local fallback: "
                + _redacted_error(error)
            ) from error

        if self._session_id is not None:
            self._audit.emit(
                session_id=self._session_id,
                state="request_completed",
                request_sequence=request_sequence,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                data_fingerprint=request_data_fingerprint(payload),
                response_sha256=hashlib.sha256(response.encode("utf-8")).hexdigest(),
            )
        self._processed_requests += 1
        return response

    def _session_needs_rotation(self) -> bool:
        renew_before = min(60, max(1, self.session_ttl_seconds // 4))
        return time.monotonic() - self._session_started_at >= self.session_ttl_seconds - renew_before

    def _has_request_budget(self) -> bool:
        remaining = self.session_ttl_seconds - (time.monotonic() - self._session_started_at)
        return remaining >= self.timeout_ms / 1000 + 5

    @staticmethod
    def _request_contains_checkpoint(payload: str) -> bool:
        try:
            value = json.loads(payload)
        except json.JSONDecodeError:
            return False
        return isinstance(value, dict) and isinstance(value.get("strategy_checkpoint"), dict)

    def _stop_session(self) -> None:
        session, self.session = self.session, None
        session_id, self._session_id = self._session_id, None
        if session is None:
            return
        if session_id is not None:
            self._audit.emit_best_effort(session_id=session_id, state="destroy_requested")
        try:
            close_code = (
                "import contextlib as _qx_contextlib, io as _qx_io\n"
                "with _qx_contextlib.redirect_stdout(_qx_io.StringIO()), _qx_contextlib.redirect_stderr(_qx_io.StringIO()):\n"
                "    _qx_close_handler(_qx_handler)"
            )
            session.run(close_code, timeout=5)
        except Exception as error:  # noqa: BLE001 - shutdown is best effort.
            _log_cleanup_failure("strategy on_stop callback failed", error)
        try:
            session.close()
            if session_id is not None:
                self._audit.emit_best_effort(session_id=session_id, state="destroyed")
        except Exception as error:  # noqa: BLE001 - best-effort container cleanup.
            if session_id is not None:
                self._audit.emit_best_effort(
                    session_id=session_id,
                    state="destroy_failed",
                    failure_code=audit_failure_code(error),
                )
            _log_cleanup_failure("container cleanup failed", error)

    def close(self) -> None:
        if not self._closed:
            self._closed = True
            self._stop_session()
        if not self._client_closed:
            self._client_closed = True
            try:
                self._docker_client.close()
            except Exception as error:  # noqa: BLE001 - close must remain idempotent.
                _log_cleanup_failure("Docker client close failed", error)


def _serve_jsonl(worker: LlmSandboxPythonStrategyWorker) -> None:
    for line in sys.stdin:
        if line.strip():
            sys.stdout.write(worker.request(line) + "\n")
            sys.stdout.flush()


def _serve_framed(worker: LlmSandboxPythonStrategyWorker) -> None:
    stream = sys.stdin.buffer
    output = sys.stdout.buffer
    while True:
        frame = read_frame(stream)
        if frame is None:
            return
        kind, sequence, payload = frame
        if kind != REQUEST:
            raise ValueError(f"llm-sandbox strategy bridge expected request frame, got kind={kind}")
        response = worker.request(payload.decode("utf-8"))
        output.write(encode_frame(RESPONSE, sequence, response.encode("utf-8")))
        output.flush()


def main() -> int:
    parser = argparse.ArgumentParser(description="Qianxing llm-sandbox strategy bridge")
    parser.add_argument("--module", required=True, help="local .py strategy file")
    parser.add_argument("--protocol", choices=("jsonl", "framed_json"), default="jsonl")
    args = parser.parse_args()
    worker: LlmSandboxPythonStrategyWorker | None = None
    try:
        worker = LlmSandboxPythonStrategyWorker(args.module)
        if args.protocol == "framed_json":
            _serve_framed(worker)
        else:
            _serve_jsonl(worker)
        return 0
    except Exception as error:  # noqa: BLE001 - bridge errors cross the process boundary.
        sys.stderr.write(f"[llm-sandbox Python Strategy] {_redacted_error(error)}\n")
        sys.stderr.flush()
        return 1
    finally:
        if worker is not None:
            worker.close()


if __name__ == "__main__":
    raise SystemExit(main())
