from __future__ import annotations

import importlib.metadata
import json
import queue
import sys
import types
from pathlib import Path

import pytest
from qianxing_strategy import e2b_worker
from qianxing_strategy.e2b_worker import _RemoteOutput


class FakeRemoteHandle:
    def __init__(self, *, respond: bool = True) -> None:
        self.respond = respond
        self.stdin: list[str] = []
        self.killed = False
        self.closed = False
        self.events: queue.Queue[tuple[str, str | None]] = queue.Queue()
        self.wait_callbacks = None

    def send_stdin(self, value: str) -> None:
        self.stdin.append(value)
        if self.respond:
            self.events.put(("stdout", '{"ok":true}\n'))

    def wait(self, *, on_stdout, on_stderr):
        self.wait_callbacks = (on_stdout, on_stderr)
        while True:
            kind, value = self.events.get()
            if kind == "end":
                return types.SimpleNamespace(exit_code=0)
            if kind == "stdout" and value is not None:
                on_stdout(value)

    def close_stdin(self) -> None:
        self.closed = True
        self.events.put(("end", None))

    def kill(self) -> None:
        self.killed = True
        self.events.put(("end", None))


class FakeSandboxFactory:
    def __init__(
        self,
        *,
        respond: bool = True,
        fail_ready: bool = False,
        cpu_count: int | None = 2,
        memory_mb: int | None = 2048,
        omit_get_info: bool = False,
    ) -> None:
        self.respond = respond
        self.fail_ready = fail_ready
        self.cpu_count = cpu_count
        self.memory_mb = memory_mb
        self.omit_get_info = omit_get_info
        self.instances = []
        self.create_calls = []

    def create(self, template: str, **kwargs):
        instance = types.SimpleNamespace()
        instance.files = types.SimpleNamespace(writes=[])

        def write(path: str, content: bytes) -> None:
            instance.files.writes.append((path, content))

        instance.files.write = write
        instance.commands = types.SimpleNamespace(calls=[], handle=None)

        def run(command: str, **run_kwargs):
            instance.commands.calls.append((command, run_kwargs))
            if run_kwargs.get("background"):
                handle = FakeRemoteHandle(respond=self.respond)
                instance.commands.handle = handle
                return handle
            if self.fail_ready and "pathlib.Path" in command:
                return types.SimpleNamespace(exit_code=78, stderr="not ready")
            return types.SimpleNamespace(exit_code=0, stderr="")

        instance.commands.run = run
        instance.killed = False

        def get_info():
            info = types.SimpleNamespace()
            if self.cpu_count is not None:
                info.cpu_count = self.cpu_count
            if self.memory_mb is not None:
                info.memory_mb = self.memory_mb
            return info

        if not self.omit_get_info:
            instance.get_info = get_info

        def kill() -> None:
            instance.killed = True

        instance.kill = kill
        self.instances.append(instance)
        self.create_calls.append((template, kwargs))
        return instance


def _configure_worker(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    respond: bool = True,
    fail_ready: bool = False,
    cpu_count: int | None = 2,
    memory_mb: int | None = 2048,
    omit_get_info: bool = False,
    has_spec: bool = True,
) -> tuple[e2b_worker.E2BPythonStrategyWorker, FakeSandboxFactory]:
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request): return {}\n", encoding="utf-8")
    factory = FakeSandboxFactory(
        respond=respond,
        fail_ready=fail_ready,
        cpu_count=cpu_count,
        memory_mb=memory_mb,
        omit_get_info=omit_get_info,
    )
    monkeypatch.setenv("QX_E2B_TEMPLATE", "qianxing-test")
    monkeypatch.setenv("QX_E2B_EXPECTED_CPU_COUNT", "2")
    monkeypatch.setenv("QX_E2B_EXPECTED_MEMORY_MB", "2048")
    monkeypatch.setenv("E2B_API_KEY", "host-only-test-api-key")
    monkeypatch.setenv("QX_STRATEGY_PYTHON_TIMEOUT_MS", "5000")
    monkeypatch.setenv("QX_E2B_SESSION_TTL_SECONDS", "60")
    if has_spec:
        monkeypatch.setenv("QX_STRATEGY_SPEC_JSON", "{}")
    else:
        monkeypatch.delenv("QX_STRATEGY_SPEC_JSON", raising=False)
    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    monkeypatch.setitem(sys.modules, "e2b", types.SimpleNamespace(Sandbox=factory))
    return e2b_worker.E2BPythonStrategyWorker(str(strategy)), factory


def test_remote_output_allows_one_response_and_fails_closed_on_extra_lines():
    output = _RemoteOutput()
    output.begin_request()

    output.on_stdout('{"ok":true}\n')
    output.on_stdout('{"unexpected":true}\n')

    assert output.responses.get_nowait() == '{"ok":true}'
    assert output.finish_request() is not None
    assert output.responses.empty()
    with pytest.raises(RuntimeError, match="multiple responses"):
        output.begin_request()


def test_remote_output_marks_late_stdout_as_fatal_for_the_next_request():
    output = _RemoteOutput()
    output.begin_request()
    output.on_stdout('{"ok":true}\n')
    assert output.responses.get_nowait() == '{"ok":true}'
    assert output.finish_request() is None

    output.on_stdout('{"late":true}\n')
    with pytest.raises(RuntimeError, match="unsolicited stdout"):
        output.begin_request()


def test_remote_output_rejects_trailing_partial_line_after_response():
    output = _RemoteOutput()
    output.begin_request()
    output.on_stdout('{"ok":true}\ntrailing')
    assert output.responses.get_nowait() == '{"ok":true}'

    error = output.finish_request()
    assert error is not None
    assert "incomplete protocol line" in str(error)


def test_remote_output_bounds_partial_protocol_lines(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(e2b_worker, "_MAX_REMOTE_LINE_BYTES", 8)
    output = _RemoteOutput()
    output.begin_request()
    output.on_stdout("123456789")

    with pytest.raises(RuntimeError, match="exceeded 8 bytes"):
        output.begin_request()


def test_e2b_worker_starts_lazily_and_keeps_host_key_out_of_remote_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    audit_path = tmp_path / "audit" / "e2b.jsonl"
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_PATH", str(audit_path))
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED", "true")
    monkeypatch.setenv("QX_STRATEGY_HOST_WORKER_ID", "runtime-worker-e2b")
    worker, factory = _configure_worker(tmp_path, monkeypatch)
    assert factory.create_calls == []

    assert worker.request('{"event":1}') == '{"ok":true}'

    template, options = factory.create_calls[0]
    assert template == "qianxing-test"
    assert options["allow_internet_access"] is False
    assert options["timeout"] == 60
    assert options["metadata"]["qianxing_expected_cpu_count"] == "2"
    assert options["metadata"]["qianxing_expected_memory_mb"] == "2048"
    remote_commands = [command for command, _ in factory.instances[0].commands.calls]
    worker_command = next(
        command for command in remote_commands if "--ready-file" in command
    )
    assert worker_command.startswith("env -i ")
    assert "host-only-test-api-key" not in worker_command
    assert "E2B_API_KEY" not in worker_command
    assert str(audit_path) not in worker_command
    assert "QX_STRATEGY_HOST_WORKER_ID" not in worker_command
    assert factory.instances[0].commands.handle.stdin == ['{"event":1}\n']
    assert all(
        "on_stdout" not in kwargs and "on_stderr" not in kwargs
        for _, kwargs in factory.instances[0].commands.calls
        if kwargs.get("background")
    )
    assert factory.instances[0].commands.handle.wait_callbacks is not None
    worker.close()
    assert factory.instances[0].killed is True
    assert factory.instances[0].commands.handle.killed is False
    records = [json.loads(line) for line in audit_path.read_text(encoding="utf-8").splitlines()]
    assert [record["lifecycle_state"] for record in records] == [
        "create_requested", "created", "ready", "request_completed", "destroy_requested", "destroyed"
    ]
    assert all(record["provider"] == "e2b" for record in records)
    assert all(record["host_worker_id"] == "runtime-worker-e2b" for record in records)
    assert "host-only-test-api-key" not in audit_path.read_text(encoding="utf-8")


def test_e2b_session_rotation_requires_and_restores_a_checkpoint(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, factory = _configure_worker(tmp_path, monkeypatch)
    clock = {"now": 100.0}
    monkeypatch.setattr(e2b_worker.time, "monotonic", lambda: clock["now"])

    worker.request('{"strategy_checkpoint":null}')
    first = factory.instances[0]
    clock["now"] = 160.0
    worker.request('{"strategy_checkpoint":{"schema_version":1}}')

    assert len(factory.instances) == 2
    assert first.killed is True
    assert first.commands.handle.killed is False
    assert first.commands.handle.stdin == ['{"strategy_checkpoint":null}\n']
    assert factory.instances[1].commands.handle.stdin == [
        '{"strategy_checkpoint":{"schema_version":1}}\n'
    ]
    worker.close()


def test_e2b_module_function_checkpoint_allows_session_rotation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, factory = _configure_worker(tmp_path, monkeypatch, has_spec=False)
    clock = {"now": 100.0}
    monkeypatch.setattr(e2b_worker.time, "monotonic", lambda: clock["now"])
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
    first = factory.instances[0]
    clock["now"] = 160.0
    worker.request(resumed)

    assert len(factory.instances) == 2
    assert first.killed is True
    assert factory.instances[1].commands.handle.stdin == [resumed + "\n"]
    worker.close()


def test_e2b_session_refuses_restart_without_checkpoint(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, factory = _configure_worker(tmp_path, monkeypatch, has_spec=False)
    clock = {"now": 100.0}
    monkeypatch.setattr(e2b_worker.time, "monotonic", lambda: clock["now"])

    worker.request('{"event":1}')
    clock["now"] = 160.0
    with pytest.raises(RuntimeError, match="no persisted strategy checkpoint"):
        worker.request('{"event":2}')

    assert len(factory.instances) == 1
    worker.close()


def test_e2b_timeout_fails_closed_without_local_fallback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, factory = _configure_worker(tmp_path, monkeypatch, respond=False)

    def timeout(*, timeout: float):
        raise queue.Empty

    worker.output.responses.get = timeout
    with pytest.raises(TimeoutError, match="without local fallback"):
        worker.request('{"event":1}')

    assert factory.instances[0].commands.handle.stdin == ['{"event":1}\n']
    worker.close()


def test_e2b_startup_failure_cleans_sandbox_and_does_not_run_strategy(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, factory = _configure_worker(tmp_path, monkeypatch, fail_ready=True)

    with pytest.raises(RuntimeError, match="did not initialize"):
        worker.request('{"event":1}')

    assert len(factory.instances) == 1
    assert factory.instances[0].killed is True
    assert factory.instances[0].commands.handle.killed is True
    assert factory.instances[0].commands.handle.stdin == []
    worker.close()


@pytest.mark.parametrize(
    ("cpu_count", "memory_mb", "message"),
    [
        (4, 2048, "do not match"),
        (2, 4096, "do not match"),
        (None, 2048, "did not report verifiable"),
        (2, None, "did not report verifiable"),
    ],
)
def test_e2b_resource_policy_fails_closed_before_strategy_start(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    cpu_count: int | None,
    memory_mb: int | None,
    message: str,
):
    worker, factory = _configure_worker(
        tmp_path,
        monkeypatch,
        cpu_count=cpu_count,
        memory_mb=memory_mb,
    )

    with pytest.raises(RuntimeError, match=message):
        worker.request('{"event":1}')

    sandbox = factory.instances[0]
    assert sandbox.killed is True
    assert sandbox.commands.handle is None
    assert sandbox.files.writes == []
    worker.close()


def test_e2b_requires_sdk_resource_inspection_before_strategy_start(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    worker, factory = _configure_worker(
        tmp_path,
        monkeypatch,
        omit_get_info=True,
    )

    with pytest.raises(RuntimeError, match="does not expose sandbox CPU and memory"):
        worker.request('{"event":1}')

    sandbox = factory.instances[0]
    assert sandbox.killed is True
    assert sandbox.commands.handle is None
    assert sandbox.files.writes == []
    worker.close()


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("QX_E2B_EXPECTED_CPU_COUNT", None),
        ("QX_E2B_EXPECTED_CPU_COUNT", "0"),
        ("QX_E2B_EXPECTED_MEMORY_MB", None),
        ("QX_E2B_EXPECTED_MEMORY_MB", "two-gib"),
    ],
)
def test_e2b_requires_positive_host_resource_policy(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    name: str,
    value: str | None,
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request): return {}\n", encoding="utf-8")
    monkeypatch.setenv("QX_E2B_TEMPLATE", "qianxing-test")
    monkeypatch.setenv("E2B_API_KEY", "host-only-test-api-key")
    monkeypatch.setenv("QX_E2B_EXPECTED_CPU_COUNT", "2")
    monkeypatch.setenv("QX_E2B_EXPECTED_MEMORY_MB", "2048")
    if value is None:
        monkeypatch.delenv(name, raising=False)
    else:
        monkeypatch.setenv(name, value)

    with pytest.raises((RuntimeError, ValueError), match="QX_E2B_EXPECTED"):
        e2b_worker.E2BPythonStrategyWorker(str(strategy))


def test_remote_input_bundle_rejects_credential_files_and_inline_keys(tmp_path: Path):
    from qianxing.sandbox_execution import _bundle_remote_inputs

    strategy = tmp_path / "strategy.py"
    strategy.write_text("def on_event(request): return {}\n", encoding="utf-8")
    secret_file = tmp_path / ".env.production"
    secret_file.write_text("TOKEN=secret-value\n", encoding="utf-8")
    with pytest.raises(ValueError, match="appears to contain credentials"):
        _bundle_remote_inputs(
            strategy,
            {"data": {"path": str(secret_file)}},
            package_version="0.2.2",
            job_id="job-test",
        )

    strategy.write_text(
        "API_KEY = 'obviously-literal-secret-value'\n"
        "def on_event(request): return {}\n",
        encoding="utf-8",
    )
    with pytest.raises(ValueError, match="inline credential"):
        _bundle_remote_inputs(
            strategy,
            {},
            package_version="0.2.2",
            job_id="job-test",
        )
