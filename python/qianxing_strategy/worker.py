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
import threading
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


def _watch_parent_exit() -> threading.Event:
    """父进程关掉这根 stdin 管道时置位——共享内存 ring 自己没有 EOF。

    ring 传输只有"下一颗请求"，没有"父进程已经不在了"这一格：父进程走
    `std::process::exit` 时不展开任何析构，这里的循环就留在 1 kHz 轮询里自转到
    天荒地老，两份 ring 文件也留在 temp 里。`strategy_host.rs` 现在在共享模式下
    同样为子进程留着 stdin 管道的写端，进程一死写端由 OS 关掉，这条阻塞读拿到
    EOF 就是父进程已经不在了的信号。
    """
    gone = threading.Event()

    def watch() -> None:
        stream = getattr(sys.stdin, "buffer", None) or sys.stdin
        try:
            while stream.read(1):
                pass
        except (OSError, ValueError):
            pass  # 管道被拆封与读到 EOF 是同一件事：父进程没了
        gone.set()

    threading.Thread(target=watch, name="qx-parent-exit-watch", daemon=True).start()
    return gone


def serve_shared(
    handler: Callable[[StrategyInput], Any],
    input_path: str,
    output_path: str,
    capacity: int,
    slot_bytes: int,
    columnar: bool = False,
) -> None:
    """Serve QXSF requests over two SPSC mmap rings."""
    parent_gone = _watch_parent_exit()
    with SharedMemoryRing(input_path, capacity, slot_bytes) as input_ring, SharedMemoryRing(
        output_path, capacity, slot_bytes
    ) as output_ring:
        while not parent_gone.is_set():
            try:
                encoded = input_ring.try_pop()
            except RingEmpty:
                time.sleep(0.001)
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

    if parent_gone.is_set():
        # 能走出上面那个循环只有"父进程已经不在了"一条路：正常情况下这两份文件由父进程
        # 的析构删除（它也只会在这之后 kill 子进程）。mmap 已经随 with 解掉，这里补删。
        for path in (input_path, output_path):
            try:
                os.unlink(path)
            except OSError:
                pass


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
        )
    else:
        serve(handler)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
