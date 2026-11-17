"""Run and durably record a strategy-refactor acceptance scenario."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import os
import platform
from pathlib import Path
import subprocess
import sys
import tempfile
import uuid


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "maturity" / "strategy-run-acceptance-v1.json"
ARTIFACT_DIR = ROOT / "maturity" / "acceptance-artifacts"
EXCLUDED_PREFIXES = (
    "maturity/acceptance-artifacts/",
    ".tmp-release-prep/",
    "python/python/.tmp/",
    "runs/",
    "python/runs/",
)
EXCLUDED_DIRS = {".codex-tmp-build", ".task-tmp"}
EXCLUDED_FILES = {"maturity/strategy-run-acceptance-v1.json", "runtime.sqlite3"}
TEXT_SUFFIXES = {
    ".bat", ".c", ".cjs", ".cpp", ".css", ".csv", ".fingerprint", ".h",
    ".hpp", ".html", ".in", ".js", ".json", ".lock", ".log", ".md",
    ".prom", ".ps1", ".py", ".rs", ".sh", ".sql", ".svg", ".toml",
    ".typed", ".txt", ".yaml", ".yml",
}
TEXT_FILENAMES = {".gitattributes", ".gitignore"}


def is_excluded_source_path(relative: str) -> bool:
    normalized = relative.replace("\\", "/")
    parts = normalized.split("/")
    return (
        normalized in EXCLUDED_FILES
        or normalized.startswith(EXCLUDED_PREFIXES)
        or any(part in EXCLUDED_DIRS for part in parts)
        or parts[0].startswith("qx-audit-debug-")
    )


def sha256_file(path: Path) -> str:
    content = path.read_bytes()
    if path.suffix.lower() in TEXT_SUFFIXES or path.name in TEXT_FILENAMES:
        # Keep source manifests and log artifacts stable when core.autocrlf
        # checks text files out with CRLF on Windows and LF on Unix.
        content = content.replace(b"\r\n", b"\n")
    return hashlib.sha256(content).hexdigest()


def tracked_source_manifest(artifact_path: Path) -> dict[str, object]:
    tracked = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, check=True, capture_output=True
    ).stdout.split(b"\0")
    untracked = subprocess.run(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    paths = sorted({item.decode("utf-8") for item in tracked + untracked if item})
    excluded_artifact = artifact_path.resolve()
    digest = hashlib.sha256()
    files = []
    for relative in paths:
        if is_excluded_source_path(relative):
            continue
        path = ROOT / relative
        if not path.is_file() or path.resolve() == excluded_artifact:
            continue
        encoded = relative.replace("\\", "/").encode("utf-8")
        file_digest = sha256_file(path)
        digest.update(len(encoded).to_bytes(4, "big"))
        digest.update(encoded)
        digest.update(bytes.fromhex(file_digest))
        files.append({"path": relative.replace("\\", "/"), "sha256": file_digest})
    return {"schema_version": 1, "files": files, "source_tree_sha256": digest.hexdigest()}


def repo_file(raw: str) -> tuple[Path, str]:
    path = (ROOT / raw).resolve()
    try:
        relative = path.relative_to(ROOT).as_posix()
    except ValueError as error:
        raise ValueError(f"path must stay inside the repository: {raw}") from error
    if not path.is_file():
        raise ValueError(f"file does not exist: {raw}")
    return path, relative


@contextmanager
def catalog_lock():
    lock_path = ARTIFACT_DIR / "catalog.lock"
    with lock_path.open("a+b") as stream:
        if os.name == "nt":
            import msvcrt

            stream.seek(0, os.SEEK_END)
            if stream.tell() == 0:
                stream.write(b"\0")
                stream.flush()
            stream.seek(0)
            msvcrt.locking(stream.fileno(), msvcrt.LK_LOCK, 1)
            try:
                yield
            finally:
                stream.seek(0)
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(stream.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(stream.fileno(), fcntl.LOCK_UN)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--acceptance-id", required=True)
    parser.add_argument("--input", action="append", default=[], help="repository-relative input file; repeatable")
    parser.add_argument("--notes", default="")
    parser.add_argument("command", nargs=argparse.REMAINDER, help="command to execute after --")
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("provide the command to execute after --")

    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    scenario = next((item for item in catalog["scenarios"] if item["acceptance_id"] == args.acceptance_id), None)
    if scenario is None:
        parser.error(f"unknown acceptance ID: {args.acceptance_id}")

    run_id = str(uuid.uuid4())
    ARTIFACT_DIR.mkdir(parents=True, exist_ok=True)
    log_path = ARTIFACT_DIR / f"{run_id}.log"
    source_manifest = tracked_source_manifest(log_path)
    inputs = []
    for raw in args.input:
        path, relative = repo_file(raw)
        inputs.append({"path": relative, "sha256": sha256_file(path)})
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout.strip()
    completed = subprocess.run(command, cwd=ROOT, capture_output=True, check=False)
    log_path.write_bytes(
        b"$ " + " ".join(command).encode("utf-8", errors="replace") + b"\n"
        + b"exit_code=" + str(completed.returncode).encode("ascii") + b"\n\n"
        + b"[stdout]\n" + completed.stdout + b"\n[stderr]\n" + completed.stderr
    )
    source_manifest_path = ARTIFACT_DIR / f"{run_id}.source-tree.json"
    with source_manifest_path.open("w", encoding="utf-8", newline="\n") as stream:
        stream.write(json.dumps(source_manifest, ensure_ascii=False, indent=2) + "\n")
    record = {
        "run_id": run_id,
        "acceptance_id": args.acceptance_id,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "source_revision": revision,
        "source_tree_sha256": source_manifest["source_tree_sha256"],
        "source_manifest": source_manifest_path.relative_to(ROOT).as_posix(),
        "environment": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python_version": platform.python_version(),
            "python_executable": sys.executable,
        },
        "command": command,
        "inputs": inputs,
        "outcome": "passed" if completed.returncode == 0 else "failed",
        "artifacts": [
            {"path": log_path.relative_to(ROOT).as_posix(), "sha256": sha256_file(log_path)},
            {"path": source_manifest_path.relative_to(ROOT).as_posix(), "sha256": sha256_file(source_manifest_path)},
        ],
    }
    if args.notes:
        record["notes"] = args.notes
    with catalog_lock():
        catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
        if not any(
            item.get("acceptance_id") == args.acceptance_id
            for item in catalog.get("scenarios", [])
        ):
            parser.error(f"acceptance scenario disappeared while command was running: {args.acceptance_id}")
        catalog["runs"].append(record)
        fd, temporary = tempfile.mkstemp(prefix=f".{CATALOG.name}.", dir=CATALOG.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
                json.dump(catalog, stream, ensure_ascii=False, indent=2)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, CATALOG)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    print(f"{record['outcome']}: {args.acceptance_id} ({run_id})")
    print(f"Evidence: {record['artifacts'][0]['path']}")
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
