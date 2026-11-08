#!/usr/bin/env python3
"""Validate two-node mTLS strategy deployment aggregation acceptance evidence."""

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
    args = parser.parse_args()
    evidence: dict[str, Any] = json.loads(args.evidence.read_text(encoding="utf-8"))
    require(evidence.get("schema_version") == 1, "unsupported evidence schema")
    require(evidence.get("kind") == "qianxing_strategy_deployment_nodes_mtls_acceptance", "wrong evidence kind")
    require(evidence.get("outcome") == "passed", "two-node acceptance did not pass")
    require(evidence.get("environment") == "windows_local_loopback_mtls", "unexpected test environment")
    require(evidence.get("registered_nodes") == ["node-east", "node-west"], "registered node identities differ")
    require(evidence.get("registered_operators") == {
        "node-east": "node-east-publisher",
        "node-west": "node-west-publisher",
    }, "registered operator mapping differs")
    require(evidence.get("effects") == {
        "external_network_access": False,
        "venue_worker_started": False,
        "orders_sent": False,
    }, "unexpected external effects")
    checks = evidence.get("checks", {})
    require(checks.get("mTLS_publish_receive_identity") == "passed", "mTLS publisher identity was not verified")
    require(checks.get("same_worker_id_remains_node_scoped") is True, "equal worker IDs were merged")
    require(checks.get("body_identity_claim_http_status") == 400, "body-supplied node identity was accepted")
    require(checks.get("future_snapshot_http_status") == 400, "future snapshot was accepted")
    require(checks.get("replay_snapshot_http_status") == 409, "old snapshot replay was accepted")
    require(checks.get("unregistered_identity_http_status") == 403, "unregistered operator was accepted")
    require(checks.get("fresh_to_stale_node_west") == "stale", "Fresh-to-Stale transition was not observed")
    require(checks.get("node_east_remained_fresh") is True, "node freshness was not isolated")
    require(checks.get("restart_publisher_restored_freshness") is True, "publisher restart did not restore Fresh")
    require(checks.get("aggregator_restart_preserved_nodes_and_deployments") is True, "aggregator restart lost durable snapshots")
    require(len(evidence.get("log_sha256", {})) >= 5, "process log hashes are incomplete")
    print(f"PASS: {args.evidence} ({evidence.get('run_id')})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
