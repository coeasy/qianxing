"""Strategy callback diagnostic input construction."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from .app import DataUnavailableError, InvalidInputError, validate_dataset


def _referenced_input_paths(value: object) -> list[str]:
    """Collect common local data-file references from a versioned input document."""
    path_keys = {
        "path",
        "file",
        "file_path",
        "input_path",
        "source_path",
        "data_path",
        "bars_path",
        "ticks_path",
        "trades_path",
        "book_path",
        "csv_path",
    }
    found: list[str] = []

    def visit(item: object) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                if key in path_keys and isinstance(child, str):
                    found.append(child)
                else:
                    visit(child)
        elif isinstance(item, (list, tuple)):
            for child in item:
                visit(child)

    visit(value)
    return found

def _strategy_diagnostic_request(
    spec: dict[str, object], strategy_id: str, strategy_version: str
):
    from qianxing_bridge.strategy import StrategyInput

    bars_path = spec.get("bars_path")
    if not isinstance(bars_path, str) or not bars_path.strip():
        raise InvalidInputError(
            "diagnose-strategy requires BacktestSpec.bars_path",
            payload={"category": "INVALID_INPUT", "message": "diagnose-strategy requires BacktestSpec.bars_path"},
        )
    path = Path(bars_path).expanduser().resolve()
    try:
        frame_bytes = path.read_bytes()
        frame = json.loads(frame_bytes)
    except OSError as exc:
        message = f"cannot read diagnostic BarFrame {str(path)!r}: {exc}"
        raise DataUnavailableError(
            message, payload={"category": "DATA_UNAVAILABLE", "message": message}
        ) from exc
    except json.JSONDecodeError as exc:
        message = f"diagnostic BarFrame JSON is invalid: {exc}"
        raise InvalidInputError(
            message, payload={"category": "INVALID_INPUT", "message": message}
        ) from exc
    if not isinstance(frame, dict):
        message = "diagnostic BarFrame must be a JSON object"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    instrument = spec.get("instrument")
    if frame.get("instrument") != instrument:
        message = "BarFrame.instrument must match BacktestSpec.instrument"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    timestamps = frame.get("ts")
    if not isinstance(timestamps, list) or not timestamps or any(type(ts) is not int or ts <= 0 for ts in timestamps):
        message = "diagnostic BarFrame.ts must contain positive integer timestamps"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    from qianxing_strategy.diagnostics import MAX_CALLBACK_DIAGNOSTIC_BARS

    if len(timestamps) > MAX_CALLBACK_DIAGNOSTIC_BARS:
        message = f"callback diagnostics accept at most {MAX_CALLBACK_DIAGNOSTIC_BARS} bars"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    if timestamps[-1] >= (1 << 64) - 1:
        message = "last Bar timestamp leaves no representable diagnostic as_of boundary"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    verdict = validate_dataset(
        {
            "schema_version": 1,
            "dataset_id": str(frame.get("source") or path.stem),
            "bars_path": str(path),
        }
    )
    if not verdict.usable or verdict.instrument != instrument or verdict.rows != len(timestamps):
        message = "BarFrame failed Rust dataset validation or does not match the BacktestSpec"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    settlement = spec.get("settlement_currency")
    initial_cash = spec.get("initial_cash_raw")
    if not isinstance(settlement, str) or not settlement.strip() or type(initial_cash) is not int:
        message = "BacktestSpec settlement_currency and initial_cash_raw are required for diagnostics"
        raise InvalidInputError(message, payload={"category": "INVALID_INPUT", "message": message})
    return StrategyInput.from_dict(
        {
            "schema_version": 1,
            "request_id": f"{spec.get('run_id') or 'backtest'}:strategy-diagnostic",
            "strategy_id": strategy_id,
            "strategy_version": strategy_version,
            "data_fingerprint": verdict.content_fingerprint,
            "as_of": timestamps[-1] + 1,
            "instrument": instrument,
            "positions": {},
            "cash": {settlement: initial_cash},
            "available_margin_raw": None,
            "risk_state": "diagnostic_fixed_initial_context",
            "research_targets": {},
            # Dataset BarFrames carry identity metadata alongside OHLCV columns.
            # StrategyInput.bars is a separate strict wire contract, so pass only
            # its declared fields after the dataset itself has been validated.
            "bars": {key: value for key, value in frame.items() if key != "instrument"},
        }
    )

def _multi_market_diagnostic_request(
    spec: dict[str, object], strategy_id: str, strategy_version: str
):
    """Build one strategy-slot diagnostic input with Rust-validated peer BarFrames."""
    from qianxing_bridge.strategy import StrategyInput, StrategyBars
    from qianxing_strategy.diagnostics import MAX_CALLBACK_DIAGNOSTIC_BARS

    markets = spec.get("markets")
    if not isinstance(markets, list) or not markets:
        raise InvalidInputError("multi-market diagnostics require a non-empty markets array")
    settlement = spec.get("settlement_currency")
    initial_cash = spec.get("initial_cash_raw")
    if not isinstance(settlement, str) or not settlement.strip() or type(initial_cash) is not int:
        raise InvalidInputError(
            "multi-market diagnostics require settlement_currency and initial_cash_raw"
        )

    target: tuple[dict, dict] | None = None
    seen_slots: set[str] = set()
    for market_index, market in enumerate(markets):
        if not isinstance(market, dict) or not isinstance(market.get("strategies"), list):
            raise InvalidInputError(f"markets[{market_index}].strategies must be an array")
        for slot in market["strategies"]:
            strategy = slot.get("strategy") if isinstance(slot, dict) else None
            slot_id = strategy.get("strategy_id") if isinstance(strategy, dict) else None
            if not isinstance(slot_id, str) or not slot_id.strip() or slot_id in seen_slots:
                raise InvalidInputError("multi-market strategy slots need unique strategy IDs")
            seen_slots.add(slot_id)
            if slot_id == strategy_id:
                target = (market, slot)
    if target is None:
        raise InvalidInputError(
            f"strategy ID {strategy_id!r} does not match a declared multi-market strategy slot"
        )

    target_market, target_slot = target
    target_instrument = target_market.get("instrument")
    if not isinstance(target_instrument, str) or not target_instrument.strip():
        raise InvalidInputError("target market instrument must be a non-empty string")
    bars_path = target_market.get("bars_path")
    if not isinstance(bars_path, str) or not bars_path.strip():
        raise InvalidInputError("target market requires bars_path")
    request = _strategy_diagnostic_request(
        {
            "run_id": spec.get("run_id"),
            "instrument": target_instrument,
            "bars_path": bars_path,
            "settlement_currency": settlement,
            "initial_cash_raw": initial_cash,
            "account_id": spec.get("account_id"),
            "strategy": target_slot.get("strategy"),
        },
        strategy_id,
        strategy_version,
    )

    market_bars: dict[str, StrategyBars] = {}
    market_inputs: list[dict[str, object]] = []
    target_as_of = request.as_of
    total_rows = 0
    for market_index, market in enumerate(markets):
        instrument = market.get("instrument") if isinstance(market, dict) else None
        path_value = market.get("bars_path") if isinstance(market, dict) else None
        if not isinstance(instrument, str) or not instrument.strip():
            raise InvalidInputError(f"markets[{market_index}].instrument must be a non-empty string")
        if not isinstance(path_value, str) or not path_value.strip():
            raise InvalidInputError(f"markets[{market_index}].bars_path must be a non-empty string")
        path = Path(path_value).expanduser().resolve()
        try:
            frame = json.loads(path.read_text(encoding="utf-8"))
        except OSError as exc:
            raise DataUnavailableError(f"cannot read BarFrame {str(path)!r}: {exc}") from exc
        except json.JSONDecodeError as exc:
            raise InvalidInputError(f"BarFrame JSON is invalid for {instrument}: {exc}") from exc
        if not isinstance(frame, dict) or frame.get("instrument") != instrument:
            raise InvalidInputError(f"BarFrame.instrument must match market {instrument}")
        timestamps = frame.get("ts")
        if not isinstance(timestamps, list) or not timestamps:
            raise InvalidInputError(f"BarFrame for {instrument} must contain timestamps")
        if len(timestamps) > MAX_CALLBACK_DIAGNOSTIC_BARS:
            raise InvalidInputError(
                f"BarFrame for {instrument} exceeds the {MAX_CALLBACK_DIAGNOSTIC_BARS}-bar diagnostic limit"
            )
        verdict = validate_dataset(
            {"schema_version": 1, "dataset_id": f"diagnostic-{market_index}", "bars_path": str(path)}
        )
        if not verdict.usable or verdict.instrument != instrument or verdict.rows != len(timestamps):
            raise InvalidInputError(f"BarFrame failed Rust dataset validation for {instrument}")
        market_inputs.append(
            {
                "instrument": instrument,
                "content_fingerprint": verdict.content_fingerprint,
                "rows": len(timestamps),
            }
        )
        total_rows += len(timestamps)
        if total_rows > MAX_CALLBACK_DIAGNOSTIC_BARS:
            raise InvalidInputError(
                f"multi-market callback diagnostics accept at most {MAX_CALLBACK_DIAGNOSTIC_BARS} total Bars"
            )
        if instrument == target_instrument:
            continue
        raw_bars = {key: value for key, value in frame.items() if key != "instrument"}
        bars = StrategyBars.from_dict(raw_bars)
        visible_count = sum(ts < target_as_of for ts in bars.ts)
        if visible_count:
            clipped = {
                key: (value[:visible_count] if isinstance(value, list) else value)
                for key, value in bars.to_dict().items()
            }
            market_bars[instrument] = StrategyBars.from_dict(clipped)

    fingerprint_material = json.dumps(
        sorted(
            (item["instrument"], item["content_fingerprint"])
            for item in market_inputs
        ),
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    request_value = request.to_dict()
    request_value["data_fingerprint"] = "multi-market-sha256:" + hashlib.sha256(
        fingerprint_material
    ).hexdigest()
    strategy_config = target_slot.get("strategy")
    factory_context = {
        "market": target_market,
        "strategy": strategy_config,
        "strategy_slot": target_slot,
    }
    return (
        StrategyInput.from_dict(request_value),
        strategy_config,
        factory_context,
        market_bars,
        sorted(market_inputs, key=lambda item: str(item["instrument"])),
    )
