"""Minimal cross-language strategy used by ``strategy-backtest`` smoke tests."""


def on_event(request):
    bars = request.bars
    if bars is None or len(bars.close_raw) < 2:
        target = 0
    else:
        target = 1 if bars.close_raw[-1] >= bars.close_raw[-2] else 0
    return {
        "schema_version": request.schema_version,
        "request_id": request.request_id,
        "strategy_id": request.strategy_id,
        "signal_id": request.as_of,
        "instrument": request.instrument,
        "target_qty": target,
        "confidence": 1,
        "priority": 0,
        "expires_at": request.as_of,
        "intents": [],
    }
