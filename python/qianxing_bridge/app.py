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


def verify_run(outcome: "str | dict[str, Any]") -> dict[str, Any]:
    """复核一轮产物，返回 ``VerificationResult``。

    ``outcome`` 直接取 :func:`run_backtest` 的返回值——各入口的交接面是同一份文档。
    产物缺失/不一致是**成功返回**（``verified=false`` + ``mismatches``），不是异常。
    """
    return json.loads(native.app_verify_run(_payload(outcome)))


def compare_runs(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """按固定收益/回撤/run_id 顺序比较相同标的和数据指纹的已完成运行。"""
    return json.loads(native.app_compare_runs(_payload(spec)))


def run_experiment(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """用 Rust 回测用例执行有界参数网格，并复用 Rust 结果比较用例。"""
    return json.loads(native.app_run_experiment(_payload(spec)))


def run_depth_backtest(spec: "str | dict[str, Any]") -> dict[str, Any]:
    """Run an L1 Tick or L2 order-book backtest through the Rust application layer."""
    return json.loads(native.app_run_depth_backtest(_payload(spec)))


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
