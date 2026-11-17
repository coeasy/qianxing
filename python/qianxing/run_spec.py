"""把"策略文件 + 行情 + 起止日期 + 初始资金"合成为标准 BacktestSpec（方案 D1）。

`qianxing run -f strategy.py -s … -e … -a …` 复用既有 `run_strategy_file` 路径：
本模块只做合成，不新增 schema、不改引擎。资金在这里做唯一一次换算
（人读的大单位 → raw 整数），其余字段都保持 spec 原有单位。

默认数据来源必须显式：`--bars` 指向冻结的 BarFrame，或 `--data` 指向数据集包目录。
这里绝不静默去仓库 `deploy/` 或网络找数据。
"""

from __future__ import annotations

import json
import re
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation, localcontext
from pathlib import Path
from typing import Any

from qianxing_bridge import BarFrame, native

DATE_FORMAT = "%Y%m%d"
_DATE_PATTERN = re.compile(r"^\d{8}$")
_ONE_DAY_MS = int(timedelta(days=1).total_seconds() * 1000)


def _date_ms(value: str, label: str) -> int:
    # strptime accepts "2024015" as 2024-01-05; only the two documented widths pass.
    compact = value.replace("-", "")
    if len(value) not in (8, 10) or not _DATE_PATTERN.match(compact):
        raise ValueError(f"{label} must be YYYYMMDD or YYYY-MM-DD: {value!r}")
    try:
        parsed = datetime.strptime(compact, DATE_FORMAT)
    except ValueError as exc:
        raise ValueError(f"{label} must be YYYYMMDD or YYYY-MM-DD: {value!r}") from exc
    return int(parsed.replace(tzinfo=timezone.utc).timestamp() * 1000)


def parse_cash(value: str | int | float | Decimal) -> int:
    """把 100000 / "100000.5" 这类大单位换算成 raw 整数（1e9 精度）。"""
    try:
        amount = Decimal(str(value))
    except InvalidOperation as exc:
        raise ValueError(f"--cash 不是合法数字: {value!r}") from exc
    if not amount.is_finite() or amount <= 0:
        raise ValueError("--cash must be a finite positive number")
    with localcontext() as context:
        context.prec = max(50, len(amount.as_tuple().digits) + 10)
        if amount >= Decimal(2 ** 127) / native.raw_scale():
            raise ValueError("--cash exceeds the raw integer range or precision")
        scaled = amount * native.raw_scale()
        if scaled != scaled.to_integral_value() or scaled <= 0:
            raise ValueError("--cash exceeds the raw integer range or precision")
        return int(scaled)


def settlement_currency(instrument: str) -> str:
    exchange = instrument.rsplit(".", 1)[-1].upper()
    if exchange in {"SSE", "SZSE", "BJSE"} or instrument == "SYNTHETIC.SAMPLE":
        return "CNY"
    raise ValueError("--currency is required when settlement cannot be inferred safely")


def load_bars(path: str | Path) -> BarFrame:
    source = Path(path).expanduser().resolve(strict=True)
    return BarFrame.from_json(source.read_text(encoding="utf-8"))


def slice_frame(frame: BarFrame, start: str | None, end: str | None) -> BarFrame:
    """按 [start, end] 自然日切窗；不重采样、不改字节语义。"""
    lower = _date_ms(start, "--start") if start else None
    upper = _date_ms(end, "--end") + _ONE_DAY_MS - 1 if end else None
    keep = [
        index
        for index, ts in enumerate(frame.ts)
        if (lower is None or ts >= lower) and (upper is None or ts <= upper)
    ]
    if not keep:
        raise ValueError("所选区间内没有 Bar；检查 --start/--end 与行情文件的覆盖范围")
    if len(keep) == len(frame.ts):
        return frame
    return BarFrame(
        instrument=frame.instrument,
        source=frame.source,
        ts=tuple(frame.ts[index] for index in keep),
        open_raw=tuple(frame.open_raw[index] for index in keep),
        high_raw=tuple(frame.high_raw[index] for index in keep),
        low_raw=tuple(frame.low_raw[index] for index in keep),
        close_raw=tuple(frame.close_raw[index] for index in keep),
        volume_raw=tuple(frame.volume_raw[index] for index in keep),
        exact_ohlcv=(tuple(frame.exact_ohlcv[index] for index in keep) if frame.exact_ohlcv is not None else None),
    )


def compose_run_spec(
    *,
    instrument: str,
    bars_path: str | Path,
    output_dir: str | Path,
    initial_cash_raw: int,
    strategy_config: dict[str, Any],
    run_id: str,
    settlement_currency: str = "CNY",
    dataset_snapshot_path: str | Path | None = None,
    seed: int = 0,
) -> dict[str, Any]:
    spec: dict[str, Any] = {
        "schema_version": 1,
        "run_id": run_id,
        "instrument": instrument,
        "bars_path": str(Path(bars_path).resolve()),
        "settlement_currency": settlement_currency,
        "initial_cash_raw": initial_cash_raw,
        "seed": seed,
        "output_dir": str(Path(output_dir).resolve()),
        "strategy": strategy_config,
    }
    if dataset_snapshot_path is not None:
        spec["dataset_snapshot_path"] = str(Path(dataset_snapshot_path).resolve())
    return spec


def write_run_spec(directory: Path | str, spec: dict[str, Any], *, name: str = "input.json") -> Path:
    target = Path(directory).expanduser().resolve()
    target.mkdir(parents=True, exist_ok=True)
    path = target / name
    path.write_text(
        json.dumps(spec, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    return path


def load_data_bundle(directory: Path | str) -> dict[str, Any]:
    """读取数据集包里的 bars/snapshot 路径与血缘标记。"""
    from .dataset_bundle import verify_bundle

    root = Path(directory).expanduser().resolve(strict=True)
    bundle = verify_bundle(root)
    return {
        "bars": root / bundle["bars_path"],
        "snapshot": root / bundle["snapshot_path"],
        "instrument": bundle["instrument"],
        "is_sample": bundle["is_sample"],
        "source": bundle.get("source"),
    }
