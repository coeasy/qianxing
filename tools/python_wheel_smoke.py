#!/usr/bin/env python3
"""Exercise the public Python wheel against shared Rust use cases after clean install."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

import qianxing as qx


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
    strategies = qx.list_builtin_strategies()
    assert len(strategies) == 17, len(strategies)
    verdict = qx.validate_dataset(qx.DatasetSpec("wheel-smoke-data", str(bars)))
    assert verdict.usable and verdict.rows == 70, verdict

    with tempfile.TemporaryDirectory(prefix="qianxing-wheel-smoke-") as temporary:
        output = Path(temporary)
        base_strategy = qx.BuiltinStrategySpec(
            kind="sma_cross", strategy_id="wheel-smoke-sma", fast_window=2, slow_window=4
        )
        base = qx.BacktestSpec(
            run_id="wheel-smoke-backtest",
            instrument="BTCUSDT.BINANCE",
            bars_path=str(bars),
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000_000,
            output_dir=str(output / "base"),
            strategy=base_strategy,
        )
        outcome = qx.run_backtest(base)
        verification = qx.verify_run(outcome)
        assert verification.verified, verification.mismatches

        experiment = qx.run_experiment(
            qx.RunExperimentSpec(
                experiment_id="wheel-smoke-selection",
                base=base,
                parameter_space=(qx.ExperimentParameterSpace("fast_window", (2, 3)),),
                strategy_kinds=("ema_cross", "sma_cross"),
            )
        )
        assert experiment.total_candidates == experiment.succeeded_candidates == 4

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
            f"selection={experiment.succeeded_candidates}/{experiment.total_candidates} "
            f"accounts={accounts.summary['accounts_succeeded']} "
            f"multileg={multileg.primary_fills}/{multileg.reference_fills} "
            f"depth_fills={depth_result.fills} custom_fills={custom.fills} "
            f"run_handle={events[-1].status} verified={verification.verified}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
