from qianxing_strategy import StrategyOutput


def on_event(request):
    target = request.research_targets.get(request.instrument, 0)
    return StrategyOutput(
        request_id=request.request_id,
        strategy_id=request.strategy_id,
        signal_id=7,
        instrument=request.instrument,
        target_qty=target,
        confidence=800,
        priority=2,
        expires_at=request.as_of,
    )
