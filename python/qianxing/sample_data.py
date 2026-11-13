"""内置合成样例数据包（方案 D2/D6/D14）。

样例是"零配置首跑"的默认输入：确定性的随机游走 BarFrame、连续自然日、无缺口、
无公司行为，并且带清晰许可标签。它只用于跑通链路与门禁，任何研究结论都必须换成
真实数据源（``qianxing data fetch``）。

落盘/校验形状由 :mod:`qianxing.dataset_bundle` 拥有；本模块只负责"合成什么"。
"""

from __future__ import annotations

import json
import random
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from qianxing_bridge import BarFrame, native

from .dataset_bundle import FREQUENCY_MS, write_dataset_bundle

SAMPLE_DATASET_ID = "qianxing-sample-bars-v1"
SAMPLE_VERSION = "1"
SAMPLE_INSTRUMENT = "SYNTHETIC.SAMPLE"
SAMPLE_ROWS = 600
SAMPLE_SEED = 20261030
SAMPLE_START = "2024-01-01"
# 2025-09-01T00:00:00Z：晚于样例覆盖区间，保证快照"采集时间"不早于数据。
SAMPLE_CAPTURED_AT_MS = 1_756_684_800_000
SAMPLE_LICENSE = "内置合成样例：不是历史行情，不可用于研究结论"
SAMPLE_SOURCE = "sample"


def build_sample_frame(
    *,
    instrument: str = SAMPLE_INSTRUMENT,
    rows: int = SAMPLE_ROWS,
    seed: int = SAMPLE_SEED,
    start: str = SAMPLE_START,
) -> BarFrame:
    """确定性地生成一段连续自然日的合成 BarFrame（无缺口、无公司行为）。"""
    if rows < 2:
        raise ValueError("sample frame needs at least two bars")
    scale = native.raw_scale()
    rng = random.Random(seed)
    start_day = datetime.strptime(start, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    timestamps: list[int] = []
    opens: list[int] = []
    highs: list[int] = []
    lows: list[int] = []
    closes: list[int] = []
    volumes: list[int] = []
    price = 100 * scale
    for index in range(rows):
        timestamps.append(int((start_day + timedelta(days=index)).timestamp() * 1000))
        open_raw = price
        close_raw = max(scale // 100, open_raw + rng.choice((-1, 0, 1)) * rng.randint(0, scale // 50))
        high_raw = max(open_raw, close_raw) + rng.randint(0, scale // 100)
        low_raw = max(1, min(open_raw, close_raw) - rng.randint(0, scale // 100))
        opens.append(open_raw)
        highs.append(high_raw)
        lows.append(low_raw)
        closes.append(close_raw)
        volumes.append(1000 * scale + rng.randint(0, 100 * scale))
        price = close_raw
    return BarFrame(
        instrument=instrument,
        source="qianxing:sample:synthetic",
        ts=tuple(timestamps),
        open_raw=tuple(opens),
        high_raw=tuple(highs),
        low_raw=tuple(lows),
        close_raw=tuple(closes),
        volume_raw=tuple(volumes),
    )


def sample_context() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "instrument_role": "other_tradable",
        "captured_at_ms": SAMPLE_CAPTURED_AT_MS,
        "frequency_ms": FREQUENCY_MS["daily"],
        "timestamp_semantics": "bar_open",
        "price_precision": 2,
        "quantity_precision": 0,
        "price_unit": "sample_price",
        "quantity_unit": "sample_unit",
        "calendar_id": "sample-continuous-days",
        "calendar_version": "1",
        "gaps": [],
        "corporate_action_policy": "not_applicable",
    }


def write_sample_bundle(
    directory: Path | str,
    *,
    instrument: str = SAMPLE_INSTRUMENT,
    rows: int = SAMPLE_ROWS,
    seed: int = SAMPLE_SEED,
    start: str = SAMPLE_START,
) -> dict[str, Any]:
    frame = build_sample_frame(instrument=instrument, rows=rows, seed=seed, start=start)
    return write_dataset_bundle(
        directory,
        frame,
        sample_context(),
        {
            "is_sample": True,
            "source": SAMPLE_SOURCE,
            "source_kind": "synthetic",
            "license": SAMPLE_LICENSE,
            "seed": seed,
            "generator": "qianxing.sample_data.build_sample_frame",
        },
        dataset_id=SAMPLE_DATASET_ID,
        version=SAMPLE_VERSION,
    )


def sample_context_text() -> str:
    return json.dumps(sample_context(), ensure_ascii=False, indent=2)
