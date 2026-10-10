"""牵星 Python SDK。

The SDK is a typed facade over the Rust application layer. It deliberately
does not implement matching, risk, portfolio accounting, or performance
metrics in Python. Install the ``qianxing`` wheel to use this namespace.
"""

from importlib.metadata import PackageNotFoundError, version

from .app import (
    AppError,
    AppTimeoutError,
    BacktestOutcome,
    BacktestSpec,
    BuiltinStrategySpec,
    CompareRunsResult,
    CompareRunsSpec,
    ComparedRun,
    ComparedRunResult,
    DatasetSpec,
    DatasetVerdict,
    ExperimentCandidateResult,
    ExperimentParameterSpace,
    ConflictError,
    DataUnavailableError,
    FidelityInsufficientError,
    InternalInvariantError,
    InvalidInputError,
    PermissionDeniedError,
    QianxingError,
    VerificationResult,
    RunExperimentResult,
    RunExperimentSpec,
    StorageFailureError,
    doctor,
    compare_runs,
    run_experiment,
    run_backtest,
    validate_dataset,
    verify_run,
)

__all__ = [
    "AppError",
    "AppTimeoutError",
    "BacktestOutcome",
    "BacktestSpec",
    "BuiltinStrategySpec",
    "CompareRunsResult",
    "CompareRunsSpec",
    "ComparedRun",
    "ComparedRunResult",
    "DatasetSpec",
    "DatasetVerdict",
    "ExperimentCandidateResult",
    "ExperimentParameterSpace",
    "ConflictError",
    "DataUnavailableError",
    "FidelityInsufficientError",
    "InternalInvariantError",
    "InvalidInputError",
    "PermissionDeniedError",
    "QianxingError",
    "VerificationResult",
    "RunExperimentResult",
    "RunExperimentSpec",
    "StorageFailureError",
    "doctor",
    "compare_runs",
    "run_experiment",
    "run_backtest",
    "validate_dataset",
    "verify_run",
]

try:
    __version__ = version("qianxing")
except PackageNotFoundError:
    __version__ = "0.1.0"
