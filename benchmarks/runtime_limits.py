"""Read effective container CPU and memory limits without assuming they exist."""

from __future__ import annotations

import os
import platform
from pathlib import Path


def _read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="ascii").strip()
    except (OSError, UnicodeError):
        return None


def _limited_integer(value: str | None) -> int | None:
    if value is None or value == "max":
        return None
    try:
        parsed = int(value)
    except ValueError:
        return None
    return parsed if parsed >= 0 else None


def _cpu_quota(cpu_max: str | None) -> tuple[int | None, int | None, float | None]:
    if not cpu_max:
        return None, None, None
    pieces = cpu_max.split()
    if len(pieces) != 2:
        return None, None, None
    quota = _limited_integer(pieces[0])
    period = _limited_integer(pieces[1])
    if quota is None or period is None or period == 0:
        return None, period, None
    return quota, period, quota / period


def _parse_cpu_list(value: str | None) -> list[int] | None:
    if not value:
        return None
    cpus: set[int] = set()
    try:
        for part in value.split(","):
            if "-" in part:
                first, last = (int(item) for item in part.split("-", 1))
                if first < 0 or last < first:
                    return None
                cpus.update(range(first, last + 1))
            else:
                cpu = int(part)
                if cpu < 0:
                    return None
                cpus.add(cpu)
    except ValueError:
        return None
    return sorted(cpus) or None


def runtime_limits(
    *,
    cgroup_root: Path = Path("/sys/fs/cgroup"),
    cpu_affinity: set[int] | None = None,
) -> dict:
    """Return observed limits; absent controller files stay explicitly unavailable."""
    if platform.system() != "Linux":
        return {
            "cgroup_version": None,
            "cgroup_controllers_visible": [],
            "cgroup_cpu_quota_micros": None,
            "cgroup_cpu_period_micros": None,
            "cgroup_cpu_limit_count": None,
            "cgroup_cpuset_cpus_effective": None,
            "cgroup_memory_limit_bytes": None,
            "cgroup_memory_current_bytes": None,
            "cgroup_files_visible": False,
            "cpu_affinity": None,
            "cpu_affinity_count": None,
        }

    if cpu_affinity is None:
        try:
            cpu_affinity = set(os.sched_getaffinity(0))
        except (AttributeError, OSError):
            cpu_affinity = set()
    affinity = sorted(cpu_affinity) or None
    controllers = _read_text(cgroup_root / "cgroup.controllers")
    cpu_max = _read_text(cgroup_root / "cpu.max")
    quota, period, cpu_limit = _cpu_quota(cpu_max)
    cpuset = _parse_cpu_list(_read_text(cgroup_root / "cpuset.cpus.effective"))
    memory_limit = _limited_integer(_read_text(cgroup_root / "memory.max"))
    memory_current = _limited_integer(_read_text(cgroup_root / "memory.current"))
    visible = any(
        value is not None
        for value in (controllers, cpu_max, _read_text(cgroup_root / "memory.max"))
    )
    return {
        "cgroup_version": 2 if controllers is not None else None,
        "cgroup_controllers_visible": controllers.split() if controllers else [],
        "cgroup_cpu_quota_micros": quota,
        "cgroup_cpu_period_micros": period,
        "cgroup_cpu_limit_count": cpu_limit,
        "cgroup_cpuset_cpus_effective": cpuset,
        "cgroup_memory_limit_bytes": memory_limit,
        "cgroup_memory_current_bytes": memory_current,
        "cgroup_files_visible": visible,
        "cpu_affinity": affinity,
        "cpu_affinity_count": len(affinity) if affinity is not None else None,
    }
