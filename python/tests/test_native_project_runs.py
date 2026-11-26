"""Real project CLI -> fresh PyO3 -> Rust engine -> indexed artifacts/cancel."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
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

    def test_demo_run_renders_latest_report_with_real_cli(self):
        from qianxing import strategy_project

        renderer = os.environ.get("QX_TEST_REPORT_CLI")
        if not renderer:
            self.skipTest("set QX_TEST_REPORT_CLI to the freshly built report renderer")
        demo = Path(self.temp.name) / "latest report demo"
        strategy_project.init_strategy_project(demo, demo=True, rows=80)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as errors:
            code = self.cli.main(["project", "run", str(demo)])
        self.assertEqual(code, 0, errors.getvalue())
        record, = self.runs.project_runs(demo)
        with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
            code = self.cli.main(["project", "report", str(demo), "--cli", renderer])
        log = self.runs._directory(demo, record["project_run_id"]) / "report.log"
        detail = log.read_text(encoding="utf-8") if log.exists() else ""
        self.assertEqual(code, 0, errors.getvalue() + detail)
        report = json.loads(output.getvalue())
        self.assertEqual(report["project_run_id"], record["project_run_id"])
        self.assertEqual(report["status"], "succeeded")
        self.assertTrue(Path(report["report_paths"]["html"]).is_file())

    def test_python_quickstart_runs_sample_preflight_engine_and_verified_report(self):
        renderer = os.environ.get("QX_TEST_REPORT_CLI")
        if not renderer:
            self.skipTest("set QX_TEST_REPORT_CLI to the freshly built report renderer")
        project = Path(self.temp.name) / "python quickstart"
        with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
            code = self.cli.main([
                "quickstart", str(project), "--rows", "80", "--format", "html,json", "--cli", renderer,
            ])
        self.assertEqual(code, 0, errors.getvalue())
        result = json.loads(output.getvalue())
        self.assertEqual(result["status"], "succeeded")
        self.assertEqual(result["data"]["source"], "sample")
        self.assertTrue(result["data"]["is_sample"])
        self.assertEqual(result["data"]["rows"], 80)
        self.assertEqual(result["preflight"]["status"], "ready")
        self.assertTrue(result["artifacts_verified"])
        self.assertEqual(result["report"]["status"], "succeeded")
        self.assertTrue(Path(result["report"]["report_paths"]["html"]).is_file())
        self.assertTrue(result["run_evidence"]["integrity_verified"])
        self.assertTrue(result["run_evidence"]["input_identity_recomputed"])
        self.assertTrue(Path(result["run_evidence"]["bundle_path"]).is_file())
        self.assertTrue(Path(result["report"]["report_artifacts"]["json"]["path"]).is_file())
        record = self.runs.project_run_status(project, result["project_run_id"])
        self.assertEqual(record["status"], "succeeded")
        self.assertTrue(self.runs.verify_project_artifacts(project, result["project_run_id"])["verified"])

    def test_python_quickstart_runs_bundled_local_paper_lifecycle(self):
        renderer = os.environ.get("QX_TEST_REPORT_CLI")
        if not renderer:
            self.skipTest("set QX_TEST_REPORT_CLI to the freshly built report renderer")
        project = Path(self.temp.name) / "python paper quickstart"
        with contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as errors:
            code = self.cli.main([
                "quickstart", str(project), "--profile", "paper", "--cli", renderer,
            ])
        self.assertEqual(code, 0, errors.getvalue())
        result = json.loads(output.getvalue())
        self.assertEqual(result["status"], "succeeded")
        self.assertEqual(result["profile"], "paper")
        self.assertEqual(result["data"]["source"], "local_synthetic_l1")
        self.assertFalse(result["data"]["network_accessed"])
        self.assertFalse(result["data"]["credentials_loaded"])
        self.assertFalse(result["data"]["live_orders_submitted"])
        self.assertGreater(result["paper"]["new_orders"], 0)
        self.assertGreater(result["paper"]["new_ledger_entries"], 0)
        self.assertTrue((project / "qianxing.runtime.json").is_file())

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

    def test_sample_project_with_less_than_one_unit_of_cash_can_trade(self):
        from qianxing import strategy_project
        from qianxing.reports import verify_run

        demo = Path(self.temp.name) / "fractional sample"
        strategy_project.init_strategy_project(demo, demo=True, cash="50")
        snapshot = json.loads((demo / "data" / "snapshot.json").read_text(encoding="utf-8"))
        self.assertEqual((snapshot["price_precision"], snapshot["quantity_precision"]), (9, 9))
        output, errors = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            code = self.cli.main(["project", "run", str(demo)])
        self.assertEqual(code, 0, errors.getvalue())
        result = json.loads(output.getvalue())
        self.assertGreater(result["fills"], 0)
        self.assertTrue(verify_run(result).verified)

    def test_demo_reinitialization_rolls_back_data_on_install_failure(self):
        from qianxing import strategy_project
        from qianxing.dataset_bundle import verify_bundle

        demo = Path(self.temp.name) / "recoverable demo"
        strategy_project.init_strategy_project(demo, demo=True, rows=30)
        demo = demo.resolve()
        extra = demo / "data" / "personal-notes.txt"
        extra.write_text("keep me", encoding="utf-8")
        paths = [demo / "input.json", demo / "qianxing.strategy.json", demo / "strategies" / "main.py",
                 demo / ".gitignore", *sorted((demo / "data").iterdir())]
        original = {p: p.read_bytes() for p in paths}

        with patch.object(strategy_project, "_write_sample_dataset", side_effect=OSError("generation failed")):
            with self.assertRaisesRegex(OSError, "generation failed"):
                strategy_project.init_strategy_project(demo, demo=True, rows=40, force=True)
        self.assertEqual(original, {p: p.read_bytes() for p in paths})

        replace = os.replace
        calls = 0
        def fail_once(source, target):
            nonlocal calls
            calls += 1
            if calls == 4:
                raise OSError("install failed")
            return replace(source, target)

        with patch.object(strategy_project.os, "replace", side_effect=fail_once):
            with self.assertRaisesRegex(OSError, "install failed"):
                strategy_project.init_strategy_project(demo, demo=True, rows=40, force=True)
        self.assertEqual(original, {p: p.read_bytes() for p in paths})
        self.assertEqual(verify_bundle(demo / "data")["rows"], 30)
        self.assertFalse(list(demo.glob(".qianxing-init-*")))

        failed = False
        def fail_manifest_once(source, target):
            nonlocal failed
            if Path(target) == demo / "qianxing.strategy.json" and not failed:
                failed = True
                raise OSError("manifest installation failed")
            return replace(source, target)

        with patch.object(strategy_project.os, "replace", side_effect=fail_manifest_once):
            with self.assertRaisesRegex(OSError, "manifest installation failed"):
                strategy_project.init_strategy_project(demo, demo=True, rows=40, force=True)
        self.assertTrue(failed)
        self.assertEqual(original, {p: p.read_bytes() for p in paths})
        self.assertEqual(verify_bundle(demo / "data")["rows"], 30)
        self.assertFalse(list(demo.glob(".qianxing-init-*")))

        strategy_project.init_strategy_project(demo, demo=True, rows=40, force=True)
        self.assertEqual(verify_bundle(demo / "data")["rows"], 40)
        self.assertEqual(extra.read_text(encoding="utf-8"), "keep me")

    def test_failed_rollback_retains_mapped_backups(self):
        from qianxing import strategy_project

        demo = (Path(self.temp.name) / "manual recovery").resolve()
        strategy_project.init_strategy_project(demo, demo=True, rows=30)
        original_input = (demo / "input.json").read_bytes()
        replace = os.replace
        installation_failed = False

        def fail_install_and_restore(source, target):
            nonlocal installation_failed
            if Path(target) == demo / "qianxing.strategy.json" and not installation_failed:
                installation_failed = True
                raise OSError("install failed")
            if installation_failed and Path(target) == demo / "input.json":
                raise OSError("restore failed")
            return replace(source, target)

        with patch.object(strategy_project.os, "replace", side_effect=fail_install_and_restore):
            with self.assertRaisesRegex(RuntimeError, "recovery files retained"):
                strategy_project.init_strategy_project(demo, demo=True, rows=40, force=True)
        staging, = demo.glob(".qianxing-init-*")
        recovery = json.loads((staging / "recovery.json").read_text(encoding="utf-8"))
        input_entry, = (item for item in recovery if item["target"] == "input.json")
        self.assertTrue(input_entry["originally_present"])
        self.assertEqual((staging / input_entry["backup"]).read_bytes(), original_input)

    def test_process_interruption_blocks_reuse_and_exposes_read_only_recovery_info(self):
        from qianxing import strategy_project

        demo = (Path(self.temp.name) / "interrupted demo").resolve()
        strategy_project.init_strategy_project(demo, demo=True, rows=30)
        script = '''
import os, sys
from qianxing import strategy_project
replace = os.replace
calls = 0
def crash(source, target):
    global calls
    calls += 1
    if calls == 4:
        os._exit(86)
    return replace(source, target)
strategy_project.os.replace = crash
strategy_project.init_strategy_project(sys.argv[1], demo=True, rows=40, force=True)
'''
        env = os.environ.copy()
        python_root = str(Path(__file__).resolve().parents[1])
        env["PYTHONPATH"] = python_root + os.pathsep + env.get("PYTHONPATH", "")
        child = subprocess.run([sys.executable, "-c", script, str(demo)], env=env, timeout=60)
        self.assertEqual(child.returncode, 86)
        with self.assertRaisesRegex(ValueError, "unfinished project initialization"):
            strategy_project.load_strategy_project(demo)
        with self.assertRaisesRegex(ValueError, "unfinished project initialization"):
            strategy_project.init_strategy_project(demo, demo=True, force=True)
        with self.assertRaisesRegex(ValueError, "unfinished project initialization"):
            strategy_project.load_strategy_project(demo / "qianxing.strategy.json")
        before = {path: path.read_bytes() for path in demo.rglob("*") if path.is_file()}
        output, errors = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            code = self.cli.main(["project", "init-recovery-info", str(demo)])
        self.assertEqual(code, 0, errors.getvalue())
        result = json.loads(output.getvalue())
        pending, = result["pending"]
        self.assertEqual(pending["status"], "manual_review_required")
        self.assertTrue(any(entry["backup_present"] for entry in pending["entries"]))
        self.assertEqual(before, {path: path.read_bytes() for path in demo.rglob("*") if path.is_file()})

        stage = Path(pending["directory"])
        journal = stage / "recovery.json"
        entries = json.loads(journal.read_text(encoding="utf-8"))
        entries[0]["target"] = "../outside"
        journal.write_text(json.dumps(entries), encoding="utf-8")
        self.assertEqual(strategy_project.project_init_recovery_info(demo)["pending"][0]["status"],
                         "invalid_recovery_mapping")

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
