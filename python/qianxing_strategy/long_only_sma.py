"""Causal, long-only SMA crossover for index research or explicitly bound markets.

This module supplies decisions only. The host owns matching, risk and accounting.
An index is not a tradable security; choosing it requires a synthetic research
market and must not be described as ETF execution.
"""
from __future__ import annotations

from .base import BaseStrategy

STRATEGY_ID = "long-only-sma"
STRATEGY_VERSION = "1.0.0"


class LongOnlySma(BaseStrategy):
    STRATEGY_ID = STRATEGY_ID
    STRATEGY_VERSION = STRATEGY_VERSION

    def __init__(self, spec):
        config = spec.get("parameters", {})
        self.fast = config.get("fast_window", 20)
        self.slow = config.get("slow_window", 60)
        self.exposure_bps = config.get("exposure_bps", 9500)
        self.start_ts = config.get("start_ts", 0)
        self.lot_raw = config.get("lot_raw", 10**9)
        for value in (self.fast, self.slow, self.exposure_bps, self.start_ts, self.lot_raw):
            if type(value) is not int:
                raise ValueError("SMA parameters must be integers")
        if not 1 <= self.fast < self.slow <= 9999:
            raise ValueError("require 1 <= fast_window < slow_window <= 9999")
        if not 0 < self.exposure_bps <= 10000 or self.start_ts < 0 or self.lot_raw <= 0:
            raise ValueError("invalid exposure_bps, start_ts or lot_raw")

    def history_requirements(self):
        """Suppress signal evaluation until the full crossover window is visible."""
        return {"warmup_bars": self.slow + 1}

    def on_bar(self, context, bar):
        instrument = bar["instrument"]
        rows = self.history(context, instrument, length=self.slow + 1, fields=("close_raw",))
        intents = []
        if bar["ts"] >= self.start_ts and len(rows) == self.slow + 1:
            closes = [row["close_raw"] for row in rows]
            if any(type(value) is not int or value <= 0 for value in closes):
                raise ValueError("SMA requires positive integer close_raw prices")
            previous = closes[:-1]
            before = sum(previous[-self.fast:]) * self.slow - sum(previous[-self.slow:]) * self.fast
            current = sum(closes[-self.fast:]) * self.slow - sum(closes[-self.slow:]) * self.fast
            position = self.position_qty(context, instrument)
            if position < 0:
                raise ValueError("long-only strategy received a short position")
            if before <= 0 < current and position == 0:
                available = context.get("available_margin_raw")
                if type(available) is not int or available < 0:
                    raise ValueError("positive buying power must be supplied by the host")
                quantity = available * self.exposure_bps * 10**9 // (10000 * closes[-1])
                quantity = quantity // self.lot_raw * self.lot_raw
                if quantity:
                    intents.append(self.buy(instrument, qty_raw=quantity))
            elif before >= 0 > current and position > 0:
                intent = self.close_position(context, instrument)
                if intent is not None:
                    intents.append(intent)
        return self.decision(context, "Bar", bar, intents=intents)


STRATEGY_CLASS = LongOnlySma
