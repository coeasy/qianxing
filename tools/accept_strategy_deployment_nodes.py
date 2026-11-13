#!/usr/bin/env python3
"""Exercise local two-node mTLS deployment snapshot publishing and aggregation."""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import socket
import ssl
import subprocess
import time
import uuid
from pathlib import Path
from typing import Any


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def openssl(binary: Path, *args: str) -> None:
    result = subprocess.run(
        [str(binary), *args], stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, timeout=30, check=False,
    )
    if result.returncode:
        raise RuntimeError(f"openssl failed ({result.returncode}): {result.stdout[-2000:]}")


def make_identities(binary: Path, directory: Path) -> dict[str, dict[str, str]]:
    directory.mkdir(parents=True, exist_ok=True)
    req_config = directory / "openssl.cnf"
    req_config.write_text(
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3_ca\n"
        "[dn]\nCN=Qianxing R14 local acceptance\n"
        "[v3_ca]\nbasicConstraints=critical,CA:TRUE\n"
        "keyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n",
        encoding="ascii",
    )
    ca_key, ca_cert = directory / "ca.key", directory / "ca.pem"
    openssl(
        binary, "req", "-config", str(req_config), "-x509", "-newkey", "rsa:2048",
        "-nodes", "-days", "2", "-subj", "/CN=Qianxing R14 Local CA",
        "-keyout", str(ca_key), "-out", str(ca_cert),
    )
    identities: dict[str, dict[str, str]] = {}
    for name, common_name in (
        ("aggregator", "localhost"),
        ("east", "node-east-operator"),
        ("west", "node-west-operator"),
        ("reader", "aggregator-reader"),
        ("unregistered", "unregistered-operator"),
    ):
        key, csr, cert = (directory / f"{name}.key", directory / f"{name}.csr", directory / f"{name}.pem")
        openssl(
            binary, "req", "-config", str(req_config), "-newkey", "rsa:2048", "-nodes",
            "-subj", f"/CN={common_name}", "-keyout", str(key), "-out", str(csr),
        )
        ext = directory / f"{name}-ext.cnf"
        ext.write_text(
            "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth,clientAuth\n",
            encoding="ascii",
        )
        openssl(
            binary, "x509", "-req", "-in", str(csr), "-CA", str(ca_cert), "-CAkey", str(ca_key),
            "-CAcreateserial", "-days", "2", "-extfile", str(ext), "-out", str(cert),
        )
        identities[name] = {"cert": str(cert.resolve()), "key": str(key.resolve())}
    identities["ca"] = {"cert": str(ca_cert.resolve())}
    return identities


def tls_context(ca: Path, identity: dict[str, str]) -> ssl.SSLContext:
    context = ssl.create_default_context(cafile=str(ca))
    context.load_cert_chain(identity["cert"], identity["key"])
    return context


def request(
    port: int,
    context: ssl.SSLContext,
    *,
    method: str = "GET",
    path: str = "/strategies/deployment-nodes",
    body: dict[str, Any] | None = None,
) -> tuple[int, bytes]:
    encoded = None if body is None else json.dumps(body, separators=(",", ":")).encode()
    headers = {"Accept": "application/json"}
    if encoded is not None:
        headers["Content-Type"] = "application/json"
    connection = http.client.HTTPSConnection("127.0.0.1", port, context=context, timeout=5)
    try:
        connection.request(method, path, body=encoded, headers=headers)
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


def wait_listening(port: int, process: subprocess.Popen[bytes], label: str) -> None:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{label} exited early with code {process.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"{label} did not bind port {port}")


def set_api_identity(config: dict[str, Any], identity: str, cert: str) -> None:
    config["api"]["operators"] = {identity: {"permission": "ReadOnly", "certificate": cert}}


def start_api(
    binary: Path,
    config_path: Path,
    port: int,
    log_path: Path,
) -> tuple[subprocess.Popen[bytes], Any]:
    log = log_path.open("wb")
    startup = subprocess.STARTUPINFO()
    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = subprocess.SW_HIDE
    process = subprocess.Popen(
        [str(binary), "serve", str(config_path)],
        stdout=log,
        stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        startupinfo=startup,
    )
    wait_listening(port, process, f"API {config_path.stem}")
    return process, log


def stop_process(process: subprocess.Popen[bytes]) -> int | None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=4)
    return process.returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--runtime-template", required=True, type=Path)
    parser.add_argument("--openssl", required=True, type=Path)
    parser.add_argument("--evidence-out", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    binary, template, openssl_bin = args.binary.resolve(), args.runtime_template.resolve(), args.openssl.resolve()
    evidence_path = (root / args.evidence_out).resolve()
    require(binary.is_file() and template.is_file() and openssl_bin.is_file(), "binary, template, and OpenSSL must exist")
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex
    work = root / "target" / f"strategy-deployment-nodes-{run_id}"
    work.mkdir(parents=True)
    identities = make_identities(openssl_bin, work / "tls")
    ca = Path(identities["ca"]["cert"])
    aggregator_port, east_port, west_port = free_port(), free_port(), free_port()
    while len({aggregator_port, east_port, west_port}) < 3:
        east_port, west_port = free_port(), free_port()

    base = json.loads(template.read_text(encoding="utf-8"))
    base["environment"] = "paper"
    base["profile"] = "single_node"
    base["storage"]["backend"] = "files"
    base["storage"]["consistency"] = "local_durable"
    base["storage"]["sqlite_path"] = None
    base["storage"]["data_dir"] = str((work / "unused-data").resolve())
    base["workers"] = [
        {"id": "api-r14", "role": "api", "enabled": True, "account_id": None, "venue_id": None,
         "endpoint": None, "symbols": [], "credential_env": None},
        {"id": "strategy-r14", "role": "strategy", "enabled": True, "account_id": "main", "venue_id": "paper",
         "endpoint": None, "symbols": ["BTCUSDT.BINANCE"], "credential_env": None},
    ]
    base["strategy"]["id"] = "strategy-r14"
    base["strategy"]["account_id"] = "main"
    base["strategy"]["venue_id"] = "paper"
    base["strategy"]["instrument"] = "BTCUSDT.BINANCE"
    base["strategy"]["live_enabled"] = False

    agg = json.loads(json.dumps(base))
    agg["api"]["bind"] = f"127.0.0.1:{aggregator_port}"
    agg["api"]["transport"] = "mtls"
    agg["api"]["tls"] = {"certificate_chain": identities["aggregator"]["cert"],
                          "private_key": identities["aggregator"]["key"], "client_ca": str(ca)}
    agg["api"]["strategy_deployment_nodes"] = [
        {"node_id": "node-east", "operator_id": "node-east-publisher", "freshness_window_ms": 5000},
        {"node_id": "node-west", "operator_id": "node-west-publisher", "freshness_window_ms": 5000},
    ]
    agg["api"]["strategy_deployment_publisher"] = None
    agg["api"]["console"] = None
    agg["api"]["operators"] = {
        "node-east-publisher": {"permission": "ReadOnly", "certificate": identities["east"]["cert"]},
        "node-west-publisher": {"permission": "ReadOnly", "certificate": identities["west"]["cert"]},
        "aggregator-reader": {"permission": "ReadOnly", "certificate": identities["reader"]["cert"]},
    }
    agg["storage"]["data_dir"] = str((work / "aggregator-data").resolve())
    agg_path = work / "aggregator.json"
    agg_path.write_text(json.dumps(agg, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    node_paths: dict[str, Path] = {}
    for node, identity, operator_id, port in (
        ("east", "east", "node-east-publisher", east_port),
        ("west", "west", "node-west-publisher", west_port),
    ):
        config = json.loads(json.dumps(base))
        config["api"]["bind"] = f"127.0.0.1:{port}"
        config["api"]["transport"] = "mtls"
        config["api"]["tls"] = {"certificate_chain": identities[identity]["cert"],
                                "private_key": identities[identity]["key"], "client_ca": str(ca)}
        set_api_identity(config, operator_id, identities[identity]["cert"])
        config["api"]["strategy_deployment_nodes"] = []
        config["api"]["strategy_deployment_publisher"] = {
            "server_host": "127.0.0.1", "server_port": aggregator_port, "server_ca": str(ca),
            "client_certificate_chain": identities[identity]["cert"],
            "client_private_key": identities[identity]["key"], "interval_ms": 1000,
        }
        config["api"]["console"] = None
        config["storage"]["data_dir"] = str((work / f"{node}-data").resolve())
        path = work / f"{node}.json"
        path.write_text(json.dumps(config, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        node_paths[node] = path

    processes: dict[str, subprocess.Popen[bytes]] = {}
    logs: dict[str, Any] = {}
    return_codes: dict[str, int | None] = {}
    evidence: dict[str, Any] = {
        "schema_version": 1,
        "kind": "qianxing_strategy_deployment_nodes_mtls_acceptance",
        "run_id": run_id,
        "environment": "windows_local_loopback_mtls",
        "registered_nodes": ["node-east", "node-west"],
        "registered_operators": {"node-east": "node-east-publisher", "node-west": "node-west-publisher"},
        "effects": {"external_network_access": False, "venue_worker_started": False, "orders_sent": False},
        "checks": {},
    }

    def start(name: str, path: Path, port: int) -> None:
        process, log = start_api(binary, path, port, work / f"{name}-{uuid.uuid4().hex[:6]}.log")
        processes[name], logs[name] = process, log

    def stop(name: str) -> None:
        process = processes.pop(name, None)
        if process is not None:
            return_codes[name] = stop_process(process)
            logs.pop(name).close()

    def statuses() -> list[dict[str, Any]]:
        status, payload = request(aggregator_port, tls_context(ca, identities["reader"]))
        require(status == 200, f"aggregator GET returned HTTP {status}: {payload[:500]!r}")
        value = json.loads(payload)
        require(isinstance(value, list), "node projection is not an array")
        return value

    def wait_fresh(node_id: str, expected: bool, timeout: float = 15) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            state = next((item for item in statuses() if item.get("node_id") == node_id), None)
            if state and (state.get("freshness") == "fresh") == expected:
                return state
            time.sleep(0.15)
        raise RuntimeError(f"node {node_id} did not reach freshness expected={expected}")

    try:
        start("aggregator", agg_path, aggregator_port)
        start("east", node_paths["east"], east_port)
        start("west", node_paths["west"], west_port)
        east_fresh = wait_fresh("node-east", True)
        west_fresh = wait_fresh("node-west", True)
        same_worker = [
            [row.get("worker_id") for row in node.get("deployments", [])]
            for node in (east_fresh, west_fresh)
        ]
        require(same_worker == [["strategy-r14"], ["strategy-r14"]], f"equal worker IDs were merged or missing: {same_worker}")
        require(east_fresh.get("authenticated_operator_id") == "node-east-publisher", "east mTLS identity mapping mismatch")
        require(west_fresh.get("authenticated_operator_id") == "node-west-publisher", "west mTLS identity mapping mismatch")
        evidence["checks"]["mTLS_publish_receive_identity"] = "passed"
        evidence["checks"]["same_worker_id_remains_node_scoped"] = True

        publisher_context = tls_context(ca, identities["east"])
        now_ms = int(time.time() * 1000)
        invalid_claim = {"schema_version": 1, "generated_at_ms": now_ms, "deployments": [], "node_id": "node-west"}
        claim_status, _ = request(aggregator_port, publisher_context, method="POST", body=invalid_claim)
        require(claim_status == 400, f"payload node identity claim was not rejected: HTTP {claim_status}")
        future_status, _ = request(
            aggregator_port, tls_context(ca, identities["east"]), method="POST",
            body={"schema_version": 1, "generated_at_ms": int(time.time() * 1000) + 60_000, "deployments": []},
        )
        require(future_status == 400, f"future snapshot was not rejected: HTTP {future_status}")
        east_current = next(item for item in statuses() if item["node_id"] == "node-east")
        replay_ts = int(east_current["source_observed_at_ms"]) - 1
        replay_status, _ = request(
            aggregator_port, tls_context(ca, identities["east"]), method="POST",
            body={"schema_version": 1, "generated_at_ms": replay_ts, "deployments": []},
        )
        require(replay_status == 409, f"replayed old snapshot was not rejected: HTTP {replay_status}")
        unregistered_status, _ = request(
            aggregator_port, tls_context(ca, identities["unregistered"]), method="POST",
            body={"schema_version": 1, "generated_at_ms": int(time.time() * 1000), "deployments": []},
        )
        require(unregistered_status == 403, f"unregistered mTLS identity was not rejected: HTTP {unregistered_status}")
        evidence["checks"]["body_identity_claim_http_status"] = claim_status
        evidence["checks"]["future_snapshot_http_status"] = future_status
        evidence["checks"]["replay_snapshot_http_status"] = replay_status
        evidence["checks"]["unregistered_identity_http_status"] = unregistered_status

        stop("west")
        west_stale = wait_fresh("node-west", False, timeout=8)
        east_still_fresh = next(item for item in statuses() if item["node_id"] == "node-east")
        require(east_still_fresh.get("freshness") == "fresh", "west staleness contaminated east freshness")
        evidence["checks"]["fresh_to_stale_node_west"] = west_stale.get("freshness")
        evidence["checks"]["node_east_remained_fresh"] = True

        start("west", node_paths["west"], west_port)
        wait_fresh("node-west", True)
        evidence["checks"]["restart_publisher_restored_freshness"] = True

        stop("east")
        stop("west")
        stop("aggregator")
        start("aggregator", agg_path, aggregator_port)
        after_restart = statuses()
        require(len(after_restart) == 2, f"aggregator restart lost durable node snapshots: {after_restart}")
        require(all(item.get("source_observed_at_ms") and item.get("deployments") for item in after_restart), "aggregator restart lost node snapshot contents")
        evidence["checks"]["aggregator_restart_preserved_nodes_and_deployments"] = True
        start("east", node_paths["east"], east_port)
        start("west", node_paths["west"], west_port)
        wait_fresh("node-east", True)
        wait_fresh("node-west", True)
        evidence["outcome"] = "passed"
    except Exception as error:  # noqa: BLE001 - preserve failed runtime evidence
        evidence.update({"outcome": "failed", "error": f"{type(error).__name__}: {error}"})
    finally:
        for name in list(processes):
            stop(name)
        evidence["process_exit_codes"] = return_codes
        log_hashes: dict[str, str] = {}
        for log_path in work.glob("*.log"):
            log_hashes[log_path.name] = hashlib.sha256(log_path.read_bytes()).hexdigest()
        evidence["log_sha256"] = log_hashes

    evidence_path.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in evidence.items() if key != "log_sha256"}, ensure_ascii=False))
    print(f"Evidence: {evidence_path.relative_to(root).as_posix()}")
    return 0 if evidence.get("outcome") == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
