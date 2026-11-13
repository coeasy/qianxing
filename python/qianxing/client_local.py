"""Asyncio facade over local Rust application use cases."""

from __future__ import annotations

import asyncio
import json
import math
from dataclasses import dataclass
from typing import Any, Generic, Literal, Mapping, Sequence, TypeVar

from .app import (
    DEFAULT_RUN_WAIT_MS,
    _translate_native_error,
    BacktestOutcome,
    BacktestSpec,
    CompareRunsResult,
    CompareRunsSpec,
    DatasetSpec,
    DatasetVerdict,
    DepthBacktestOutcome,
    DepthBacktestSpec,
    MultiAccountBacktestResult,
    MultiAccountBacktestSpec,
    MultiAccountDepthBacktestResult,
    MultiAccountDepthBacktestSpec,
    MultiAccountJointBacktestOutcome,
    MultiAccountJointBacktestSpec,
    MultiLegBacktestOutcome,
    MultiLegBacktestSpec,
    MultiMarketBacktestOutcome,
    MultiMarketBacktestSpec,
    MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec,
    RunDepthExperimentResult,
    RunDepthExperimentSpec,
    RunExperimentResult,
    RunExperimentSpec,
    RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec,
    RunMultiMarketDepthExperimentResult,
    RunMultiMarketDepthExperimentSpec,
    RunMultiMarketExperimentResult,
    RunMultiMarketExperimentSpec,
    VerificationResult,
    compare_runs,
    run_backtest,
    run_backtest_with_strategy_slots,
    run_depth_backtest,
    run_depth_backtest_with_strategy,
    run_depth_backtest_with_strategy_slots,
    run_depth_experiment,
    run_depth_experiment_with_strategy,
    run_experiment,
    run_experiment_with_strategy,
    run_multi_account_backtest,
    run_multi_account_depth_backtest,
    run_multi_leg_backtest,
    run_multi_leg_backtest_with_strategy,
    run_multi_leg_experiment,
    run_multi_leg_experiment_with_strategy,
    run_multi_market_backtest,
    run_multi_market_depth_backtest,
    run_multi_market_depth_backtest_with_strategy_slots,
    run_multi_market_depth_experiment,
    run_multi_market_depth_experiment_with_strategy,
    run_multi_market_experiment,
    run_multi_market_experiment_with_strategy,
    start_backtest,
    start_backtest_with_strategy,
    start_backtest_with_strategy_slots,
    start_depth_backtest,
    start_depth_backtest_with_strategy,
    start_depth_backtest_with_strategy_slots,
    start_depth_experiment,
    start_depth_experiment_with_strategy,
    start_experiment,
    start_experiment_with_strategy,
    start_multi_account_backtest,
    start_multi_account_depth_backtest,
    start_multi_leg_backtest,
    start_multi_leg_backtest_with_strategy,
    start_multi_leg_experiment,
    start_multi_leg_experiment_with_strategy,
    start_multi_market_backtest,
    start_multi_market_depth_backtest,
    start_multi_market_depth_backtest_with_strategy_slots,
    start_multi_market_depth_experiment,
    start_multi_market_depth_experiment_with_strategy,
    start_multi_market_experiment,
    start_multi_market_experiment_with_strategy,
    validate_dataset,
    verify_depth_run,
    verify_run,
)


_ResultT = TypeVar("_ResultT")


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
    event_id: str = ""

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "RunEvent":
        if "event_id" not in value:
            value = {**value, "event_id": f"{value['run_id']}:{value['seq']}"}
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

    async def async_run_multi_account_joint_backtest(
        self, spec: MultiAccountJointBacktestSpec | dict[str, Any]
    ) -> dict[str, Any]:
        return await asyncio.to_thread(self.run_multi_account_joint_backtest, spec)

    async def async_start_multi_account_joint_backtest(
        self, spec: MultiAccountJointBacktestSpec | dict[str, Any]
    ) -> AsyncRunHandle[MultiAccountJointBacktestOutcome]:
        return await self._async_start(self.start_multi_account_joint_backtest, spec)

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

