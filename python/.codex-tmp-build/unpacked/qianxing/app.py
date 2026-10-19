"""Typed Python facade for the versioned qx-app use cases.

Typed facade for schemas currently implemented by qx-app. Every candidate
backtest and comparison is executed by the shared Rust application layer.
Tick, OrderBook, two-leg Bar backtests, parameter experiments, and run lifecycles use shared Rust application use cases. Paper and Live command clients submit to the Rust API and reuse its configured workers.
"""

from __future__ import annotations

import asyncio
import json
import math
import time
from collections.abc import Mapping, Sequence
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
    "application_capabilities": "/app/capabilities",
    "provider_preflight": "/app/capabilities/providers/preflight",
    "builtin_strategy_catalog": "/app/strategies",
    "validate_dataset": "/app/validate-dataset",
    "backtest": "/app/backtest",
    "provider_backtest": "/app/provider-backtest",
    "provider_depth_backtest": "/app/provider-depth-backtest",
    "multi_account_backtest": "/app/multi-account-backtest",
    "multi_account_backtest_start": "/app/multi-account-backtest/start",
    "multi_account_depth_backtest": "/app/multi-account-depth-backtest",
    "multi_account_depth_backtest_start": "/app/multi-account-depth-backtest/start",
    "multi_market_backtest": "/app/multi-market-backtest",
    "multi_market_backtest_start": "/app/multi-market-backtest/start",
    "multi_market_depth_backtest": "/app/multi-market-depth-backtest",
    "multi_market_depth_backtest_start": "/app/multi-market-depth-backtest/start",
    "multi_market_experiment": "/app/run-multi-market-experiment",
    "multi_market_experiment_start": "/app/run-multi-market-experiment/start",
    "multi_market_depth_experiment": "/app/run-multi-market-depth-experiment",
    "multi_market_depth_experiment_start": "/app/run-multi-market-depth-experiment/start",
    "backtest_start": "/app/backtest/start",
    "verify": "/app/verify",
    "verify_depth": "/app/verify-depth",
    "compare_runs": "/app/compare-runs",
    "plan_risk_parity": "/app/plan-risk-parity",
    "plan_algo_execution": "/app/plan-algo-execution",
    "plan_capital_allocation": "/app/plan-capital-allocation",
    "paper_algo_start": "/app/paper/algo-executions/start",
    "paper_algo_get": "/app/paper/algo-executions/get",
    "paper_algo_cancel": "/app/paper/algo-executions/cancel",
    "paper_algo_list": "/app/paper/algo-executions/list",
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
    "run_events": "/app/runs/{run_id}/events",
    "run_cancel": "/app/runs/{run_id}/cancel",
    "live_events": "/events/live",
}


# 方案 §9：「`wait()` 默认有限时；无限等待仅由调用方显式请求并可取消」。原生句柄原来的缺省是
# `None`——调用方一个参数都不写就拿到了无限等待，而远端句柄的缺省是 30 s：同一个 SDK 的两条腿
# 对"等多久"给了两个相反的答案。这里把缺省统一成有限的 30 s，`None` 保留为**显式**的无限等待。
DEFAULT_RUN_WAIT_MS = 30_000


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
class BuiltinStrategyInfo:
    """Discoverable built-in strategy metadata projected from the Rust strategy crate."""

    name: str
    description: str
    signal_knobs: tuple[str, ...]
    requires_reference_leg: bool


_BUILTIN_STRATEGY_INFO_KEYS = frozenset(
    {"name", "description", "signal_knobs", "requires_reference_leg"}
)


def _parse_builtin_strategy_catalog(value: Any) -> tuple[BuiltinStrategyInfo, ...]:
    if not isinstance(value, list):
        raise TypeError("strategy catalog must be a JSON array")
    if not value:
        raise ValueError("strategy catalog must not be empty")
    result: list[BuiltinStrategyInfo] = []
    names: set[str] = set()
    for index, entry in enumerate(value):
        if not isinstance(entry, dict) or set(entry) != _BUILTIN_STRATEGY_INFO_KEYS:
            raise ValueError(f"strategy catalog entry {index} has an invalid field set")
        name = entry["name"]
        description = entry["description"]
        signal_knobs = entry["signal_knobs"]
        requires_reference_leg = entry["requires_reference_leg"]
        if (
            not isinstance(name, str)
            or not name
            or not isinstance(description, str)
            or not description
            or not isinstance(signal_knobs, list)
            or any(not isinstance(knob, str) or not knob for knob in signal_knobs)
            or len(set(signal_knobs)) != len(signal_knobs)
            or type(requires_reference_leg) is not bool
            or name in names
        ):
            raise ValueError(f"strategy catalog entry {index} has invalid field values")
        names.add(name)
        result.append(
            BuiltinStrategyInfo(
                name=name,
                description=description,
                signal_knobs=tuple(signal_knobs),
                requires_reference_leg=requires_reference_leg,
            )
        )
    return tuple(result)


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
    target_notional_raw: int | None = None
    target_available_margin_bps: int | None = None
    target_volatility_bps: int | None = None
    target_kelly_fraction_bps: int | None = None
    kelly_lookback_bars: int | None = None
    fast_window: int = 5
    slow_window: int = 20
    period: int = 14
    threshold_bps: int = 100

    def to_dict(self) -> dict[str, Any]:
        value = asdict(self)
        if self.target_notional_raw is None:
            value.pop("target_notional_raw")
        if self.target_available_margin_bps is None:
            value.pop("target_available_margin_bps")
        if self.target_volatility_bps is None:
            value.pop("target_volatility_bps")
        if self.target_kelly_fraction_bps is None:
            value.pop("target_kelly_fraction_bps")
        if self.kelly_lookback_bars is None:
            value.pop("kelly_lookback_bars")
        return value


def list_builtin_strategies() -> tuple[BuiltinStrategyInfo, ...]:
    """List the authoritative Rust built-in strategies and their signal inputs."""
    try:
        value = json.loads(native.app_list_builtin_strategies())
        return _parse_builtin_strategy_catalog(value)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


@dataclass(frozen=True)
class ProtectiveExitSpec:
    stop_loss_bps: int | None = None
    take_profit_bps: int | None = None
    trailing_stop_bps: int | None = None

    def to_dict(self) -> dict[str, Any]:
        return {key: value for key, value in asdict(self).items() if value is not None}


@dataclass(frozen=True)
class MakerTakerFeeSchedule:
    """Maker/taker rates, in basis points, consumed by the shared Rust fee model."""

    maker_fee_bps: int
    taker_fee_bps: int

    def to_dict(self) -> dict[str, int]:
        return asdict(self)


@dataclass(frozen=True)
class PortfolioStrategySpec:
    """One strategy's deterministic priority and reserved account capital."""

    priority: int
    reserved_capital_raw: int
    strategy: BuiltinStrategySpec

    def to_dict(self) -> dict[str, Any]:
        return {
            "priority": self.priority,
            "reserved_capital_raw": self.reserved_capital_raw,
            "strategy": self.strategy.to_dict(),
        }


@dataclass(frozen=True)
class MultiMarketBacktestMarket:
    instrument: str
    bars_path: str
    priority: int
    strategies: tuple[PortfolioStrategySpec, ...]
    market_spec: TradingInstrumentSpec | dict[str, Any] | None = None
    fee_schedule: MakerTakerFeeSchedule | dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "instrument": self.instrument,
            "bars_path": self.bars_path,
            "priority": self.priority,
            "strategies": [strategy.to_dict() for strategy in self.strategies],
        }
        for key in ("market_spec", "fee_schedule"):
            item = getattr(self, key)
            if item is not None:
                value[key] = item.to_dict() if hasattr(item, "to_dict") else dict(item)
        return value


@dataclass(frozen=True)
class MultiMarketBacktestSpec:
    run_id: str
    account_id: str
    settlement_currency: str
    initial_cash_raw: int
    output_dir: str
    markets: tuple[MultiMarketBacktestMarket, ...]
    seed: int = 0
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "run_id": self.run_id,
            "account_id": self.account_id,
            "settlement_currency": self.settlement_currency,
            "initial_cash_raw": self.initial_cash_raw,
            "seed": self.seed,
            "output_dir": self.output_dir,
            "markets": [market.to_dict() for market in self.markets],
        }


@dataclass(frozen=True)
class MultiMarketDepthBacktestMarket:
    instrument: str
    depth_path: str
    priority: int
    market_spec: TradingInstrumentSpec | dict[str, Any]
    fee_bps: int = 0
    latency_snapshots: int = 0
    queue_position_bps: int = 0
    market_impact_bps: int = 0
    strategy: BuiltinStrategySpec = field(default_factory=lambda: BuiltinStrategySpec("sma_cross", "strategy"))
    order_policy: OrderPolicy | dict[str, Any] | None = None
    data_tier: str | None = None

    def to_dict(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "instrument": self.instrument,
            "depth_path": self.depth_path,
            "priority": self.priority,
            "market_spec": self.market_spec.to_dict() if hasattr(self.market_spec, "to_dict") else dict(self.market_spec),
            "fee_bps": self.fee_bps,
            "latency_snapshots": self.latency_snapshots,
            "queue_position_bps": self.queue_position_bps,
            "market_impact_bps": self.market_impact_bps,
            "strategy": self.strategy.to_dict(),
        }
        if self.order_policy is not None:
            value["order_policy"] = self.order_policy.to_dict() if hasattr(self.order_policy, "to_dict") else dict(self.order_policy)
        if self.data_tier is not None:
            value["data_tier"] = self.data_tier
        return value


@dataclass(frozen=True)
class MultiMarketDepthBacktestSpec:
    run_id: str
    account_id: str
    settlement_currency: str
    initial_cash_raw: int
    output_dir: str
    markets: tuple[MultiMarketDepthBacktestMarket, ...]
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "run_id": self.run_id,
            "account_id": self.account_id,
            "settlement_currency": self.settlement_currency,
            "initial_cash_raw": self.initial_cash_raw,
            "output_dir": self.output_dir,
            "markets": [market.to_dict() for market in self.markets],
        }


@dataclass(frozen=True)
class MultiMarketDepthBacktestOutcome:
    run_id: str
    account_id: str
    spec_fingerprint: str
    data_fingerprint: str
    shared_result_hash: str
    markets_total: int
    successful_fills: int
    pending_orders: int
    equity_points: int
    final_equity_raw: int
    return_bps: int
    max_drawdown_bps: int
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiMarketDepthBacktestOutcome":
        return cls(**value)


@dataclass(frozen=True)
class PortfolioBacktestConfig:
    """Additional same-account single-leg strategies in a deterministic Bar or depth run."""

    primary_priority: int
    primary_reserved_capital_raw: int
    additional_strategies: tuple[PortfolioStrategySpec, ...]

    def to_dict(self) -> dict[str, Any]:
        return {
            "primary_priority": self.primary_priority,
            "primary_reserved_capital_raw": self.primary_reserved_capital_raw,
            "additional_strategies": [
                member.to_dict() for member in self.additional_strategies
            ],
        }


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
    protective_exit: ProtectiveExitSpec | dict[str, Any] | None = None
    account_id: str | None = None
    market_spec: TradingInstrumentSpec | dict[str, Any] | None = None
    fee_schedule: MakerTakerFeeSchedule | dict[str, int] | None = None
    portfolio: PortfolioBacktestConfig | dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        value = {
            **asdict(self),
            "strategy": self.strategy.to_dict(),
        }
        if self.account_id is None:
            value.pop("account_id")
        if self.protective_exit is None:
            value.pop("protective_exit")
        elif isinstance(self.protective_exit, ProtectiveExitSpec):
            value["protective_exit"] = self.protective_exit.to_dict()
        if self.market_spec is None:
            value.pop("market_spec")
        elif isinstance(self.market_spec, TradingInstrumentSpec):
            value["market_spec"] = self.market_spec.to_dict()
        if self.fee_schedule is None:
            value.pop("fee_schedule")
        elif isinstance(self.fee_schedule, MakerTakerFeeSchedule):
            value["fee_schedule"] = self.fee_schedule.to_dict()
        if self.portfolio is None:
            value.pop("portfolio")
        elif isinstance(self.portfolio, PortfolioBacktestConfig):
            value["portfolio"] = self.portfolio.to_dict()
        return value


@dataclass(frozen=True)
class MultiAccountBacktestSpec:
    """A bounded batch of isolated account backtests sharing one settlement currency."""

    batch_id: str
    runs: tuple[BacktestSpec, ...]
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "batch_id": self.batch_id,
            "runs": [run.to_dict() for run in self.runs],
        }


@dataclass(frozen=True)
class MultiAccountRunResult:
    account_id: str
    outcome: BacktestOutcome | None
    error_json: str | None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiAccountRunResult":
        outcome = value.get("outcome")
        return cls(
            account_id=value["account_id"],
            outcome=BacktestOutcome.from_dict(outcome) if outcome else None,
            error_json=value.get("error_json"),
        )


@dataclass(frozen=True)
class MultiAccountBacktestResult:
    batch_id: str
    settlement_currency: str
    spec_fingerprint: str
    runs: tuple[MultiAccountRunResult, ...]
    summary: dict[str, Any]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiAccountBacktestResult":
        return cls(
            batch_id=value["batch_id"],
            settlement_currency=value["settlement_currency"],
            spec_fingerprint=value["spec_fingerprint"],
            runs=tuple(MultiAccountRunResult.from_dict(item) for item in value["runs"]),
            summary=dict(value["summary"]),
        )


@dataclass(frozen=True)
class MultiAccountDepthBacktestSpec:
    """Bounded independent Tick/OrderBook runs using the shared Rust depth kernel."""

    batch_id: str
    runs: tuple[DepthBacktestSpec, ...]
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "batch_id": self.batch_id,
            "runs": [run.to_dict() for run in self.runs],
        }


@dataclass(frozen=True)
class MultiAccountDepthRunResult:
    account_id: str
    outcome: "DepthBacktestOutcome | None"
    error_json: str | None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiAccountDepthRunResult":
        outcome = value.get("outcome")
        return cls(
            account_id=value["account_id"],
            outcome=DepthBacktestOutcome.from_dict(outcome) if outcome else None,
            error_json=value.get("error_json"),
        )


@dataclass(frozen=True)
class MultiAccountDepthBacktestResult:
    batch_id: str
    settlement_currency: str
    spec_fingerprint: str
    runs: tuple[MultiAccountDepthRunResult, ...]
    summary: dict[str, Any]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiAccountDepthBacktestResult":
        return cls(
            batch_id=value["batch_id"],
            settlement_currency=value["settlement_currency"],
            spec_fingerprint=value["spec_fingerprint"],
            runs=tuple(MultiAccountDepthRunResult.from_dict(item) for item in value["runs"]),
            summary=dict(value["summary"]),
        )


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
    account_id: str | None = None

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
    name: Literal[
        "fast_window",
        "slow_window",
        "period",
        "threshold_bps",
        "quantity_raw",
        "target_kelly_fraction_bps",
        "market_making_spread_bps",
        "market_making_quote_qty_raw",
        "market_making_max_inventory_raw",
        "market_making_tick_size_raw",
        "market_making_inventory_skew_bps",
    ]
    values: tuple[int, ...]

    def to_dict(self) -> dict[str, Any]:
        return {"name": self.name, "values": list(self.values)}


@dataclass(frozen=True)
class ExperimentSplitPlan:
    """Training/test split in bar indices; walk-forward windows are bounded by Rust."""

    train_bars: int
    test_bars: int
    walk_forward: bool = False
    step_bars: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "train_bars": self.train_bars,
            "test_bars": self.test_bars,
            "walk_forward": self.walk_forward,
            "step_bars": self.step_bars,
        }


@dataclass(frozen=True)
class RunExperimentSpec:
    experiment_id: str
    base: BacktestSpec
    parameter_space: tuple[ExperimentParameterSpace, ...]
    schema_version: int = 1
    split_plan: ExperimentSplitPlan | None = None
    baselines: tuple[Literal["buy-and-hold", "zero-signal", "none"], ...] = ()
    strategy_kinds: tuple[str, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        value = {
            "schema_version": self.schema_version,
            "experiment_id": self.experiment_id,
            "base": self.base.to_dict(),
            "parameter_space": [dimension.to_dict() for dimension in self.parameter_space],
        }
        # Match Rust's skip_serializing_if so callers upgrading the typed SDK do not change
        # fingerprints for legacy experiment documents.
        if self.split_plan is not None:
            value["split_plan"] = self.split_plan.to_dict()
        if self.baselines:
            value["baselines"] = list(self.baselines)
        if self.strategy_kinds:
            value["strategy_kinds"] = sorted(self.strategy_kinds)
        return value


@dataclass(frozen=True)
class RunMultiLegExperimentSpec:
    experiment_id: str
    base: MultiLegBacktestSpec
    parameter_space: tuple[ExperimentParameterSpace, ...]
    schema_version: int = 1
    strategy_kinds: tuple[str, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        value = {
            "schema_version": self.schema_version,
            "experiment_id": self.experiment_id,
            "base": self.base.to_dict(),
            "parameter_space": [dimension.to_dict() for dimension in self.parameter_space],
        }
        if self.strategy_kinds:
            value["strategy_kinds"] = sorted(self.strategy_kinds)
        return value


@dataclass(frozen=True)
class RunMultiMarketExperimentSpec:
    experiment_id: str
    target_strategy_id: str
    base: MultiMarketBacktestSpec
    parameter_space: tuple[ExperimentParameterSpace, ...]
    schema_version: int = 1
    strategy_kinds: tuple[str, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        value = {
            "schema_version": self.schema_version,
            "experiment_id": self.experiment_id,
            "target_strategy_id": self.target_strategy_id,
            "base": self.base.to_dict(),
            "parameter_space": [dimension.to_dict() for dimension in self.parameter_space],
        }
        if self.strategy_kinds:
            value["strategy_kinds"] = sorted(self.strategy_kinds)
        return value


@dataclass(frozen=True)
class RunMultiMarketDepthExperimentSpec:
    experiment_id: str
    target_strategy_id: str
    base: MultiMarketDepthBacktestSpec
    parameter_space: tuple[ExperimentParameterSpace, ...]
    schema_version: int = 1
    strategy_kinds: tuple[str, ...] = ()
    callback_parameter_space: tuple[ExperimentParameterSpace, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        value = {
            "schema_version": self.schema_version,
            "experiment_id": self.experiment_id,
            "target_strategy_id": self.target_strategy_id,
            "base": self.base.to_dict(),
            "parameter_space": [dimension.to_dict() for dimension in self.parameter_space],
        }
        if self.strategy_kinds:
            value["strategy_kinds"] = sorted(self.strategy_kinds)
        if self.callback_parameter_space:
            value["callback_parameter_space"] = [
                dimension.to_dict() for dimension in self.callback_parameter_space
            ]
        return value


@dataclass(frozen=True)
class RiskParityAssetSpec:
    instrument: str
    mark_price_raw: int
    returns_bps: tuple[int, ...]

    def to_dict(self) -> dict[str, Any]:
        return {"instrument": self.instrument, "mark_price_raw": self.mark_price_raw,
                "returns_bps": list(self.returns_bps)}


@dataclass(frozen=True)
class RiskParitySpec:
    portfolio_id: str
    available_cash_raw: int
    target_gross_notional_raw: int
    assets: tuple[RiskParityAssetSpec, ...]
    current_positions: dict[str, int] = field(default_factory=dict)
    max_turnover_bps: int = 10_000
    min_trade_notional_raw: int = 0
    schema_version: int = 1

    def to_dict(self) -> dict[str, Any]:
        return {"schema_version": self.schema_version, "portfolio_id": self.portfolio_id,
                "available_cash_raw": self.available_cash_raw,
                "target_gross_notional_raw": self.target_gross_notional_raw,
                "current_positions": self.current_positions,
                "assets": [asset.to_dict() for asset in self.assets],
                "max_turnover_bps": self.max_turnover_bps,
                "min_trade_notional_raw": self.min_trade_notional_raw}


@dataclass(frozen=True)
class RiskParityResult:
    schema_version: int
    portfolio_id: str
    plan_hash: str
    plan: dict[str, Any]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RiskParityResult":
        return cls(value["schema_version"], value["portfolio_id"], value["plan_hash"], value["plan"])


@dataclass(frozen=True)
class RunDepthExperimentSpec:
    experiment_id: str
    base: DepthBacktestSpec
    parameter_space: tuple[ExperimentParameterSpace, ...]
    schema_version: int = 1
    strategy_kinds: tuple[str, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        value = {
            "schema_version": self.schema_version,
            "experiment_id": self.experiment_id,
            "base": self.base.to_dict(),
            "parameter_space": [dimension.to_dict() for dimension in self.parameter_space],
        }
        if self.strategy_kinds:
            value["strategy_kinds"] = sorted(self.strategy_kinds)
        return value


@dataclass(frozen=True)
class ExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: BacktestOutcome | None
    error: str | None
    strategy_kind: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=BacktestOutcome.from_dict(raw_outcome) if raw_outcome is not None else None,
            error=value.get("error"),
            strategy_kind=value.get("strategy_kind"),
        )


@dataclass(frozen=True)
class ExperimentFoldReport:
    fold: int
    train_range: tuple[int, int]
    test_range: tuple[int, int]
    selected_ordinal: int
    selected_parameters: dict[str, int]
    in_sample_return_bps: int
    out_of_sample_return_bps: int
    selected_strategy_kind: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ExperimentFoldReport":
        return cls(
            fold=value["fold"],
            train_range=tuple(value["train_range"]),
            test_range=tuple(value["test_range"]),
            selected_ordinal=value["selected_ordinal"],
            selected_parameters=value["selected_parameters"],
            in_sample_return_bps=value["in_sample_return_bps"],
            out_of_sample_return_bps=value["out_of_sample_return_bps"],
            selected_strategy_kind=value.get("selected_strategy_kind"),
        )


@dataclass(frozen=True)
class ExperimentOutOfSampleReport:
    folds: tuple[ExperimentFoldReport, ...]
    in_sample_return_bps: int
    out_of_sample_return_bps: int
    degradation_bps: int

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ExperimentOutOfSampleReport":
        return cls(
            folds=tuple(ExperimentFoldReport.from_dict(item) for item in value["folds"]),
            in_sample_return_bps=value["in_sample_return_bps"],
            out_of_sample_return_bps=value["out_of_sample_return_bps"],
            degradation_bps=value["degradation_bps"],
        )


@dataclass(frozen=True)
class ExperimentBaselineReport:
    name: str
    return_bps: int
    max_drawdown_bps: int

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ExperimentBaselineReport":
        return cls(
            name=value["name"],
            return_bps=value["return_bps"],
            max_drawdown_bps=value["max_drawdown_bps"],
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
    out_of_sample: ExperimentOutOfSampleReport | None = None
    baselines: tuple[ExperimentBaselineReport, ...] = ()

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
            out_of_sample=(
                ExperimentOutOfSampleReport.from_dict(value["out_of_sample"])
                if value.get("out_of_sample") is not None
                else None
            ),
            baselines=tuple(
                ExperimentBaselineReport.from_dict(item) for item in value.get("baselines", ())
            ),
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
class MarketMakingSpec:
    spread_bps: int
    quote_qty_raw: int
    max_inventory_raw: int
    tick_size_raw: int
    inventory_skew_bps: int = 0

    def to_dict(self) -> dict[str, int]:
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
    market_making: MarketMakingSpec | dict[str, int] | None = None
    account_id: str | None = None
    portfolio: PortfolioBacktestConfig | dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        payload = {**asdict(self), "strategy": self.strategy.to_dict()}
        if isinstance(self.market_making, MarketMakingSpec):
            payload["market_making"] = self.market_making.to_dict()
        if isinstance(self.portfolio, PortfolioBacktestConfig):
            payload["portfolio"] = self.portfolio.to_dict()
        # Mirror Rust's skip_serializing_if on these optional v1 extensions so
        # legacy requests keep their historical JSON shape across SDKs.
        for optional_field in (
            "market_spec",
            "order_policy",
            "market_making",
            "account_id",
            "portfolio",
        ):
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
    account_id: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "DepthBacktestOutcome":
        return cls(**value)


@dataclass(frozen=True)
class DepthExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: DepthBacktestOutcome | None
    error: str | None
    strategy_kind: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "DepthExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=DepthBacktestOutcome.from_dict(raw_outcome) if raw_outcome else None,
            error=value.get("error"),
            strategy_kind=value.get("strategy_kind"),
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
class FundingRateEvent:
    """Historical derivative funding rate applied at its UTC 8-hour settlement."""

    instrument: InstrumentId | dict[str, str]
    settlement_ts: int
    rate_bps: int

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
    funding_events: tuple[FundingRateEvent | dict[str, Any], ...] = ()
    fill_model: Literal["next_bar_open", "best_price", "one_tick_slippage"] = "next_bar_open"
    schema_version: int = 1
    account_id: str | None = None

    def to_dict(self) -> dict[str, Any]:
        payload = {**asdict(self), "strategy": self.strategy.to_dict()}
        if self.funding_events:
            payload["funding_events"] = [
                event.to_dict() if isinstance(event, FundingRateEvent) else dict(event)
                for event in self.funding_events
            ]
        else:
            payload.pop("funding_events", None)
        if self.account_id is None:
            payload.pop("account_id")
        return payload


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
    account_id: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiLegBacktestOutcome":
        return cls(**value)


@dataclass(frozen=True)
class MultiLegExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: MultiLegBacktestOutcome | None
    error: str | None
    strategy_kind: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiLegExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=MultiLegBacktestOutcome.from_dict(raw_outcome) if raw_outcome else None,
            error=value.get("error"),
            strategy_kind=value.get("strategy_kind"),
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
class MultiMarketExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: MultiMarketBacktestOutcome | None
    error: str | None
    strategy_kind: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiMarketExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=MultiMarketBacktestOutcome.from_dict(raw_outcome) if raw_outcome else None,
            error=value.get("error"),
            strategy_kind=value.get("strategy_kind"),
        )


@dataclass(frozen=True)
class MultiMarketExperimentRankedCandidate:
    rank: int
    ordinal: int
    run_id: str
    return_bps: int
    max_drawdown_bps: int
    shared_result_hash: str


@dataclass(frozen=True)
class RunMultiMarketExperimentResult:
    schema_version: int
    experiment_id: str
    target_strategy_id: str
    spec_fingerprint: str
    total_candidates: int
    completed_candidates: int
    succeeded_candidates: int
    failed_candidates: int
    candidates: tuple[MultiMarketExperimentCandidateResult, ...]
    ranking: tuple[MultiMarketExperimentRankedCandidate, ...]
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunMultiMarketExperimentResult":
        return cls(
            schema_version=value["schema_version"],
            experiment_id=value["experiment_id"],
            target_strategy_id=value["target_strategy_id"],
            spec_fingerprint=value["spec_fingerprint"],
            total_candidates=value["total_candidates"],
            completed_candidates=value["completed_candidates"],
            succeeded_candidates=value["succeeded_candidates"],
            failed_candidates=value["failed_candidates"],
            candidates=tuple(MultiMarketExperimentCandidateResult.from_dict(item) for item in value["candidates"]),
            ranking=tuple(MultiMarketExperimentRankedCandidate(**item) for item in value["ranking"]),
            artifact_path=value["artifact_path"],
        )


@dataclass(frozen=True)
class MultiMarketDepthExperimentCandidateResult:
    ordinal: int
    parameters: dict[str, int]
    outcome: MultiMarketDepthBacktestOutcome | None
    error: str | None
    strategy_kind: str | None = None

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiMarketDepthExperimentCandidateResult":
        raw_outcome = value.get("outcome")
        return cls(
            ordinal=value["ordinal"],
            parameters=value["parameters"],
            outcome=MultiMarketDepthBacktestOutcome.from_dict(raw_outcome) if raw_outcome else None,
            error=value.get("error"),
            strategy_kind=value.get("strategy_kind"),
        )


@dataclass(frozen=True)
class MultiMarketDepthExperimentRankedCandidate:
    rank: int
    ordinal: int
    run_id: str
    return_bps: int
    max_drawdown_bps: int
    shared_result_hash: str


@dataclass(frozen=True)
class RunMultiMarketDepthExperimentResult:
    schema_version: int
    experiment_id: str
    target_strategy_id: str
    spec_fingerprint: str
    total_candidates: int
    completed_candidates: int
    succeeded_candidates: int
    failed_candidates: int
    candidates: tuple[MultiMarketDepthExperimentCandidateResult, ...]
    ranking: tuple[MultiMarketDepthExperimentRankedCandidate, ...]
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunMultiMarketDepthExperimentResult":
        return cls(
            schema_version=value["schema_version"],
            experiment_id=value["experiment_id"],
            target_strategy_id=value["target_strategy_id"],
            spec_fingerprint=value["spec_fingerprint"],
            total_candidates=value["total_candidates"],
            completed_candidates=value["completed_candidates"],
            succeeded_candidates=value["succeeded_candidates"],
            failed_candidates=value["failed_candidates"],
            candidates=tuple(MultiMarketDepthExperimentCandidateResult.from_dict(item) for item in value["candidates"]),
            ranking=tuple(MultiMarketDepthExperimentRankedCandidate(**item) for item in value["ranking"]),
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
        value = self._request_json_value(path, method=method, payload=payload)
        if not isinstance(value, dict):
            raise QianxingError("Rust API returned a non-object JSON document")
        return value

    def _request_json_value(
        self,
        path: str,
        *,
        method: str,
        payload: Any = None,
    ) -> Any:
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
        # 工作流 → 路由键写成两个字面量分支。原写法 `APP_ROUTE_TEMPLATES[f"{self.workflow}_commands"]`
        # 把「工作流名」与「路由键名」用一条没写在任何地方的命名约定绑在一起：两个键在源码里
        # 没有字面量读者（门禁核不出它们到底有没有被用上），键改名时也只会拿到运行时 KeyError。
        route = (
            APP_ROUTE_TEMPLATES["paper_commands"]
            if self.workflow == "paper"
            else APP_ROUTE_TEMPLATES["live_commands"]
        )
        return self._request_json(
            route,
            method="POST",
            payload={"schema_version": 1, "workflow": self.workflow, "command": command_document},
        )

    async def async_submit(self, command: TradeCommand | dict[str, Any]) -> dict[str, Any]:
        """Submit through the Rust workflow without blocking the event loop.

        Cancelling this coroutine does not undo a request already sent to the server; callers
        must retain the command ``request_id`` and query the audit/run state after uncertainty.
        """
        return await asyncio.to_thread(self.submit, command)


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

    def wait(self, timeout_ms: int | None = DEFAULT_RUN_WAIT_MS) -> str:
        """Wait for a terminal state or timeout; a timeout does not cancel the run.

        The default is finite (``DEFAULT_RUN_WAIT_MS``): plan §9 wants ``wait()``
        bounded unless the caller *explicitly* asks to wait forever, which is
        spelled ``timeout_ms=None``. Cancellation stays cooperative either way —
        ``cancel()`` from another thread ends the wait.
        """
        if timeout_ms is not None and timeout_ms < 0:
            raise ValueError("timeout_ms must be non-negative")
        return str(self._native.wait(timeout_ms))

    def events(self, after_seq: int = 0) -> list["RunEvent"]:
        """Read lifecycle events from the Rust run worker after an exclusive cursor."""
        if isinstance(after_seq, bool) or not isinstance(after_seq, int) or after_seq < 0:
            raise ValueError("after_seq must be a non-negative integer")
        try:
            return [
                RunEvent.from_dict(item)
                for item in json.loads(self._native.events_json(after_seq))
            ]
        except Exception as exc:
            raise _translate_native_error(exc) from exc

    def result(self) -> _ResultT | None:
        """Take the outcome once. Return None while running or after cancellation."""
        try:
            encoded = self._native.result_json()
        except Exception as exc:
            raise _translate_native_error(exc) from exc
        if encoded is None:
            return None
        return self._result_type.from_dict(json.loads(encoded))


@dataclass(frozen=True)
class RunEvent:
    seq: int
    run_id: str
    kind: Literal["started", "cancellation_requested", "succeeded", "cancelled", "failed"]
    status: Literal["running", "cancelling", "succeeded", "cancelled", "failed"]

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunEvent":
        return cls(**value)


class AsyncRunHandle(Generic[_ResultT]):
    """Asyncio view over the same cooperative Rust run handle."""

    def __init__(self, handle: RunHandle[_ResultT]):
        self._handle = handle

    @property
    def run_id(self) -> str:
        return self._handle.run_id

    async def async_status(self) -> str:
        return await asyncio.to_thread(lambda: self._handle.status)

    async def async_cancel(self) -> None:
        await asyncio.to_thread(self._handle.cancel)

    async def async_wait(self, timeout: float | None = DEFAULT_RUN_WAIT_MS / 1000) -> str:
        if timeout is not None and (not math.isfinite(timeout) or timeout < 0):
            raise ValueError("timeout must be finite and non-negative, or None")
        timeout_ms = None if timeout is None else math.ceil(timeout * 1000)
        return await asyncio.to_thread(self._handle.wait, timeout_ms)

    async def async_result(self) -> _ResultT | None:
        return await asyncio.to_thread(self._handle.result)

    async def async_events(self, after_seq: int = 0) -> list[RunEvent]:
        return await asyncio.to_thread(self._handle.events, after_seq)


class AsyncCoreClient:
    """Non-blocking asyncio facade over local Rust application use cases.

    All requests and result types are delegated to the synchronous functions above;
    Python only moves the blocking native calls off the event loop.
    """

    async def _start(self, method: Any, spec: Any) -> AsyncRunHandle[Any]:
        task = asyncio.create_task(asyncio.to_thread(method, spec))
        try:
            return AsyncRunHandle(await asyncio.shield(task))
        except asyncio.CancelledError:
            # Thread cancellation cannot interrupt PyO3/Rust work. Once the handle exists,
            # request cooperative Rust cancellation before returning cancellation to caller.
            try:
                handle = await task
                await asyncio.to_thread(handle.cancel)
            except Exception:
                pass
            raise

    async def async_validate_dataset(self, spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
        return await asyncio.to_thread(validate_dataset, spec)

    async def async_run_backtest(self, spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
        return await asyncio.to_thread(run_backtest, spec)

    async def async_start_backtest(self, spec: BacktestSpec | dict[str, Any]) -> AsyncRunHandle[BacktestOutcome]:
        return await self._start(start_backtest, spec)

    async def async_start_backtest_with_strategy(
        self,
        spec: BacktestSpec | dict[str, Any],
        strategy: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> AsyncRunHandle[BacktestOutcome]:
        def start(value: Any) -> RunHandle[BacktestOutcome]:
            return start_backtest_with_strategy(
                value,
                strategy,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                source_sha256=source_sha256,
            )

        return await self._start(start, spec)

    async def async_run_backtest_with_strategy_slots(
        self,
        spec: BacktestSpec | dict[str, Any],
        strategies: Sequence[Mapping[str, Any]],
    ) -> BacktestOutcome:
        return await asyncio.to_thread(run_backtest_with_strategy_slots, spec, strategies)

    async def async_start_backtest_with_strategy_slots(
        self,
        spec: BacktestSpec | dict[str, Any],
        strategies: Sequence[Mapping[str, Any]],
    ) -> AsyncRunHandle[BacktestOutcome]:
        def start(value: Any) -> RunHandle[BacktestOutcome]:
            return start_backtest_with_strategy_slots(value, strategies)

        return await self._start(start, spec)

    async def async_verify_run(self, outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
        return await asyncio.to_thread(verify_run, outcome)

    async def async_compare_runs(self, spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
        return await asyncio.to_thread(compare_runs, spec)

    async def async_run_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
        return await asyncio.to_thread(run_experiment, spec)

    async def async_run_experiment_with_strategy(
        self,
        spec: RunExperimentSpec | dict[str, Any],
        strategy_factory: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> RunExperimentResult:
        return await asyncio.to_thread(
            run_experiment_with_strategy,
            spec,
            strategy_factory,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_start_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> AsyncRunHandle[RunExperimentResult]:
        return await self._start(start_experiment, spec)

    async def async_start_experiment_with_strategy(
        self,
        spec: RunExperimentSpec | dict[str, Any],
        strategy_factory: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> AsyncRunHandle[RunExperimentResult]:
        def start(value: Any) -> RunHandle[RunExperimentResult]:
            return start_experiment_with_strategy(
                value,
                strategy_factory,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                source_sha256=source_sha256,
            )

        return await self._start(start, spec)

    async def async_run_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> DepthBacktestOutcome:
        return await asyncio.to_thread(run_depth_backtest, spec)

    async def async_start_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> AsyncRunHandle[DepthBacktestOutcome]:
        return await self._start(start_depth_backtest, spec)

    async def async_run_depth_backtest_with_strategy(
        self,
        spec: DepthBacktestSpec | dict[str, Any],
        strategy: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> DepthBacktestOutcome:
        return await asyncio.to_thread(
            run_depth_backtest_with_strategy,
            spec,
            strategy,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_start_depth_backtest_with_strategy(
        self,
        spec: DepthBacktestSpec | dict[str, Any],
        strategy: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> AsyncRunHandle[DepthBacktestOutcome]:
        def start(value: Any) -> RunHandle[DepthBacktestOutcome]:
            return start_depth_backtest_with_strategy(
                value,
                strategy,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                source_sha256=source_sha256,
            )

        return await self._start(start, spec)

    async def async_run_depth_backtest_with_strategy_slots(
        self,
        spec: DepthBacktestSpec | dict[str, Any],
        strategies: Sequence[Mapping[str, Any]],
    ) -> DepthBacktestOutcome:
        return await asyncio.to_thread(run_depth_backtest_with_strategy_slots, spec, strategies)

    async def async_start_depth_backtest_with_strategy_slots(
        self,
        spec: DepthBacktestSpec | dict[str, Any],
        strategies: Sequence[Mapping[str, Any]],
    ) -> AsyncRunHandle[DepthBacktestOutcome]:
        def start(value: Any) -> RunHandle[DepthBacktestOutcome]:
            return start_depth_backtest_with_strategy_slots(value, strategies)

        return await self._start(start, spec)

    async def async_run_multi_leg_backtest_with_strategy(
        self, spec: MultiLegBacktestSpec | dict[str, Any], strategy: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> MultiLegBacktestOutcome:
        return await asyncio.to_thread(
            run_multi_leg_backtest_with_strategy, spec, strategy,
            strategy_id=strategy_id, strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_run_multi_leg_experiment_with_strategy(
        self, spec: RunMultiLegExperimentSpec | dict[str, Any], strategy_factory: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> RunMultiLegExperimentResult:
        return await asyncio.to_thread(
            run_multi_leg_experiment_with_strategy, spec, strategy_factory,
            strategy_id=strategy_id, strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_start_multi_leg_experiment_with_strategy(
        self, spec: RunMultiLegExperimentSpec | dict[str, Any], strategy_factory: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> AsyncRunHandle[RunMultiLegExperimentResult]:
        def start(value: Any) -> RunHandle[RunMultiLegExperimentResult]:
            return start_multi_leg_experiment_with_strategy(
                value, strategy_factory, strategy_id=strategy_id,
                strategy_version=strategy_version, source_sha256=source_sha256,
            )
        return await self._start(start, spec)

    async def async_start_multi_leg_backtest_with_strategy(
        self, spec: MultiLegBacktestSpec | dict[str, Any], strategy: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> AsyncRunHandle[MultiLegBacktestOutcome]:
        def start(value: Any) -> RunHandle[MultiLegBacktestOutcome]:
            return start_multi_leg_backtest_with_strategy(
                value, strategy, strategy_id=strategy_id,
                strategy_version=strategy_version, source_sha256=source_sha256,
            )
        return await self._start(start, spec)

    async def async_verify_depth_run(self, outcome: DepthBacktestOutcome | dict[str, Any]) -> VerificationResult:
        return await asyncio.to_thread(verify_depth_run, outcome)

    async def async_run_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RunDepthExperimentResult:
        return await asyncio.to_thread(run_depth_experiment, spec)

    async def async_start_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> AsyncRunHandle[RunDepthExperimentResult]:
        return await self._start(start_depth_experiment, spec)

    async def async_run_depth_experiment_with_strategy(
        self,
        spec: RunDepthExperimentSpec | dict[str, Any],
        strategy_factory: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> RunDepthExperimentResult:
        return await asyncio.to_thread(
            run_depth_experiment_with_strategy,
            spec,
            strategy_factory,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_start_depth_experiment_with_strategy(
        self,
        spec: RunDepthExperimentSpec | dict[str, Any],
        strategy_factory: Any,
        *,
        strategy_id: str,
        strategy_version: str,
        source_sha256: str,
    ) -> AsyncRunHandle[RunDepthExperimentResult]:
        def start(value: Any) -> RunHandle[RunDepthExperimentResult]:
            return start_depth_experiment_with_strategy(
                value,
                strategy_factory,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                source_sha256=source_sha256,
            )

        return await self._start(start, spec)

    async def async_run_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
        return await asyncio.to_thread(run_multi_leg_backtest, spec)

    async def async_run_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> MultiMarketBacktestOutcome:
        return await asyncio.to_thread(run_multi_market_backtest, spec)

    async def async_start_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> AsyncRunHandle[MultiMarketBacktestOutcome]:
        return await self._start(start_multi_market_backtest, spec)

    async def async_run_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> MultiMarketDepthBacktestOutcome:
        return await asyncio.to_thread(run_multi_market_depth_backtest, spec)

    async def async_start_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> AsyncRunHandle[MultiMarketDepthBacktestOutcome]:
        return await self._start(start_multi_market_depth_backtest, spec)

    async def async_run_multi_market_depth_backtest_with_strategy_slots(
        self,
        spec: MultiMarketDepthBacktestSpec | dict[str, Any],
        strategies: Sequence[Mapping[str, Any]],
    ) -> MultiMarketDepthBacktestOutcome:
        return await asyncio.to_thread(
            run_multi_market_depth_backtest_with_strategy_slots, spec, strategies
        )

    async def async_start_multi_market_depth_backtest_with_strategy_slots(
        self,
        spec: MultiMarketDepthBacktestSpec | dict[str, Any],
        strategies: Sequence[Mapping[str, Any]],
    ) -> AsyncRunHandle[MultiMarketDepthBacktestOutcome]:
        def start(value: Any) -> RunHandle[MultiMarketDepthBacktestOutcome]:
            return start_multi_market_depth_backtest_with_strategy_slots(value, strategies)
        return await self._start(start, spec)

    async def async_run_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RunMultiMarketExperimentResult:
        return await asyncio.to_thread(run_multi_market_experiment, spec)

    async def async_start_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> AsyncRunHandle[RunMultiMarketExperimentResult]:
        return await self._start(start_multi_market_experiment, spec)

    async def async_run_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RunMultiMarketDepthExperimentResult:
        return await asyncio.to_thread(run_multi_market_depth_experiment, spec)

    async def async_start_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> AsyncRunHandle[RunMultiMarketDepthExperimentResult]:
        return await self._start(start_multi_market_depth_experiment, spec)

    async def async_run_multi_market_depth_experiment_with_strategy(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any], strategy_factory: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> RunMultiMarketDepthExperimentResult:
        return await asyncio.to_thread(
            run_multi_market_depth_experiment_with_strategy, spec, strategy_factory,
            strategy_id=strategy_id, strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_start_multi_market_depth_experiment_with_strategy(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any], strategy_factory: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> AsyncRunHandle[RunMultiMarketDepthExperimentResult]:
        def start(value: Any) -> RunHandle[RunMultiMarketDepthExperimentResult]:
            return start_multi_market_depth_experiment_with_strategy(
                value, strategy_factory, strategy_id=strategy_id,
                strategy_version=strategy_version, source_sha256=source_sha256,
            )
        return await self._start(start, spec)

    async def async_run_multi_market_experiment_with_strategy(
        self, spec: RunMultiMarketExperimentSpec | dict[str, Any], strategy_factory: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> RunMultiMarketExperimentResult:
        return await asyncio.to_thread(
            run_multi_market_experiment_with_strategy, spec, strategy_factory,
            strategy_id=strategy_id, strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    async def async_start_multi_market_experiment_with_strategy(
        self, spec: RunMultiMarketExperimentSpec | dict[str, Any], strategy_factory: Any, *,
        strategy_id: str, strategy_version: str, source_sha256: str,
    ) -> AsyncRunHandle[RunMultiMarketExperimentResult]:
        def start(value: Any) -> RunHandle[RunMultiMarketExperimentResult]:
            return start_multi_market_experiment_with_strategy(
                value, strategy_factory, strategy_id=strategy_id,
                strategy_version=strategy_version, source_sha256=source_sha256,
            )
        return await self._start(start, spec)

    async def async_run_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> MultiAccountBacktestResult:
        return await asyncio.to_thread(run_multi_account_backtest, spec)

    async def async_start_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> AsyncRunHandle[MultiAccountBacktestResult]:
        return await self._start(start_multi_account_backtest, spec)

    async def async_run_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> MultiAccountDepthBacktestResult:
        return await asyncio.to_thread(run_multi_account_depth_backtest, spec)

    async def async_start_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> AsyncRunHandle[MultiAccountDepthBacktestResult]:
        return await self._start(start_multi_account_depth_backtest, spec)

    async def async_start_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> AsyncRunHandle[MultiLegBacktestOutcome]:
        return await self._start(start_multi_leg_backtest, spec)

    async def async_run_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RunMultiLegExperimentResult:
        return await asyncio.to_thread(run_multi_leg_experiment, spec)

    async def async_start_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> AsyncRunHandle[RunMultiLegExperimentResult]:
        return await self._start(start_multi_leg_experiment, spec)


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

    async def async_document(self) -> dict[str, Any]:
        """Read the same Rust run document without blocking the caller's event loop."""
        return await asyncio.to_thread(self.document)

    def events(self, after_seq: int = 0) -> list[dict[str, Any]]:
        if isinstance(after_seq, bool) or not isinstance(after_seq, int) or after_seq < 0:
            raise ValueError("after_seq must be a non-negative integer")
        route = APP_ROUTE_TEMPLATES["run_events"].format(run_id=self.run_id)
        return self._client._request_json(f"{route}?after_seq={after_seq}", method="GET")

    async def async_events(self, after_seq: int = 0) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self.events, after_seq)

    @property
    def status(self) -> str:
        return str(self.document().get("status", "unknown"))

    async def async_status(self) -> str:
        return str((await self.async_document()).get("status", "unknown"))

    def cancel(self) -> dict[str, Any]:
        return self._client._request_json(
            APP_ROUTE_TEMPLATES["run_cancel"].format(run_id=self.run_id),
            method="POST",
            payload={},
        )

    async def async_cancel(self) -> dict[str, Any]:
        return await asyncio.to_thread(self.cancel)

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

    async def async_wait(self, timeout: float = 30.0, poll_interval: float = 0.25) -> str:
        """Await terminal Rust status with the same finite bounds as ``wait``."""
        if (
            not math.isfinite(timeout)
            or not math.isfinite(poll_interval)
            or timeout <= 0
            or poll_interval <= 0
        ):
            raise ValueError("timeout and poll_interval must be finite and positive")
        deadline = time.monotonic() + timeout
        while True:
            status = await self.async_status()
            if status in {"succeeded", "cancelled", "failed"}:
                return status
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return status
            await asyncio.sleep(min(poll_interval, remaining))

    def result(self) -> _ResultT | dict[str, Any] | None:
        document = self.document()
        return self._result_from_document(document)

    async def async_result(self) -> _ResultT | dict[str, Any] | None:
        return self._result_from_document(await self.async_document())

    def _result_from_document(self, document: dict[str, Any]) -> _ResultT | dict[str, Any] | None:
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

    def application_capabilities(self) -> dict[str, Any]:
        """Discover Rust use cases; venue/provider readiness remains runtime-specific."""
        value = self._request_json_value(
            APP_ROUTE_TEMPLATES["application_capabilities"], method="GET"
        )
        if (
            not isinstance(value, dict)
            or value.get("schema_version") != 1
            or value.get("execution_backend") != "shared-rust"
            or value.get("runtime_preflight_required") is not True
            or not isinstance(value.get("capabilities"), list)
            or not isinstance(value.get("limitations"), list)
        ):
            raise QianxingError(
                "Rust API returned an invalid application capability catalog",
                payload={"category": "INTERNAL_INVARIANT"},
            )
        return value

    async def async_application_capabilities(self) -> dict[str, Any]:
        return await asyncio.to_thread(self.application_capabilities)

    def provider_preflight(self, data_request: Mapping[str, Any]) -> dict[str, Any]:
        """Check a data request against configured Rust Providers without I/O."""
        return self._post(APP_ROUTE_TEMPLATES["provider_preflight"], data_request)

    async def async_provider_preflight(self, data_request: Mapping[str, Any]) -> dict[str, Any]:
        return await asyncio.to_thread(self.provider_preflight, data_request)

    def list_builtin_strategies(self) -> tuple[BuiltinStrategyInfo, ...]:
        """Fetch the same Rust-owned strategy catalog used by local SDK calls."""
        value = self._request_json_value(
            APP_ROUTE_TEMPLATES["builtin_strategy_catalog"], method="GET"
        )
        try:
            return _parse_builtin_strategy_catalog(value)
        except (TypeError, ValueError) as exc:
            raise QianxingError(
                "Rust API returned an invalid built-in strategy catalog",
                payload={"category": "INTERNAL_INVARIANT"},
            ) from exc

    async def async_list_builtin_strategies(self) -> tuple[BuiltinStrategyInfo, ...]:
        return await asyncio.to_thread(self.list_builtin_strategies)

    def get_live_events(self, after_seq: int | None = None) -> list[dict[str, Any]]:
        if after_seq is not None and (
            isinstance(after_seq, bool)
            or not isinstance(after_seq, int)
            or after_seq < 0
            or after_seq > (1 << 64) - 1
        ):
            raise ValueError("after_seq must be a u64 sequence or None")
        route = APP_ROUTE_TEMPLATES["live_events"]
        if after_seq is not None:
            route = f"{route}?after={after_seq}"
        events = self._request_json_value(route, method="GET")
        if not isinstance(events, list) or any(not isinstance(event, dict) for event in events):
            raise QianxingError("Rust API returned an invalid live event page")
        return events

    async def async_get_live_events(self, after_seq: int | None = None) -> list[dict[str, Any]]:
        return await asyncio.to_thread(self.get_live_events, after_seq)

    def _start(self, route: str, spec: Any, result_type: type[_ResultT]) -> RemoteRunHandle[_ResultT]:
        document = self._post(route, spec)
        run_id = document.get("run_id")
        if not isinstance(run_id, str):
            raise QianxingError("Rust API start response omitted run_id", payload=document)
        return RemoteRunHandle(self, run_id, result_type)

    async def _async_start(self, method: Any, spec: Any) -> RemoteRunHandle[Any]:
        task = asyncio.create_task(asyncio.to_thread(method, spec))
        try:
            return await asyncio.shield(task)
        except asyncio.CancelledError:
            # The HTTP request itself cannot be interrupted once urllib has sent it. If the
            # server accepted the run, request cooperative cancellation before propagating the
            # caller's cancellation so a detached worker is not silently left behind.
            try:
                handle = await task
                await handle.async_cancel()
            except Exception:
                pass
            raise

    def validate_dataset(self, spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
        return DatasetVerdict.from_dict(self._post(APP_ROUTE_TEMPLATES["validate_dataset"], spec))

    def run_backtest(self, spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
        return BacktestOutcome.from_dict(self._post(APP_ROUTE_TEMPLATES["backtest"], spec))

    def run_provider_backtest_with_manifest(
        self, spec: dict[str, Any]
    ) -> dict[str, Any]:
        """Run the shared Rust Provider selection, freeze and backtest workflow remotely."""
        return self._post(APP_ROUTE_TEMPLATES["provider_backtest"], spec)

    def run_provider_depth_backtest(self, spec: Mapping[str, Any]) -> dict[str, Any]:
        """Fetch Provider Quote snapshots and run the shared Rust depth backtest."""
        return self._post(APP_ROUTE_TEMPLATES["provider_depth_backtest"], spec)

    def start_backtest(self, spec: BacktestSpec | dict[str, Any]) -> RemoteRunHandle[BacktestOutcome]:
        return self._start(APP_ROUTE_TEMPLATES["backtest_start"], spec, BacktestOutcome)

    def verify_run(self, outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
        return VerificationResult.from_dict(self._post(APP_ROUTE_TEMPLATES["verify"], outcome))

    def compare_runs(self, spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
        return CompareRunsResult.from_dict(self._post(APP_ROUTE_TEMPLATES["compare_runs"], spec))

    def plan_risk_parity(self, spec: RiskParitySpec | dict[str, Any]) -> RiskParityResult:
        return RiskParityResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["plan_risk_parity"], spec)
        )

    def plan_algo_execution(self, spec: dict[str, Any]) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["plan_algo_execution"], spec)

    def plan_capital_allocation(self, spec: dict[str, Any]) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["plan_capital_allocation"], spec)

    def start_paper_algo_execution(self, request: dict[str, Any]) -> dict[str, Any]:
        """Persist a shared Rust TWAP/VWAP parent for the configured Paper worker."""
        return self._post(APP_ROUTE_TEMPLATES["paper_algo_start"], request)

    def get_paper_algo_execution(self, parent_id: str) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["paper_algo_get"], {"parent_id": parent_id})

    def cancel_paper_algo_execution(self, parent_id: str) -> dict[str, Any]:
        return self._post(APP_ROUTE_TEMPLATES["paper_algo_cancel"], {"parent_id": parent_id})

    def list_paper_algo_executions(
        self, *, after_parent_id: str | None = None, limit: int = 100
    ) -> dict[str, Any]:
        return self._post(
            APP_ROUTE_TEMPLATES["paper_algo_list"],
            {"after_parent_id": after_parent_id, "limit": limit},
        )

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

    def run_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> MultiMarketBacktestOutcome:
        return MultiMarketBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_backtest"], spec)
        )

    def start_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_backtest_start"], spec, MultiMarketBacktestOutcome
        )

    def run_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> MultiMarketDepthBacktestOutcome:
        return MultiMarketDepthBacktestOutcome.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_depth_backtest"], spec)
        )

    def start_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketDepthBacktestOutcome]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_depth_backtest_start"],
            spec,
            MultiMarketDepthBacktestOutcome,
        )

    def run_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RunMultiMarketExperimentResult:
        return RunMultiMarketExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_experiment"], spec)
        )

    def start_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiMarketExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_experiment_start"], spec, RunMultiMarketExperimentResult
        )

    def run_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RunMultiMarketDepthExperimentResult:
        return RunMultiMarketDepthExperimentResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_market_depth_experiment"], spec)
        )

    def start_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RemoteRunHandle[RunMultiMarketDepthExperimentResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_market_depth_experiment_start"],
            spec,
            RunMultiMarketDepthExperimentResult,
        )

    def run_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> MultiAccountBacktestResult:
        return MultiAccountBacktestResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_account_backtest"], spec)
        )

    def start_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountBacktestResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_account_backtest_start"],
            spec,
            MultiAccountBacktestResult,
        )

    def run_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> MultiAccountDepthBacktestResult:
        return MultiAccountDepthBacktestResult.from_dict(
            self._post(APP_ROUTE_TEMPLATES["multi_account_depth_backtest"], spec)
        )

    def start_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountDepthBacktestResult]:
        return self._start(
            APP_ROUTE_TEMPLATES["multi_account_depth_backtest_start"],
            spec,
            MultiAccountDepthBacktestResult,
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

    # Async facade methods deliberately delegate to the synchronous typed methods above.
    # This keeps schemas, error translation, and Rust routes single-sourced.
    async def async_validate_dataset(self, spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
        return await asyncio.to_thread(self.validate_dataset, spec)

    async def async_run_backtest(self, spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
        return await asyncio.to_thread(self.run_backtest, spec)

    async def async_start_backtest(self, spec: BacktestSpec | dict[str, Any]) -> RemoteRunHandle[BacktestOutcome]:
        return await self._async_start(self.start_backtest, spec)

    async def async_verify_run(self, outcome: BacktestOutcome | dict[str, Any]) -> VerificationResult:
        return await asyncio.to_thread(self.verify_run, outcome)

    async def async_compare_runs(self, spec: CompareRunsSpec | dict[str, Any]) -> CompareRunsResult:
        return await asyncio.to_thread(self.compare_runs, spec)

    async def async_plan_risk_parity(self, spec: RiskParitySpec | dict[str, Any]) -> RiskParityResult:
        return await asyncio.to_thread(self.plan_risk_parity, spec)

    async def async_run_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
        return await asyncio.to_thread(self.run_experiment, spec)

    async def async_start_experiment(self, spec: RunExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunExperimentResult]:
        return await self._async_start(self.start_experiment, spec)

    async def async_run_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> DepthBacktestOutcome:
        return await asyncio.to_thread(self.run_depth_backtest, spec)

    async def async_start_depth_backtest(self, spec: DepthBacktestSpec | dict[str, Any]) -> RemoteRunHandle[DepthBacktestOutcome]:
        return await self._async_start(self.start_depth_backtest, spec)

    async def async_verify_depth_run(self, outcome: DepthBacktestOutcome | dict[str, Any]) -> VerificationResult:
        return await asyncio.to_thread(self.verify_depth_run, outcome)

    async def async_run_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RunDepthExperimentResult:
        return await asyncio.to_thread(self.run_depth_experiment, spec)

    async def async_start_depth_experiment(self, spec: RunDepthExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunDepthExperimentResult]:
        return await self._async_start(self.start_depth_experiment, spec)

    async def async_run_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_leg_backtest, spec)

    async def async_run_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> MultiMarketBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_market_backtest, spec)

    async def async_start_multi_market_backtest(
        self, spec: MultiMarketBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketBacktestOutcome]:
        return await self._async_start(self.start_multi_market_backtest, spec)

    async def async_run_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> MultiMarketDepthBacktestOutcome:
        return await asyncio.to_thread(self.run_multi_market_depth_backtest, spec)

    async def async_start_multi_market_depth_backtest(
        self, spec: MultiMarketDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiMarketDepthBacktestOutcome]:
        return await self._async_start(self.start_multi_market_depth_backtest, spec)

    async def async_run_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RunMultiMarketExperimentResult:
        return await asyncio.to_thread(self.run_multi_market_experiment, spec)

    async def async_start_multi_market_experiment(self, spec: RunMultiMarketExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiMarketExperimentResult]:
        return await self._async_start(self.start_multi_market_experiment, spec)

    async def async_run_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RunMultiMarketDepthExperimentResult:
        return await asyncio.to_thread(self.run_multi_market_depth_experiment, spec)

    async def async_start_multi_market_depth_experiment(
        self, spec: RunMultiMarketDepthExperimentSpec | dict[str, Any]
    ) -> RemoteRunHandle[RunMultiMarketDepthExperimentResult]:
        return await self._async_start(self.start_multi_market_depth_experiment, spec)

    async def async_run_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> MultiAccountBacktestResult:
        return await asyncio.to_thread(self.run_multi_account_backtest, spec)

    async def async_start_multi_account_backtest(
        self, spec: MultiAccountBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountBacktestResult]:
        return await self._async_start(self.start_multi_account_backtest, spec)

    async def async_run_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> MultiAccountDepthBacktestResult:
        return await asyncio.to_thread(self.run_multi_account_depth_backtest, spec)

    async def async_start_multi_account_depth_backtest(
        self, spec: MultiAccountDepthBacktestSpec | dict[str, Any]
    ) -> RemoteRunHandle[MultiAccountDepthBacktestResult]:
        return await self._async_start(self.start_multi_account_depth_backtest, spec)

    async def async_start_multi_leg_backtest(self, spec: MultiLegBacktestSpec | dict[str, Any]) -> RemoteRunHandle[MultiLegBacktestOutcome]:
        return await self._async_start(self.start_multi_leg_backtest, spec)

    async def async_run_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RunMultiLegExperimentResult:
        return await asyncio.to_thread(self.run_multi_leg_experiment, spec)

    async def async_start_multi_leg_experiment(self, spec: RunMultiLegExperimentSpec | dict[str, Any]) -> RemoteRunHandle[RunMultiLegExperimentResult]:
        return await self._async_start(self.start_multi_leg_experiment, spec)


def validate_dataset(spec: DatasetSpec | dict[str, Any]) -> DatasetVerdict:
    """Validate a local Bar dataset using the shared Rust application use case."""
    return DatasetVerdict.from_dict(_invoke(native.app_validate_dataset, spec))


def run_backtest(spec: BacktestSpec | dict[str, Any]) -> BacktestOutcome:
    """Run the deterministic single-instrument Bar backtest."""
    return BacktestOutcome.from_dict(_invoke(native.app_run_backtest, spec))


def run_backtest_with_strategy(
    spec: BacktestSpec | dict[str, Any],
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> BacktestOutcome:
    """Run Python strategy decisions inside the shared Rust Bar engine.

    Python supplies only a decision callback. Rust owns risk, OMS, matching, accounting,
    artifacts and replay verification. The source digest is part of the deterministic run
    identity; provide the SHA-256 of the exact strategy artifact being executed.
    """
    try:
        from qianxing_bridge.app import run_backtest_with_strategy as _run_strategy

        result = _run_strategy(
            _encode_payload(spec),
            strategy,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )
        return BacktestOutcome.from_dict(result)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_backtest_with_strategy(
    spec: BacktestSpec | dict[str, Any],
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[BacktestOutcome]:
    """Start a caller-authored strategy in the cooperative Rust RunHandle lifecycle."""
    try:
        from qianxing_bridge.app import start_backtest_with_strategy as _start_strategy

        native_handle = _start_strategy(
            _encode_payload(spec),
            strategy,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )
        return RunHandle(native_handle, BacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def run_backtest_with_strategy_slots(
    spec: BacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> BacktestOutcome:
    """Run several identified Python callbacks in the strategy slots of a Bar portfolio.

    Each item contains ``strategy_id``, ``strategy_version``, ``source_sha256``, and
    ``strategy``. The Rust scheduler executes the callbacks in portfolio priority order;
    risk, OMS, matching, account ledger and replay verification remain shared Rust behavior.
    """
    try:
        from qianxing_bridge.app import run_backtest_with_strategy_slots as _run_slots

        return BacktestOutcome.from_dict(_run_slots(_encode_payload(spec), strategies))
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_backtest_with_strategy_slots(
    spec: BacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> RunHandle[BacktestOutcome]:
    """Start multiple identified Python callbacks on a cancellable Rust Bar portfolio."""
    try:
        from qianxing_bridge.app import start_backtest_with_strategy_slots as _start_slots

        native_handle = _start_slots(_encode_payload(spec), strategies)
        return RunHandle(native_handle, BacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


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


def plan_risk_parity(spec: RiskParitySpec | dict[str, Any]) -> RiskParityResult:
    """Plan a deterministic equal-risk-contribution rebalance in the Rust core."""
    return RiskParityResult.from_dict(_invoke(native.app_plan_risk_parity, spec))


def plan_capital_allocation(spec: dict[str, Any]) -> dict[str, Any]:
    """Allocate a portfolio cash budget deterministically in the shared Rust core.

    Lower numeric priority is served first; strategy id breaks ties. This planner
    does not submit orders or claim to execute a shared portfolio backtest.
    """
    return _invoke(native.app_plan_capital_allocation, spec)


def plan_algo_execution(spec: dict[str, Any]) -> dict[str, Any]:
    """Plan TWAP/VWAP child orders through the shared Rust application use case."""
    return _invoke(native.app_plan_algo_execution, spec)


def run_experiment(spec: RunExperimentSpec | dict[str, Any]) -> RunExperimentResult:
    """Run a bounded Cartesian grid through Rust backtests and return their Rust comparison."""
    return RunExperimentResult.from_dict(_invoke(native.app_run_experiment, spec))


def run_experiment_with_strategy(
    spec: RunExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunExperimentResult:
    """Run each Bar parameter candidate with a fresh Python strategy on the shared Rust engine.

    ``strategy_factory(candidate_spec_dict)`` creates one strategy instance per candidate.
    Its ``on_event(context, event)`` output still passes through Rust validation, risk, OMS,
    matching, accounting and replay. Strategy source identity binds the experiment fingerprint.
    """
    from qianxing_bridge.app import run_experiment_with_strategy as _run_strategy_experiment

    return RunExperimentResult.from_dict(_run_strategy_experiment(
        _encode_payload(spec),
        strategy_factory,
        strategy_id=strategy_id,
        strategy_version=strategy_version,
        source_sha256=source_sha256,
    ))


def run_multi_leg_experiment(
    spec: RunMultiLegExperimentSpec | dict[str, Any],
) -> RunMultiLegExperimentResult:
    """Run a bounded grid through the shared Rust multi-leg backtest use case."""
    return RunMultiLegExperimentResult.from_dict(
        _invoke(native.app_run_multi_leg_experiment, spec)
    )


def run_multi_leg_experiment_with_strategy(
    spec: RunMultiLegExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunMultiLegExperimentResult:
    """Run each bounded grid candidate with a fresh Python strategy on the Rust engine.

    ``strategy_factory(candidate_spec_dict)`` is called once per candidate. Its returned object
    implements ``on_event(context, event)`` and must not be shared between candidate runs.
    """
    from qianxing_bridge.app import (
        run_multi_leg_experiment_with_strategy as _run_strategy_experiment,
    )
    return RunMultiLegExperimentResult.from_dict(_run_strategy_experiment(
        _encode_payload(spec), strategy_factory, strategy_id=strategy_id,
        strategy_version=strategy_version, source_sha256=source_sha256,
    ))


def run_depth_experiment(
    spec: RunDepthExperimentSpec | dict[str, Any],
) -> RunDepthExperimentResult:
    """Run Tick/OrderBook candidates through the same Rust depth engine."""
    return RunDepthExperimentResult.from_dict(_invoke(native.app_run_depth_experiment, spec))


def run_depth_experiment_with_strategy(
    spec: RunDepthExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunDepthExperimentResult:
    """Run each Tick/OrderBook parameter candidate with a fresh Python strategy.

    The candidate spec is supplied to the factory after Rust applies its parameter values.
    Callback decisions still pass through the shared Rust depth matching and accounting path.
    """
    from qianxing_bridge.app import (
        run_depth_experiment_with_strategy as _run_strategy_experiment,
    )

    return RunDepthExperimentResult.from_dict(_run_strategy_experiment(
        _encode_payload(spec), strategy_factory, strategy_id=strategy_id,
        strategy_version=strategy_version, source_sha256=source_sha256,
    ))


def start_depth_experiment(
    spec: RunDepthExperimentSpec | dict[str, Any],
) -> RunHandle[RunDepthExperimentResult]:
    """Start a cancellable Tick/OrderBook experiment in the Rust run lifecycle."""
    try:
        native_handle = native.app_start_depth_experiment(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunDepthExperimentResult)


def start_depth_experiment_with_strategy(
    spec: RunDepthExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[RunDepthExperimentResult]:
    """Start a cancellable Rust depth parameter experiment with custom strategy callbacks."""
    from qianxing_bridge.app import (
        start_depth_experiment_with_strategy as _start_strategy_experiment,
    )

    try:
        native_handle = _start_strategy_experiment(
            _encode_payload(spec), strategy_factory, strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
        )
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


def run_depth_backtest_with_strategy(
    spec: DepthBacktestSpec | dict[str, Any],
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> DepthBacktestOutcome:
    """Run a Python Strategy callback on the shared Rust Tick/OrderBook engines."""
    try:
        from qianxing_bridge.app import run_depth_backtest_with_strategy as _run_strategy

        value = _run_strategy(
            _encode_payload(spec),
            strategy,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )
        return DepthBacktestOutcome.from_dict(value)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def run_depth_backtest_with_strategy_slots(
    spec: DepthBacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> DepthBacktestOutcome:
    """Run multiple Python callbacks inside one Rust Tick/OrderBook portfolio ledger."""
    try:
        from qianxing_bridge.app import run_depth_backtest_with_strategy_slots as _run_slots

        value = _run_slots(_encode_payload(spec), strategies)
        return DepthBacktestOutcome.from_dict(value)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def run_multi_leg_backtest_with_strategy(
    spec: MultiLegBacktestSpec | dict[str, Any], strategy: Any, *,
    strategy_id: str, strategy_version: str, source_sha256: str,
) -> MultiLegBacktestOutcome:
    """Run Python-authored signal decisions through shared Rust two-leg engines."""
    try:
        from qianxing_bridge.app import run_multi_leg_backtest_with_strategy as _run_strategy
        value = _run_strategy(
            _encode_payload(spec), strategy, strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
        )
        return MultiLegBacktestOutcome.from_dict(value)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_multi_leg_backtest_with_strategy(
    spec: MultiLegBacktestSpec | dict[str, Any], strategy: Any, *,
    strategy_id: str, strategy_version: str, source_sha256: str,
) -> RunHandle[MultiLegBacktestOutcome]:
    """Start a cancellable Python strategy run on Rust's two-leg lifecycle."""
    try:
        from qianxing_bridge.app import start_multi_leg_backtest_with_strategy as _start_strategy
        native_handle = _start_strategy(
            _encode_payload(spec), strategy, strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
        )
        return RunHandle(native_handle, MultiLegBacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_depth_backtest_with_strategy(
    spec: DepthBacktestSpec | dict[str, Any],
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[DepthBacktestOutcome]:
    """Start a cancellable Python Strategy callback on the Rust depth RunHandle."""
    try:
        from qianxing_bridge.app import start_depth_backtest_with_strategy as _start_strategy

        native_handle = _start_strategy(
            _encode_payload(spec),
            strategy,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )
        return RunHandle(native_handle, DepthBacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_depth_backtest_with_strategy_slots(
    spec: DepthBacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> RunHandle[DepthBacktestOutcome]:
    """Start a cancellable Rust Tick/OrderBook portfolio with multiple Python callbacks."""
    try:
        from qianxing_bridge.app import start_depth_backtest_with_strategy_slots as _start_slots

        native_handle = _start_slots(_encode_payload(spec), strategies)
        return RunHandle(native_handle, DepthBacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def run_multi_leg_backtest(spec: MultiLegBacktestSpec | dict[str, Any]) -> MultiLegBacktestOutcome:
    """Run a two-leg Bar backtest via the shared Rust strategy and matching engines."""
    return MultiLegBacktestOutcome.from_dict(_invoke(native.app_run_multi_leg_backtest, spec))


@dataclass(frozen=True)
class MultiMarketBacktestOutcome:
    run_id: str
    account_id: str
    spec_fingerprint: str
    data_fingerprint: str
    shared_result_hash: str
    markets_total: int
    strategies_total: int
    successful_fills: int
    equity_points: int
    final_equity_raw: int
    return_bps: int
    max_drawdown_bps: int
    artifact_path: str

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "MultiMarketBacktestOutcome":
        return cls(**value)


def run_multi_market_backtest(
    spec: MultiMarketBacktestSpec | dict[str, Any],
) -> MultiMarketBacktestOutcome:
    """Run 3..32 markets through one deterministic Rust account ledger and event engine."""
    return MultiMarketBacktestOutcome.from_dict(
        _invoke(native.app_run_multi_market_backtest, spec)
    )


def run_multi_market_depth_backtest(
    spec: MultiMarketDepthBacktestSpec | dict[str, Any],
) -> MultiMarketDepthBacktestOutcome:
    return MultiMarketDepthBacktestOutcome.from_dict(
        _invoke(native.app_run_multi_market_depth_backtest, spec)
    )


def start_multi_market_depth_backtest(
    spec: MultiMarketDepthBacktestSpec | dict[str, Any],
) -> RunHandle[MultiMarketDepthBacktestOutcome]:
    try:
        native_handle = native.app_start_multi_market_depth_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, MultiMarketDepthBacktestOutcome)


def run_multi_market_depth_backtest_with_strategy_slots(
    spec: MultiMarketDepthBacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> MultiMarketDepthBacktestOutcome:
    try:
        from qianxing_bridge.app import run_multi_market_depth_backtest_with_strategy_slots as _run
        return MultiMarketDepthBacktestOutcome.from_dict(_run(_encode_payload(spec), strategies))
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_multi_market_depth_backtest_with_strategy_slots(
    spec: MultiMarketDepthBacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> RunHandle[MultiMarketDepthBacktestOutcome]:
    try:
        from qianxing_bridge.app import start_multi_market_depth_backtest_with_strategy_slots as _start
        return RunHandle(_start(_encode_payload(spec), strategies), MultiMarketDepthBacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_multi_market_backtest(
    spec: MultiMarketBacktestSpec | dict[str, Any],
) -> RunHandle[MultiMarketBacktestOutcome]:
    """Start a cancellable N-market run owned by the shared Rust application layer."""
    try:
        native_handle = native.app_start_multi_market_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, MultiMarketBacktestOutcome)


def run_multi_market_experiment(
    spec: RunMultiMarketExperimentSpec | dict[str, Any],
) -> RunMultiMarketExperimentResult:
    return RunMultiMarketExperimentResult.from_dict(
        _invoke(native.app_run_multi_market_experiment, spec)
    )


def start_multi_market_experiment(
    spec: RunMultiMarketExperimentSpec | dict[str, Any],
) -> RunHandle[RunMultiMarketExperimentResult]:
    try:
        native_handle = native.app_start_multi_market_experiment(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunMultiMarketExperimentResult)


def run_multi_market_depth_experiment(
    spec: RunMultiMarketDepthExperimentSpec | dict[str, Any],
) -> RunMultiMarketDepthExperimentResult:
    return RunMultiMarketDepthExperimentResult.from_dict(
        _invoke(native.app_run_multi_market_depth_experiment, spec)
    )


def start_multi_market_depth_experiment(
    spec: RunMultiMarketDepthExperimentSpec | dict[str, Any],
) -> RunHandle[RunMultiMarketDepthExperimentResult]:
    try:
        native_handle = native.app_start_multi_market_depth_experiment(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunMultiMarketDepthExperimentResult)


def run_multi_market_depth_experiment_with_strategy(
    spec: RunMultiMarketDepthExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunMultiMarketDepthExperimentResult:
    try:
        from qianxing_bridge.app import run_multi_market_depth_experiment_with_strategy as _run
        return RunMultiMarketDepthExperimentResult.from_dict(_run(
            _encode_payload(spec), strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
            factory=strategy_factory,
        ))
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_multi_market_depth_experiment_with_strategy(
    spec: RunMultiMarketDepthExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[RunMultiMarketDepthExperimentResult]:
    try:
        from qianxing_bridge.app import start_multi_market_depth_experiment_with_strategy as _start
        native_handle = _start(
            _encode_payload(spec), strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
            factory=strategy_factory,
        )
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, RunMultiMarketDepthExperimentResult)


def run_multi_market_experiment_with_strategy(
    spec: RunMultiMarketExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunMultiMarketExperimentResult:
    try:
        from qianxing_bridge.app import run_multi_market_experiment_with_strategy as _run
        return RunMultiMarketExperimentResult.from_dict(_run(
            _encode_payload(spec), strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
            factory=strategy_factory,
        ))
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_multi_market_experiment_with_strategy(
    spec: RunMultiMarketExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[RunMultiMarketExperimentResult]:
    try:
        from qianxing_bridge.app import start_multi_market_experiment_with_strategy as _start
        native_handle = _start(
            _encode_payload(spec), strategy_id=strategy_id,
            strategy_version=strategy_version, source_sha256=source_sha256,
            factory=strategy_factory,
        )
        return RunHandle(native_handle, RunMultiMarketExperimentResult)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def run_multi_market_backtest_with_strategy_slots(
    spec: MultiMarketBacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> MultiMarketBacktestOutcome:
    """Run identified Python callbacks in declared market slots through the same Rust portfolio engine."""
    try:
        from qianxing_bridge.app import run_multi_market_backtest_with_strategy_slots as _run
        return MultiMarketBacktestOutcome.from_dict(_run(_encode_payload(spec), strategies))
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def start_multi_market_backtest_with_strategy_slots(
    spec: MultiMarketBacktestSpec | dict[str, Any],
    strategies: Sequence[Mapping[str, Any]],
) -> RunHandle[MultiMarketBacktestOutcome]:
    """Start a cancellable Rust shared-ledger N-market run with Python callback slots."""
    try:
        from qianxing_bridge.app import start_multi_market_backtest_with_strategy_slots as _start
        return RunHandle(_start(_encode_payload(spec), strategies), MultiMarketBacktestOutcome)
    except Exception as exc:
        raise _translate_native_error(exc) from exc


def run_multi_account_backtest(
    spec: MultiAccountBacktestSpec | dict[str, Any],
) -> MultiAccountBacktestResult:
    """Run isolated account Bar backtests through the shared Rust engine."""
    return MultiAccountBacktestResult.from_dict(
        _invoke(native.app_run_multi_account_backtest, spec)
    )


def start_multi_account_backtest(
    spec: MultiAccountBacktestSpec | dict[str, Any],
) -> RunHandle[MultiAccountBacktestResult]:
    """Start a cancellable isolated-account batch in the shared Rust kernel."""
    try:
        native_handle = native.app_start_multi_account_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, MultiAccountBacktestResult)


def run_multi_account_depth_backtest(
    spec: MultiAccountDepthBacktestSpec | dict[str, Any],
) -> MultiAccountDepthBacktestResult:
    return MultiAccountDepthBacktestResult.from_dict(
        _invoke(native.app_run_multi_account_depth_backtest, spec)
    )


def start_multi_account_depth_backtest(
    spec: MultiAccountDepthBacktestSpec | dict[str, Any],
) -> RunHandle[MultiAccountDepthBacktestResult]:
    from qianxing_bridge import native

    try:
        native_handle = native.app_start_multi_account_depth_backtest(_encode_payload(spec))
    except Exception as exc:
        raise _translate_native_error(exc) from exc
    return RunHandle(native_handle, MultiAccountDepthBacktestResult)


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


def start_experiment_with_strategy(
    spec: RunExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[RunExperimentResult]:
    """Start a cancellable experiment with a fresh Python strategy per Rust candidate."""
    from qianxing_bridge.app import start_experiment_with_strategy as _start_strategy_experiment

    try:
        native_handle = _start_strategy_experiment(
            _encode_payload(spec),
            strategy_factory,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )
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


def start_multi_leg_experiment_with_strategy(
    spec: RunMultiLegExperimentSpec | dict[str, Any],
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> RunHandle[RunMultiLegExperimentResult]:
    """Start a cancellable Rust experiment with one fresh Python strategy per candidate."""
    from qianxing_bridge.app import (
        start_multi_leg_experiment_with_strategy as _start_strategy_experiment,
    )
    native_handle = _start_strategy_experiment(
        _encode_payload(spec), strategy_factory, strategy_id=strategy_id,
        strategy_version=strategy_version, source_sha256=source_sha256,
    )
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
                "portfolio.risk-parity.plan.v1",
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
