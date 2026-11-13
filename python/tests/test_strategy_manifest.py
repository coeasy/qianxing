import contextlib
import hashlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing import cli, strategy_execution
from qianxing.strategy_manifest import (
    apply_parameter_defaults,
    build_module_manifest,
    declared_config_validator,
    resolve_strategy_parameters,
)


class PythonStrategyManifestTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.strategy_path = self.root / "strategy.py"
        self.spec_path = self.root / "experiment.json"
        self.bars_path = self.root / "validated-bars.json"
        self._write_bars(50)
        self._write_strategy()

    def tearDown(self):
        self.temp.cleanup()

    def _write_strategy(self, manifest="""{
        'schema_version': 1,
        'strategy_id': 'manifest-sma',
        'strategy_version': '2.1',
        'parameters': {
            'fast': {'type': 'integer', 'minimum': 2, 'maximum': 10, 'default': 5},
            'slow': {'type': 'integer', 'minimum': 10, 'maximum': 40, 'default': 20},
        },
        'instrument_requirements': {
            'asset_classes': ['crypto'], 'products': ['spot'], 'venues': ['BINANCE'],
            'requires_explicit_spec': True,
        },
        'data_level': 'bar',
        'warmup_bars': 40,
        'dependencies': [],
    }"""):
        source = (
            "STRATEGY_ID = 'manifest-sma'\n"
            "STRATEGY_VERSION = '2.1'\n"
            f"STRATEGY_MANIFEST = {manifest}\n"
            "def create_strategy(spec):\n"
            "    return {'spec': spec}\n"
        )
        self.strategy_path.write_text(source, encoding="utf-8")

    def _write_class_strategy(self):
        source = '''from qianxing_strategy import BaseStrategy

class SmaCross(BaseStrategy):
    STRATEGY_ID = "class-sma"
    STRATEGY_VERSION = "3.0"
    STRATEGY_MANIFEST = {
        "schema_version": 1,
        "strategy_id": STRATEGY_ID,
        "strategy_version": STRATEGY_VERSION,
        "parameters": {
            "fast_window": {"type": "integer", "minimum": 2, "maximum": 30, "default": 5},
            "slow_window": {"type": "integer", "minimum": 10, "maximum": 40, "default": 20},
        },
        "instrument_requirements": {
            "asset_classes": ["crypto"], "products": ["spot"], "venues": ["BINANCE"],
            "requires_explicit_spec": True,
        },
        "data_level": "bar",
        "warmup_bars": 40,
        "dependencies": [],
    }

    @staticmethod
    def validate_config(config):
        if config["fast_window"] >= config["slow_window"]:
            raise ValueError("fast_window must be below slow_window")

    def __init__(self, spec):
        self.spec = spec

STRATEGY_CLASS = SmaCross

def create_strategy(spec):
    return SmaCross(spec)
'''
        self.strategy_path.write_text(source, encoding="utf-8")

    def _write_experiment(self, values):
        payload = {
            "experiment_id": "manifest-search",
            "base": self._backtest_base(),
            "parameter_space": [{"name": "fast", "values": values}],
            "split_plan": {"train_bars": 41, "test_bars": 5},
        }
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")

    def _write_bars(self, rows):
        self.bars_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "instrument": "BTCUSDT.BINANCE",
                    "source": "strategy-manifest-test",
                    "ts": [1000 + 1000 * index for index in range(rows)],
                    "open_raw": [100_000_000_000] * rows,
                    "high_raw": [101_000_000_000] * rows,
                    "low_raw": [99_000_000_000] * rows,
                    "close_raw": [100_000_000_000] * rows,
                    "volume_raw": [1_000_000_000] * rows,
                }
            ),
            encoding="utf-8",
        )

    def _backtest_base(self):
        return {
            "schema_version": 1,
            "run_id": "manifest-run",
            "instrument": "BTCUSDT.BINANCE",
            "asset_class": "crypto",
            "bars_path": str(self.bars_path),
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000,
            "output_dir": "runs/manifest-test",
            "strategy": {"strategy_id": "manifest-sma", "fast": 5},
            "market_spec": {
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "product": "spot",
            },
            "fee_schedule": {"maker_fee_bps": 10, "taker_fee_bps": 10},
        }

    def test_inspect_strategy_emits_schema_bound_hashes_and_requirements(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = cli.main(["inspect-strategy", str(self.strategy_path)])

        self.assertEqual(code, 0)
        manifest = json.loads(output.getvalue())
        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual(manifest["kind"], "qianxing-python-strategy-manifest")
        self.assertEqual(manifest["strategy_id"], "manifest-sma")
        self.assertEqual(manifest["strategy_version"], "2.1")
        self.assertEqual(manifest["data_level"], "bar")
        self.assertEqual(manifest["warmup_bars"], 40)
        self.assertEqual(manifest["instrument_requirements"]["products"], ["spot"])
        self.assertEqual(manifest["completeness"], "complete")
        self.assertEqual(
            manifest["source_sha256"],
            hashlib.sha256(self.strategy_path.read_bytes()).hexdigest(),
        )
        self.assertEqual(len(manifest["bundle_sha256"]), 64)
        if importlib.util.find_spec("jsonschema") is not None:
            import jsonschema

            schema = json.loads(
                (Path(__file__).resolve().parents[2] / "schemas" / "python-strategy-manifest-v1.json")
                .read_text(encoding="utf-8")
            )
            jsonschema.Draft202012Validator(schema).validate(manifest)

    def test_runtime_timer_data_level_is_part_of_the_manifest_contract(self):
        source = self.strategy_path.read_text(encoding="utf-8")
        source = source.replace("'data_level': 'bar'", "'data_level': 'timer'")
        source = source.replace("'warmup_bars': 40", "'warmup_bars': 0")
        self.strategy_path.write_text(source, encoding="utf-8")

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = cli.main(["inspect-strategy", str(self.strategy_path)])

        self.assertEqual(code, 0)
        manifest = json.loads(output.getvalue())
        self.assertEqual(manifest["data_level"], "timer")
        self.assertEqual(manifest["warmup_bars"], 0)
        if importlib.util.find_spec("jsonschema") is not None:
            import jsonschema

            schema = json.loads(
                (Path(__file__).resolve().parents[2] / "schemas" / "python-strategy-manifest-v1.json")
                .read_text(encoding="utf-8")
            )
            jsonschema.Draft202012Validator(schema).validate(manifest)

    def test_inspect_strategy_reads_manifest_from_exported_strategy_class(self):
        self._write_class_strategy()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = cli.main(["inspect-strategy", str(self.strategy_path)])

        self.assertEqual(code, 0)
        manifest = json.loads(output.getvalue())
        self.assertEqual(manifest["strategy_id"], "class-sma")
        self.assertEqual(manifest["strategy_version"], "3.0")
        self.assertEqual(manifest["parameters"]["slow_window"]["default"], 20)
        self.assertEqual(manifest["warmup_bars"], 40)
        self.assertEqual(manifest["completeness"], "complete")
        if importlib.util.find_spec("jsonschema") is not None:
            import jsonschema

            schema = json.loads(
                (Path(__file__).resolve().parents[2] / "schemas" / "python-strategy-manifest-v1.json")
                .read_text(encoding="utf-8")
            )
            jsonschema.Draft202012Validator(schema).validate(manifest)

    def test_class_manifest_defaults_and_validator_are_used_before_run(self):
        self._write_class_strategy()
        payload = self._backtest_base()
        payload["strategy"] = {"strategy_id": "class-sma", "fast_window": 5}
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy", return_value={"ok": True}) as run:
            with contextlib.redirect_stdout(output):
                code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 0)
        strategy = run.call_args.args[1].strategy
        self.assertEqual(strategy.spec["strategy"]["fast_window"], 5)
        self.assertEqual(strategy.spec["strategy"]["slow_window"], 20)

    def test_repository_sma_example_uses_class_manifest_and_typed_bar_hook(self):
        from qianxing_strategy import StrategyDataPortal

        example_path = (
            Path(__file__).resolve().parents[2]
            / "python"
            / "examples"
            / "strategy_file_sma.py"
        )
        module, factory, bundle_sha256 = cli._load_strategy_file(str(example_path))
        manifest = build_module_manifest(module, example_path, bundle_sha256)
        spec = {
            "strategy": {
                "strategy_id": "file-sma-cross",
                "quantity_raw": 1_000_000_000,
                "fast_window": 2,
                "slow_window": 3,
            }
        }
        strategy = factory(spec)
        history = {
            "BTCUSDT.BINANCE": [{
                "ts": 1,
                "open_raw": 100_000_000_000,
                "high_raw": 100_000_000_000,
                "low_raw": 100_000_000_000,
                "close_raw": 100_000_000_000,
                "volume_raw": 1_000_000_000,
            }]
        }
        decision = strategy.on_event(
            {
                "strategy_id": "file-sma-cross",
                "positions": {},
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "data": StrategyDataPortal(history, as_of=1),
            },
            {
                "Bar": {
                    "ts": 1,
                    "close_raw": 100_000_000_000,
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                }
            },
        )

        self.assertEqual(manifest["completeness"], "complete")
        self.assertEqual(manifest["warmup_bars"], 40)
        self.assertEqual(strategy.history_requirements(), {"warmup_bars": 40})
        self.assertEqual(decision["strategy_id"], "file-sma-cross")
        self.assertEqual(decision["intents"], [])

    def test_class_manifest_strategy_runs_through_the_rust_backtest(self):
        self._write_class_strategy()
        payload = self._backtest_base()
        payload["run_id"] = "class-manifest-engine-run"
        payload["output_dir"] = str(self.root / "class-manifest-engine-run")
        payload["seed"] = 7
        payload["strategy"] = {
            "kind": "sma_cross",
            "strategy_id": "class-sma",
            "quantity_raw": 1_000_000_000,
            "fast_window": 5,
            "slow_window": 20,
        }
        payload["market_spec"] = {
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "product": "spot",
            "base_currency": "BTC",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size": 1_000_000_000,
            "linear": True,
            "inverse": False,
            "price_tick": 1_000_000,
            "qty_step": 1_000_000,
            "min_qty": 1_000_000,
            "max_leverage": 1,
            "maintenance_margin_bps": 0,
            "valid_from": 0,
            "valid_to": None,
        }
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 0)
        result = json.loads(output.getvalue())
        self.assertEqual(result["run_id"], payload["run_id"])
        self.assertEqual(result["instrument"], "BTCUSDT.BINANCE")
        self.assertEqual(result["equity_points"], 50)

    def test_tick_base_strategy_file_runs_through_the_rust_depth_engine(self):
        root = Path(__file__).resolve().parents[1]
        strategy_path = root / "examples" / "strategy_file_tick.py"
        depth_path = self.root / "depth-frame.json"
        snapshots = []
        scale = 1_000_000_000
        for index in range(48):
            phase = index % 20
            mid = 100 + (phase * 2 if phase < 10 else (20 - phase) * 2)
            snapshots.append(
                {
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "ts": 1000 + index * 1000,
                    "sequence": index + 1,
                    "bids": [{"price": (mid - 1) * scale, "qty": 100 * scale}],
                    "asks": [{"price": (mid + 1) * scale, "qty": 100 * scale}],
                }
            )
        depth_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "source": "strategy-file-cli-tick-test",
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "snapshots": snapshots,
                }
            ),
            encoding="utf-8",
        )
        spec = json.loads(
            (root / "examples" / "strategy-file-tick-backtest.example.json").read_text(
                encoding="utf-8"
            )
        )
        spec["run_id"] = "file-tick-rust-engine"
        spec["depth_path"] = str(depth_path)
        spec["output_dir"] = str(self.root / "file-tick-runs")
        self.spec_path.write_text(json.dumps(spec), encoding="utf-8")
        output = io.StringIO()

        with contextlib.redirect_stdout(output):
            code = cli.main(["run-strategy", str(strategy_path), str(self.spec_path)])

        self.assertEqual(code, 0)
        result = json.loads(output.getvalue())
        self.assertEqual(result["tier"], "l1")
        self.assertEqual(result["run_id"], "file-tick-rust-engine")
        self.assertEqual(result["equity_points"], 48)
        self.assertGreater(result["fills"], 0)

        search_spec = json.loads(
            (root / "examples" / "strategy-file-tick-search.example.json").read_text(
                encoding="utf-8"
            )
        )
        search_spec["experiment_id"] = "file-tick-rust-grid"
        search_spec["base"]["run_id"] = "file-tick-rust-grid-base"
        search_spec["base"]["depth_path"] = str(depth_path)
        search_spec["base"]["output_dir"] = str(self.root / "file-tick-grid-runs")
        search_path = self.root / "tick-search.json"
        search_path.write_text(json.dumps(search_spec), encoding="utf-8")
        search_output = io.StringIO()
        with contextlib.redirect_stdout(search_output), contextlib.redirect_stderr(io.StringIO()):
            search_code = cli.main(
                ["search-strategy", str(strategy_path), str(search_path), "--summary"]
            )

        self.assertEqual(search_code, 0)
        summary = json.loads(search_output.getvalue())
        self.assertEqual(summary["candidate_count"], 9)
        self.assertEqual(summary["succeeded_candidates"], 9)
        self.assertEqual(summary["ranking_scope"], "full_input_in_sample")
        self.assertTrue(summary["top_candidates"])

    def test_class_validator_runs_for_each_search_candidate_factory_call(self):
        self._write_class_strategy()
        module, factory, bundle_sha256 = cli._load_strategy_file(self.strategy_path)
        manifest = build_module_manifest(module, self.strategy_path, bundle_sha256)
        candidate_factory = cli._bind_manifest_config_factory(
            factory, module, manifest, "class-sma"
        )
        candidate_spec = self._backtest_base()
        candidate_spec["strategy"] = {"strategy_id": "class-sma", "fast_window": 5}

        with self.assertRaisesRegex(ValueError, "fast_window must be below slow_window"):
            candidate_factory(candidate_spec, {"fast_window": 25})

    def test_class_validator_rejects_incompatible_params_before_preflight(self):
        self._write_class_strategy()
        payload = self._backtest_base()
        payload["strategy"] = {
            "strategy_id": "class-sma",
            "fast_window": 25,
            "slow_window": 20,
        }
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        error = io.StringIO()
        with patch.object(strategy_execution, "validate_strategy_execution_preflight_v1") as preflight:
            with patch.object(strategy_execution, "run_backtest_with_strategy") as run:
                with contextlib.redirect_stderr(error):
                    code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(preflight.called)
        self.assertFalse(run.called)
        self.assertIn("fast_window must be below slow_window", error.getvalue())

    def test_manifest_defaults_bind_only_to_the_selected_market_slot(self):
        manifest = {
            "parameters": {
                "fast": {"type": "integer", "default": 5},
                "slow": {"type": "integer", "default": 20},
            }
        }
        spec = {
            "markets": [
                {
                    "strategies": [
                        {"strategy": {"strategy_id": "selected", "fast": 7}},
                        {"strategy": {"strategy_id": "peer", "fast": 3}},
                    ]
                }
            ]
        }

        bound = apply_parameter_defaults(spec, manifest, target_strategy_id="selected")

        selected, peer = bound["markets"][0]["strategies"]
        self.assertEqual(selected["strategy"], {"strategy_id": "selected", "fast": 7, "slow": 20})
        self.assertEqual(peer["strategy"], {"strategy_id": "peer", "fast": 3})
        self.assertNotIn("slow", spec["markets"][0]["strategies"][0]["strategy"])
        self.assertEqual(
            resolve_strategy_parameters(bound, manifest, target_strategy_id="selected"),
            {"fast": 7, "slow": 20},
        )

    def test_run_binds_manifest_defaults_before_preflight_and_factory(self):
        source = self.strategy_path.read_text(encoding="utf-8")
        source = source.replace(
            "def create_strategy(spec):\n    return {'spec': spec}\n",
            "def validate_config(config):\n"
            "    if config['fast'] >= config['slow']:\n"
            "        raise ValueError('fast must be below slow')\n"
            "def create_strategy(spec):\n    return {'spec': spec}\n",
        )
        self.strategy_path.write_text(source, encoding="utf-8")
        payload = self._backtest_base()
        payload["strategy"].pop("fast")
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy", return_value={"ok": True}) as run:
            with contextlib.redirect_stdout(output):
                code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 0)
        strategy_binding = run.call_args.args[1]
        configured_strategy = strategy_binding.strategy["spec"]["strategy"]
        self.assertEqual(configured_strategy["fast"], 5)
        self.assertEqual(configured_strategy["slow"], 20)

    def test_invalid_module_config_fails_before_dataset_preflight(self):
        source = self.strategy_path.read_text(encoding="utf-8")
        source = source.replace(
            "def create_strategy(spec):\n    return {'spec': spec}\n",
            "def validate_config(config):\n"
            "    raise ValueError('incompatible parameter pair')\n"
            "def create_strategy(spec):\n    return {'spec': spec}\n",
        )
        self.strategy_path.write_text(source, encoding="utf-8")
        self.spec_path.write_text(json.dumps(self._backtest_base()), encoding="utf-8")
        error = io.StringIO()
        with patch.object(strategy_execution, "validate_strategy_execution_preflight_v1") as preflight:
            with patch.object(strategy_execution, "run_backtest_with_strategy") as run:
                with contextlib.redirect_stderr(error):
                    code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(preflight.called)
        self.assertFalse(run.called)
        self.assertIn("incompatible parameter pair", error.getvalue())

    def test_candidate_factory_rebinds_defaults_and_runs_validator_each_time(self):
        observed: list[dict[str, object]] = []

        def validate_config(config):
            observed.append(dict(config))
            if config["fast"] >= config["slow"]:
                raise ValueError("fast must be below slow")

        module = type("StrategyModule", (), {"validate_config": staticmethod(validate_config)})()
        manifest = {
            "completeness": "complete",
            "parameters": {
                "fast": {"type": "integer", "default": 5},
                "slow": {"type": "integer", "default": 20},
            },
        }
        factory = cli._bind_manifest_config_factory(
            lambda spec: {"spec": spec}, module, manifest, "manifest-sma"
        )

        first = factory({"strategy": {"strategy_id": "manifest-sma", "fast": 7}})
        self.assertEqual(first["spec"]["strategy"]["slow"], 20)
        with self.assertRaisesRegex(ValueError, "fast must be below slow"):
            factory(
                {"strategy": {"strategy_id": "manifest-sma", "fast": 7}},
                {"fast": 25},
            )
        self.assertEqual(
            observed,
            [{"fast": 7, "slow": 20}, {"fast": 25, "slow": 20}],
        )

    def test_inspect_out_refuses_to_overwrite_strategy_source(self):
        output = io.StringIO()
        error = io.StringIO()
        with contextlib.redirect_stderr(error), contextlib.redirect_stdout(output):
            code = cli.main(
                ["inspect-strategy", str(self.strategy_path), "--out", str(self.strategy_path)]
            )
        self.assertEqual(code, 2)
        self.assertIn("overwrite an input file", error.getvalue())
        self.assertIn("create_strategy", self.strategy_path.read_text(encoding="utf-8"))

    def test_strategy_without_declarations_remains_partial(self):
        self._write_strategy(manifest="None")
        source = self.strategy_path.read_text(encoding="utf-8")
        source = source.replace("STRATEGY_MANIFEST = None\n", "")
        self.strategy_path.write_text(source, encoding="utf-8")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = cli.main(["inspect-strategy", str(self.strategy_path)])
        self.assertEqual(code, 0)
        manifest = json.loads(output.getvalue())
        self.assertEqual(manifest["completeness"], "partial")
        self.assertIsNone(manifest["warmup_bars"])
        self.assertIsNone(manifest["instrument_requirements"])

    def test_search_rejects_values_outside_the_declared_parameter_range(self):
        self._write_experiment([2, 11])
        output = io.StringIO()
        with patch.object(cli, "run_experiment_with_strategy") as run:
            with contextlib.redirect_stderr(output):
                code = cli.main(
                    ["search-strategy", str(self.strategy_path), str(self.spec_path)]
                )
        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("above maximum 10", output.getvalue())

    def test_search_uses_declared_strategy_identity_and_accepts_in_range_values(self):
        self._write_experiment([2, 10])
        output = io.StringIO()
        with patch.object(
            cli,
            "run_experiment_with_strategy",
            return_value={"artifact_path": "runs/outcome.json"},
        ) as run:
            with contextlib.redirect_stdout(output):
                code = cli.main(
                    ["search-strategy", str(self.strategy_path), str(self.spec_path)]
                )
        self.assertEqual(code, 0)
        self.assertEqual(run.call_args.kwargs["strategy_id"], "manifest-sma")
        self.assertEqual(run.call_args.kwargs["strategy_version"], "2.1")
        self.assertEqual(json.loads(output.getvalue())["artifact_path"], "runs/outcome.json")

    def test_required_parameters_can_be_fixed_in_experiment_base(self):
        self._write_strategy(
            manifest="""{
                'schema_version': 1,
                'strategy_id': 'manifest-sma',
                'strategy_version': '2.1',
                'parameters': {
                    'fast': {'type': 'integer', 'required': True, 'minimum': 2, 'maximum': 10},
                    'slow': {'type': 'integer', 'minimum': 10, 'maximum': 40, 'default': 20},
                },
                'instrument_requirements': {
                    'asset_classes': ['crypto'], 'products': ['spot'], 'venues': ['BINANCE'],
                    'requires_explicit_spec': True,
                },
                'data_level': 'bar',
                'warmup_bars': 40,
                'dependencies': [],
            }"""
        )
        self.spec_path.write_text(
            json.dumps(
                {
                    "experiment_id": "manifest-search",
                    "base": {**self._backtest_base(), "strategy": {"strategy_id": "manifest-sma", "fast": 4}},
                    "parameter_space": [{"name": "slow", "values": [20, 30]}],
                    "split_plan": {"train_bars": 41, "test_bars": 5},
                }
            ),
            encoding="utf-8",
        )
        with patch.object(
            cli,
            "run_experiment_with_strategy",
            return_value={"completed_candidates": 2},
        ) as run:
            with contextlib.redirect_stdout(io.StringIO()):
                code = cli.main(
                    ["search-strategy", str(self.strategy_path), str(self.spec_path)]
                )
        self.assertEqual(code, 0)
        self.assertTrue(run.called)

    def test_complete_manifest_requires_matching_asset_class_before_run(self):
        payload = self._backtest_base()
        payload["asset_class"] = "equity"
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("asset_class", output.getvalue())

    def test_complete_manifest_requires_explicit_market_spec_when_declared(self):
        payload = self._backtest_base()
        payload.pop("market_spec")
        payload.pop("fee_schedule")
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("requires an explicit market_spec", output.getvalue())

    def test_complete_manifest_requires_allowed_product_and_venue(self):
        payload = self._backtest_base()
        payload["market_spec"]["product"] = "perpetual"
        payload["market_spec"]["instrument"]["venue"] = "OKX"
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        message = json.loads(output.getvalue())["message"]
        self.assertIn("product", message)

        payload["market_spec"]["product"] = "spot"
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("venue", json.loads(output.getvalue())["message"])

    def test_complete_manifest_requires_more_validated_bars_than_warmup(self):
        payload = self._backtest_base()
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        self._write_bars(40)
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("at least 41 validated Bars", output.getvalue())

    def test_complete_manifest_rejects_non_bar_execution_surface(self):
        self._write_strategy(
            manifest="""{
                'schema_version': 1, 'strategy_id': 'manifest-sma', 'strategy_version': '2.1',
                'parameters': {},
                'instrument_requirements': {'asset_classes': [], 'products': [], 'venues': [], 'requires_explicit_spec': False},
                'data_level': 'tick', 'warmup_bars': 0, 'dependencies': [],
            }"""
        )
        payload = self._backtest_base()
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("accepts Bar data only", output.getvalue())

        self._write_strategy(
            manifest="""{
                'schema_version': 1, 'strategy_id': 'manifest-sma', 'strategy_version': '2.1',
                'parameters': {},
                'instrument_requirements': {'asset_classes': [], 'products': [], 'venues': [], 'requires_explicit_spec': False},
                'data_level': 'timer', 'warmup_bars': 0, 'dependencies': [],
            }"""
        )
        output = io.StringIO()
        with patch.object(strategy_execution, "run_backtest_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["run-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("supported levels are bar, tick, and orderbook", output.getvalue())

    def test_search_training_window_must_cover_declared_warmup(self):
        self._write_experiment([2, 10])
        payload = json.loads(self.spec_path.read_text(encoding="utf-8"))
        payload["split_plan"]["train_bars"] = 40
        self.spec_path.write_text(json.dumps(payload), encoding="utf-8")
        output = io.StringIO()
        with patch.object(cli, "run_experiment_with_strategy") as run, contextlib.redirect_stderr(output):
            code = cli.main(["search-strategy", str(self.strategy_path), str(self.spec_path)])

        self.assertEqual(code, 2)
        self.assertFalse(run.called)
        self.assertIn("train_bars must be at least 41", output.getvalue())

    def test_multi_market_run_preflights_only_the_selected_strategy_slot(self):
        target_market = {
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": str(self.bars_path),
            "asset_class": "crypto",
            "market_spec": self._backtest_base()["market_spec"],
            "strategies": [{"strategy": {"strategy_id": "selected"}}],
        }
        peer_market = {
            "instrument": "ETHUSDT.OKX",
            "strategies": [{"strategy": {"strategy_id": "peer"}}],
        }
        self.spec_path.write_text(
            json.dumps({"run_id": "portfolio", "markets": [target_market, peer_market]}),
            encoding="utf-8",
        )
        output = io.StringIO()
        with patch.object(
            strategy_execution,
            "run_multi_market_backtest_with_strategy_slots",
            return_value={"ok": True},
        ) as run, contextlib.redirect_stdout(output):
            code = cli.main(
                [
                    "run-strategy",
                    str(self.strategy_path),
                    str(self.spec_path),
                    "--strategy-id",
                    "selected",
                ]
            )

        self.assertEqual(code, 0)
        self.assertEqual(run.call_args.args[1][0]["strategy_id"], "selected")

    def test_duplicate_strategy_dependencies_are_rejected(self):
        module = type(
            "StrategyModule",
            (),
            {
                "STRATEGY_MANIFEST": None,
                "STRATEGY_DEPENDENCIES": ("helper.py", "helper.py"),
            },
        )()
        with self.assertRaisesRegex(ValueError, "must not contain duplicates"):
            build_module_manifest(module, self.strategy_path, "0" * 64)

    def test_conflicting_module_and_class_manifests_are_rejected(self):
        strategy_class = type(
            "ClassStrategy",
            (),
            {"STRATEGY_MANIFEST": {"schema_version": 1, "parameters": {}}},
        )
        module = type(
            "StrategyModule",
            (),
            {
                "STRATEGY_MANIFEST": {
                    "schema_version": 1,
                    "parameters": {"fast": {"type": "integer"}},
                },
                "STRATEGY_CLASS": strategy_class,
            },
        )()

        with self.assertRaisesRegex(ValueError, "conflicts with STRATEGY_CLASS"):
            build_module_manifest(module, self.strategy_path, "0" * 64)

    def test_module_and_class_config_validators_cannot_diverge(self):
        strategy_class = type(
            "ClassStrategy",
            (),
            {"validate_config": staticmethod(lambda config: None)},
        )
        module = type(
            "StrategyModule",
            (),
            {
                "STRATEGY_CLASS": strategy_class,
                "validate_config": lambda config: None,
            },
        )()

        with self.assertRaisesRegex(ValueError, "either on the module or STRATEGY_CLASS"):
            declared_config_validator(module)


if __name__ == "__main__":
    unittest.main()
