#!/usr/bin/env python3
"""Exercise the packaged same-origin console BFF and its deployment read/write routes."""

from __future__ import annotations

import argparse
import hashlib
import http.cookiejar
import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any


def free_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def request(
    opener: urllib.request.OpenerDirector,
    url: str,
    *,
    method: str = "GET",
    body: dict[str, Any] | None = None,
    headers: dict[str, str] | None = None,
) -> tuple[int, bytes]:
    encoded = None if body is None else json.dumps(body, separators=(",", ":")).encode()
    request_headers = {"Accept": "application/json, text/html", **(headers or {})}
    if encoded is not None:
        request_headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=encoded, headers=request_headers, method=method)
    try:
        with opener.open(req, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def decode_json(payload: bytes) -> Any:
    return json.loads(payload.decode("utf-8"))


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--runtime-template", required=True, type=Path)
    parser.add_argument("--evidence-out", required=True, type=Path)
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    binary = args.binary.resolve()
    template = args.runtime_template.resolve()
    evidence_path = (root / args.evidence_out).resolve()
    require(binary.is_file(), f"qx-cli binary not found: {binary}")
    require(template.is_file(), f"runtime template not found: {template}")
    evidence_path.parent.mkdir(parents=True, exist_ok=True)

    run_id = uuid.uuid4().hex
    work_dir = root / "target" / f"console-bff-acceptance-{run_id}"
    work_dir.mkdir(parents=True)
    api_port, console_port = free_loopback_port(), free_loopback_port()
    while api_port == console_port:
        console_port = free_loopback_port()

    config = json.loads(template.read_text(encoding="utf-8"))
    config["api"]["transport"] = "mtls"
    config["api"]["tls"] = {
        "certificate_chain": "unused-console-acceptance.pem",
        "private_key": "unused-console-acceptance.key",
        "client_ca": "unused-console-acceptance-ca.pem",
    }
    config["api"]["operators"] = {
        "console-operator": {
            "permission": "Admin",
            "certificate": "console-acceptance-client-cert",
        }
    }
    config["workers"].append(
        {
            "id": "strategy-console-fixture",
            "role": "strategy",
            "enabled": True,
            "account_id": "main",
            "venue_id": "binance-testnet",
            "endpoint": None,
            "symbols": ["BTCUSDT.BINANCE"],
            "credential_env": None,
        }
    )
    config["strategy"] = {
        "version": "console-acceptance-v1",
        "max_orders": 1,
        "account_id": "main",
        "venue_id": "binance-testnet",
        "instrument": "BTCUSDT.BINANCE",
        "target_qty": 0,
        "builtin_strategy": "ema_cross",
        "bars_snapshot_path": str((root / "deploy" / "qianxing.bar-frame.example.json").resolve()),
    }
    config["api"]["bind"] = f"127.0.0.1:{api_port}"
    config["api"]["console"]["bind"] = f"127.0.0.1:{console_port}"
    config["api"]["console"]["static_dir"] = str((root / "web" / "console").resolve())
    config["storage"]["data_dir"] = str((work_dir / "data").resolve())
    runtime_path = work_dir / "runtime.json"
    runtime_text = json.dumps(config, ensure_ascii=False, indent=2) + "\n"
    runtime_path.write_text(runtime_text, encoding="utf-8")

    token = f"r18-local-{uuid.uuid4().hex}-{uuid.uuid4().hex}"
    token_env = config["api"]["console"]["bootstrap_token_env"]
    child_env = os.environ.copy()
    child_env[token_env] = token
    log_path = work_dir / "console.log"
    process: subprocess.Popen[bytes] | None = None
    result: dict[str, Any] = {
        "schema_version": 1,
        "kind": "qianxing_console_bff_acceptance",
        "run_id": run_id,
        "environment": "isolated_windows_local_loopback",
        "operator_id": config["api"]["console"]["operator"],
        "effects": "local revision/operation records only; no worker supervisor or venue process is started",
    }
    try:
        with log_path.open("wb") as log_file:
            process = subprocess.Popen(
                [str(binary), "console", str(runtime_path)],
                cwd=root,
                env=child_env,
                stdout=log_file,
                stderr=subprocess.STDOUT,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )

        base = f"http://127.0.0.1:{console_port}"
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        login_status = 0
        login_body = b""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"console exited early with code {process.returncode}")
            login_status, login_body = request(opener, f"{base}/?token={token}")
            if login_status == 200:
                break
            time.sleep(0.2)
        require(login_status == 200, f"console login returned HTTP {login_status}")
        require(b"<html" in login_body.lower(), "console login did not return the packaged HTML page")

        cookies = {cookie.name: cookie.value for cookie in jar}
        csrf = cookies.get("qx_console_csrf", "")
        require(bool(cookies.get("qx_console_session")), "login did not issue a session cookie")
        require(bool(csrf), "login did not issue a CSRF cookie")

        deployment_url = f"{base}/strategies/deployments?worker_id={urllib.parse.quote(next(w['id'] for w in config['workers'] if w['role'] == 'strategy'))}"
        read_status, read_bytes = request(opener, deployment_url)
        require(read_status == 200, f"authorized deployment read returned HTTP {read_status}")
        deployments = decode_json(read_bytes)
        require(isinstance(deployments, list) and deployments, "deployment read returned no strategy worker")
        worker_id = deployments[0]["worker_id"]

        operation_url = f"{base}/strategies/deployments/operations"
        csrf_status, csrf_bytes = request(
            opener,
            operation_url,
            method="POST",
            body={},
            headers={"Origin": base},
        )
        require(
            csrf_status == 403 and b"console_csrf_token_invalid" in csrf_bytes,
            f"write without the CSRF header returned HTTP {csrf_status}",
        )

        binary_digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        plan_digest = hashlib.sha256(f"{run_id}:plan".encode()).hexdigest()
        revision_status, revision_bytes = request(
            opener,
            f"{base}/strategies/deployments/revisions",
            method="POST",
            headers={"Origin": base, "X-QX-CSRF": csrf},
            body={
                "worker_id": worker_id,
                "revision": 1,
                "bundle_sha256": binary_digest,
                "plan_sha256": plan_digest,
                "runtime_config_json": runtime_text,
                "desired_running": True,
            },
        )
        require(revision_status == 201, f"authorized revision write returned HTTP {revision_status}: {revision_bytes[:1000]!r}")
        revision = decode_json(revision_bytes)
        require(revision.get("created_by") == result["operator_id"], "revision identity was not bound to the authenticated console operator")

        operation_id = f"console-acceptance-{run_id}"
        operation_status, operation_bytes = request(
            opener,
            operation_url,
            method="POST",
            headers={"Origin": base, "X-QX-CSRF": csrf},
            body={
                "operation_id": operation_id,
                "idempotency_key": operation_id,
                "worker_id": worker_id,
                "action": "deploy",
                "target_revision": 1,
            },
        )
        require(operation_status == 202, f"authorized operation write returned HTTP {operation_status}: {operation_bytes[:1000]!r}")
        operation = decode_json(operation_bytes)
        require(operation.get("state") == "accepted", "deployment operation did not remain visibly accepted")

        operation_read_status, operation_read_bytes = request(
            opener,
            f"{operation_url}?operation_id={urllib.parse.quote(operation_id)}",
        )
        require(operation_read_status == 200, f"authorized operation read returned HTTP {operation_read_status}")
        operation_read = decode_json(operation_read_bytes)

        projection_status, projection_bytes = request(opener, deployment_url)
        require(projection_status == 200, f"deployment projection returned HTTP {projection_status}")
        projection = decode_json(projection_bytes)[0]
        require(projection.get("desired_revision") == 1, "deployment projection omitted the accepted desired revision")
        require(projection.get("trading_enabled") is None, "local acceptance must not fabricate live trading readiness")

        readiness_status, readiness_bytes = request(opener, f"{base}/ready")
        readiness = decode_json(readiness_bytes)
        require("readiness_layers" in readiness, "readiness endpoint omitted layer projection")
        require(readiness.get("ready") is False, "isolated paper configuration unexpectedly claimed production readiness")

        result.update(
            {
                "outcome": "passed",
                "login_http_status": login_status,
                "session_cookie_issued": True,
                "csrf_cookie_issued": True,
                "authorized_deployment_read_http_status": read_status,
                "csrf_rejection_http_status": csrf_status,
                "revision_create_http_status": revision_status,
                "revision_created_by": revision.get("created_by"),
                "operation_create_http_status": operation_status,
                "operation_state_after_acceptance": operation_read.get("state"),
                "operation_read_http_status": operation_read_status,
                "deployment_projection_http_status": projection_status,
                "desired_revision": projection.get("desired_revision"),
                "trading_enabled": projection.get("trading_enabled"),
                "readiness_http_status": readiness_status,
                "readiness_ready": readiness.get("ready"),
                "readiness_layer_statuses": {
                    key: value.get("status")
                    for key, value in readiness.get("readiness_layers", {}).items()
                },
            }
        )
    except Exception as error:  # noqa: BLE001 - preserve acceptance failure evidence
        result.update({"outcome": "failed", "error": f"{type(error).__name__}: {error}"})
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if log_path.exists():
            log_digest = hashlib.sha256(log_path.read_bytes()).hexdigest()
            result["console_log_sha256"] = log_digest

    evidence_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in result.items() if key != "console_log_sha256"}, ensure_ascii=False))
    print(f"Evidence: {evidence_path.relative_to(root).as_posix()}")
    return 0 if result.get("outcome") == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
