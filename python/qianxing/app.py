"""Typed Python facade for the versioned qx-app use cases.

Typed facade for schemas currently implemented by qx-app. Every candidate
backtest and comparison is executed by the shared Rust application layer.
Tick, OrderBook, two-leg Bar backtests, parameter experiments, and run lifecycles use shared Rust application use cases. Paper and Live command clients submit to the Rust API and reuse its configured workers.
"""

from __future__ import annotations

import json
import math
import time
from collections.abc import Mapping
from dataclasses import asdict, dataclass, field, is_dataclass
from importlib.metadata import PackageNotFoundError, version
from ssl import SSLContext
from typing import Any, Generic, Literal, TypeVar
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

from qianxing_bridge import native

# 远端客户端的路由表：Python 侧对 qx-api 路由表的**唯一**声明面。
# 门禁 `client_route_wiring_check` 逐条与 qx-api 的分派表核对——这里写了一个后端不存在的
# 路径，会在门禁当场变红，而不是等用户跑到那一步才拿到 404。`{run_id}` 是路径模板占位符，
# 调用侧用 `.format(run_id=...)` 填。
APP_ROUTE_TEMPLATES: dict[str, str] = {
    "validate_dataset": "/app/validate-dataset",
    "backtest": "/app/backtest",
    "backtest_start": "/app/backtest/start",
    "verify": "/app/verify",
    "verify_depth": "/app/verify-depth",
    "compare_runs": "/app/compare-runs",
    "run_experiment": "/app/run-experiment",
    "run_experiment_start": "/app/run-experiment/start",
    "depth_backtest": "/app/depth-backtest",
    "depth_backtest_start": "/app/depth-backtest/start",
    "depth_experiment": "/app/run-depth-experiment",
    "depth_experiment_start": "/app/run-depth-experiment/start",
    "multi_leg_backtest": "/app/multi-leg-backtest",
    "multi_leg_backtest_start": "/app/multi-leg-backtest/start",
    "multi_leg_experiment": "/app/run-multi-leg-experiment",
    "multi_leg_experiment_start": "/app/run-multi-leg-experiment/start",
    "paper_commands": "/app/paper/commands",
    "live_commands": "/app/live/commands",
    "run_status": "/app/runs/{run_id}",
    "run_cancel": "/app/runs/{run_id}/cancel",
}


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
    payload = (
        value.to_dict()
        if hasattr(value, "to_dict")
        else asdict(value)
        if is_dataclass(value)
        else value
    )
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
class RunMultiLegExperimentSpec:
    experiment_id: str
    base: MultiLegBacktestSpec
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
class RunDepthExperimentSpec:
    experiment_id: str
    base: DepthBacktestSpec
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
class OrderPolicy:
    reduce_only: bool = False
    position_side: Literal["net", "long", "short"] = "net"
    margin_mode: Literal["cash", "cross", "isolated"] = "cash"
    position_mode: Literal["one_way", "hedge"] = "one_way"
    leverage: int = 1
    post_only: bool = False

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


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
    market_spec: TradingInstrumentSpec | dict[str, Any] | None = None
    order_policy: OrderPolicy | dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        payload = {**asdict(self), "strategy": self.strategy.to_dict()}
        # Mirror Rust's skip_serializing_if on these optional v1 extensions so
        # legacy requests keep their historical JSON shape across SDKs.
        for optional_field in ("market_spec", "order_policy"):
            if payload[optional_field] is None:
                del payload[optional_field]
        return payload


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


@dataclass(frozen=True)
class DepthExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: DepthBacktestOutcome | None
    error: str | None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "DepthExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=DepthBacktestOutcome.from_dict(raw_outcome) if raw_outcome else None,
            error=value.get("error"),
        )


@dataclass(frozen=True)
class DepthExperimentRankedCandidate:
    rank: int
    ordinal: int
    run_id: str
    return_bps: int
    max_drawdown_bps: int
    result_hash: str


@dataclass(frozen=True)
class RunDepthExperimentResult:
    schema_version: int
    experiment_id: str
    spec_fingerprint: str
    total_candidates: int
    completed_candidates: int
    succeeded_candidates: int
    failed_candidates: int
    candidates: tuple[DepthExperimentCandidateResult, ...]
    ranking: tuple[DepthExperimentRankedCandidate, ...]
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunDepthExperimentResult":
        return cls(
            schema_version=value["schema_version"],
            experiment_id=value["experiment_id"],
            spec_fingerprint=value["spec_fingerprint"],
            total_candidates=value["total_candidates"],
            completed_candidates=value["completed_candidates"],
            succeeded_candidates=value["succeeded_candidates"],
            failed_candidates=value["failed_candidates"],
            candidates=tuple(DepthExperimentCandidateResult.from_dict(item) for item in value["candidates"]),
            ranking=tuple(DepthExperimentRankedCandidate(**item) for item in value["ranking"]),
            artifact_path=value["artifact_path"],
        )


@dataclass(frozen=True)
class InstrumentId:
    symbol: str
    venue: str

    def to_dict(self) -> dict[str, str]:
        return asdict(self)


@dataclass(frozen=True)
class TradingInstrumentSpec:
    """The normalized Rust product contract consumed by market-aware app runs."""

    instrument: InstrumentId | dict[str, str]
    product: Literal["spot", "margin", "perpetual", "future", "option"]
    base_currency: str
    quote_currency: str
    settlement_currency: str
    contract_size: int
    linear: bool
    inverse: bool
    price_tick: int
    qty_step: int
    min_qty: int
    max_leverage: int
    maintenance_margin_bps: int
    valid_from: int
    valid_to: int | None = None

    def to_dict(self) -> dict[str, Any]:
        value = asdict(self)
        if isinstance(self.instrument, InstrumentId):
            value["instrument"] = self.instrument.to_dict()
        return value


@dataclass(frozen=True)
class MultiLegBacktestSpec:
    run_id: str
    primary_bars_path: str
    reference_bars_path: str
    settlement_currency: str
    primary_initial_cash_raw: int
    reference_initial_cash_raw: int
    output_dir: str
    strategy: BuiltinStrategySpec
    seed: int = 0
    primary_market_spec: TradingInstrumentSpec | dict[str, Any] | None = None
    reference_market_spec: TradingInstrumentSpec | dict[str, Any] | None = None
    maker_fee_bps: int = 2
    taker_fee_bps: int = 5
    primary_maker_fee_bps: int | None = None
    primary_taker_fee_bps: int | None = None
    reference_maker_fee_bps: int | None = None
    reference_taker_fee_bps: int | None = None
    funding_bps: int = 0
    fill_model: Literal["next_bar_open", "best_price", "one_tick_slippage"] = "next_bar_open"
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {**asdict(self), "strategy": self.strategy.to_dict()}


@dataclass(frozen=True)
class MultiLegBacktestOutcome:
    run_id: str
    primary_instrument: str
    reference_instrument: str
    primary_result_hash: str
    reference_result_hash: str
    combined_return_bps: int
    primary_fills: int
    reference_fills: int
    attribution_artifact: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiLegBacktestOutcome":
        return cls(**value)


@dataclass(frozen=True)
class MultiLegExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: MultiLegBacktestOutcome | None
    error: str | None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiLegExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=MultiLegBacktestOutcome.from_dict(raw_outcome) if raw_outcome else None,
            error=value.get("error"),
        )


@dataclass(frozen=True)
class MultiLegExperimentRankedCandidate:
    rank: int
    ordinal: int
    run_id: str
    combined_return_bps: int
    primary_result_hash: str
    reference_result_hash: str


@dataclass(frozen=True)
class RunMultiLegExperimentResult:
    schema_version: int
    experiment_id: str
    spec_fingerprint: str
    total_candidates: int
    completed_candidates: int
    succeeded_candidates: int
    failed_candidates: int
    candidates: tuple[MultiLegExperimentCandidateResult, ...]
    ranking: tuple[MultiLegExperimentRankedCandidate, ...]
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunMultiLegExperimentResult":
        return cls(
            schema_version=value["schema_version"],
            experiment_id=value["experiment_id"],
            spec_fingerprint=value["spec_fingerprint"],
            total_candidates=value["total_candidates"],
            completed_candidates=value["completed_candidates"],
            succeeded_candidates=value["succeeded_candidates"],
            failed_candidates=value["failed_candidates"],
            candidates=tuple(
                MultiLegExperimentCandidateResult.from_dict(item)
                for item in value["candidates"]
            ),
            ranking=tuple(MultiLegExperimentRankedCandidate(**item) for item in value["ranking"]),
            artifact_path=value["artifact_path"],
        )


@dataclass(frozen=True)
class TradeCommand:
    """Existing Rust control-plane command contract; no Python execution logic is added."""

    command_id: int
    request_id: str
    operator_id: str
    reason: str
    kind: Literal[
        "SubmitOrder", "PauseStrategy", "ResumeStrategy", "ChangeRiskLimit",
        "CancelOrder", "ReconcileAccount", "RetryJob", "SwitchVenue",
    ]
    target: str
    payload: dict[str, str]
    permission: Literal["ReadOnly", "Research", "Trading", "Admin"] = "Trading"
    dry_run: bool = False

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class _HttpJsonClient:
    """Shared HTTP transport for the Rust API facades; contains no domain logic."""

    def __init__(
        self,
        base_url: str,
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        parsed = urlsplit(base_url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc or parsed.query or parsed.fragment:
            raise ValueError("base_url must be an http(s) origin/path without query or fragment")
        if not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be finite and positive")
        request_headers = dict(headers or {})
        for name, value in request_headers.items():
            if (
                not isinstance(name, str)
                or not name.strip()
                or not isinstance(value, str)
                or "\r" in name
                or "\n" in name
                or "\r" in value
                or "\n" in value
            ):
                raise ValueError("headers must contain valid string names and values")
            if name.casefold() == "content-type":
                raise ValueError("Content-Type is managed by the Qianxing API client")
        self.base_url = base_url.rstrip("/")
        self.headers = request_headers
        self.ssl_context = ssl_context
        self.timeout = timeout

    def _request_json(
        self,
        path: str,
        *,
        method: str,
        payload: Any = None,
    ) -> dict[str, Any]:
        data = None if payload is None else _encode_payload(payload).encode("utf-8")
        request = Request(
            f"{self.base_url}{path}",
            data=data,
            headers=(
                {**self.headers, "Content-Type": "application/json"}
                if data is not None
                else self.headers
            ),
            method=method,
        )
        try:
            with urlopen(request, timeout=self.timeout, context=self.ssl_context) as response:
                body = response.read().decode("utf-8")
                value = json.loads(body) if body else {}
                if not isinstance(value, dict):
                    raise QianxingError("Rust API returned a non-object JSON document")
                return value
        except HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            try:
                document = json.loads(raw)
            except ValueError:
                document = {"message": raw, "status": exc.code}
            if isinstance(document, dict):
                category = document.get("category")
                if category in _APP_ERRORS:
                    raise _APP_ERRORS[category](document.get("message", raw), payload=document) from exc
                message = document.get("message", document.get("error", raw))
                raise QianxingError(str(message), payload=document) from exc
            raise QianxingError(raw, payload={"status": exc.code}) from exc
        except (URLError, TimeoutError, OSError) as exc:
            raise QianxingError(f"Rust API request failed: {exc}") from exc


class TradeWorkflowClient(_HttpJsonClient):
    """Thin HTTP client for the Rust API's authenticated Paper/Live control workflows."""

    def __init__(
        self,
        base_url: str,
        workflow: Literal["paper", "live"],
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        if workflow not in ("paper", "live"):
            raise ValueError("workflow must be 'paper' or 'live'")
        if workflow == "live" and urlsplit(base_url).scheme != "https":
            raise ValueError("LiveClient requires HTTPS for every command request")
        super().__init__(
            base_url, headers=headers, ssl_context=ssl_context, timeout=timeout
        )
        self.workflow = workflow

    def submit(self, command: TradeCommand | dict[str, Any]) -> dict[str, Any]:
        """Submit through qx-api; Rust authenticates, validates, persists, and enqueues it."""
        command_document = command.to_dict() if isinstance(command, TradeCommand) else command
        return self._request_json(
            APP_ROUTE_TEMPLATES[f"{self.workflow}_commands"],
            method="POST",
            payload={"schema_version": 1, "workflow": self.workflow, "command": command_document},
        )


class PaperClient(TradeWorkflowClient):
    def __init__(
        self,
        base_url: str,
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        super().__init__(
            base_url, "paper", headers=headers, ssl_context=ssl_context, timeout=timeout
        )


class LiveClient(TradeWorkflowClient):
    def __init__(
        self,
        base_url: str,
        *,
        headers: Mapping[str, str] | None = None,
        ssl_context: SSLContext | None = None,
        timeout: float = 30.0,
    ):
        super().__init__(
            base_url, "live", headers=headers, ssl_context=ssl_context, timeout=timeout
        )


class RunHandle(Generic[_ResultT]):
    """Cooperative Rust worker handle for Bar, multi-leg, depth, and experiment runs."""

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


class RemoteRunHandle(Generic[_ResultT]):
    """Bounded HTTP lifecycle handle backed by the Rust API run registry."""

    def __init__(
        self,
        client: "AppClient",
        run_id: str,
        result_type: type[_ResultT] | None = None,
    ):
        if not run_id or any(not (char.isascii() and (char.isalnum() or char in "-_.")) for char in run_id):
            raise ValueError("run_id must contain only ASCII letters, digits, '.', '_' or '-'")
        self._client = client
        self.run_id = run_id
        self._result_type = result_type

    def document(self) -> dict[str, Any]:
        return self._client._request_json(
            APP_ROUTE_TEMPLATES["run_status"].format(run_id=self.run_id), method="GET"
        )

    @property
    def status(self) -> str:
        return str(self.document().get("status", "unknown"))

    def cancel(self) -> dict[str, Any]:
        return self._client._request_json(
            APP_ROUTE_TEMPLATES["run_cancel"].format(run_id=self.run_id),
            method="POST",
            payload={},
        )

    def wait(self, timeout: float = 30.0, poll_interval: float = 0.25) -> str:
        if (
            not math.isfinite(timeout)
            or not math.isfinite(poll_interval)
            or timeout <= 0
            or poll_interval <= 0
        ):
            raise ValueError("timeout and poll_interval must be finite and positive")
        deadline = time.monotonic() + timeout
        while True:
            status = self.status
            if status in {"succeeded", "cancelled", "failed"}:
                return status
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return status
            time.sleep(min(poll_interval, remaining))

    def result(self) -> _ResultT | dict[str, Any] | None:
        document = self.document()
        status = document.get("status")
        if status == "failed":
            error = document["error"]
            if not isinstance(error, dict):
                raise InternalInvariantError(
                    "Rust API reported a failed run without an error document",
                    payload={"run_id": self.run_id, "status": status},
                )
            error_type = _APP_ERRORS.get(error.get("category"), AppError)
            raise error_type(error.get("message", "Rust run failed"), payload=error)
        if status != "succeeded":
            return None
        result = document.get("result")
        if not isinstance(result, dict):
            raise InternalInvariantError(
                "Rust API reported a successful run without a result document",
                payload={"run_id": self.run_id, "status": status},
            )
        if self._result_type is not None:
            return self._result_type.from_dict(result)
        return result


class AppClient(_HttpJsonClient):
    """HTTP facade over the same Rust API use cases exposed by the native SDK."""

    def _post(self, route: str, spec: Any) -> dict[str, Any]:
        return self._request_json(route, method="POST", payload=spec)

    def _start(self, route: str, spec: Any, result_type: type[_ResultT]) -> RemoteRunHandle[_ResultT]:
        document = self._post(route, spec)
        run_id = document.get("run_id")
        if not isinstance(run_id, str):
            raise QianxingError("Rust API start response omitted run_id", payload=document)
        return RemoteRunHandle(self, run_id, result_type)

    def validate_dataset(self, spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
        return DatasetVerdict.from_dict(self._post(APP_ROUTE_TEMPLATES["validate_dataset"], spec))

    def run_backtest(self, spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
        return BacktestOutcome.from_dict(self._post(APP_ROUTE_TEMPLATES["backtest"], spec))

    def start_backtest(self, spec: BacktestSpec | dict[str, Any]) -> RemoteRunHandle[BacktestOutcome]:
        return self._start(APP_ROUTE_TEMPLATES["backtest_start"], spec, BacktestOutcome)

    def verify_run(self, outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
        return VerificationResult.from_dict(self._post(APP_ROUTE_TEMPLATES["verify"], outcome))

    def compare_runs(self, spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
        return CompareRunsResult.from_dict(self._post(APP_ROUTE_TEMPLATES["compare_runs"], spec))

    def run_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
        return RunExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["run_experiment"], spec)
        )

    def start_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunExperimentResult]:
        return self._start(APP_ROUTE_TEMPLATES["run_experiment_start"], spec, RunExperimentResult)

    def run_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> DepthBacktestOutcome:
        return DepthBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["depth_backtest"], spec)
        )

    def start_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> RemoteRunHandle[DepthBacktestOutcome]:
        return self._start(APP_ROUTE_TEMPLATES["depth_backtest_start"], spec, DepthBacktestOutcome)

    def verify_depth_run(self, outcome: DepthBacktestOutcome | dict[str, Any]) -> VerificationResult:
        return VerificationResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["verify_depth"], outcome)
        )

    def run_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RunDepthExperimentResult:
        return RunDepthExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["depth_experiment"], spec)
        )

    def start_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunDepthExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["depth_experiment_start"], spec, RunDepthExperimentResult
        )

    def run_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
        return MultiLegBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_leg_backtest"], spec)
        )

    def start_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> RemoteRunHandle[MultiLegBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_leg_backtest_start"], spec, MultiLegBacktestOutcome
        )

    def run_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RunMultiLegExperimentResult:
        return RunMultiLegExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_leg_experiment"], spec)
        )

    def start_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiLegExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_leg_experiment_start"],
            spec,
            RunMultiLegExperimentResult,
        )


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


def run_multi_leg_experiment(
    spec: RunMultiLegExperimentSpec | dict[str, Any],
) -> RunMultiLegExperimentResult:
    """Run a bounded grid through the shared Rust multi-leg backtest use case."""
    return RunMultiLegExperimentResult.from_dict(
        _invoke(native.app_run_multi_leg_experiment, spec)
    )


def run_depth_experiment(
    spec: RunDepthExperimentSpec | dict[str, Any],
) -> RunDepthExperimentResult:
    """Run Tick/OrderBook candidates through the same Rust depth engine."""
    return RunDepthExperimentResult.from_dict(_invoke(native.app_run_depth_experiment, spec))


def start_depth_experiment(
    spec: RunDepthExperimentSpec | dict[str, Any],
) -> RunHandle[RunDepthExperimentResult]:
    """Start a cancellable Tick/OrderBook experiment in the Rust run lifecycle."""
    try:
        native_handle = native.app_start_depth_experiment(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunDepthExperimentResult)


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


def run_multi_leg_backtest(spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
    """Run a two-leg Bar backtest via the shared Rust strategy and matching engines."""
    return MultiLegBacktestOutcome.from_dict(_invoke(native.app_run_multi_leg_backtest, spec))


def start_multi_leg_backtest(
    spec: MultiLegBacktestSpec | dict[str, Any],
) -> RunHandle[MultiLegBacktestOutcome]:
    """Start a cancellable two-leg Rust backtest."""
    try:
        native_handle = native.app_start_multi_leg_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, MultiLegBacktestOutcome)


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


def start_multi_leg_experiment(
    spec: RunMultiLegExperimentSpec | dict[str, Any],
) -> RunHandle[RunMultiLegExperimentResult]:
    """Start a cancellable Rust multi-leg parameter experiment."""
    try:
        native_handle = native.app_start_multi_leg_experiment(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunMultiLegExperimentResult)


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
                "experiment.grid.multi-leg.bar.v1",
                "experiment.grid.multi-leg.bar.run-handle.v1",
                "experiment.grid.depth.tick-orderbook.v1",
                "experiment.grid.depth.tick-orderbook.run-handle.v1",
                "backtest.depth.l1.v1",
                "backtest.depth.l2.v1",
                "backtest.depth.run-handle.v1",
                "run.verify.depth.v1",
                "backtest.multi-leg.bar.v1",
                "backtest.multi-leg.bar.run-handle.v1",
            ]
            if app_available
            else []
        ),
        "remote_workflows": ["control.paper.v1", "control.live.v1"],
        "limitations": [
            "Paper/Live submission requires a running qx-api service with matching workers; Live additionally requires authenticated operator policy.",
        ],
    }
