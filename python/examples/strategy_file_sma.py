"""Class-based Bar strategy for ``qianxing run-strategy/search-strategy``."""

from qianxing_strategy import BaseStrategy

STRATEGY_ID = "file-sma-cross"
STRATEGY_VERSION = "1.3"
WARMUP_BARS = 40


class SmaCross(BaseStrategy):
    """Small event strategy using the standard Python strategy helper."""

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
                "maximum": 10,
                "default": 5,
            },
            "slow_window": {
                "type": "integer",
                "minimum": 10,
                "maximum": 40,
                "default": 20,
            },
        },
        "instrument_requirements": {
            "asset_classes": ["crypto"],
            "products": ["spot", "perpetual"],
            "venues": ["BINANCE", "OKX"],
            "requires_explicit_spec": True,
        },
        "data_level": "bar",
        "warmup_bars": WARMUP_BARS,
        "dependencies": [],
    }

    def __init__(self, spec):
        config = spec["strategy"]
        parameters = spec.get("parameters", {})
        self.fast_window = int(parameters.get("fast_window", config["fast_window"]))
        self.slow_window = int(parameters.get("slow_window", config["slow_window"]))
        self.quantity_raw = int(config["quantity_raw"])
        if not 0 < self.fast_window < self.slow_window or self.quantity_raw <= 0:
            raise ValueError(
                "require 0 < fast_window < slow_window and quantity_raw > 0"
            )

    def history_requirements(self):
        return {"warmup_bars": WARMUP_BARS}

    def on_bar(self, context, bar):
        instrument = bar["instrument"]
        closes = self.history_decimal(
            context,
            instrument,
            length=self.slow_window + 1,
            field="close",
        )
        intents = []
        if len(closes) > self.slow_window:
            previous = closes[:-1]
            fast_now = sum(closes[-self.fast_window :])
            slow_now = sum(closes[-self.slow_window :])
            fast_before = sum(previous[-self.fast_window :])
            slow_before = sum(previous[-self.slow_window :])
            instrument_key = f"{instrument['symbol']}.{instrument['venue']}"
            position = self.position_qty(context, instrument_key)

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
                close_intent = self.close_position(context, instrument)
                if close_intent is not None:
                    intents.append(close_intent)

        return self.decision(context, "Bar", bar, intents=intents)


STRATEGY_CLASS = SmaCross
