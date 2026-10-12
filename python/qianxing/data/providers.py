"""Provider adapters whose selection and validation execute in the Rust SDK."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from typing import Any, Mapping, Protocol

from qianxing_bridge import native

BAR_JSON_CONTENT_TYPE = "application/vnd.qianxing.bar+json;v=1"


class ProviderAdapter(Protocol):
    """Adapter contract: receive canonical query JSON and return canonical result JSON."""

    def fetch(self, query_json: str) -> str: ...


class ProviderAdapterError(RuntimeError):
    """Base error carrying the Rust failover disposition for a provider failure."""

    qx_provider_error_class = "switch_provider"


class ProviderManualInterventionError(ProviderAdapterError):
    qx_provider_error_class = "manual_intervention"


class ProviderPermanentError(ProviderAdapterError):
    qx_provider_error_class = "permanent"


@dataclass(frozen=True)
class ProviderQuery:
    kind: str
    asset_class: str
    frequency: str
    start: int
    end: int
    instrument_set: tuple[str, ...] = ()
    field_set: tuple[str, ...] = ()
    adjustment: str = "none"
    quality_policy: str = "strict"
    as_of: int | None = None

    def to_dict(self) -> dict[str, Any]:
        value = asdict(self)
        value["instrument_set"] = sorted(set(self.instrument_set))
        value["field_set"] = sorted(set(self.field_set))
        return value

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), sort_keys=True, separators=(",", ":"))


@dataclass(frozen=True)
class ProviderCapability:
    provider_id: str
    version: str
    data_kinds: tuple[str, ...]
    asset_classes: tuple[str, ...]
    frequencies: tuple[str, ...]
    auth_scope: str
    rate_limit_per_second: int
    freshness_seconds: int
    historical_start: int
    historical_end: int
    realtime: bool
    priority: int
    quality_score: int
    cost_score: int

    def to_dict(self) -> dict[str, Any]:
        value = asdict(self)
        for key in ("data_kinds", "asset_classes", "frequencies"):
            value[key] = sorted(set(value[key]))
        return value

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), sort_keys=True, separators=(",", ":"))


@dataclass(frozen=True)
class ProviderPayloadRow:
    event_time: int
    receive_time: int
    instrument: str
    content_type: str
    json: str

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "ProviderPayloadRow":
        return cls(**dict(value))


@dataclass(frozen=True)
class ProviderResponse:
    provider_id: str
    provider_version: str
    request_id: str
    received_at: int
    rows: tuple[ProviderPayloadRow, ...] = field(default_factory=tuple)

    def to_json(self) -> str:
        """Ask the Rust boundary to construct all raw-record and result fingerprints."""
        rows_json = json.dumps(
            [asdict(row) for row in self.rows], sort_keys=True, separators=(",", ":")
        )
        return native.provider_result_json(
            self.provider_id,
            self.provider_version,
            self.request_id,
            self.received_at,
            rows_json,
        )


class ProviderRegistry:
    """Python adapter registry backed by Rust's deterministic selection/failover engine."""

    def __init__(self) -> None:
        self._native = native.provider_registry()

    def register(self, capability: ProviderCapability, adapter: ProviderAdapter) -> None:
        self._native.register(capability.to_json(), adapter)

    def candidates(self, query: ProviderQuery) -> tuple[str, ...]:
        return tuple(self._native.candidates(query.to_json()))

    def fetch(self, query: ProviderQuery) -> dict[str, Any]:
        return json.loads(self._native.fetch_json(query.to_json()))

    def fetch_bar_frame(self, query: ProviderQuery) -> dict[str, Any]:
        """Fetch one Bar instrument and return the Rust-validated canonical BarFrame."""
        return json.loads(self._native.fetch_bar_frame_json(query.to_json()))

    def run_backtest(self, spec: Any, query: ProviderQuery) -> Any:
        """Freeze provider bars at ``spec.bars_path`` and run the shared Rust backtest use case."""
        from pathlib import Path

        from qianxing.app import BacktestSpec, run_backtest

        frame_json = self._native.fetch_bar_frame_json(query.to_json())
        payload = frame_json.encode("utf-8")
        values = spec.to_dict() if isinstance(spec, BacktestSpec) else dict(spec)
        path = Path(values["bars_path"])
        path.parent.mkdir(parents=True, exist_ok=True)
        created = False
        try:
            with path.open("xb") as output:
                created = True
                output.write(payload)
        except FileExistsError:
            if path.read_bytes() != payload:
                raise FileExistsError(
                    f"Refusing to replace different frozen provider data at {path}"
                )
        except BaseException:
            if created:
                path.unlink(missing_ok=True)
            raise
        return run_backtest(values)


__all__ = [
    "BAR_JSON_CONTENT_TYPE",
    "ProviderAdapter",
    "ProviderAdapterError",
    "ProviderCapability",
    "ProviderManualInterventionError",
    "ProviderPayloadRow",
    "ProviderPermanentError",
    "ProviderQuery",
    "ProviderRegistry",
    "ProviderResponse",
]
