"""应用层用例的 Python 门面（T2-2 / 退出门 G1）。

应用函数与 ``qx-cli app`` 子命令、``POST /app/*`` 路由调的是**同一组 ``qx-app`` 用例**：
进出都是 JSON 文档，所以「同一 use case 三入口结果哈希相同、错误 code 与 correlation id 相同」
在这条链上是构造性的，不是靠约定维持的。

失败一律抛 ``QxAppError``，异常文本就是 ``AppError`` 的 JSON 文档::

    {"action": ..., "category": ..., "correlation_id": ..., "message": ...,
     "reason_code": ..., "retry": ..., "safe_to_retry": ..., "source_code": ...}

调用方按 ``category`` 判断行动大类；有 ``reason_code`` 时可进一步区分具体拒绝原因。
**不要**按 ``message`` 分支——那是中文展示层。

扩展未构建（没跑过 ``cargo build -p qx-python`` 或 ``tools/build_python_wheel.*``）时这里会如实
抛 ``RuntimeError``，不做静默回退：本模块没有第二份实现，"回退"就等于"Python 侧自己再算一遍"，
而那正是 G1 要消灭的形状。
"""

from __future__ import annotations

from collections import deque
from functools import wraps
import json
from collections.abc import Mapping, Sequence
import sys
from typing import Any

from qianxing_strategy.base import (
    BaseStrategy,
    StrategyDataPortal,
    StrategyWarmupBinding,
    _dispatch_on_error,
    _dispatch_on_stop,
    declared_warmup_bars,
)
from qianxing_strategy.errors import StrategyErrorV1

from . import native


def _strategy_error_boundary(stage: str):
    """Notify a strategy before a local callback failure crosses into Rust."""
    def decorate(handler):
        @wraps(handler)
        def wrapped(self, context_json: str, *args, **kwargs):
            context: dict[str, Any] = {}
            previous_stage = getattr(self, "_callback_error_stage", None)
            self._callback_error_stage = stage
            try:
                decoded = json.loads(context_json)
                if isinstance(decoded, dict):
                    context = self._context_with_data(decoded)
            except Exception:
                # The callback still owns the authoritative parse/validation error.
                pass
            try:
                return handler(self, context_json, *args, **kwargs)
            except Exception as error:
                try:
                    timestamp, instrument = _strategy_error_event_details(args)
                    error_v1 = StrategyErrorV1.from_context(
                        context,
                        error,
                        stage=getattr(self, "_callback_error_stage", stage),
                        timestamp=timestamp,
                        instrument=instrument,
                    )
                except (TypeError, ValueError):
                    error_v1 = None
                try:
                    _dispatch_on_error(self.strategy, context, error, error_v1)
                except Exception as hook_error:
                    sys.stderr.write(
                        "strategy on_error hook failed: "
                        f"{type(hook_error).__name__}; original callback error retained\n"
                    )
                    sys.stderr.flush()
                raise
            finally:
                self._callback_error_stage = previous_stage

        return wrapped

    return decorate


def _strategy_error_event_details(args: Sequence[Any]) -> tuple[int | None, str | None]:
    """Extract only the event timestamp and instrument needed by StrategyErrorV1."""
    for value in args:
        if isinstance(value, str):
            try:
                value = json.loads(value)
            except json.JSONDecodeError:
                continue
        if not isinstance(value, Mapping):
            continue
        payload: Any = value
        if len(value) == 1:
            candidate = next(iter(value.values()))
            if isinstance(candidate, Mapping):
                payload = candidate
        order = payload.get("order") if isinstance(payload, Mapping) else None
        if isinstance(order, Mapping):
            instrument = order.get("instrument")
        else:
            instrument = payload.get("instrument") if isinstance(payload, Mapping) else None
        if isinstance(instrument, Mapping):
            symbol = instrument.get("symbol")
            venue = instrument.get("venue")
            instrument = f"{symbol}.{venue}" if symbol and venue else None
        if not isinstance(instrument, str) or not instrument.strip():
            instrument = None
        timestamp = payload.get("ts") if isinstance(payload, Mapping) else None
        if not isinstance(timestamp, int) or isinstance(timestamp, bool) or timestamp <= 0:
            fill = payload.get("fill") if isinstance(payload, Mapping) else None
            timestamp = fill.get("ts") if isinstance(fill, Mapping) else None
        if not isinstance(timestamp, int) or isinstance(timestamp, bool) or timestamp <= 0:
            timestamp = None
        if timestamp is not None or instrument is not None:
            return timestamp, instrument
    return None, None


def _payload(value: "str | dict[str, Any]") -> str:
    """接受现成的 JSON 字符串，也接受 dict——两者最终交给 Rust 的是同一份字节。"""
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)


class _PythonStrategyFactory:
    """Decode each Rust candidate spec and adapt its fresh strategy to the callback contract."""

    def __init__(self, factory: Any, *, bar_warmup: bool = True) -> None:
        self.factory = factory
        self.bar_warmup = bar_warmup

    def __call__(self, candidate_json: str, parameters_json: str | None = None) -> _PythonBacktestCallback:
        candidate = json.loads(candidate_json)
        if parameters_json is None:
            strategy = self.factory(candidate)
        else:
            strategy = self.factory(candidate, json.loads(parameters_json))
        if strategy is None:
            raise TypeError("Python strategy factory must return a strategy object")
        return _PythonBacktestCallback(strategy, bar_warmup=self.bar_warmup)


def validate_dataset(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """校验一份数据集能否支撑 Bar 回测，返回 ``DatasetVerdict``。

    "数据不足"是**成功返回**（``usable=false`` + 非空 ``gaps``），不是异常。
    """
    return json.loads(native.app_validate_dataset(_payload(spec)))


def run_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """跑一次 Bar 回测并落四份产物，返回 ``BacktestOutcome``。"""
    return json.loads(native.app_run_backtest(_payload(spec)))


class _PythonBacktestCallback:
    """Serialize the Python callback boundary; decisions are validated by qx-strategy."""

    _MAX_HISTORY_BARS_PER_INSTRUMENT = 10_000

    def __init__(self, strategy: Any, *, bar_warmup: bool = True) -> None:
        if isinstance(strategy, StrategyWarmupBinding):
            self.strategy = strategy.strategy
            self._warmup_bars = strategy.warmup_bars
            self._manifest_warmup_bars = strategy.warmup_bars
            self._history_requirements_checked = strategy.history_requirements_checked
        else:
            self.strategy = strategy
            self._warmup_bars = 0
            self._manifest_warmup_bars = None
            self._history_requirements_checked = False
        self._bar_warmup = bar_warmup
        if (
            not bar_warmup
            and self._history_requirements_checked
            and self._warmup_bars
        ):
            raise ValueError(
                "history_requirements.warmup_bars is supported only for Bar events"
            )
        self._warmup_seen: dict[str, tuple[int, Any]] = {}
        self._warmup_completed: set[str] = set()
        self._bar_history: dict[str, deque[Mapping[str, Any]]] = {}
        self._last_context: dict[str, Any] | None = None

    @_strategy_error_boundary("initialization")
    def _qx_on_init(self, context_json: str) -> None:
        context = self._context_with_data(json.loads(context_json))
        self._resolve_history_requirements()
        handler = getattr(self.strategy, "on_init", None)
        if handler is not None:
            handler(context)

    def _resolve_history_requirements(self) -> None:
        if self._history_requirements_checked:
            return
        previous_stage = getattr(self, "_callback_error_stage", "initialization")
        self._callback_error_stage = "history_requirements"
        try:
            declared = declared_warmup_bars(self.strategy)
            if (
                self._manifest_warmup_bars is not None
                and declared is not None
                and declared != self._manifest_warmup_bars
            ):
                raise ValueError(
                    "strategy history_requirements.warmup_bars conflicts with "
                    "STRATEGY_MANIFEST.warmup_bars"
                )
            self._warmup_bars = (
                declared
                if declared is not None
                else (self._manifest_warmup_bars or 0)
            )
            if not self._bar_warmup and self._warmup_bars:
                raise ValueError(
                    "history_requirements.warmup_bars is supported only for Bar events"
                )
        except Exception:
            # The decorator around _qx_on_init reads this stage when it emits the
            # versioned error callback, then preserves the original exception.
            raise
        else:
            self._history_requirements_checked = True
            self._callback_error_stage = previous_stage

    @_strategy_error_boundary("start")
    def _qx_on_start(self, context_json: str) -> None:
        context = self._context_with_data(json.loads(context_json))
        self._resolve_history_requirements()
        handler = getattr(self.strategy, "on_start", None)
        if handler is not None:
            handler(context)
        if self._bar_warmup and self._warmup_bars == 0:
            self._notify_warmup_complete(context, None, 0)

    def _qx_on_stop(self) -> None:
        self._finish_stop(self._last_context or {}, "host_stop")

    def _qx_on_stop_with_context(self, stop_context_json: str) -> None:
        stop = json.loads(stop_context_json)
        if (
            not isinstance(stop, Mapping)
            or stop.get("schema_version") != 1
            or stop.get("reason")
            not in {"completed", "cancelled", "failed", "shutdown", "host_stop"}
        ):
            raise ValueError("StrategyStopContextV1 has an unsupported version or reason")
        context = stop.get("context")
        if context is None:
            context = self._last_context or {}
        elif not isinstance(context, Mapping):
            raise ValueError("StrategyStopContextV1.context must be an object or null")
        else:
            context = self._context_with_data(dict(context))
        self._finish_stop(context, stop["reason"])

    def _finish_stop(self, context: Mapping[str, Any], reason: str) -> None:
        try:
            _dispatch_on_stop(self.strategy, context, reason)
        except Exception as error:
            try:
                error_v1 = StrategyErrorV1.from_context(
                    context,
                    error,
                    stage="stop",
                )
            except (TypeError, ValueError):
                error_v1 = None
            try:
                _dispatch_on_error(self.strategy, context, error, error_v1)
            except Exception as hook_error:
                sys.stderr.write(
                    "strategy on_error hook failed: "
                    f"{type(hook_error).__name__}; original callback error retained\n"
                )
                sys.stderr.flush()
            raise
        incomplete = [
            (instrument, observed)
            for observed, instrument in self._warmup_seen.values()
            if observed < self._warmup_bars
        ]
        if incomplete:
            raise ValueError(
                "strategy run ended before Bar warmup completed: "
                + ", ".join(
                    f"instrument={instrument!r} observed={observed} required={self._warmup_bars}"
                    for instrument, observed in incomplete
                )
            )

    @_strategy_error_boundary("market_event")
    def _qx_on_event(self, context_json: str, event_json: str) -> str:
        context = json.loads(context_json)
        event = json.loads(event_json)
        if not isinstance(event, Mapping) or len(event) != 1:
            raise ValueError("MarketEvent must be a single tagged object")
        kind, payload = next(iter(event.items()))
        if isinstance(self.strategy, BaseStrategy) and kind == "Bar" and isinstance(payload, Mapping):
            self._remember_bar(payload)
        context = self._context_with_data(context)
        self._resolve_history_requirements()
        if self._warmup_bars and kind != "Bar":
            raise ValueError("history_requirements.warmup_bars is supported only for Bar events")
        observed: int | None = None
        if self._warmup_bars:
            instrument = payload.get("instrument") if isinstance(payload, Mapping) else None
            if instrument is None:
                raise ValueError("Bar warmup event must identify its instrument")
            instrument_key = json.dumps(
                instrument, separators=(",", ":"), ensure_ascii=False, sort_keys=True
            )
            observed = self._warmup_seen.get(instrument_key, (0, instrument))[0] + 1
            self._warmup_seen[instrument_key] = (observed, instrument)
            if observed == self._warmup_bars and instrument_key not in self._warmup_completed:
                self._notify_warmup_complete(context, instrument, observed)
                self._warmup_completed.add(instrument_key)
        handler = getattr(self.strategy, "on_event", self.strategy)
        decision = handler(context, event)
        if decision is None:
            fallback = getattr(self.strategy, "empty_decision", None)
            if callable(fallback):
                decision = fallback(context, kind, payload)
        if observed is not None and observed < self._warmup_bars:
            return self._decision_without_intents(decision)
        if isinstance(decision, str):
            return decision
        if not isinstance(decision, Mapping):
            raise TypeError("Python Strategy.on_event must return a decision mapping or JSON string")
        return json.dumps(dict(decision), separators=(",", ":"), ensure_ascii=False)

    def _notify_warmup_complete(
        self, context: Mapping[str, Any], instrument: Any | None, observed_bars: int
    ) -> None:
        handler = getattr(self.strategy, "on_warmup_complete", None)
        if handler is not None:
            previous_stage = getattr(self, "_callback_error_stage", None)
            self._callback_error_stage = "warmup_complete"
            try:
                handler(context, instrument, observed_bars)
            except Exception:
                raise
            else:
                self._callback_error_stage = previous_stage

    @staticmethod
    def _decision_without_intents(decision: Any) -> str:
        if isinstance(decision, str):
            try:
                value = json.loads(decision)
            except json.JSONDecodeError as error:
                raise ValueError("warmup strategy decision must be valid JSON") from error
        else:
            value = decision
        if not isinstance(value, Mapping):
            raise TypeError("Python Strategy.on_event must return a decision mapping or JSON string")
        result = dict(value)
        if not isinstance(result.get("intents"), list):
            raise TypeError("warmup strategy decision must contain an intents array")
        result["intents"] = []
        return json.dumps(result, separators=(",", ":"), ensure_ascii=False)

    @_strategy_error_boundary("order_update")
    def _qx_on_order_update(self, context_json: str, update_json: str) -> str:
        context = self._context_with_data(json.loads(context_json))
        update = json.loads(update_json)
        handler = getattr(self.strategy, "on_order_update", None)
        if handler is None:
            return "null"
        decision = handler(context, update)
        if decision is None:
            return "null"
        if isinstance(decision, str):
            return decision
        if not isinstance(decision, Mapping):
            raise TypeError(
                "Python Strategy.on_order_update must return a decision mapping, JSON string, or None"
            )
        return json.dumps(dict(decision), separators=(",", ":"), ensure_ascii=False)

    @_strategy_error_boundary("fill_update")
    def _qx_on_fill_update_v2(self, context_json: str, update_json: str) -> str:
        context = self._context_with_data(json.loads(context_json))
        update = json.loads(update_json)
        if (
            not isinstance(update, Mapping)
            or update.get("schema_version") != 2
            or update.get("event") != "fill"
            or isinstance(update.get("event_seq"), bool)
            or not isinstance(update.get("event_seq"), int)
            or update.get("event_seq") < 0
            or isinstance(update.get("ts"), bool)
            or not isinstance(update.get("ts"), int)
            or update.get("ts") <= 0
            or not isinstance(update.get("order"), Mapping)
            or not isinstance(update.get("fill"), Mapping)
        ):
            raise ValueError("StrategyFillUpdateV2 has an invalid version, identity or payload")
        handler = getattr(self.strategy, "on_fill_update", None)
        if callable(handler):
            decision = handler(context, update)
        else:
            legacy = {
                "schema_version": 1,
                "event": "fill",
                "order": update["order"],
                "fill": update["fill"],
            }
            handler = getattr(self.strategy, "on_order_update", None)
            if handler is None:
                return "null"
            decision = handler(context, legacy)
        if decision is None:
            return "null"
        if isinstance(decision, str):
            return decision
        if not isinstance(decision, Mapping):
            raise TypeError(
                "Python Strategy.on_fill_update must return a decision mapping, JSON string, or None"
            )
        return json.dumps(dict(decision), separators=(",", ":"), ensure_ascii=False)

    @_strategy_error_boundary("order_update")
    def _qx_on_order_status_update(self, context_json: str, update_json: str) -> str:
        context = self._context_with_data(json.loads(context_json))
        update = json.loads(update_json)
        handler = getattr(self.strategy, "on_order_status_update", None)
        if handler is None:
            handler = getattr(self.strategy, "on_order", None)
        if handler is None:
            return "null"
        decision = handler(context, update)
        if decision is None:
            return "null"
        if isinstance(decision, str):
            return decision
        if not isinstance(decision, Mapping):
            raise TypeError(
                "Python Strategy.on_order_status_update must return a decision mapping, JSON string, or None"
            )
        return json.dumps(dict(decision), separators=(",", ":"), ensure_ascii=False)

    def _context_with_data(self, context: dict[str, Any]) -> dict[str, Any]:
        if isinstance(self.strategy, BaseStrategy):
            as_of = context.get("as_of")
            if isinstance(as_of, bool) or not isinstance(as_of, int) or as_of <= 0:
                raise ValueError("StrategyContext.as_of must be a positive integer")
            context["data"] = StrategyDataPortal(self._bar_history, as_of)
        self._last_context = dict(context)
        return context

    def _remember_bar(self, bar: Mapping[str, Any]) -> None:
        instrument = bar.get("instrument")
        timestamp = bar.get("ts")
        if isinstance(timestamp, bool) or not isinstance(timestamp, int) or timestamp <= 0:
            raise ValueError("Bar.ts must be a positive integer for point-in-time history")
        key = BaseStrategy._instrument_key(instrument)
        history = self._bar_history.setdefault(
            key, deque(maxlen=self._MAX_HISTORY_BARS_PER_INSTRUMENT)
        )
        if history and timestamp < history[-1]["ts"]:
            raise ValueError(f"Bar history for {key} moved backwards in time")
        if history and timestamp == history[-1]["ts"]:
            if dict(bar) != dict(history[-1]):
                raise ValueError(f"Bar history for {key} has conflicting data at {timestamp}")
            return
        history.append(dict(bar))


def _strategy_slot_payload(
    strategies: Sequence[Mapping[str, Any]],
    *,
    bar_warmup: bool = True,
) -> tuple[str, list[_PythonBacktestCallback]]:
    if isinstance(strategies, (str, bytes)) or not strategies:
        raise ValueError("strategies must be a non-empty sequence of named callback slots")
    identities: list[dict[str, str]] = []
    callbacks: list[_PythonBacktestCallback] = []
    required = {"strategy_id", "strategy_version", "source_sha256", "strategy"}
    for index, binding in enumerate(strategies):
        if not isinstance(binding, Mapping) or set(binding) != required:
            raise ValueError(
                f"strategies[{index}] must contain exactly {sorted(required)}"
            )
        identities.append({
            "strategy_id": binding["strategy_id"],
            "strategy_version": binding["strategy_version"],
            "source_sha256": binding["source_sha256"],
        })
        callbacks.append(_PythonBacktestCallback(binding["strategy"], bar_warmup=bar_warmup))
    return json.dumps(identities, separators=(",", ":")), callbacks


def run_backtest_with_strategy(
    spec: "str | dict[str, Any]",
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> dict[str, Any]:
    """Run Python-authored decisions inside the same Rust Bar engine.

    ``strategy.on_event(context, market_event)`` receives JSON-shaped dictionaries. Return
    a Rust ``StrategyDecision`` mapping (``side`` uses ``Buy``/``Sell`` and quantity/price
    fields are raw fixed-point integers); risk, OMS, fills, accounting and replay stay in Rust.
    ``source_sha256`` participates in the run identity so changed strategy source cannot reuse
    an existing ``run_id``.
    """
    return json.loads(
        native.app_run_backtest_with_strategy(
            _payload(spec),
            strategy_id,
            strategy_version,
            source_sha256,
            _PythonBacktestCallback(strategy),
        )
    )


def start_backtest_with_strategy(
    spec: "str | dict[str, Any]",
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> Any:
    """Start the callback strategy on Rust RunHandle with cancel/status/result support."""
    return native.app_start_backtest_with_strategy(
        _payload(spec),
        strategy_id,
        strategy_version,
        source_sha256,
        _PythonBacktestCallback(strategy),
    )


def run_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    """Run multiple Python callbacks in declared slots of one shared Rust portfolio."""
    identities_json, callbacks = _strategy_slot_payload(strategies)
    return json.loads(
        native.app_run_backtest_with_strategy_slots(
            _payload(spec), identities_json, callbacks
        )
    )


def start_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> Any:
    """Start multiple Python callbacks in one cancellable Rust portfolio run."""
    identities_json, callbacks = _strategy_slot_payload(strategies)
    return native.app_start_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    )


def run_depth_backtest_with_strategy(
    spec: "str | dict[str, Any]",
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> dict[str, Any]:
    """Run caller-authored decisions through shared Rust Tick/OrderBook engines."""
    return json.loads(
        native.app_run_depth_backtest_with_strategy(
            _payload(spec),
            strategy_id,
            strategy_version,
            source_sha256,
            _PythonBacktestCallback(strategy, bar_warmup=False),
        )
    )


def start_depth_backtest_with_strategy(
    spec: "str | dict[str, Any]",
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> Any:
    """Start a cancellable Python strategy run on the Rust depth RunHandle."""
    return native.app_start_depth_backtest_with_strategy(
        _payload(spec),
        strategy_id,
        strategy_version,
        source_sha256,
        _PythonBacktestCallback(strategy, bar_warmup=False),
    )


def run_depth_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    """Run named Python callbacks in declared Tick/OrderBook portfolio slots."""
    identities_json, callbacks = _strategy_slot_payload(strategies, bar_warmup=False)
    return json.loads(native.app_run_depth_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    ))


def start_depth_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> Any:
    """Start a cancellable multi-callback Tick/OrderBook portfolio run."""
    identities_json, callbacks = _strategy_slot_payload(strategies, bar_warmup=False)
    return native.app_start_depth_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    )


def run_multi_leg_backtest_with_strategy(
    spec: "str | dict[str, Any]",
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> dict[str, Any]:
    """Run Python-authored decisions through shared Rust two-leg matching."""
    return json.loads(native.app_run_multi_leg_backtest_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonBacktestCallback(strategy),
    ))


def start_multi_leg_backtest_with_strategy(
    spec: "str | dict[str, Any]",
    strategy: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> Any:
    """Start a cancellable Python strategy run on the Rust two-leg lifecycle."""
    return native.app_start_multi_leg_backtest_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonBacktestCallback(strategy),
    )


def run_multi_leg_experiment_with_strategy(
    spec: "str | dict[str, Any]",
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> dict[str, Any]:
    """Run every multi-leg parameter candidate through a fresh Python strategy instance."""
    return json.loads(native.app_run_multi_leg_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonStrategyFactory(strategy_factory),
    ))


def start_multi_leg_experiment_with_strategy(
    spec: "str | dict[str, Any]",
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> Any:
    """Start a cancellable experiment; Rust requests a fresh strategy per candidate."""
    return native.app_start_multi_leg_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonStrategyFactory(strategy_factory),
    )


def run_multi_account_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run bounded independent account backtests through the shared Rust use case."""
    return json.loads(native.app_run_multi_account_backtest(_payload(spec)))


def start_multi_account_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable isolated-account batch in the Rust run lifecycle."""
    return native.app_start_multi_account_backtest(_payload(spec))


def run_multi_account_depth_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run isolated-account Tick/OrderBook backtests through the shared Rust kernel."""
    return json.loads(native.app_run_multi_account_depth_backtest(_payload(spec)))


def start_multi_account_depth_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable multi-account depth batch in the Rust run lifecycle."""
    return native.app_start_multi_account_depth_backtest(_payload(spec))


def start_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust Bar backtest and return its shared run handle."""
    return native.app_start_backtest(_payload(spec))


def verify_run(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """复核一轮产物，返回 ``VerificationResult``。

    ``outcome`` 直接取 :func:`run_backtest` 的返回值——各入口的交接面是同一份文档。
    产物缺失/不一致是**成功返回**（``verified=false`` + ``mismatches``），不是异常。
    """
    return json.loads(native.app_verify_run(_payload(outcome)))


def read_run_report_v1(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """Read the shared bounded version-1 report core for a Bar run."""
    return json.loads(native.app_read_run_report_v1(_payload(outcome)))


def read_depth_run_report_v1(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """Read the shared bounded version-1 report core for a Depth run."""
    return json.loads(native.app_read_depth_run_report_v1(_payload(outcome)))


def read_multi_account_report_v1(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """Read the shared versioned report core for a verified Bar/Depth account batch."""
    return json.loads(native.app_read_multi_account_report_v1(_payload(outcome)))


def read_multi_account_joint_report_v1(
    outcome: "str | dict[str, Any]",
    joint_account_event_pages: "dict[str, dict[str, dict[str, int]]] | None" = None,
) -> dict[str, Any]:
    """Read replay-verified joint-account facts with optional independent event pages."""
    if isinstance(outcome, str):
        try:
            decoded = json.loads(outcome)
        except json.JSONDecodeError:
            decoded = None
        if isinstance(decoded, dict) and "outcome" in decoded:
            outcome = decoded
    if isinstance(outcome, dict) and "outcome" in outcome:
        unknown = set(outcome) - {"outcome", "joint_account_event_pages"}
        if unknown or joint_account_event_pages is not None:
            raise ValueError("joint report wrapper contains unknown fields or duplicate page options")
        joint_account_event_pages = outcome.get("joint_account_event_pages")
        outcome = outcome["outcome"]
    options = (
        None
        if joint_account_event_pages is None
        else _payload({"joint_account_event_pages": joint_account_event_pages})
    )
    return json.loads(
        native.app_read_multi_account_joint_report_v1(_payload(outcome), options)
    )


def read_multi_leg_report_facts_v1(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """Read replay-verified accounting and per-leg metrics for a persisted multi-leg run."""
    return json.loads(native.app_read_multi_leg_report_facts_v1(_payload(outcome)))


def read_multi_leg_research_report_v1(
    request: "str | dict[str, Any]",
) -> dict[str, Any]:
    """Read the complete bounded multi-leg report through the shared qx-app reader."""
    return json.loads(native.app_read_multi_leg_research_report_v1(_payload(request)))


def read_multi_leg_event_report_page_v1(
    request: "str | dict[str, Any]",
) -> dict[str, Any]:
    """Replay a bounded multi-leg EventLog page through the shared qx-app reader."""
    return json.loads(native.app_read_multi_leg_event_report_page_v1(_payload(request)))


def read_multi_market_event_report_page_v1(
    request: "str | dict[str, Any]",
) -> dict[str, Any]:
    """Replay a bounded page from a multi-market EventLog through the shared qx-app reader."""
    return json.loads(native.app_read_multi_market_event_report_page_v1(_payload(request)))


def read_multi_market_depth_event_report_page_v1(
    request: "str | dict[str, Any]",
) -> dict[str, Any]:
    """Replay a bounded page from a multi-market Depth EventLog through qx-app."""
    return json.loads(native.app_read_multi_market_depth_event_report_page_v1(_payload(request)))


def read_multi_market_research_report_v1(
    request: "str | dict[str, Any]",
) -> dict[str, Any]:
    """Read a complete verified bounded shared-account multi-market Bar report."""
    return json.loads(native.app_read_multi_market_research_report_v1(_payload(request)))


def read_multi_market_depth_research_report_v1(
    request: "str | dict[str, Any]",
) -> dict[str, Any]:
    """Read a complete verified bounded shared-account multi-market L1/L2 report."""
    return json.loads(native.app_read_multi_market_depth_research_report_v1(_payload(request)))


def compare_runs(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """按固定收益/回撤/run_id 顺序比较相同标的和数据指纹的已完成运行。"""
    return json.loads(native.app_compare_runs(_payload(spec)))


def plan_risk_parity(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Create a Rust-owned deterministic long-only risk parity rebalance plan."""
    return json.loads(native.app_plan_risk_parity(_payload(spec)))


def plan_capital_allocation(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Allocate portfolio cash deterministically in the shared Rust domain."""
    return json.loads(native.app_plan_capital_allocation(_payload(spec)))


def plan_algo_execution(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Create a Rust-owned deterministic TWAP/VWAP child-order schedule."""
    return json.loads(native.app_plan_algo_execution(_payload(spec)))


def run_experiment(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """用 Rust 回测用例执行有界参数网格，并复用 Rust 结果比较用例。"""
    return json.loads(native.app_run_experiment(_payload(spec)))


def prepare_quick_research_spec(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Validate and canonicalize the guided OOS research preset in qx-app."""
    return json.loads(native.app_prepare_quick_research_spec_v1(_payload(spec)))


def run_experiment_with_strategy(
    spec: "str | dict[str, Any]",
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> dict[str, Any]:
    """用 Rust 参数实验执行器为每个候选创建一个新的 Python 策略实例。"""
    return json.loads(native.app_run_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonStrategyFactory(strategy_factory),
    ))


def start_experiment(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust parameter experiment and return its run handle."""
    return native.app_start_experiment(_payload(spec))


def start_experiment_with_strategy(
    spec: "str | dict[str, Any]",
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> Any:
    """启动可取消的共享 Rust 参数实验，每个候选都新建 Python 策略实例。"""
    return native.app_start_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonStrategyFactory(strategy_factory),
    )


def run_multi_leg_experiment(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """用多腿 Rust 回测用例执行有界参数网格。"""
    return json.loads(native.app_run_multi_leg_experiment(_payload(spec)))


def start_multi_leg_experiment(spec: "str | dict[str, Any]") -> Any:
    """启动可取消的 Rust 多腿参数实验并返回共享 RunHandle。"""
    return native.app_start_multi_leg_experiment(_payload(spec))


def run_depth_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run an L1 Tick or L2 order-book backtest through the Rust application layer."""
    return json.loads(native.app_run_depth_backtest(_payload(spec)))


def start_depth_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust L1 Tick or L2 order-book backtest."""
    return native.app_start_depth_backtest(_payload(spec))


def run_depth_experiment(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run Tick/OrderBook parameter candidates through the Rust backtest use case."""
    return json.loads(native.app_run_depth_experiment(_payload(spec)))


def start_depth_experiment(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust Tick/OrderBook parameter grid."""
    return native.app_start_depth_experiment(_payload(spec))


def run_depth_experiment_with_strategy(
    spec: "str | dict[str, Any]",
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> dict[str, Any]:
    """Run every Tick/OrderBook candidate through a fresh caller-authored Python strategy."""
    return json.loads(native.app_run_depth_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonStrategyFactory(strategy_factory, bar_warmup=False),
    ))


def start_depth_experiment_with_strategy(
    spec: "str | dict[str, Any]",
    strategy_factory: Any,
    *,
    strategy_id: str,
    strategy_version: str,
    source_sha256: str,
) -> Any:
    """Start a cancellable depth grid with one fresh Python callback per candidate."""
    return native.app_start_depth_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256,
        _PythonStrategyFactory(strategy_factory, bar_warmup=False),
    )


def run_multi_leg_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run a Rust application-layer multi-leg Bar backtest."""
    return json.loads(native.app_run_multi_leg_backtest(_payload(spec)))


def start_multi_leg_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust multi-leg run and return its shared RunHandle."""
    return native.app_start_multi_leg_backtest(_payload(spec))


def run_multi_market_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run a shared-account N-market Bar portfolio in the Rust application layer."""
    return json.loads(native.app_run_multi_market_backtest(_payload(spec)))


def run_multi_account_joint_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    return json.loads(native.app_run_multi_account_joint_backtest(_payload(spec)))


def start_multi_account_joint_backtest(spec: "str | dict[str, Any]") -> Any:
    return native.app_start_multi_account_joint_backtest(_payload(spec))


def start_multi_market_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust N-market run."""
    return native.app_start_multi_market_backtest(_payload(spec))


def run_multi_market_depth_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run same-account multi-market L2 execution in the shared Rust kernel."""
    return json.loads(native.app_run_multi_market_depth_backtest(_payload(spec)))


def start_multi_market_depth_backtest(spec: "str | dict[str, Any]") -> Any:
    return native.app_start_multi_market_depth_backtest(_payload(spec))


def run_multi_market_depth_experiment_with_strategy(
    spec: "str | dict[str, Any]", *, strategy_id: str, strategy_version: str,
    source_sha256: str, factory: Any,
) -> dict[str, Any]:
    adapted_factory = _PythonStrategyFactory(factory, bar_warmup=False)
    return json.loads(native.app_run_multi_market_depth_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256, adapted_factory,
    ))


def start_multi_market_depth_experiment_with_strategy(
    spec: "str | dict[str, Any]", *, strategy_id: str, strategy_version: str,
    source_sha256: str, factory: Any,
) -> Any:
    adapted_factory = _PythonStrategyFactory(factory, bar_warmup=False)
    return native.app_start_multi_market_depth_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256, adapted_factory,
    )


def run_multi_market_depth_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    identities_json, callbacks = _strategy_slot_payload(strategies, bar_warmup=False)
    return json.loads(native.app_run_multi_market_depth_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    ))


def start_multi_market_depth_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> Any:
    identities_json, callbacks = _strategy_slot_payload(strategies, bar_warmup=False)
    return native.app_start_multi_market_depth_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    )


def run_multi_market_experiment(spec: "str | dict[str, Any]") -> dict[str, Any]:
    return json.loads(native.app_run_multi_market_experiment(_payload(spec)))


def start_multi_market_experiment(spec: "str | dict[str, Any]") -> Any:
    return native.app_start_multi_market_experiment(_payload(spec))


def run_multi_market_experiment_with_strategy(
    spec: "str | dict[str, Any]", *, strategy_id: str, strategy_version: str,
    source_sha256: str, factory: Any,
) -> dict[str, Any]:
    adapted_factory = _PythonStrategyFactory(factory)
    return json.loads(native.app_run_multi_market_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256, adapted_factory,
    ))


def start_multi_market_experiment_with_strategy(
    spec: "str | dict[str, Any]", *, strategy_id: str, strategy_version: str,
    source_sha256: str, factory: Any,
) -> Any:
    adapted_factory = _PythonStrategyFactory(factory)
    return native.app_start_multi_market_experiment_with_strategy(
        _payload(spec), strategy_id, strategy_version, source_sha256, adapted_factory,
    )


def run_multi_market_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    """Run identified Python callbacks in declared market slots on the shared Rust engine."""
    identities_json, callbacks = _strategy_slot_payload(strategies)
    return json.loads(native.app_run_multi_market_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    ))


def start_multi_market_backtest_with_strategy_slots(
    spec: "str | dict[str, Any]", strategies: Sequence[Mapping[str, Any]]
) -> Any:
    """Start a cancellable N-market run with Python callbacks on the Rust RunHandle."""
    identities_json, callbacks = _strategy_slot_payload(strategies)
    return native.app_start_multi_market_backtest_with_strategy_slots(
        _payload(spec), identities_json, callbacks
    )


def verify_depth_run(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """Verify the manifest, summary, equity, and fills of a depth run."""
    return json.loads(native.app_verify_depth_run(_payload(outcome)))


def app_error_payload(error: BaseException) -> "dict[str, Any] | None":
    """把 ``QxAppError`` 的异常文本解析回 ``AppError`` 文档；不是它则返回 ``None``。

    这是给调用方（和跨入口等价性用例）准备的唯一取文档入口：异常文本就是那份 JSON，
    别在别处再拼一次键名清单。
    """
    text = str(error)
    try:
        payload = json.loads(text)
    except ValueError:
        return None
    return payload if isinstance(payload, dict) and "category" in payload else None
