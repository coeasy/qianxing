from qianxing_strategy import StrategyIntent, StrategyOutput


def on_event(request):
    return StrategyOutput(
        request_id=request.request_id,
        strategy_id=request.strategy_id,
        signal_id=8,
        instrument=request.instrument,
        target_qty=0,
        confidence=900,
        priority=3,
        expires_at=request.as_of,
        intents=(
            StrategyIntent(
                intent_id=801,
                instrument=request.instrument,
                side="buy",
                qty_raw=2,
                limit_price_raw=100,
                post_only=True,
            ),
            StrategyIntent(
                intent_id=802,
                instrument="ETHUSDT.BINANCE",
                side="sell",
                qty_raw=1,
                reduce_only=True,
            ),
        ),
    )
