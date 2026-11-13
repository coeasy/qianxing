#!/usr/bin/env python3
"""Validate the bounded local Paper strategy deployment lifecycle evidence."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--storage-backend", choices=("files", "sqlite", "postgres"))
    args = parser.parse_args()
    evidence: dict[str, Any] = json.loads(args.evidence.read_text(encoding="utf-8"))
    require(evidence.get("schema_version") == 1, "unsupported evidence schema")
    require(evidence.get("kind") == "qianxing_strategy_deployment_lifecycle_acceptance", "wrong evidence kind")
    require(evidence.get("outcome") == "passed", "lifecycle acceptance did not pass")
    backend = args.storage_backend or evidence.get("storage_backend", "files")
    require(backend in ("files", "sqlite", "postgres"), "unsupported storage backend")
    require(evidence.get("storage_backend", backend) == backend, "storage backend mismatch")
    if backend == "postgres":
        require(
            evidence.get("runtime_profile") == "distributed"
            and evidence.get("storage_consistency") == "transactional",
            "PostgreSQL lifecycle evidence must use distributed/transactional runtime configuration",
        )
    require(
        evidence.get("environment")
        in {
            f"{system}_local_{backend}_paper_loopback"
            for system in ("windows", "linux", "darwin")
        }
        or (backend == "files" and evidence.get("environment") == "windows_local_paper_loopback"),
        "unexpected acceptance environment",
    )
    require(evidence.get("effects") == {
        "venue_worker_started": False,
        "orders_sent": False,
        "external_network_access": False,
    }, "unexpected external effects")
    operations = evidence.get("operations")
    require(isinstance(operations, list), "operations must be an array")
    expected = [("deploy", 1), ("pause", 1), ("resume", 1), ("rollback", 2), ("deploy", 3), ("stop", 4)]
    actual = [(item.get("action"), item.get("target_revision")) for item in operations]
    require(actual == expected, f"unexpected lifecycle sequence: {actual}")
    for index, operation in enumerate(operations):
        expected_state = "failed" if index == 4 else "succeeded"
        require(operation.get("state") == expected_state, f"unexpected operation result: {operation}")
    require("restored" in str(operations[4].get("error_message", "")).lower(), "failure restoration was not confirmed")
    final = evidence.get("final_projection")
    require(isinstance(final, dict), "final projection is missing")
    require(final.get("state") == "stopped" and final.get("desired_running") is False, "final worker is not stopped")
    require(final.get("desired_revision") == 4, "final stopped revision is not 4")
    require(evidence.get("restored_revision") == 2, "previous revision 2 was not restored")
    if evidence.get("supervisor_restarted"):
        restart = evidence.get("restart_projection")
        require(
            isinstance(evidence.get("supervisor_stopped_gracefully"), bool)
            and isinstance(evidence.get("supervisor_exit_code_before_restart"), int),
            "supervisor shutdown result is missing",
        )
        require(isinstance(restart, dict), "restart projection is missing")
        restart_observation = restart.get("observation")
        require(
            restart.get("desired_revision") == 4
            and restart.get("desired_running") is False
            and isinstance(restart_observation, dict)
            and restart_observation.get("phase") == "stopped",
            "restart did not restore the durable stopped deployment",
        )
    checkpoint_restart = evidence.get("checkpoint_restart")
    if checkpoint_restart is not None:
        require(isinstance(checkpoint_restart, dict), "checkpoint restart evidence must be an object")
        require(
            checkpoint_restart.get("supervisor_stopped_gracefully") is True
            and isinstance(checkpoint_restart.get("supervisor_exit_code"), int),
            "checkpoint supervisor shutdown result is missing or unclean",
        )
        before_pid = checkpoint_restart.get("before_process_id")
        after_pid = checkpoint_restart.get("after_process_id")
        require(
            isinstance(before_pid, int) and isinstance(after_pid, int) and before_pid != after_pid,
            "checkpoint evidence must show a different strategy process after supervisor restart",
        )
        require(
            checkpoint_restart.get("checkpoint_input_supplied") is True
            and checkpoint_restart.get("checkpoint_roundtrip_observed") is True,
            "restarted strategy did not report checkpoint input and roundtrip",
        )
        projection = checkpoint_restart.get("projection")
        require(isinstance(projection, dict), "checkpoint restart projection is missing")
        observation = projection.get("observation")
        lifecycle = observation.get("host_lifecycle") if isinstance(observation, dict) else None
        require(
            isinstance(observation, dict)
            and observation.get("process_id") == after_pid
            and observation.get("checkpoint_roundtrip_observed") is True
            and isinstance(lifecycle, dict)
            and lifecycle.get("checkpoint_input_supplied") is True,
            "checkpoint restart projection does not match the recorded lifecycle",
        )
        restore_records = checkpoint_restart.get("restore_records")
        require(
            isinstance(restore_records, list)
            and len(restore_records) == 1
            and isinstance(restore_records[0], dict)
            and isinstance(restore_records[0].get("bar_count"), int)
            and restore_records[0]["bar_count"] > 0,
            "strategy on_restore callback did not report restored state",
        )
    require(evidence.get("api_readiness_assessed") is False, "paper lifecycle evidence cannot claim readiness")
    print(f"PASS: {args.evidence} ({evidence.get('run_id')})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
