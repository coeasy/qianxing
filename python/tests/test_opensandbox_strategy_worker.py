from __future__ import annotations

import json
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any

import pytest

from qianxing_strategy import opensandbox_worker


class _Value:
    def __init__(self, **values: Any) -> None:
        self.values = values
        for key, value in values.items():
            setattr(self, key, value)


class _WriteEntry(_Value):
    pass


class _FakeFiles:
    def __init__(self, *, read_error: Exception | None = None) -> None:
        self.entries: list[_WriteEntry] = []
        self.deleted: list[str] = []
        self.read_error = read_error

    def write_files(self, entries: list[_WriteEntry]) -> None:
        self.entries.extend(entries)

    def read_bytes(self, path: str) -> bytes:
        if self.read_error is not None:
            raise self.read_error
        return b'{"decision":"accepted"}'

    def delete_files(self, paths: list[str]) -> None:
        self.deleted.extend(paths)


class _FakeCommands:
    def __init__(self) -> None:
        self.commands: list[tuple[str, Any]] = []

    def run(self, command: str, *, opts: Any) -> SimpleNamespace:
        self.commands.append((command, opts))
        if "qianxing_strategy.opensandbox_guest" in command:
            return SimpleNamespace(id="guest-execution-1", exit_code=0)
        return SimpleNamespace(exit_code=0, logs=SimpleNamespace(stderr=[]))

    def get_command_status(self, execution_id: str) -> SimpleNamespace:
        assert execution_id == "guest-execution-1"
        return SimpleNamespace(running=True)


class _FakeSandbox:
    instances: list[_FakeSandbox] = []
    read_error: Exception | None = None

    @classmethod
    def create(cls, image: str, **kwargs: Any) -> _FakeSandbox:
        instance = cls(image, kwargs)
        cls.instances.append(instance)
        return instance

    def __init__(self, image: str, create_options: dict[str, Any]) -> None:
        self.image = image
        self.create_options = create_options
        self.files = _FakeFiles(read_error=type(self).read_error)
        self.commands = _FakeCommands()
        self.destroyed = False

    def destroy(self) -> None:
        self.destroyed = True


def _install_fake_sdk(monkeypatch: pytest.MonkeyPatch) -> None:
    root = ModuleType("opensandbox")
    root.SandboxSync = _FakeSandbox
    config = ModuleType("opensandbox.config")
    config.ConnectionConfigSync = _Value
    execd = ModuleType("opensandbox.models.execd")
    execd.RunCommandOpts = _Value
    filesystem = ModuleType("opensandbox.models.filesystem")
    filesystem.WriteEntry = _WriteEntry
    sandboxes = ModuleType("opensandbox.models.sandboxes")
    sandboxes.NetworkPolicy = _Value
    for name, module in (
        ("opensandbox", root),
        ("opensandbox.config", config),
        ("opensandbox.models", ModuleType("opensandbox.models")),
        ("opensandbox.models.execd", execd),
        ("opensandbox.models.filesystem", filesystem),
        ("opensandbox.models.sandboxes", sandboxes),
    ):
        monkeypatch.setitem(sys.modules, name, module)
    monkeypatch.setattr(opensandbox_worker.importlib.metadata, "version", lambda _: "0.2.2")
    monkeypatch.setenv("OPEN_SANDBOX_DOMAIN", "sandbox.example.test:8080")
    monkeypatch.setenv("OPEN_SANDBOX_API_KEY", "host-only-secret")
    monkeypatch.setenv(
        "QX_OPENSANDBOX_IMAGE", "qianxing-runtime@sha256:" + "a" * 64
    )
    monkeypatch.setenv("QX_STRATEGY_PYTHON_TIMEOUT_MS", "15000")
    monkeypatch.setenv("QX_OPENSANDBOX_SESSION_TTL_SECONDS", "3600")
    monkeypatch.setenv("QX_OPENSANDBOX_CPUS", "2")
    monkeypatch.setenv("QX_OPENSANDBOX_MEMORY_MIB", "3072")
    _FakeSandbox.instances.clear()
    _FakeSandbox.read_error = None


def test_opensandbox_strategy_worker_uses_deny_all_and_graceful_close(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch)
    audit_path = tmp_path / "audit" / "opensandbox.jsonl"
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_PATH", str(audit_path))
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED", "true")
    monkeypatch.setenv("QX_STRATEGY_HOST_WORKER_ID", "runtime-worker-osb")
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")

    worker = opensandbox_worker.OpenSandboxPythonStrategyWorker(str(strategy))
    response = worker.request('{"event":"bar"}')

    assert response == '{"decision":"accepted"}'
    sandbox = _FakeSandbox.instances[0]
    assert sandbox.create_options["network_policy"].values == {
        "defaultAction": "deny",
        "egress": [],
    }
    assert sandbox.create_options["resource"] == {
        "cpu": "2",
        "memory": "3072Mi",
    }
    assert sandbox.create_options["metadata"]["qianxing_resource_cpu"] == "2"
    assert sandbox.create_options["metadata"]["qianxing_resource_memory_mib"] == "3072"
    guest_command = next(
        command
        for command, _ in sandbox.commands.commands
        if "qianxing_strategy.opensandbox_guest" in command
    )
    assert "host-only-secret" not in guest_command
    assert str(audit_path) not in guest_command
    assert "QX_STRATEGY_HOST_WORKER_ID" not in guest_command
    assert "env -i" in guest_command
    assert sandbox.files.deleted

    worker.close()
    assert sandbox.destroyed
    assert any(
        b'"close":true' in entry.data
        for entry in sandbox.files.entries
    )
    records = [json.loads(line) for line in audit_path.read_text(encoding="utf-8").splitlines()]
    assert [record["lifecycle_state"] for record in records] == [
        "create_requested", "created", "ready", "request_completed", "destroy_requested", "destroyed"
    ]
    assert all(record["provider"] == "opensandbox" for record in records)
    assert all(record["host_worker_id"] == "runtime-worker-osb" for record in records)
    assert "host-only-secret" not in audit_path.read_text(encoding="utf-8")


def test_opensandbox_module_function_checkpoint_allows_session_rotation(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch)
    monkeypatch.delenv("QX_STRATEGY_SPEC_JSON", raising=False)
    clock = {"now": 100.0}
    monkeypatch.setattr(opensandbox_worker.time, "monotonic", lambda: clock["now"])
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")
    worker = opensandbox_worker.OpenSandboxPythonStrategyWorker(str(strategy))
    bootstrap = json.dumps(
        {
            "strategy_checkpoint": {
                "schema_version": 1,
                "strategy_id": "s",
                "strategy_version": "v1",
                "adapter_state": {"schema_version": 1, "state_mode": "bootstrap"},
                "strategy_state": {},
            }
        },
        separators=(",", ":"),
    )
    resumed = json.dumps(
        {
            "strategy_checkpoint": {
                "schema_version": 1,
                "strategy_id": "s",
                "strategy_version": "v1",
                "adapter_state": {"schema_version": 1, "state_mode": "module-function"},
                "strategy_state": {"counter": 1},
            }
        },
        separators=(",", ":"),
    )

    worker.request(bootstrap)
    first = _FakeSandbox.instances[0]
    clock["now"] = 3700.0
    worker.request(resumed)

    assert len(_FakeSandbox.instances) == 2
    assert first.destroyed
    assert any(b'"close":true' in entry.data for entry in first.files.entries)
    second = _FakeSandbox.instances[1]
    assert any(
        "/requests/" in entry.path
        and json.loads(entry.data.decode("utf-8")).get("payload") == resumed
        for entry in second.files.entries
    )
    worker.close()


def test_opensandbox_module_function_refuses_rotation_without_checkpoint(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch)
    monkeypatch.delenv("QX_STRATEGY_SPEC_JSON", raising=False)
    clock = {"now": 100.0}
    monkeypatch.setattr(opensandbox_worker.time, "monotonic", lambda: clock["now"])
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")
    worker = opensandbox_worker.OpenSandboxPythonStrategyWorker(str(strategy))
    worker.request('{"event":1}')
    clock["now"] = 3700.0

    with pytest.raises(RuntimeError, match="no persisted strategy checkpoint"):
        worker.request('{"event":2}')

    assert len(_FakeSandbox.instances) == 1
    worker.close()


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("QX_OPENSANDBOX_CPUS", "0"),
        ("QX_OPENSANDBOX_CPUS", "65"),
        ("QX_OPENSANDBOX_MEMORY_MIB", "511"),
        ("QX_OPENSANDBOX_MEMORY_MIB", "65537"),
    ],
)
def test_opensandbox_worker_rejects_unbounded_resource_policy_before_creation(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    name: str,
    value: str,
) -> None:
    _install_fake_sdk(monkeypatch)
    monkeypatch.setenv(name, value)
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")

    with pytest.raises(ValueError, match=name):
        opensandbox_worker.OpenSandboxPythonStrategyWorker(str(strategy))

    assert _FakeSandbox.instances == []


def test_opensandbox_worker_rejects_mutable_image_tags_before_creation(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch)
    monkeypatch.setenv("QX_OPENSANDBOX_IMAGE", "qianxing-runtime:latest")
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")

    with pytest.raises(ValueError, match="OCI SHA-256 image reference"):
        opensandbox_worker.OpenSandboxPythonStrategyWorker(str(strategy))

    assert _FakeSandbox.instances == []


def test_opensandbox_strategy_worker_fails_closed_and_destroys_sandbox_on_request_error(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _install_fake_sdk(monkeypatch)
    _FakeSandbox.read_error = OSError("remote response unavailable")
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request):\n    return {}\n", encoding="utf-8")

    worker = opensandbox_worker.OpenSandboxPythonStrategyWorker(str(strategy))
    with pytest.raises(RuntimeError, match="without local fallback"):
        worker.request('{"event":"bar"}')

    assert _FakeSandbox.instances[0].destroyed
