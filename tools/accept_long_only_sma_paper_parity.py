"""Run the shared LongOnlySma fixture through Rust backtest and real Paper workers."""
from __future__ import annotations

from fractions import Fraction
import json
import os
from pathlib import Path
import subprocess
import tempfile
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

from qianxing_bridge.app import run_backtest_with_strategy, verify_run  # noqa: E402
from qianxing_strategy.long_only_sma import LongOnlySma  # noqa: E402


def decimal_value(value: dict[str, object] | None) -> Fraction | None:
    if value is None:
        return None
    return Fraction(int(value["coefficient"]), 10 ** int(value["scale"]))


def event_rows(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def fill_rows(events: list[dict[str, object]]) -> list[tuple[str, Fraction, Fraction]]:
    order_sides = {}
    for event in events:
        kind = event.get("kind", {})
        if "OrderSubmitted" in kind:
            order = kind["OrderSubmitted"]["order"]
            order_sides[order["client_id"]] = order["side"]
        if "LedgerApplied" in kind:
            entry = kind["LedgerApplied"]["entry"]
            if entry["kind"] == "TradePosition":
                exact = entry.get("exact_amounts")
                quantity = decimal_value(exact.get("quantity")) if exact else Fraction(entry["quantity"], 10**9)
                if entry.get("order_id") is not None and quantity:
                    order_sides.setdefault(entry["order_id"], "Buy" if quantity > 0 else "Sell")
    fills = []
    for event in events:
        kind = event.get("kind", {})
        if "Filled" not in kind:
            continue
        fill = kind["Filled"]["fill"]
        exact = fill.get("exact_amounts")
        quantity = decimal_value(exact.get("quantity")) if exact else Fraction(fill["qty"], 10**9)
        price = decimal_value(exact.get("price")) if exact else Fraction(fill["price"], 10**9)
        side = order_sides.get(fill["order_id"])
        if side is None:
            raise RuntimeError(f"fill has no preceding order side: {fill['order_id']}")
        fills.append((side, quantity, price))
    return fills


def ledger_rows(events: list[dict[str, object]]) -> list[tuple[str, Fraction, Fraction, Fraction | None]]:
    rows = []
    for event in events:
        kind = event.get("kind", {})
        if "LedgerApplied" not in kind:
            continue
        entry = kind["LedgerApplied"]["entry"]
        entry_kind = entry["kind"]
        if entry_kind not in {"TradeCash", "TradePosition", "Fee"}:
            continue
        exact = entry.get("exact_amounts")
        amount = decimal_value(exact.get("amount")) if exact else Fraction(entry["amount"], 10**9)
        quantity = decimal_value(exact.get("quantity")) if exact else Fraction(entry["quantity"], 10**9)
        price = decimal_value(exact.get("price")) if exact else (
            Fraction(entry["price"], 10**9) if entry.get("price") is not None else None
        )
        rows.append((entry_kind, amount, quantity, price))
    return rows


def local_backtest(bars_path: Path, output_dir: Path) -> tuple[list[tuple[str, Fraction, Fraction]], list[tuple[str, Fraction, Fraction, Fraction | None]]]:
    fixture = json.loads((ROOT / "python/tests/fixtures/long-only-sma-parity-v1.json").read_text(encoding="utf-8"))
    scale = 10**9
    closes = fixture["close_raw"] + [fixture["close_raw"][-1]]
    bars_path.write_text(json.dumps({
        "schema_version": 1,
        "instrument": fixture["instrument"],
        "source": "long-only-sma-parity-v1",
        "ts": [100 + index for index in range(len(closes))],
        "open_raw": closes,
        "high_raw": closes,
        "low_raw": closes,
        "close_raw": closes,
        "volume_raw": [scale] * len(closes),
    }), encoding="utf-8")
    spec = {
        "schema_version": 1,
        "run_id": "r16-long-only-sma-paper-parity",
        "instrument": fixture["instrument"],
        "bars_path": str(bars_path),
        "settlement_currency": "USDT",
        "initial_cash_raw": fixture["available_margin_raw"],
        "seed": 20261029,
        "output_dir": str(output_dir),
        "strategy": {
            "kind": "sma_cross", "strategy_id": LongOnlySma.STRATEGY_ID,
            "quantity_raw": scale, "fast_window": 5, "slow_window": 20,
        },
        "market_spec": {
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "product": "spot", "base_currency": "BTC", "quote_currency": "USDT",
            "settlement_currency": "USDT", "contract_size": scale, "linear": False,
            "inverse": False, "price_tick": 1, "qty_step": 1, "min_qty": 1,
            "min_notional_raw": 0, "max_leverage": 1, "maintenance_margin_bps": 0,
            "valid_from": 0, "valid_to": None,
            "exact_rules": {
                "contract_size": {"coefficient": "1", "scale": 0},
                "price_tick": {"coefficient": "1", "scale": 8},
                "quantity_step": {"coefficient": "1", "scale": 8},
                "minimum_quantity": {"coefficient": "1", "scale": 8},
                "minimum_notional": {"coefficient": "0", "scale": 0},
            },
        },
        "fee_schedule": {"maker_fee_bps": 0, "taker_fee_bps": 5},
    }
    result = run_backtest_with_strategy(
        spec, LongOnlySma({"parameters": fixture["strategy"]}),
        strategy_id=LongOnlySma.STRATEGY_ID,
        strategy_version=LongOnlySma.STRATEGY_VERSION,
        source_sha256="a" * 64,
    )
    verification = verify_run(result)
    if not verification["verified"]:
        raise RuntimeError(f"Rust backtest run artifact did not verify: {verification['mismatches']}")
    trace = event_rows(Path(result["artifacts"]["event_trace"]))
    fills = fill_rows(trace)
    ledger = ledger_rows(trace)
    if len(fills) != 2:
        raise RuntimeError(f"expected two local Rust fills; got {fills}")
    return fills, ledger


def paper_workers(root: Path) -> tuple[list[tuple[str, Fraction, Fraction]], list[tuple[str, Fraction, Fraction, Fraction | None]]]:
    root.mkdir(parents=True, exist_ok=True)
    fixture = json.loads((ROOT / "python/tests/fixtures/long-only-sma-parity-v1.json").read_text(encoding="utf-8"))
    bars = {
        "schema_version": 1, "instrument": fixture["instrument"], "source": "long-only-sma-parity-v1",
        "ts": [100 + index for index in range(6)],
        "open_raw": fixture["close_raw"], "high_raw": fixture["close_raw"],
        "low_raw": fixture["close_raw"], "close_raw": fixture["close_raw"],
        "volume_raw": [10**9] * 6,
    }
    bars_path = root / "bars.json"
    bars_path.write_text(json.dumps({key: value[:4] if isinstance(value, list) else value for key, value in bars.items()}), encoding="utf-8")
    runtime_template = json.loads((ROOT / "deploy/qianxing.runtime.paper-strategy.example.json").read_text(encoding="utf-8"))
    runtime_template["storage"]["data_dir"] = str(root)
    runtime_template["workers"][2]["account_id"] = "main"
    runtime_template["workers"][2]["venue_id"] = "paper"
    runtime_template["workers"][3]["instrument_spec_path"] = str(ROOT / "deploy/qianxing.binance.spot.spec.json")
    runtime_template["workers"][3]["paper_initial_cash_raw"] = fixture["available_margin_raw"]
    runtime_template["strategy"].update({
        "version": LongOnlySma.STRATEGY_VERSION, "account_id": "main", "venue_id": "paper",
        "instrument": fixture["instrument"], "target_snapshot_path": None,
        "risk_rules": {"version": "r16-fixture", "max_qty_raw": 500_000_000_000, "max_notional_raw": None, "no_short": True},
        "python_timeout_ms": 10000, "id": LongOnlySma.STRATEGY_ID,
        "bars_snapshot_path": str(bars_path), "python_module": "qianxing_strategy.long_only_sma",
        "python_strategy_spec": {"parameters": fixture["strategy"]},
    })
    exe = ROOT / "target/debug/qx-cli.exe"
    if not exe.exists():
        env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG="0", CARGO_BUILD_JOBS="1")
        subprocess.run(["cargo", "build", "-p", "qx-cli", "--bin", "qx-cli", "--offline"], cwd=ROOT, env=env, check=True)
    for step, bar_count, quote in ((1, 4, 3_000_000_000), (2, 6, 2_000_000_000)):
        runtime = json.loads(json.dumps(runtime_template))
        runtime["scheduler"].update({
            "state_path": f"scheduler-{step}.json",
            "jobs_path": str(root / f"jobs-{step}.json"),
            "job_queue_path": f"job-queue-{step}",
        })
        runtime_path = root / f"runtime-{step}.json"
        runtime_path.write_text(json.dumps(runtime), encoding="utf-8")
        job = {
            "job_id": f"r16-step-{step}", "job_version": "v1", "owner": "strategy-paper",
            "enabled": True, "trigger": {"Cron": "* * * * *"}, "window": "Any",
            "depends_on": [], "input_refs": ["market:BTCUSDT.BINANCE"],
            "output_refs": ["strategy:order-intent"], "timeout_seconds": 60,
            "retry_policy": {"max_attempts": 1, "backoff_seconds": 0, "retryable_codes": []},
            "concurrency_key": "r16-long-only-sma", "idempotency_key": f"r16-step-{step}-v1",
            "permission_scope": "strategy", "audit_reason": "R16 fixture parity", "dry_run": False,
        }
        (root / f"jobs-{step}.json").write_text(json.dumps([job]), encoding="utf-8")
        visible_bars = {key: value[:bar_count] if isinstance(value, list) else value for key, value in bars.items()}
        bars_path.write_text(json.dumps(visible_bars), encoding="utf-8")
        if step == 1:
            subprocess.run([str(exe), "paper-worker", str(runtime_path), "paper-execution", "--once"], cwd=ROOT, check=True)
        subprocess.run([
            str(exe), "paper-e2e", str(runtime_path), "--quote-price-raw", str(quote),
            "--quote-qty-raw", "1000000000000",
        ], cwd=ROOT, check=True)
    events = json.loads((root / "paper-main-paper-events.json").read_text(encoding="utf-8"))
    audit = json.loads((root / "audit.json").read_text(encoding="utf-8"))
    final_records = {}
    for item in audit:
        record = item["record"]
        final_records[record["command_id"]] = record["status"]
    if len(fill_rows(events)) != 2 or len(events) < 1:
        raise RuntimeError("Paper event log did not contain both expected fills")
    if not final_records or any(status != "Executed" for status in final_records.values()):
        raise RuntimeError(f"Paper command audit did not finish Executed: {final_records}")
    positions = [row[2] for row in ledger_rows(events) if row[0] == "TradePosition"]
    if sum(positions, Fraction(0)) != 0:
        raise RuntimeError(f"Paper exact Ledger position is not flat: {positions}")
    return fill_rows(events), ledger_rows(events)


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="r16-sma-parity-", dir=ROOT / "target") as directory:
        root = Path(directory)
        local_fills, local_ledger = local_backtest(root / "local-bars.json", root / "local-run")
        paper_fills, paper_ledger = paper_workers(root / "paper")
        if local_fills != paper_fills:
            raise RuntimeError(f"ordered fill mismatch: local={local_fills}, paper={paper_fills}")
        if local_ledger != paper_ledger:
            raise RuntimeError(f"Ledger effect mismatch: local={local_ledger}, paper={paper_ledger}")
        print(json.dumps({"local_fills": local_fills, "paper_fills": paper_fills, "ledger_rows": len(paper_ledger), "flat_position": True}, default=str))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
