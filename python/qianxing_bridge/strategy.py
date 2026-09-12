"""版本化 Python 策略 JSON 契约。

策略进程只能接收不可变定点输入并返回 Signal/Portfolio/OrderIntent 目标，不得直接访问
交易所、控制面或账簿。协议与 Rust ``qx_runtime`` 中的
``StrategyContractInput/Output`` 一一对应，便于 JSONL、Arrow 和其他语言复用。
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Mapping


SCHEMA_VERSION = 1


def _require_text(value: Any, name: str) -> str:
    text = str(value)
    if not text.strip():
        raise ValueError(f"{name} must be non-empty")
    return text


@dataclass(frozen=True)
class StrategyBars:
    source: str
    ts: tuple[int, ...]
    open_raw: tuple[int, ...]
    high_raw: tuple[int, ...]
    low_raw: tuple[int, ...]
    close_raw: tuple[int, ...]
    volume_raw: tuple[int, ...]

    def validate(self) -> None:
        _require_text(self.source, "bars.source")
        if not self.ts:
            raise ValueError("bars.ts must be non-empty")
        columns = (
            self.open_raw,
            self.high_raw,
            self.low_raw,
            self.close_raw,
            self.volume_raw,
        )
        if any(len(column) != len(self.ts) for column in columns):
            raise ValueError("strategy bars column length mismatch")
        if any(left >= right for left, right in zip(self.ts, self.ts[1:])):
            raise ValueError("strategy bars timestamps must be strictly increasing")

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        return {
            "source": self.source,
            "ts": list(self.ts),
            "open_raw": list(self.open_raw),
            "high_raw": list(self.high_raw),
            "low_raw": list(self.low_raw),
            "close_raw": list(self.close_raw),
            "volume_raw": list(self.volume_raw),
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "StrategyBars":
        bars = cls(
            source=str(value["source"]),
            ts=tuple(int(item) for item in value["ts"]),
            open_raw=tuple(int(item) for item in value["open_raw"]),
            high_raw=tuple(int(item) for item in value["high_raw"]),
            low_raw=tuple(int(item) for item in value["low_raw"]),
            close_raw=tuple(int(item) for item in value["close_raw"]),
            volume_raw=tuple(int(item) for item in value["volume_raw"]),
        )
        bars.validate()
        return bars


@dataclass(frozen=True)
class StrategyInput:
    request_id: str
    strategy_id: str
    strategy_version: str
    data_fingerprint: str
    as_of: int
    instrument: str
    positions: Mapping[str, int] = field(default_factory=dict)
    cash: Mapping[str, int] = field(default_factory=dict)
    available_margin_raw: int | None = None
    risk_state: str = ""
    research_targets: Mapping[str, int] = field(default_factory=dict)
    bars: StrategyBars | None = None
    schema_version: int = SCHEMA_VERSION

    def validate(self) -> None:
        if self.schema_version != SCHEMA_VERSION:
            raise ValueError(f"unsupported strategy schema_version: {self.schema_version}")
        for value, name in (
            (self.request_id, "request_id"),
            (self.strategy_id, "strategy_id"),
            (self.strategy_version, "strategy_version"),
            (self.data_fingerprint, "data_fingerprint"),
            (self.instrument, "instrument"),
            (self.risk_state, "risk_state"),
        ):
            _require_text(value, name)
        if self.as_of <= 0:
            raise ValueError("as_of must be positive")
        if self.available_margin_raw is not None and self.available_margin_raw < 0:
            raise ValueError("available_margin_raw cannot be negative")
        for mapping_name, mapping in (
            ("positions", self.positions),
            ("research_targets", self.research_targets),
        ):
            for instrument in mapping:
                _require_text(instrument, f"{mapping_name}.instrument")
        if self.bars is not None:
            self.bars.validate()

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        return {
            "schema_version": self.schema_version,
            "request_id": self.request_id,
            "strategy_id": self.strategy_id,
            "strategy_version": self.strategy_version,
            "data_fingerprint": self.data_fingerprint,
            "as_of": self.as_of,
            "instrument": self.instrument,
            "positions": dict(self.positions),
            "cash": dict(self.cash),
            "available_margin_raw": self.available_margin_raw,
            "risk_state": self.risk_state,
            "research_targets": dict(self.research_targets),
            "bars": None if self.bars is None else self.bars.to_dict(),
        }

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":"), ensure_ascii=False)

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "StrategyInput":
        bars_value = value.get("bars")
        result = cls(
            schema_version=int(value.get("schema_version", 0)),
            request_id=str(value["request_id"]),
            strategy_id=str(value["strategy_id"]),
            strategy_version=str(value["strategy_version"]),
            data_fingerprint=str(value["data_fingerprint"]),
            as_of=int(value["as_of"]),
            instrument=str(value["instrument"]),
            positions={str(key): int(item) for key, item in dict(value.get("positions", {})).items()},
            cash={str(key): int(item) for key, item in dict(value.get("cash", {})).items()},
            available_margin_raw=(
                None if value.get("available_margin_raw") is None else int(value["available_margin_raw"])
            ),
            risk_state=str(value["risk_state"]),
            research_targets={
                str(key): int(item) for key, item in dict(value.get("research_targets", {})).items()
            },
            bars=None if bars_value is None else StrategyBars.from_dict(bars_value),
        )
        result.validate()
        return result

    @classmethod
    def from_json(cls, payload: str) -> "StrategyInput":
        value = json.loads(payload)
        if not isinstance(value, dict):
            raise ValueError("strategy input must be a JSON object")
        return cls.from_dict(value)


@dataclass(frozen=True)
class StrategyIntent:
    """跨语言可交换的单笔订单意图；最终订单仍由 Rust Risk/OMS 创建。"""

    intent_id: int
    instrument: str
    side: str
    qty_raw: int
    limit_price_raw: int | None = None
    reduce_only: bool = False
    post_only: bool = False
    position_side: str | None = None

    def validate(self) -> None:
        if self.intent_id <= 0:
            raise ValueError("intent_id must be positive")
        _require_text(self.instrument, "intent.instrument")
        if self.side.lower() not in {"buy", "sell"}:
            raise ValueError("intent.side must be buy or sell")
        if self.qty_raw <= 0:
            raise ValueError("intent.qty_raw must be positive")
        if self.limit_price_raw is not None and self.limit_price_raw <= 0:
            raise ValueError("intent.limit_price_raw must be positive")
        if self.position_side is not None and self.position_side.lower() not in {
            "net",
            "long",
            "short",
        }:
            raise ValueError("intent.position_side must be net, long or short")

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        return {
            "intent_id": self.intent_id,
            "instrument": self.instrument,
            "side": self.side.lower(),
            "qty_raw": self.qty_raw,
            "limit_price_raw": self.limit_price_raw,
            "reduce_only": self.reduce_only,
            "post_only": self.post_only,
            "position_side": None if self.position_side is None else self.position_side.lower(),
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "StrategyIntent":
        result = cls(
            intent_id=int(value["intent_id"]),
            instrument=str(value["instrument"]),
            side=str(value["side"]),
            qty_raw=int(value["qty_raw"]),
            limit_price_raw=(
                None if value.get("limit_price_raw") is None else int(value["limit_price_raw"])
            ),
            reduce_only=bool(value.get("reduce_only", False)),
            post_only=bool(value.get("post_only", False)),
            position_side=(
                None if value.get("position_side") is None else str(value["position_side"])
            ),
        )
        result.validate()
        return result


@dataclass(frozen=True)
class StrategyOutput:
    request_id: str
    strategy_id: str
    signal_id: int
    instrument: str
    target_qty: int
    confidence: int = 0
    priority: int = 0
    expires_at: int = 0
    intents: tuple[StrategyIntent, ...] = field(default_factory=tuple)
    schema_version: int = SCHEMA_VERSION

    def validate_for(self, request: StrategyInput) -> None:
        request.validate()
        if (
            self.schema_version != SCHEMA_VERSION
            or self.request_id != request.request_id
            or self.strategy_id != request.strategy_id
            or self.signal_id <= 0
            or self.instrument != request.instrument
            or not self.instrument.strip()
            or (self.expires_at != 0 and self.expires_at < request.as_of)
        ):
            raise ValueError("strategy output identity or expiry does not match input")
        intent_ids: set[int] = set()
        for intent in self.intents:
            intent.validate()
            if intent.intent_id in intent_ids:
                raise ValueError("strategy output contains duplicate intent_id")
            intent_ids.add(intent.intent_id)

    def to_dict(self, request: StrategyInput) -> dict[str, Any]:
        self.validate_for(request)
        return {
            "schema_version": self.schema_version,
            "request_id": self.request_id,
            "strategy_id": self.strategy_id,
            "signal_id": self.signal_id,
            "instrument": self.instrument,
            "target_qty": self.target_qty,
            "confidence": self.confidence,
            "priority": self.priority,
            "expires_at": self.expires_at,
            "intents": [intent.to_dict() for intent in self.intents],
        }

    def to_json(self, request: StrategyInput) -> str:
        return json.dumps(self.to_dict(request), separators=(",", ":"), ensure_ascii=False)

    @classmethod
    def from_dict(cls, value: Mapping[str, Any], request: StrategyInput) -> "StrategyOutput":
        result = cls(
            schema_version=int(value.get("schema_version", 0)),
            request_id=str(value["request_id"]),
            strategy_id=str(value["strategy_id"]),
            signal_id=int(value["signal_id"]),
            instrument=str(value["instrument"]),
            target_qty=int(value["target_qty"]),
            confidence=int(value.get("confidence", 0)),
            priority=int(value.get("priority", 0)),
            expires_at=int(value.get("expires_at", 0)),
            intents=tuple(StrategyIntent.from_dict(item) for item in value.get("intents", [])),
        )
        result.validate_for(request)
        return result

    @classmethod
    def from_json(cls, payload: str, request: StrategyInput) -> "StrategyOutput":
        value = json.loads(payload)
        if not isinstance(value, dict):
            raise ValueError("strategy output must be a JSON object")
        return cls.from_dict(value, request)
