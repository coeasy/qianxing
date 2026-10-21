#!/usr/bin/env python3
"""Exercise the public Python wheel against shared Rust use cases after clean install."""

from __future__ import annotations

import json
import sys
import tempfile
from pathlib import Path

import qianxing as qx


def write_builtin_catalog_frame(path: Path, bars: int = 90) -> None:
    scale = 1_000_000_000
    ts_values, opens, highs, lows, closes, volumes = [], [], [], [], [], []
    for ts in range(1, bars + 1):
        up = (ts // 5) % 7 < 4
        spike = 3 if ts % 9 == 0 else 1
        body = spike * (2 + ts % 13) * scale
        drift = (40 + ts // 3) * scale
        open_raw = drift if up else drift + body
        close_raw = drift + body if up else drift
        upper_wick = 0 if ts % 6 == 0 else (1 + ts % 4) * scale
        lower_wick = 0 if ts % 7 == 0 else (1 + ts % 5) * scale
        ts_values.append(1_000 + (ts - 1) * 1_000)
        opens.append(open_raw)
        highs.append(max(open_raw, close_raw) + upper_wick)
        lows.append(min(open_raw, close_raw) - lower_wick)
        closes.append(close_raw)
        volumes.append((100 + ts % 7 * 13) * scale)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "instrument": "BTCUSDT.BINANCE",
                "source": "python-wheel-smoke-builtin-catalog-v1",
                "ts": ts_values,
                "open_raw": opens,
                "high_raw": highs,
                "low_raw": lows,
                "close_raw": closes,
                "volume_raw": volumes,
            }
        ),
        encoding="utf-8",
    )


def write_ashare_frame(path: Path, bars: int = 60) -> None:
    scale = 1_000_000_000
    tick = 10_000_000
    closes = []
    for index in range(bars):
        phase = index % 20
        cents = 1_000 + (phase if phase < 10 else 20 - phase)
        closes.append(cents * scale // 100)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "instrument": "000001.SZSE",
                "source": "python-wheel-smoke-ashare-v1",
                "ts": [1_717_378_200_000 + index * 60_000 for index in range(bars)],
                "open_raw": closes,
                "high_raw": [price + tick for price in closes],
                "low_raw": [price - tick for price in closes],
                "close_raw": closes,
                "volume_raw": [4_000 * scale] * bars,
            }
        ),
        encoding="utf-8",
    )


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: python_wheel_smoke.py <repository-root>")
    root = Path(sys.argv[1]).resolve()
    bars = root / "deploy" / "qianxing.bar-frame.example.json"
    primary = root / "deploy" / "qianxing.bar-frame.pairs-primary.example.json"
    reference = root / "deploy" / "qianxing.bar-frame.pairs-reference.example.json"
    depth = root / "deploy" / "qianxing.depth-frame.example.json"

    implemented = qx.doctor()["implemented_use_cases"]
    assert "backtest.bar.v1" in implemented
    assert callable(qx.AppClient.provider_preflight)
    assert callable(qx.AppClient.async_provider_preflight)
    strategies = qx.list_builtin_strategies()
    assert len(strategies) == 17, len(strategies)
    single_leg_kinds = tuple(
        item.name for item in strategies if not item.requires_reference_leg
    )
    assert len(single_leg_kinds) == 13, single_leg_kinds
    verdict = qx.validate_dataset(qx.DatasetSpec("wheel-smoke-data", str(bars)))
    assert verdict.usable and verdict.rows == 70, verdict

    with tempfile.TemporaryDirectory(prefix="qianxing-wheel-smoke-") as temporary:
        output = Path(temporary)
        classic_bars = output / "classic-strategy-bars.json"
        write_builtin_catalog_frame(classic_bars)
        base_strategy = qx.BuiltinStrategySpec(
            kind="sma_cross", strategy_id="wheel-smoke-sma", fast_window=2, slow_window=4
        )
        base = qx.BacktestSpec(
            run_id="wheel-smoke-backtest",
            instrument="BTCUSDT.BINANCE",
            bars_path=str(classic_bars),
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000_000,
            output_dir=str(output / "base"),
            strategy=base_strategy,
        )
        outcome = qx.run_backtest(base)
        verification = qx.verify_run(outcome)
        assert verification.verified, verification.mismatches

        ashare_bars = output / "ashare-bars.json"
        write_ashare_frame(ashare_bars)
        ashare_spec = qx.BacktestSpec(
            run_id="wheel-smoke-ashare",
            instrument="000001.SZSE",
            bars_path=str(ashare_bars),
            settlement_currency="CNY",
            initial_cash_raw=100_000_000_000_000,
            output_dir=str(output / "ashare"),
            strategy=qx.BuiltinStrategySpec(
                kind="sma_cross",
                strategy_id="wheel-smoke-ashare",
                quantity_raw=1_000_000_000_000,
                fast_window=2,
                slow_window=3,
            ),
            market_spec=qx.TradingInstrumentSpec(
                instrument=qx.InstrumentId(symbol="000001", venue="SZSE"),
                product="spot",
                base_currency="000001",
                quote_currency="CNY",
                settlement_currency="CNY",
                contract_size=1_000_000_000,
                linear=True,
                inverse=False,
                price_tick=10_000_000,
                qty_step=100_000_000_000,
                min_qty=100_000_000_000,
                max_leverage=1,
                maintenance_margin_bps=0,
                valid_from=0,
            ),
            margin_model=qx.BacktestNoMargin(),
            execution_cost_rules=qx.BacktestExecutionCostRules(
                name="wheel-smoke-ashare", maker_bp=3, taker_bp=12, latency_base_ns=1_000_000
            ),
            ashare_rules=qx.AshareBacktestRules(
                **json.loads((root / "deploy" / "qianxing.ashare.rules.json").read_text(encoding="utf-8"))
            ),
            fill_model="best_price",
            max_bar_volume_participation_bps=2_500,
            allow_short=False,
        )
        ashare_outcome = qx.run_backtest(ashare_spec)
        assert ashare_outcome.fills > 0, ashare_outcome
        ashare_summary = json.loads(
            Path(ashare_outcome.artifacts["summary"]).read_text(encoding="utf-8")
        )
        assert ashare_summary["fee_model"].startswith("AShareFee@v1["), ashare_summary

        experiment = qx.run_experiment(
            qx.RunExperimentSpec(
                experiment_id="wheel-smoke-parameter-grid",
                base=base,
                parameter_space=(qx.ExperimentParameterSpace("fast_window", (2, 3)),),
                strategy_kinds=("ema_cross", "sma_cross"),
            )
        )
        assert experiment.total_candidates == experiment.succeeded_candidates == 4
        classic_selection = qx.run_experiment(
            qx.RunExperimentSpec(
                experiment_id="wheel-smoke-classic-selection",
                base=base,
                parameter_space=(),
                strategy_kinds=single_leg_kinds,
            )
        )
        assert (
            classic_selection.total_candidates
            == classic_selection.succeeded_candidates
            == len(single_leg_kinds)
        )
        assert all(
            candidate.outcome is not None and candidate.outcome.result_hash
            for candidate in classic_selection.candidates
        ), classic_selection.candidates

        accounts = qx.run_multi_account_backtest(
            qx.MultiAccountBacktestSpec(
                batch_id="wheel-smoke-accounts",
                runs=tuple(
                    qx.BacktestSpec(
                        run_id=f"wheel-smoke-account-{index}",
                        instrument="BTCUSDT.BINANCE",
                        bars_path=str(bars),
                        settlement_currency="USDT",
                        initial_cash_raw=100_000_000_000_000,
                        output_dir=str(output / f"account-{index}"),
                        strategy=qx.BuiltinStrategySpec(
                            kind="sma_cross",
                            strategy_id=f"wheel-smoke-account-strategy-{index}",
                            fast_window=2,
                            slow_window=4,
                        ),
                        account_id=f"wheel-smoke-account-{index}",
                    )
                    for index in range(2)
                ),
            )
        )
        assert accounts.summary["accounts_succeeded"] == 2, accounts.summary

        overflow_runs = tuple(
            qx.BacktestSpec(
                run_id=f"wheel-smoke-overflow-{index}",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(classic_bars),
                settlement_currency="USDT",
                initial_cash_raw=(1 << 127) - 1 if index == 0 else 1,
                output_dir=str(output / f"overflow-{index}"),
                strategy=base_strategy,
                account_id=f"wheel-smoke-overflow-account-{index}",
            )
            for index in range(2)
        )
        try:
            qx.run_multi_account_backtest(
                qx.MultiAccountBacktestSpec(
                    batch_id="wheel-smoke-overflow-preflight", runs=overflow_runs
                )
            )
        except qx.InvalidInputError as error:
            assert error.payload["category"] == "INVALID_INPUT", error.payload
        else:
            raise AssertionError("multi-account cash overflow must fail before execution")

        multileg = qx.run_multi_leg_backtest(
            qx.MultiLegBacktestSpec(
                run_id="wheel-smoke-multileg",
                primary_bars_path=str(primary),
                reference_bars_path=str(reference),
                settlement_currency="USDT",
                primary_initial_cash_raw=100_000_000_000_000,
                reference_initial_cash_raw=100_000_000_000_000,
                output_dir=str(output / "multileg"),
                strategy=qx.BuiltinStrategySpec(
                    kind="pairs_arbitrage", strategy_id="wheel-smoke-pairs"
                ),
            )
        )
        assert len(multileg.primary_result_hash) == len(multileg.reference_result_hash) == 16

        depth_result = qx.run_depth_backtest(
            qx.DepthBacktestSpec(
                run_id="wheel-smoke-depth",
                depth_path=str(depth),
                tier="l2",
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(output / "depth"),
                strategy=base_strategy,
            )
        )
        assert len(depth_result.result_hash) == 16
        overflow_depth_runs = tuple(
            qx.DepthBacktestSpec(
                run_id=f"wheel-smoke-depth-overflow-{index}",
                depth_path=str(depth),
                tier="l2",
                settlement_currency="USDT",
                initial_cash_raw=(1 << 127) - 1 if index == 0 else 1,
                output_dir=str(output / f"depth-overflow-{index}"),
                strategy=base_strategy,
                account_id=f"wheel-smoke-depth-overflow-account-{index}",
            )
            for index in range(2)
        )
        try:
            qx.run_multi_account_depth_backtest(
                qx.MultiAccountDepthBacktestSpec(
                    batch_id="wheel-smoke-depth-overflow-preflight",
                    runs=overflow_depth_runs,
                )
            )
        except qx.InvalidInputError as error:
            assert error.payload["category"] == "INVALID_INPUT", error.payload
        else:
            raise AssertionError("multi-account depth cash overflow must fail before execution")

        class BuyOnce:
            emitted = False

            def on_event(self, context: dict, event: dict) -> dict:
                bar = event["Bar"]
                intents = []
                if not self.emitted:
                    self.emitted = True
                    intents.append(
                        {
                            "intent_id": 1,
                            "instrument": bar["instrument"],
                            "side": "Buy",
                            "qty": 1_000_000,
                            "limit": None,
                            "policy": None,
                            "reduce_only": False,
                            "post_only": False,
                        }
                    )
                return {
                    "schema_version": 1,
                    "request_id": f"{context['strategy_id']}:{bar['ts']}",
                    "strategy_id": context["strategy_id"],
                    "signal_id": bar["ts"],
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": intents,
                }

        custom = qx.run_backtest_with_strategy(
            qx.BacktestSpec(
                run_id="wheel-smoke-custom-strategy",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(bars),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(output / "custom"),
                strategy=qx.BuiltinStrategySpec(
                    kind="sma_cross", strategy_id="wheel-smoke-custom"
                ),
            ),
            BuyOnce(),
            strategy_id="wheel-smoke-custom",
            strategy_version="wheel-smoke-v1",
            source_sha256="a" * 64,
        )
        assert custom.fills > 0
        assert qx.verify_run(custom).verified

        handle = qx.start_backtest(
            qx.BacktestSpec(
                run_id="wheel-smoke-run-handle",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(bars),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(output / "run-handle"),
                strategy=base_strategy,
            )
        )
        assert handle.wait(timeout_ms=30_000) == "succeeded"
        handle_result = handle.result()
        assert handle_result is not None and qx.verify_run(handle_result).verified
        events = handle.events()
        assert events and events[-1].kind == "succeeded", events

        print(
            "wheel e2e "
            f"strategies={len(strategies)} rows={verdict.rows} fills={outcome.fills} "
            f"ashare_fills={ashare_outcome.fills} "
            f"classic_selection={classic_selection.succeeded_candidates}/{classic_selection.total_candidates} "
            f"parameter_grid={experiment.succeeded_candidates}/{experiment.total_candidates} "
            f"accounts={accounts.summary['accounts_succeeded']} "
            "account_overflow_preflight=True depth_account_overflow_preflight=True "
            f"multileg={multileg.primary_fills}/{multileg.reference_fills} "
            f"depth_fills={depth_result.fills} custom_fills={custom.fills} "
            f"run_handle={events[-1].status} verified={verification.verified}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
