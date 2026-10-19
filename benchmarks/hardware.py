"""Portable hardware facts shared by every benchmark evidence writer."""

from __future__ import annotations

import json
import os
import platform
import subprocess


def _windows_hardware_details() -> tuple[int | None, int | None, str | None, str | None]:
    if os.name != "nt":
        return None, None, None
    command = (
        "$c=Get-CimInstance Win32_Processor; "
        "$p=$c | Measure-Object -Property NumberOfCores -Sum; "
        "$m=Get-CimInstance Win32_ComputerSystem; "
        "[pscustomobject]@{physical_cores=$p.Sum; total_ram_bytes=$m.TotalPhysicalMemory; "
        "processor_name=$c[0].Name; model=$m.Model} "
        "| ConvertTo-Json -Compress"
    )
    try:
        result = subprocess.run(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=10,
            check=True,
        )
        details = json.loads(result.stdout)
        return (
            int(details["physical_cores"]),
            int(details["total_ram_bytes"]),
            str(details["model"]),
            str(details["processor_name"]).strip(),
        )
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, json.JSONDecodeError):
        return None, None, None, None


def _total_ram_bytes() -> int | None:
    if hasattr(os, "sysconf"):
        try:
            return int(os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE"))
        except (OSError, ValueError):
            return None
    return None


def hardware_fingerprint() -> dict:
    physical_cores, ram_bytes, model, processor_name = _windows_hardware_details()
    if ram_bytes is None:
        ram_bytes = _total_ram_bytes()
    result = {
        "os": platform.system(),
        "arch": platform.machine(),
        "processor": platform.processor(),
        "logical_cpu_count": os.cpu_count(),
        "physical_core_count": physical_cores,
        "total_ram_bytes": ram_bytes,
    }
    if model:
        result["system_model"] = model
    if processor_name:
        result["processor_name"] = processor_name
    return result
