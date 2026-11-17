"""Diagnostic example that filters the full input at the request's as_of boundary."""

ONE_UNIT = 1_000_000_000


def on_event(request):
    bars = request.bars
    visible = []
    if bars is not None:
        visible = [
            close for ts, close in zip(bars.ts, bars.close_raw) if ts < request.as_of
        ]
    target = ONE_UNIT if len(visible) >= 2 and visible[-1] >= visible[-2] else 0
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
