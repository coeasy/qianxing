"""Real project CLI -> fresh PyO3 -> Rust engine -> indexed artifacts/cancel."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


class NativeProjectRunTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = os.environ.get("QX_TEST_NATIVE_LIBRARY")
        if not path:
            raise unittest.SkipTest("set QX_TEST_NATIVE_LIBRARY to the freshly built extension")
        name = "qianxing_bridge._qianxing_native"
        loader = importlib.machinery.ExtensionFileLoader(name, str(Path(path).resolve(strict=True)))
        spec = importlib.util.spec_from_file_location(name, path, loader=loader)
        native = importlib.util.module_from_spec(spec)
        loader.exec_module(native)
        cls.modules = patch.dict(sys.modules, {name: native})
        cls.modules.start()
        cls.addClassCleanup(cls.modules.stop)

    def setUp(self):
        from qianxing import cli, project_runs, strategy_project
        self.cli, self.runs = cli, project_runs
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "project with spaces"
        strategy_project.init_strategy_project(self.root)
        scale = 10**9
        rows = 100
        (self.root / "bars.json").write_text(json.dumps({
            "schema_version": 1, "instrument": "BTCUSDT.BINANCE", "source": "project-test",
            "ts": [1000 * (index + 1) for index in range(rows)],
            "open_raw": [100 * scale] * rows, "high_raw": [101 * scale] * rows,
            "low_raw": [99 * scale] * rows, "close_raw": [100 * scale] * rows,
            "volume_raw": [scale] * rows,
        }), encoding="utf-8")
        (self.root / "input.json").write_text(json.dumps({
            "schema_version": 1, "run_id": "project-native", "instrument": "BTCUSDT.BINANCE",
            "bars_path": "bars.json", "settlement_currency": "USDT",
            "initial_cash_raw": 100000 * scale, "seed": 20261028, "output_dir": "user-output",
            "strategy": {"kind": "sma_cross", "strategy_id": "my-strategy",
                         "quantity_raw": scale, "fast_window": 5, "slow_window": 20},
        }), encoding="utf-8")
        manifest_path = self.root / strategy_project.PROJECT_FILE
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["input_spec"] = "input.json"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    def execute(self):
        with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
            code = self.cli.main(["project", "run", str(self.root)])
        return code, output.getvalue(), errors.getvalue()

    def test_native_success_verifies_replay_and_isolates_repeated_run_artifacts(self):
        from qianxing.app import verify_run
        previous = Path.cwd()
        for _ in range(2):
            code, output, errors = self.execute()
            self.assertEqual(code, 0, errors)
            outcome = json.loads(output)
            self.assertTrue(verify_run(outcome).verified)
        self.assertEqual(Path.cwd(), previous)
        records = self.runs.project_runs(self.root)
        self.assertEqual(len(records), 2)
        self.assertNotEqual(records[0]["result_path"], records[1]["result_path"])
        self.assertFalse((self.root / "user-output").exists())
        for record in records:
            self.assertEqual(record["status"], "succeeded")
            self.assertTrue(self.runs.verify_project_artifacts(self.root, record["project_run_id"])["verified"])
            self.assertTrue(self.runs.project_run_events(self.root, record["project_run_id"]))
        artifact = next(iter(records[0]["artifacts"].values()))
        Path(artifact["path"]).write_text("changed", encoding="utf-8")
        self.assertFalse(self.runs.verify_project_artifacts(self.root, records[0]["project_run_id"])["verified"])

        from qianxing import strategy_project
        from qianxing.dataset_bundle import verify_bundle
        demo = Path(self.temp.name) / "offline demo"
        with patch("qianxing_ashare.fetch_bars", side_effect=AssertionError("offline demo attempted a fetch")):
            strategy_project.init_strategy_project(demo, demo=True, source="sample")
            manifest = json.loads((demo / strategy_project.PROJECT_FILE).read_text(encoding="utf-8"))
            self.assertEqual(manifest["data"], {"source": "sample"})
            spec_path = demo / "input.json"
            spec = json.loads(spec_path.read_text(encoding="utf-8"))
            for key in ("bars_path", "dataset_snapshot_path", "output_dir"):
                self.assertFalse(Path(spec[key]).is_absolute())
            original = spec_path.read_bytes()
            with patch.dict(os.environ, {"QIANXING_DATA_SOURCE": "baostock"}):
                plan = strategy_project.local_project_preflight(demo, explain=True)
            self.assertEqual(plan["data_selection"]["configuration"]["data.source"]["value"], "sample")
            self.assertEqual(plan["data_selection"]["configuration"]["data.source"]["source"], "manifest")
            self.assertEqual(len(plan["checked_markets"]), 1)
            self.assertEqual(len(plan["checked_markets"][0]["dataset_snapshot_sha256"]), 64)
            self.assertTrue(plan["datasets"][0]["is_sample"])
            self.assertTrue(strategy_project.show_strategy_project(demo)["datasets"][0]["is_sample"])
            with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
                code = self.cli.main(["project", "run", str(demo)])
            self.assertEqual(code, 0, errors.getvalue())
            self.assertTrue(verify_run(json.loads(output.getvalue())).verified)
            record, = self.runs.project_runs(demo)
            self.assertTrue(self.runs.verify_project_artifacts(demo, record["project_run_id"])["verified"])
            self.assertEqual(spec_path.read_bytes(), original)

            source = demo / "strategies" / "main.py"
            marker = Path(self.temp.name) / "imports.txt"
            source.write_text(source.read_text(encoding="utf-8") + (
                "\nfrom pathlib import Path\n"
                f"with Path({str(marker)!r}).open('a') as handle: handle.write('import\\n')\n"
            ), encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
                code = self.cli.main(["run", "-f", str(source), "--data", str(demo / "data"), "-o", str(demo / "shorthand")])
            self.assertEqual(code, 0, errors.getvalue())
            short_result = json.loads(output.getvalue())
            self.assertTrue(verify_run(short_result).verified)
            self.assertEqual(marker.read_text().splitlines(), ["import"])
            generated, = (demo / "shorthand").glob("*/input.json")
            with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
                code = self.cli.main(["run", "-f", str(source), "--spec", str(generated)])
            self.assertEqual(code, 0, errors.getvalue())
            explicit_result = json.loads(output.getvalue())
            self.assertEqual(short_result["result_hash"], explicit_result["result_hash"])
            self.assertTrue(verify_run(explicit_result).verified)
            verify_bundle(demo / "data")

            bars_path = demo / "data" / "bars.json"
            bars_path.write_bytes(bars_path.read_bytes() + b"\n")
            with self.assertRaisesRegex(Exception, "does not bind|mismatched"):
                strategy_project.local_project_preflight(demo)

    def test_native_cancellation_is_terminal_without_successful_outcome(self):
        source = self.root / "strategies" / "main.py"
        # Runs execute the frozen strategy snapshot, so __file__ is not the project root.
        source.write_text(f'''from pathlib import Path
import time
from qianxing_strategy import BaseStrategy
from qianxing.project_runs import project_runs, cancel_project_run
ROOT = Path({str(self.root)!r})
class Strategy(BaseStrategy):
    STRATEGY_ID = "my-strategy"
    STRATEGY_VERSION = "0.1.0"
    def on_bar(self, context, bar):
        record = project_runs(ROOT)[0]
        cancel_project_run(ROOT, record["project_run_id"])
        time.sleep(0.02)
        return None
STRATEGY_CLASS = Strategy
''', encoding="utf-8")
        code, output, errors = self.execute()
        self.assertEqual(code, 130, errors)
        record, = self.runs.project_runs(self.root)
        self.assertEqual(record["status"], "cancelled")
        self.assertNotIn("result_path", record)
        self.assertEqual(output, "")

    def test_server_plan_uses_real_bridge_without_importing_strategy(self):
        from qianxing.strategy_project import server_project_plan
        runtime_source = Path(__file__).resolve().parents[2] / "deploy" / "qianxing.runtime.strategy-backtest.example.json"
        runtime = json.loads(runtime_source.read_text(encoding="utf-8"))
        (self.root / "runtime.json").write_text(json.dumps(runtime), encoding="utf-8")
        (self.root / "strategies" / "main.py").write_text("raise RuntimeError('must not execute')\n", encoding="utf-8")
        result = server_project_plan(self.root, runtime_config="runtime.json", worker_id="strategy-backtest")
        self.assertEqual(result["worker"]["id"], "strategy-backtest")
        self.assertEqual(result["strategy_host"], "python_module")
        self.assertFalse(result["readiness"]["trading_enabled"])
        self.assertFalse(result["strategy_artifact_binding"]["user_code_executed"])


if __name__ == "__main__":
    unittest.main()
