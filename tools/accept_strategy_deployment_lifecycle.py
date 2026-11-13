#!/usr/bin/env python3
"""Exercise the supervisor deployment lifecycle using isolated Paper workers."""

from __future__ import annotations

import argparse
import hashlib
import http.cookiejar
import json
import os
import platform
import signal
import socket
import ssl
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


def free_loopback_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def openssl(binary: Path, *args: str) -> None:
    result = subprocess.run(
        [str(binary), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"openssl failed ({result.returncode}): {result.stdout[-2000:]}")


def make_local_mtls(openssl_bin: Path, folder: Path) -> dict[str, str]:
    folder.mkdir(parents=True, exist_ok=True)
    openssl_config = folder / "openssl.cnf"
    openssl_config.write_text(
        "[req]\nprompt=no\ndistinguished_name=dn\n[dn]\nCN=Qianxing local acceptance\n",
        encoding="ascii",
    )
    ca_key, ca_cert = folder / "ca.key", folder / "ca.pem"
    openssl(
        openssl_bin,
        "req", "-config", str(openssl_config), "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
        "-subj", "/CN=Qianxing R09 Local Test CA",
        "-keyout", str(ca_key), "-out", str(ca_cert),
    )

    server_key, server_csr, server_cert = (
        folder / "server.key", folder / "server.csr", folder / "server.pem"
    )
    openssl(
        openssl_bin,
        "req", "-config", str(openssl_config), "-newkey", "rsa:2048", "-nodes",
        "-subj", "/CN=localhost",
        "-keyout", str(server_key), "-out", str(server_csr),
    )
    server_ext = folder / "server-ext.cnf"
    server_ext.write_text(
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n",
        encoding="ascii",
    )
    openssl(
        openssl_bin,
        "x509", "-req", "-in", str(server_csr), "-CA", str(ca_cert),
        "-CAkey", str(ca_key), "-CAcreateserial", "-days", "2",
        "-extfile", str(server_ext), "-out", str(server_cert),
    )

    operator_key, operator_csr, operator_cert = (
        folder / "operator.key", folder / "operator.csr", folder / "operator.pem"
    )
    openssl(
        openssl_bin,
        "req", "-config", str(openssl_config), "-newkey", "rsa:2048", "-nodes",
        "-subj", "/CN=console-operator",
        "-keyout", str(operator_key), "-out", str(operator_csr),
    )
    operator_ext = folder / "operator-ext.cnf"
    operator_ext.write_text("extendedKeyUsage=clientAuth\n", encoding="ascii")
    openssl(
        openssl_bin,
        "x509", "-req", "-in", str(operator_csr), "-CA", str(ca_cert),
        "-CAkey", str(ca_key), "-CAcreateserial", "-days", "2",
        "-extfile", str(operator_ext), "-out", str(operator_cert),
    )
    return {
        "ca": str(ca_cert.resolve()),
        "server_cert": str(server_cert.resolve()),
        "server_key": str(server_key.resolve()),
        "operator_cert": str(operator_cert.resolve()),
    }


def http_request(
    opener: urllib.request.OpenerDirector,
    url: str,
    *,
    method: str = "GET",
    body: dict[str, Any] | None = None,
    headers: dict[str, str] | None = None,
) -> tuple[int, Any]:
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


def json_body(payload: Any) -> Any:
    return json.loads(payload.decode("utf-8"))


def wait_listener(port: int, process: subprocess.Popen[bytes], label: str, timeout: float = 30) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{label} exited early with code {process.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.25):
                return
        except OSError:
            time.sleep(0.15)
    raise RuntimeError(f"{label} did not bind loopback port {port}")


def stop_supervisor(process: subprocess.Popen[bytes], timeout: float = 30) -> tuple[int, bool]:
    if process.poll() is not None:
        return int(process.returncode or 0), True
    try:
        if process.stdin is not None:
            process.stdin.close()
        else:
            process.send_signal(signal.CTRL_BREAK_EVENT if os.name == "nt" else signal.SIGINT)
    except OSError:
        process.terminate()
    try:
        return int(process.wait(timeout=timeout)), True
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            return int(process.wait(timeout=5)), False
        except subprocess.TimeoutExpired:
            process.kill()
            return int(process.wait(timeout=5)), False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--runtime-template", required=True, type=Path)
    parser.add_argument("--openssl", required=True, type=Path)
    parser.add_argument("--evidence-out", required=True, type=Path)
    parser.add_argument(
        "--storage-backend", choices=("files", "sqlite", "postgres"), default="files"
    )
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    binary, template, openssl_bin = args.binary.resolve(), args.runtime_template.resolve(), args.openssl.resolve()
    evidence_path = (root / args.evidence_out).resolve()
    require(binary.is_file(), f"qx-cli binary not found: {binary}")
    require(template.is_file(), f"runtime template not found: {template}")
    require(openssl_bin.is_file(), f"OpenSSL binary not found: {openssl_bin}")
    evidence_path.parent.mkdir(parents=True, exist_ok=True)

    run_id = uuid.uuid4().hex
    work_dir = root / "target" / f"strategy-deployment-lifecycle-{run_id}"
    work_dir.mkdir(parents=True)
    tls = make_local_mtls(openssl_bin, work_dir / "tls")
    api_port, console_port = free_loopback_port(), free_loopback_port()
    while api_port == console_port:
        console_port = free_loopback_port()

    config = json.loads(template.read_text(encoding="utf-8"))
    config["api"]["bind"] = f"127.0.0.1:{api_port}"
    config["api"]["transport"] = "mtls"
    config["api"]["tls"] = {
        "certificate_chain": tls["server_cert"],
        "private_key": tls["server_key"],
        "client_ca": tls["ca"],
    }
    config["api"]["operators"] = {
        "console-operator": {"permission": "Admin", "certificate": tls["operator_cert"]}
    }
    config["api"]["console"] = {
        "bind": f"127.0.0.1:{console_port}",
        "static_dir": str((root / "web" / "console").resolve()),
        "operator": "console-operator",
        "bootstrap_token_env": "QX_CONSOLE_BOOTSTRAP_TOKEN",
        "session_ttl_seconds": 3600,
    }
    config["storage"]["backend"] = args.storage_backend
    config["storage"]["data_dir"] = str((work_dir / "data").resolve())
    if args.storage_backend == "sqlite":
        config["storage"]["sqlite_path"] = str((work_dir / "control.sqlite").resolve())
    elif args.storage_backend == "postgres":
        require(
            bool(os.environ.get("QX_TEST_POSTGRES_DSN")),
            "QX_TEST_POSTGRES_DSN is required for PostgreSQL lifecycle acceptance",
        )
        # PostgreSQL is only valid under the explicitly distributed runtime contract.
        # The paper example defaults to single_node/local_durable, which correctly
        # rejects PostgreSQL during supervisor startup.
        config["profile"] = "distributed"
        config["storage"]["consistency"] = "transactional"
        config["storage"]["postgres_dsn_env"] = "QX_TEST_POSTGRES_DSN"
    worker_id = f"strategy-r09-{run_id[:12]}"
    strategy_fixture = root / "python" / "tests" / "fixtures" / "runtime_checkpoint_process_strategy.py"
    restore_log = work_dir / "strategy-restores.jsonl"
    first_job_id = f"checkpoint-before-restart-{run_id[:8]}"
    second_job_id = f"checkpoint-after-restart-{run_id[:8]}"
    jobs = json.loads(
        (root / "deploy" / "qianxing.scheduler.paper-order-smoke.json").read_text(
            encoding="utf-8"
        )
    )
    first_job = json.loads(json.dumps(jobs[0]))
    first_job.update({"job_id": first_job_id, "owner": worker_id, "concurrency_key": first_job_id})
    first_due = datetime.now(timezone.utc) + timedelta(minutes=1)
    first_job["trigger"] = {
        "Cron": (
            f"{first_due.minute} {first_due.hour} {first_due.day} "
            f"{first_due.month} {(first_due.weekday() + 1) % 7}"
        )
    }
    second_job = json.loads(json.dumps(first_job))
    second_job.update(
        {
            "job_id": second_job_id,
            "concurrency_key": second_job_id,
            "depends_on": [first_job_id],
            "trigger": {"Cron": "* * * * *"},
        }
    )
    jobs_path = work_dir / "jobs.json"
    jobs_path.write_text(json.dumps([first_job, second_job], ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    config["scheduler"]["jobs_path"] = str(jobs_path.resolve())
    config["scheduler"]["job_queue_path"] = str((work_dir / "data" / "job-queue").resolve())
    config["scheduler"]["tick_interval_ms"] = 250
    config["workers"] = [
        {
            "id": "api-r09",
            "role": "api",
            "enabled": True,
            "account_id": None,
            "venue_id": None,
            "endpoint": None,
            "symbols": [],
            "credential_env": None,
        },
        {
            "id": "scheduler-r09",
            "role": "scheduler",
            "enabled": True,
            "account_id": None,
            "venue_id": None,
            "endpoint": None,
            "symbols": [],
            "credential_env": None,
        },
        {
            "id": worker_id,
            "role": "strategy",
            "enabled": True,
            "account_id": "main",
            "venue_id": "paper",
            "endpoint": None,
            "symbols": ["BTCUSDT.BINANCE"],
            "credential_env": None,
        },
    ]
    config["strategy"]["id"] = worker_id
    config["strategy"]["account_id"] = "main"
    config["strategy"]["venue_id"] = "paper"
    config["strategy"]["instrument"] = "BTCUSDT.BINANCE"
    config["strategy"]["target_qty"] = 0
    config["strategy"]["version"] = "1.0"
    config["strategy"]["builtin_strategy"] = None
    config["strategy"]["python_module"] = str(strategy_fixture.resolve())
    strategy_digest = hashlib.sha256(strategy_fixture.read_bytes()).hexdigest()
    config["strategy"]["strategy_artifact_sha256"] = strategy_digest
    config["strategy"]["python_strategy_spec"] = {
        "data_level": "bar",
        "parameters": {"restore_log": str(restore_log.resolve())},
    }
    config["strategy"]["bars_snapshot_path"] = str(
        (root / "deploy" / "qianxing.bar-frame.example.json").resolve()
    )
    config["strategy"]["live_enabled"] = False
    config["shutdown_timeout_ms"] = 8000
    runtime_path = work_dir / "runtime.json"
    runtime_text = json.dumps(config, ensure_ascii=False, indent=2) + "\n"
    runtime_path.write_text(runtime_text, encoding="utf-8")

    token = f"r09-local-{uuid.uuid4().hex}-{uuid.uuid4().hex}"
    child_env = os.environ.copy()
    child_env["QX_CONSOLE_BOOTSTRAP_TOKEN"] = token
    supervisor_log = (work_dir / "supervisor.log").open("wb")
    console_log = (work_dir / "console.log").open("wb")
    supervisor: subprocess.Popen[bytes] | None = None
    console: subprocess.Popen[bytes] | None = None
    result: dict[str, Any] = {
        "schema_version": 1,
        "kind": "qianxing_strategy_deployment_lifecycle_acceptance",
        "run_id": run_id,
        "environment": f"{platform.system().lower()}_local_{args.storage_backend}_paper_loopback",
        "storage_backend": args.storage_backend,
        "runtime_profile": config.get("profile", "single_node"),
        "storage_consistency": config["storage"].get("consistency", "local_durable"),
        "operator_id": "console-operator",
        "worker_id": worker_id,
        "effects": {
            "venue_worker_started": False,
            "orders_sent": False,
            "external_network_access": False,
        },
        "operations": [],
    }

    try:
        creationflags = 0
        startupinfo = None
        if os.name == "nt":
            creationflags = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0) | getattr(
                subprocess, "CREATE_NEW_CONSOLE", 0
            )
            startupinfo = subprocess.STARTUPINFO()
            startupinfo.dwFlags |= subprocess.STARTF_USESHOWWINDOW
            startupinfo.wShowWindow = subprocess.SW_HIDE

        def launch_supervisor() -> subprocess.Popen[bytes]:
            supervisor_env = {**child_env, "QX_WORKER_STOP_CHANNEL": "stdin-eof-v1"}
            return subprocess.Popen(
                [str(binary), "supervise", str(runtime_path)],
                cwd=root,
                env=supervisor_env,
                stdin=subprocess.PIPE,
                stdout=supervisor_log,
                stderr=subprocess.STDOUT,
                creationflags=creationflags,
                startupinfo=startupinfo,
            )

        supervisor = launch_supervisor()
        wait_listener(api_port, supervisor, "supervised API")
        console_creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0) if os.name == "nt" else 0
        console = subprocess.Popen(
            [str(binary), "console", str(runtime_path)],
            cwd=root,
            env=child_env,
            stdout=console_log,
            stderr=subprocess.STDOUT,
            creationflags=console_creationflags,
        )
        wait_listener(console_port, supervisor, "console BFF")

        base = f"http://127.0.0.1:{console_port}"
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        login_status, login_page = http_request(opener, f"{base}/?token={token}")
        require(login_status == 200 and b"<html" in login_page.lower(), "console bootstrap login failed")
        cookies = {cookie.name: cookie.value for cookie in jar}
        csrf = cookies.get("qx_console_csrf", "")
        require(cookies.get("qx_console_session") and csrf, "session/CSRF cookies were not issued")
        write_headers = {"Origin": base, "X-QX-CSRF": csrf}
        deployment_url = f"{base}/strategies/deployments?worker_id={urllib.parse.quote(worker_id)}"
        read_status, read_bytes = http_request(opener, deployment_url)
        require(read_status == 200, f"initial deployment read returned HTTP {read_status}")
        require(any(item.get("worker_id") == worker_id for item in json_body(read_bytes)), "worker projection missing")

        def create_revision(
            revision: int,
            runtime: dict[str, Any],
            bundle_digest: str,
            *,
            desired_running: bool = True,
        ) -> dict[str, Any]:
            config_json = json.dumps(runtime, ensure_ascii=False, separators=(",", ":"))
            status, payload = http_request(
                opener,
                f"{base}/strategies/deployments/revisions",
                method="POST",
                headers=write_headers,
                body={
                    "worker_id": worker_id,
                    "revision": revision,
                    "bundle_sha256": bundle_digest,
                    "plan_sha256": hashlib.sha256(f"{run_id}:plan:{revision}".encode()).hexdigest(),
                    "runtime_config_json": config_json,
                    "desired_running": desired_running,
                },
            )
            require(status == 201, f"revision {revision} creation returned HTTP {status}: {payload[:1000]!r}")
            created = json_body(payload)
            require(created.get("created_by") == "console-operator", "operator identity was not bound")
            return created

        def submit_operation(action: str, revision: int, *, expect: str = "succeeded") -> dict[str, Any]:
            operation_id = f"r09-{run_id}-{action}-{revision}-{len(result['operations'])}"
            status, payload = http_request(
                opener,
                f"{base}/strategies/deployments/operations",
                method="POST",
                headers=write_headers,
                body={
                    "operation_id": operation_id,
                    "idempotency_key": operation_id,
                    "worker_id": worker_id,
                    "action": action,
                    "target_revision": revision,
                },
            )
            require(status in (200, 202), f"{action} submit returned HTTP {status}: {payload[:1000]!r}")
            deadline = time.monotonic() + 45
            operation: dict[str, Any] = {}
            while time.monotonic() < deadline:
                read_status, read_payload = http_request(
                    opener,
                    f"{base}/strategies/deployments/operations?operation_id={urllib.parse.quote(operation_id)}",
                )
                require(read_status == 200, f"{action} readback returned HTTP {read_status}")
                operation = json_body(read_payload)
                state = str(operation.get("state", "")).lower()
                if state in ("succeeded", "failed"):
                    break
                if supervisor.poll() is not None:
                    raise RuntimeError(f"supervisor exited during {action}: {supervisor.returncode}")
                time.sleep(0.2)
            else:
                raise RuntimeError(f"{action} operation did not reach a terminal state: {operation}")
            state = str(operation.get("state", "")).lower()
            require(state == expect, f"{action} expected {expect}, got {state}: {operation}")
            item = {
                "action": action,
                "target_revision": revision,
                "operation_id": operation_id,
                "state": state,
                "error_code": operation.get("error_code"),
                "error_message": operation.get("error_message"),
            }
            result["operations"].append(item)
            return operation

        def projection() -> dict[str, Any]:
            status, payload = http_request(opener, deployment_url)
            require(status == 200, f"deployment projection returned HTTP {status}")
            deployments = json_body(payload)
            return next(item for item in deployments if item.get("worker_id") == worker_id)

        frozen = json.loads(runtime_text)
        create_revision(1, frozen, strategy_digest)
        submit_operation("deploy", 1)
        first: dict[str, Any] | None = None
        first_deadline = time.monotonic() + 30
        while time.monotonic() < first_deadline:
            candidate = projection()
            observation = candidate.get("observation")
            if (
                candidate.get("state", "").lower()
                in ("running", "healthy", "observed", "strategy_initializing")
                and isinstance(observation, dict)
                and str(observation.get("phase", "")).lower() == "running"
            ):
                first = candidate
                break
            if supervisor.poll() is not None:
                raise RuntimeError(
                    f"supervisor exited during initial worker startup: {supervisor.returncode}"
                )
            time.sleep(0.2)
        require(first is not None, f"worker did not become initialized: {candidate}")
        require(first.get("desired_revision") == 1, f"deployed revision 1 is not projected: {first}")

        checkpoint_before_restart: dict[str, Any] | None = None
        last_checkpoint_projection: dict[str, Any] | None = None
        checkpoint_deadline = time.monotonic() + 90
        while time.monotonic() < checkpoint_deadline:
            if supervisor.poll() is not None:
                raise RuntimeError(
                    f"supervisor exited before checkpoint commit: {supervisor.returncode}"
                )
            candidate = projection()
            last_checkpoint_projection = candidate
            observed = candidate.get("observation")
            lifecycle = observed.get("host_lifecycle") if isinstance(observed, dict) else None
            if (
                isinstance(observed, dict)
                and observed.get("checkpoint_roundtrip_observed") is True
                and isinstance(lifecycle, dict)
                and lifecycle.get("checkpoint_input_supplied") is False
            ):
                checkpoint_before_restart = candidate
                break
            time.sleep(0.25)
        require(
            checkpoint_before_restart is not None,
            "initial strategy worker did not commit its first checkpoint; "
            f"last projection: {json.dumps(last_checkpoint_projection, ensure_ascii=False)}",
        )
        before_observation = checkpoint_before_restart["observation"]
        before_process_id = before_observation.get("process_id")
        require(isinstance(before_process_id, int), "pre-restart strategy process ID is missing")
        checkpoint_supervisor_exit, checkpoint_supervisor_stopped_gracefully = stop_supervisor(
            supervisor, timeout=30
        )
        supervisor = launch_supervisor()
        wait_listener(api_port, supervisor, "supervisor restarted with a committed checkpoint")
        checkpoint_after_restart: dict[str, Any] | None = None
        last_restored_projection: dict[str, Any] | None = None
        restore_deadline = time.monotonic() + 90
        while time.monotonic() < restore_deadline:
            if supervisor.poll() is not None:
                raise RuntimeError(
                    f"supervisor exited during checkpoint restore: {supervisor.returncode}"
                )
            try:
                candidate = projection()
            except (OSError, urllib.error.URLError, RuntimeError):
                time.sleep(0.25)
                continue
            last_restored_projection = candidate
            observed = candidate.get("observation")
            lifecycle = observed.get("host_lifecycle") if isinstance(observed, dict) else None
            if (
                isinstance(observed, dict)
                and observed.get("process_id") != before_process_id
                and observed.get("checkpoint_roundtrip_observed") is True
                and isinstance(lifecycle, dict)
                and lifecycle.get("checkpoint_input_supplied") is True
            ):
                checkpoint_after_restart = candidate
                break
            time.sleep(0.25)
        require(
            checkpoint_after_restart is not None,
            "restarted strategy worker did not restore and round-trip its checkpoint; "
            f"last projection: {json.dumps(last_restored_projection, ensure_ascii=False)}",
        )
        restore_records = [
            json.loads(line)
            for line in restore_log.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        require(len(restore_records) == 1, f"expected one checkpoint restore callback: {restore_records}")
        require(
            isinstance(restore_records[0].get("bar_count"), int)
            and restore_records[0]["bar_count"] > 0,
            f"restored strategy state was empty: {restore_records[0]}",
        )
        submit_operation("pause", 1)
        paused = projection()
        require(paused.get("observed_phase", "").lower() == "paused" or "paused" in json.dumps(paused).lower(), "paused observation missing")
        submit_operation("resume", 1)
        resumed = projection()
        require("running" in json.dumps(resumed).lower(), "running observation missing after resume")

        create_revision(2, frozen, strategy_digest)
        submit_operation("rollback", 2)
        rolled_back = projection()
        require(rolled_back.get("desired_revision") == 2, "rollback revision 2 is not projected")
        require("running" in json.dumps(rolled_back).lower(), "Paper rollback did not project revision 2 as running")

        bad_binary = work_dir / "invalid-strategy.bin"
        bad_binary.write_bytes(b"this is deliberately not an executable\n")
        bad_digest = hashlib.sha256(bad_binary.read_bytes()).hexdigest()
        failing = json.loads(json.dumps(frozen))
        failing["strategy"]["builtin_strategy"] = None
        failing["strategy"]["python_module"] = None
        failing["strategy"]["python_strategy_spec"] = None
        failing["strategy"]["bars_snapshot_path"] = None
        failing["strategy"]["external_executable"] = str(bad_binary.resolve())
        failing["strategy"]["strategy_artifact_sha256"] = bad_digest
        create_revision(3, failing, bad_digest)
        failed = submit_operation("deploy", 3, expect="failed")
        error_text = str(failed.get("error_message") or "")
        require("restored" in error_text.lower(), f"failed deployment did not confirm restoration: {failed}")
        restored = projection()
        require(restored.get("desired_revision") == 2, f"previous revision was not restored: {restored}")
        require("running" in json.dumps(restored).lower(), f"previous worker is not observed running: {restored}")

        stopped_config = json.loads(json.dumps(frozen))
        for worker in stopped_config["workers"]:
            if worker.get("id") == worker_id:
                worker["enabled"] = False
        create_revision(4, stopped_config, strategy_digest, desired_running=False)
        submit_operation("stop", 4)
        stopped = projection()
        require("stopped" in json.dumps(stopped).lower(), f"stop did not project stopped state: {stopped}")
        supervisor_exit_before_restart, supervisor_stopped_gracefully = stop_supervisor(
            supervisor, timeout=30
        )
        # The recurring checkpoint jobs have served their purpose. Stop the
        # supervisor before clearing their declarations and scheduler snapshot so
        # the final restart cannot revive a job owned by the stopped Strategy.
        jobs_path.write_text("[]\n", encoding="utf-8")
        scheduler_state_path = Path(config["scheduler"]["state_path"])
        if not scheduler_state_path.is_absolute():
            scheduler_state_path = Path(config["storage"]["data_dir"]) / scheduler_state_path
        scheduler_state_path.unlink(missing_ok=True)
        supervisor = launch_supervisor()
        wait_listener(api_port, supervisor, "restarted supervised API")
        restart_projection: dict[str, Any] | None = None
        last_restart_projection: dict[str, Any] | None = None
        restart_deadline = time.monotonic() + 30
        while time.monotonic() < restart_deadline:
            if supervisor.poll() is not None:
                raise RuntimeError(
                    f"restarted supervisor exited early with code {supervisor.returncode}"
                )
            try:
                candidate = projection()
                last_restart_projection = candidate
                observation = candidate.get("observation")
                if (
                    candidate.get("desired_revision") == 4
                    and candidate.get("desired_running") is False
                    and isinstance(observation, dict)
                    and str(observation.get("phase", "")).lower() == "stopped"
                ):
                    restart_projection = candidate
                    break
            except (OSError, urllib.error.URLError, RuntimeError):
                pass
            time.sleep(0.2)
        require(
            restart_projection is not None,
            "storage/supervisor restart did not restore the stopped deployment revision; "
            f"last projection: {json.dumps(last_restart_projection, ensure_ascii=False)}",
        )
        result.update(
            {
                "outcome": "passed",
                "bootstrap_login_http_status": login_status,
                "session_cookie_issued": True,
                "csrf_cookie_issued": True,
                "revision_count": 4,
                "restored_revision": restored.get("desired_revision"),
                "final_projection": stopped,
                "supervisor_restarted": True,
                "supervisor_stopped_gracefully": supervisor_stopped_gracefully,
                "supervisor_exit_code_before_restart": supervisor_exit_before_restart,
                "restart_projection": restart_projection,
                "checkpoint_restart": {
                    "supervisor_stopped_gracefully": checkpoint_supervisor_stopped_gracefully,
                    "supervisor_exit_code": checkpoint_supervisor_exit,
                    "before_process_id": before_process_id,
                    "after_process_id": checkpoint_after_restart["observation"]["process_id"],
                    "checkpoint_input_supplied": checkpoint_after_restart["observation"]["host_lifecycle"]["checkpoint_input_supplied"],
                    "checkpoint_roundtrip_observed": checkpoint_after_restart["observation"]["checkpoint_roundtrip_observed"],
                    "restore_records": restore_records,
                    "projection": checkpoint_after_restart,
                },
                "api_readiness_assessed": False,
            }
        )
    except Exception as error:  # noqa: BLE001 - retain failed lifecycle evidence
        result.update({"outcome": "failed", "error": f"{type(error).__name__}: {error}"})
    finally:
        if console is not None and console.poll() is None:
            console.terminate()
            try:
                console.wait(timeout=8)
            except subprocess.TimeoutExpired:
                console.kill()
                console.wait(timeout=4)
        if supervisor is not None and supervisor.poll() is None:
            stop_supervisor(supervisor)
        supervisor_log.close()
        console_log.close()
        result["supervisor_exit_code"] = None if supervisor is None else supervisor.returncode
        result["console_exit_code"] = None if console is None else console.returncode
        for label, path in (("supervisor", work_dir / "supervisor.log"), ("console", work_dir / "console.log")):
            if path.exists():
                result.setdefault("log_sha256", {})[label] = hashlib.sha256(path.read_bytes()).hexdigest()
    evidence_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in result.items() if key != "final_projection"}, ensure_ascii=False))
    print(f"Evidence: {evidence_path.relative_to(root).as_posix()}")
    return 0 if result.get("outcome") == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
