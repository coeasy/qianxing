"""Cross-account OKX/Binance perpetual spread example for sandbox runtimes.

The strategy compares aligned OKX and Binance closes, opens opposite legs when
the basis moves beyond a threshold, and flattens after it mean-reverts. Account
scoped position keys are required so an unreconciled account is never treated
as flat.
"""

from decimal import Decimal

from qianxing_bridge.strategy import StrategyInput, account_position_key
from qianxing_strategy import StrategyIntent, StrategyOutput


BINANCE_ACCOUNT = "binance-usdm-sandbox"
OKX_ACCOUNT = "okx-demo-swap"
BINANCE_INSTRUMENT = "BTC/USDT:USDT.BINANCE"
OKX_INSTRUMENT = "BTC/USDT:USDT.OKX"
BINANCE_TARGET = Decimal("0.001")  # base BTC quantity; align with current frozen Binance rules
OKX_TARGET = Decimal("1")  # OKX contract count; align contract size with Binance target notional
LEVERAGE = 2
WINDOW = 20
ENTRY_BPS = Decimal("30")
EXIT_BPS = Decimal("5")


def _position(request: StrategyInput, account_id: str, instrument: str) -> Decimal | None:
    key = account_position_key(account_id, instrument)
    value = request.positions_exact.get(key)
    return None if value is None else Decimal(value)


def _delta_intent(
    *,
    intent_id: int,
    account_id: str,
    instrument: str,
    current: Decimal,
    desired: Decimal,
) -> StrategyIntent | None:
    # If the signal reverses, flatten first. The next bar may open the opposite
    # pair after reconciliation has confirmed the close.
    target = Decimal(0) if current and desired and current * desired < 0 else desired
    delta = target - current
    if not delta:
        return None
    return StrategyIntent(
        intent_id=intent_id,
        instrument=instrument,
        account_id=account_id,
        side="buy" if delta > 0 else "sell",
        qty_raw=0,
        qty_exact=format(abs(delta), "f"),
        reduce_only=target == 0 and current != 0,
        position_side="net",
        position_mode="one_way",
        margin_mode="isolated",
        leverage=LEVERAGE,
    )


def on_event(request: StrategyInput) -> StrategyOutput:
    okx_bars = request.bars
    binance_bars = request.market_bars.get(BINANCE_INSTRUMENT)
    binance_current = _position(request, BINANCE_ACCOUNT, BINANCE_INSTRUMENT)
    okx_current = _position(request, OKX_ACCOUNT, OKX_INSTRUMENT)
    # A missing alias means that account has not produced a reconciled ledger
    # snapshot yet; do not infer a zero position and risk duplicating a leg.
    signal = 0
    if (
        binance_current is not None
        and okx_current is not None
        and okx_bars is not None
        and binance_bars is not None
    ):
        binance_by_ts = dict(zip(binance_bars.ts, binance_bars.close_raw))
        aligned = [
            (okx_close, binance_by_ts[ts])
            for ts, okx_close in zip(okx_bars.ts, okx_bars.close_raw)
            if ts in binance_by_ts
        ]
        if len(aligned) >= WINDOW:
            basis = [
                (Decimal(okx_close - binance_close) * Decimal(10_000))
                / Decimal(binance_close)
                for okx_close, binance_close in aligned[-WINDOW:]
            ]
            deviation = basis[-1] - sum(basis) / WINDOW
            if deviation >= ENTRY_BPS:
                signal = -1  # OKX rich: short OKX, long Binance.
            elif deviation <= -ENTRY_BPS:
                signal = 1  # OKX cheap: long OKX, short Binance.
            elif abs(deviation) > EXIT_BPS:
                if okx_current > 0 and binance_current < 0:
                    signal = 1
                elif okx_current < 0 and binance_current > 0:
                    signal = -1

    intents = []
    if binance_current is not None and okx_current is not None:
        binance_desired = -BINANCE_TARGET * signal
        okx_desired = OKX_TARGET * signal
        for intent in (
            _delta_intent(
                intent_id=1,
                account_id=BINANCE_ACCOUNT,
                instrument=BINANCE_INSTRUMENT,
                current=binance_current,
                desired=binance_desired,
            ),
            _delta_intent(
                intent_id=2,
                account_id=OKX_ACCOUNT,
                instrument=OKX_INSTRUMENT,
                current=okx_current,
                desired=okx_desired,
            ),
        ):
            if intent is not None:
                intents.append(intent)

    return StrategyOutput(
        request_id=request.request_id,
        strategy_id=request.strategy_id,
        signal_id=max(1, request.as_of),
        instrument=request.instrument,
        target_qty=0,
        expires_at=request.as_of + 60_000,
        intents=tuple(intents),
    )
