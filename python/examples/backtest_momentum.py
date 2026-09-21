"""Minimal cross-language strategy used by ``strategy-backtest`` smoke tests."""

# 契约里的数量一律是定点 raw 口径（SCALE=1e9），写成 1 表示 1e-9 个单位标的：
# 名义额小到 5bp 费用在整数记账里归零，示例等于没走过费用与账本路径。
ONE_UNIT = 1_000_000_000


def on_event(request):
    bars = request.bars
    if bars is None or len(bars.close_raw) < 2:
        target = 0
    else:
        target = ONE_UNIT if bars.close_raw[-1] >= bars.close_raw[-2] else 0
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
