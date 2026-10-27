"""Fixed-width columnar strategy request payload (QXCB v1)."""

from __future__ import annotations

import json
import struct

from qianxing_strategy import StrategyBars, StrategyInput

MAGIC = b"QXCB"
VERSION = 1
EXACT_OHLCV_VERSION = 2
EXACT_OHLCV_FLAG = 1
HEADER = struct.Struct("<4sHHII")
HEADER_SIZE = HEADER.size
MAX_BYTES = 16 * 1024 * 1024


def _read_i128(payload: bytes, offset: int) -> tuple[int, int]:
    end = offset + 16
    if end > len(payload):
        raise ValueError("columnar strategy payload is truncated")
    return int.from_bytes(payload[offset:end], "little", signed=True), end


def decode_request(payload: bytes) -> StrategyInput:
    if len(payload) > MAX_BYTES or len(payload) < HEADER_SIZE:
        raise ValueError("columnar strategy payload exceeds bounds")
    magic, version, flags, metadata_len, rows = HEADER.unpack_from(payload)
    if magic != MAGIC or version not in (VERSION, EXACT_OHLCV_VERSION):
        raise ValueError("columnar strategy header mismatch")
    if version == VERSION and flags != 0:
        raise ValueError("columnar strategy v1 does not support flags")
    if version == EXACT_OHLCV_VERSION and flags != EXACT_OHLCV_FLAG:
        raise ValueError("columnar strategy v2 requires the exact OHLCV flag")
    if rows == 0:
        raise ValueError("columnar strategy payload must contain bars")
    metadata_start = HEADER_SIZE
    metadata_end = metadata_start + metadata_len
    if metadata_end > len(payload):
        raise ValueError("columnar strategy metadata is truncated")
    expected = metadata_end + rows * (8 + 5 * 16)
    if expected != len(payload):
        raise ValueError("columnar strategy column length mismatch")
    value = json.loads(payload[metadata_start:metadata_end].decode("utf-8"))
    if value.get("bars") is not None:
        raise ValueError("columnar strategy metadata must not contain bars")
    exact_ohlcv = value.pop("__qx_bars_exact_ohlcv", None)
    if version == VERSION and exact_ohlcv is not None:
        raise ValueError("columnar strategy v1 cannot contain exact OHLCV metadata")
    if version == EXACT_OHLCV_VERSION and (
        not isinstance(exact_ohlcv, list) or len(exact_ohlcv) != rows
    ):
        raise ValueError("columnar strategy v2 exact OHLCV row count mismatch")
    offset = metadata_end
    timestamps = []
    for _ in range(rows):
        timestamps.append(struct.unpack_from("<Q", payload, offset)[0])
        offset += 8
    columns: list[list[int]] = []
    for _ in range(5):
        column = []
        for _ in range(rows):
            item, offset = _read_i128(payload, offset)
            column.append(item)
        columns.append(column)
    source = str(value.pop("__qx_bars_source", "shared-columnar-v1"))
    bars = {
        "source": source,
        "ts": timestamps,
        "open_raw": columns[0],
        "high_raw": columns[1],
        "low_raw": columns[2],
        "close_raw": columns[3],
        "volume_raw": columns[4],
    }
    if exact_ohlcv is not None:
        bars["exact_ohlcv"] = exact_ohlcv
    value["bars"] = bars
    return StrategyInput.from_json(json.dumps(value, separators=(",", ":")))
