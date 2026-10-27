import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing import cli
from qianxing_bridge.app import _PythonBacktestCallback


class StrategyFileCliTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.strategy_path = self.root / "my_strategy.py"
        self.strategy_path.write_text(
            "STRATEGY_ID = 'my-strategy'\n"
            "STRATEGY_VERSION = 'test-v1'\n"
            "def create_strategy(spec):\n"
            "    return {'candidate': spec}\n",
            encoding="utf-8",
        )
        self.input_path = self.root / "spec.json"

    def tearDown(self):
        self.temp.cleanup()

    def _write_input(self, value):
        self.input_path.write_text(json.dumps(value), encoding="utf-8")

    def test_run_strategy_loads_file_and_binds_its_source_digest(self):
        self._write_input({"run_id": "run-1", "strategy": {"fast_window": 3}})
        output = io.StringIO()
        with patch.object(cli, "run_backtest_with_strategy", return_value={"verified": True}) as run:
            with contextlib.redirect_stdout(output):
                code = cli.main(["run-strategy", str(self.strategy_path), str(self.input_path)])

        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue()), {"verified": True})
        payload, strategy = run.call_args.args
        self.assertEqual(payload["run_id"], "run-1")
        self.assertEqual(strategy, {"candidate": payload})
        self.assertEqual(run.call_args.kwargs["strategy_id"], "my-strategy")
        self.assertEqual(run.call_args.kwargs["strategy_version"], "test-v1")
        self.assertEqual(len(run.call_args.kwargs["source_sha256"]), 64)

    def test_run_strategy_accepts_class_only_strategy_file(self):
        self.strategy_path.write_text(
            "from qianxing_strategy import BaseStrategy\n"
            "STRATEGY_ID = 'class-only'\n"
            "STRATEGY_VERSION = '1'\n"
            "class ClassOnly(BaseStrategy):\n"
            "    def __init__(self, spec):\n"
            "        self.spec = spec\n"
            "STRATEGY_CLASS = ClassOnly\n",
            encoding="utf-8",
        )
        self._write_input({"run_id": "class-only-run", "strategy": {"fast_window": 3}})
        output = io.StringIO()
        with patch.object(cli, "run_backtest_with_strategy", return_value={"verified": True}) as run:
            with contextlib.redirect_stdout(output):
                code = cli.main(["run-strategy", str(self.strategy_path), str(self.input_path)])

        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue()), {"verified": True})
        self.assertEqual(run.call_args.args[1].spec["run_id"], "class-only-run")

    def test_run_strategy_routes_l1_depth_spec_to_local_python_callback(self):
        payload = {
            "schema_version": 1,
            "run_id": "tick-run",
            "depth_path": "ticks.jsonl",
            "tier": "l1",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": "runs/tick-run",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "my-strategy",
                "quantity_raw": 1,
                "fast_window": 2,
                "slow_window": 4,
            },
        }
        self._write_input(payload)
        output = io.StringIO()
        result = {"run_id": "tick-run", "verified": True}
        with patch.object(cli, "validate_strategy_execution_preflight_v1", return_value={}):
            with patch.object(cli, "run_depth_backtest_with_strategy", return_value=result) as run:
                with contextlib.redirect_stdout(output):
                    code = cli.main(
                        ["run-strategy", str(self.strategy_path), str(self.input_path)]
                    )

        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue()), result)
        passed_payload, strategy = run.call_args.args
        self.assertEqual(passed_payload["tier"], "l1")
        self.assertEqual(strategy, {"candidate": payload})
        self.assertEqual(run.call_args.kwargs["strategy_id"], "my-strategy")
        self.assertEqual(len(run.call_args.kwargs["source_sha256"]), 64)

    def test_run_strategy_rejects_depth_manifest_level_mismatch(self):
        self._write_input(
            {
                "schema_version": 1,
                "run_id": "book-run",
                "depth_path": "books.jsonl",
                "tier": "l2",
                "settlement_currency": "USDT",
                "initial_cash_raw": 100_000,
                "latency_snapshots": 0,
                "queue_position_bps": 0,
                "market_impact_bps": 0,
                "output_dir": "runs/book-run",
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": "my-strategy",
                    "quantity_raw": 1,
                    "fast_window": 2,
                    "slow_window": 4,
                },
            }
        )
        manifest = {
            "strategy_id": "my-strategy",
            "strategy_version": "test-v1",
            "data_level": "tick",
        }
        stderr = io.StringIO()
        with patch.object(cli, "build_module_manifest", return_value=manifest):
            with patch.object(cli, "validate_strategy_execution_preflight_v1", return_value={}):
                with patch.object(cli, "run_depth_backtest_with_strategy") as run:
                    with contextlib.redirect_stderr(stderr):
                        code = cli.main(
                            ["run-strategy", str(self.strategy_path), str(self.input_path)]
                        )

        self.assertEqual(code, 2)
        self.assertIn("does not match depth tier", json.loads(stderr.getvalue())["message"])
        run.assert_not_called()

    def test_search_strategy_routes_depth_grid_through_fresh_file_factory(self):
        payload = {
            "schema_version": 1,
            "experiment_id": "depth-grid",
            "base": {
                "schema_version": 1,
                "run_id": "depth-base",
                "depth_path": "depth.json",
                "tier": "l1",
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": "my-strategy",
                    "quantity_raw": 1,
                    "fast_window": 3,
                    "slow_window": 8,
                },
            },
            "parameter_space": [
                {"name": "fast_window", "values": [2, 3]},
                {"name": "slow_window", "values": [8, 10]},
            ],
        }
        self._write_input(payload)
        result = {
            "experiment_id": "depth-grid",
            "total_candidates": 4,
            "succeeded_candidates": 4,
            "failed_candidates": 0,
            "candidates": [{"ordinal": 2, "parameters": {"fast_window": 3}}],
            "ranking": [{"rank": 1, "ordinal": 2, "return_bps": 12}],
            "artifact_path": "runs/depth-grid.json",
        }
        output = io.StringIO()
        errors = io.StringIO()

        def fake_search(spec, factory, **identity):
            self.assertEqual(identity["strategy_id"], "my-strategy")
            candidate = {
                **spec["base"],
                "strategy": {**spec["base"]["strategy"], "fast_window": 2},
            }
            self.assertEqual(factory(candidate), {"candidate": candidate})
            return result

        with patch.object(cli, "validate_strategy_execution_preflight_v1", return_value={}):
            with patch.object(cli, "run_depth_experiment_with_strategy", side_effect=fake_search):
                with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                    code = cli.main(
                        [
                            "search-strategy",
                            str(self.strategy_path),
                            str(self.input_path),
                            "--summary",
                        ]
                    )

        self.assertEqual(code, 0)
        summary = json.loads(output.getvalue())
        self.assertEqual(summary["candidate_count"], 4)
        self.assertEqual(summary["top_candidates"][0]["parameters"], {"fast_window": 3})
        self.assertEqual(summary["ranking_scope"], "full_input_in_sample")
        self.assertIn("样本内候选排序", errors.getvalue())

    def test_manifest_warmup_factory_binds_requirement_to_every_instance(self):
        class Strategy:
            def history_requirements(self):
                return {"warmup_bars": 5}

        factory = cli._bind_manifest_warmup_factory(lambda _spec: Strategy(), 5)
        first = factory({"candidate": 1})
        second = factory({"candidate": 2})

        from qianxing_strategy.base import StrategyWarmupBinding

        self.assertIsInstance(first, StrategyWarmupBinding)
        self.assertIsInstance(second, StrategyWarmupBinding)
        self.assertEqual(first.warmup_bars, 5)
        self.assertEqual(second.warmup_bars, 5)
        self.assertIsNot(first.strategy, second.strategy)

        conflict = cli._bind_manifest_warmup_factory(
            lambda _spec: Strategy(), 6
        )({})
        self.assertFalse(conflict.history_requirements_checked)
        callback = _PythonBacktestCallback(conflict)
        context = {
            "strategy_id": "warmup-test",
            "strategy_version": "1",
            "as_of": 100,
            "positions": {},
            "cash": {},
        }
        with self.assertRaisesRegex(ValueError, "conflicts"):
            callback._qx_on_init(json.dumps(context))

    def test_run_strategy_injects_fresh_instances_into_multi_market_slots(self):
        payload = {
            "run_id": "portfolio-run",
            "markets": [
                {
                    "instrument": "BTCUSDT.BINANCE",
                    "strategies": [
                        {"strategy": {"strategy_id": "btc-slot"}},
                        {"strategy": {"strategy_id": "btc-secondary"}},
                    ],
                },
                {
                    "instrument": "ETHUSDT.OKX",
                    "strategies": [{"strategy": {"strategy_id": "eth-slot"}}],
                },
            ],
        }
        self._write_input(payload)
        output = io.StringIO()
        result = {"markets_total": 2, "replay_verified": True}
        with patch.object(
            cli, "run_multi_market_backtest_with_strategy_slots", return_value=result
        ) as run:
            with contextlib.redirect_stdout(output):
                code = cli.main(
                    ["run-strategy", str(self.strategy_path), str(self.input_path)]
                )

        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue()), result)
        passed_payload, bindings = run.call_args.args
        self.assertEqual(passed_payload["run_id"], "portfolio-run")
        self.assertEqual(
            [binding["strategy_id"] for binding in bindings],
            ["btc-slot", "btc-secondary", "eth-slot"],
        )
        self.assertTrue(all(binding["strategy_version"] == "test-v1" for binding in bindings))
        self.assertEqual(len({binding["source_sha256"] for binding in bindings}), 1)
        strategies = [binding["strategy"] for binding in bindings]
        self.assertEqual(
            [strategy["candidate"]["strategy"]["strategy_id"] for strategy in strategies],
            ["btc-slot", "btc-secondary", "eth-slot"],
        )
        self.assertEqual(
            [strategy["candidate"]["market"]["instrument"] for strategy in strategies],
            ["BTCUSDT.BINANCE", "BTCUSDT.BINANCE", "ETHUSDT.OKX"],
        )
        self.assertTrue(all(strategy["candidate"]["run_id"] == "portfolio-run" for strategy in strategies))
        self.assertIsNot(strategies[0], strategies[1])

    def test_run_strategy_can_select_one_multi_market_slot(self):
        self._write_input(
            {
                "run_id": "portfolio-run",
                "markets": [
                    {"strategies": [{"strategy": {"strategy_id": "slot-a"}}]},
                    {"strategies": [{"strategy": {"strategy_id": "slot-b"}}]},
                ],
            }
        )
        with patch.object(
            cli, "run_multi_market_backtest_with_strategy_slots", return_value={"ok": True}
        ) as run:
            with contextlib.redirect_stdout(io.StringIO()):
                code = cli.main(
                    [
                        "run-strategy",
                        str(self.strategy_path),
                        str(self.input_path),
                        "--strategy-id",
                        "slot-b",
                    ]
                )

        self.assertEqual(code, 0)
        self.assertEqual([binding["strategy_id"] for binding in run.call_args.args[1]], ["slot-b"])

    def test_run_strategy_rejects_unknown_multi_market_slot(self):
        self._write_input(
            {"run_id": "portfolio-run", "markets": [{"strategies": []}]}
        )
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            code = cli.main(
                [
                    "run-strategy",
                    str(self.strategy_path),
                    str(self.input_path),
                    "--strategy-id",
                    "missing-slot",
                ]
            )
        self.assertEqual(code, 2)
        self.assertIn("does not match", json.loads(stderr.getvalue())["message"])

    def test_strategy_bundle_digest_changes_when_declared_helper_changes(self):
        helper = self.root / "indicators.py"
        helper.write_text("WINDOW = 5\n", encoding="utf-8")
        self.strategy_path.write_text(
            "STRATEGY_DEPENDENCIES = ('indicators.py',)\n"
            "def create_strategy(spec):\n"
            "    return spec\n",
            encoding="utf-8",
        )
        _, _, first_digest = cli._load_strategy_file(str(self.strategy_path))

        helper.write_text("WINDOW = 8\n", encoding="utf-8")
        _, _, second_digest = cli._load_strategy_file(str(self.strategy_path))

        self.assertNotEqual(first_digest, second_digest)

    def test_strategy_bundle_rejects_dependency_paths_outside_strategy_directory(self):
        self.strategy_path.write_text(
            "STRATEGY_DEPENDENCIES = ('../outside.py',)\n"
            "def create_strategy(spec):\n"
            "    return spec\n",
            encoding="utf-8",
        )

        with self.assertRaises(cli.InvalidInputError):
            cli._load_strategy_file(str(self.strategy_path))

    def test_strategy_file_can_export_a_base_strategy_class_without_factory(self):
        self.strategy_path.write_text(
            "from qianxing_strategy import BaseStrategy\n"
            "class ClassOnly(BaseStrategy):\n"
            "    def __init__(self, spec):\n"
            "        self.received_spec = spec\n"
            "STRATEGY_CLASS = ClassOnly\n",
            encoding="utf-8",
        )

        _module, factory, _digest = cli._load_strategy_file(str(self.strategy_path))
        supplied_spec = {"strategy": {"fast_window": 5}}
        first = factory(supplied_spec)
        second = factory(supplied_spec)

        from qianxing_strategy import BaseStrategy

        self.assertIsInstance(first, BaseStrategy)
        self.assertIsNot(first, second)
        self.assertIs(first.received_spec, supplied_spec)

    def test_strategy_class_without_spec_constructor_is_supported(self):
        self.strategy_path.write_text(
            "from qianxing_strategy import BaseStrategy\n"
            "class ClassOnly(BaseStrategy):\n"
            "    pass\n"
            "STRATEGY_CLASS = ClassOnly\n",
            encoding="utf-8",
        )

        _module, factory, _digest = cli._load_strategy_file(str(self.strategy_path))
        from qianxing_strategy import BaseStrategy

        self.assertIsInstance(factory({"strategy": {}}), BaseStrategy)

    def test_strategy_class_export_must_use_the_standard_base(self):
        self.strategy_path.write_text(
            "class NotAStandardStrategy:\n"
            "    pass\n"
            "STRATEGY_CLASS = NotAStandardStrategy\n",
            encoding="utf-8",
        )

        with self.assertRaisesRegex(cli.InvalidInputError, "BaseStrategy subclass"):
            cli._load_strategy_file(str(self.strategy_path))

    def test_tick_example_exposes_complete_class_manifest_and_fresh_factory(self):
        root = Path(__file__).resolve().parents[1]
        strategy_path = root / "examples" / "strategy_file_tick.py"
        module, factory, digest = cli._load_strategy_file(str(strategy_path))
        manifest = cli.build_module_manifest(module, strategy_path, digest)
        spec = json.loads(
            (root / "examples" / "strategy-file-tick-backtest.example.json").read_text(
                encoding="utf-8"
            )
        )
        first = factory(spec)
        second = factory(spec)

        from qianxing_strategy import BaseStrategy

        self.assertEqual(manifest["data_level"], "tick")
        self.assertEqual(manifest["warmup_bars"], 0)
        self.assertIsInstance(first, BaseStrategy)
        self.assertIsInstance(second, BaseStrategy)
        self.assertIsNot(first, second)
        self.assertEqual(len(manifest["bundle_sha256"]), 64)

    def test_tick_search_example_uses_a_bounded_depth_spec_and_declared_parameters(self):
        root = Path(__file__).resolve().parents[1]
        spec = json.loads(
            (root / "examples" / "strategy-file-tick-search.example.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(spec["base"]["tier"], "l1")
        self.assertEqual(
            {dimension["name"] for dimension in spec["parameter_space"]},
            {"fast_window", "slow_window"},
        )
        self.assertLess(
            sum(len(dimension["values"]) for dimension in spec["parameter_space"]), 256
        )

    def test_search_strategy_passes_a_fresh_candidate_factory(self):
        self._write_input(
            {
                "experiment_id": "exp-1",
                "base": {"strategy": {"fast_window": 3}},
                "parameter_space": [{"name": "fast_window", "values": [3, 5]}],
                "split_plan": {"train_bars": 20, "test_bars": 10},
            }
        )
        output = io.StringIO()

        def fake_search(spec, factory, **identity):
            candidate = {"strategy": {"fast_window": 5}}
            self.assertEqual(factory(candidate), {"candidate": candidate})
            self.assertEqual(identity["strategy_id"], "my-strategy")
            return {"completed_candidates": 2}

        with patch.object(cli, "run_experiment_with_strategy", side_effect=fake_search):
            with contextlib.redirect_stdout(output):
                code = cli.main(
                    ["search-strategy", str(self.strategy_path), str(self.input_path)]
                )

        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue()), {"completed_candidates": 2})

    def test_search_strategy_routes_multi_market_file_candidates(self):
        self._write_input(
            {
                "experiment_id": "multi-exp",
                "target_strategy_id": "slot-okx",
                "base": {
                    "markets": [
                        {
                            "instrument": "BTC/USDT:USDT.OKX",
                            "strategies": [
                                {
                                    "priority": 0,
                                    "reserved_capital_raw": 100,
                                    "strategy": {
                                        "strategy_id": "slot-okx",
                                        "fast_window": 3,
                                    },
                                }
                            ],
                        }
                    ]
                },
                "callback_parameter_space": [{"name": "risk_scale", "values": [7, 9]}],
            }
        )
        output = io.StringIO()
        errors = io.StringIO()
        candidate = {
            "run_id": "multi-exp-0001",
            "markets": [
                {
                    "instrument": "BTC/USDT:USDT.OKX",
                    "strategies": [
                        {
                            "priority": 0,
                            "reserved_capital_raw": 100,
                                        "strategy": {"strategy_id": "slot-okx", "fast_window": 3},
                        }
                    ],
                }
            ],
        }
        report = {
            "schema_version": 1,
            "experiment_id": "multi-exp",
            "target_strategy_id": "slot-okx",
            "spec_fingerprint": "ab" * 8,
            "total_candidates": 2,
            "completed_candidates": 2,
            "succeeded_candidates": 2,
            "failed_candidates": 0,
            "candidates": [
                {"ordinal": 1, "parameters": {"risk_scale": 7}},
            ],
            "ranking": [{
                "rank": 1,
                "ordinal": 1,
                "run_id": "multi-exp-0001",
                "return_bps": 150,
                "max_drawdown_bps": 20,
                "shared_result_hash": "cd" * 8,
            }],
            "artifact_path": "runs/multi-exp.json",
        }

        def fake_search(spec, strategy_factory, **identity):
            from qianxing import RunMultiMarketExperimentResult

            self.assertEqual(spec["experiment_id"], "multi-exp")
            self.assertEqual(identity["strategy_id"], "slot-okx")
            self.assertEqual(
                strategy_factory(candidate, {"risk_scale": 7}),
                {
                    "candidate": {
                        **candidate,
                        "market": candidate["markets"][0],
                        "strategy": candidate["markets"][0]["strategies"][0]["strategy"],
                        "strategy_slot": candidate["markets"][0]["strategies"][0],
                        "parameters": {"risk_scale": 7},
                    }
                },
            )
            return RunMultiMarketExperimentResult.from_dict(report)

        with patch.object(cli, "run_multi_market_experiment_with_strategy", side_effect=fake_search):
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                code = cli.main(
                    [
                        "search-strategy",
                        str(self.strategy_path),
                        str(self.input_path),
                        "--summary",
                    ]
                )

        self.assertEqual(code, 0)
        summary = json.loads(output.getvalue())
        self.assertEqual(summary["top_candidates"][0]["parameters"], {"risk_scale": 7})
        self.assertIn("in-sample", summary["note"])
        self.assertIn("split_plan", errors.getvalue())

    def test_multi_market_search_routes_adaptive_custom_ranges_and_summarizes_trials(self):
        self._write_input(
            {
                "experiment_id": "multi-exp",
                "target_strategy_id": "slot-okx",
                "base": {
                    "markets": [
                        {
                            "instrument": "BTC/USDT:USDT.OKX",
                            "strategies": [
                                {
                                    "strategy": {
                                        "strategy_id": "slot-okx",
                                        "fast_window": 3,
                                    }
                                }
                            ],
                        }
                    ]
                },
                "split_plan": {"train_bars": 20, "test_bars": 10},
                "search": {
                    "algorithm": "tpe",
                    "seed": 4,
                    "budget": 5,
                    "fold_local_search": True,
                    "continuous_parameter_space": [
                        {"name": "risk_scale", "min": 1, "max": 8, "step": 1}
                    ],
                },
            }
        )
        output = io.StringIO()
        candidate = {
            "markets": [
                {
                    "instrument": "BTC/USDT:USDT.OKX",
                    "strategies": [
                        {"strategy": {"strategy_id": "slot-okx", "fast_window": 3}}
                    ],
                }
            ]
        }
        report = {
            "schema_version": 1,
            "experiment_id": "multi-exp",
            "target_strategy_id": "slot-okx",
            "spec_fingerprint": "ab" * 8,
            "total_candidates": 5,
            "completed_candidates": 5,
            "succeeded_candidates": 5,
            "failed_candidates": 0,
            "candidates": [{"ordinal": 1, "parameters": {"risk_scale": 3}}],
            "ranking": [
                {
                    "rank": 1,
                    "ordinal": 1,
                    "run_id": "multi-exp-0001",
                    "return_bps": 10,
                    "max_drawdown_bps": 0,
                    "shared_result_hash": "cd" * 8,
                }
            ],
            "search": {
                "algorithm": "tpe",
                "objective": "return",
                "seed": 4,
                "budget": 5,
                "fold_local_search": True,
                "fold_searches": [
                    {
                        "fold": 1,
                        "seed": 4,
                        "train_range": [0, 20],
                        "test_range": [20, 30],
                        "trials": [
                            {
                                "ordinal": 5,
                                "proposal": "tpe_continuous",
                                "parameters": {"risk_scale": 3},
                                "training_score_bps": 10,
                            }
                        ],
                    }
                ],
            },
            "artifact_path": "runs/multi-exp.json",
        }

        def fake_search(spec, strategy_factory, **identity):
            self.assertEqual(spec["search"]["budget"], 5)
            self.assertEqual(identity["strategy_id"], "slot-okx")
            self.assertEqual(
                strategy_factory(candidate, {"risk_scale": 3})["candidate"]["parameters"],
                {"risk_scale": 3},
            )
            from qianxing import RunMultiMarketExperimentResult

            return RunMultiMarketExperimentResult.from_dict(report)

        with patch.object(cli, "run_multi_market_experiment_with_strategy", side_effect=fake_search):
            with contextlib.redirect_stdout(output):
                code = cli.main(
                    [
                        "search-strategy",
                        str(self.strategy_path),
                        str(self.input_path),
                        "--summary",
                    ]
                )
        self.assertEqual(code, 0)
        summary = json.loads(output.getvalue())
        self.assertEqual(summary["search"]["algorithm"], "tpe")
        self.assertEqual(
            summary["search"]["fold_searches"][0]["top_training_trials"][0]["proposal"],
            "tpe_continuous",
        )

    def test_multi_market_experiment_summary_includes_walk_forward_metrics(self):
        summary = cli._experiment_summary(
            {
                "experiment_id": "multi-exp",
                "target_strategy_id": "slot-okx",
                "total_candidates": 2,
                "succeeded_candidates": 2,
                "failed_candidates": 0,
                "candidates": [
                    {"ordinal": 2, "parameters": {"risk_scale": 9}},
                ],
                "ranking": [
                    {"rank": 1, "ordinal": 2, "return_bps": 120},
                ],
                "out_of_sample": {
                    "in_sample_return_bps": 100,
                    "out_of_sample_return_bps": 45,
                    "degradation_bps": 55,
                    "cross_fold_robustness": {"folds_evaluated": 2},
                    "folds": [
                        {
                            "fold": 1,
                            "selected_ordinal": 2,
                            "selected_parameters": {"risk_scale": 9},
                            "in_sample_return_bps": 100,
                            "out_of_sample_return_bps": 45,
                        }
                    ],
                },
                "artifact_path": "runs/multi-exp.json",
            }
        )
        self.assertEqual(summary["ranking_scope"], "first_training_fold")
        self.assertEqual(summary["top_candidates"][0]["parameters"], {"risk_scale": 9})
        self.assertEqual(summary["out_of_sample"]["out_of_sample_return_bps"], 45)
        self.assertEqual(
            summary["out_of_sample"]["fold_selections"][0]["parameters"],
            {"risk_scale": 9},
        )

    def test_search_requires_at_least_one_parameter_dimension(self):
        self._write_input({"experiment_id": "exp-1", "base": {}, "parameter_space": []})
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            code = cli.main(["search-strategy", str(self.strategy_path), str(self.input_path)])
        self.assertEqual(code, 2)
        self.assertIn("parameter_space", json.loads(stderr.getvalue())["message"])

    def test_search_summary_surfaces_top_parameters_and_held_out_metrics(self):
        report = {
            "experiment_id": "exp-1",
            "total_candidates": 2,
            "succeeded_candidates": 2,
            "failed_candidates": 0,
            "candidates": [
                {
                    "ordinal": 1,
                    "parameters": {"fast_window": 3},
                    "outcome": {
                        "run_id": "run-1",
                        "fills": 4,
                        "return_bps": 100,
                        "max_drawdown_bps": 30,
                    },
                }
            ],
            "comparison": {
                "runs": [
                    {
                        "rank": 1,
                        "run": {
                            "run_id": "run-1",
                            "return_bps": 100,
                            "max_drawdown_bps": 30,
                        },
                    }
                ]
            },
            "out_of_sample": {
                "in_sample_return_bps": 100,
                "out_of_sample_return_bps": 45,
                "degradation_bps": 55,
                "cross_fold_robustness": None,
                "folds": [
                    {
                        "fold": 1,
                        "selected_ordinal": 1,
                        "selected_parameters": {"fast_window": 3},
                        "in_sample_return_bps": 100,
                        "out_of_sample_return_bps": 45,
                    }
                ],
            },
            "baselines": [],
            "cost_sensitivity": [],
            "search": {"neighborhood": {"assessment": "stable_positive"}},
            "artifact_path": "runs/exp-1",
        }
        summary = cli._experiment_summary(report)
        self.assertEqual(
            summary["top_candidates_by_comparison"][0]["parameters"],
            {"fast_window": 3},
        )
        self.assertEqual(summary["out_of_sample"]["out_of_sample_return_bps"], 45)
        self.assertEqual(summary["parameter_neighborhood_assessment"], "stable_positive")


if __name__ == "__main__":
    unittest.main()
