from __future__ import annotations

import asyncio
import importlib.metadata
import json
import sys
import types
from pathlib import Path

import pytest

from qianxing_strategy import microsandbox_worker


class _EventType:
    STDOUT = "stdout"
    STDERR = "stderr"
    EXITED = "exited"
    FAILED = "failed"
    STDIN_ERROR = "stdin_error"


class _BackendKind:
    LOCAL = "local"
    CLOUD = "cloud"


class _Network:
    @staticmethod
    def none() -> str:
        return "deny-all"


class _Stdin:
    @staticmethod
    def pipe() -> str:
        return "pipe"


class _FakeScope:
    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False


class _FakeSink:
    def __init__(self, handle: _FakeHandle) -> None:
        self.handle = handle
        self.closed = False
        self.writes: list[bytes] = []

    async def write(self, value: bytes) -> None:
        self.writes.append(value)
        if self.handle.respond:
            await self.handle.events.put(
                types.SimpleNamespace(event_type=_EventType.STDOUT, data=b'{"ok":true}\n')
            )
        else:
            await self.handle.events.put(
                types.SimpleNamespace(event_type=_EventType.FAILED, data=b"worker failed")
            )

    async def close(self) -> None:
        self.closed = True
        if not self.handle.ended:
            await self.handle.events.put(
                types.SimpleNamespace(event_type=_EventType.STDERR, data=b"on_stop\n")
            )
            await self.handle.events.put(
                types.SimpleNamespace(event_type=_EventType.EXITED, code=0, data=None)
            )
            self.handle.ended = True


class _FakeHandle:
    def __init__(self, *, respond: bool) -> None:
        self.respond = respond
        self.events: asyncio.Queue = asyncio.Queue()
        self.stdin = _FakeSink(self)
        self.killed = False
        self.ended = False

    def take_stdin(self):
        return self.stdin

    def __aiter__(self):
        return self

    async def __anext__(self):
        event = await self.events.get()
        if event is None:
            raise StopAsyncIteration
        return event

    async def kill(self) -> None:
        self.killed = True
        if not self.ended:
            await self.events.put(
                types.SimpleNamespace(event_type=_EventType.EXITED, code=-9, data=None)
            )
            self.ended = True


class _FakeSandbox:
    def __init__(self, *, respond: bool, create_options: dict) -> None:
        self.respond = respond
        self.create_options = create_options
        self.exec_calls: list[tuple[str, list[str], dict]] = []
        self.stream_calls: list[tuple[str, list[str], dict]] = []
        self.file_writes: list[tuple[str, bytes]] = []
        self.handle: _FakeHandle | None = None
        self.destroy_calls: list[dict] = []

    @property
    def fs(self):
        async def write(path: str, content: bytes) -> None:
            self.file_writes.append((path, content))

        return types.SimpleNamespace(write=write)

    async def exec(self, command: str, args: list[str], **kwargs):
        self.exec_calls.append((command, args, kwargs))
        return types.SimpleNamespace(success=True, stderr_text="")

    async def exec_stream(self, command: str, args: list[str], **kwargs):
        self.stream_calls.append((command, args, kwargs))
        self.handle = _FakeHandle(respond=self.respond)
        return self.handle

    async def destroy(self, *, force: bool, timeout: float) -> None:
        self.destroy_calls.append({"force": force, "timeout": timeout})


class _FakeSdk:
    def __init__(self, *, respond: bool) -> None:
        self.respond = respond
        self.instances: list[_FakeSandbox] = []
        self.create_calls: list[tuple[str, dict]] = []

    async def create(self, name: str, **kwargs) -> _FakeSandbox:
        self.create_calls.append((name, kwargs))
        sandbox = _FakeSandbox(respond=self.respond, create_options=kwargs)
        self.instances.append(sandbox)
        return sandbox


def _install_fake_sdk(monkeypatch: pytest.MonkeyPatch, *, respond: bool) -> _FakeSdk:
    sdk = _FakeSdk(respond=respond)

    def backend_scope(kind: str, **kwargs):
        return _FakeScope()

    module = types.SimpleNamespace(
        BackendKind=_BackendKind,
        ExecEventType=_EventType,
        Network=_Network,
        Sandbox=sdk,
        Stdin=_Stdin,
        backend_scope=backend_scope,
    )
    monkeypatch.setitem(sys.modules, "microsandbox", module)
    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    return sdk


def _configure(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    respond: bool,
    has_spec: bool = True,
) -> tuple[microsandbox_worker.MicrosandboxPythonStrategyWorker, _FakeSdk]:
    strategy_file = tmp_path / "strategy.py"
    strategy_file.write_text(
        "def on_event(request):\n    return {'intents': []}\n", encoding="utf-8"
    )
    for name, value in {
        "QX_MICROSANDBOX_BACKEND": "local",
        "QX_MICROSANDBOX_IMAGE": "qianxing-test@sha256:" + "b" * 64,
        "QX_MICROSANDBOX_CPUS": "1",
        "QX_MICROSANDBOX_MEMORY_MIB": "512",
        "QX_MICROSANDBOX_SESSION_TTL_SECONDS": "60",
        "QX_STRATEGY_PYTHON_TIMEOUT_MS": "5000",
    }.items():
        monkeypatch.setenv(name, value)
    if has_spec:
        monkeypatch.setenv("QX_STRATEGY_SPEC_JSON", "{}")
    else:
        monkeypatch.delenv("QX_STRATEGY_SPEC_JSON", raising=False)
    sdk = _install_fake_sdk(monkeypatch, respond=respond)
    return microsandbox_worker.MicrosandboxPythonStrategyWorker(str(strategy_file)), sdk


@pytest.mark.asyncio
async def test_microsandbox_worker_uses_network_none_and_gracefully_closes_guest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    audit_path = tmp_path / "audit" / "microsandbox.jsonl"
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_PATH", str(audit_path))
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED", "true")
    monkeypatch.setenv("QX_STRATEGY_HOST_WORKER_ID", "runtime-worker-micro")
    worker, sdk = _configure(tmp_path, monkeypatch, respond=True)
    assert sdk.create_calls == []

    with worker.backend_scope():
        assert await worker._request('{"event":1}') == '{"ok":true}'

    sandbox = sdk.instances[0]
    assert sandbox.create_options["network"] == "deny-all"
    assert sandbox.create_options["cpus"] == 1
    assert sandbox.create_options["memory"] == 512
    command, args, options = sandbox.stream_calls[0]
    assert command == "env"
    assert args[0] == "-i"
    assert "MICROSANDBOX_API_KEY" not in " ".join(args)
    assert str(audit_path) not in " ".join(args)
    assert "QX_STRATEGY_HOST_WORKER_ID" not in " ".join(args)
    assert options["stdin"] == "pipe"
    assert sandbox.handle is not None

    await worker.close()

    assert sandbox.handle.stdin.closed is True
    assert sandbox.handle.killed is False
    assert sandbox.destroy_calls == [{"force": False, "timeout": 10.0}]
    assert "on_stop" in capsys.readouterr().err
    records = [json.loads(line) for line in audit_path.read_text(encoding="utf-8").splitlines()]
    assert [record["lifecycle_state"] for record in records] == [
        "create_requested", "created", "ready", "request_completed", "destroy_requested", "destroyed"
    ]
    assert all(record["provider"] == "microsandbox" for record in records)
    assert all(record["host_worker_id"] == "runtime-worker-micro" for record in records)


@pytest.mark.asyncio
async def test_microsandbox_worker_fails_closed_after_guest_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, sdk = _configure(tmp_path, monkeypatch, respond=False)

    with worker.backend_scope():
        with pytest.raises(RuntimeError, match="without local fallback"):
            await worker._request('{"event":1}')

    sandbox = sdk.instances[0]
    assert sandbox.handle is not None
    assert sandbox.handle.killed is True
    assert sandbox.destroy_calls == [{"force": True, "timeout": 10.0}]


@pytest.mark.asyncio
async def test_microsandbox_rotation_requires_and_forwards_checkpoint(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, sdk = _configure(tmp_path, monkeypatch, respond=True)
    clock = {"now": 100.0}
    monkeypatch.setattr(microsandbox_worker.time, "monotonic", lambda: clock["now"])

    with worker.backend_scope():
        await worker._request('{"strategy_checkpoint":{"schema_version":1}}')
        first = sdk.instances[0]
        clock["now"] = 160.0
        await worker._request('{"strategy_checkpoint":{"schema_version":1,"state":{}}}')

    assert len(sdk.instances) == 2
    assert first.handle is not None
    assert first.handle.stdin.closed is True
    assert first.handle.killed is False
    assert first.destroy_calls == [{"force": False, "timeout": 10.0}]
    assert sdk.instances[1].handle is not None
    assert sdk.instances[1].handle.stdin.writes == [
        b'{"strategy_checkpoint":{"schema_version":1,"state":{}}}\n'
    ]
    await worker.close()


@pytest.mark.asyncio
async def test_microsandbox_module_function_checkpoint_allows_session_rotation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, sdk = _configure(tmp_path, monkeypatch, respond=True, has_spec=False)
    clock = {"now": 100.0}
    monkeypatch.setattr(microsandbox_worker.time, "monotonic", lambda: clock["now"])
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

    with worker.backend_scope():
        await worker._request(bootstrap)
        first = sdk.instances[0]
        clock["now"] = 160.0
        await worker._request(resumed)

    assert len(sdk.instances) == 2
    assert first.handle is not None and first.handle.killed is False
    assert first.destroy_calls == [{"force": False, "timeout": 10.0}]
    assert sdk.instances[1].handle is not None
    assert sdk.instances[1].handle.stdin.writes == [resumed.encode("utf-8") + b"\n"]
    await worker.close()


@pytest.mark.asyncio
async def test_microsandbox_module_function_still_requires_checkpoint_to_rotate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, sdk = _configure(tmp_path, monkeypatch, respond=True, has_spec=False)
    clock = {"now": 100.0}
    monkeypatch.setattr(microsandbox_worker.time, "monotonic", lambda: clock["now"])

    with worker.backend_scope():
        await worker._request('{"event":1}')
        clock["now"] = 160.0
        with pytest.raises(RuntimeError, match="no persisted strategy checkpoint"):
            await worker._request('{"event":2}')

    assert len(sdk.instances) == 1
    await worker.close()


def test_microsandbox_worker_rejects_credential_bearing_backend_url(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy_file = tmp_path / "strategy.py"
    strategy_file.write_text("def on_event(request): return {}\n", encoding="utf-8")
    monkeypatch.setenv(
        "QX_MICROSANDBOX_IMAGE", "qianxing-test@sha256:" + "b" * 64
    )
    monkeypatch.setenv("QX_MICROSANDBOX_URL", "https://user:secret@example.test")

    with pytest.raises(ValueError, match="without embedded credentials"):
        microsandbox_worker.MicrosandboxPythonStrategyWorker(str(strategy_file))


def test_microsandbox_worker_rejects_mutable_image_tag(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy_file = tmp_path / "strategy.py"
    strategy_file.write_text("def on_event(request): return {}\n", encoding="utf-8")
    monkeypatch.setenv("QX_MICROSANDBOX_IMAGE", "qianxing-runtime:latest")

    with pytest.raises(ValueError, match="OCI SHA-256 image reference"):
        microsandbox_worker.MicrosandboxPythonStrategyWorker(str(strategy_file))


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("QX_MICROSANDBOX_CPUS", "0"),
        ("QX_MICROSANDBOX_CPUS", "65"),
        ("QX_MICROSANDBOX_MEMORY_MIB", "511"),
        ("QX_MICROSANDBOX_MEMORY_MIB", "65537"),
    ],
)
def test_microsandbox_worker_rejects_unbounded_resource_policy(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    name: str,
    value: str,
):
    strategy_file = tmp_path / "strategy.py"
    strategy_file.write_text("def on_event(request): return {}\n", encoding="utf-8")
    for env_name, env_value in {
        "QX_MICROSANDBOX_BACKEND": "local",
        "QX_MICROSANDBOX_IMAGE": "qianxing-test@sha256:" + "b" * 64,
        "QX_MICROSANDBOX_CPUS": "1",
        "QX_MICROSANDBOX_MEMORY_MIB": "512",
        "QX_MICROSANDBOX_SESSION_TTL_SECONDS": "60",
        "QX_STRATEGY_PYTHON_TIMEOUT_MS": "5000",
        "QX_STRATEGY_SPEC_JSON": "{}",
    }.items():
        monkeypatch.setenv(env_name, env_value)
    monkeypatch.setenv(name, value)
    sdk = _install_fake_sdk(monkeypatch, respond=True)

    with pytest.raises(ValueError, match=name):
        microsandbox_worker.MicrosandboxPythonStrategyWorker(str(strategy_file))

    assert sdk.create_calls == []
