"""Python Strategy API example for a small-coin, scale-aware limit order."""

from qianxing_strategy import StrategyIntent, StrategyOutput


def on_event(request):
    """Emit one exact limit intent; runtime rules and account risk remain authoritative."""
    return StrategyOutput(
        request_id=request.request_id,
        strategy_id=request.strategy_id,
        signal_id=max(1, request.as_of),
        instrument=request.instrument,
        target_qty=0,
        intents=(
            StrategyIntent(
                intent_id=1,
                instrument=request.instrument,
                side="buy",
                qty_raw=0,
                qty_exact="0.0000000001",
                limit_price_exact="0.00000005",
            ),
        ),
        expires_at=request.as_of,
    )
