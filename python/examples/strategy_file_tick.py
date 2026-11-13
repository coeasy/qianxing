"""Class-based L1 strategy for local ``qianxing run-strategy`` research."""

from qianxing_strategy import BaseStrategy


STRATEGY_ID = "tick-moving-average"
STRATEGY_VERSION = "1.0"


class TickMovingAverage(BaseStrategy):
    STRATEGY_ID = STRATEGY_ID
    STRATEGY_VERSION = STRATEGY_VERSION
    STRATEGY_MANIFEST = {
        "schema_version": 1,
        "strategy_id": STRATEGY_ID,
        "strategy_version": STRATEGY_VERSION,
        "parameters": {
            "fast_window": {
                "type": "integer",
                "minimum": 2,
                "maximum": 100,
                "default": 5,
            },
            "slow_window": {
                "type": "integer",
                "minimum": 3,
                "maximum": 500,
                "default": 20,
            },
            "quantity_raw": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1_000_000_000_000,
                "default": 1_000_000_000,
            },
        },
        "instrument_requirements": {
            "asset_classes": [],
            "products": [],
            "venues": [],
            "requires_explicit_spec": False,
        },
        "data_level": "tick",
        # Warmup bars are not Tick counts; this strategy tracks its own price window.
        "warmup_bars": 0,
        "dependencies": [],
    }

    def __init__(self, spec):
        strategy = spec["strategy"]
        self.fast_window = int(strategy["fast_window"])
        self.slow_window = int(strategy["slow_window"])
        self.quantity_raw = int(strategy["quantity_raw"])
        if not 0 < self.fast_window < self.slow_window or self.quantity_raw <= 0:
            raise ValueError("require 0 < fast_window < slow_window and quantity_raw > 0")
        self.prices = []

    def on_tick(self, context, tick):
        last = tick.get("last_raw")
        if last is None:
            last = (int(tick["bid_raw"]) + int(tick["ask_raw"])) // 2
        self.prices.append(int(last))
        self.prices = self.prices[-(self.slow_window + 1) :]

        intents = []
        if len(self.prices) > self.slow_window:
            previous = self.prices[:-1]
            fast_now = sum(self.prices[-self.fast_window :])
            slow_now = sum(self.prices[-self.slow_window :])
            fast_before = sum(previous[-self.fast_window :])
            slow_before = sum(previous[-self.slow_window :])
            instrument = tick["instrument"]
            key = f"{instrument['symbol']}.{instrument['venue']}"
            position = self.position_qty(context, key)

            crossed_up = (
                fast_before * self.slow_window <= slow_before * self.fast_window
                and fast_now * self.slow_window > slow_now * self.fast_window
            )
            crossed_down = (
                fast_before * self.slow_window >= slow_before * self.fast_window
                and fast_now * self.slow_window < slow_now * self.fast_window
            )
            if crossed_up and position <= 0:
                intents.append(self.buy(instrument, qty_raw=self.quantity_raw))
            elif crossed_down and position > 0:
                close = self.close_position(context, instrument)
                if close is not None:
                    intents.append(close)

        return self.decision(context, "Tick", tick, intents=intents)


STRATEGY_CLASS = TickMovingAverage


def create_strategy(spec):
    """Return a fresh strategy instance for one run or search candidate."""
    return STRATEGY_CLASS(spec)
