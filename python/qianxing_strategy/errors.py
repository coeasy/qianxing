"""Versioned strategy callback error contract shared with Rust and C/C++."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any, Mapping


STRATEGY_ERROR_SCHEMA_VERSION = 1
STRATEGY_ERROR_STAGES = frozenset(
    {
        "initialization",
        "history_requirements",
        "warmup_complete",
        "start",
        "market_event",
        "order_update",
        "fill_update",
        "account_update",
        "stop",
    }
)
MAX_STRATEGY_ERROR_MESSAGE_BYTES = 16 * 1024


def _bounded_message(error: BaseException) -> str:
    try:
        message = str(error) or type(error).__name__
    except BaseException:  # noqa: BLE001 - diagnostics must never mask the original failure.
        message = type(error).__name__
    encoded = message.encode("utf-8", errors="replace")
    if len(encoded) > MAX_STRATEGY_ERROR_MESSAGE_BYTES:
        encoded = encoded[:MAX_STRATEGY_ERROR_MESSAGE_BYTES]
        message = encoded.decode("utf-8", errors="ignore")
    return message or type(error).__name__


@dataclass(frozen=True, slots=True)
class StrategyErrorV1:
    """Bounded callback failure details with the same wire fields as Rust `StrategyErrorV1`."""

    schema_version: int
    stage: str
    strategy_id: str
    strategy_version: str
    timestamp: int | None
    instrument: str | None
    message: str

    @classmethod
    def from_exception(
        cls,
        *,
        stage: str,
        strategy_id: str,
        strategy_version: str,
        timestamp: int | None,
        instrument: str | None,
        error: BaseException,
    ) -> StrategyErrorV1:
        value = cls(
            schema_version=STRATEGY_ERROR_SCHEMA_VERSION,
            stage=stage,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            timestamp=timestamp,
            instrument=instrument,
            message=_bounded_message(error),
        )
        value.validate()
        return value

    @classmethod
    def from_context(
        cls,
        context: Mapping[str, Any],
        error: BaseException,
        *,
        stage: str,
        timestamp: int | None = None,
        instrument: str | None = None,
    ) -> StrategyErrorV1:
        raw_timestamp = context.get("as_of") if timestamp is None else timestamp
        normalized_timestamp = (
            raw_timestamp
            if isinstance(raw_timestamp, int)
            and not isinstance(raw_timestamp, bool)
            and raw_timestamp > 0
            else None
        )
        return cls.from_exception(
            stage=stage,
            strategy_id=context.get("strategy_id"),
            strategy_version=context.get("strategy_version"),
            timestamp=normalized_timestamp,
            instrument=instrument,
            error=error,
        )

    def validate(self) -> None:
        if (
            isinstance(self.schema_version, bool)
            or not isinstance(self.schema_version, int)
            or self.schema_version != STRATEGY_ERROR_SCHEMA_VERSION
        ):
            raise ValueError("StrategyErrorV1 schema_version is unsupported")
        if not isinstance(self.stage, str) or self.stage not in STRATEGY_ERROR_STAGES:
            raise ValueError("StrategyErrorV1 stage is unsupported")
        for name, value in (
            ("strategy_id", self.strategy_id),
            ("strategy_version", self.strategy_version),
            ("message", self.message),
        ):
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"StrategyErrorV1 {name} must be non-empty text")
        if len(self.message.encode("utf-8")) > MAX_STRATEGY_ERROR_MESSAGE_BYTES:
            raise ValueError("StrategyErrorV1 message exceeds the 16 KiB bound")
        if self.timestamp is not None and (
            isinstance(self.timestamp, bool)
            or not isinstance(self.timestamp, int)
            or self.timestamp <= 0
        ):
            raise ValueError("StrategyErrorV1 timestamp must be positive or None")
        if self.instrument is not None and (
            not isinstance(self.instrument, str) or not self.instrument.strip()
        ):
            raise ValueError("StrategyErrorV1 instrument must be non-empty text or None")

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        return asdict(self)


__all__ = ["StrategyErrorV1", "STRATEGY_ERROR_SCHEMA_VERSION", "STRATEGY_ERROR_STAGES"]
