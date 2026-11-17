"""Minimal scheduler-driven Runtime strategy using ``BaseStrategy.on_timer``.

This example is for ``qx-cli strategy-worker``. It observes a frozen Bar
snapshot when one is configured and deliberately emits no trading intents.
"""

from typing import ClassVar

from qianxing_strategy import BaseStrategy


STRATEGY_ID = "scheduled-observer"
STRATEGY_VERSION = "1.0"


class ScheduledObserver(BaseStrategy):
    STRATEGY_ID = STRATEGY_ID
    STRATEGY_VERSION = STRATEGY_VERSION
    STRATEGY_MANIFEST: ClassVar[dict[str, object]] = {
        "schema_version": 1,
        "strategy_id": STRATEGY_ID,
        "strategy_version": STRATEGY_VERSION,
        "parameters": {},
        "instrument_requirements": {
            "asset_classes": [],
            "products": [],
            "venues": [],
            "requires_explicit_spec": False,
        },
        "data_level": "timer",
        "warmup_bars": 0,
        "dependencies": [],
    }

    def __init__(self, _spec):
        self.timer_calls = 0
        self.last_close_exact = None

    def on_timer(self, context, timer):
        self.timer_calls += 1
        close = context["data"].current_decimal(
            context["instrument"], field="close"
        )
        self.last_close_exact = None if close is None else str(close)
        # Replace this no-order decision with deterministic intent logic when ready.
        return self.decision(context, "Timer", timer)


STRATEGY_CLASS = ScheduledObserver


def create_strategy(spec):
    return STRATEGY_CLASS(spec)
