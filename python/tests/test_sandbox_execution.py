from __future__ import annotations

import hashlib
import importlib.metadata
import io
import json
import sys
import types
import zipfile
from pathlib import Path

import pytest
from qianxing.sandbox_execution import (
    E2BStrategySandboxProvider,
    MicrosandboxStrategySandboxProvider,
    OpenSandboxStrategySandboxProvider,
    SandboxExecutionError,
    _bundle_remote_inputs,
    _extract_output_archive,
    _input_preflight_script,
    _output_pack_script,
    _rewrite_remote_output_paths,
)


def _zip_bytes(files: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, content in files.items():
            archive.writestr(name, content)
    return buffer.getvalue()


def _read_audit_records(path: Path) -> list[dict[str, object]]:
    records = [
        json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
    ]
    schema_path = (
        Path(__file__).resolve().parents[2]
        / "schemas"
        / "python-strategy-sandbox-audit-v1.json"
    )
    from jsonschema import Draft202012Validator

    validator = Draft202012Validator(
        json.loads(schema_path.read_text(encoding="utf-8"))
    )
    previous = "0" * 64
    for sequence, record in enumerate(records, start=1):
        validator.validate(record)
        assert record["record_sequence"] == sequence
        assert record["previous_record_sha256"] == previous
        unsigned = {
            key: value for key, value in record.items() if key != "record_sha256"
        }
        canonical = json.dumps(
            unsigned, sort_keys=True, separators=(",", ":"), ensure_ascii=True
        ).encode("utf-8")
        previous = hashlib.sha256(canonical).hexdigest()
        assert record["record_sha256"] == previous
    assert "api_key" not in json.dumps(records).lower()
    return records


def test_remote_bundle_collects_local_python_without_importing_strategy(tmp_path: Path):
    marker = tmp_path / "imported-on-host.txt"
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "from pathlib import Path\n"
        f"Path({str(marker)!r}).write_text('host import')\n"
        "from helper import value\n"
        "STRATEGY_DEPENDENCIES = ('lazy.py',)\n",
        encoding="utf-8",
    )
    (tmp_path / "helper.py").write_text("value = 1\n", encoding="utf-8")
    (tmp_path / "lazy.py").write_text("value = 2\n", encoding="utf-8")
    bars = tmp_path / "bars.json"
    bars.write_text('{"rows": []}\n', encoding="utf-8")
    spec = {
        "experiment_id": "remote-test",
        "base": {"bars_path": str(bars), "output_dir": "runs/base"},
    }

    packed, manifest = _bundle_remote_inputs(
        strategy,
        spec,
        package_version="0.2.2",
        job_id="job-1",
    )

    assert not marker.exists()
    assert manifest["kind"] == "qianxing-remote-research-input"
    assert manifest["data_files"][0]["sha256"]
    with zipfile.ZipFile(io.BytesIO(packed)) as archive:
        assert "strategy/strategy.py" in archive.namelist()
        assert "strategy/helper.py" in archive.namelist()
        assert "strategy/lazy.py" in archive.namelist()
        remote_spec = json.loads(archive.read("input/spec.json"))
        remote_manifest = json.loads(archive.read("input/job-manifest.json"))
    assert remote_spec["base"]["bars_path"].startswith("/workspace/input/data/")
    assert remote_spec["base"]["output_dir"].startswith("/workspace/output/")
    assert {row["path"] for row in remote_manifest["strategy_files"]} == {
        "strategy/strategy.py",
        "strategy/helper.py",
        "strategy/lazy.py",
    }
    assert remote_manifest["remote_spec_sha256"]


def test_remote_bundle_rejects_secret_fields(tmp_path: Path):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec = {"base": {"bars_path": "bars.json"}, "api_key": "must-not-upload"}

    with pytest.raises(ValueError, match="credential-like field"):
        _bundle_remote_inputs(
            strategy,
            spec,
            package_version="0.2.2",
            job_id="job-1",
        )


@pytest.mark.parametrize(
    "secret_key", ["apiKey", "client_secret", "accessToken", "JWT"]
)
def test_remote_bundle_rejects_camel_case_and_nested_secret_fields(
    tmp_path: Path, secret_key: str
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec = {"nested": {secret_key: "must-not-upload"}}

    with pytest.raises(ValueError, match="credential-like field"):
        _bundle_remote_inputs(
            strategy,
            spec,
            package_version="0.2.2",
            job_id="job-1",
        )


def test_output_archive_rejects_traversal_and_does_not_write_outside(tmp_path: Path):
    target = tmp_path / "output"
    packed = _zip_bytes({"../escape.txt": b"no"})

    with pytest.raises(SandboxExecutionError, match="unsafe path"):
        _extract_output_archive(packed, target)

    assert not (tmp_path / "escape.txt").exists()
    assert not target.exists()


def test_remote_result_path_rejects_traversal_and_windows_separators(tmp_path: Path):
    output = tmp_path / "research"
    for malicious in (
        "/workspace/output/../outside.html",
        "/workspace/output/reports\\..\\outside.html",
        "/workspace/output/C:/outside.html",
    ):
        with pytest.raises(SandboxExecutionError, match="unsafe artifact path"):
            _rewrite_remote_output_paths({"report": malicious}, output)


def test_remote_output_packaging_script_is_valid_python():
    packed_script = _output_pack_script()
    compile(packed_script, "<e2b-output-packager>", "exec")
    assert "max_bytes=268435456" in packed_script
    assert "max_archive=134217728" in packed_script


def test_remote_input_preflight_checks_strategy_data_spec_and_package_fingerprints():
    script = _input_preflight_script("0.2.2")
    compile(script, "<e2b-input-preflight>", "exec")
    assert "strategy bundle fingerprint mismatch" in script
    assert "data file fingerprint mismatch" in script
    assert "research spec fingerprint mismatch" in script
    assert "expected_version = '0.2.2'" in script


def test_e2b_research_uses_isolated_sandbox_and_retrieves_report(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "raise RuntimeError('this module must only run in the sandbox')\n",
        encoding="utf-8",
    )
    bars = tmp_path / "bars.json"
    bars.write_text('{"rows": []}\n', encoding="utf-8")
    spec_path = tmp_path / "research.json"
    spec_path.write_text("{}\n", encoding="utf-8")
    spec = {
        "experiment_id": "remote-test",
        "base": {"bars_path": str(bars), "output_dir": "runs/base"},
    }
    output = tmp_path / "remote-output"
    remote_result = {
        "schema_version": 1,
        "kind": "qianxing-research-report",
        "report_path": "/workspace/output/research.html",
    }
    output_zip = _zip_bytes(
        {
            "exit-code": b"0\n",
            "package-version.txt": b"0.2.2\n",
            "research.json": json.dumps(remote_result).encode("utf-8"),
            "research.html": b"<html></html>",
        }
    )

    class FakeFiles:
        def __init__(self):
            self.uploaded: dict[str, bytes] = {}

        def write(self, path: str, content: bytes):
            self.uploaded[path] = content

        def read(self, path: str, *, format: str):
            assert path == "/workspace/qianxing-output.zip"
            assert format == "bytes"
            return output_zip

    class FakeCommands:
        def __init__(self):
            self.command = ""
            self.timeout = None

        def run(self, command: str, *, timeout: int):
            self.command = command
            self.timeout = timeout
            return types.SimpleNamespace(exit_code=0, stdout="", stderr="")

    class FakeSandbox:
        create_args = None
        last = None

        @classmethod
        def create(cls, template: str, **kwargs):
            instance = cls()
            cls.last = instance
            cls.create_args = (template, kwargs)
            instance.sandbox_id = "sbx-test"
            instance.files = FakeFiles()
            instance.commands = FakeCommands()
            instance.killed = False
            return instance

        def get_info(self):
            return types.SimpleNamespace(cpu_count=2, memory_mb=2048)

        def kill(self):
            self.killed = True

    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    monkeypatch.setitem(sys.modules, "e2b", types.SimpleNamespace(Sandbox=FakeSandbox))

    provider = E2BStrategySandboxProvider(
        template_id="qianxing-test",
        timeout_seconds=120,
        expected_cpu_count=2,
        expected_memory_mb=2048,
        audit_dir=tmp_path / "audit",
    )
    result = provider.run_research(
        strategy_file=strategy,
        input_path=spec_path,
        input_spec=spec,
        output_dir=output,
        diagnostic_startup_windows=[5, 10],
        diagnostic_max_checks=3,
        diagnostic_timeout_seconds=4,
        diagnostic_candidate_limit=7,
        strategy_id="remote-sma",
        strategy_version="v3",
    )

    assert FakeSandbox.create_args[0] == "qianxing-test"
    assert FakeSandbox.create_args[1]["allow_internet_access"] is False
    assert FakeSandbox.create_args[1]["timeout"] == 120
    assert "envs" not in FakeSandbox.create_args[1]
    assert FakeSandbox.create_args[1]["metadata"]["qianxing_expected_cpu_count"] == "2"
    assert (
        FakeSandbox.create_args[1]["metadata"]["qianxing_expected_memory_mb"] == "2048"
    )
    assert FakeSandbox.last.killed is True
    assert "remote input verification failed" in FakeSandbox.last.commands.command
    assert "strategy bundle fingerprint mismatch" in FakeSandbox.last.commands.command
    assert "0.2.2" in FakeSandbox.last.commands.command
    assert (
        b"--diagnostic-candidate-limit 7" in FakeSandbox.last.commands.command.encode()
    )
    assert "--diagnostic-startup-windows 5 10" in FakeSandbox.last.commands.command
    assert result["provider"] == "e2b"
    assert result["sandbox_id"] == "sbx-test"
    assert result["research_result"]["report_path"] == str(output / "research.html")
    assert (output / "research.html").read_bytes() == b"<html></html>"
    saved_result = json.loads((output / "research.json").read_text(encoding="utf-8"))
    assert saved_result["report_path"] == str(output / "research.html")
    audit_path = Path(result["audit_log"])
    audit_records = _read_audit_records(audit_path)
    assert [row["lifecycle_state"] for row in audit_records] == [
        "create_requested",
        "created",
        "ready",
        "request_completed",
        "destroy_requested",
        "destroyed",
    ]
    assert all(row["provider"] == "e2b" for row in audit_records)
    assert audit_records[0]["resource_limits"]["cpu_count"] == 2
    assert audit_records[0]["resource_limits"]["memory_bytes"] == 2_048_000_000
    assert audit_records[3]["response_sha256"]
    assert audit_records[3]["strategy_id"] == "remote-sma"
    assert audit_records[3]["strategy_version"] == "v3"
    assert len(audit_records[3]["data_fingerprint"]) == 64
    assert result["audit_log"] not in json.dumps(FakeSandbox.create_args[1])


def test_e2b_command_failure_cleans_up_without_local_fallback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text("{}\n", encoding="utf-8")
    state = {"killed": False}

    class FakeFiles:
        def write(self, _path: str, _content: bytes):
            return None

        def read(self, *_args, **_kwargs):
            raise AssertionError("failed commands must not try to fetch a result")

    class FakeCommands:
        def run(self, _command: str, *, timeout: int):
            assert timeout == 120
            raise RuntimeError("simulated provider command failure")

    class FakeSandbox:
        @classmethod
        def create(cls, _template: str, **_kwargs):
            instance = cls()
            instance.sandbox_id = "sbx-failure"
            instance.files = FakeFiles()
            instance.commands = FakeCommands()
            return instance

        def get_info(self):
            return types.SimpleNamespace(cpu_count=2, memory_mb=2048)

        def kill(self):
            state["killed"] = True
            raise RuntimeError("simulated cleanup failure")

    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    monkeypatch.setitem(sys.modules, "e2b", types.SimpleNamespace(Sandbox=FakeSandbox))

    provider = E2BStrategySandboxProvider(
        template_id="qianxing-test",
        timeout_seconds=120,
        expected_cpu_count=2,
        expected_memory_mb=2048,
        audit_dir=tmp_path / "audit",
    )
    with pytest.raises(
        SandboxExecutionError,
        match="additionally, E2B sandbox sbx-failure cleanup failed",
    ) as raised:
        provider.run_research(strategy_file=strategy, input_path=spec_path)
    assert state["killed"] is True
    assert "simulated provider command failure" in str(raised.value)
    audit_files = list((tmp_path / "audit").glob("*.jsonl"))
    assert len(audit_files) == 1
    assert str(audit_files[0]) in str(raised.value)
    audit_records = _read_audit_records(audit_files[0])
    audit_states = [row["lifecycle_state"] for row in audit_records]
    assert audit_states == [
        "create_requested",
        "created",
        "ready",
        "request_failed",
        "destroy_requested",
        "destroy_failed",
    ]


def test_e2b_research_rejects_template_resource_mismatch_before_upload(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text("{}\n", encoding="utf-8")
    state = {"uploaded": False, "command_started": False, "killed": False}

    class FakeFiles:
        def write(self, _path: str, _content: bytes):
            state["uploaded"] = True

    class FakeCommands:
        def run(self, _command: str, *, timeout: int):
            state["command_started"] = True

    class FakeSandbox:
        @classmethod
        def create(cls, _template: str, **_kwargs):
            instance = cls()
            instance.sandbox_id = "sbx-resource-mismatch"
            instance.files = FakeFiles()
            instance.commands = FakeCommands()
            return instance

        def get_info(self):
            return types.SimpleNamespace(cpu_count=1, memory_mb=1024)

        def kill(self):
            state["killed"] = True

    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    monkeypatch.setitem(sys.modules, "e2b", types.SimpleNamespace(Sandbox=FakeSandbox))
    provider = E2BStrategySandboxProvider(
        template_id="qianxing-test",
        timeout_seconds=120,
        expected_cpu_count=2,
        expected_memory_mb=2048,
        audit_dir=tmp_path / "audit",
    )

    with pytest.raises(SandboxExecutionError, match="do not match the host policy"):
        provider.run_research(strategy_file=strategy, input_path=spec_path)

    assert state == {"uploaded": False, "command_started": False, "killed": True}
    audit_files = list((tmp_path / "audit").glob("*.jsonl"))
    assert len(audit_files) == 1
    audit_records = _read_audit_records(audit_files[0])
    audit_states = [row["lifecycle_state"] for row in audit_records]
    assert audit_states == [
        "create_requested",
        "created",
        "request_failed",
        "destroy_requested",
        "destroyed",
    ]


def test_remote_research_audit_storage_failure_prevents_sandbox_creation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text("{}\n", encoding="utf-8")
    audit_file = tmp_path / "not-a-directory"
    audit_file.write_text("occupied\n", encoding="utf-8")
    state = {"created": False}

    class FakeSandbox:
        @classmethod
        def create(cls, *_args, **_kwargs):
            state["created"] = True
            raise AssertionError("provider must not start without a writable audit log")

    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    monkeypatch.setitem(sys.modules, "e2b", types.SimpleNamespace(Sandbox=FakeSandbox))
    provider = E2BStrategySandboxProvider(
        template_id="qianxing-test",
        timeout_seconds=120,
        expected_cpu_count=2,
        expected_memory_mb=2048,
        audit_dir=audit_file,
    )

    with pytest.raises(OSError):
        provider.run_research(strategy_file=strategy, input_path=spec_path)
    assert state["created"] is False


def test_e2b_research_requires_declared_template_resources(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.delenv("QX_E2B_EXPECTED_CPU_COUNT", raising=False)
    monkeypatch.delenv("QX_E2B_EXPECTED_MEMORY_MB", raising=False)
    with pytest.raises(ValueError, match="QX_E2B_EXPECTED_CPU_COUNT"):
        E2BStrategySandboxProvider(template_id="qianxing-test")


def test_opensandbox_research_applies_cpu_memory_and_deny_all_limits(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    bars = tmp_path / "bars.json"
    bars.write_text('{"rows": []}\n', encoding="utf-8")
    spec = {
        "experiment_id": "opensandbox-resource-test",
        "base": {"bars_path": str(bars), "output_dir": "runs/base"},
    }
    spec_path = tmp_path / "research.json"
    spec_path.write_text(json.dumps(spec), encoding="utf-8")
    output = tmp_path / "remote-output"
    remote_result = {
        "schema_version": 1,
        "kind": "qianxing-research-report",
        "report_path": "/workspace/output/research.html",
    }
    output_archive = _zip_bytes(
        {
            "exit-code": b"0\n",
            "research.json": json.dumps(remote_result).encode("utf-8"),
            "research.html": b"<html></html>",
        }
    )

    class FakeValue:
        def __init__(self, **values):
            self.values = values
            for key, value in values.items():
                setattr(self, key, value)

    class FakeWriteEntry(FakeValue):
        pass

    class FakeFiles:
        def write_files(self, _entries):
            return None

        def read_bytes(self, path: str):
            assert path == "/workspace/qianxing-output.zip"
            return output_archive

    class FakeCommands:
        def run(self, _command: str, *, opts):
            return types.SimpleNamespace(
                exit_code=0, logs=types.SimpleNamespace(stderr=[])
            )

    class FakeSandbox:
        create_args = None

        @classmethod
        def create(cls, image: str, **kwargs):
            cls.create_args = (image, kwargs)
            instance = cls()
            instance.id = "osb-resource-test"
            instance.files = FakeFiles()
            instance.commands = FakeCommands()
            instance.destroyed = False
            return instance

        def destroy(self):
            self.destroyed = True

    opensandbox = types.ModuleType("opensandbox")
    opensandbox.SandboxSync = FakeSandbox
    config = types.ModuleType("opensandbox.config")
    config.ConnectionConfigSync = FakeValue
    execd = types.ModuleType("opensandbox.models.execd")
    execd.RunCommandOpts = FakeValue
    filesystem = types.ModuleType("opensandbox.models.filesystem")
    filesystem.WriteEntry = FakeWriteEntry
    sandboxes = types.ModuleType("opensandbox.models.sandboxes")
    sandboxes.NetworkPolicy = FakeValue
    for name, module in (
        ("opensandbox", opensandbox),
        ("opensandbox.config", config),
        ("opensandbox.models", types.ModuleType("opensandbox.models")),
        ("opensandbox.models.execd", execd),
        ("opensandbox.models.filesystem", filesystem),
        ("opensandbox.models.sandboxes", sandboxes),
    ):
        monkeypatch.setitem(sys.modules, name, module)
    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")

    provider = OpenSandboxStrategySandboxProvider(
        domain="sandbox.example.test:8080",
        image="qianxing-runtime@sha256:" + "a" * 64,
        timeout_seconds=120,
        cpu_count=3,
        memory_mib=4096,
        audit_dir=tmp_path / "audit",
    )
    result = provider.run_research(
        strategy_file=strategy,
        input_path=spec_path,
        input_spec=spec,
        output_dir=output,
    )

    assert FakeSandbox.create_args[0] == "qianxing-runtime@sha256:" + "a" * 64
    create_options = FakeSandbox.create_args[1]
    assert create_options["resource"] == {"cpu": "3", "memory": "4096Mi"}
    assert create_options["network_policy"].values == {
        "defaultAction": "deny",
        "egress": [],
    }
    assert create_options["metadata"]["qianxing_resource_cpu"] == "3"
    assert create_options["metadata"]["qianxing_resource_memory_mib"] == "4096"
    assert result["resource_limits"] == {"cpu": "3", "memory": "4096Mi"}
    assert (output / "research.html").read_bytes() == b"<html></html>"
    audit_records = _read_audit_records(Path(result["audit_log"]))
    assert [row["lifecycle_state"] for row in audit_records] == [
        "create_requested",
        "created",
        "ready",
        "request_completed",
        "destroy_requested",
        "destroyed",
    ]
    assert audit_records[0]["network_policy"] == "deny_all"
    assert audit_records[0]["resource_limits"]["memory_bytes"] == 4096 * 1024 * 1024


@pytest.mark.parametrize(
    "limits",
    [
        {"cpu_count": 0},
        {"cpu_count": 65},
        {"cpu_count": True},
        {"memory_mib": 511},
        {"memory_mib": 65_537},
        {"memory_mib": True},
    ],
)
def test_opensandbox_research_rejects_invalid_resource_limits_before_network(
    limits: dict[str, object],
):
    with pytest.raises(ValueError):
        OpenSandboxStrategySandboxProvider(
            domain="sandbox.example.test:8080",
            image="qianxing-runtime@sha256:" + "a" * 64,
            **limits,
        )


def test_opensandbox_research_rejects_mutable_image_tags():
    with pytest.raises(ValueError, match="OCI SHA-256 image reference"):
        OpenSandboxStrategySandboxProvider(
            domain="sandbox.example.test:8080",
            image="qianxing-runtime:latest",
        )


def test_microsandbox_research_uses_host_backend_and_network_none(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    from contextlib import contextmanager

    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text(
        json.dumps(
            {
                "experiment_id": "microsandbox-test",
                "base": {"run_id": "micro-run"},
                "split_plan": {"train_bars": 10, "test_bars": 5},
            }
        ),
        encoding="utf-8",
    )
    output_archive = _zip_bytes(
        {
            "exit-code": b"0\n",
            "research.json": json.dumps(
                {"report_html": "/workspace/output/research.html"}
            ).encode(),
            "research.html": b"<html>ok</html>",
        }
    )
    calls: dict[str, object] = {"backend": None, "sandbox": None}

    class BackendKind:
        LOCAL = "local"
        CLOUD = "cloud"

    class Network:
        @staticmethod
        def none():
            return "deny-all"

    class FakeSandbox:
        name = "qianxing-micro-job"

        def __init__(self):
            self.exec_calls: list[tuple[str, list[str], dict[str, object]]] = []
            self.files: dict[str, bytes] = {}
            self.destroy_calls: list[dict[str, object]] = []
            self.fs = self

        async def exec(self, command: str, args: list[str], **kwargs):
            self.exec_calls.append((command, args, kwargs))
            return types.SimpleNamespace(success=True, stderr_text="")

        async def write(self, path: str, content: bytes):
            self.files[path] = content

        async def read(self, path: str):
            assert path == "/workspace/output/qianxing-output.zip"
            return output_archive

        async def destroy(self, **kwargs):
            self.destroy_calls.append(kwargs)

    sandbox = FakeSandbox()

    class Sandbox:
        @staticmethod
        async def create(name: str, **kwargs):
            calls["create"] = (name, kwargs)
            return sandbox

    @contextmanager
    def backend_scope(kind: str, **kwargs):
        calls["backend"] = (kind, kwargs)
        yield

    monkeypatch.setitem(
        sys.modules,
        "microsandbox",
        types.SimpleNamespace(
            BackendKind=BackendKind,
            Network=Network,
            Sandbox=Sandbox,
            backend_scope=backend_scope,
        ),
    )
    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    audit_dir = tmp_path / "audit"
    provider = MicrosandboxStrategySandboxProvider(
        image="qianxing-runtime@sha256:" + "b" * 64,
        backend="cloud",
        api_key="host-only-secret",
        url="https://sandbox.example.test",
        timeout_seconds=120,
        cpu_count=2,
        memory_mib=2048,
        audit_dir=audit_dir,
    )

    result = provider.run_research(
        strategy_file=strategy,
        input_path=spec_path,
        output_dir=tmp_path / "result",
    )

    assert result["provider"] == "microsandbox"
    assert result["backend"] == "cloud"
    assert result["network_egress"] == "deny_all"
    assert calls["backend"] == (
        BackendKind.CLOUD,
        {"api_key": "host-only-secret", "url": "https://sandbox.example.test"},
    )
    _, create_options = calls["create"]
    assert create_options["network"] == "deny-all"
    assert create_options["cpus"] == 2
    assert create_options["memory"] == 2048
    assert "host-only-secret" not in repr(sandbox.exec_calls)
    assert sandbox.files["/workspace/qianxing-input.zip"]
    assert sandbox.destroy_calls == [{"force": True, "timeout": 10.0}]
    records = _read_audit_records(next(audit_dir.glob("remote-research-*.jsonl")))
    assert [record["lifecycle_state"] for record in records] == [
        "create_requested",
        "created",
        "ready",
        "request_completed",
        "destroy_requested",
        "destroyed",
    ]
    assert "host-only-secret" not in json.dumps(records)


def test_microsandbox_research_cleans_up_and_fails_closed_on_guest_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    from contextlib import contextmanager

    strategy = tmp_path / "strategy.py"
    strategy.write_text("def create_strategy(spec): return object()\n", encoding="utf-8")
    spec_path = tmp_path / "research.json"
    spec_path.write_text("{}\n", encoding="utf-8")
    destroyed: list[bool] = []

    class FakeSandbox:
        name = "qianxing-micro-failed"

        def __init__(self):
            self.fs = self

        async def exec(self, command: str, args: list[str], **kwargs):
            if command == "env":
                raise RuntimeError("guest transport failed host-only-secret")
            return types.SimpleNamespace(success=True, stderr_text="")

        async def write(self, _path: str, _content: bytes):
            return None

        async def destroy(self, **kwargs):
            destroyed.append(kwargs.get("force") is True)

    sandbox = FakeSandbox()

    class BackendKind:
        LOCAL = "local"
        CLOUD = "cloud"

    class Network:
        @staticmethod
        def none():
            return "deny-all"

    class Sandbox:
        @staticmethod
        async def create(_name: str, **_kwargs):
            return sandbox

    @contextmanager
    def backend_scope(_kind: str, **_kwargs):
        yield

    monkeypatch.setitem(
        sys.modules,
        "microsandbox",
        types.SimpleNamespace(
            BackendKind=BackendKind,
            Network=Network,
            Sandbox=Sandbox,
            backend_scope=backend_scope,
        ),
    )
    monkeypatch.setattr(importlib.metadata, "version", lambda _name: "0.2.2")
    provider = MicrosandboxStrategySandboxProvider(
        image="qianxing-runtime@sha256:" + "c" * 64,
        backend="cloud",
        api_key="host-only-secret",
        audit_dir=tmp_path / "audit",
    )

    with pytest.raises(SandboxExecutionError, match="research job failed") as caught:
        provider.run_research(
            strategy_file=strategy,
            input_path=spec_path,
            output_dir=tmp_path / "result",
        )

    assert "host-only-secret" not in str(caught.value)
    assert destroyed == [True]


@pytest.mark.parametrize(
    "kwargs",
    [
        {"image": "qianxing-runtime:latest"},
        {"image": "qianxing-runtime@sha256:" + "a" * 64, "backend": "cloud"},
        {
            "image": "qianxing-runtime@sha256:" + "a" * 64,
            "url": "https://user:pass@sandbox.example.test",
        },
        {"image": "qianxing-runtime@sha256:" + "a" * 64, "cpu_count": 0},
        {"image": "qianxing-runtime@sha256:" + "a" * 64, "memory_mib": 511},
    ],
)
def test_microsandbox_research_rejects_unsafe_configuration(kwargs: dict[str, object]):
    with pytest.raises((ValueError, TypeError)):
        MicrosandboxStrategySandboxProvider(**kwargs)


@pytest.mark.asyncio
async def test_microsandbox_research_rejects_active_event_loop_before_provider_access(
    tmp_path: Path,
):
    provider = MicrosandboxStrategySandboxProvider(
        image="qianxing-runtime@sha256:" + "e" * 64,
        audit_dir=tmp_path / "audit",
    )

    with pytest.raises(SandboxExecutionError, match="active event loop"):
        provider.run_research(
            strategy_file=tmp_path / "not-read.py",
            input_path=tmp_path / "not-read.json",
        )

    assert not (tmp_path / "audit").exists()


def test_remote_research_cli_dispatches_to_provider(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    from qianxing import cli, sandbox_execution

    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text(
        json.dumps(
            {
                "experiment_id": "cli-test",
                "base": {"run_id": "remote-cli-test"},
                "split_plan": {"train_bars": 10, "test_bars": 5},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    calls = {}

    class FakeProvider:
        def __init__(self, *, template_id: str, timeout_seconds: int):
            calls["provider"] = (template_id, timeout_seconds)

        def run_research(self, **kwargs):
            calls["run"] = kwargs
            return {"kind": "qianxing-remote-research-execution", "job_id": "job-test"}

    monkeypatch.setattr(sandbox_execution, "E2BStrategySandboxProvider", FakeProvider)
    monkeypatch.setattr(
        cli,
        "prepare_quick_research_spec",
        lambda payload: {**payload, "baselines": ["buy-and-hold"]},
    )

    result = cli.main(
        [
            "remote-research",
            str(strategy),
            str(spec_path),
            "--template",
            "qianxing-test",
        ]
    )

    assert result == 0
    assert calls["provider"] == ("qianxing-test", 900)
    assert calls["run"]["strategy_file"] == str(strategy)
    assert calls["run"]["input_spec"]["experiment_id"] == "cli-test"
    assert calls["run"]["input_spec"]["baselines"] == ["buy-and-hold"]
    assert calls["run"]["diagnostic_candidate_limit"] == 16
    assert json.loads(capsys.readouterr().out)["job_id"] == "job-test"


def test_remote_research_cli_passes_opensandbox_resource_limits(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    from qianxing import cli, sandbox_execution

    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text(
        json.dumps(
            {
                "experiment_id": "cli-test",
                "base": {"run_id": "remote-cli-test"},
                "split_plan": {"train_bars": 10, "test_bars": 5},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    calls = {}

    class FakeProvider:
        def __init__(self, **kwargs):
            calls["provider"] = kwargs

        def run_research(self, **kwargs):
            calls["run"] = kwargs
            return {"kind": "qianxing-remote-research-execution", "job_id": "job-test"}

    monkeypatch.setattr(
        sandbox_execution, "OpenSandboxStrategySandboxProvider", FakeProvider
    )
    monkeypatch.setattr(
        cli,
        "prepare_quick_research_spec",
        lambda payload: {**payload, "baselines": ["buy-and-hold"]},
    )
    result = cli.main(
        [
            "remote-research",
            str(strategy),
            str(spec_path),
            "--provider",
            "opensandbox",
            "--domain",
            "sandbox.example.test:8080",
            "--image",
            "qianxing-runtime@sha256:" + "a" * 64,
            "--cpus",
            "3",
            "--memory-mib",
            "4096",
        ]
    )

    assert result == 0
    assert calls["provider"]["cpu_count"] == 3
    assert calls["provider"]["memory_mib"] == 4096
    assert calls["provider"]["domain"] == "sandbox.example.test:8080"
    assert calls["run"]["input_spec"]["baselines"] == ["buy-and-hold"]
    assert calls["run"]["diagnostic_candidate_limit"] == 16
    assert json.loads(capsys.readouterr().out)["job_id"] == "job-test"


def test_remote_research_cli_dispatches_to_microsandbox(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    from qianxing import cli, sandbox_execution

    strategy = tmp_path / "strategy.py"
    strategy.write_text(
        "def create_strategy(spec): return object()\n", encoding="utf-8"
    )
    spec_path = tmp_path / "research.json"
    spec_path.write_text(
        json.dumps(
            {
                "experiment_id": "cli-microsandbox-test",
                "base": {"run_id": "remote-cli-test"},
                "split_plan": {"train_bars": 10, "test_bars": 5},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    calls: dict[str, object] = {}

    class FakeProvider:
        def __init__(self, **kwargs):
            calls["provider"] = kwargs

        def run_research(self, **kwargs):
            calls["run"] = kwargs
            return {"kind": "qianxing-remote-research-execution", "job_id": "micro-job"}

    monkeypatch.setattr(
        sandbox_execution, "MicrosandboxStrategySandboxProvider", FakeProvider
    )
    monkeypatch.setattr(
        cli,
        "prepare_quick_research_spec",
        lambda payload: {**payload, "baselines": ["buy-and-hold"]},
    )
    result = cli.main(
        [
            "remote-research",
            str(strategy),
            str(spec_path),
            "--provider",
            "microsandbox",
            "--image",
            "qianxing-runtime@sha256:" + "d" * 64,
            "--backend",
            "cloud",
            "--cpus",
            "3",
            "--memory-mib",
            "3072",
        ]
    )

    assert result == 0
    assert calls["provider"]["image"].endswith("d" * 64)
    assert calls["provider"]["backend"] == "cloud"
    assert calls["provider"]["cpu_count"] == 3
    assert calls["provider"]["memory_mib"] == 3072
    assert calls["run"]["input_spec"]["baselines"] == ["buy-and-hold"]
    assert json.loads(capsys.readouterr().out)["job_id"] == "micro-job"
