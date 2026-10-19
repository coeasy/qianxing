import time

from qianxing_strategy import StrategyOutput


def on_event(request):
    time.sleep(2.0)
    return StrategyOutput(
        request_id=request.request_id,
        strategy_id=request.strategy_id,
        signal_id=9,
        instrument=request.instrument,
        target_qty=0,
        confidence=0,
        priority=0,
        expires_at=request.as_of,
    )
