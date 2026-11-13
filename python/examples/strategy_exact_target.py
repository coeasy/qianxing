"""Python Strategy API example for an exact target position on a small coin."""

from decimal import Decimal

from qianxing_strategy import StrategyOutput


def on_event(request):
    """Rebalance to a 1e-10 target using one-way position semantics."""
    current = Decimal(request.positions_exact.get(request.instrument, "0"))
    desired = Decimal("0.0000000001")
    target = current if current == desired else desired
    return StrategyOutput.for_target_position(
        request,
        signal_id=max(1, request.as_of),
        target_qty_exact=format(target, "f"),
        expires_at=request.as_of,
    )
