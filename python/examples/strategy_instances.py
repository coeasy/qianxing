"""Small, deterministic Python strategies for the versioned strategy worker API."""

ONE_UNIT = 1_000_000_000  # fixed-point quantity scale (1e9)


def _decision(request, target_qty):
    return {
        "schema_version": request.schema_version,
        "request_id": request.request_id,
        "strategy_id": request.strategy_id,
        "signal_id": request.as_of,
        "instrument": request.instrument,
        "target_qty": target_qty,
        "confidence": 1,
        "priority": 0,
        "expires_at": request.as_of,
        "intents": [],
    }


def momentum(request):
    """Hold one unit after an up close; otherwise flatten."""
    bars = request.bars
    target = (
        ONE_UNIT
        if bars is not None
        and len(bars.close_raw) >= 2
        and bars.close_raw[-1] > bars.close_raw[-2]
        else 0
    )
    return _decision(request, target)


def sma_cross(request, fast=3, slow=5):
    """Long on a fast/slow simple moving-average cross, flat otherwise."""
    if fast <= 0 or slow <= fast:
        raise ValueError("require 0 < fast < slow")
    closes = request.bars.close_raw if request.bars is not None else ()
    target = 0
    if len(closes) >= slow:
        fast_mean = sum(closes[-fast:]) // fast
        slow_mean = sum(closes[-slow:]) // slow
        target = ONE_UNIT if fast_mean > slow_mean else 0
    return _decision(request, target)


def mean_reversion(request, lookback=5, deviation_bps=100):
    """Buy below and flatten above a fixed-point rolling mean threshold."""
    if lookback <= 0 or deviation_bps < 0:
        raise ValueError("lookback must be positive and deviation_bps non-negative")
    closes = request.bars.close_raw if request.bars is not None else ()
    target = 0
    if len(closes) >= lookback:
        window = closes[-lookback:]
        mean = sum(window) // lookback
        close = window[-1]
        if mean > 0 and close * 10_000 <= mean * (10_000 - deviation_bps):
            target = ONE_UNIT
    return _decision(request, target)
