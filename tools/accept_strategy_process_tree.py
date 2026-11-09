#!/usr/bin/env python3
"""Verify dynamic strategy preflight reaps descendants on return and timeout."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from qianxing.process_tree import run_process_tree


def process_alive(process_id: int) -> bool:
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel32.OpenProcess.restype = wintypes.HANDLE
        kernel32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel32.GetExitCodeProcess.restype = wintypes.BOOL
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel32.OpenProcess(0x1000, False, process_id)
        if not handle:
            return False
        try:
            code = wintypes.DWORD()
            return bool(kernel32.GetExitCodeProcess(handle, ctypes.byref(code))) and code.value == 259
        finally:
            kernel32.CloseHandle(handle)
    proc_stat = Path(f"/proc/{process_id}/stat")
    if proc_stat.exists():
        try:
            stat = proc_stat.read_text(encoding="ascii")
            state = stat[stat.rfind(")") + 2 :].split()[0]
            return state != "Z"
        except (OSError, IndexError):
            return False
    try:
        os.kill(process_id, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def wait_for_child_exit(process_id: int, timeout: float = 5.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not process_alive(process_id):
            return True
        time.sleep(0.05)
    return not process_alive(process_id)


def child_script(pid_file: Path) -> str:
    return (
        "import os, pathlib, sys, time; "
        "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()), encoding='ascii'); "
        "time.sleep(60)"
    )


def parent_script(pid_file: Path, *, hang: bool) -> str:
    spawn = (
        f"subprocess.Popen([sys.executable, '-c', {child_script(pid_file)!r}, sys.argv[1]], "
        "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, close_fds=True)"
    )
    lines = [
        "import pathlib, subprocess, sys, time",
        spawn,
        "deadline = time.monotonic() + 5",
        "while not pathlib.Path(sys.argv[1]).exists() and time.monotonic() < deadline:",
        "    time.sleep(.01)",
        "assert pathlib.Path(sys.argv[1]).exists()",
        "time.sleep(60)" if hang else "print('parent-returned')",
    ]
    return "\n".join(lines)


def run_case(root: Path, label: str, *, hang: bool, timeout: float) -> dict[str, object]:
    pid_file = root / f"{label}.pid"
    args = [sys.executable, "-c", parent_script(pid_file, hang=hang), str(pid_file)]
    started = time.monotonic()
    timed_out = False
    if hang:
        try:
            run_process_tree(args, cwd=ROOT, timeout_seconds=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
        else:
            raise RuntimeError(f"{label}: expected dynamic preflight timeout")
    else:
        result = run_process_tree(args, cwd=ROOT, timeout_seconds=timeout)
        if result.returncode != 0 or "parent-returned" not in result.stdout:
            raise RuntimeError(f"{label}: parent did not return successfully: {result}")
    if not pid_file.is_file():
        raise RuntimeError(f"{label}: child did not publish its PID before parent exit")
    child_pid = int(pid_file.read_text(encoding="ascii"))
    reaped = wait_for_child_exit(child_pid)
    if not reaped:
        # Best-effort cleanup protects the host even if this acceptance exposes a regression.
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(child_pid), "/T", "/F"], capture_output=True, check=False)
        else:
            try:
                os.kill(child_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        raise RuntimeError(f"{label}: descendant process {child_pid} survived cleanup")
    return {
        "case": label,
        "parent_expected_timeout": hang,
        "timed_out": timed_out,
        "descendant_pid": child_pid,
        "descendant_reaped": reaped,
        "elapsed_seconds": round(time.monotonic() - started, 4),
    }


def run_detached_process_limit_case(root: Path) -> dict[str, object]:
    if os.name == "nt":
        raise RuntimeError("the detached-session boundary case is POSIX-only")
    pid_file = root / "detached-child.pid"
    detached_script = child_script(pid_file)
    parent = "\n".join([
        "import pathlib, subprocess, sys, time",
        "subprocess.Popen([sys.executable, '-c', " + repr(detached_script) + ", sys.argv[1]], "
        "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, "
        "close_fds=True, start_new_session=True)",
        "deadline = time.monotonic() + 5",
        "while not pathlib.Path(sys.argv[1]).exists() and time.monotonic() < deadline:",
        "    time.sleep(.01)",
        "assert pathlib.Path(sys.argv[1]).exists()",
        "print('parent-returned')",
    ])
    args = [sys.executable, "-c", parent, str(pid_file)]
    result = run_process_tree(args, cwd=ROOT, timeout_seconds=5.0)
    if result.returncode != 0 or "parent-returned" not in result.stdout:
        raise RuntimeError(f"detached-limit: parent did not return successfully: {result}")
    if not pid_file.is_file():
        raise RuntimeError("detached-limit: child did not publish its PID before parent exit")
    child_pid = int(pid_file.read_text(encoding="ascii"))
    escaped = process_alive(child_pid)
    cleaned = False
    try:
        if not escaped:
            raise RuntimeError("detached-limit: a new-session child was unexpectedly reaped")
    finally:
        if process_alive(child_pid):
            try:
                os.kill(child_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        cleaned = wait_for_child_exit(child_pid)
    if not cleaned:
        raise RuntimeError(f"detached-limit: acceptance could not clean child {child_pid}")
    return {
        "case": "detached-new-session",
        "parent_expected_timeout": False,
        "detached_child_pid": child_pid,
        "escaped_owned_process_group": escaped,
        "acceptance_cleanup_reaped_child": cleaned,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--timeout-seconds", type=float, default=0.5)
    args = parser.parse_args()
    output = args.output.resolve()
    if not 0.1 <= args.timeout_seconds <= 10:
        parser.error("timeout must be in 0.1..10 seconds")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="qx-preflight-tree-", dir=output.parent) as temporary:
        root = Path(temporary)
        rows = [
            run_case(root, "normal-return", hang=False, timeout=5.0),
            run_case(root, "timeout", hang=True, timeout=args.timeout_seconds),
        ]
        if os.name != "nt":
            rows.append(run_detached_process_limit_case(root))
    report = {
        "schema_version": 1,
        "evidence_kind": "dynamic_strategy_preflight_process_tree_cleanup",
        "recorded_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "platform": sys.platform,
        "timeout_seconds": args.timeout_seconds,
        "process_tree_cases": rows,
        "detached_process_limit_documented": "POSIX children that deliberately start a new session escape the owned process group and require separate ownership; the Linux acceptance verifies this boundary and cleans up its fixture child.",
    }
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    all_descendants_reaped = all(
        row.get("descendant_reaped", row.get("acceptance_cleanup_reaped_child", False))
        for row in rows
    )
    print(json.dumps({"out": str(output), "cases": len(rows), "all_descendants_reaped": all_descendants_reaped}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
