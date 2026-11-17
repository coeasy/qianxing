"""Diagnostic fixture: leaked indicator state is visible although the decision is flat."""


def on_event(request):
    return {
        "schema_version": request.schema_version,
        "request_id": request.request_id,
        "strategy_id": request.strategy_id,
        "signal_id": request.as_of,
        "instrument": request.instrument,
        "target_qty": 0,
        "confidence": 0,
        "priority": 0,
        "expires_at": request.as_of,
        "intents": [],
    }


def diagnostic_state(request):
    """Deliberately reads the final input close without applying the as_of cutoff."""
    closes = request.bars.close_raw if request.bars is not None else ()
    return {"indicator.last_close_raw": closes[-1] if closes else None}
