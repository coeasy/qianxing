"""应用层用例的 Python 门面（T2-2 / 退出门 G1）。

应用函数与 ``qx-cli app`` 子命令、``POST /app/*`` 路由调的是**同一组 ``qx-app`` 用例**：
进出都是 JSON 文档，所以「同一 use case 三入口结果哈希相同、错误 code 与 correlation id 相同」
在这条链上是构造性的，不是靠约定维持的。

失败一律抛 ``QxAppError``，异常文本就是 ``AppError`` 的 JSON 文档::

    {"action": ..., "category": ..., "correlation_id": ..., "message": ...,
     "retry": ..., "safe_to_retry": ..., "source_code": ...}

调用方按 ``category`` 分支（``InvalidInput`` 改输入、``DataUnavailable`` 去取数据、
``Conflict`` 先解决冲突……），**不要**按 ``message`` 分支——那是中文展示层。

扩展未构建（没跑过 ``cargo build -p qx-python`` 或 ``tools/build_python_wheel.*``）时这里会如实
抛 ``RuntimeError``，不做静默回退：本模块没有第二份实现，"回退"就等于"Python 侧自己再算一遍"，
而那正是 G1 要消灭的形状。
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import Any

from . import native


def _payload(value: "str | dict[str, Any]") -> str:
    """接受现成的 JSON 字符串，也接受 dict——两者最终交给 Rust 的是同一份字节。"""
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)


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

    def __init__(self, strategy: Any) -> None:
        self.strategy = strategy

    def _qx_on_event(self, context_json: str, event_json: str) -> str:
        context = json.loads(context_json)
        event = json.loads(event_json)
        handler = getattr(self.strategy, "on_event", self.strategy)
        decision = handler(context, event)
        if isinstance(decision, str):
            return decision
        if not isinstance(decision, Mapping):
            raise TypeError("Python Strategy.on_event must return a decision mapping or JSON string")
        return json.dumps(dict(decision), separators=(",", ":"), ensure_ascii=False)


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
            _PythonBacktestCallback(strategy),
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
        _PythonBacktestCallback(strategy),
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


def run_multi_account_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run bounded independent account backtests through the shared Rust use case."""
    return json.loads(native.app_run_multi_account_backtest(_payload(spec)))


def start_multi_account_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable isolated-account batch in the Rust run lifecycle."""
    return native.app_start_multi_account_backtest(_payload(spec))


def start_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust Bar backtest and return its shared run handle."""
    return native.app_start_backtest(_payload(spec))


def verify_run(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """复核一轮产物，返回 ``VerificationResult``。

    ``outcome`` 直接取 :func:`run_backtest` 的返回值——各入口的交接面是同一份文档。
    产物缺失/不一致是**成功返回**（``verified=false`` + ``mismatches``），不是异常。
    """
    return json.loads(native.app_verify_run(_payload(outcome)))


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


def start_experiment(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust parameter experiment and return its run handle."""
    return native.app_start_experiment(_payload(spec))


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


def run_multi_leg_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run a Rust application-layer multi-leg Bar backtest."""
    return json.loads(native.app_run_multi_leg_backtest(_payload(spec)))


def start_multi_leg_backtest(spec: "str | dict[str, Any]") -> Any:
    """Start a cancellable Rust multi-leg run and return its shared RunHandle."""
    return native.app_start_multi_leg_backtest(_payload(spec))


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
