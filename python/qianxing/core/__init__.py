"""本地用例层：在**本进程**里跑 Rust 内核的用例（方案 §6.2 接口分层）。

这一层是 `qianxing` 的地基：数据校验、Bar / Tick+OrderBook / 多腿回测、参数网格实验，
以及它们的协作式句柄 [`RunHandle`][qianxing.core.RunHandle]。调用不经任何服务——
没有 HTTP、没有服务端会话、没有凭据。

**这一层刻意不导出**远端面（`AppClient` / `RemoteRunHandle`）与服务端执行面
（`PaperClient` / `LiveClient` / `TradeWorkflowClient`）：想在进程内跑用例的人不该
被要求先起一个服务，想连服务的人该去 `qianxing.client`。分层不是装饰——它让
"我到底需不需要一个服务"这个问题在 import 那一行就有答案。

跑一次完整回测只需要这一层：

    from qianxing.core import BacktestSpec, BuiltinStrategySpec, run_backtest

    outcome = run_backtest(spec)

各子层的分工见 `qianxing` 包文档。

异步本地调用使用 [`AsyncCoreClient`][qianxing.core.AsyncCoreClient]，返回的
[`AsyncRunHandle`][qianxing.core.AsyncRunHandle] 仍管理同一个 Rust Run；asyncio 取消
会请求 Rust 协作取消，不会在 Python 层另建撮合或回测实现。
"""

from qianxing.app import (
    AsyncCoreClient,
    AsyncRunHandle,
    BacktestOutcome,
    BacktestSpec,
    BuiltinStrategySpec,
    BuiltinStrategyInfo,
    DatasetSpec,
    DatasetVerdict,
    DepthBacktestOutcome,
    DepthBacktestSpec,
    MarketMakingSpec,
    MakerTakerFeeSchedule,
    MultiLegBacktestOutcome,
    MultiLegBacktestSpec,
    PortfolioBacktestConfig,
    PortfolioStrategySpec,
    OrderPolicy,
    ProtectiveExitSpec,
    plan_capital_allocation,
    plan_algo_execution,
    RunHandle,
    RunEvent,
    run_backtest,
    list_builtin_strategies,
    run_depth_backtest,
    run_multi_leg_backtest,
    start_backtest,
    start_depth_backtest,
    start_multi_leg_backtest,
    validate_dataset,
)

__all__ = [
    "AsyncCoreClient",
    "AsyncRunHandle",
    "BacktestOutcome",
    "BacktestSpec",
    "BuiltinStrategySpec",
    "BuiltinStrategyInfo",
    "DatasetSpec",
    "DatasetVerdict",
    "DepthBacktestOutcome",
    "DepthBacktestSpec",
    "MarketMakingSpec",
    "MakerTakerFeeSchedule",
    "MultiLegBacktestOutcome",
    "MultiLegBacktestSpec",
    "PortfolioBacktestConfig",
    "PortfolioStrategySpec",
    "OrderPolicy",
    "ProtectiveExitSpec",
    "plan_capital_allocation",
    "plan_algo_execution",
    "RunHandle",
    "RunEvent",
    "run_backtest",
    "list_builtin_strategies",
    "run_depth_backtest",
    "run_multi_leg_backtest",
    "start_backtest",
    "start_depth_backtest",
    "start_multi_leg_backtest",
    "validate_dataset",
]
