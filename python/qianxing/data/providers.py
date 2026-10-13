"""Provider adapters whose selection and validation execute in the Rust SDK."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from typing import Any, Mapping, Protocol

from qianxing_bridge import native

BAR_JSON_CONTENT_TYPE = "application/vnd.qianxing.bar+json;v=1"
ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE = "application/vnd.qianxing.order-book-snapshot+json;v=1"


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
    required_auth_scope: str | None = None
    require_realtime: bool = False
    max_staleness_ms: int | None = None

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


@dataclass(frozen=True)
class ProviderDatasetBacktestOutcome:
    """Result of freezing provider data, validating its manifest, and running Rust backtest."""

    outcome: Any
    manifest: Mapping[str, Any]
    dataset_verdict: Any
    manifest_path: str


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

    def fetch_bar_dataset(
        self,
        query: ProviderQuery,
        dataset_id: str,
        dataset_version: str,
        interval_ms: int,
        timezone: str = "UTC",
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        """Return a Rust-validated BarFrame and its DatasetManifestV2 with quality metadata."""
        frame_json, manifest_json = self._fetch_bar_dataset_json(
            query, dataset_id, dataset_version, interval_ms, timezone
        )
        return json.loads(frame_json), json.loads(manifest_json)

    def _fetch_bar_dataset_json(
        self,
        query: ProviderQuery,
        dataset_id: str,
        dataset_version: str,
        interval_ms: int,
        timezone: str,
    ) -> tuple[str, str]:
        payload = json.loads(
            native.provider_bar_dataset_json(
                self._native,
                query.to_json(),
                dataset_id,
                dataset_version,
                interval_ms,
                timezone,
            )
        )
        return str(payload["frame_json"]), str(payload["manifest_json"])

    def fetch_depth_frame(self, query: ProviderQuery, tier: str) -> dict[str, Any]:
        """Fetch Quote snapshots and normalize them to the Rust DepthFrame contract."""
        return json.loads(self._native.fetch_depth_frame_json(query.to_json(), tier))

    def run_backtest(self, spec: Any, query: ProviderQuery) -> Any:
        """Freeze provider bars at ``spec.bars_path`` and run the shared Rust backtest use case."""
        from qianxing.app import BacktestSpec, run_backtest

        frame_json = self._native.fetch_bar_frame_json(query.to_json())
        values = spec.to_dict() if isinstance(spec, BacktestSpec) else dict(spec)
        _freeze_frame(values["bars_path"], frame_json)
        return run_backtest(values)

    def run_backtest_with_manifest(
        self,
        spec: Any,
        query: ProviderQuery,
        dataset_id: str,
        dataset_version: str,
        interval_ms: int,
        timezone: str = "UTC",
        manifest_path: str | None = None,
    ) -> ProviderDatasetBacktestOutcome:
        """Freeze and validate a DatasetManifestV2 before invoking the Rust Bar backtest."""
        from qianxing.app import BacktestOutcome, BacktestSpec, DatasetVerdict

        values = spec.to_dict() if isinstance(spec, BacktestSpec) else dict(spec)
        manifest_file = manifest_path or f"{values['bars_path']}.manifest.json"
        payload = json.loads(
            self._native.run_bar_backtest_with_manifest_json(
                query.to_json(),
                json.dumps(values, sort_keys=True, separators=(",", ":")),
                json.dumps(
                    {
                        "dataset_id": dataset_id,
                        "dataset_version": dataset_version,
                        "interval_ms": interval_ms,
                        "timezone": timezone,
                        "manifest_path": manifest_file,
                    },
                    sort_keys=True,
                    separators=(",", ":"),
                ),
            )
        )
        return ProviderDatasetBacktestOutcome(
            outcome=BacktestOutcome.from_dict(payload["outcome"]),
            manifest=payload["manifest"],
            dataset_verdict=DatasetVerdict.from_dict(payload["dataset_verdict"]),
            manifest_path=payload["manifest_path"],
        )

    def run_depth_backtest(self, spec: Any, query: ProviderQuery, tier: str) -> Any:
        """Freeze Rust-normalized Quote snapshots and run the shared Rust depth workflow."""
        from qianxing.app import DepthBacktestSpec, run_depth_backtest

        frame_json = self._native.fetch_depth_frame_json(query.to_json(), tier)
        values = spec.to_dict() if isinstance(spec, DepthBacktestSpec) else dict(spec)
        if values["tier"] != tier:
            raise ValueError("provider depth tier must equal DepthBacktestSpec.tier")
        _freeze_frame(values["depth_path"], frame_json)
        return run_depth_backtest(values)


def _freeze_frame(path_value: str, frame_json: str) -> None:
    """Write immutable normalized provider data once; never silently replace a frozen input."""
    from pathlib import Path

    path = Path(path_value)
    payload = frame_json.encode("utf-8")
    path.parent.mkdir(parents=True, exist_ok=True)
    created = False
    try:
        with path.open("xb") as output:
            created = True
            output.write(payload)
    except FileExistsError:
        if path.read_bytes() != payload:
            raise FileExistsError(f"Refusing to replace different frozen provider data at {path}")
    except BaseException:
        if created:
            path.unlink(missing_ok=True)
        raise


__all__ = [
    "BAR_JSON_CONTENT_TYPE",
    "ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE",
    "ProviderAdapter",
    "ProviderAdapterError",
    "ProviderCapability",
    "ProviderDatasetBacktestOutcome",
    "ProviderManualInterventionError",
    "ProviderPayloadRow",
    "ProviderPermanentError",
    "ProviderQuery",
    "ProviderRegistry",
    "ProviderResponse",
]
