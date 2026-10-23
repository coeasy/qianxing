"""Cross-platform subprocess timing and peak resident-memory measurement."""

from __future__ import annotations

import ctypes
import locale
import os
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

try:
    import psutil
except ImportError:  # Optional; reports preserve unavailable instead of inventing a value.
    psutil = None


class _ProcessMemoryCountersEx(ctypes.Structure):
    _fields_ = [
        ("cb", ctypes.c_ulong),
        ("PageFaultCount", ctypes.c_ulong),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


def _resident_bytes(pid: int) -> int | None:
    if sys.platform == "win32":
        process = ctypes.windll.kernel32.OpenProcess(0x0400 | 0x0010, False, pid)
        if not process:
            return None
        try:
            counters = _ProcessMemoryCountersEx()
            counters.cb = ctypes.sizeof(counters)
            if not ctypes.windll.psapi.GetProcessMemoryInfo(
                process, ctypes.byref(counters), counters.cb
            ):
                return None
            return int(counters.PeakWorkingSetSize)
        finally:
            ctypes.windll.kernel32.CloseHandle(process)

    if sys.platform.startswith("linux"):
        try:
            status = Path(f"/proc/{pid}/status").read_text(encoding="ascii")
            for line in status.splitlines():
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024
        except (OSError, ValueError, IndexError):
            return None
        return None

    if sys.platform == "darwin":
        try:
            value = subprocess.check_output(
                ["ps", "-o", "rss=", "-p", str(pid)], text=True, stderr=subprocess.DEVNULL
            ).strip()
            return int(value) * 1024 if value else None
        except (OSError, subprocess.SubprocessError, ValueError):
            return None

    return None


@dataclass(frozen=True)
class ProcessMeasurement:
    elapsed_ms: float
    stdout: str
    stderr: str
    return_code: int
    peak_rss_bytes: int | None
    peak_tree_rss_bytes: int | None
    peak_process_count: int
    tree_cpu_seconds: float | None
    tree_io_read_bytes: int | None
    tree_io_write_bytes: int | None

    def __iter__(self):
        # Preserve the established seven-field unpacking contract for existing runners.
        yield self.elapsed_ms
        yield self.stdout
        yield self.stderr
        yield self.return_code
        yield self.peak_rss_bytes
        yield self.peak_tree_rss_bytes
        yield self.peak_process_count


def run_measured(
    command: list[str], cwd: Path, timeout_seconds: float | None = None
) -> ProcessMeasurement:
    """Run without pipe backpressure and return elapsed ms plus process peak RSS.

    Linux and Windows expose a process high-water mark. macOS is sampled while
    the child runs, so very short-lived RSS spikes can be missed there. When
    psutil is available, aggregate RSS and process count include descendants and
    are sampled at the same interval; process-tree samples can miss short spikes.
    """
    with tempfile.TemporaryFile(mode="w+b") as stdout_file, tempfile.TemporaryFile(
        mode="w+b"
    ) as stderr_file:
        started = time.perf_counter()
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdout=stdout_file,
            stderr=stderr_file,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        peak_rss: int | None = None
        peak_tree_rss: int | None = None
        peak_process_count = 1
        observed_processes: dict[int, object] = {}
        tree_cpu_seconds: float | None = None
        tree_io_read_bytes: int | None = None
        tree_io_write_bytes: int | None = None
        while True:
            measured = _resident_bytes(process.pid)
            if measured is not None:
                peak_rss = measured if peak_rss is None else max(peak_rss, measured)
            if psutil is not None:
                try:
                    root = psutil.Process(process.pid)
                    processes = [root, *root.children(recursive=True)]
                    rss_total = 0
                    live_count = 0
                    for child in processes:
                        try:
                            rss_total += child.memory_info().rss
                            live_count += 1
                            observed_processes[child.pid] = child
                        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
                            continue
                    if live_count:
                        peak_tree_rss = (
                            rss_total if peak_tree_rss is None else max(peak_tree_rss, rss_total)
                        )
                        peak_process_count = max(peak_process_count, live_count)
                    cpu_total = 0.0
                    io_read_total = 0
                    io_write_total = 0
                    cpu_available = True
                    io_available = True
                    for child in observed_processes.values():
                        try:
                            cpu = child.cpu_times()
                            cpu_total += cpu.user + cpu.system
                        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
                            cpu_available = False
                        try:
                            io = child.io_counters()
                            io_read_total += io.read_bytes
                            io_write_total += io.write_bytes
                        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess, AttributeError):
                            io_available = False
                    # A zero CPU-time result for a live sampled process is usually
                    # below the platform timer resolution; preserve it as unavailable.
                    if cpu_available and cpu_total > 0:
                        tree_cpu_seconds = max(tree_cpu_seconds or 0.0, cpu_total)
                    if io_available:
                        tree_io_read_bytes = max(tree_io_read_bytes or 0, io_read_total)
                        tree_io_write_bytes = max(tree_io_write_bytes or 0, io_write_total)
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass
            if process.poll() is not None:
                break
            if (
                timeout_seconds is not None
                and time.perf_counter() - started >= timeout_seconds
            ):
                _terminate_process_tree(process)
                stdout_file.seek(0)
                stderr_file.seek(0)
                raise subprocess.TimeoutExpired(
                    command,
                    timeout_seconds,
                    output=_decode_process_output(stdout_file.read()),
                    stderr=_decode_process_output(stderr_file.read()),
                )
            time.sleep(0.01)
        return_code = process.wait()
        measured = _resident_bytes(process.pid)
        if measured is not None:
            peak_rss = measured if peak_rss is None else max(peak_rss, measured)
        elapsed_ms = (time.perf_counter() - started) * 1_000.0
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = _decode_process_output(stdout_file.read())
        stderr = _decode_process_output(stderr_file.read())
        return ProcessMeasurement(
            elapsed_ms=elapsed_ms,
            stdout=stdout,
            stderr=stderr,
            return_code=return_code,
            peak_rss_bytes=peak_rss,
            peak_tree_rss_bytes=peak_tree_rss,
            peak_process_count=peak_process_count,
            tree_cpu_seconds=tree_cpu_seconds,
            tree_io_read_bytes=tree_io_read_bytes,
            tree_io_write_bytes=tree_io_write_bytes,
        )


def _decode_process_output(raw: bytes) -> str:
    """Decode redirected child output without assuming Windows uses UTF-8."""
    encodings = ["utf-8", locale.getpreferredencoding(False)]
    if sys.platform == "win32":
        encodings.append("mbcs")
    for encoding in dict.fromkeys(encodings):
        try:
            return raw.decode(encoding)
        except (LookupError, UnicodeDecodeError):
            continue
    return raw.decode("utf-8", errors="replace")


def _terminate_process_tree(process: subprocess.Popen[bytes]) -> None:
    descendants = []
    if psutil is not None:
        try:
            descendants = psutil.Process(process.pid).children(recursive=True)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    try:
        process.terminate()
    except OSError:
        pass
    for child in reversed(descendants):
        try:
            child.terminate()
        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
            continue
    if psutil is not None and descendants:
        _, alive = psutil.wait_procs(descendants, timeout=1.0)
        for child in alive:
            try:
                child.kill()
            except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
                continue
    try:
        process.wait(timeout=1.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
