"""Typed Python facade for the versioned qx-app use cases.

Typed facade for schemas currently implemented by qx-app. Every candidate
backtest and comparison is executed by the shared Rust application layer.
Tick, OrderBook, and parameter experiments run through the shared Rust engines and application use cases. Multi-leg, Paper, and Live workflows are not exposed by this facade.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from importlib.metadata import PackageNotFoundError, version
from typing import Any, Generic, Literal, TypeVar

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


_ResultT = TypeVar("_ResultT")


def _translate_native_error(exc: BaseException) -> QianxingError:
    try:
        error = json.loads(str(exc))
    except (TypeError, ValueError):
        return QianxingError(str(exc))
    if not isinstance(error, dict):
        return QianxingError(str(exc))
    error_type = _APP_ERRORS.get(error.get("category"), AppError)
    return error_type(error.get("message", str(exc)), payload=error)


def _encode_payload(value: Any) -> str:
    payload = value.to_dict() if hasattr(value, "to_dict") else value
    return payload if isinstance(payload, str) else json.dumps(
        payload, ensure_ascii=False, separators=(",", ":")
    )


def _invoke(call: Any, value: Any) -> dict[str, Any]:
    try:
        return json.loads(call(_encode_payload(value)))
    except Exception as exc:
        raise _translate_native_error(exc) from exc


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


@dataclass(frozen=True)
class ExperimentParameterSpace:
    name: Literal["fast_window", "slow_window", "period", "threshold_bps", "quantity_raw"]
    values: tuple[int, ...]

    def to_dict(self) -> dict[str, Any]:
        return {"name": self.name, "values": list(self.values)}


@dataclass(frozen=True)
class RunExperimentSpec:
    experiment_id: str
    base: BacktestSpec
    parameter_space: tuple[ExperimentParameterSpace, ...]
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "experiment_id": self.experiment_id,
            "base": self.base.to_dict(),
            "parameter_space": [dimension.to_dict() for dimension in self.parameter_space],
        }


@dataclass(frozen=True)
class ExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: BacktestOutcome | None
    error: str | None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=BacktestOutcome.from_dict(raw_outcome) if raw_outcome is not None else None,
            error=value.get("error"),
        )


@dataclass(frozen=True)
class RunExperimentResult:
    schema_version: int
    experiment_id: str
    total_candidates: int
    completed_candidates: int
    succeeded_candidates: int
    failed_candidates: int
    spec_fingerprint: str
    candidates: tuple[ExperimentCandidateResult, ...]
    comparison: CompareRunsResult | None
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunExperimentResult":
        comparison = value.get("comparison")
        return cls(
            schema_version=value["schema_version"],
            experiment_id=value["experiment_id"],
            total_candidates=value["total_candidates"],
            completed_candidates=value["completed_candidates"],
            succeeded_candidates=value["succeeded_candidates"],
            failed_candidates=value["failed_candidates"],
            spec_fingerprint=value["spec_fingerprint"],
            candidates=tuple(
                ExperimentCandidateResult.from_dict(item) for item in value["candidates"]
            ),
            comparison=CompareRunsResult.from_dict(comparison) if comparison is not None else None,
            artifact_path=value["artifact_path"],
        )


@dataclass(frozen=True)
class DepthBacktestSpec:
    run_id: str
    depth_path: str
    tier: Literal["l1", "l2"]
    settlement_currency: str
    initial_cash_raw: int
    output_dir: str
    strategy: BuiltinStrategySpec
    fee_bps: int = 0
    latency_snapshots: int = 0
    queue_position_bps: int = 0
    market_impact_bps: int = 0
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {**asdict(self), "strategy": self.strategy.to_dict()}


@dataclass(frozen=True)
class DepthBacktestOutcome:
    run_id: str
    instrument: str
    tier: str
    result_hash: str
    data_fingerprint: str
    fills: int
    equity_points: int
    return_bps: int
    max_drawdown_bps: int
    artifacts: dict[str, str]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "DepthBacktestOutcome":
        return cls(**value)


class RunHandle(Generic[_ResultT]):
    """Cooperative Rust worker handle for Bar, depth, and parameter experiment runs."""

    def __init__(self, native_handle: Any, result_type: type[_ResultT]):
        self._native = native_handle
        self._result_type = result_type

    @property
    def run_id(self) -> str:
        return str(self._native.run_id)

    @property
    def status(self) -> str:
        return str(self._native.status)

    def cancel(self) -> None:
        """Request cooperative cancellation at the next Rust engine boundary."""
        self._native.cancel()

    def wait(self, timeout_ms: int | None = None) -> str:
        """Wait for a terminal state or timeout; timeout does not cancel the run."""
        if timeout_ms is not None and timeout_ms < 0:
            raise ValueError("timeout_ms must be non-negative")
        return str(self._native.wait(timeout_ms))

    def result(self) -> _ResultT | None:
        """Take the outcome once. Return None while running or after cancellation."""
        try:
            encoded = self._native.result_json()
        except Exception as exc:
            raise _translate_native_error(exc) from exc
        if encoded is None:
            return None
        return self._result_type.from_dict(json.loads(encoded))


def validate_dataset(spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
    """Validate a local Bar dataset using the shared Rust application use case."""
    return DatasetVerdict.from_dict(_invoke(native.app_validate_dataset, spec))


def run_backtest(spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
    """Run the deterministic single-instrument Bar backtest."""
    return BacktestOutcome.from_dict(_invoke(native.app_run_backtest, spec))


def start_backtest(spec: BacktestSpec | dict[str, Any]) -> RunHandle[BacktestOutcome]:
    """Start a Bar backtest; matching and cancellation remain in the Rust engine."""
    try:
        native_handle = native.app_start_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, BacktestOutcome)


def verify_run(outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
    """Verify the artifacts produced by :func:`run_backtest`."""
    value = asdict(outcome) if isinstance(outcome, BacktestOutcome) else outcome
    return VerificationResult.from_dict(_invoke(native.app_verify_run, value))


def compare_runs(spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
    """Rank completed runs from identical market data using the shared Rust use case."""
    return CompareRunsResult.from_dict(_invoke(native.app_compare_runs, spec))


def run_experiment(spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
    """Run a bounded Cartesian grid through Rust backtests and return their Rust comparison."""
    return RunExperimentResult.from_dict(_invoke(native.app_run_experiment, spec))


def run_depth_backtest(spec: DepthBacktestSpec | dict[str, Any]) -> DepthBacktestOutcome:
    """Run deterministic L1 Tick or L2 order-book matching in the shared Rust kernel."""
    return DepthBacktestOutcome.from_dict(_invoke(native.app_run_depth_backtest, spec))


def start_depth_backtest(spec: DepthBacktestSpec | dict[str, Any]) -> RunHandle[DepthBacktestOutcome]:
    """Start an L1 Tick or L2 OrderBook run with the shared Rust lifecycle."""
    try:
        native_handle = native.app_start_depth_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, DepthBacktestOutcome)


def verify_depth_run(outcome: DepthBacktestOutcome | dict[str, Any]) -> VerificationResult:
    """Verify the persisted artifacts of an L1/L2 depth run through Rust."""
    value = asdict(outcome) if isinstance(outcome, DepthBacktestOutcome) else outcome
    return VerificationResult.from_dict(_invoke(native.app_verify_depth_run, value))


def start_experiment(spec: RunExperimentSpec | dict[str, Any]) -> RunHandle[RunExperimentResult]:
    """Start a Rust parameter experiment with candidate and Bar-loop cancellation."""
    try:
        native_handle = native.app_start_experiment(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunExperimentResult)


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
            [
                "dataset.validate.bar.v1",
                "backtest.bar.v1",
                "backtest.bar.run-handle.v1",
                "run.verify.v1",
                "backtest.compare.v1",
                "experiment.grid.bar.v1",
                "experiment.grid.bar.run-handle.v1",
                "backtest.depth.l1.v1",
                "backtest.depth.l2.v1",
                "backtest.depth.run-handle.v1",
                "run.verify.depth.v1",
            ]
            if app_available
            else []
        ),
        "limitations": [
            "Multi-leg backtests, Paper orchestration, and Live controls are not yet exposed by this SDK facade."
        ],
    }
