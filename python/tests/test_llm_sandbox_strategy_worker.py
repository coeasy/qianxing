from __future__ import annotations

import ast
import io
import json
import re
import sys
import tarfile
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any, ClassVar

import pytest
from qianxing_strategy import llm_sandbox_worker


class _FakeDockerClient:
    instances: ClassVar[list[_FakeDockerClient]] = []

    def __init__(self, *, base_url: str, tls: Any, timeout: int) -> None:
        self.base_url = base_url
        self.tls = tls
        self.timeout = timeout
        self.closed = False
        type(self).instances.append(self)

    def close(self) -> None:
        self.closed = True


class _FakeInteractiveSession:
    instances: ClassVar[list[_FakeInteractiveSession]] = []
    fail_runtime = False
    omit_response_marker = False

    def __init__(self, **kwargs: Any) -> None:
        self.kwargs = kwargs
        config = kwargs["runtime_configs"]
        host_config = {
            "NetworkMode": config["network_mode"],
            "Memory": config["mem_limit"],
            "MemorySwap": config["memswap_limit"],
            "NanoCpus": config["nano_cpus"],
            "PidsLimit": config["pids_limit"],
            "CapDrop": config["cap_drop"],
            "SecurityOpt": config["security_opt"],
            "Privileged": config["privileged"],
            "ReadonlyRootfs": config["read_only"],
            "Tmpfs": config["tmpfs"],
            "ShmSize": config["shm_size"],
        }
        if type(self).fail_runtime:
            host_config["NetworkMode"] = "bridge"
        image_id = "sha256:" + "b" * 64
        docker_image = SimpleNamespace(
            id=image_id,
            attrs={"RepoDigests": [kwargs["image"]]},
        )
        container = SimpleNamespace(
            attrs={
                "Config": {"Image": image_id, "User": config["user"]},
                "Image": image_id,
                "HostConfig": host_config,
            }
        )
        self._backend_session = SimpleNamespace(
            container=container,
            docker_image=docker_image,
            config=SimpleNamespace(image=kwargs["image"]),
        )
        self.opened = False
        self.closed = False
        self.run_calls: list[tuple[str, float | None]] = []
        self.copied: list[tuple[str, str]] = []
        self.uploaded_archives: list[tuple[str, bytes]] = []
        self.commands: list[str] = []
        type(self).instances.append(self)
        container.put_archive = self._put_archive

    def open(self) -> None:
        self.opened = True

    def execute_command(self, command: str) -> SimpleNamespace:
        self.commands.append(command)
        return SimpleNamespace(exit_code=0)

    def copy_to_runtime(self, source: str, destination: str) -> None:
        self.copied.append((source, destination))

    def _put_archive(self, destination: str, payload: bytes) -> bool:
        self.uploaded_archives.append((destination, payload))
        return True

    def run(self, code: str, *, timeout: float | None = None) -> SimpleNamespace:
        self.run_calls.append((code, timeout))
        if "_qx_response = _qx_handle_line" not in code:
            return SimpleNamespace(exit_code=0, stdout="", stderr="")
        if type(self).omit_response_marker:
            return SimpleNamespace(exit_code=0, stdout="no response", stderr="")
        match = re.search(r"print\((.+?) \+ _qx_response\)", code)
        assert match is not None
        marker = ast.literal_eval(match.group(1))
        return SimpleNamespace(
            exit_code=0,
            stdout=marker + '{"ok":true,"output":{}}\n',
            stderr="",
        )

    def close(self) -> None:
        self.closed = True


def _install_fake_sdk(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    class _Tls:
        @staticmethod
        def TLSConfig(**kwargs: Any) -> dict[str, Any]:
            return kwargs

    docker = ModuleType("docker")
    docker.tls = _Tls
    docker.DockerClient = _FakeDockerClient
    llm_sandbox = ModuleType("llm_sandbox")
    llm_sandbox.InteractiveSandboxSession = _FakeInteractiveSession
    monkeypatch.setitem(sys.modules, "docker", docker)
    monkeypatch.setitem(sys.modules, "llm_sandbox", llm_sandbox)
    monkeypatch.setattr(llm_sandbox_worker.importlib.metadata, "version", lambda _: "0.2.2")

    for key, value in {
        "QX_LLMSANDBOX_IMAGE": "qianxing-runtime@sha256:" + "a" * 64,
        "QX_LLMSANDBOX_DOCKER_HOST": "tcp://docker.example.test:2376",
        "QX_LLMSANDBOX_SESSION_TTL_SECONDS": "3600",
        "QX_LLMSANDBOX_EXPECTED_CPU_COUNT": "2",
        "QX_LLMSANDBOX_EXPECTED_MEMORY_MB": "2048",
        "QX_STRATEGY_PYTHON_TIMEOUT_MS": "15000",
    }.items():
        monkeypatch.setenv(key, value)
    tls_files = {
        "QX_LLMSANDBOX_DOCKER_CA_CERT": "ca.pem",
        "QX_LLMSANDBOX_DOCKER_CLIENT_CERT": "client.pem",
        "QX_LLMSANDBOX_DOCKER_CLIENT_KEY": "client.key",
    }
    for env_name, filename in tls_files.items():
        path = tmp_path / filename
        path.write_text("test TLS fixture", encoding="utf-8")
        monkeypatch.setenv(env_name, str(path))
    _FakeDockerClient.instances.clear()
    _FakeInteractiveSession.instances.clear()
    _FakeInteractiveSession.fail_runtime = False
    _FakeInteractiveSession.omit_response_marker = False


def _strategy_file(tmp_path: Path) -> Path:
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")
    return strategy


def test_llm_sandbox_uses_tls_remote_docker_and_verifies_hardening(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch, tmp_path)
    audit_path = tmp_path / "audit" / "llm-sandbox.jsonl"
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_PATH", str(audit_path))
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED", "true")
    monkeypatch.setenv("QX_STRATEGY_HOST_WORKER_ID", "runtime-worker-llm")

    worker = llm_sandbox_worker.LlmSandboxPythonStrategyWorker(
        str(_strategy_file(tmp_path))
    )
    response = worker.request(
        json.dumps(
            {
                "strategy_id": "s",
                "strategy_version": "1.0",
                "data_fingerprint": "data-v1",
            }
        )
    )

    assert response == '{"ok":true,"output":{}}'
    docker_client = _FakeDockerClient.instances[0]
    assert docker_client.base_url == "tcp://docker.example.test:2376"
    assert docker_client.tls["verify"] is True
    session = _FakeInteractiveSession.instances[0]
    config = session.kwargs["runtime_configs"]
    assert config["network_mode"] == "none"
    assert config["nano_cpus"] == 2_000_000_000
    assert config["mem_limit"] == 2_048_000_000
    assert config["pids_limit"] == 128
    assert config["cap_drop"] == ["ALL"]
    assert "cap_add" not in config
    assert config["read_only"] is True
    assert config["user"] == "65532:65532"
    assert config["tmpfs"]["/workspace"].startswith("rw,nosuid,nodev,size=")
    assert config["shm_size"] == 67_108_864
    assert config["security_opt"] == ["no-new-privileges:true"]
    assert config["environment"] == {
        "HOME": "/tmp",
        "PIP_NO_INDEX": "1",
        "PYTHONUNBUFFERED": "1",
    }
    assert session.kwargs["history_size"] == 0
    assert session.uploaded_archives[0][0] == "/workspace"
    with tarfile.open(fileobj=io.BytesIO(session.uploaded_archives[0][1])) as archive:
        member = archive.getmember("qianxing-strategy-input.zip")
        assert member.uid == 65_532
        assert member.gid == 65_532
        assert member.mode == 0o644
    assert all("DOCKER" not in key for key in config["environment"])

    worker.close()
    assert session.closed and docker_client.closed
    audit = audit_path.read_text(encoding="utf-8")
    records = [json.loads(line) for line in audit.splitlines()]
    assert [row["lifecycle_state"] for row in records] == [
        "create_requested",
        "created",
        "ready",
        "request_completed",
        "destroy_requested",
        "destroyed",
    ]
    assert all(row["provider"] == "llm-sandbox" for row in records)


def test_llm_sandbox_rejects_runtime_that_does_not_apply_deny_all_network(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch, tmp_path)
    _FakeInteractiveSession.fail_runtime = True
    worker = llm_sandbox_worker.LlmSandboxPythonStrategyWorker(
        str(_strategy_file(tmp_path))
    )

    with pytest.raises(RuntimeError, match="isolation settings do not match"):
        worker.request('{"strategy_id":"s","strategy_version":"1"}')

    assert _FakeInteractiveSession.instances[0].closed
    assert not _FakeInteractiveSession.instances[0].run_calls
    worker.close()
    assert _FakeDockerClient.instances[0].closed


def test_llm_sandbox_rotation_requires_and_uses_persisted_checkpoint(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch, tmp_path)
    clock = {"now": 100.0}
    monkeypatch.setattr(llm_sandbox_worker.time, "monotonic", lambda: clock["now"])
    worker = llm_sandbox_worker.LlmSandboxPythonStrategyWorker(
        str(_strategy_file(tmp_path))
    )
    first = '{"strategy_id":"s","strategy_version":"1"}'
    worker.request(first)
    clock["now"] += 3_600

    with pytest.raises(RuntimeError, match="no persisted strategy checkpoint"):
        worker.request(first)
    assert len(_FakeInteractiveSession.instances) == 1

    resumed = json.dumps(
        {
            "strategy_id": "s",
            "strategy_version": "1",
            "strategy_checkpoint": {"schema_version": 1},
        }
    )
    worker.request(resumed)
    assert len(_FakeInteractiveSession.instances) == 2
    assert _FakeInteractiveSession.instances[0].closed
    worker.close()


def test_llm_sandbox_fails_closed_on_missing_response_and_no_local_fallback(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch, tmp_path)
    _FakeInteractiveSession.omit_response_marker = True
    worker = llm_sandbox_worker.LlmSandboxPythonStrategyWorker(
        str(_strategy_file(tmp_path))
    )

    with pytest.raises(RuntimeError, match="refusing local fallback"):
        worker.request('{"strategy_id":"s","strategy_version":"1"}')

    assert _FakeInteractiveSession.instances[0].closed
    worker.close()
    assert _FakeDockerClient.instances[0].closed


@pytest.mark.parametrize(
    "docker_host",
    [
        "tcp://user:password@docker.example.test:2376",
        "http://docker.example.test:2375",
        "tcp://docker.example.test:2376?token=secret",
    ],
)
def test_llm_sandbox_rejects_uncredentialed_or_plain_remote_docker_endpoints(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, docker_host: str
) -> None:
    _install_fake_sdk(monkeypatch, tmp_path)
    monkeypatch.setenv("QX_LLMSANDBOX_DOCKER_HOST", docker_host)

    with pytest.raises(ValueError, match="credential-free tcp://host:port"):
        llm_sandbox_worker.LlmSandboxPythonStrategyWorker(
            str(_strategy_file(tmp_path))
        )
