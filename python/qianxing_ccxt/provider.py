"""Connect CCXT historical OHLCV to the Python SDK's Rust ProviderRegistry."""

from __future__ import annotations

import hashlib
import json
import time
from typing import Any, Callable

from qianxing.data.providers import (
    BAR_JSON_CONTENT_TYPE,
    ProviderCapability,
    ProviderManualInterventionError,
    ProviderPayloadRow,
    ProviderPermanentError,
    ProviderRegistry,
    ProviderResponse,
)

from . import CcxtConnectorError, CcxtErrorClass, CcxtExchangeClient

MAX_PROVIDER_BAR_ROWS = 100_000


class CcxtProviderAdapter:
    """Map one CCXT exchange's paginated OHLCV client into the Rust Provider contract.

    Register one adapter per exchange. Selection, failover, lineage checks, BarFrame
    normalization, manifest freezing, and backtesting remain in the shared Rust SDK.
    """

    def __init__(
        self,
        provider_id: str,
        version: str,
        client: CcxtExchangeClient,
        *,
        clock_ms: Callable[[], int] | None = None,
    ) -> None:
        if not provider_id.strip() or not version.strip():
            raise ValueError("provider_id and version must be non-empty")
        self.provider_id = provider_id
        self.version = version
        self.client = client
        self._clock_ms = clock_ms or (lambda: time.time_ns() // 1_000_000)

    def fetch(self, query_json: str) -> str:
        try:
            query = json.loads(query_json)
        except (TypeError, json.JSONDecodeError) as error:
            raise ProviderPermanentError(f"invalid Provider query JSON: {error}") from error
        if not isinstance(query, dict) or query.get("kind") != "Bar":
            raise ProviderPermanentError("CCXT Provider only supports historical Bar requests")
        instruments = query.get("instrument_set")
        if not isinstance(instruments, list) or len(instruments) != 1:
            raise ProviderPermanentError("CCXT Bar request requires exactly one instrument")
        if query.get("require_realtime"):
            raise ProviderPermanentError("CCXT OHLCV history is not a realtime subscription")

        instrument = instruments[0]
        try:
            frame = self.client.fetch_ohlcv(
                instrument,
                timeframe=str(query["frequency"]),
                start_ms=int(query["start"]),
                end_ms=int(query["end"]),
            )
        except CcxtConnectorError as error:
            if error.error_class == CcxtErrorClass.AUTHENTICATION:
                raise ProviderManualInterventionError(str(error)) from error
            if error.error_class in (CcxtErrorClass.INVALID, CcxtErrorClass.UNSUPPORTED):
                raise ProviderPermanentError(str(error)) from error
            raise RuntimeError(str(error)) from error

        frame.validate()
        if frame.instrument != instrument:
            raise ProviderPermanentError("CCXT BarFrame instrument differs from the request")
        row_count = len(frame.ts)
        if row_count > MAX_PROVIDER_BAR_ROWS:
            raise ProviderPermanentError(
                f"CCXT BarFrame exceeds {MAX_PROVIDER_BAR_ROWS} row limit"
            )
        if any(timestamp < query["start"] or timestamp > query["end"] for timestamp in frame.ts):
            raise ProviderPermanentError("CCXT BarFrame contains timestamps outside the query")

        received_at = int(self._clock_ms())
        if received_at < 0:
            raise ProviderPermanentError("CCXT receive timestamp must be non-negative")
        rows = tuple(
            ProviderPayloadRow(
                event_time=timestamp,
                receive_time=received_at,
                instrument=frame.instrument,
                content_type=BAR_JSON_CONTENT_TYPE,
                json=json.dumps(
                    {
                        "ts": timestamp,
                        "open": open_raw,
                        "high": high_raw,
                        "low": low_raw,
                        "close": close_raw,
                        "volume": volume_raw,
                    },
                    separators=(",", ":"),
                ),
            )
            for timestamp, open_raw, high_raw, low_raw, close_raw, volume_raw in zip(
                frame.ts,
                frame.open_raw,
                frame.high_raw,
                frame.low_raw,
                frame.close_raw,
                frame.volume_raw,
                strict=True,
            )
        )
        digest = hashlib.sha256(query_json.encode("utf-8")).hexdigest()[:16]
        return ProviderResponse(
            provider_id=self.provider_id,
            provider_version=self.version,
            request_id=f"ccxt-{digest}-{received_at}",
            received_at=received_at,
            rows=rows,
        ).to_json()


def register_ccxt_provider(
    registry: ProviderRegistry,
    capability: ProviderCapability,
    client: CcxtExchangeClient,
    *,
    clock_ms: Callable[[], int] | None = None,
) -> CcxtProviderAdapter:
    """Register one configured CCXT exchange with the Python SDK's Rust registry."""

    adapter = CcxtProviderAdapter(
        capability.provider_id,
        capability.version,
        client,
        clock_ms=clock_ms,
    )
    registry.register(capability, adapter)
    return adapter


__all__ = ["CcxtProviderAdapter", "MAX_PROVIDER_BAR_ROWS", "register_ccxt_provider"]
