"""Typed Python facade for the versioned qx-app use cases.

Only schemas currently implemented by qx-app are exposed here. These are
synchronous Bar research use cases; this module does not imply Tick/Book,
Paper, or Live application coverage.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from importlib.metadata import PackageNotFoundError, version
from typing import Any, Literal

from qianxing_bridge import native


class QianxingError(RuntimeError):
    """Base class for stable application-layer errors."""

    def __init__(self, message: str, *, payload: dict[str, Any] | None = None):
        super().__init__(message)
        self.payload = payload or {}
        self.code = self.payload.get("category", "INTERNAL_INVARIANT")
        self.category = self.code
        self.correlation_id = self.payload.get("correlation_id")
        self.action = self.payload.get("action")
        self.retry = self.payload.get("retry")


class AppError(QianxingError):
    """A stable qx-app failure with machine-readable category and action."""


class InvalidInputError(AppError):
    pass


class DataUnavailableError(AppError):
    pass


class FidelityInsufficientError(AppError):
    pass


class PermissionDeniedError(AppError):
    pass


class ConflictError(AppError):
    pass


class AppTimeoutError(AppError):
    pass


class StorageFailureError(AppError):
    pass


class InternalInvariantError(AppError):
    pass


_APP_ERRORS: dict[str, type[AppError]] = {
    "INVALID_INPUT": InvalidInputError,
    "DATA_UNAVAILABLE": DataUnavailableError,
    "FIDELITY_INSUFFICIENT": FidelityInsufficientError,
    "PERMISSION_DENIED": PermissionDeniedError,
    "CONFLICT": ConflictError,
    "TIMEOUT": AppTimeoutError,
    "STORAGE_FAILURE": StorageFailureError,
    "INTERNAL_INVARIANT": InternalInvariantError,
}


def _invoke(call: Any, value: Any) -> dict[str, Any]:
    payload = value.to_dict() if hasattr(value, "to_dict") else value
    encoded = payload if isinstance(payload, str) else json.dumps(
        payload, ensure_ascii=False, separators=(",", ":")
    )
    try:
        return json.loads(call(encoded))
    except Exception as exc:
        try:
            error = json.loads(str(exc))
        except (TypeError, ValueError):
            raise QianxingError(str(exc)) from exc
        error_type = _APP_ERRORS.get(error.get("category"), AppError)
        raise error_type(error.get("message", str(exc)), payload=error) from exc


@dataclass(frozen=True)
class DatasetSpec:
    dataset_id: str
    bars_path: str
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(frozen=True)
class BuiltinStrategySpec:
    kind: Literal[
        "sma_cross", "ema_cross", "macd", "rsi", "bollinger",
        "donchian_breakout", "momentum", "mean_reversion", "grid",
        "atr_trend", "keltner_trend", "vwap_reversion",
        "volatility_breakout", "pairs_arbitrage", "basis_arbitrage",
        "cross_venue_arbitrage", "spot_futures_arbitrage",
    ]
    strategy_id: str
    quantity_raw: int = 1_000_000_000
    fast_window: int = 5
    slow_window: int = 20
    period: int = 14
    threshold_bps: int = 100

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(frozen=True)
class BacktestSpec:
    run_id: str
    instrument: str
    bars_path: str
    settlement_currency: str
    initial_cash_raw: int
    output_dir: str
    strategy: BuiltinStrategySpec
    seed: int = 0
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {
            **asdict(self),
            "strategy": self.strategy.to_dict(),
        }


@dataclass(frozen=True)
class DatasetVerdict:
    dataset_id: str
    instrument: str
    rows: int
    content_fingerprint: str
    usable: bool
    gaps: tuple[str, ...] = field(default_factory=tuple)

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "DatasetVerdict":
        return cls(**{**value, "gaps": tuple(value.get("gaps", ()))})


@dataclass(frozen=True)
class BacktestOutcome:
    run_id: str
    instrument: str
    result_hash: str
    data_fingerprint: str
    fills: int
    equity_points: int
    return_bps: int
    max_drawdown_bps: int
    artifacts: dict[str, str]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "BacktestOutcome":
        return cls(**value)


@dataclass(frozen=True)
class VerificationResult:
    run_id: str
    result_hash: str
    data_fingerprint: str
    verified: bool
    checks: tuple[str, ...]
    mismatches: tuple[str, ...]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "VerificationResult":
        return cls(
            **{
                **value,
                "checks": tuple(value.get("checks", ())),
                "mismatches": tuple(value.get("mismatches", ())),
            }
        )


@dataclass(frozen=True)
class ComparedRun:
    run_id: str
    instrument: str
    data_fingerprint: str
    result_hash: str
    return_bps: int
    max_drawdown_bps: int

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(frozen=True)
class CompareRunsSpec:
    runs: tuple[ComparedRun, ...]
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {"schema_version": self.schema_version, "runs": [run.to_dict() for run in self.runs]}


@dataclass(frozen=True)
class ComparedRunResult:
    rank: int
    run: ComparedRun

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ComparedRunResult":
        return cls(rank=value["rank"], run=ComparedRun(**value["run"]))


@dataclass(frozen=True)
class CompareRunsResult:
    schema_version: int
    instrument: str
    data_fingerprint: str
    runs: tuple[ComparedRunResult, ...]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "CompareRunsResult":
        return cls(
            schema_version=value["schema_version"],
            instrument=value["instrument"],
            data_fingerprint=value["data_fingerprint"],
            runs=tuple(ComparedRunResult.from_dict(item) for item in value["runs"]),
        )


def validate_dataset(spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
    """Validate a local Bar dataset using the shared Rust application use case."""
    return DatasetVerdict.from_dict(_invoke(native.app_validate_dataset, spec))


def run_backtest(spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
    """Run the currently supported deterministic single-instrument Bar backtest."""
    return BacktestOutcome.from_dict(_invoke(native.app_run_backtest, spec))


def verify_run(outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
    """Verify the artifacts produced by :func:`run_backtest`."""
    value = asdict(outcome) if isinstance(outcome, BacktestOutcome) else outcome
    return VerificationResult.from_dict(_invoke(native.app_verify_run, value))


def compare_runs(spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
    """Rank completed runs from identical market data using the shared Rust use case."""
    return CompareRunsResult.from_dict(_invoke(native.app_compare_runs, spec))


def doctor() -> dict[str, Any]:
    """Report which native application entry points are available in this install."""
    extension_available = native.available()
    app_available = native.app_available()
    try:
        package_version = version("qianxing")
    except PackageNotFoundError:
        package_version = "0.1.0"
    return {
        "package": "qianxing",
        "version": package_version,
        "native_extension_available": extension_available,
        "application_use_cases_available": app_available,
        "implemented_use_cases": (
            ["dataset.validate.bar.v1", "backtest.bar.v1", "run.verify.v1", "backtest.compare.v1"]
            if app_available
            else []
        ),
        "limitations": [
            "Tick/OrderBook, multi-leg backtests, Paper, and Live are not exposed by this SDK facade."
        ],
    }
