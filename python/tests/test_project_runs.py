"""Local project receipts, cancellation races and execution routing."""
import base64
import contextlib
import csv
import ctypes
import hashlib
import io
import json
import os
from importlib import metadata as importlib_metadata
from pathlib import Path
import sys
import tempfile
import types
import unittest
import zipfile
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing import cli, project_runs as runs, strategy_project as project


class Handle:
    run_id = "engine-id"

    def __init__(self, state="succeeded", result=None):
        self.state = state
        self.cancelled = False
        self.outcome = result if result is not None else {"artifacts": {}}

    def cancel(self):
        self.cancelled = True
        self.state = "cancelled"

    def wait(self, timeout):
        return self.state

    def result(self):
        if self.state == "failed":
            raise ValueError("engine failure")
        return None if self.state == "cancelled" else self.outcome

    def events(self, cursor):
        return [{"seq": 1, "run_id": self.run_id, "kind": self.state,
                 "status": self.state}] if cursor == 0 else []


class ProjectRunTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = {"project_id": "demo"}
        self.loader = patch.object(project, "load_strategy_project", return_value=(self.root, self.manifest))
        self.loader.start()
        self.addCleanup(self.loader.stop)
        self.output = io.StringIO()
        self.stderr = contextlib.redirect_stderr(self.output)
        self.stderr.__enter__()
        self.addCleanup(self.stderr.__exit__, None, None, None)

    def test_receipt_indexes_artifacts_and_engine_events_without_changing_cwd(self):
        artifact = self.root / "equity.csv"
        artifact.write_text("equity\n1\n", encoding="utf-8")
        outcome = {"artifacts": {"equity": "equity.csv"}}
        previous = Path.cwd()
        def execute(root, manifest):
            self.assertEqual(Path.cwd(), previous)
            result = runs.wait_project_run(lambda: Handle(result=outcome), lambda: self.fail())
            runs.record_project_result(result)
            return 0
        self.assertEqual(runs.run_project(self.root, execute), 0)
        self.assertEqual(Path.cwd(), previous)
        record, = runs.project_runs(self.root)
        self.assertEqual(record["status"], "succeeded")
        self.assertEqual(record["engine_run_id"], "engine-id")
        self.assertEqual(record["artifacts"]["equity"]["sha256"], hashlib.sha256(artifact.read_bytes()).hexdigest())
        self.assertEqual(len(runs.project_run_events(self.root, record["project_run_id"])), 1)
        self.assertTrue(Path(record["result_path"]).is_file())

    def test_receipt_write_retries_a_windows_sharing_violation(self):
        target = self.root / "run.json"
        swaps = []
        real_replace = os.replace

        def locked_twice(source, destination):
            swaps.append(source)
            if len(swaps) < 3:
                raise PermissionError(5, "Access is denied")
            real_replace(source, destination)

        with patch.object(runs.os, "name", "nt"), patch.object(runs.os, "replace", side_effect=locked_twice), \
             patch.object(runs.time, "sleep") as sleep:
            runs._write(target, {"status": "running"})

        self.assertEqual(json.loads(target.read_text(encoding="utf-8")), {"status": "running"})
        self.assertEqual(len(swaps), 3)
        self.assertEqual(sleep.call_count, 2)
        self.assertEqual([path.name for path in self.root.glob("run.json.*.tmp")], [])

    def test_receipt_write_gives_up_after_the_retry_budget_without_losing_the_receipt(self):
        target = self.root / "run.json"
        target.write_text('{"status": "preparing"}\n', encoding="utf-8")

        def always_locked(source, destination):
            raise PermissionError(5, "Access is denied")

        with patch.object(runs.os, "name", "nt"), patch.object(runs.os, "replace", side_effect=always_locked), \
             patch.object(runs.time, "sleep") as sleep:
            with self.assertRaises(PermissionError):
                runs._write(target, {"status": "running"})

        self.assertEqual(sleep.call_count, 11)
        self.assertEqual(json.loads(target.read_text(encoding="utf-8")), {"status": "preparing"})
        self.assertEqual([path.name for path in self.root.glob("run.json.*.tmp")], [])

    def test_posix_never_retries_a_replace_permission_error(self):
        target = self.root / "run.json"

        def always_locked(source, destination):
            raise PermissionError(13, "Permission denied")

        with patch.object(runs.os, "name", "posix"), patch.object(runs.os, "replace", side_effect=always_locked), \
             patch.object(runs.time, "sleep") as sleep:
            with self.assertRaises(PermissionError):
                runs._write(target, {"status": "running"})

        sleep.assert_not_called()

    def test_run_captures_bounded_stdout_and_stderr_apart_from_events(self):
        artifact = self.root / "equity.csv"
        artifact.write_text("equity\n1\n", encoding="utf-8")
        outcome = {"artifacts": {"equity": "equity.csv"}}
        console = io.StringIO()

        def execute(root, manifest):
            print("engine stdout line")
            print("engine stderr line", file=sys.stderr)
            result = runs.wait_project_run(lambda: Handle(result=outcome), lambda: self.fail())
            runs.record_project_result(result)
            return 0

        with contextlib.redirect_stdout(console):
            self.assertEqual(runs.run_project(self.root, execute), 0)
        self.assertIn("engine stdout line", console.getvalue())
        self.assertIn("engine stderr line", self.output.getvalue())
        record, = runs.project_runs(self.root)
        logs = record["output_logs"]
        stdout_log = Path(logs["stdout"]["path"])
        stderr_log = Path(logs["stderr"]["path"])
        self.assertEqual((stdout_log.name, stderr_log.name), ("stdout.log", "stderr.log"))
        self.assertIn("engine stdout line", stdout_log.read_text(encoding="utf-8"))
        self.assertNotIn("engine stderr line", stdout_log.read_text(encoding="utf-8"))
        self.assertIn("engine stderr line", stderr_log.read_text(encoding="utf-8"))
        self.assertEqual(
            logs["stdout"]["sha256"], hashlib.sha256(stdout_log.read_bytes()).hexdigest()
        )
        self.assertEqual(logs["stdout"]["bytes"], stdout_log.stat().st_size)
        self.assertFalse(logs["stdout"]["truncated"])
        events = (runs._directory(self.root, record["project_run_id"]) / "events.jsonl").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("engine stdout line", events)
        self.assertNotIn("engine stderr line", events)

    def test_run_output_log_stops_at_the_configured_limit(self):
        artifact = self.root / "equity.csv"
        artifact.write_text("equity\n1\n", encoding="utf-8")
        outcome = {"artifacts": {"equity": "equity.csv"}}

        def execute(root, manifest):
            print("x" * 64)
            result = runs.wait_project_run(lambda: Handle(result=outcome), lambda: self.fail())
            runs.record_project_result(result)
            return 0

        with patch.object(runs, "_OUTPUT_LOG_LIMIT", 16):
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(runs.run_project(self.root, execute), 0)
        record, = runs.project_runs(self.root)
        logs = record["output_logs"]["stdout"]
        log = Path(logs["path"])
        self.assertTrue(logs["truncated"])
        self.assertEqual(log.stat().st_size, 16 + len(runs._OUTPUT_TRUNCATION_NOTICE))
        self.assertEqual(log.read_bytes()[:16], b"x" * 16)

    def test_cancel_before_start_does_not_invoke_callback_engine(self):
        def execute(root, manifest):
            record, = runs.project_runs(root)
            runs.cancel_project_run(root, record["project_run_id"])
            return runs.wait_project_run(lambda: self.fail("must not start"), lambda: self.fail())
        self.assertEqual(runs.run_project(self.root, execute), 130)
        self.assertEqual(runs.project_runs(self.root)[0]["status"], "cancelled")

    def test_cancel_while_running_waits_for_native_terminal_state(self):
        run = runs.ProjectRun(self.root, "demo")
        handle = Handle(state="running")
        def wait(timeout):
            if not handle.cancelled:
                runs.cancel_project_run(self.root, run.run_id)
                return "running"
            return "cancelled"
        handle.wait = wait
        with self.assertRaises(runs.ProjectRunCancelled):
            run.wait(handle)
        self.assertTrue(handle.cancelled)

    def test_failure_and_preflight_error_are_durable_and_context_resets(self):
        def execute(root, manifest):
            return runs.wait_project_run(lambda: Handle(state="failed"), lambda: self.fail())
        with self.assertRaisesRegex(ValueError, "engine failure"):
            runs.run_project(self.root, execute)
        record, = runs.project_runs(self.root)
        self.assertEqual(record["status"], "failed")
        error = record["error"]
        self.assertEqual(error["type"], "ValueError")
        self.assertEqual(error["code"], "INVALID_INPUT")
        self.assertEqual(error["category"], "INVALID_INPUT")
        self.assertFalse(error["retryable"])
        self.assertEqual(error["stage"], "execute")
        self.assertEqual(error["project_run_id"], record["project_run_id"])
        self.assertEqual(error["correlation_id"], record["project_run_id"])
        self.assertEqual(error["cause"], "engine failure")
        self.assertTrue(error["remediation"])
        self.assertIsNone(runs._ACTIVE.get())

    def test_keyboard_interrupt_requests_rust_cancel(self):
        run = runs.ProjectRun(self.root, "demo")
        handle = Handle(state="running")
        def wait(timeout):
            if not handle.cancelled:
                raise KeyboardInterrupt()
            return "cancelled"
        handle.wait = wait
        with self.assertRaises(runs.ProjectRunCancelled):
            run.wait(handle)
        self.assertTrue(handle.cancelled)

    def test_cancel_terminal_run_does_not_relabel_success(self):
        def execute(root, _manifest):
            artifact = root / "result.txt"
            artifact.write_text("verified result", encoding="utf-8")
            runs.record_project_result({"artifacts": {"result": str(artifact)}})
            return 0
        runs.run_project(self.root, execute)
        record, = runs.project_runs(self.root)
        status = runs.cancel_project_run(self.root, record["project_run_id"])
        self.assertEqual(status["status"], "succeeded")
        self.assertFalse(status["cancellation_requested"])

    def test_success_without_a_complete_artifact_set_is_recorded_as_failure(self):
        code = runs.run_project(self.root, lambda _root, _manifest: 0)
        record, = runs.project_runs(self.root)
        self.assertEqual(code, 1)
        self.assertEqual(record["status"], "failed")
        self.assertEqual(record["error"]["category"], "ARTIFACT_INTEGRITY_INCOMPLETE")
        self.assertEqual(record["error"]["checks"]["artifact_manifest"], "no_required_artifacts")

    def test_artifact_removed_after_indexing_prevents_success_terminal_state(self):
        def execute(root, _manifest):
            artifact = root / "result.txt"
            artifact.write_text("verified result", encoding="utf-8")
            runs.record_project_result({"artifacts": {"result": str(artifact)}})
            artifact.unlink()
            return 0
        code = runs.run_project(self.root, execute)
        record, = runs.project_runs(self.root)
        self.assertEqual(code, 1)
        self.assertEqual(record["status"], "failed")
        self.assertEqual(record["error"]["checks"]["result"], "missing")

    def test_corrupt_receipt_is_reported_as_unreadable(self):
        run = runs.ProjectRun(self.root, "demo")
        (run.directory / "run.json").write_text("{broken", encoding="utf-8")
        status = runs.project_run_status(self.root, run.run_id)
        self.assertEqual(status["status"], "corrupt")
        self.assertEqual(status["error"]["category"], "RUN_RECEIPT_UNREADABLE")
        integrity = runs.verify_project_artifacts(self.root, run.run_id)
        self.assertFalse(integrity["verified"])
        self.assertEqual(integrity["artifacts"]["run_receipt"], "RUN_RECEIPT_UNREADABLE")
        self.assertEqual(runs.project_runs(self.root)[0]["status"], "corrupt")

    def test_macos_process_identity_uses_process_start_time(self):
        process = types.SimpleNamespace(returncode=0, stdout="Tue Oct 28 10:20:30 2026\n")
        with patch("qianxing.project_runs.os.name", "posix"), \
             patch("qianxing.project_runs.platform.system", return_value="Darwin"), \
             patch.object(runs, "_darwin_process_start_time", return_value=None), \
             patch("qianxing.project_runs.subprocess.run", return_value=process):
            identity = runs._process_identity(42)
        self.assertEqual(identity["process_start"], "Tue Oct 28 10:20:30 2026")
        self.assertEqual(identity["process_start_source"], "ps_lstart")
        self.assertEqual(identity["process_start_precision"], "one_second")

    def test_darwin_proc_pidinfo_reads_microsecond_start_fields(self):
        class ProcPidInfo:
            argtypes = None
            restype = None

            def __call__(self, pid, flavor, arg, output, output_size):
                self.asserted = (pid, flavor, arg, output_size)
                fields = ctypes.cast(output, ctypes.POINTER(ctypes.c_uint64))
                fields[15] = 1_798_876_543
                fields[16] = 654_321
                return output_size

        proc_pidinfo = ProcPidInfo()
        libproc = types.SimpleNamespace(proc_pidinfo=proc_pidinfo)
        with patch("ctypes.CDLL", return_value=libproc):
            self.assertEqual(
                runs._darwin_process_start_time(42), (1_798_876_543, 654_321)
            )
        self.assertEqual(proc_pidinfo.asserted, (42, 3, 0, 136))

    def test_macos_process_identity_prefers_microsecond_start_time(self):
        with patch("qianxing.project_runs.os.name", "posix"), \
             patch("qianxing.project_runs.platform.system", return_value="Darwin"), \
             patch.object(runs, "_darwin_process_start_time", return_value=(1_798_876_543, 654_321)), \
             patch("qianxing.project_runs.subprocess.run") as process:
            identity = runs._process_identity(42)
        process.assert_not_called()
        self.assertEqual(identity["process_start"], "1798876543.654321")
        self.assertEqual(identity["process_start_source"], "proc_pidinfo")
        self.assertEqual(identity["process_start_precision"], "one_microsecond")

    def test_reused_pid_with_different_start_identity_is_not_alive(self):
        with patch("qianxing.project_runs.os.name", "posix"), \
             patch("qianxing.project_runs.platform.system", return_value="Linux"), \
             patch("qianxing.project_runs.os.kill"), \
             patch("qianxing.project_runs._process_identity", return_value={"process_start": "new"}):
            self.assertFalse(runs._process_alive(42, {"process_start": "old"}))

    def test_run_id_traversal_rejected_and_event_tail_bounded(self):
        for bad in ("../run", "x", "a" * 33):
            with self.assertRaises(ValueError):
                runs.project_run_status(self.root, bad)
        run = runs.ProjectRun(self.root, "demo")
        for seq in range(10):
            run.event({"seq": seq})
        self.assertEqual(runs.project_run_events(self.root, run.run_id, 2), [{"seq": 8}, {"seq": 9}])
        with (run.directory / "events.jsonl").open("a") as stream:
            stream.write('{"incomplete":')
        self.assertEqual(runs.project_run_events(self.root, run.run_id, 2), [{"seq": 9}])

    def test_missing_owner_is_interrupted_not_completed(self):
        run = runs.ProjectRun(self.root, "demo")
        with patch.object(runs, "_process_alive", return_value=False):
            self.assertEqual(runs.project_run_status(self.root, run.run_id)["status"], "interrupted")
        self.assertTrue(runs._process_alive(os.getpid()))

    def test_project_cli_routes_to_cancellable_engine_and_retains_stdout(self):
        source = self.root / "strategy.py"
        source.write_text("STRATEGY_ID = 'demo'\ndef create_strategy(spec):\n    return object()\n", encoding="utf-8")
        spec = self.root / "spec.json"
        spec.write_text('{"run_id":"demo","strategy":{}}', encoding="utf-8")
        summary = self.root / "summary.json"
        summary.write_text("{}", encoding="utf-8")
        self.manifest.update(strategy_file=str(source), input_spec=str(spec))
        from qianxing import strategy_execution
        def preflight(_root, *, input_spec_override, strategy_file_override):
            self.assertTrue(Path(strategy_file_override).is_relative_to(self.root / ".qianxing" / "runs"))
            active = runs._ACTIVE.get()
            source.write_text("raise RuntimeError('source changed after snapshot')\n", encoding="utf-8")
            return {
                "strategy_file": str(strategy_file_override),
                "strategy_bundle_sha256": active.record["strategy_bundle_sha256"],
                "input_spec": str(input_spec_override),
                "input_spec_sha256": active.record["preflight_input_sha256"],
            }

        outcome = {"artifacts": {"summary": str(summary)}}
        with patch.object(cli, "local_project_preflight", side_effect=preflight), patch.object(strategy_execution, "start_backtest_with_strategy", return_value=Handle(result=outcome)) as start, patch.object(strategy_execution, "validate_strategy_execution_preflight_v1", return_value={}), contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(cli.main(["project", "run", str(self.root)]), 0)
        start.assert_called_once()
        output_document = json.loads(output.getvalue())
        self.assertEqual(set(output_document["artifacts"]), {"summary"})
        self.assertTrue(Path(output_document["artifacts"]["summary"]).is_file())
        record = runs.project_runs(self.root)[0]
        self.assertEqual(record["status"], "succeeded")
        self.assertTrue(record["strategy_snapshots"])
        self.assertEqual(
            runs.verify_project_artifacts(self.root, record["project_run_id"])["artifacts"]["strategy_bundle"],
            "matched",
        )

    def test_project_strategy_bundle_is_frozen_and_integrity_checked(self):
        dependency = self.root / "helper.py"
        dependency.write_text("VALUE = 7\n", encoding="utf-8")
        strategy = self.root / "strategy.py"
        strategy.write_text(
            "import helper\nSTRATEGY_DEPENDENCIES = ('helper.py',)\n"
            "def create_strategy(spec):\n    return helper.VALUE\n",
            encoding="utf-8",
        )
        snapshots = {}

        def execute(root, _manifest):
            snapshots["main"] = runs.snapshot_project_strategy(root, strategy)
            artifact = root / "result.txt"
            artifact.write_text("verified result", encoding="utf-8")
            runs.record_project_result({"artifacts": {"result": str(artifact)}})
            return 0

        self.assertEqual(runs.run_project(self.root, execute), 0)
        record = runs.project_runs(self.root)[0]
        strategy.write_text("changed\n", encoding="utf-8")
        dependency.write_text("VALUE = 999\n", encoding="utf-8")
        self.assertEqual(snapshots["main"].read_text(encoding="utf-8").splitlines()[0], "import helper")
        self.assertEqual(
            (snapshots["main"].parent / "helper.py").read_text(encoding="utf-8"),
            "VALUE = 7\n",
        )
        integrity = runs.verify_project_artifacts(self.root, record["project_run_id"])
        self.assertEqual(integrity["artifacts"]["strategy:strategy.py"], "matched")
        self.assertEqual(integrity["artifacts"]["strategy:helper.py"], "matched")
        self.assertEqual(integrity["artifacts"]["strategy_bundle"], "matched")
        self.assertEqual(integrity["artifacts"]["strategy_runtime_environment"], "matched")
        self.assertTrue(record["strategy_runtime_environment"]["distributions"])
        (snapshots["main"].parent / "helper.py").write_text("tampered\\n", encoding="utf-8")
        tampered = runs.verify_project_artifacts(self.root, record["project_run_id"])
        self.assertEqual(tampered["artifacts"]["strategy_bundle"], "changed")

    def test_dependency_lock_snapshot_mutation_is_detected(self):
        version = importlib_metadata.version("pip")
        lock = self.root / "requirements.lock"
        lock.write_text(f"pip=={version}\n", encoding="utf-8")
        result = self.root / "result.txt"

        def execute(root, _manifest):
            result.write_text("ok", encoding="utf-8")
            runs.record_project_result({"artifacts": {"result": str(result)}})
            return 0

        with patch.object(runs, "_installed_distribution_versions", return_value={"pip": version}):
            self.assertEqual(runs.run_project(self.root, execute), 0)
            record = runs.project_runs(self.root)[0]
            self.assertEqual(
                runs.verify_project_artifacts(self.root, record["project_run_id"])["artifacts"]["dependency_lock"],
                "matched",
            )
            snapshot = Path(record["dependency_lock"]["snapshot"])
            snapshot.write_text(f"pip=={version}\n# tampered\n", encoding="utf-8")
            integrity = runs.verify_project_artifacts(self.root, record["project_run_id"])
        self.assertEqual(integrity["artifacts"]["dependency_lock"], "changed")
        self.assertFalse(integrity["verified"])

    def test_locked_wheel_snapshot_mutation_is_detected(self):
        version = importlib_metadata.version("pip")
        wheelhouse = self.root / "wheelhouse"
        wheelhouse.mkdir()
        wheel = wheelhouse / f"pip-{version}-py3-none-any.whl"
        metadata_path = f"pip-{version}.dist-info/METADATA"
        metadata = f"Metadata-Version: 2.1\nName: pip\nVersion: {version}\n\n".encode()
        record_path = f"pip-{version}.dist-info/RECORD"
        with zipfile.ZipFile(wheel, "w") as archive:
            archive.writestr(metadata_path, metadata)
            archive.writestr(record_path, f"{metadata_path},sha256={base64.urlsafe_b64encode(hashlib.sha256(metadata).digest()).rstrip(b'=').decode()},\n{record_path},,\n")
        wheel_hash = hashlib.sha256(wheel.read_bytes()).hexdigest()
        requirements = runs._parse_dependency_lock(
            f"pip=={version} --hash=sha256:{wheel_hash}\n".encode()
        )
        run_directory = self.root / "snapshot"
        snapshots = runs._snapshot_locked_wheels(self.root, run_directory, requirements)
        self.assertEqual(len(snapshots), 1)
        wheel_record = snapshots[0]
        record = {
            "dependency_lock": {
                "requirements": requirements,
                "wheelhouse_sha256": runs._dependency_wheelhouse_sha256(snapshots),
            },
            "dependency_wheels": snapshots,
        }
        self.assertEqual(runs._verify_dependency_wheels(record), "matched")
        Path(wheel_record["snapshot"]).write_bytes(b"tampered wheel bytes")
        self.assertEqual(runs._verify_dependency_wheels(record), "changed")

    def test_preflight_runs_in_project_directory_even_from_other_cwd(self):
        previous = Path.cwd()
        def inspect(path, *, input_spec_override=None, strategy_file_override=None, explain=False):
            self.assertEqual(Path.cwd(), previous)
            raise ValueError("expected")
        with patch.object(project, "_local_project_preflight", side_effect=inspect):
            with self.assertRaisesRegex(ValueError, "expected"):
                project.local_project_preflight(self.root)
        self.assertEqual(Path.cwd(), previous)

    def renderer_patch(self):
        """The renderer identity probe is its own case; report commands need a ready path."""
        return patch(
            "qianxing.report_renderer.require_report_renderer",
            return_value={"path": "qx-cli", "status": "ready", "origin": "external",
                          "selection": "explicit", "version": "0.3.0"},
        )

    def test_report_renderer_prefers_the_bundled_copy_and_never_falls_back_silently(self):
        from qianxing import report_renderer
        bundled = self.root / "_bin" / "qx-cli"
        bundled.parent.mkdir(parents=True)
        bundled.write_text("renderer", encoding="utf-8")
        on_path = self.root / "path-qx-cli"
        with patch.object(report_renderer, "_bundled_renderer", return_value=bundled), \
             patch.object(report_renderer.shutil, "which", return_value=str(on_path)):
            path, origin = report_renderer._select_renderer(None, None)
            self.assertEqual((Path(path), origin), (bundled.absolute(), "bundled"))
            explicit = self.root / "explicit-qx-cli"
            path, origin = report_renderer._select_renderer(str(explicit), None)
            self.assertEqual((Path(path), origin), (explicit.resolve(), "external"))
            self.assertEqual(report_renderer._select_renderer("qx-cli", None)[1], "external")
        with patch.object(report_renderer, "_bundled_renderer", return_value=self.root / "absent"), \
             patch.object(report_renderer.shutil, "which", return_value=str(on_path)):
            path, origin = report_renderer._select_renderer(None, None)
            self.assertEqual((Path(path), origin), (on_path.resolve(), "external"))
        with patch.object(report_renderer, "_bundled_renderer", return_value=self.root / "absent"), \
             patch.object(report_renderer.shutil, "which", return_value=None):
            self.assertEqual(report_renderer._select_renderer(None, None), (None, "missing"))
            # An empty --cli is an explicit "no renderer", not a request to use the bundled copy.
            self.assertEqual(report_renderer._select_renderer("", None), (None, "external"))

    def report_run(self):
        summary = self.root / "summary.json"
        summary.write_text("{}", encoding="utf-8")
        def execute(root, manifest):
            runs.record_project_result({"artifacts": {"summary": str(summary)}})
            return 0
        runs.run_project(self.root, execute)
        return runs.project_runs(self.root)[0], summary

    def test_project_report_checks_artifacts_before_spawning_rust(self):
        record, summary = self.report_run()
        summary.write_text("changed", encoding="utf-8")
        with patch("subprocess.run") as process:
            with self.assertRaisesRegex(ValueError, "missing, changed"):
                runs.generate_project_report(self.root, record["project_run_id"])
        process.assert_not_called()

    def test_project_report_refuses_to_overwrite_indexed_summary_or_run_receipt(self):
        record, summary = self.report_run()
        directory = runs._directory(self.root, record["project_run_id"])
        receipt = directory / "run.json"
        for protected in (summary, receipt):
            before = protected.read_bytes()
            with self.subTest(protected=protected), patch("subprocess.run") as process:
                with self.assertRaisesRegex(ValueError, "aliases a project input"):
                    runs.generate_project_report(
                        self.root, record["project_run_id"], output=str(protected)
                    )
                process.assert_not_called()
                self.assertEqual(protected.read_bytes(), before)

    @unittest.skipUnless(hasattr(Path, "symlink_to"), "symbolic links are unavailable")
    def test_project_report_resolves_symlink_alias_to_protected_input(self):
        record, summary = self.report_run()
        alias = self.root / "reports" / "summary-link.html"
        alias.parent.mkdir()
        try:
            alias.symlink_to(summary)
        except OSError as error:
            self.skipTest(f"cannot create symlink in this environment: {error}")
        with patch("subprocess.run") as process:
            with self.assertRaisesRegex(ValueError, "aliases a project input"):
                runs.generate_project_report(self.root, record["project_run_id"], output=str(alias))
        process.assert_not_called()

    def test_project_report_delegates_to_rust_and_retains_run_status(self):
        record, summary = self.report_run()
        target = self.root / "reports with spaces" / "report.html"
        def render(command, **options):
            self.assertEqual(options["cwd"], self.root)
            self.assertEqual(command[:2], ["qx-cli", "report"])
            self.assertEqual(Path(command[2]).resolve(), summary.resolve())
            self.assertEqual(Path(command[-1]).resolve(), target.resolve())
            target.parent.mkdir()
            target.write_text("<!doctype html><title>report</title>", encoding="utf-8")
            return types.SimpleNamespace(returncode=0, stdout="rendered", stderr="")
        with self.renderer_patch(), patch("subprocess.run", side_effect=render):
            result = runs.generate_project_report(self.root, record["project_run_id"], output="reports with spaces/report.html")
        self.assertEqual(result["status"], "succeeded")
        self.assertIsNone(result["strategy_bundle_sha256"])
        self.assertEqual(result["report_sha256"], hashlib.sha256(target.read_bytes()).hexdigest())
        self.assertTrue(Path(result["log_path"]).is_file())
        self.assertEqual(runs.project_run_status(self.root, record["project_run_id"])["status"], "succeeded")

    def test_project_report_fails_if_frozen_strategy_bundle_changes_during_render(self):
        record, _summary = self.report_run()
        target = runs._directory(self.root, record["project_run_id"]) / "report.html"

        def render(_command, **_options):
            target.write_text("<html>report</html>", encoding="utf-8")
            return types.SimpleNamespace(returncode=0, stdout="rendered", stderr="")

        verified = {"project_run_id": record["project_run_id"], "verification": "recorded_artifact_bytes",
                    "verified": True, "artifacts": {"strategy_bundle": "matched"}}
        changed = {"project_run_id": record["project_run_id"], "verification": "recorded_artifact_bytes",
                   "verified": False, "artifacts": {"strategy_bundle": "changed"}}
        with patch("qianxing.project_runs.verify_project_artifacts", side_effect=[verified, changed]), \
             self.renderer_patch(), patch("subprocess.run", side_effect=render):
            with self.assertRaisesRegex(ValueError, "Rust report generation failed"):
                runs.generate_project_report(self.root, record["project_run_id"])
        status_path = runs._directory(self.root, record["project_run_id"]) / "report-status.json"
        status = json.loads(status_path.read_text(encoding="utf-8"))
        self.assertEqual(status["status"], "failed")
        self.assertEqual(status["strategy_bundle_sha256"], "matched")
        self.assertEqual(status["report_sha256"], hashlib.sha256(target.read_bytes()).hexdigest())

    def test_project_artifact_verification_detects_runtime_dependency_environment_change(self):
        record, _summary = self.report_run()
        directory = runs._directory(self.root, record["project_run_id"])
        receipt = json.loads((directory / "run.json").read_text(encoding="utf-8"))
        receipt["strategy_runtime_environment"] = {"sha256": "recorded-environment"}
        runs._write(directory / "run.json", receipt)
        with patch("qianxing.project_runs._strategy_runtime_environment", return_value={"sha256": "changed-environment"}):
            integrity = runs.verify_project_artifacts(self.root, record["project_run_id"])
        self.assertFalse(integrity["verified"])
        self.assertEqual(integrity["artifacts"]["strategy_runtime_environment"], "changed")

    def test_failed_report_is_not_a_failed_backtest(self):
        record, _summary = self.report_run()
        with self.renderer_patch(), patch("subprocess.run", return_value=types.SimpleNamespace(returncode=2, stdout="", stderr="replay mismatch")):
            with self.assertRaisesRegex(ValueError, "Rust report generation failed"):
                runs.generate_project_report(self.root, record["project_run_id"])
        status_path = runs._directory(self.root, record["project_run_id"]) / "report-status.json"
        self.assertEqual(json.loads(status_path.read_text())["status"], "failed")
        self.assertEqual(runs.project_run_status(self.root, record["project_run_id"])["status"], "succeeded")

    def test_error_envelope_keeps_sdk_policy_and_does_not_echo_action_codes(self):
        class AppFailure(RuntimeError):
            payload = {
                "category": "INVALID_INPUT",
                "action": "FIX_INPUT",
                "safe_to_retry": True,
                "message": "spec is invalid",
                "correlation_id": "sdk-correlation",
            }

        envelope = runs.project_error_envelope(
            AppFailure("spec is invalid"), stage="plan", run_id="run-1"
        )
        self.assertEqual(envelope["code"], "INVALID_INPUT")
        self.assertEqual(envelope["category"], "INVALID_INPUT")
        self.assertTrue(envelope["retryable"])
        self.assertEqual(envelope["action"], "FIX_INPUT")
        self.assertNotEqual(envelope["remediation"], "FIX_INPUT")
        self.assertIn("project doctor", envelope["remediation"])
        self.assertEqual(envelope["correlation_id"], "sdk-correlation")
        self.assertEqual(envelope["stage"], "plan")
        self.assertEqual(envelope["project_run_id"], "run-1")
        self.assertEqual(envelope["cause"], "spec is invalid")
        self.assertEqual(envelope["type"], "AppFailure")
        self.assertEqual(envelope["message"], "spec is invalid")

    def test_error_envelope_maps_unclassified_exceptions_without_losing_the_class_name(self):
        envelope = runs.project_error_envelope(RuntimeError("boom"), stage="execute", run_id="run-2")
        self.assertEqual(envelope["code"], "PROJECT_ERROR")
        self.assertFalse(envelope["retryable"])
        self.assertEqual(envelope["correlation_id"], "run-2")
        self.assertEqual(envelope["cause"], "boom")
        self.assertTrue(envelope["remediation"])

    def test_project_report_renders_formats_and_machine_readable_json(self):
        record, summary = self.report_run()
        directory = runs._directory(self.root, record["project_run_id"])
        html_path = directory / "report.html"
        xlsx_path = directory / "report.xlsx"
        json_path = directory / "report.json"
        projection = {"schema_version": 1, "summary": {"run_id": "engine-id"}, "verification": {"verified": True}}

        def render(command, **options):
            self.assertEqual(options["cwd"], self.root)
            self.assertEqual(Path(command[2]).resolve(), summary.resolve())
            self.assertEqual(len(command[3:]), 6)
            self.assertEqual(command[3], "--html")
            self.assertEqual(command[4], "-o")
            self.assertEqual(Path(command[5]).resolve(), html_path.resolve())
            self.assertEqual(command[6], "--xlsx")
            self.assertEqual(Path(command[7]).resolve(), xlsx_path.resolve())
            self.assertEqual(command[8], "--json")
            html_path.write_text("<html>report</html>", encoding="utf-8")
            xlsx_path.write_bytes(b"workbook")
            return types.SimpleNamespace(returncode=0, stdout=json.dumps(projection), stderr="")

        with self.renderer_patch(), patch("subprocess.run", side_effect=render):
            result = runs.generate_project_report(
                self.root, record["project_run_id"], formats="html,xlsx,json"
            )
        self.assertEqual(result["status"], "succeeded")
        self.assertEqual(result["formats"], ["html", "xlsx", "json"])
        self.assertEqual(json.loads(json_path.read_text(encoding="utf-8")), projection)
        self.assertTrue(all(item["present"] for item in result["report_artifacts"].values()))
        self.assertEqual(result["report_sha256"], hashlib.sha256(html_path.read_bytes()).hexdigest())
        self.assertNotIn("error", result)
        status = json.loads((directory / "report-status.json").read_text(encoding="utf-8"))
        self.assertEqual(status["report_artifacts"]["xlsx"]["sha256"],
                         hashlib.sha256(b"workbook").hexdigest())

    def test_project_report_rejects_unknown_format_before_spawning_rust(self):
        record, _summary = self.report_run()
        with patch("subprocess.run") as process:
            with self.assertRaisesRegex(ValueError, "unsupported report format"):
                runs.generate_project_report(self.root, record["project_run_id"], formats="html,csv")
        process.assert_not_called()

    def test_runtime_import_hook_preserves_distribution_discovery(self):
        from importlib import metadata

        before = len(list(metadata.distributions()))
        runs._ensure_runtime_import_hook()
        self.assertGreater(before, 0)
        self.assertEqual(len(list(metadata.distributions())), before)

    def test_project_report_rejects_colliding_format_paths(self):
        record, _summary = self.report_run()
        with patch("subprocess.run") as process:
            with self.assertRaisesRegex(ValueError, "same output path"):
                runs.generate_project_report(
                    self.root, record["project_run_id"], output="report.xlsx", formats="html,xlsx"
                )
        process.assert_not_called()

    def test_project_report_failure_records_structured_error_envelope(self):
        record, _summary = self.report_run()
        with self.renderer_patch(), patch("subprocess.run", return_value=types.SimpleNamespace(returncode=2, stdout="", stderr="boom")):
            with self.assertRaises(ValueError) as caught:
                runs.generate_project_report(self.root, record["project_run_id"])
        self.assertIn("next:", str(caught.exception))
        status = json.loads(
            (runs._directory(self.root, record["project_run_id"]) / "report-status.json").read_text(encoding="utf-8")
        )
        error = status["error"]
        self.assertEqual(error["code"], "REPORT_FAILED")
        self.assertEqual(error["category"], "REPORT_FAILED")
        self.assertFalse(error["retryable"])
        self.assertEqual(error["stage"], "report")
        self.assertEqual(error["project_run_id"], record["project_run_id"])
        self.assertEqual(error["correlation_id"], record["project_run_id"])
        self.assertTrue(error["cause"])
        self.assertTrue(error["remediation"])

    def test_project_report_records_missing_renderer_without_running_a_substitute(self):
        from qianxing import report_renderer
        record, _summary = self.report_run()
        directory = runs._directory(self.root, record["project_run_id"])
        with patch.object(report_renderer, "_select_renderer", return_value=(None, "missing")), \
             patch("subprocess.run") as process:
            with self.assertRaises(report_renderer.ReportRendererError) as caught:
                runs.generate_project_report(self.root, record["project_run_id"])
        process.assert_not_called()
        self.assertEqual(caught.exception.diagnostic["status"], "missing")
        self.assertIn("--cli PATH", caught.exception.diagnostic["action"])
        status = json.loads((directory / "report-status.json").read_text(encoding="utf-8"))
        self.assertEqual(status["status"], "failed")
        self.assertEqual(status["reason"], "report_renderer_missing")
        self.assertEqual(status["error"]["code"], "REPORT_FAILED")
        self.assertEqual(status["report_sha256"], None)
        # The backtest itself stays successful; only the report stage failed.
        self.assertEqual(runs.project_run_status(self.root, record["project_run_id"])["status"], "succeeded")


if __name__ == "__main__":
    unittest.main()
