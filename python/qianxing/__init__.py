"""牵星 Python SDK。

The SDK is a typed facade over the Rust application layer. It deliberately
does not implement matching, risk, portfolio accounting, or performance
metrics in Python. Install the ``qianxing`` wheel to use this namespace.

分层（方案 §6.2「接口分层」）——每一层只回答一个问题：

===================  ==============================================  ==================
子层                 放什么                                           要不要服务
===================  ==============================================  ==================
``qianxing.core``    本地用例：校验、Bar/Tick/多腿回测、协作式句柄      不要
``qianxing.client``  远端 API：提交/状态/取消/等待/取结果                要一台 qx-api
``qianxing.data``    数据集与合约身份（provider-neutral）                不要
``qianxing.strategy``策略 contract 与 worker 的输入输出类型              不要
``qianxing.paper``   Paper 轨：服务端执行、仓库自带撮合、不需凭据        要一台 qx-api
``qianxing.live``    实盘轨：服务端执行、需要已认证 API policy           要一台 qx-api
``qianxing.experiments`` 参数网格与候选排名                              不要
``qianxing.reports`` 产物复核与跨 Run 对比                              不要
===================  ==============================================  ==================

这一层（顶层命名空间）稳定导出常用接口，与 ``qianxing.app`` 的公开面**逐一相等**——
不多导出、也不少导出。子层是它的**分层视图**：每一层的名字都取自这份名单，
子层不另造私有名字。

``qianxing.ccxt`` / ``qianxing.ashare`` 是可选适配器的入口，装对应的 extra 才可用；
``import qianxing`` 与 ``import qianxing.core`` 在任何 extras 都没装的环境里都成立
（方案 §6.2：「optional dependencies 不改变核心语义」）。
"""

from importlib.metadata import PackageNotFoundError, version

from .app import (
    AppError,
    AppTimeoutError,
    AppClient,
    AsyncCoreClient,
    AsyncRunHandle,
    BacktestOutcome,
    BacktestSpec,
    BuiltinStrategySpec,
    CompareRunsResult,
    CompareRunsSpec,
    ComparedRun,
    ComparedRunResult,
    DatasetSpec,
    DatasetVerdict,
    DepthBacktestOutcome,
    DepthBacktestSpec,
    MarketMakingSpec,
    DepthExperimentCandidateResult,
    DepthExperimentRankedCandidate,
    ExperimentBaselineReport,
    MultiLegBacktestSpec,
    MultiLegBacktestOutcome,
    MultiLegExperimentCandidateResult,
    MultiLegExperimentRankedCandidate,
    OrderPolicy,
    ExperimentCandidateResult,
    ExperimentFoldReport,
    ExperimentOutOfSampleReport,
    ExperimentParameterSpace,
    ExperimentSplitPlan,
    ConflictError,
    DataUnavailableError,
    FidelityInsufficientError,
    InternalInvariantError,
    InstrumentId,
    InvalidInputError,
    MakerTakerFeeSchedule,
    PermissionDeniedError,
    ProtectiveExitSpec,
    QianxingError,
    VerificationResult,
    RunExperimentResult,
    RunExperimentSpec,
    RunDepthExperimentResult,
    RunDepthExperimentSpec,
    RunMultiLegExperimentResult,
    RunMultiLegExperimentSpec,
    RiskParityAssetSpec,
    RiskParitySpec,
    RiskParityResult,
    RunHandle,
    RemoteRunHandle,
    TradeCommand,
    TradeWorkflowClient,
    PaperClient,
    LiveClient,
    StorageFailureError,
    TradingInstrumentSpec,
    doctor,
    compare_runs,
    plan_algo_execution,
    plan_risk_parity,
    run_experiment,
    run_depth_experiment,
    run_multi_leg_experiment,
    run_backtest,
    start_backtest,
    start_experiment,
    start_multi_leg_experiment,
    start_depth_backtest,
    start_depth_experiment,
    start_multi_leg_backtest,
    run_depth_backtest,
    run_multi_leg_backtest,
    validate_dataset,
    verify_run,
    verify_depth_run,
)

__all__ = [
    "AppError",
    "AppTimeoutError",
    "AppClient",
    "AsyncCoreClient",
    "AsyncRunHandle",
    "BacktestOutcome",
    "BacktestSpec",
    "BuiltinStrategySpec",
    "CompareRunsResult",
    "CompareRunsSpec",
    "ComparedRun",
    "ComparedRunResult",
    "DatasetSpec",
    "DatasetVerdict",
    "DepthBacktestOutcome",
    "DepthBacktestSpec",
    "MarketMakingSpec",
    "DepthExperimentCandidateResult",
    "DepthExperimentRankedCandidate",
    "ExperimentBaselineReport",
    "MultiLegBacktestSpec",
    "MultiLegBacktestOutcome",
    "MultiLegExperimentCandidateResult",
    "MultiLegExperimentRankedCandidate",
    "OrderPolicy",
    "ExperimentCandidateResult",
    "ExperimentFoldReport",
    "ExperimentOutOfSampleReport",
    "ExperimentParameterSpace",
    "ExperimentSplitPlan",
    "ConflictError",
    "DataUnavailableError",
    "FidelityInsufficientError",
    "InternalInvariantError",
    "InstrumentId",
    "InvalidInputError",
    "MakerTakerFeeSchedule",
    "PermissionDeniedError",
    "ProtectiveExitSpec",
    "QianxingError",
    "VerificationResult",
    "RunExperimentResult",
    "RunExperimentSpec",
    "RunDepthExperimentResult",
    "RunDepthExperimentSpec",
    "RunMultiLegExperimentResult",
    "RunMultiLegExperimentSpec",
    "RiskParityAssetSpec",
    "RiskParitySpec",
    "RiskParityResult",
    "RunHandle",
    "RemoteRunHandle",
    "TradeCommand",
    "TradeWorkflowClient",
    "PaperClient",
    "LiveClient",
    "StorageFailureError",
    "TradingInstrumentSpec",
    "doctor",
    "compare_runs",
    "plan_algo_execution",
    "plan_risk_parity",
    "run_experiment",
    "run_depth_experiment",
    "run_multi_leg_experiment",
    "run_backtest",
    "start_backtest",
    "start_experiment",
    "start_multi_leg_experiment",
    "start_depth_backtest",
    "start_depth_experiment",
    "start_multi_leg_backtest",
    "run_depth_backtest",
    "run_multi_leg_backtest",
    "validate_dataset",
    "verify_run",
    "verify_depth_run",
]

try:
    __version__ = version("qianxing")
except PackageNotFoundError:
    __version__ = "0.1.0"
