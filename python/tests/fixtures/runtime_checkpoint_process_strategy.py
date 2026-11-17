"""Local-only Runtime strategy fixture for durable checkpoint restart acceptance."""

from __future__ import annotations

import json
import os
from pathlib import Path

from qianxing_strategy import BaseStrategy


class CheckpointProcessStrategy(BaseStrategy):
    STRATEGY_ID = "checkpoint-process-worker"
    STRATEGY_VERSION = "1.0"

    def __init__(self, spec):
        self.restore_log = spec["parameters"]["restore_log"]
        self.bar_count = 0

    def on_bar(self, context, bar):
        self.bar_count += 1
        return self.decision(context, "Bar", bar)

    def on_restore(self, context):
        record = {
            "pid": os.getpid(),
            "bar_count": self.bar_count,
            "worker_id": context.get("worker_id"),
        }
        with Path(self.restore_log).open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(record, sort_keys=True) + "\n")


STRATEGY_CLASS = CheckpointProcessStrategy


def create_strategy(spec):
    return STRATEGY_CLASS(spec)
