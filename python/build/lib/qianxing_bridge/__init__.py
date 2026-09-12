"""牵星跨语言交换边界。

Python 侧只消费版本化 JSON；它不会把可变 DataFrame 写回 Kernel。需要 pandas/pyarrow
时由调用方显式转换，原始定点整数列始终保留。
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any


_FNV_OFFSET = 0xCBF29CE484222325
_FNV_PRIME = 0x100000001B3
_U64_MASK = (1 << 64) - 1


def _fnv_write_byte(value: int, byte: int) -> int:
    return ((value ^ byte) * _FNV_PRIME) & _U64_MASK


def _fnv_write_u64(value: int, number: int) -> int:
    for byte in (number & _U64_MASK).to_bytes(8, "little"):
        value = _fnv_write_byte(value, byte)
    return value


def _fnv_write_i128(value: int, number: int) -> int:
    for byte in number.to_bytes(16, "little", signed=True):
        value = _fnv_write_byte(value, byte)
    return value


def _fnv_write_text(value: int, text: str) -> int:
    encoded = text.encode("utf-8")
    value = _fnv_write_u64(value, len(encoded))
    for byte in encoded:
        value = _fnv_write_byte(value, byte)
    return value


@dataclass(frozen=True)
class TransformManifest:
    operation: str
    algorithm_version: str
    parameters: dict[str, str]
    input_hash: int
    output_hash: int

    def validate(self) -> None:
        if (
            not self.operation.strip()
            or not self.algorithm_version.strip()
            or self.input_hash == 0
            or self.output_hash == 0
        ):
            raise ValueError("TransformManifest missing operation/version/hash")

    def to_json(self) -> str:
        self.validate()
        return json.dumps(
            {
                "operation": self.operation,
                "algorithm_version": self.algorithm_version,
                "parameters": self.parameters,
                "input_hash": self.input_hash,
                "output_hash": self.output_hash,
            },
            separators=(",", ":"),
            ensure_ascii=False,
        )

    @classmethod
    def from_json(cls, payload: str) -> "TransformManifest":
        value = json.loads(payload)
        manifest = cls(
            operation=value["operation"],
            algorithm_version=value["algorithm_version"],
            parameters=dict(value["parameters"]),
            input_hash=value["input_hash"],
            output_hash=value["output_hash"],
        )
        manifest.validate()
        return manifest


@dataclass(frozen=True)
class BarFrame:
    instrument: str
    source: str
    ts: tuple[int, ...]
    open_raw: tuple[int, ...]
    high_raw: tuple[int, ...]
    low_raw: tuple[int, ...]
    close_raw: tuple[int, ...]
    volume_raw: tuple[int, ...]

    @classmethod
    def from_json(cls, payload: str) -> "BarFrame":
        value = json.loads(payload)
        frame = cls(
            instrument=value["instrument"],
            source=value["source"],
            ts=tuple(value["ts"]),
            open_raw=tuple(value["open_raw"]),
            high_raw=tuple(value["high_raw"]),
            low_raw=tuple(value["low_raw"]),
            close_raw=tuple(value["close_raw"]),
            volume_raw=tuple(value["volume_raw"]),
        )
        frame.validate()
        return frame

    def validate(self) -> None:
        columns = (
            self.ts,
            self.open_raw,
            self.high_raw,
            self.low_raw,
            self.close_raw,
            self.volume_raw,
        )
        if not self.ts:
            raise ValueError("empty BarFrame")
        if any(len(column) != len(self.ts) for column in columns):
            raise ValueError("column length mismatch")
        if any(left >= right for left, right in zip(self.ts, self.ts[1:])):
            raise ValueError("timestamps must be strictly increasing")

    def to_json(self) -> str:
        self.validate()
        return json.dumps(
            {
                "instrument": self.instrument,
                "source": self.source,
                "ts": list(self.ts),
                "open_raw": list(self.open_raw),
                "high_raw": list(self.high_raw),
                "low_raw": list(self.low_raw),
                "close_raw": list(self.close_raw),
                "volume_raw": list(self.volume_raw),
            },
            separators=(",", ":"),
            ensure_ascii=False,
        )

    def digest(self) -> int:
        value = _FNV_OFFSET
        value = _fnv_write_text(value, self.instrument)
        value = _fnv_write_text(value, self.source)
        value = _fnv_write_u64(value, len(self.ts))
        for columns in zip(
            self.ts,
            self.open_raw,
            self.high_raw,
            self.low_raw,
            self.close_raw,
            self.volume_raw,
        ):
            value = _fnv_write_u64(value, columns[0])
            for column in columns[1:]:
                value = _fnv_write_i128(value, column)
        return value

    def select_time(self, start: int, end: int) -> "BarFrame":
        if start > end:
            raise ValueError("invalid time interval")
        indexes = [i for i, ts in enumerate(self.ts) if start <= ts <= end]
        if not indexes:
            raise ValueError("empty time selection")
        selected = BarFrame(
            instrument=self.instrument,
            source=self.source,
            ts=tuple(self.ts[i] for i in indexes),
            open_raw=tuple(self.open_raw[i] for i in indexes),
            high_raw=tuple(self.high_raw[i] for i in indexes),
            low_raw=tuple(self.low_raw[i] for i in indexes),
            close_raw=tuple(self.close_raw[i] for i in indexes),
            volume_raw=tuple(self.volume_raw[i] for i in indexes),
        )
        selected.validate()
        return selected

    def select_time_with_manifest(
        self, start: int, end: int
    ) -> tuple["BarFrame", TransformManifest]:
        input_hash = self.digest()
        selected = self.select_time(start, end)
        manifest = TransformManifest(
            operation="select_time",
            algorithm_version="qx-datastruct/select-time-v1",
            parameters={"start": str(start), "end": str(end)},
            input_hash=input_hash,
            output_hash=selected.digest(),
        )
        return selected, manifest

    def resample(self, interval: int) -> "BarFrame":
        if interval <= 0:
            raise ValueError("interval must be positive")
        rows: dict[int, list[int]] = {}
        for i, ts in enumerate(self.ts):
            bucket = (ts // interval) * interval
            if bucket not in rows:
                rows[bucket] = [
                    self.open_raw[i],
                    self.high_raw[i],
                    self.low_raw[i],
                    self.close_raw[i],
                    self.volume_raw[i],
                ]
            else:
                row = rows[bucket]
                row[1] = max(row[1], self.high_raw[i])
                row[2] = min(row[2], self.low_raw[i])
                row[3] = self.close_raw[i]
                row[4] += self.volume_raw[i]
        result = BarFrame(
            instrument=self.instrument,
            source=self.source,
            ts=tuple(rows),
            open_raw=tuple(row[0] for row in rows.values()),
            high_raw=tuple(row[1] for row in rows.values()),
            low_raw=tuple(row[2] for row in rows.values()),
            close_raw=tuple(row[3] for row in rows.values()),
            volume_raw=tuple(row[4] for row in rows.values()),
        )
        result.validate()
        return result

    def resample_with_manifest(
        self, interval: int
    ) -> tuple["BarFrame", TransformManifest]:
        input_hash = self.digest()
        result = self.resample(interval)
        manifest = TransformManifest(
            operation="resample",
            algorithm_version="qx-datastruct/resample-floor-v1",
            parameters={"interval": str(interval)},
            input_hash=input_hash,
            output_hash=result.digest(),
        )
        return result, manifest

    def to_pandas(self) -> Any:
        import pandas as pd

        return pd.DataFrame(
            {
                "ts": self.ts,
                "open_raw": self.open_raw,
                "high_raw": self.high_raw,
                "low_raw": self.low_raw,
                "close_raw": self.close_raw,
                "volume_raw": self.volume_raw,
            }
        )

    def to_pyarrow(self) -> Any:
        import pyarrow as pa

        return pa.table(
            {
                "ts": self.ts,
                "open_raw": self.open_raw,
                "high_raw": self.high_raw,
                "low_raw": self.low_raw,
                "close_raw": self.close_raw,
                "volume_raw": self.volume_raw,
            }
        )

    def to_pyarrow_native(self) -> Any:
        """Use the optional Rust extension and Arrow C Data Interface bridge."""

        from .native import to_pyarrow_columns

        columns = to_pyarrow_columns(self.to_json())
        return __import__("pyarrow").table(
            {
                "ts": columns[0],
                "open_raw": columns[1],
                "high_raw": columns[2],
                "low_raw": columns[3],
                "close_raw": columns[4],
                "volume_raw": columns[5],
            }
        )


def load_account_snapshot(payload: str) -> dict[str, Any]:
    """解析 Qianxing account wire JSON，并执行最小 schema 级校验。"""

    value = json.loads(payload)
    required = {
        "protocol",
        "schema_version",
        "header",
        "cash_raw",
        "positions",
        "orders",
        "fills",
        "transfers",
        "reconcile",
    }
    missing = sorted(required.difference(value))
    if missing:
        raise ValueError(f"missing snapshot fields: {missing}")
    if value["protocol"] != "QIANXING_ACCOUNT" or value["schema_version"] != 1:
        raise ValueError("unsupported account snapshot schema")
    return value
