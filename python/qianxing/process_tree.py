"""Bounded subprocess execution with best-effort descendant cleanup."""

from __future__ import annotations

import os
import signal
import subprocess
import time
import ctypes
from ctypes import wintypes
from pathlib import Path
from typing import Mapping, Sequence


def run_process_tree(
    args: Sequence[str],
    *,
    cwd: str | Path,
    timeout_seconds: float,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a child in its own process group and reap its descendants on exit/timeout.

    POSIX children start a new session and the whole process group is signalled. Windows
    children start a new process group and ``taskkill /T`` is used to terminate descendants.
    Processes that deliberately detach into another session or otherwise escape platform tree
    ownership cannot be controlled by a parent process and remain outside this boundary.
    """
    creationflags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
    process = subprocess.Popen(
        list(args),
        cwd=str(cwd),
        env=dict(env) if env is not None else None,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        start_new_session=os.name != "nt",
        creationflags=creationflags,
    )
    try:
        windows_job = _attach_windows_job(process) if os.name == "nt" else None
    except BaseException:
        _terminate_tree(process)
        raise
    if os.name == "nt" and windows_job is None:
        _terminate_tree(process)
        raise OSError("could not assign dynamic preflight to a Windows kill-on-close job")
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired as error:
        _terminate_tree(process)
        stdout, stderr = process.communicate()
        raise subprocess.TimeoutExpired(
            args,
            timeout_seconds,
            output=stdout if stdout else error.output,
            stderr=stderr if stderr else error.stderr,
        ) from None
    except BaseException:
        _terminate_tree(process)
        raise
    else:
        # Dynamic preflight is a bounded request/response operation. A strategy that starts
        # background helpers must not leave them running after returning its answer.
        completed = subprocess.CompletedProcess(args, process.returncode, stdout, stderr)
        if os.name != "nt":
            # `communicate()` only waits for the direct child. Descendants can remain in the
            # session created above after a successful response, so reap the owned process
            # group before returning to the caller.
            _terminate_tree(process)
        return completed
    finally:
        if windows_job is not None:
            _close_windows_job(windows_job)


def _terminate_tree(process: subprocess.Popen[str]) -> None:
    if os.name == "nt":
        try:
            subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=10,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            pass
    else:
        _signal_process_group(process.pid, signal.SIGTERM)
        time.sleep(0.2)
        _signal_process_group(process.pid, signal.SIGKILL)
    if process.poll() is None:
        process.kill()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def _signal_process_group(process_id: int, signum: int) -> None:
    try:
        os.killpg(process_id, signum)
    except ProcessLookupError:
        pass
    except PermissionError:
        # The direct child still gets a final kill/wait in _terminate_tree.
        pass


def _attach_windows_job(process: subprocess.Popen[str]) -> int | None:
    if os.name != "nt":
        return None

    class IoCounters(ctypes.Structure):
        _fields_ = [(name, ctypes.c_ulonglong) for name in (
            "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
            "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
        )]

    class BasicLimitInformation(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_longlong),
            ("PerJobUserTimeLimit", ctypes.c_longlong),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class ExtendedLimitInformation(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", BasicLimitInformation),
            ("IoInfo", IoCounters),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateJobObjectW.restype = wintypes.HANDLE
    kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    kernel32.SetInformationJobObject.argtypes = [
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD
    ]
    kernel32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    handle = kernel32.CreateJobObjectW(None, None)
    if not handle:
        return None
    information = ExtendedLimitInformation()
    information.BasicLimitInformation.LimitFlags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    configured = kernel32.SetInformationJobObject(
        handle, 9, ctypes.byref(information), ctypes.sizeof(information)
    )
    assigned = configured and kernel32.AssignProcessToJobObject(
        handle, wintypes.HANDLE(process._handle)  # type: ignore[attr-defined]
    )
    if not assigned:
        kernel32.CloseHandle(handle)
        return None
    return int(handle)


def _close_windows_job(handle: int) -> None:
    if os.name == "nt":
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle.restype = wintypes.BOOL
        if not kernel32.CloseHandle(wintypes.HANDLE(handle)):
            raise ctypes.WinError(ctypes.get_last_error())
