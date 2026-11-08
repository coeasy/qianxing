#!/usr/bin/env python3
"""Acquire checksum-verified Binance Spot monthly kline archives into a BarFrame.

The official archive bytes are retained beside the canonical frame. The output
directory must not exist, so a prior capture is never silently replaced.
"""
from __future__ import annotations

import argparse
import calendar
import csv
from datetime import date, datetime, timezone
from decimal import Decimal, InvalidOperation, ROUND_DOWN
import hashlib
import io
import json
from pathlib import Path
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
from zipfile import BadZipFile, ZipFile

BASE_URL = "https://data.binance.vision/data/spot/monthly/klines"
DAY_MS = 86_400_000
DAY_US = DAY_MS * 1000
RAW_SCALE = Decimal(1_000_000_000)
SOURCE = "binance-public-data:spot-monthly-klines;checksum=sha256"
MAX_ARCHIVE_BYTES = 32 * 1024 * 1024


def fetch(url: str, attempts: int = 3) -> bytes:
    last_error: Exception | None = None
    for attempt in range(attempts):
        try:
            request = Request(url, headers={"User-Agent": "Qianxing-Historical-Data-Acceptance/1"})
            with urlopen(request, timeout=45) as response:
                payload = response.read(MAX_ARCHIVE_BYTES + 1)
            if len(payload) > MAX_ARCHIVE_BYTES:
                raise ValueError(f"download exceeds {MAX_ARCHIVE_BYTES} bytes: {url}")
            return payload
        except (HTTPError, URLError, TimeoutError, OSError) as error:
            if isinstance(error, HTTPError) and error.code == 404:
                raise RuntimeError(f"official Binance archive is missing: {url}") from error
            last_error = error
            if attempt + 1 < attempts:
                time.sleep(1 + attempt)
    raise RuntimeError(f"download failed after {attempts} attempts: {url}: {last_error}")


def sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def verify_checksum(checksum_payload: bytes, archive: bytes, archive_name: str) -> str:
    try:
        fields = checksum_payload.decode("ascii").strip().split()
    except UnicodeDecodeError as error:
        raise RuntimeError(f"checksum is not ASCII for {archive_name}") from error
    if not fields or len(fields[0]) != 64:
        raise RuntimeError(f"checksum file has invalid format for {archive_name}")
    expected = fields[0].lower()
    if len(fields) > 1 and fields[-1].lstrip("*") != archive_name:
        raise RuntimeError(f"checksum file names a different archive: {fields[-1]}")
    actual = sha256(archive)
    if actual != expected:
        raise RuntimeError(f"SHA-256 mismatch for {archive_name}: expected {expected}, got {actual}")
    return expected


def exact_decimal(text: str) -> tuple[dict[str, object], int, int]:
    try:
        value = Decimal(text)
    except InvalidOperation as error:
        raise RuntimeError(f"invalid source decimal {text!r}") from error
    if not value.is_finite() or value < 0:
        raise RuntimeError(f"source decimal must be finite and non-negative: {text!r}")
    sign, digits, exponent = value.as_tuple()
    coefficient = int("".join(map(str, digits)) or "0") * (-1 if sign else 1)
    scale = max(-exponent, 0)
    if exponent > 0:
        coefficient *= 10**exponent
    while scale and coefficient % 10 == 0:
        coefficient //= 10
        scale -= 1
    if scale > 18:
        raise RuntimeError(f"source decimal precision exceeds BarFrame v2 limit: {text!r}")
    raw = int((value * RAW_SCALE).to_integral_value(rounding=ROUND_DOWN))
    precision = max(-exponent, 0)
    return {"coefficient": str(coefficient), "scale": scale}, raw, precision


def parse_month_rows(payload: bytes, symbol: str, year: int, month: int) -> list[dict[str, object]]:
    expected_name = f"{symbol}-1d-{year:04d}-{month:02d}.csv"
    try:
        with ZipFile(io.BytesIO(payload)) as archive:
            names = [name for name in archive.namelist() if not name.endswith("/")]
            if names != [expected_name]:
                raise RuntimeError(f"unexpected archive members for {symbol}-{year:04d}-{month:02d}: {names}")
            csv_bytes = archive.read(expected_name)
    except BadZipFile as error:
        raise RuntimeError(f"official archive is not a valid zip: {symbol}-{year:04d}-{month:02d}") from error

    rows: list[dict[str, object]] = []
    reader = csv.reader(io.StringIO(csv_bytes.decode("utf-8"), newline=""))
    for line_number, columns in enumerate(reader, start=1):
        if line_number == 1 and columns and columns[0].lower() == "open_time":
            continue
        if len(columns) != 12:
            raise RuntimeError(f"expected 12 Binance kline columns at {expected_name}:{line_number}")
        try:
            open_time = int(columns[0])
            close_time = int(columns[6])
            trades = int(columns[8])
        except ValueError as error:
            raise RuntimeError(f"invalid integer field at {expected_name}:{line_number}") from error
        if year >= 2025:
            if open_time % DAY_US or close_time != open_time + DAY_US - 1:
                raise RuntimeError(f"invalid 1d UTC microsecond timestamps at {expected_name}:{line_number}")
            open_time_ms = open_time // 1000
        else:
            if open_time % DAY_MS or close_time != open_time + DAY_MS - 1:
                raise RuntimeError(f"invalid 1d UTC millisecond timestamps at {expected_name}:{line_number}")
            open_time_ms = open_time
        timestamp = datetime.fromtimestamp(open_time_ms / 1000, tz=timezone.utc)
        if timestamp.year != year or timestamp.month != month:
            raise RuntimeError(f"invalid 1d UTC open time at {expected_name}:{line_number}")
        if trades < 0:
            raise RuntimeError(f"invalid trade count at {expected_name}:{line_number}")

        values = [exact_decimal(columns[index]) for index in (1, 2, 3, 4, 5)]
        exact = [item[0] for item in values]
        raw = [item[1] for item in values]
        precision = [item[2] for item in values]
        open_value, high_value, low_value, close_value, volume_value = map(Decimal, columns[1:6])
        if min(open_value, high_value, low_value, close_value) <= 0 or volume_value < 0:
            raise RuntimeError(f"non-positive OHLC or negative volume at {expected_name}:{line_number}")
        if high_value < max(open_value, close_value, low_value) or low_value > min(open_value, close_value, high_value):
            raise RuntimeError(f"inconsistent OHLC bounds at {expected_name}:{line_number}")
        rows.append({"ts": open_time_ms, "raw": raw, "exact": exact, "precision": precision})
    return rows


def month_sequence(start: date, end: date) -> list[tuple[int, int]]:
    result = []
    year, month = start.year, start.month
    while (year, month) <= (end.year, end.month):
        result.append((year, month))
        year, month = (year + 1, 1) if month == 12 else (year, month + 1)
    return result


def acquire(
    symbol: str,
    start: date,
    end: date,
    output: Path,
    expected_archives: dict[str, str] | None = None,
    snapshot_captured_at_ms: int | None = None,
) -> dict[str, object]:
    if output.exists():
        raise RuntimeError(f"refusing to overwrite existing output directory: {output}")
    output.mkdir(parents=True)
    archives_dir = output / "archives"
    archives_dir.mkdir()

    archive_records = []
    all_rows: list[dict[str, object]] = []
    for year, month in month_sequence(start, end):
        archive_name = f"{symbol}-1d-{year:04d}-{month:02d}.zip"
        base = f"{BASE_URL}/{symbol}/1d/{archive_name}"
        archive_bytes = fetch(base)
        checksum_bytes = fetch(base + ".CHECKSUM")
        digest = verify_checksum(checksum_bytes, archive_bytes, archive_name)
        if expected_archives is not None and expected_archives.get(archive_name) != digest:
            raise RuntimeError(
                f"official archive revision differs from expected evidence for {archive_name}: {digest}"
            )
        rows = parse_month_rows(archive_bytes, symbol, year, month)
        (archives_dir / archive_name).write_bytes(archive_bytes)
        (archives_dir / f"{archive_name}.CHECKSUM").write_bytes(checksum_bytes)
        archive_records.append({
            "url": base,
            "checksum_url": base + ".CHECKSUM",
            "archive": archive_name,
            "archive_sha256": digest,
            "checksum_file_sha256": sha256(checksum_bytes),
            "rows": len(rows),
        })
        all_rows.extend(rows)

    if expected_archives is not None and set(expected_archives) != {
        row["archive"] for row in archive_records
    }:
        raise RuntimeError("expected acquisition archive list does not match the requested month range")

    selected = [row for row in all_rows if start <= datetime.fromtimestamp(row["ts"] / 1000, tz=timezone.utc).date() <= end]
    selected.sort(key=lambda row: row["ts"])
    expected_dates = list(
        (start.fromordinal(day).isoformat() for day in range(start.toordinal(), end.toordinal() + 1))
    )
    observed_dates = [datetime.fromtimestamp(row["ts"] / 1000, tz=timezone.utc).date().isoformat() for row in selected]
    if observed_dates != expected_dates:
        missing = sorted(set(expected_dates) - set(observed_dates))[:10]
        duplicates = len(observed_dates) - len(set(observed_dates))
        raise RuntimeError(f"historical UTC day coverage is incomplete; missing={missing}, duplicate_rows={duplicates}")

    def column(name: str) -> list[int]:
        index = {"open_raw": 0, "high_raw": 1, "low_raw": 2, "close_raw": 3, "volume_raw": 4}[name]
        return [row["raw"][index] for row in selected]

    exact_rows = []
    price_precision = 0
    quantity_precision = 0
    for row in selected:
        values = row["exact"]
        exact_rows.append({
            "open": values[0], "high": values[1], "low": values[2],
            "close": values[3], "volume": values[4],
        })
        price_precision = max(price_precision, *row["precision"][:4])
        quantity_precision = max(quantity_precision, row["precision"][4])

    frame = {
        "schema_version": 2,
        "instrument": f"{symbol}.BINANCE",
        "source": SOURCE,
        "ts": [row["ts"] for row in selected],
        "open_raw": column("open_raw"),
        "high_raw": column("high_raw"),
        "low_raw": column("low_raw"),
        "close_raw": column("close_raw"),
        "volume_raw": column("volume_raw"),
        "exact_ohlcv": exact_rows,
    }
    frame_bytes = (json.dumps(frame, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    (output / "bars.json").write_bytes(frame_bytes)
    captured_at_ms = snapshot_captured_at_ms or int(datetime.now(timezone.utc).timestamp() * 1000)
    context = {
        "schema_version": 1,
        "instrument_role": "crypto_spot",
        "captured_at_ms": captured_at_ms,
        "frequency_ms": DAY_MS,
        "timestamp_semantics": "bar_open",
        "price_precision": price_precision,
        "quantity_precision": quantity_precision,
        "price_unit": "USDT",
        "quantity_unit": symbol.removesuffix("USDT"),
        "calendar_id": "binance-spot-24x7-utc",
        "calendar_version": "utc-day-v1",
        "gaps": [],
        "corporate_action_policy": "not_applicable",
    }
    (output / "snapshot-context.json").write_text(
        json.dumps(context, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    evidence = {
        "schema_version": 1,
        "kind": "binance-spot-historical-klines-acceptance-v1",
        "source_project": "https://github.com/binance/binance-public-data",
        "archive_base_url": BASE_URL,
        "symbol": symbol,
        "interval": "1d",
        "timezone": "UTC",
        "timestamp_semantics": "bar_open",
        "archive_timestamp_unit": "microseconds since 2025-01-01, milliseconds before",
        "start_date": start.isoformat(),
        "end_date": end.isoformat(),
        "rows": len(selected),
        "coverage_days_contiguous": True,
        "archive_checksums_verified": True,
        "source": SOURCE,
        "frame_sha256": sha256(frame_bytes),
        "snapshot_captured_at_ms": captured_at_ms,
        "archive_manifest": archive_records,
    }
    (output / "acquisition.json").write_text(
        json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return evidence


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--symbol", default="BTCUSDT")
    parser.add_argument("--start", type=date.fromisoformat, required=True)
    parser.add_argument("--end", type=date.fromisoformat, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--expected-acquisition",
        type=Path,
        help="require every official monthly archive hash to match a prior acquisition.json",
    )
    args = parser.parse_args()
    symbol = args.symbol.upper()
    if not symbol.isascii() or not symbol.isalnum() or args.start > args.end:
        parser.error("symbol must be an ASCII Binance pair and start must not exceed end")
    try:
        expected_archives = None
        snapshot_captured_at_ms = None
        if args.expected_acquisition is not None:
            expected = json.loads(args.expected_acquisition.read_text(encoding="utf-8"))
            if (
                expected.get("symbol") != symbol
                or expected.get("start_date") != args.start.isoformat()
                or expected.get("end_date") != args.end.isoformat()
            ):
                raise RuntimeError("expected acquisition does not bind the requested symbol and date range")
            expected_archives = {
                item["archive"]: item["archive_sha256"]
                for item in expected.get("archive_manifest", [])
            }
            if not expected_archives:
                raise RuntimeError("expected acquisition has no monthly archive hash records")
            snapshot_captured_at_ms = expected.get("snapshot_captured_at_ms")
            if not isinstance(snapshot_captured_at_ms, int) or snapshot_captured_at_ms <= 0:
                raise RuntimeError("expected acquisition has no valid frozen snapshot_captured_at_ms")
        evidence = acquire(
            symbol, args.start, args.end, args.output.resolve(), expected_archives,
            snapshot_captured_at_ms,
        )
        if args.expected_acquisition is not None and evidence["frame_sha256"] != expected.get("frame_sha256"):
            raise RuntimeError("reproduced BarFrame hash differs from expected acquisition evidence")
    except Exception as error:
        print(f"acquisition failed: {error}")
        return 2
    print(json.dumps({key: evidence[key] for key in ("symbol", "start_date", "end_date", "rows", "frame_sha256")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
