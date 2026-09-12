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


def serve_shared(
    handler: Callable[[StrategyInput], Any],
    input_path: str,
    output_path: str,
    capacity: int,
    slot_bytes: int,
    columnar: bool = False,
) -> None:
    """Serve QXSF requests over two SPSC mmap rings."""
    with SharedMemoryRing(input_path, capacity, slot_bytes) as input_ring, SharedMemoryRing(
        output_path, capacity, slot_bytes
    ) as output_ring:
        while True:
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
