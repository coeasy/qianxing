from __future__ import annotations

import contextlib
import hashlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
PYTHON_ROOT = ROOT / "python"
sys.path.insert(0, str(PYTHON_ROOT))

from qianxing import cli
from qianxing_bridge.strategy import StrategyInput
from qianxing_strategy import BaseStrategy
from qianxing_strategy import diagnostic_callback_worker
from qianxing_strategy.diagnostic_callback_worker import _load_factory
from qianxing_strategy.diagnostics import diagnose_callback_strategy


def diagnostic_request() -> StrategyInput:
    closes = [100, 102, 99, 104, 106, 101]
    return StrategyInput.from_dict(
        {
            "schema_version": 1,
            "request_id": "callback-diagnostic",
            "strategy_id": "callback-test",
            "strategy_version": "1",
            "data_fingerprint": "barframe:test",
            "as_of": 7,
            "instrument": "TEST.SIM",
            "positions": {},
            "cash": {"USD": 1_000_000},
            "available_margin_raw": None,
            "risk_state": "diagnostic_fixed_initial_context",
            "research_targets": {},
            "bars": {
                "source": "synthetic-test",
                "ts": [1, 2, 3, 4, 5, 6],
                "open_raw": closes,
                "high_raw": [value + 2 for value in closes],
                "low_raw": [value - 2 for value in closes],
                "close_raw": closes,
                "volume_raw": [10, 11, 12, 13, 14, 15],
            },
        }
    )


class CallbackStrategyDiagnosticsTest(unittest.TestCase):
    def test_diagnostic_worker_runs_class_lifecycle_and_declared_warmup(self):
        class ClassicStrategy(BaseStrategy):
            STRATEGY_MANIFEST = {"warmup_bars": 2}

            def __init__(self):
                self.events = []

            def history_requirements(self):
                return {"warmup_bars": 2}

            def prenext(self, context, bar):
                self.events.append(("prenext", bar["ts"]))
                return self.decision(context, "Bar", bar)

            def on_warmup_complete(self, context, instrument, observed_bars):
                self.events.append(("warmup", observed_bars))

            def next(self, context, bar):
                self.events.append(("next", bar["ts"]))
                return self.decision(context, "Bar", bar)

            def diagnostic_state(self):
                return {"events": self.events}

        strategy = ClassicStrategy()
        with patch.object(
            diagnostic_callback_worker,
            "_load_factory",
            return_value=(lambda _spec: strategy, SimpleNamespace()),
        ):
            result = diagnostic_callback_worker._handle(
                diagnostic_request(),
                Path("strategy.py"),
                {"output_dir": "unused"},
            )

        self.assertEqual(result["output"]["signal_id"], 6)
        self.assertEqual(
            result["diagnostic_state"]["events"],
            [
                ("prenext", 1),
                ("warmup", 2),
                ("next", 2),
                ("next", 3),
                ("next", 4),
                ("next", 5),
                ("next", 6),
            ],
        )

    def test_diagnostic_worker_reports_history_requirements_failure_as_v1(self):
        observed = []

        class BrokenHistoryStrategy(BaseStrategy):
            def history_requirements(self):
                raise RuntimeError("diagnostic warmup declaration failed")

            def on_error_v1(self, error):
                observed.append(error.to_dict())

        strategy = BrokenHistoryStrategy()
        with patch.object(
            diagnostic_callback_worker,
            "_load_factory",
            return_value=(lambda _spec: strategy, SimpleNamespace()),
        ):
            with self.assertRaisesRegex(
                RuntimeError, "diagnostic warmup declaration failed"
            ):
                diagnostic_callback_worker._handle(
                    diagnostic_request(),
                    Path("strategy.py"),
                    {"output_dir": "unused"},
                )

        self.assertEqual(observed[0]["stage"], "history_requirements")
        self.assertEqual(observed[0]["strategy_id"], "callback-test")
        self.assertEqual(observed[0]["timestamp"], 7)
        self.assertEqual(observed[0]["instrument"], "TEST.SIM")
        self.assertEqual(observed[0]["message"], "diagnostic warmup declaration failed")

    def test_diagnostic_worker_accepts_standard_strategy_class_export(self):
        strategy_file = ROOT / "python" / "examples" / "strategy_file_sma.py"
        factory = _load_factory(strategy_file)
        strategy = factory(
            {
                "strategy": {
                    "fast_window": 3,
                    "slow_window": 15,
                    "quantity_raw": 1,
                },
                "parameters": {"fast_window": 3, "slow_window": 15},
            }
        )
        self.assertIsInstance(strategy, BaseStrategy)
        self.assertEqual(strategy.fast_window, 3)
        self.assertEqual(strategy.slow_window, 15)

    def test_stateful_file_callback_replays_only_visible_bars_for_warmup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "causal.py"
            strategy.write_text(
                "def create_strategy(spec):\n"
                " class S:\n"
                "  def __init__(self): self.closes=[]\n"
                "  def on_event(self, context, event):\n"
                "   self.closes.append(event['Bar']['close_raw'])\n"
                "   return {'signal_id': event['Bar']['ts'], 'confidence': len(self.closes), 'intents': []}\n"
                "  def diagnostic_state(self): return {'bars_seen': len(self.closes)}\n"
                " return S()\n",
                encoding="utf-8",
            )
            frame_path = root / "bars.json"
            frame_path.write_text("{}", encoding="utf-8")
            report = diagnose_callback_strategy(
                diagnostic_request(),
                strategy,
                {"bars_path": str(frame_path), "output_dir": str(root / "runs"), "account_id": "test-account"},
                strategy_bundle_sha256="ab" * 32,
                startup_windows=[2, 4],
                max_checks=2,
                timeout_seconds=10,
            )
        self.assertEqual(report["worker_kind"], "python_callback_visible_bar_replay")
        self.assertEqual(report["lookahead"]["status"], "no_change_detected")
        self.assertEqual(report["startup"]["status"], "startup_sensitive")
        self.assertEqual(report["strategy_bundle_sha256"], "ab" * 32)
        self.assertIn("does not simulate fills", report["context_assumption"])

    def test_factory_reading_full_configured_bar_file_is_probed_with_perturbations(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "future_reader.py"
            strategy.write_text(
                "import json\n"
                "def create_strategy(spec):\n"
                " bars=json.load(open(spec['bars_path'], encoding='utf-8'))\n"
                " last=bars['close_raw'][-1]\n"
                " class S:\n"
                "  def on_event(self, context, event):\n"
                "   return {'signal_id': last, 'confidence': 1, 'intents': []}\n"
                " return S()\n",
                encoding="utf-8",
            )
            frame_path = root / "bars.json"
            frame_path.write_text("{}", encoding="utf-8")
            report = diagnose_callback_strategy(
                diagnostic_request(),
                strategy,
                {"bars_path": str(frame_path)},
                strategy_bundle_sha256="cd" * 32,
                startup_windows=[2, 4],
                max_checks=2,
                timeout_seconds=10,
            )
        self.assertEqual(report["lookahead"]["status"], "potential_lookahead_detected")
        self.assertGreater(report["lookahead"]["flagged_checks"], 0)
        self.assertIn("External data reads", report["diagnostic_scope"])

    def test_cli_command_writes_diagnostics_without_overwriting_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "causal.py"
            strategy.write_text(
                "STRATEGY_ID='callback-test'\n"
                "STRATEGY_VERSION='1'\n"
                "def create_strategy(spec):\n"
                " class S:\n"
                "  def on_event(self, context, event): return {'signal_id': event['Bar']['ts'], 'confidence': 1, 'intents': []}\n"
                " return S()\n",
                encoding="utf-8",
            )
            bars = {
                "instrument": "TEST.SIM",
                **diagnostic_request().bars.to_dict(),
            }
            bars_path = root / "bars.json"
            bars_path.write_text(json.dumps(bars), encoding="utf-8")
            spec_path = root / "backtest.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "run_id": "diagnostic-run",
                        "instrument": "TEST.SIM",
                        "bars_path": str(bars_path),
                        "settlement_currency": "USD",
                        "initial_cash_raw": 1_000_000,
                        "output_dir": str(root / "run"),
                        "strategy": {"kind": "sma_cross", "strategy_id": "callback-test", "quantity_raw": 1, "fast_window": 1, "slow_window": 2},
                    }
                ),
                encoding="utf-8",
            )
            output_path = root / "bias.json"
            stdout = io.StringIO()
            stderr = io.StringIO()
            with patch.object(
                cli,
                "validate_dataset",
                return_value=SimpleNamespace(
                    usable=True,
                    instrument="TEST.SIM",
                    rows=6,
                    content_fingerprint="fixture-fingerprint",
                ),
            ), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = cli.main(
                    [
                        "diagnose-strategy",
                        str(strategy),
                        str(spec_path),
                        "--startup-windows",
                        "2,4",
                        "--max-lookahead-checks",
                        "2",
                        "--out",
                        str(output_path),
                    ]
                )
            self.assertEqual(code, 0)
            result = json.loads(stdout.getvalue())
            saved = json.loads(output_path.read_text(encoding="utf-8"))
            self.assertEqual(result, saved)
            self.assertEqual(result["strategy_id"], "callback-test")
            self.assertIn("Strategy diagnostics:", stderr.getvalue())
            with contextlib.redirect_stderr(io.StringIO()):
                collision_code = cli.main(
                    ["diagnose-strategy", str(strategy), str(spec_path), "--out", str(bars_path)]
                )
            self.assertEqual(collision_code, 2)
            self.assertEqual(json.loads(bars_path.read_text(encoding="utf-8")), bars)

    def test_multi_market_diagnostics_perturb_peer_market_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "peer_future_reader.py"
            strategy.write_text(
                "import json\n"
                "def create_strategy(spec):\n"
                " assert spec['market']['instrument']=='TEST.SIM'\n"
                " assert spec['strategy']['strategy_id']=='target-slot'\n"
                " assert spec['strategy_slot']['strategy']['strategy_id']=='target-slot'\n"
                " peer=next(m for m in spec['markets'] if m['instrument']=='PEER.SIM')\n"
                " last=json.load(open(peer['bars_path'], encoding='utf-8'))['close_raw'][-1]\n"
                " class S:\n"
                "  def on_event(self, context, event): return {'signal_id': last, 'confidence': 1, 'intents': []}\n"
                " return S()\n",
                encoding="utf-8",
            )

            def write_frame(path: Path, instrument: str, closes: list[int]) -> None:
                path.write_text(
                    json.dumps(
                        {
                            "instrument": instrument,
                            "source": "diagnostic-fixture",
                            "ts": [1, 2, 3, 4, 5, 6],
                            "open_raw": closes,
                            "high_raw": [value + 2 for value in closes],
                            "low_raw": [value - 2 for value in closes],
                            "close_raw": closes,
                            "volume_raw": [10, 11, 12, 13, 14, 15],
                        }
                    ),
                    encoding="utf-8",
                )

            target_bars = root / "target-bars.json"
            peer_bars = root / "peer-bars.json"
            write_frame(target_bars, "TEST.SIM", [100, 102, 99, 104, 106, 101])
            write_frame(peer_bars, "PEER.SIM", [200, 202, 199, 204, 206, 201])
            spec = {
                "schema_version": 1,
                "run_id": "multi-market-diagnostic",
                "account_id": "diagnostic-account",
                "settlement_currency": "USD",
                "initial_cash_raw": 1_000_000,
                "seed": 1,
                "output_dir": str(root / "run"),
                "markets": [
                    {
                        "instrument": "TEST.SIM",
                        "bars_path": str(target_bars),
                        "strategies": [
                            {
                                "priority": 0,
                                "reserved_capital_raw": 500_000,
                                "strategy": {"kind": "sma_cross", "strategy_id": "target-slot"},
                            }
                        ],
                    },
                    {
                        "instrument": "PEER.SIM",
                        "bars_path": str(peer_bars),
                        "strategies": [
                            {
                                "priority": 0,
                                "reserved_capital_raw": 500_000,
                                "strategy": {"kind": "sma_cross", "strategy_id": "peer-slot"},
                            }
                        ],
                    },
                ],
            }
            spec_path = root / "multi-market.json"
            spec_path.write_text(json.dumps(spec), encoding="utf-8")

            def validate_dataset_stub(dataset_spec):
                bars_path = (
                    dataset_spec["bars_path"]
                    if isinstance(dataset_spec, dict)
                    else dataset_spec.bars_path
                )
                frame = json.loads(Path(bars_path).read_text(encoding="utf-8"))
                return SimpleNamespace(
                    usable=True,
                    instrument=frame["instrument"],
                    rows=len(frame["ts"]),
                    content_fingerprint=f"fixture:{frame['instrument']}",
                )

            stdout = io.StringIO()
            with patch.object(cli, "validate_dataset", side_effect=validate_dataset_stub):
                with contextlib.redirect_stdout(stdout):
                    code = cli.main(
                        [
                            "diagnose-strategy",
                            str(strategy),
                            str(spec_path),
                            "--strategy-id",
                            "target-slot",
                            "--startup-windows",
                            "2,4",
                            "--max-lookahead-checks",
                            "2",
                        ]
                    )
            self.assertEqual(code, 0)
            result = json.loads(stdout.getvalue())
            self.assertEqual(result["strategy_id"], "target-slot")
            self.assertEqual(result["multi_market"]["peer_market_bars_included"], 1)
            self.assertEqual(result["multi_market"]["point_in_time_ledger_scope"], "not_supplied")
            self.assertTrue(result["lookahead"]["cross_market_inputs_included"])
            self.assertGreater(result["lookahead"]["flagged_checks"], 0)
            self.assertTrue(
                any(
                    "PEER.SIM" in check["cross_market_instruments_perturbed"]
                    for check in result["lookahead"]["checks"]
                )
            )

    def test_multi_market_diagnostic_attachment_revalidates_each_input_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "causal.py"
            strategy.write_text(
                "STRATEGY_ID='target-slot'\n"
                "STRATEGY_VERSION='1'\n"
                "def create_strategy(spec): return object()\n",
                encoding="utf-8",
            )

            def write_frame(path: Path, instrument: str, offset: int) -> None:
                closes = [offset + value for value in (100, 102, 99, 104, 106, 101)]
                path.write_text(
                    json.dumps(
                        {
                            "instrument": instrument,
                            "source": "diagnostic-fixture",
                            "ts": [1, 2, 3, 4, 5, 6],
                            "open_raw": closes,
                            "high_raw": [value + 2 for value in closes],
                            "low_raw": [value - 2 for value in closes],
                            "close_raw": closes,
                            "volume_raw": [10, 11, 12, 13, 14, 15],
                        }
                    ),
                    encoding="utf-8",
                )

            target_bars = root / "target-bars.json"
            peer_bars = root / "peer-bars.json"
            write_frame(target_bars, "TEST.SIM", 0)
            write_frame(peer_bars, "PEER.SIM", 100)
            base = {
                "schema_version": 1,
                "run_id": "multi-market-diagnostic-attach",
                "account_id": "diagnostic-account",
                "settlement_currency": "USD",
                "initial_cash_raw": 1_000_000,
                "seed": 1,
                "output_dir": str(root / "run"),
                "markets": [
                    {
                        "instrument": "TEST.SIM",
                        "bars_path": str(target_bars),
                        "strategies": [
                            {
                                "priority": 0,
                                "reserved_capital_raw": 500_000,
                                "strategy": {"kind": "sma_cross", "strategy_id": "target-slot"},
                            }
                        ],
                    },
                    {
                        "instrument": "PEER.SIM",
                        "bars_path": str(peer_bars),
                        "strategies": [
                            {
                                "priority": 0,
                                "reserved_capital_raw": 500_000,
                                "strategy": {"kind": "sma_cross", "strategy_id": "peer-slot"},
                            }
                        ],
                    },
                ],
            }
            experiment = {
                "schema_version": 1,
                "experiment_id": "attach-multi-diagnostic",
                "target_strategy_id": "target-slot",
                "base": base,
                "callback_parameter_space": [{"name": "threshold", "values": [1]}],
            }
            experiment_path = root / "experiment.json"
            experiment_path.write_text(json.dumps(experiment), encoding="utf-8")
            _module, _factory, bundle_sha = cli._load_strategy_file(str(strategy))

            def validate_dataset_stub(dataset_spec):
                bars_path = (
                    dataset_spec["bars_path"]
                    if isinstance(dataset_spec, dict)
                    else dataset_spec.bars_path
                )
                frame = json.loads(Path(bars_path).read_text(encoding="utf-8"))
                return SimpleNamespace(
                    usable=True,
                    instrument=frame["instrument"],
                    rows=len(frame["ts"]),
                    content_fingerprint=f"fixture:{frame['instrument']}:{frame['close_raw'][0]}",
                )

            with patch.object(cli, "validate_dataset", side_effect=validate_dataset_stub):
                request, _config, _context, _peers, market_inputs = cli._multi_market_diagnostic_request(
                    base, "target-slot", "1"
                )
            diagnostics = {
                "kind": "qianxing-strategy-bias-diagnostics",
                "strategy": "python_create_strategy_callback",
                "strategy_id": "target-slot",
                "strategy_version": "1",
                "strategy_bundle_sha256": bundle_sha,
                "backtest_spec_sha256": hashlib.sha256(
                    json.dumps(
                        base,
                        ensure_ascii=False,
                        sort_keys=True,
                        separators=(",", ":"),
                        allow_nan=False,
                    ).encode("utf-8")
                ).hexdigest(),
                "data_fingerprint": request.data_fingerprint,
                "instrument": "TEST.SIM",
                "backtest_strategy_config": base["markets"][0]["strategies"][0]["strategy"],
                "multi_market": {
                    "target_strategy_id": "target-slot",
                    "market_count": 2,
                    "market_inputs": market_inputs,
                    "peer_market_bars_included": 1,
                    "peer_market_instruments": ["PEER.SIM"],
                    "point_in_time_ledger_scope": "not_supplied",
                },
                "lookahead": {"status": "no_change_detected"},
                "startup": {"status": "stable_across_requested_windows"},
            }
            diagnostics_path = root / "diagnostics.json"
            diagnostics_path.write_text(json.dumps(diagnostics), encoding="utf-8")
            report_output = root / "report.html"
            captured = {}
            experiment_result = {
                "schema_version": 1,
                "experiment_id": "attach-multi-diagnostic",
                "spec_fingerprint": "a" * 16,
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "candidates": [],
                "comparison": {"runs": []},
                "artifact_path": str(root / "result.json"),
            }

            def capture_report(document, output, **_kwargs):
                captured["document"] = document
                return Path(output)

            with patch.object(cli, "validate_dataset", side_effect=validate_dataset_stub):
                with patch.object(cli, "run_multi_market_experiment_with_strategy", return_value=experiment_result):
                    with patch.object(
                        cli,
                        "load_verified_multi_market_candidate_details",
                        return_value={"kind": "multi_market_bar", "runs": []},
                    ):
                        with patch.object(cli, "write_experiment_html", side_effect=capture_report):
                            with contextlib.redirect_stdout(io.StringIO()):
                                code = cli.main(
                                    [
                                        "search-strategy",
                                        str(strategy),
                                        str(experiment_path),
                                        "--diagnostics",
                                        str(diagnostics_path),
                                        "--html",
                                        str(report_output),
                                    ]
                                )
            self.assertEqual(code, 0)
            self.assertEqual(
                captured["document"]["diagnostics"]["multi_market"]["market_inputs"],
                market_inputs,
            )
            from qianxing.reports.experiment_html import render_experiment_html

            rendered = render_experiment_html(captured["document"])
            self.assertIn("策略偏差与预热诊断", rendered)
            self.assertIn("no_change_detected", rendered)

            diagnostics["multi_market"]["market_inputs"][1]["content_fingerprint"] = "tampered"
            diagnostics_path.write_text(json.dumps(diagnostics), encoding="utf-8")
            with patch.object(cli, "validate_dataset", side_effect=validate_dataset_stub):
                with patch.object(cli, "run_multi_market_experiment_with_strategy", return_value=experiment_result):
                    with patch.object(
                        cli,
                        "load_verified_multi_market_candidate_details",
                        return_value={"kind": "multi_market_bar", "runs": []},
                    ):
                        with patch.object(cli, "write_experiment_html", side_effect=capture_report):
                            with contextlib.redirect_stderr(io.StringIO()):
                                code = cli.main(
                                    [
                                        "search-strategy",
                                        str(strategy),
                                        str(experiment_path),
                                        "--diagnostics",
                                        str(diagnostics_path),
                                        "--html",
                                        str(report_output),
                                    ]
                                )
            self.assertEqual(code, 2)

    def test_search_html_accepts_only_identity_matching_diagnostics(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy = root / "causal.py"
            strategy.write_text(
                "STRATEGY_ID='callback-test'\n"
                "STRATEGY_VERSION='1'\n"
                "def create_strategy(spec): return object()\n",
                encoding="utf-8",
            )
            base = {
                "run_id": "run-base",
                "instrument": "TEST.SIM",
                "bars_path": str(root / "bars.json"),
                "settlement_currency": "USD",
                "initial_cash_raw": 1_000_000,
                "output_dir": str(root / "runs"),
                "strategy": {"kind": "sma_cross", "strategy_id": "callback-test", "quantity_raw": 1, "fast_window": 1, "slow_window": 2},
            }
            experiment = root / "experiment.json"
            experiment.write_text(
                json.dumps(
                    {
                        "experiment_id": "search-1",
                        "base": base,
                        "parameter_space": [{"name": "fast_window", "values": [1]}],
                    }
                ),
                encoding="utf-8",
            )
            _module, _factory, bundle_sha = cli._load_strategy_file(str(strategy))
            diagnostics = {
                "kind": "qianxing-strategy-bias-diagnostics",
                "strategy": "python_create_strategy_callback",
                "strategy_id": "callback-test",
                "strategy_version": "1",
                "strategy_bundle_sha256": bundle_sha,
                "backtest_spec_sha256": hashlib.sha256(
                    json.dumps(base, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
                ).hexdigest(),
                "data_fingerprint": "barframe:fixture",
                "instrument": "TEST.SIM",
                "lookahead": {"status": "no_change_detected"},
                "startup": {"status": "stable_across_requested_windows"},
            }
            diagnostic_path = root / "diagnostics.json"
            diagnostic_path.write_text(json.dumps(diagnostics), encoding="utf-8")
            output = root / "report.html"
            result = {
                "schema_version": 1,
                "experiment_id": "search-1",
                "spec_fingerprint": "a" * 16,
                "input_data_fingerprint": "barframe:fixture",
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "candidates": [],
                "comparison": {"runs": []},
                "artifact_path": str(root / "result.json"),
            }
            stdout = io.StringIO()
            with patch.object(cli, "run_experiment_with_strategy", return_value=result):
                with contextlib.redirect_stdout(stdout):
                    code = cli.main(
                        [
                            "search-strategy",
                            str(strategy),
                            str(experiment),
                            "--summary",
                            "--diagnostics",
                            str(diagnostic_path),
                            "--html",
                            str(output),
                        ]
                    )
                self.assertEqual(code, 0)
                self.assertIn("策略偏差与预热诊断", output.read_text(encoding="utf-8"))
                diagnostics["strategy_version"] = "different"
                diagnostic_path.write_text(json.dumps(diagnostics), encoding="utf-8")
                with contextlib.redirect_stderr(io.StringIO()):
                    code = cli.main(
                        [
                            "search-strategy",
                            str(strategy),
                            str(experiment),
                            "--diagnostics",
                            str(diagnostic_path),
                            "--html",
                            str(output),
                        ]
                    )
            self.assertEqual(code, 2)

    def test_candidate_diagnostics_use_each_candidate_parameters_including_fold_search(self):
        base = {
            "run_id": "candidate-base",
            "instrument": "TEST.SIM",
            "bars_path": "bars.json",
            "settlement_currency": "USD",
            "initial_cash_raw": 1_000_000,
            "output_dir": "runs",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "callback-test",
                "quantity_raw": 1,
                "fast_window": 1,
                "slow_window": 8,
            },
        }
        experiment_spec = {
            "experiment_id": "candidate-search",
            "base": base,
            "parameter_space": [{"name": "fast_window", "values": [2, 3]}],
        }
        experiment_document = {
            "experiment_id": "candidate-search",
            "input_data_fingerprint": "bars-sha256",
            "candidates": [
                {"ordinal": 1, "parameters": {"fast_window": 2}, "outcome": {"run_id": "one"}},
                {"ordinal": 2, "parameters": {"fast_window": 3}, "outcome": {"run_id": "two"}},
            ],
            "search": {
                "fold_searches": [
                    {
                        "fold": 1,
                        "candidates": [
                            {"ordinal": 1, "parameters": {"fast_window": 4}, "outcome": {"run_id": "fold-one"}},
                        ],
                    }
                ]
            },
        }
        seen = []

        def fake_diagnose(_request, _strategy_file, spec, **_kwargs):
            seen.append(spec["strategy"]["fast_window"])
            return {
                "lookahead": {"status": "no_change_detected", "flagged_checks": 0},
                "startup": {"status": "stable_across_requested_windows"},
            }

        with (
            patch.object(cli, "_strategy_diagnostic_request", return_value=diagnostic_request()),
            patch("qianxing_strategy.diagnostics.diagnose_callback_strategy", side_effect=fake_diagnose),
        ):
            report = cli._diagnose_experiment_candidates(
                experiment_spec,
                experiment_document,
                "strategy.py",
                strategy_id="callback-test",
                strategy_version="1",
                strategy_bundle_sha256="a" * 64,
                startup_windows=[2, 4],
                max_checks=2,
                timeout_seconds=3,
            )

        self.assertEqual(seen, [2, 3, 4])
        self.assertEqual(report["coverage_status"], "complete")
        self.assertEqual(report["diagnosed_count"], 3)
        self.assertEqual([item["scope"] for item in report["items"]], ["primary", "primary", "fold_local"])
        self.assertEqual(len({item["candidate_fingerprint"] for item in report["items"]}), 3)

    def test_multi_market_candidate_diagnostics_pass_callback_parameters_to_factory(self):
        base = {
            "run_id": "multi-candidate-base",
            "account_id": "account-1",
            "settlement_currency": "USD",
            "initial_cash_raw": 1_000_000,
            "markets": [
                {
                    "instrument": "TEST.SIM",
                    "bars_path": "bars.json",
                    "strategies": [
                        {"strategy": {"kind": "callback", "strategy_id": "callback-test", "lookback": 4}}
                    ],
                }
            ],
        }
        experiment_spec = {
            "experiment_id": "multi-candidate-search",
            "target_strategy_id": "callback-test",
            "base": base,
            "parameter_space": [],
            "callback_parameter_space": [{"name": "threshold", "values": [7]}],
        }
        experiment_document = {
            "experiment_id": "multi-candidate-search",
            "candidates": [
                {"ordinal": 1, "parameters": {"threshold": 7}, "outcome": {"run_id": "one"}}
            ],
        }
        captured = {}

        def fake_request(spec, strategy_id, strategy_version):
            captured["candidate_spec"] = spec
            captured["strategy_id"] = strategy_id
            captured["strategy_version"] = strategy_version
            strategy = spec["markets"][0]["strategies"][0]["strategy"]
            context = {
                "market": spec["markets"][0],
                "strategy": strategy,
                "strategy_slot": spec["markets"][0]["strategies"][0],
            }
            return diagnostic_request(), strategy, context, {}, [{"instrument": "TEST.SIM"}]

        def fake_diagnose(_request, _strategy_file, _spec, **kwargs):
            captured["factory_context"] = kwargs["factory_context"]
            return {
                "lookahead": {"status": "no_change_detected", "flagged_checks": 0},
                "startup": {"status": "stable_across_requested_windows"},
            }

        with (
            patch.object(cli, "_multi_market_diagnostic_request", side_effect=fake_request),
            patch("qianxing_strategy.diagnostics.diagnose_callback_strategy", side_effect=fake_diagnose),
        ):
            report = cli._diagnose_experiment_candidates(
                experiment_spec,
                experiment_document,
                "strategy.py",
                strategy_id="callback-test",
                strategy_version="1",
                strategy_bundle_sha256="b" * 64,
                startup_windows=[2],
                max_checks=1,
                timeout_seconds=2,
            )

        self.assertEqual(captured["factory_context"]["parameters"], {"threshold": 7})
        self.assertEqual(
            captured["factory_context"]["strategy"]["lookback"], 4,
            "callback parameters are passed in the dedicated parameters object, like the live search factory",
        )
        self.assertEqual(report["diagnosed_count"], 1)
        self.assertEqual(report["coverage_status"], "complete")

    def test_candidate_diagnostic_limit_is_reported_as_partial(self):
        experiment_spec = {
            "base": {
                "run_id": "limit-base",
                "instrument": "TEST.SIM",
                "bars_path": "bars.json",
                "settlement_currency": "USD",
                "initial_cash_raw": 1,
                "strategy": {"kind": "sma_cross", "strategy_id": "callback-test"},
            },
            "parameter_space": [],
        }
        experiment_document = {
            "candidates": [
                {"ordinal": 1, "parameters": {}, "outcome": {"run_id": "one"}},
                {"ordinal": 2, "parameters": {}, "outcome": {"run_id": "two"}},
                {"ordinal": 3, "parameters": {}, "outcome": None, "pruning": {"processed_bars": 2}},
            ]
        }

        with (
            patch.object(cli, "_strategy_diagnostic_request", return_value=diagnostic_request()),
            patch(
                "qianxing_strategy.diagnostics.diagnose_callback_strategy",
                return_value={"lookahead": {"status": "no_change_detected"}, "startup": {"status": "stable"}},
            ),
        ):
            report = cli._diagnose_experiment_candidates(
                experiment_spec,
                experiment_document,
                "strategy.py",
                strategy_id="callback-test",
                strategy_version="1",
                strategy_bundle_sha256="c" * 64,
                startup_windows=[2],
                max_checks=1,
                timeout_seconds=2,
                candidate_limit=1,
            )

        self.assertEqual(report["coverage_status"], "partial")
        self.assertEqual(report["diagnosed_count"], 1)
        self.assertEqual(report["not_run_candidate_count"], 1)
        self.assertEqual(report["unavailable_candidate_count"], 1)
        self.assertEqual(
            [item["status"] for item in report["items"]],
            ["diagnosed", "not_run_candidate_limit", "candidate_pruned"],
        )

    def test_search_strategy_cli_wires_candidate_diagnostics_into_summary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy_file = root / "strategy.py"
            strategy_file.write_text("def create_strategy(spec): return spec\n", encoding="utf-8")
            input_path = root / "experiment.json"
            base = {
                "run_id": "cli-candidate-base",
                "instrument": "TEST.SIM",
                "bars_path": "bars.json",
                "settlement_currency": "USD",
                "initial_cash_raw": 1_000_000,
                "output_dir": "runs",
                "strategy": {"kind": "sma_cross", "strategy_id": "callback-test", "fast_window": 1},
            }
            input_path.write_text(
                json.dumps(
                    {
                        "experiment_id": "cli-candidate-search",
                        "base": base,
                        "parameter_space": [{"name": "fast_window", "values": [2]}],
                    }
                ),
                encoding="utf-8",
            )
            result = {
                "experiment_id": "cli-candidate-search",
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "candidates": [
                    {"ordinal": 1, "parameters": {"fast_window": 2}, "outcome": {"run_id": "candidate-1", "fills": 1}}
                ],
                "comparison": {"runs": []},
                "artifact_path": str(root / "result.json"),
                "input_data_fingerprint": "bars-sha256",
            }
            fake_module = SimpleNamespace(STRATEGY_ID="callback-test", STRATEGY_VERSION="1")
            stdout = io.StringIO()
            with (
                patch.object(cli, "_load_strategy_file", return_value=(fake_module, lambda _spec: object(), "d" * 64)),
                patch.object(cli, "build_module_manifest", return_value={"strategy_version": "1", "warmup_bars": None}),
                patch.object(cli, "validate_strategy_execution_preflight_v1"),
                patch.object(cli, "_strategy_identity", return_value=("callback-test", "1")),
                patch.object(cli, "run_experiment_with_strategy", return_value=result),
                patch.object(cli, "_strategy_diagnostic_request", return_value=diagnostic_request()),
                patch(
                    "qianxing_strategy.diagnostics.diagnose_callback_strategy",
                    return_value={
                        "lookahead": {"status": "no_change_detected", "flagged_checks": 0},
                        "startup": {"status": "stable_across_requested_windows"},
                    },
                ),
                contextlib.redirect_stdout(stdout),
            ):
                code = cli.main(
                    [
                        "search-strategy",
                        str(strategy_file),
                        str(input_path),
                        "--diagnose-candidates",
                        "--summary",
                        "--diagnostic-startup-windows",
                        "2",
                        "4",
                        "--diagnostic-max-checks",
                        "2",
                    ]
                )

            document = json.loads(stdout.getvalue())
            self.assertEqual(code, 0)
            self.assertEqual(document["candidate_diagnostics"]["coverage_status"], "complete")
            self.assertEqual(document["candidate_diagnostics"]["diagnosed_count"], 1)
            self.assertEqual(document["candidate_diagnostics"]["items"][0]["parameters"], {"fast_window": 2})

    def test_research_command_defaults_to_candidate_diagnostics_summary_and_html(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            strategy_file = root / "strategy.py"
            strategy_file.write_text("def create_strategy(spec): return spec\n", encoding="utf-8")
            input_path = root / "experiment.json"
            input_path.write_text(
                json.dumps(
                    {
                        "experiment_id": "guided-research",
                        "base": {
                            "run_id": "guided-base",
                            "instrument": "TEST.SIM",
                            "bars_path": "bars.json",
                            "settlement_currency": "USD",
                            "initial_cash_raw": 1_000_000,
                            "output_dir": "runs",
                            "strategy": {
                                "kind": "sma_cross",
                                "strategy_id": "callback-test",
                                "fast_window": 1,
                            },
                        },
                        "parameter_space": [{"name": "fast_window", "values": [2]}],
                        "split_plan": {
                            "train_bars": 3,
                            "test_bars": 2,
                            "walk_forward": False,
                            "step_bars": 0,
                            "purge_bars": 0,
                            "embargo_bars": 0,
                        },
                    }
                ),
                encoding="utf-8",
            )
            result = {
                "experiment_id": "guided-research",
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "candidates": [
                    {
                        "ordinal": 1,
                        "parameters": {"fast_window": 2},
                        "outcome": {"run_id": "candidate-1", "fills": 1},
                    }
                ],
                "comparison": {"runs": []},
                "artifact_path": str(root / "result.json"),
                "input_data_fingerprint": "bars-sha256",
            }
            diagnostics = {
                "coverage_status": "complete",
                "candidate_count": 1,
                "diagnosed_count": 1,
                "diagnostic_failure_count": 0,
                "unavailable_candidate_count": 0,
                "not_run_candidate_count": 0,
                "items": [
                    {
                        "scope": "primary",
                        "ordinal": 1,
                        "parameters": {"fast_window": 2},
                        "candidate_fingerprint": "candidate-sha",
                        "status": "diagnosed",
                    }
                ],
            }
            stdout = io.StringIO()
            stderr = io.StringIO()
            with (
                patch.object(
                    cli,
                    "_load_strategy_file",
                    return_value=(SimpleNamespace(STRATEGY_ID="callback-test", STRATEGY_VERSION="1"), lambda _spec: object(), "d" * 64),
                ),
                patch.object(
                    cli,
                    "build_module_manifest",
                    return_value={"strategy_version": "1", "warmup_bars": None},
                ),
                patch.object(cli, "validate_strategy_execution_preflight_v1"),
                patch.object(cli, "_strategy_identity", return_value=("callback-test", "1")),
                patch.object(
                    cli,
                    "prepare_quick_research_spec",
                    side_effect=lambda payload: {**payload, "baselines": ["buy-and-hold"]},
                ),
                patch.object(cli, "run_experiment_with_strategy", return_value=result) as run_experiment,
                patch.object(
                    cli, "_diagnose_experiment_candidates", return_value=diagnostics
                ) as diagnose_candidates,
                patch.object(cli, "load_verified_candidate_details", return_value={"kind": "bar", "runs": []}),
                patch.object(cli, "write_experiment_html", return_value=root / "research.html") as write_html,
                contextlib.redirect_stdout(stdout),
                contextlib.redirect_stderr(stderr),
            ):
                code = cli.main(["research", str(strategy_file), str(input_path)])

            document = json.loads(stdout.getvalue())
            self.assertEqual(code, 0)
            self.assertEqual(document["candidate_diagnostics"]["coverage_status"], "complete")
            self.assertEqual(document["candidate_diagnostics"]["diagnosed_count"], 1)
            self.assertEqual(write_html.call_count, 1)
            self.assertEqual(
                run_experiment.call_args.args[0]["baselines"], ["buy-and-hold"]
            )
            self.assertEqual(
                diagnose_candidates.call_args.kwargs["candidate_limit"], 16
            )
            self.assertIsNone(write_html.call_args.args[1])
            self.assertEqual(
                write_html.call_args.args[0]["research_details"], {"kind": "bar", "runs": []}
            )
            self.assertIn("Interactive HTML report:", stderr.getvalue())

    def test_guided_research_requires_oos_split_and_effectiveness_baseline(self):
        payload = {"base": {"run_id": "quick"}, "split_plan": {"train_bars": 10, "test_bars": 5}}
        normalized = {**payload, "baselines": ["buy-and-hold"]}
        with patch.object(cli, "prepare_quick_research_spec", return_value=normalized) as prepare:
            self.assertEqual(cli._prepare_guided_research_input(payload), normalized)
            prepare.assert_called_once_with(payload)
        with self.assertRaisesRegex(cli.InvalidInputError, "JSON object"):
            cli._prepare_guided_research_input([])

    def test_guided_multi_market_research_preserves_its_strict_schema(self):
        payload = {
            "base": {"markets": [{}]},
            "split_plan": {"train_bars": 10, "test_bars": 5},
        }
        with patch.object(cli, "prepare_quick_research_spec", return_value=payload) as prepare:
            self.assertEqual(cli._prepare_guided_research_input(payload), payload)
            prepare.assert_called_once_with(payload)


if __name__ == "__main__":
    unittest.main()
