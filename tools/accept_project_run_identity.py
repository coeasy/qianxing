#!/usr/bin/env python3
"""Exercise native Linux/macOS project-run PID identity and stale-owner detection."""
from __future__ import annotations

from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from qianxing.project_runs import _process_alive, _process_identity


def main() -> int:
    system = platform.system()
    if system not in {"Linux", "Darwin"}:
        raise SystemExit("this acceptance requires native Linux or macOS process identity")

    self_identity = _process_identity(os.getpid())
    if not self_identity or self_identity.get("platform") != system:
        raise RuntimeError(f"current process has no native {system} identity")
    fallback_identity = None
    if system == "Linux":
        if not self_identity.get("boot_id") or not self_identity.get("process_start", "").isdigit():
            raise RuntimeError(f"Linux process identity is incomplete: {self_identity}")
        evidence_kind = "linux_project_run_process_identity"
    else:
        if (
            self_identity.get("process_start_source") != "proc_pidinfo"
            or self_identity.get("process_start_precision") != "one_microsecond"
            or len(self_identity.get("process_start", "").split(".")) != 2
        ):
            raise RuntimeError(f"macOS proc_pidinfo identity is incomplete: {self_identity}")
        # Force only the native reader to fail so the real `ps` fallback is exercised.
        # This verifies the fallback command and output parser on a macOS host.
        with patch("qianxing.project_runs._darwin_process_start_time", return_value=None):
            fallback_identity = _process_identity(os.getpid())
        if (
            not fallback_identity
            or fallback_identity.get("process_start_source") != "ps_lstart"
            or fallback_identity.get("process_start_precision") != "one_second"
        ):
            raise RuntimeError(f"macOS ps fallback identity is incomplete: {fallback_identity}")
        evidence_kind = "macos_project_run_process_identity"
    if _process_identity(os.getpid()) != self_identity:
        raise RuntimeError("current process identity changed during a stable process lifetime")
    if _process_alive(os.getpid(), self_identity) is not True:
        raise RuntimeError("current process with matching identity was not reported alive")

    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        close_fds=True,
    )
    try:
        deadline = time.monotonic() + 5
        child_identity = None
        while time.monotonic() < deadline:
            child_identity = _process_identity(child.pid)
            if child_identity is not None:
                break
            time.sleep(0.01)
        if not child_identity:
            raise RuntimeError(f"child process {child.pid} has no readable {system} identity")
        if _process_alive(child.pid, child_identity) is not True:
            raise RuntimeError("live child with matching identity was not reported alive")
        stale_identity = {**child_identity, "process_start": child_identity["process_start"] + "-stale"}
        if _process_alive(child.pid, stale_identity) is not False:
            raise RuntimeError("a live PID with stale start identity was not rejected")
    finally:
        child.terminate()
        child.wait(timeout=5)

    if _process_alive(child.pid, child_identity) is not False:
        raise RuntimeError("reaped child PID was still reported alive")

    evidence = {
        "schema_version": 1,
        "evidence_kind": evidence_kind,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "platform": platform.platform(),
        "self_pid": os.getpid(),
        "self_identity": self_identity,
        "fallback_identity": fallback_identity,
        "self_identity_stable": True,
        "child_pid": child.pid,
        "child_identity": child_identity,
        "matching_identity_alive": True,
        "stale_start_identity_rejected": True,
        "reaped_child_reported_dead": True,
    }
    output = Path(sys.argv[1]).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"out": str(output), "evidence_kind": evidence["evidence_kind"], "identity_checks": 5}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
