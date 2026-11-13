#!/usr/bin/env python3
"""Freeze a quality-checked public Binance Spot OHLCV window for repeatable benchmarks."""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from decimal import Decimal
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen


INTERVAL_MS = 15 * 60 * 1000
RAW_SCALE = 1_000_000_000
BASE_URL = "https://api.binance.com"


def get_json(path: str, params: dict[str, int | str] | None = None):
    url = f"{BASE_URL}{path}"
    if params:
        url = f"{url}?{urlencode(params)}"
    error: Exception | None = None
    for attempt in range(5):
        try:
            request = Request(url, headers={"User-Agent": "qianxing-benchmark-data/1"})
            with urlopen(request, timeout=30) as response:
                return json.load(response)
        except (HTTPError, URLError, TimeoutError) as caught:
            error = caught
            if isinstance(caught, HTTPError) and caught.code not in (418, 429, 500, 502, 503, 504):
                raise
            time.sleep(min(2**attempt, 16))
    raise RuntimeError(f"Binance public API failed after retries: {error}")


def raw_decimal(value: str) -> int:
    scaled = Decimal(value) * RAW_SCALE
    integral = scaled.to_integral_value()
    if scaled != integral:
        raise ValueError(f"source precision exceeds 1e9 raw scale: {value}")
    return int(integral)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--days", type=int, default=180)
    parser.add_argument("--symbol", default="BTCUSDT")
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--manifest", type=Path)
    args = parser.parse_args()
    if args.days < 1 or args.days > 3650:
        parser.error("--days must be in 1..3650")
    manifest_path = args.manifest or args.out.with_suffix(".manifest.json")

    server_time = int(get_json("/api/v3/time")["serverTime"])
    end_ms = server_time // INTERVAL_MS * INTERVAL_MS
    start_ms = end_ms - args.days * 24 * 60 * 60 * 1000
    rows: list[list[object]] = []
    cursor = start_ms
    while cursor < end_ms:
        page = get_json(
            "/api/v3/klines",
            {
                "symbol": args.symbol.upper(),
                "interval": "15m",
                "startTime": cursor,
                "endTime": end_ms - 1,
                "limit": 1000,
            },
        )
        if not page:
            break
        rows.extend(page)
        next_cursor = int(page[-1][0]) + INTERVAL_MS
        if next_cursor <= cursor:
            raise RuntimeError("Binance kline pagination did not advance")
        cursor = next_cursor
        time.sleep(0.05)

    times = [int(row[0]) for row in rows]
    expected_count = (end_ms - start_ms) // INTERVAL_MS
    if len(times) != expected_count or times != list(range(start_ms, end_ms, INTERVAL_MS)):
        gaps = [left for left, right in zip(times, times[1:]) if right - left != INTERVAL_MS]
        raise RuntimeError(
            f"incomplete 15m timeline: expected={expected_count} actual={len(times)} gaps={gaps[:8]}"
        )
    if any(int(row[6]) >= end_ms for row in rows):
        raise RuntimeError("window contains a bar that is not fully closed")
    for row in rows:
        open_price, high_price, low_price, close_price = map(Decimal, row[1:5])
        volume = Decimal(row[5])
        if (
            min(open_price, high_price, low_price, close_price) <= 0
            or high_price < max(open_price, close_price, low_price)
            or low_price > min(open_price, close_price, high_price)
            or volume < 0
        ):
            raise RuntimeError(f"invalid OHLCV candle at open time {row[0]}")

    frame = {
        "instrument": "BTCUSDT.BINANCE",
        "source": "binance-public-spot-api-v3-klines",
        "ts": times,
        "open_raw": [raw_decimal(str(row[1])) for row in rows],
        "high_raw": [raw_decimal(str(row[2])) for row in rows],
        "low_raw": [raw_decimal(str(row[3])) for row in rows],
        "close_raw": [raw_decimal(str(row[4])) for row in rows],
        "volume_raw": [raw_decimal(str(row[5])) for row in rows],
    }
    payload = (json.dumps(frame, separators=(",", ":"), ensure_ascii=False) + "\n").encode()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(payload)
    manifest = {
        "schema_version": 1,
        "provider": "Binance public Spot REST API /api/v3/klines",
        "symbol": args.symbol.upper(),
        "interval": "15m",
        "interval_ms": INTERVAL_MS,
        "days": args.days,
        "bars": len(rows),
        "first_open_time_ms": times[0],
        "last_open_time_ms": times[-1],
        "exclusive_end_time_ms": end_ms,
        "server_time_ms": server_time,
        "all_bars_closed": True,
        "duplicates": len(times) - len(set(times)),
        "gaps": 0,
        "raw_scale": RAW_SCALE,
        "frame_sha256": hashlib.sha256(payload).hexdigest(),
        "frame_bytes": len(payload),
    }
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
