"""JSONL runner for user-defined Python strategies.

The configured module should expose ``on_event(request)`` and return either a
``StrategyOutput`` or a mapping accepted by ``StrategyOutput.from_dict``.
``on_decision(request)`` remains a legacy alias. The worker never receives
credentials and never owns a CCXT client.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import importlib
import importlib.util
import json
import os
import sys
import time
from pathlib import Path
from typing import Any, Callable

from . import StrategyInput, StrategyOutput
from .columnar import decode_request as decode_columnar_request
from .frame import REQUEST, RESPONSE, encode_frame, read_frame
from .ring import RingEmpty, SharedMemoryRing


def _load_handler(reference: str) -> Callable[[StrategyInput], Any]:
    path = Path(reference)
    if path.exists() and path.suffix == ".py":
        spec = importlib.util.spec_from_file_location("qianxing_user_strategy", path)
        if spec is None or spec.loader is None:
            raise ValueError(f"cannot load strategy module: {reference}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    else:
        module = importlib.import_module(reference)
    expected = os.environ.get("QX_STRATEGY_ARTIFACT_SHA256", "").strip()
    if expected:
        if len(expected) != 64 or any(char not in "0123456789abcdefABCDEF" for char in expected):
            raise ValueError("QX_STRATEGY_ARTIFACT_SHA256 must be 64 hex characters")
        module_path = getattr(module, "__file__", None)
        if not module_path:
            raise ValueError("strategy artifact fingerprint requires a file-backed module")
        artifact = Path(module_path)
        if not artifact.is_file():
            raise ValueError(f"strategy artifact is not a regular file: {artifact}")
        actual = hashlib.sha256(artifact.read_bytes()).hexdigest()
        if actual.lower() != expected.lower():
            raise ValueError(
                f"strategy artifact SHA-256 mismatch: expected={expected} actual={actual}"
            )
    handler = getattr(module, "on_event", None)
    if not callable(handler):
        handler = getattr(module, "on_decision", None)
    if not callable(handler):
        raise ValueError("strategy module must expose callable on_event(request)")
    return handler


def _handle_line(handler: Callable[[StrategyInput], Any], line: str) -> str:
    try:
        request = StrategyInput.from_json(line)
        value = handler(request)
        if isinstance(value, StrategyOutput):
            output = value
        elif isinstance(value, dict):
            output = StrategyOutput.from_dict(value, request)
        else:
            raise ValueError("on_decision must return StrategyOutput or dict")
        return json.dumps(
            {"ok": True, "output": output.to_dict(request)},
            separators=(",", ":"),
            ensure_ascii=False,
        )
    except Exception as error:  # keep one response per request for supervisor recovery
        return json.dumps(
            {"ok": False, "error": str(error)},
            separators=(",", ":"),
            ensure_ascii=False,
        )


def serve(handler: Callable[[StrategyInput], Any]) -> None:
    for line in sys.stdin:
        if line.strip():
            sys.stdout.write(_handle_line(handler, line))
            sys.stdout.write("\n")
            sys.stdout.flush()


def serve_framed(handler: Callable[[StrategyInput], Any]) -> None:
    input_stream = sys.stdin.buffer
    output_stream = sys.stdout.buffer
    while True:
        frame = read_frame(input_stream)
        if frame is None:
            return
        kind, sequence, payload = frame
        if kind != REQUEST:
            raise ValueError(f"strategy worker expected request frame, got kind={kind}")
        response = _handle_line(handler, payload.decode("utf-8"))
        output_stream.write(encode_frame(RESPONSE, sequence, response.encode("utf-8")))
        output_stream.flush()


# 空闲侧最多每这么多秒确认一次父进程还在（每次确认都要开一次进程句柄）。
PARENT_LIVENESS_PROBE_SECONDS = 1.0

_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_ERROR_ACCESS_DENIED = 5
_PROCESS_STILL_ACTIVE = 259


def _parent_process_alive(pid: int) -> bool:
    """父进程是否还在运行。

    问不出结论时一律回答"还在"：把健康的 worker 误杀比让它多活一会儿更糟。
    """
    if pid <= 0:
        return True
    if os.name == "nt":
        import ctypes

        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            # 打不开句柄：被安全策略拒绝时进程仍在，只有 pid 已不存在才当真没了。
            return ctypes.GetLastError() == _ERROR_ACCESS_DENIED
        try:
            exit_code = ctypes.c_ulong()
            if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
                return True
            return exit_code.value == _PROCESS_STILL_ACTIVE
        finally:
            kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def serve_shared(
    handler: Callable[[StrategyInput], Any],
    input_path: str,
    output_path: str,
    capacity: int,
    slot_bytes: int,
    columnar: bool = False,
    parent_pid: int = 0,
) -> None:
    """Serve QXSF requests over two SPSC mmap rings.

    ``parent_pid`` is the Rust process that spawned this worker. The ring transport has
    no stdin to close, so the only normal exit is the parent's ``Drop`` killing us; if the
    parent is killed outright, the idle loop checks the parent's liveness and returns
    instead of spinning forever on the ring files.
    """
    with SharedMemoryRing(input_path, capacity, slot_bytes) as input_ring, SharedMemoryRing(
        output_path, capacity, slot_bytes
    ) as output_ring:
        next_parent_check = time.monotonic() + PARENT_LIVENESS_PROBE_SECONDS
        while True:
            try:
                encoded = input_ring.try_pop()
            except RingEmpty:
                time.sleep(0.001)
                now = time.monotonic()
                if parent_pid > 0 and now >= next_parent_check:
                    next_parent_check = now + PARENT_LIVENESS_PROBE_SECONDS
                    if not _parent_process_alive(parent_pid):
                        return
                continue
            frame = read_frame(io.BytesIO(encoded))
            if frame is None:
                raise ValueError("shared strategy request frame is empty")
            kind, sequence, payload = frame
            if kind != REQUEST:
                raise ValueError(f"strategy worker expected request frame, got kind={kind}")
            if columnar:
                request = decode_columnar_request(payload)
                response = _handle_line(handler, request.to_json())
            else:
                response = _handle_line(handler, payload.decode("utf-8"))
            encoded_response = encode_frame(RESPONSE, sequence, response.encode("utf-8"))
            output_ring.push_wait(encoded_response, time.monotonic() + 30.0)


def main() -> int:
    parser = argparse.ArgumentParser(description="Qianxing Python strategy JSONL worker")
    parser.add_argument("--module", required=True, help="module name or .py file")
    parser.add_argument(
        "--protocol",
        choices=("jsonl", "framed_json", "shared_memory_json", "shared_memory_columnar"),
        default="jsonl",
        help="strategy transport protocol",
    )
    parser.add_argument("--input-ring", help="shared-memory input ring path")
    parser.add_argument("--output-ring", help="shared-memory output ring path")
    parser.add_argument("--ring-capacity", type=int, default=1024)
    parser.add_argument("--ring-slot-bytes", type=int, default=64 * 1024)
    parser.add_argument(
        "--parent-pid",
        type=int,
        default=0,
        help="spawned-by process id; the shared-memory loop exits when it is gone (0 disables the check)",
    )
    args = parser.parse_args()
    handler = _load_handler(args.module)
    if args.protocol == "framed_json":
        serve_framed(handler)
    elif args.protocol in ("shared_memory_json", "shared_memory_columnar"):
        if not args.input_ring or not args.output_ring:
            parser.error("shared_memory_json requires --input-ring and --output-ring")
        serve_shared(
            handler,
            args.input_ring,
            args.output_ring,
            args.ring_capacity,
            args.ring_slot_bytes,
            columnar=args.protocol == "shared_memory_columnar",
            parent_pid=args.parent_pid,
        )
    else:
        serve(handler)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
