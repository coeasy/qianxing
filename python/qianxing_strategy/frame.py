"""Qianxing Strategy API v1 framed transport.

The payload remains the validated JSON contract for compatibility. The frame
adds bounded length, sequence and CRC32 semantics so binary transports do not
depend on newline scanning or accidental partial writes.
"""

from __future__ import annotations

import struct
import zlib
from typing import BinaryIO

MAGIC = b"QXSF"
VERSION = 1
HEADER = struct.Struct("<4sHBBQII")
HEADER_SIZE = HEADER.size
MAX_FRAME_BYTES = 16 * 1024 * 1024
REQUEST = 1
RESPONSE = 2
ERROR = 3


def _read_exact(stream: BinaryIO, size: int) -> bytes:
    chunks: list[bytes] = []
    remaining = size
    while remaining:
        chunk = stream.read(remaining)
        if not chunk:
            raise EOFError("strategy frame ended before payload completed")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_frame(stream: BinaryIO, max_frame_bytes: int = MAX_FRAME_BYTES):
    header = stream.read(HEADER_SIZE)
    if not header:
        return None
    if len(header) != HEADER_SIZE:
        raise ValueError("strategy frame header is truncated")
    magic, version, kind, flags, sequence, payload_len, expected_crc = HEADER.unpack(header)
    if magic != MAGIC:
        raise ValueError("strategy frame magic mismatch")
    if version != VERSION:
        raise ValueError(f"unsupported strategy frame version: {version}")
    if flags != 0:
        raise ValueError("strategy frame has unsupported flags")
    total = HEADER_SIZE + payload_len
    if total > max_frame_bytes:
        raise ValueError(f"strategy frame exceeds limit: bytes={total} max={max_frame_bytes}")
    payload = _read_exact(stream, payload_len)
    actual_crc = zlib.crc32(payload) & 0xFFFFFFFF
    if actual_crc != expected_crc:
        raise ValueError(
            f"strategy frame CRC32 mismatch: expected={expected_crc:08x} actual={actual_crc:08x}"
        )
    if kind not in (REQUEST, RESPONSE, ERROR):
        raise ValueError(f"unknown strategy frame kind: {kind}")
    return kind, sequence, payload


def encode_frame(kind: int, sequence: int, payload: bytes, max_frame_bytes: int = MAX_FRAME_BYTES) -> bytes:
    total = HEADER_SIZE + len(payload)
    if total > max_frame_bytes:
        raise ValueError(f"strategy frame exceeds limit: bytes={total} max={max_frame_bytes}")
    return HEADER.pack(
        MAGIC,
        VERSION,
        kind,
        0,
        sequence,
        len(payload),
        zlib.crc32(payload) & 0xFFFFFFFF,
    ) + payload
