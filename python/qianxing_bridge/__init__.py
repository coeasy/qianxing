"""牵星跨语言交换边界。

Python 侧只消费版本化 JSON；它不会把可变 DataFrame 写回 Kernel。需要 pandas/pyarrow
时由调用方显式转换，原始定点整数列始终保留。
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from fractions import Fraction
from typing import Any

# 子模块入口（T2-2）：`app` 是应用层用例的 Python 门面，`native` 是 PyO3 扩展桥。
# 这里只 import 模块、不触发扩展加载——`native._extension()` 是按需加载的，
# 扩展没构建时 `import qianxing_bridge` 仍然可用；依赖 Rust 契约的 digest 与变换清单
# 在调用时会明确提示构建扩展，不在 Python 再藏一份哈希算法。
from . import app, native  # noqa: F401

#: BarFrame JSON 契约版本。落盘/交换文档在顶层显式携带该值，Rust 侧
#: （``qx-data`` 的 ``JsonBarFrameProvider``）据此切换兼容分支与严格模式：
#: 缺省/0 表示旧格式（允许未知列、不强制 ``source``），``>=1`` 表示严格模式。
BAR_FRAME_SCHEMA_VERSION = 2

#: v1 BarFrame JSON 的完整字段集合；两侧任何新增列都必须同步更新这里。
BAR_FRAME_JSON_FIELDS = (
    "instrument",
    "source",
    "ts",
    "open_raw",
    "high_raw",
    "low_raw",
    "close_raw",
    "volume_raw",
)

#: v1 允许的顶层键：内容列 + 契约版本。Rust 严格模式的 `deny_unknown_fields`
#: 使用同一集合（`qx-data/src/provider.rs` 的 `StrictBarFrameJson`）。
BAR_FRAME_STRICT_FIELDS = ("schema_version", *BAR_FRAME_JSON_FIELDS)
BAR_FRAME_EXACT_FIELDS = (*BAR_FRAME_JSON_FIELDS, "exact_ohlcv")


def read_bar_frame_schema_version(payload: "str | dict[str, Any]") -> int:
    """读取 BarFrame JSON 顶层 ``schema_version``，缺省视为 0（旧格式）。"""

    value = json.loads(payload) if isinstance(payload, str) else payload
    if not isinstance(value, dict):
        raise ValueError("BarFrame JSON must be an object")
    raw = value.get("schema_version", 0)
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise ValueError("schema_version must be an integer")
    if raw < 0 or raw > BAR_FRAME_SCHEMA_VERSION:
        raise ValueError(f"unsupported BarFrame schema_version: {raw}")
    return raw


def _exact_decimal(value: Any) -> Fraction:
    if not isinstance(value, dict) or set(value) != {"coefficient", "scale"}:
        raise ValueError("exact OHLCV values require coefficient and scale")
    coefficient = value["coefficient"]
    scale = value["scale"]
    if (
        not isinstance(coefficient, str)
        or re.fullmatch(r"-?(?:0|[1-9][0-9]*)", coefficient) is None
        or isinstance(scale, bool)
        or not isinstance(scale, int)
        or not 0 <= scale <= 18
    ):
        raise ValueError("exact OHLCV decimal coefficient or scale is invalid")
    return Fraction(int(coefficient), 10**scale)


def _exact_decimal_to_raw(value: dict[str, Any]) -> int:
    coefficient = int(value["coefficient"])
    scale = value["scale"]
    if scale <= 9:
        return coefficient * 10 ** (9 - scale)
    divisor = 10 ** (scale - 9)
    magnitude = abs(coefficient) // divisor
    return -magnitude if coefficient < 0 else magnitude


def _validate_exact_ohlcv_row(row: Any) -> tuple[dict[str, str | int], ...]:
    names = ("open", "high", "low", "close", "volume")
    if not isinstance(row, dict) or set(row) != set(names):
        raise ValueError("each exact_ohlcv row requires exactly open/high/low/close/volume")
    decimals = tuple(row[name] for name in names)
    values = tuple(_exact_decimal(value) for value in decimals)
    open_value, high_value, low_value, close_value, volume_value = values
    if min(open_value, high_value, low_value, close_value) <= 0 or volume_value < 0:
        raise ValueError("exact OHLCV prices must be positive and volume non-negative")
    if high_value < max(open_value, low_value, close_value):
        raise ValueError("exact OHLCV high must contain open, low and close")
    if low_value > min(open_value, high_value, close_value):
        raise ValueError("exact OHLCV low must contain open, high and close")
    return decimals


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
    exact_ohlcv: tuple[dict[str, dict[str, str | int]], ...] | None = None

    @classmethod
    def from_json(cls, payload: str) -> "BarFrame":
        value = json.loads(payload)
        schema_version = read_bar_frame_schema_version(value)
        if schema_version >= 1:
            if not isinstance(value.get("instrument"), str) or not value["instrument"].strip():
                raise ValueError("BarFrame schema_version>=1 requires a non-empty instrument")
            if not isinstance(value.get("source"), str) or not value["source"].strip():
                raise ValueError("BarFrame schema_version>=1 requires a non-empty source")
            missing = sorted(set(BAR_FRAME_JSON_FIELDS).difference(value))
            if missing:
                raise ValueError(f"BarFrame schema_version={schema_version} missing {missing}")
            allowed = (
                set(BAR_FRAME_STRICT_FIELDS)
                if schema_version == 1
                else {"schema_version", *BAR_FRAME_EXACT_FIELDS}
            )
            unknown = sorted(set(value).difference(allowed))
            if unknown:
                raise ValueError(f"BarFrame schema_version={schema_version} unknown {unknown}")
        if schema_version == 2 and "exact_ohlcv" not in value:
            raise ValueError("BarFrame schema_version=2 requires exact_ohlcv")
        if schema_version < 2 and "exact_ohlcv" in value:
            raise ValueError("BarFrame exact_ohlcv requires schema_version=2")
        frame = cls(
            instrument=value["instrument"],
            source=value["source"],
            ts=tuple(value["ts"]),
            open_raw=tuple(value["open_raw"]),
            high_raw=tuple(value["high_raw"]),
            low_raw=tuple(value["low_raw"]),
            close_raw=tuple(value["close_raw"]),
            volume_raw=tuple(value["volume_raw"]),
            exact_ohlcv=(
                tuple(value["exact_ohlcv"])
                if schema_version == 2 and isinstance(value["exact_ohlcv"], list)
                else None
            ),
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
        if not self.source.strip():
            raise ValueError("BarFrame source is required")
        if not self.ts:
            raise ValueError("empty BarFrame")
        if any(len(column) != len(self.ts) for column in columns):
            raise ValueError("column length mismatch")
        if any(left >= right for left, right in zip(self.ts, self.ts[1:])):
            raise ValueError("timestamps must be strictly increasing")
        if self.exact_ohlcv is not None:
            if len(self.exact_ohlcv) != len(self.ts):
                raise ValueError("exact_ohlcv row count must match BarFrame")
            for index, row in enumerate(self.exact_ohlcv):
                exact = _validate_exact_ohlcv_row(row)
                raw = (
                    self.open_raw[index],
                    self.high_raw[index],
                    self.low_raw[index],
                    self.close_raw[index],
                    self.volume_raw[index],
                )
                if tuple(_exact_decimal_to_raw(value) for value in exact) != raw:
                    raise ValueError(f"exact_ohlcv raw projection mismatch at row {index}")

    def to_json(self) -> str:
        self.validate()
        schema_version = 2 if self.exact_ohlcv is not None else 1
        value = {
            "schema_version": schema_version,
            "instrument": self.instrument,
            "source": self.source,
            "ts": list(self.ts),
            "open_raw": list(self.open_raw),
            "high_raw": list(self.high_raw),
            "low_raw": list(self.low_raw),
            "close_raw": list(self.close_raw),
            "volume_raw": list(self.volume_raw),
        }
        if self.exact_ohlcv is not None:
            value["exact_ohlcv"] = list(self.exact_ohlcv)
        return json.dumps(
            value,
            separators=(",", ":"),
            ensure_ascii=False,
        )

    def digest(self) -> int:
        """Return the Rust BarFrame digest through the native compatibility bridge."""
        return native.frame_digest(self.to_json())

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
            exact_ohlcv=(
                tuple(self.exact_ohlcv[i] for i in indexes)
                if self.exact_ohlcv is not None
                else None
            ),
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
    """解析 Qianxing account wire JSON，并执行最小 schema 级校验。

    必填集合与 v1/v2/v3 account snapshot schema 的 ``required`` 是同一份口径（含
    ``equity_raw``）：两侧各自宽严时，同一份产物会一边解得开、一边解不开。必填说的是**键必须在**，不是值必须算得出——八个汇总钱字段在协议上全是 ``Option``，未算那一格写侧印 ``null``。
    """

    value = json.loads(payload)
    required = {
        "protocol",
        "schema_version",
        "header",
        "cash_raw",
        "equity_raw",
        "positions",
        "orders",
        "fills",
        "transfers",
        "reconcile",
    }
    missing = sorted(required.difference(value))
    if missing:
        raise ValueError(f"missing snapshot fields: {missing}")
    if value["protocol"] != "QIANXING_ACCOUNT" or value["schema_version"] not in (1, 2, 3):
        raise ValueError("unsupported account snapshot schema")
    header_version = value["header"].get("schema_version", value["schema_version"])
    if header_version != value["schema_version"]:
        raise ValueError("account snapshot schema version mismatch")
    if value["schema_version"] == 1 and any(
        position.get("settlement_currency") is not None
        for position in value["positions"].values()
    ):
        raise ValueError("v1 account snapshot cannot declare position settlement_currency")
    if value["schema_version"] < 3 and (
        value.get("cash_exact")
        or any(position.get("exact_amounts") is not None for position in value["positions"].values())
    ):
        raise ValueError("v1/v2 account snapshot cannot declare exact balances or positions")
    cash_exact = value.get("cash_exact", {})
    if not isinstance(cash_exact, dict):
        raise ValueError("cash_exact must be an object")
    for currency, exact in cash_exact.items():
        if not isinstance(exact, dict) or set(exact) != {"coefficient", "scale"}:
            raise ValueError(f"cash_exact.{currency} must be an exact decimal")
        coefficient_text = exact["coefficient"]
        digits = coefficient_text[1:] if isinstance(coefficient_text, str) and coefficient_text.startswith("-") else coefficient_text
        if (
            not isinstance(coefficient_text, str)
            or not digits
            or not digits.isascii()
            or not digits.isdigit()
            or (len(digits) > 1 and digits.startswith("0"))
        ):
            raise ValueError(f"cash_exact.{currency}.coefficient is invalid")
        coefficient = int(coefficient_text)
        scale = exact["scale"]
        if not isinstance(scale, int) or isinstance(scale, bool) or not 0 <= scale <= 18:
            raise ValueError(f"cash_exact.{currency}.scale is invalid")
        raw = value["cash_raw"].get(currency)
        if not isinstance(raw, int) or isinstance(raw, bool):
            raise ValueError(f"cash_exact.{currency} has no integer cash_raw projection")
        projected = coefficient * 10 ** (9 - scale) if scale <= 9 else (
            abs(coefficient) // 10 ** (scale - 9)
        ) * (-1 if coefficient < 0 else 1)
        if projected != raw:
            raise ValueError(f"cash_exact.{currency} disagrees with cash_raw projection")
    return value
