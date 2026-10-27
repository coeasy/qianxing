"""Python Strategy API example for a Bitget Spot quote-currency market buy."""

from qianxing_strategy import StrategyIntent, StrategyOutput


def on_event(request):
    """Spend at most 12.34 USDT; the runtime still applies account risk limits."""
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
                quote_budget_exact="12.34",
            ),
        ),
        expires_at=request.as_of,
    )
