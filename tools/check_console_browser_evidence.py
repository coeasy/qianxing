#!/usr/bin/env python3
"""Validate a manually observed packaged-console browser acceptance record."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    evidence: dict[str, Any] = json.loads(args.evidence.read_text(encoding="utf-8"))

    require(evidence.get("kind") == "qianxing_console_browser_acceptance", "wrong evidence kind")
    require(evidence.get("observation_method") == "codex_in_app_browser_accessibility_tree", "browser observation method is missing")
    require(evidence.get("bootstrap_token_recorded") is False, "bootstrap token must not be persisted")
    require(evidence.get("browser", {}).get("origin") == "http://127.0.0.1", "browser evidence is not loopback-only")
    require(evidence.get("browser", {}).get("connected") is True, "console UI did not connect")

    revision = evidence.get("revision", {})
    require(revision.get("http_status") == 201, "revision creation was not accepted")
    require(revision.get("created_by") == evidence.get("operator_id"), "revision operator identity is not bound")

    operation = evidence.get("operation", {})
    require(operation.get("http_status") == 200, "console BFF did not return the operation result")
    require(operation.get("action") == "deploy" and operation.get("state") == "accepted", "deploy operation is not visibly accepted")
    require(bool(operation.get("operation_id")), "operation ID is missing")

    projection = evidence.get("deployment_projection", {})
    require(projection.get("desired_revision") == revision.get("revision"), "desired revision is not visible in the browser projection")
    require(projection.get("desired_running") is True, "desired running state is not visible")
    require(projection.get("observed") is None, "fixture unexpectedly claims worker observation")
    require(projection.get("trading_enabled") is None, "isolated browser acceptance must remain fail-closed")

    readiness = evidence.get("readiness", {})
    require(readiness.get("http_status") == 503 and readiness.get("ready") is False, "isolated readiness unexpectedly reports ready")
    require(evidence.get("effects", {}).get("venue_worker_started") is False, "acceptance must not start a venue worker")
    require(evidence.get("effects", {}).get("supervisor_started") is False, "acceptance must not start a supervisor")

    print(
        "passed: packaged browser login/connect, revision create, deploy operation readback, "
        "deployment projection, and fail-closed readiness"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
