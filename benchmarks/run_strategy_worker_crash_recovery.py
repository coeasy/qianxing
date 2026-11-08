#!/usr/bin/env python3
"""Exercise Paper Strategy worker recovery across real persistence boundaries."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import time
from urllib.parse import unquote, urlsplit, urlunsplit


CRASH_POINTS = (
    "control_command_persisted",
    "command_result_persisted",
    "cursor_checkpoint_committed",
    "scheduler_run_completed",
    "queue_job_acknowledged",
    "prepared_journal_cleaned_after_ack",
)


def runtime_for_attempt(
    source: Path,
    scratch: Path,
    backend: str,
    postgres_dsn_env: str,
) -> tuple[Path, Path, Path | None]:
    config = json.loads(source.read_text(encoding="utf-8-sig"))
    root = source.parent

    def resolve_asset(raw: str) -> str:
        path = Path(raw)
        if path.is_absolute():
            return str(path)
        for candidate in (root / path, Path.cwd() / path):
            if candidate.exists():
                return str(candidate.resolve())
        return str((root / path).resolve())

    data = scratch / "data"
    config["storage"]["data_dir"] = str(data.resolve())
    config["storage"]["backend"] = {
        "file": "files",
        "sqlite": "sqlite",
        "postgres": "postgres",
    }[backend]
    sqlite_path = scratch / "runtime.sqlite3"
    config["storage"]["sqlite_path"] = str(sqlite_path.resolve()) if backend == "sqlite" else None
    config["storage"]["postgres_dsn_env"] = postgres_dsn_env if backend == "postgres" else None
    if backend == "postgres":
        config["storage"]["consistency"] = "transactional"
        config["profile"] = "distributed"
    scheduler = config["scheduler"]
    scheduler["state_path"] = "scheduler.json"
    scheduler["job_queue_path"] = str((scratch / "job-queue").resolve())
    scheduler["jobs_path"] = resolve_asset(scheduler["jobs_path"])
    config["strategy"]["target_snapshot_path"] = resolve_asset(config["strategy"]["target_snapshot_path"])
    for worker in config["workers"]:
        spec = worker.get("instrument_spec_path")
        if spec:
            worker["instrument_spec_path"] = resolve_asset(spec)
    runtime = scratch / "runtime.json"
    runtime.write_text(json.dumps(config, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return runtime, data, sqlite_path if backend == "sqlite" else None


def postgres_dsn_for_database(dsn: str, database: str) -> str:
    parsed = urlsplit(dsn)
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.hostname:
        raise ValueError("PostgreSQL crash recovery requires a postgres:// DSN with a host")
    return urlunsplit((parsed.scheme, parsed.netloc, f"/{database}", parsed.query, ""))


def postgres_client_environment(dsn: str, database: str | None = None) -> dict[str, str]:
    parsed = urlsplit(dsn)
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.hostname:
        raise ValueError("PostgreSQL crash recovery requires a postgres:// DSN with a host")
    query = dict(item.split("=", 1) if "=" in item else (item, "") for item in parsed.query.split("&") if item)
    environment = os.environ.copy()
    environment["PGHOST"] = parsed.hostname
    environment["PGPORT"] = str(parsed.port or 5432)
    if parsed.username:
        environment["PGUSER"] = unquote(parsed.username)
    if parsed.password:
        environment["PGPASSWORD"] = unquote(parsed.password)
    environment["PGDATABASE"] = database or unquote(parsed.path.lstrip("/")) or "postgres"
    if query.get("sslmode"):
        environment["PGSSLMODE"] = query["sslmode"]
    return environment


def psql(
    executable: Path,
    dsn: str,
    sql: str,
    *,
    database: str | None = None,
) -> str:
    completed = subprocess.run(
        [str(executable), "-X", "-v", "ON_ERROR_STOP=1", "-A", "-t", "-c", sql],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=postgres_client_environment(dsn, database),
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"psql failed ({completed.returncode}): {completed.stderr.strip()}"
        )
    return completed.stdout.strip()


def create_postgres_database(executable: Path, dsn: str, database: str) -> None:
    psql(executable, dsn, f'CREATE DATABASE "{database}"')


def drop_postgres_database(executable: Path, dsn: str, database: str) -> None:
    psql(executable, dsn, f'DROP DATABASE IF EXISTS "{database}" WITH (FORCE)')


def read_postgres_control_state(executable: Path, dsn: str) -> dict[str, object]:
    payload = psql(
        executable,
        dsn,
        "SELECT content FROM qx_control_state WHERE state_id = 1",
    )
    if not payload:
        raise RuntimeError("PostgreSQL control state was not persisted")
    value = json.loads(payload)
    if not isinstance(value, dict):
        raise RuntimeError("PostgreSQL control state is not a JSON object")
    return value


def run(binary: Path, runtime: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(binary), "paper-e2e", str(runtime)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=env,
        check=False,
    )


def cursor_snapshot(data: Path) -> dict[str, object] | None:
    cursors = sorted((data / "strategy-live-state").glob("*.execution-cursor.json"))
    if len(cursors) > 1:
        raise RuntimeError(f"expected at most one Strategy cursor, found {len(cursors)}")
    if not cursors:
        return None
    payload = cursors[0].read_bytes()
    state = json.loads(payload)
    checkpoint = state.get("strategy_checkpoint")
    checkpoint_payload = (
        json.dumps(checkpoint, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        if checkpoint is not None
        else b""
    )
    return {
        "path": cursors[0].name,
        "sha256": hashlib.sha256(payload).hexdigest(),
        "next_seq_by_log": state.get("next_seq_by_log", {}),
        "commit_generation": state.get("commit_generation", 0),
        "checkpoint_sha256": hashlib.sha256(checkpoint_payload).hexdigest() if checkpoint is not None else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", required=True, type=Path, help="Paper runtime JSON")
    parser.add_argument("--binary", required=True, type=Path, help="bench-prepared-evaluation qx-cli binary")
    parser.add_argument("--output", required=True, type=Path, help="JSON evidence output")
    parser.add_argument("--backend", choices=("file", "sqlite", "postgres"), default="file")
    parser.add_argument(
        "--postgres-dsn-env",
        default="QX_BENCH_POSTGRES_DSN",
        help="environment variable containing an isolated PostgreSQL URI DSN",
    )
    parser.add_argument(
        "--psql",
        type=Path,
        help="psql executable; defaults to psql on PATH",
    )
    parser.add_argument("--lease-seconds", type=int, default=2)
    parser.add_argument("--recovery-wait-seconds", type=float, default=3.0)
    args = parser.parse_args()
    source = args.runtime.resolve()
    binary = args.binary.resolve()
    output = args.output.resolve()
    if args.lease_seconds < 1 or args.recovery_wait_seconds <= args.lease_seconds:
        parser.error("recovery wait must exceed a positive lease duration")
    for path in (source, binary):
        if not path.is_file():
            parser.error(f"file not found: {path}")
    psql_binary = args.psql.resolve() if args.psql is not None else None
    if args.backend == "postgres":
        if not args.postgres_dsn_env.strip():
            parser.error("--postgres-dsn-env must name the PostgreSQL DSN environment variable")
        if psql_binary is None:
            discovered_psql = shutil.which("psql")
            psql_binary = Path(discovered_psql).resolve() if discovered_psql else None
        if psql_binary is None or not psql_binary.is_file():
            parser.error("PostgreSQL worker recovery requires psql; pass --psql PATH")
        if not os.environ.get(args.postgres_dsn_env):
            parser.error(f"{args.postgres_dsn_env} must contain an isolated PostgreSQL URI DSN")
    output.parent.mkdir(parents=True, exist_ok=True)

    rows = []
    postgres_dsn = os.environ.get(args.postgres_dsn_env) if args.backend == "postgres" else None
    postgres_databases: list[str] = []
    try:
        with tempfile.TemporaryDirectory(prefix="qx-worker-crash-", dir=output.parent) as temporary:
            temp_root = Path(temporary)
            for point in CRASH_POINTS:
                scratch = temp_root / point
                scratch.mkdir()
                database = None
                attempt_dsn = postgres_dsn
                if args.backend == "postgres":
                    database = f"qx_crash_{os.getpid()}_{point}"
                    create_postgres_database(psql_binary, postgres_dsn, database)
                    postgres_databases.append(database)
                    attempt_dsn = postgres_dsn_for_database(postgres_dsn, database)
                runtime, data, sqlite_path = runtime_for_attempt(
                    source, scratch, args.backend, args.postgres_dsn_env
                )
                cursor_before_crash = cursor_snapshot(data)
                env = os.environ.copy()
                if attempt_dsn is not None:
                    env[args.postgres_dsn_env] = attempt_dsn
                env["QX_BENCH_STRATEGY_LEASE_SECONDS"] = str(args.lease_seconds)
                env["QX_BENCH_PREPARED_CRASH_AFTER"] = point
                crashed = run(binary, runtime, env)
                if crashed.returncode != 86:
                    raise RuntimeError(
                        f"{point}: expected injected exit 86, got {crashed.returncode}\n"
                        f"stdout:\n{crashed.stdout}\nstderr:\n{crashed.stderr}"
                    )
                cursor_after_crash = cursor_snapshot(data)

                time.sleep(args.recovery_wait_seconds)
                env.pop("QX_BENCH_PREPARED_CRASH_AFTER", None)
                env.pop("QX_BENCH_STRATEGY_LEASE_SECONDS", None)
                recovered = run(binary, runtime, env)
                if recovered.returncode != 0:
                    raise RuntimeError(
                        f"{point}: recovery failed with exit {recovered.returncode}\n"
                        f"stdout:\n{recovered.stdout}\nstderr:\n{recovered.stderr}"
                    )
                if args.backend == "file":
                    control_path = data / "control-plane.json"
                    control = json.loads(control_path.read_text(encoding="utf-8"))
                elif args.backend == "sqlite":
                    connection = sqlite3.connect(sqlite_path)
                    try:
                        row = connection.execute(
                            "SELECT content FROM qx_control_state WHERE state_id = 1"
                        ).fetchone()
                    finally:
                        connection.close()
                    if row is None:
                        raise RuntimeError(f"{point}: SQLite control state was not persisted")
                    control = json.loads(row[0])
                else:
                    control = read_postgres_control_state(psql_binary, attempt_dsn)
                statuses = [item["status"] for item in control.get("audit", [])]
                if len(control.get("commands", {})) != 1 or statuses.count("Accepted") != 1 or statuses.count("Executed") != 1:
                    raise RuntimeError(f"{point}: expected one Accepted and one Executed audit, got {statuses}")
                summary = next((line for line in recovered.stdout.splitlines() if "[Paper · E2E]" in line), "")
                if "orders=1 (+1" not in summary or "ledger_entries=4 (+4" not in summary:
                    raise RuntimeError(f"{point}: Paper recovery summary did not show one new order and four ledger entries: {summary}")
                cursor_after_recovery = cursor_snapshot(data)
                repeated = run(binary, runtime, env)
                if repeated.returncode != 0:
                    raise RuntimeError(
                        f"{point}: idempotence restart failed with exit {repeated.returncode}\n"
                        f"stdout:\n{repeated.stdout}\nstderr:\n{repeated.stderr}"
                    )
                cursor_after_idempotent_restart = cursor_snapshot(data)
                if cursor_after_recovery != cursor_after_idempotent_restart:
                    raise RuntimeError(
                        f"{point}: cursor/checkpoint changed after idempotent restart: "
                        f"{cursor_after_recovery} -> {cursor_after_idempotent_restart}"
                    )
                if point in CRASH_POINTS[2:] and cursor_after_crash != cursor_after_recovery:
                    raise RuntimeError(
                        f"{point}: cursor/checkpoint changed after its commit boundary: "
                        f"{cursor_after_crash} -> {cursor_after_recovery}"
                    )
                rows.append(
                    {
                        "crash_point": point,
                        "injected_exit_code": crashed.returncode,
                        "recovery_exit_code": recovered.returncode,
                        "control_command_count": len(control["commands"]),
                        "control_audit_statuses": statuses,
                        "paper_summary": summary,
                        "cursor_before_crash": cursor_before_crash,
                        "cursor_after_crash": cursor_after_crash,
                        "cursor_after_recovery": cursor_after_recovery,
                        "cursor_after_idempotent_restart": cursor_after_idempotent_restart,
                        "idempotent_restart_exit_code": repeated.returncode,
                    }
                )
    finally:
        if args.backend == "postgres":
            for database in reversed(postgres_databases):
                drop_postgres_database(psql_binary, postgres_dsn, database)

    report = {
        "schema_version": 1,
        "evidence_kind": "paper_strategy_worker_crash_recovery",
        "recorded_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "platform": os.name,
        "lease_seconds": args.lease_seconds,
        "recovery_wait_seconds": args.recovery_wait_seconds,
        "storage_backend": args.backend,
        "venue": "paper",
        "network_accessed": False,
        "real_order_sent": False,
        "recovery_count": len(rows),
        "crash_recovery": rows,
    }
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"out": str(output), "recovery_count": len(rows), "passed": True}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
