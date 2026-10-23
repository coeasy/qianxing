"""Behavior checks for strategy future-data and startup-window diagnostics."""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
import contextlib
import io
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
PYTHON_ROOT = ROOT / "python"
sys.path.insert(0, str(PYTHON_ROOT))

from qianxing_bridge.strategy import StrategyInput  # noqa: E402
from qianxing_strategy.diagnostics import _validate_pit_ledger, diagnose, main  # noqa: E402


class StrategyDiagnosticsTests(unittest.TestCase):
    def setUp(self):
        self.request = StrategyInput.from_dict(
            json.loads((PYTHON_ROOT / "examples/strategy-diagnostics-input.json").read_text())
        )
        current = os.environ.get("PYTHONPATH", "")
        self.python_path = str(PYTHON_ROOT) + (os.pathsep + current if current else "")

    def run_diagnostic(self, strategy: str):
        with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
            return diagnose(
                self.request,
                str(PYTHON_ROOT / "examples" / strategy),
                [1, 3, 5],
                max_checks=20,
                timeout_seconds=10,
            )

    @staticmethod
    def pit_ledger(facts):
        return {
            "schema_version": 1,
            "kind": "qianxing-pit-ledger",
            "source": "synthetic-provider-history",
            "facts": facts,
        }

    @staticmethod
    def pit_fact(fact_id, effective_at, versions, field="corporate_action.cash_dividend"):
        return {
            "fact_id": fact_id,
            "entity_id": "TEST.SIM",
            "field": field,
            "effective_at": effective_at,
            "versions": versions,
        }

    def test_future_dependent_example_is_detected(self):
        result = self.run_diagnostic("backtest_momentum.py")
        self.assertEqual(result["lookahead"]["status"], "potential_lookahead_detected")
        self.assertGreater(result["lookahead"]["flagged_checks"], 0)

    def test_asof_causal_example_is_not_flagged_for_future_data(self):
        result = self.run_diagnostic("strategy_causal_momentum.py")
        self.assertEqual(result["lookahead"]["status"], "no_change_detected")
        self.assertEqual(result["lookahead"]["flagged_checks"], 0)

    def test_startup_window_sensitivity_is_reported(self):
        result = self.run_diagnostic("strategy_causal_momentum.py")
        self.assertEqual(result["startup"]["status"], "startup_sensitive")
        self.assertEqual(result["startup"]["reference_window_bars"], 5)

    def test_startup_replay_excludes_future_bars_and_marks_short_reference(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); o={'target_qty':sum(r['bars']['close_raw']), 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o}), flush=True)\n",
                encoding="utf-8",
            )
            request_value = self.request.to_dict()
            request_value["as_of"] = 4
            first = StrategyInput.from_dict(request_value)
            changed_value = json.loads(json.dumps(request_value))
            changed_value["bars"]["close_raw"][3:] = [9_000, 99_000]
            second = StrategyInput.from_dict(changed_value)
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                left = diagnose(first, "", [3, 5], 1, 10, [sys.executable, str(worker)])["startup"]
                right = diagnose(second, "", [3, 5], 1, 10, [sys.executable, str(worker)])["startup"]
        self.assertEqual(left["status"], "insufficient_reference_history")
        self.assertEqual(left["visible_bars"], 3)
        self.assertEqual(left["excluded_future_bars"], 2)
        self.assertEqual(left["reference_available_bars"], 3)
        self.assertFalse(left["reference_history_complete"])
        self.assertFalse(left["windows"][1]["requested_window_satisfied"])
        self.assertEqual(left["trajectory"]["status"], "insufficient_reference_history")
        self.assertEqual(left["trajectory"]["decision_points_eligible"], 0)
        self.assertEqual(
            left["trajectory"]["decision_points_excluded_insufficient_reference_history"],
            3,
        )
        self.assertEqual(left["windows"], right["windows"])

    def test_startup_trajectory_finds_early_state_drift_that_converges_at_the_end(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); c=r['bars']['close_raw']; o={'target_qty':0, 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o,'diagnostic_state':{'mean_close':sum(c)/len(c)}}), flush=True)\n",
                encoding="utf-8",
            )
            request_value = self.request.to_dict()
            request_value["as_of"] = 7
            prices = [1, 30, 30, 30, 30, 30]
            request_value["bars"].update(
                {
                    "ts": [1, 2, 3, 4, 5, 6],
                    "open_raw": prices,
                    "high_raw": prices,
                    "low_raw": prices,
                    "close_raw": prices,
                    "volume_raw": [1, 1, 1, 1, 1, 1],
                }
            )
            request = StrategyInput.from_dict(request_value)
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                startup = diagnose(
                    request,
                    "",
                    [3, 5],
                    max_checks=20,
                    timeout_seconds=10,
                    worker_command=[sys.executable, str(worker)],
                )["startup"]

        self.assertEqual(startup["windows"][0]["intermediate_state_changed_fields"], [])
        self.assertEqual(startup["windows"][1]["intermediate_state_changed_fields"], [])
        self.assertEqual(startup["status"], "startup_sensitive")
        self.assertEqual(startup["trajectory"]["status"], "startup_sensitive")
        self.assertEqual(startup["trajectory"]["first_divergence"]["decision_at"], 6)
        self.assertEqual(
            startup["trajectory"]["first_divergence"]["intermediate_state_changed_fields"],
            ["mean_close"],
        )
        self.assertEqual(
            [row["decision_at"] for row in startup["trajectory"]["divergences"]],
            [6],
        )
        self.assertEqual(
            startup["trajectory"]["divergences"][0],
            startup["trajectory"]["first_divergence"],
        )
        self.assertEqual(startup["trajectory"]["decision_points_checked"], 2)
        self.assertEqual(startup["trajectory"]["decision_points_eligible"], 2)
        self.assertEqual(
            startup["trajectory"]["decision_points_excluded_insufficient_reference_history"],
            4,
        )
        self.assertLessEqual(startup["trajectory"]["worker_replays"], 20)

    def test_startup_trajectory_reports_sampling_when_replay_budget_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); o={'target_qty':0, 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o}), flush=True)\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                trajectory = diagnose(
                    self.request,
                    "",
                    [1, 3, 5],
                    max_checks=6,
                    timeout_seconds=10,
                    worker_command=[sys.executable, str(worker)],
                )["startup"]["trajectory"]
        self.assertFalse(trajectory["decision_points_sampled"])
        self.assertEqual(trajectory["decision_points_checked"], 1)
        self.assertEqual(trajectory["decision_points_eligible"], 1)
        self.assertEqual(
            trajectory["decision_points_excluded_insufficient_reference_history"],
            4,
        )
        self.assertLessEqual(trajectory["worker_replays"], 6)

    def test_intermediate_indicator_state_detects_leak_before_decision_changes(self):
        result = self.run_diagnostic("strategy_intermediate_only.py")
        checks = result["lookahead"]["checks"]
        self.assertEqual(result["lookahead"]["flagged_checks"], 0)
        self.assertGreater(result["lookahead"]["intermediate_state_flagged_checks"], 0)
        self.assertEqual(result["lookahead"]["status"], "potential_lookahead_detected")
        self.assertTrue(any(check["intermediate_state_changed_fields"] for check in checks))
        self.assertTrue(result["startup"]["intermediate_state_available"])

    def test_external_jsonl_worker_is_diagnosed_without_python_strategy_adapter(self):
        # This exercises the same process contract used by the C++ JSONL worker.
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); b=r['bars']; o={'target_qty':b['close_raw'][-1], 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o}), flush=True)\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                result = diagnose(
                    self.request,
                    "",
                    [1, 3, 5],
                    max_checks=20,
                    timeout_seconds=10,
                    worker_command=[sys.executable, str(worker)],
                )
        self.assertEqual(result["lookahead"]["status"], "potential_lookahead_detected")
        self.assertIsNone(result["strategy"])
        self.assertIsNone(result["lookahead"]["checks"][0]["baseline_intermediate_state"])
        self.assertFalse(result["lookahead"]["intermediate_state_available"])

    def test_external_worker_intermediate_state_detects_leak_with_stable_decision(self):
        # External workers can opt into the language-neutral diagnostic_state field.
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); b=r['bars']; o={'target_qty':0, 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o,'diagnostic_state':{'last_close':b['close_raw'][-1], 'window_sum':sum(b['close_raw'])}}), flush=True)\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                result = diagnose(
                    self.request,
                    "",
                    [1, 3, 5],
                    max_checks=20,
                    timeout_seconds=10,
                    worker_command=[sys.executable, str(worker)],
                )
        self.assertEqual(result["lookahead"]["flagged_checks"], 0)
        self.assertGreater(result["lookahead"]["intermediate_state_flagged_checks"], 0)
        self.assertEqual(result["lookahead"]["status"], "potential_lookahead_detected")
        self.assertEqual(result["startup"]["status"], "startup_sensitive")
        self.assertTrue(result["lookahead"]["intermediate_state_available"])

    def test_future_dependent_signal_identity_is_reported_even_if_position_is_stable(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); b=r['bars']; o={'target_qty':0, 'confidence':0, 'priority':0, 'intents':[], 'signal_id':b['close_raw'][-1], 'expires_at':r['as_of']}\n"
                " print(json.dumps({'ok':True,'output':o}), flush=True)\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                result = diagnose(
                    self.request, "", [1, 3, 5], max_checks=2, timeout_seconds=10,
                    worker_command=[sys.executable, str(worker)],
                )
        self.assertEqual(result["lookahead"]["status"], "potential_lookahead_detected")
        self.assertTrue(any(
            "signal_id" in check["changed_decision_fields"]
            for check in result["lookahead"]["checks"]
        ))

    def test_external_worker_state_availability_change_is_not_silently_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); b=r['bars']; o={'target_qty':0, 'confidence':0, 'priority':0, 'intents':[]}\n"
                " reply={'ok':True,'output':o}\n"
                " if b['close_raw'][-1]>200: reply['diagnostic_state']={'large_future_close':True}\n"
                " print(json.dumps(reply), flush=True)\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
                result = diagnose(
                    self.request,
                    "",
                    [1, 3, 5],
                    max_checks=20,
                    timeout_seconds=10,
                    worker_command=[sys.executable, str(worker)],
                )
        lookahead = result["lookahead"]
        self.assertEqual(lookahead["flagged_checks"], 0)
        self.assertEqual(lookahead["status"], "potential_lookahead_detected")
        self.assertTrue(lookahead["intermediate_state_available"])
        self.assertTrue(any(
            "__availability__" in (check["intermediate_state_changed_fields"] or [])
            for check in lookahead["checks"]
        ))

    def test_external_worker_cli_writes_machine_readable_report(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            input_path = Path(directory) / "input.json"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); o={'target_qty':r['bars']['close_raw'][-1], 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o}), flush=True)\n",
                encoding="utf-8",
            )
            input_path.write_text(json.dumps(self.request.to_dict()), encoding="utf-8")
            argv = [
                "qianxing-strategy-diagnose", "--input", str(input_path),
                "--worker-executable", sys.executable, "--worker-arg", str(worker),
                "--startup-windows", "1,3,5", "--max-lookahead-checks", "4",
            ]
            output = io.StringIO()
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}), patch.object(sys, "argv", argv), contextlib.redirect_stdout(output):
                self.assertEqual(main(), 0)
            report = json.loads(output.getvalue())
            self.assertEqual(report["worker_kind"], "external_jsonl")
            self.assertEqual(report["lookahead"]["status"], "potential_lookahead_detected")

    def test_pit_ledger_detects_late_facts_and_material_revisions(self):
        ledger = self.pit_ledger(
            [
                self.pit_fact("late-announcement", 1, [
                    {"available_at": 4, "revision": "r1", "value_fingerprint": "dividend-100"},
                ]),
                self.pit_fact("revised-price", 1, [
                    {"available_at": 1, "revision": "r1", "value_fingerprint": "close-100"},
                    {"available_at": 4, "revision": "r2", "value_fingerprint": "close-110"},
                ], field="bar.close"),
                self.pit_fact("same-value-revision", 1, [
                    {"available_at": 1, "revision": "r1", "value_fingerprint": "volume-20"},
                    {"available_at": 4, "revision": "r2", "value_fingerprint": "volume-20"},
                ], field="bar.volume"),
            ]
        )
        result = diagnose(
            self.request,
            str(PYTHON_ROOT / "examples" / "strategy_causal_momentum.py"),
            [1, 3, 5],
            max_checks=20,
            timeout_seconds=10,
            pit_ledger=ledger,
        )["data_visibility"]
        self.assertEqual(result["status"], "potential_pit_bias_detected")
        self.assertEqual(result["affected_fact_decision_counts"]["future_value_unavailable"], 3)
        self.assertEqual(result["affected_fact_decision_counts"]["revised_value_after_decision"], 3)
        self.assertEqual(result["affected_fact_counts"]["future_value_unavailable"], 1)
        self.assertEqual(result["affected_fact_counts"]["revised_value_after_decision"], 1)
        self.assertEqual(len(result["ledger_fingerprint"]), 64)

    def test_pit_ledger_keeps_unknown_availability_separate_from_clean(self):
        unknown = self.pit_ledger(
            [self.pit_fact("missing-publication-time", 1, [
                {"available_at": None, "revision": "r1", "value_fingerprint": "value"},
            ])]
        )
        clean = self.pit_ledger(
            [self.pit_fact("known-before-use", 1, [
                {"available_at": 1, "revision": "r1", "value_fingerprint": "same"},
                {"available_at": 4, "revision": "r2", "value_fingerprint": "same"},
            ])]
        )
        with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
            unknown_result = diagnose(
                self.request, str(PYTHON_ROOT / "examples" / "strategy_causal_momentum.py"),
                [1], max_checks=5, timeout_seconds=10, pit_ledger=unknown,
            )["data_visibility"]
            clean_result = diagnose(
                self.request, str(PYTHON_ROOT / "examples" / "strategy_causal_momentum.py"),
                [1], max_checks=5, timeout_seconds=10, pit_ledger=clean,
            )["data_visibility"]
        self.assertEqual(unknown_result["status"], "visibility_unknown")
        self.assertEqual(unknown_result["affected_fact_decision_counts"]["availability_unknown"], 5)
        self.assertEqual(clean_result["status"], "no_pit_bias_detected")

    def test_pit_adjustment_chain_flags_possible_backward_adjustment_without_claiming_causality(self):
        ledger = self.pit_ledger([
            self.pit_fact("later-corporate-action", 4, [
                {"available_at": 5, "revision": "action-r1", "value_fingerprint": "cash-10"},
            ]),
            self.pit_fact("revised-prior-close", 2, [
                {"available_at": 1, "revision": "bar-r1", "value_fingerprint": "close-100"},
                {"available_at": 5, "revision": "bar-r2", "value_fingerprint": "close-90"},
            ], field="bar.close_raw"),
        ])
        ledger["price_adjustment_basis"] = "backward_adjusted"
        result = diagnose(
            self.request,
            str(PYTHON_ROOT / "examples" / "strategy_causal_momentum.py"),
            [1], max_checks=10, timeout_seconds=10, pit_ledger=ledger,
        )["data_visibility"]["adjustment_chain"]

        self.assertEqual(result["status"], "potential_back_adjustment_lookahead")
        self.assertEqual(result["potential_back_adjustment_lookahead_count"], 1)
        self.assertTrue(result["correlation_only"])
        example = result["examples"][0]
        self.assertEqual(example["causal_attribution"], "temporal_correlation_only")
        self.assertTrue(example["potential_back_adjustment_lookahead"])
        self.assertGreater(example["pre_action_decision_points"], 0)
        self.assertEqual(example["action_known_at_pre_action_decisions"], 0)

    def test_pit_adjustment_unknown_and_raw_basis_do_not_claim_back_adjustment(self):
        facts = [
            self.pit_fact("later-action", 4, [
                {"available_at": None, "revision": "action-r1", "value_fingerprint": "cash-10"},
            ]),
            self.pit_fact("revised-price", 2, [
                {"available_at": 1, "revision": "bar-r1", "value_fingerprint": "close-100"},
                {"available_at": 5, "revision": "bar-r2", "value_fingerprint": "close-90"},
            ], field="bar.close"),
        ]
        with patch.dict(os.environ, {"PYTHONPATH": self.python_path}):
            for basis in ("unknown", "raw", "forward_adjusted"):
                with self.subTest(basis=basis):
                    ledger = self.pit_ledger(facts)
                    ledger["price_adjustment_basis"] = basis
                    chain = diagnose(
                        self.request,
                        str(PYTHON_ROOT / "examples" / "strategy_causal_momentum.py"),
                        [1], max_checks=10, timeout_seconds=10, pit_ledger=ledger,
                    )["data_visibility"]["adjustment_chain"]
                    self.assertEqual(chain["potential_back_adjustment_lookahead_count"], 0)
                    self.assertTrue(chain["correlation_only"])
                    if chain["examples"]:
                        self.assertEqual(
                            chain["examples"][0]["causal_attribution"],
                            "temporal_correlation_only",
                        )

    def test_pit_ledger_rejects_invalid_adjustment_basis(self):
        ledger = self.pit_ledger([
            self.pit_fact("fact", 1, [
                {"available_at": 1, "revision": "r1", "value_fingerprint": "v1"},
            ]),
        ])
        ledger["price_adjustment_basis"] = ["raw"]
        with self.assertRaisesRegex(ValueError, "price_adjustment_basis"):
            _validate_pit_ledger(ledger)

    def test_pit_ledger_rejects_duplicate_fact_ids_and_non_increasing_versions(self):
        duplicate = self.pit_ledger(
            [
                self.pit_fact("duplicate", 1, [
                    {"available_at": 1, "revision": "r1", "value_fingerprint": "a"},
                ]),
                self.pit_fact("duplicate", 1, [
                    {"available_at": 2, "revision": "r1", "value_fingerprint": "b"},
                ]),
            ]
        )
        unordered = self.pit_ledger(
            [self.pit_fact("unordered", 1, [
                {"available_at": 2, "revision": "r1", "value_fingerprint": "a"},
                {"available_at": 2, "revision": "r2", "value_fingerprint": "b"},
            ])]
        )
        for ledger in (duplicate, unordered):
            with self.subTest(ledger=ledger):
                with self.assertRaises(ValueError):
                    diagnose(
                        self.request,
                        str(PYTHON_ROOT / "examples" / "strategy_causal_momentum.py"),
                        [1], max_checks=5, timeout_seconds=10, pit_ledger=ledger,
                    )

    def test_pit_ledger_cli_is_part_of_the_same_machine_report(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker.py"
            input_path = Path(directory) / "input.json"
            ledger_path = Path(directory) / "pit-ledger.json"
            worker.write_text(
                "import json,sys\n"
                "for line in sys.stdin:\n"
                " r=json.loads(line); o={'target_qty':0, 'confidence':0, 'priority':0, 'intents':[]}\n"
                " print(json.dumps({'ok':True,'output':o}), flush=True)\n",
                encoding="utf-8",
            )
            input_path.write_text(json.dumps(self.request.to_dict()), encoding="utf-8")
            ledger_path.write_text(
                json.dumps(self.pit_ledger([self.pit_fact("not-yet-published", 1, [
                    {"available_at": 5, "revision": "r1", "value_fingerprint": "value"},
                ])])),
                encoding="utf-8",
            )
            argv = [
                "qianxing-strategy-diagnose", "--input", str(input_path),
                "--worker-executable", sys.executable, "--worker-arg", str(worker),
                "--pit-ledger", str(ledger_path), "--startup-windows", "1,3,5",
                "--max-lookahead-checks", "4",
            ]
            output = io.StringIO()
            with patch.dict(os.environ, {"PYTHONPATH": self.python_path}), patch.object(sys, "argv", argv), contextlib.redirect_stdout(output):
                self.assertEqual(main(), 0)
            report = json.loads(output.getvalue())
            self.assertEqual(report["data_visibility"]["status"], "potential_pit_bias_detected")
            self.assertEqual(report["data_visibility"]["decision_points_checked"], 3)
            self.assertTrue(report["data_visibility"]["decision_points_sampled"])


if __name__ == "__main__":
    unittest.main()
