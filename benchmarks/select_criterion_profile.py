#!/usr/bin/env python3
"""Choose the smallest Criterion profile covering a pull request's Rust scope."""

from __future__ import annotations

import subprocess
import sys

from run_rust_bench import criterion_profile_for_changed_files


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: select_criterion_profile.py BASE_SHA HEAD_SHA", file=sys.stderr)
        return 2
    base_sha, head_sha = sys.argv[1:]
    result = subprocess.run(
        ["git", "diff", "--name-only", base_sha, head_sha],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if result.returncode:
        sys.stderr.write(result.stderr)
        return result.returncode
    profile = criterion_profile_for_changed_files(result.stdout.splitlines())
    print(f"profile={profile}")
    print(f"Selected Criterion profile {profile} from {len(result.stdout.splitlines())} changed paths.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
