#!/usr/bin/env python3
"""Measure a real loopback qx-cli API process under concurrent backtest runs.

This workload is synthetic and never starts a venue worker or submits an order.
The 64-watcher boundary is claimed only when /metrics observes all 64 slots and
the next run is rejected by the watcher limit, followed by a successful retry.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import platform
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

from hardware import hardware_fingerprint
from run_profile_backtest import generate_frame, load_fixture
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

try:
    import psutil
except ImportError:
    psutil = None

ROOT = Path(__file__).resolve().parent.parent
TERMINAL = {"succeeded", "failed", "cancelled"}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def percentile(values: list[float], percent: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = (len(ordered) - 1) * percent / 100
    low = int(index)
    high = min(low + 1, len(ordered) - 1)
    return round(ordered[low] + (ordered[high] - ordered[low]) * (index - low), 3)


def request(base: str, method: str, route: str, payload: dict | None = None) -> dict:
    body = None if payload is None else json.dumps(payload, separators=(",", ":")).encode("utf-8")
    req = urllib.request.Request(
        base + route,
        data=body,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    begin = time.perf_counter()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=45 if method == "POST" else 15) as response:
            status, text = response.status, response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        status, text = error.code, error.read().decode("utf-8", errors="replace")
    except (OSError, TimeoutError) as error:
        return {"status": None, "elapsed_ms": round((time.perf_counter() - begin) * 1000, 3), "error": str(error)}
    try:
        document = json.loads(text)
    except ValueError:
        document = text
    return {"status": status, "elapsed_ms": round((time.perf_counter() - begin) * 1000, 3), "body": document}


def metrics(base: str) -> tuple[dict, dict[str, int]]:
    response = request(base, "GET", "/metrics")
    samples: dict[str, int] = {}
    if response["status"] == 200 and isinstance(response.get("body"), str):
        for line in response["body"].splitlines():
            if line.startswith("qx_app_run_watcher") or line.startswith("qx_app_run_watchers"):
                name, value = line.split()
                samples[name] = int(float(value))
    response["body_preview"] = str(response.pop("body", ""))[:300]
    return response, samples


def port_number() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def base_spec(identity: dict, market_spec: dict) -> dict:
    return {
        "schema_version": 1,
        "run_id": "replaced",
        "instrument": identity["instrument"],
        "bars_path": "bars.json",
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000_000_000_000,
        "seed": 20261022,
        "output_dir": "server-replaces-this",
        "market_spec": market_spec,
        "fee_schedule": {"maker_fee_bps": 2, "taker_fee_bps": 5},
        "strategy": {
            "kind": "sma_cross", "strategy_id": "live-capacity-sma",
            "quantity_raw": 1_000_000_000, "fast_window": 2, "slow_window": 3,
        },
    }


def watcher_sample(process: subprocess.Popen, stop: threading.Event, measurements: dict) -> None:
    if psutil is None:
        return
    while not stop.is_set() and process.poll() is None:
        try:
            root = psutil.Process(process.pid)
            children = [root, *root.children(recursive=True)]
            rss = sum(child.memory_info().rss for child in children if child.is_running())
            measurements["peak_tree_rss_bytes"] = max(measurements["peak_tree_rss_bytes"] or 0, rss)
            measurements["peak_process_count"] = max(measurements["peak_process_count"], len(children))
        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
            pass
        stop.wait(0.05)


def run(binary: Path, bars: int, concurrency: int, timeout: float) -> dict:
    target = (ROOT / "target").resolve()
    target.mkdir(exist_ok=True)
    fixture, identity = load_fixture("crypto-spot")
    with tempfile.TemporaryDirectory(prefix="qx-live-capacity-", dir=target) as name:
        work = Path(name).resolve()
        if not work.is_relative_to(target):
            raise RuntimeError("temporary capacity directory escaped target root")
        data, artifacts, store = (work / part for part in ("data", "artifacts", "store"))
        data.mkdir()
        artifacts.mkdir()
        store.mkdir()
        frame = data / "bars.json"
        generate_frame(bars, "crypto-spot", fixture, identity, frame)
        port = port_number()
        base = f"http://127.0.0.1:{port}"
        config = {
            "schema_version": 1,
            "environment": "paper",
            "api": {"bind": f"127.0.0.1:{port}", "transport": "plaintext", "tls": None, "operators": {}},
            "storage": {"backend": "files", "data_dir": str(store), "sqlite_path": None},
            "workers": [{"id": "api", "role": "api", "enabled": True,
                         "account_id": None, "venue_id": None, "endpoint": None,
                         "symbols": [], "credential_env": None}],
            "shutdown_timeout_ms": 30000,
        }
        config_path = work / "runtime.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        env = os.environ.copy()
        env["QX_API_DATA_ROOT"] = str(data)
        env["QX_API_ARTIFACT_ROOT"] = str(artifacts)
        stdout_path, stderr_path = work / "server.stdout", work / "server.stderr"
        # File descriptors avoid pipe backpressure while 64 runs produce logs.
        with stdout_path.open("w", encoding="utf-8") as stdout, stderr_path.open("w", encoding="utf-8") as stderr:
            process = subprocess.Popen(
                [str(binary), "serve", str(config_path)], cwd=ROOT, env=env,
                stdout=stdout, stderr=stderr,
                creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
            )
            stop = threading.Event()
            memory = {"peak_tree_rss_bytes": None, "peak_process_count": 0}
            monitor = threading.Thread(target=watcher_sample, args=(process, stop, memory), daemon=True)
            monitor.start()
            evidence: dict = {}
            try:
                ready_deadline = time.monotonic() + 20
                while True:
                    health = request(base, "GET", "/health")
                    if health["status"] == 200:
                        break
                    if process.poll() is not None or time.monotonic() >= ready_deadline:
                        raise RuntimeError("API failed to become healthy")
                    time.sleep(0.05)
                initial_metrics, initial = metrics(base)
                if initial_metrics["status"] != 200 or initial.get("qx_app_run_watcher_capacity") != 64:
                    raise RuntimeError(f"watcher metrics unavailable: {initial_metrics}, {initial}")
                spec = base_spec(identity, fixture)
                started_at = time.monotonic()

                def start(index: int) -> tuple[str, dict]:
                    run_id = f"live-capacity-{index:03d}"
                    body = {**spec, "run_id": run_id}
                    return run_id, request(base, "POST", "/app/backtest/start", body)

                overflow = None
                admission_samples: list[dict] = []
                with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
                    futures = [pool.submit(start, index) for index in range(concurrency)]
                    while not all(future.done() for future in futures):
                        sample_response, sample = metrics(base)
                        admission_samples.append({"at_ms": round((time.monotonic() - started_at) * 1000, 3),
                                                  "http_status": sample_response["status"], **sample})
                        if (concurrency == 64 and overflow is None
                                and sample.get("qx_app_run_watchers_active") == 64):
                            run_id, response = start(64)
                            overflow = {"run_id": run_id, **response}
                        time.sleep(0.25)
                    results = dict(future.result() for future in futures)
                loaded_metrics, loaded = metrics(base)
                if loaded_metrics["status"] != 200:
                    raise RuntimeError(f"loaded metrics failed: {loaded_metrics}")
                if concurrency == 64 and overflow is None and loaded.get("qx_app_run_watchers_active") == 64:
                    run_id, response = start(64)
                    overflow = {"run_id": run_id, **response}
                # Bounded low-rate probes avoid confusing API rate limiting with watcher admission.
                probes: list[dict] = []
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    health_probe = request(base, "GET", "/health")
                    metric_probe, state = metrics(base)
                    probes.append({"at_ms": round((time.monotonic() - started_at) * 1000, 3),
                                   "health": health_probe["status"],
                                   "health_ms": health_probe["elapsed_ms"],
                                   "metrics": metric_probe["status"],
                                   "metrics_ms": metric_probe["elapsed_ms"], **state})
                    if state.get("qx_app_run_watchers_active") == 0:
                        break
                    if process.poll() is not None:
                        break
                    time.sleep(1)
                terminal = {run_id: request(base, "GET", f"/app/runs/{run_id}") for run_id, response in results.items()
                            if response["status"] == 200}
                final_metrics, final = metrics(base)
                recovered = None
                watcher_rejected = bool(
                    overflow and overflow["status"] == 503
                    and isinstance(overflow.get("body"), dict)
                    and overflow["body"].get("category") == "TIMEOUT"
                )
                if watcher_rejected and final.get("qx_app_run_watchers_active") == 0:
                    run_id, response = start(65)
                    recovered = {"run_id": run_id, **response}
                    if response["status"] == 200:
                        recovery_deadline = time.monotonic() + min(timeout, 30)
                        while time.monotonic() < recovery_deadline:
                            check = request(base, "GET", f"/app/runs/{run_id}")
                            if isinstance(check.get("body"), dict) and check["body"].get("status") in TERMINAL:
                                recovered["terminal_status"] = check["body"]["status"]
                                break
                            time.sleep(0.2)
                statuses = {run_id: response.get("body", {}).get("status")
                            if isinstance(response.get("body"), dict) else None
                            for run_id, response in terminal.items()}
                accepted = sum(response["status"] == 200 for response in results.values())
                evidence = {
                    "schema_version": 1,
                    "workload": "live-loopback-api-synthetic-crypto-spot-bar-v1",
                    "bars_per_run": bars,
                    "requested_concurrency": concurrency,
                    "accepted_runs": accepted,
                    "start_failures": {run_id: response for run_id, response in results.items()
                                       if response["status"] != 200},
                    "run_terminal_statuses": statuses,
                    "all_accepted_succeeded": accepted == concurrency and len(statuses) == accepted
                        and all(status == "succeeded" for status in statuses.values()),
                    "starts_by_http_status": {str(code): sum(r["status"] == code for r in results.values())
                                              for code in sorted({r["status"] for r in results.values()}, key=str)},
                    "start_latency_ms": {"p50": percentile([r["elapsed_ms"] for r in results.values()], 50),
                                         "p95": percentile([r["elapsed_ms"] for r in results.values()], 95)},
                    "health_latency_ms": {"p50": percentile([p["health_ms"] for p in probes], 50),
                                          "p95": percentile([p["health_ms"] for p in probes], 95)},
                    "initial_watcher_metrics": initial,
                    "admission_watcher_samples": admission_samples,
                    "loaded_watcher_metrics": loaded,
                    "final_watcher_metrics": final,
                    "overflow_probe": overflow,
                    "recovered_run": recovered,
                    "watcher_saturation_and_recovery_proven": bool(
                        watcher_rejected and max(
                            [loaded.get("qx_app_run_watchers_active", 0),
                             *[sample.get("qx_app_run_watchers_active", 0) for sample in admission_samples]]
                        ) == 64
                        and final.get("qx_app_run_watchers_active") == 0
                        and recovered and recovered["status"] == 200
                        and recovered.get("terminal_status") == "succeeded"
                        and final.get("qx_app_run_watcher_rejections_total", 0)
                            > initial.get("qx_app_run_watcher_rejections_total", 0)
                    ),
                    "observed_peak_tree_rss_bytes": memory["peak_tree_rss_bytes"],
                    "observed_peak_process_count": memory["peak_process_count"],
                    "rss_sample_interval_ms": 50 if psutil is not None else None,
                    "elapsed_ms": round((time.monotonic() - started_at) * 1000, 3),
                    "probe_count": len(probes),
                    "probes": probes,
                    "binary_sha256": digest(binary),
                    "binary_build_profile": binary.parent.name if binary.parent.name in {"debug", "release"} else "unknown",
                    "barframe_sha256": digest(frame),
                    "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
                    "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
                    "hardware": hardware_fingerprint(),
                    "python": platform.python_version(),
                    "profile_coverage_claim": False,
                    "scope": "Synthetic loopback API, one crypto spot instrument, no venue worker or orders; one run, sampled RSS, no allocation census or long-duration soak.",
                }
            finally:
                stop.set()
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
                monitor.join(timeout=1)
                evidence["server_exit_code"] = process.returncode
                evidence["server_shutdown"] = "benchmark_terminated_process"
                evidence["server_stderr_tail"] = stderr_path.read_text(encoding="utf-8", errors="replace")[-2000:]
        return evidence


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target" / "release" / ("qx-cli.exe" if sys.platform == "win32" else "qx-cli"))
    parser.add_argument("--bars", type=int, default=8_000)
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument("--timeout-seconds", type=float, default=180)
    parser.add_argument("--require-saturation", action="store_true")
    parser.add_argument("--out", type=Path, default=ROOT / "benchmarks" / "results" / "live-api-capacity.json")
    args = parser.parse_args()
    if not 100 <= args.bars <= 1_000_000 or not 1 <= args.concurrency <= 64 or args.timeout_seconds <= 0:
        parser.error("bars must be 100..1,000,000, concurrency 1..64, and timeout positive")
    if args.require_saturation and args.concurrency != 64:
        parser.error("--require-saturation requires --concurrency 64")
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary does not exist: {binary}")
    report = run(binary, args.bars, args.concurrency, args.timeout_seconds)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"evidence={args.out.resolve()} accepted={report['accepted_runs']} "
          f"all_succeeded={report['all_accepted_succeeded']} "
          f"saturation_recovered={report['watcher_saturation_and_recovery_proven']}")
    return 0 if report["all_accepted_succeeded"] and (not args.require_saturation or report["watcher_saturation_and_recovery_proven"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
