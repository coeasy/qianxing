from collections.abc import Callable
from typing import Any, cast

from qianxing.app import (
    InvalidInputError,
    RunExperimentResult,
    RunExperimentSpec,
    RunHandle,
    RunMultiMarketExperimentResult,
    RunMultiMarketExperimentSpec,
    _prepare_quick_research_spec,
    run_experiment,
    run_experiment_with_strategy,
    run_multi_market_experiment,
    run_multi_market_experiment_with_strategy,
    start_experiment,
    start_experiment_with_strategy,
    start_multi_market_experiment,
    start_multi_market_experiment_with_strategy,
)

GuidedResearchResult = RunExperimentResult | RunMultiMarketExperimentResult
GuidedResearchHandle = RunHandle[RunExperimentResult] | RunHandle[RunMultiMarketExperimentResult]
GuidedResearchSpec = RunExperimentSpec | RunMultiMarketExperimentSpec | dict[str, Any]


def _research_dispatch(
    spec: GuidedResearchSpec,
    *,
    strategy_factory: Callable[[dict[str, Any]], Any] | None,
    strategy_id: str | None,
    strategy_version: str | None,
    source_sha256: str | None,
    start: bool,
) -> GuidedResearchResult | GuidedResearchHandle:
    """Normalize the guided-research contract in Rust, then select its owning runner."""
    normalized = _prepare_quick_research_spec(spec)
    base = normalized.get("base")
    multi_market = isinstance(base, dict) and isinstance(base.get("markets"), list)

    metadata = (strategy_id, strategy_version, source_sha256)
    if strategy_factory is None:
        if any(value is not None for value in metadata):
            raise InvalidInputError(
                "strategy_id, strategy_version, and source_sha256 require strategy_factory"
            )
    elif (
        not isinstance(strategy_id, str)
        or not strategy_id.strip()
        or not isinstance(strategy_version, str)
        or not strategy_version.strip()
        or not isinstance(source_sha256, str)
        or not source_sha256.strip()
    ):
        raise InvalidInputError(
            "Python strategy research requires non-empty strategy_id, "
            "strategy_version, and source_sha256"
        )
    elif len(source_sha256) != 64 or any(
        char not in "0123456789abcdefABCDEF" for char in source_sha256
    ):
        raise InvalidInputError("source_sha256 must be a 64-character hexadecimal SHA-256 digest")

    if strategy_factory is not None:
        strategy_id = cast(str, strategy_id)
        strategy_version = cast(str, strategy_version)
        source_sha256 = cast(str, source_sha256)

    if multi_market:
        if start:
            if strategy_factory is None:
                return start_multi_market_experiment(normalized)
            return start_multi_market_experiment_with_strategy(
                normalized,
                strategy_factory,
                strategy_id=strategy_id,
                strategy_version=strategy_version,
                source_sha256=source_sha256,
            )
        if strategy_factory is None:
            return run_multi_market_experiment(normalized)
        return run_multi_market_experiment_with_strategy(
            normalized,
            strategy_factory,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )

    if start:
        if strategy_factory is None:
            return start_experiment(normalized)
        return start_experiment_with_strategy(
            normalized,
            strategy_factory,
            strategy_id=strategy_id,
            strategy_version=strategy_version,
            source_sha256=source_sha256,
        )
    if strategy_factory is None:
        return run_experiment(normalized)
    return run_experiment_with_strategy(
        normalized,
        strategy_factory,
        strategy_id=strategy_id,
        strategy_version=strategy_version,
        source_sha256=source_sha256,
    )


def research(
    spec: GuidedResearchSpec,
    *,
    strategy_factory: Callable[[dict[str, Any]], Any] | None = None,
    strategy_id: str | None = None,
    strategy_version: str | None = None,
    source_sha256: str | None = None,
) -> GuidedResearchResult:
    """Run a guided, held-out research experiment through the shared Rust application.

    ``spec`` must be a single-market or multi-market experiment document with an explicit
    training/held-out ``split_plan``. Rust validates and normalizes it before execution; for
    single-market experiments Rust also supplies the default ``buy-and-hold`` baseline.
    An optional ``strategy_factory(candidate_spec)`` creates a fresh local Python strategy for
    each candidate and must be accompanied by a stable strategy ID, version, and source digest.

    This function returns the existing typed experiment result. It does not claim that
    multi-market results have a cross-market effectiveness benchmark, and it does not replace
    point-in-time leakage/warmup diagnostics on a strategy module or the CLI's HTML report.
    """
    return _research_dispatch(
        spec,
        strategy_factory=strategy_factory,
        strategy_id=strategy_id,
        strategy_version=strategy_version,
        source_sha256=source_sha256,
        start=False,
    )


def start_research(
    spec: GuidedResearchSpec,
    *,
    strategy_factory: Callable[[dict[str, Any]], Any] | None = None,
    strategy_id: str | None = None,
    strategy_version: str | None = None,
    source_sha256: str | None = None,
) -> GuidedResearchHandle:
    """Start the same Rust-validated guided research flow with a cancellable ``RunHandle``."""
    return _research_dispatch(
        spec,
        strategy_factory=strategy_factory,
        strategy_id=strategy_id,
        strategy_version=strategy_version,
        source_sha256=source_sha256,
        start=True,
    )
