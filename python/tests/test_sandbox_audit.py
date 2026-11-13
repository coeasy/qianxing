from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from qianxing_strategy.sandbox_audit import (
    SandboxExecutionAudit,
    request_data_fingerprint,
    request_identity,
)


def _audit(path, *, provider="e2b") -> SandboxExecutionAudit:
    return SandboxExecutionAudit(
        provider=provider,
        job_id="job-1",
        package_version="0.2.2",
        entry_sha256="a" * 64,
        bundle_sha256="b" * 64,
        spec_sha256="c" * 64,
        resource_limits={
            "cpu_count": 2,
            "memory_bytes": 2_147_483_648,
            "request_timeout_ms": 5_000,
            "session_ttl_seconds": 60,
            "max_request_bytes": 16 * 1024 * 1024,
            "max_response_bytes": 16 * 1024 * 1024,
        },
        network_policy="deny_all",
        runtime_ref="qianxing-template",
        host_worker_id="strategy-worker-01",
        path=path,
    )


def test_sandbox_audit_records_are_versioned_hash_chained_and_secret_free(tmp_path):
    path = tmp_path / "audit" / "worker.jsonl"
    audit = _audit(path)
    audit.emit(session_id="session-1", state="create_requested")
    audit.emit(session_id="session-1", state="ready")
    audit.emit(
        session_id="session-1",
        state="request_completed",
        request_sequence=1,
        strategy_id="eth-maker",
        strategy_version="2.1",
        data_fingerprint="sha256:bars-001",
        response_sha256="d" * 64,
    )
    audit.emit(session_id="session-1", state="destroyed")

    records = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    schema_path = Path(__file__).resolve().parents[2] / "schemas" / "python-strategy-sandbox-audit-v1.json"
    from jsonschema import Draft202012Validator

    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    Draft202012Validator.check_schema(schema)
    validator = Draft202012Validator(schema)
    assert [record["lifecycle_state"] for record in records] == [
        "create_requested",
        "ready",
        "request_completed",
        "destroyed",
    ]
    previous = "0" * 64
    for index, record in enumerate(records, start=1):
        validator.validate(record)
        assert record["schema_version"] == 1
        assert record["record_sequence"] == index
        assert record["provider"] == "e2b"
        assert record["network_policy"] == "deny_all"
        assert record["previous_record_sha256"] == previous
        unsigned = {key: value for key, value in record.items() if key != "record_sha256"}
        canonical = json.dumps(
            unsigned, sort_keys=True, separators=(",", ":"), ensure_ascii=True
        ).encode("utf-8")
        previous = hashlib.sha256(canonical).hexdigest()
        assert record["record_sha256"] == previous
        assert "api_key" not in json.dumps(record).lower()

    assert records[2]["data_fingerprint"] == "sha256:bars-001"
    assert records[2]["host_worker_id"] == "strategy-worker-01"
    assert records[2]["strategy_id"] == "eth-maker"
    assert records[2]["strategy_version"] == "2.1"
    assert records[2]["response_sha256"] == "d" * 64
    assert records[2]["resource_limits"]["memory_bytes"] == 2_147_483_648


def test_sandbox_audit_requires_a_host_path_when_policy_marks_audit_required(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setenv("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED", "true")
    monkeypatch.delenv("QX_STRATEGY_SANDBOX_AUDIT_PATH", raising=False)

    with pytest.raises(RuntimeError, match="no host audit path"):
        SandboxExecutionAudit.from_environment(
            provider="opensandbox",
            job_id="job-1",
            package_version="0.2.2",
            entry_sha256="a" * 64,
            bundle_sha256="b" * 64,
            spec_sha256="c" * 64,
            resource_limits={
                "cpu_count": 1,
                "memory_bytes": 1024,
                "request_timeout_ms": 15_000,
                "session_ttl_seconds": 60,
                "max_request_bytes": 1024,
                "max_response_bytes": 1024,
            },
            network_policy="deny_all",
            runtime_ref="image@sha256:" + "e" * 64,
        )


def test_request_fingerprint_reader_ignores_payload_contents():
    assert request_data_fingerprint(
        '{"data_fingerprint":"sha256:bars-001","account":"secret"}'
    ) == "sha256:bars-001"
    assert request_data_fingerprint('{"event":"bar"}') is None
    assert request_data_fingerprint("not-json") is None
    assert request_identity(
        '{"strategy_id":"eth-maker","strategy_version":"2.1","account":"secret"}'
    ) == ("eth-maker", "2.1")
    assert request_identity("not-json") == (None, None)
