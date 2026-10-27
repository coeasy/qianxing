"""应用层用例的 Python 门面（T2-2 / 退出门 G1）。

与 `python/tests/test_native_extension.py` 同一约定：扩展是构建产物，没构建就 skip
（`cargo build -p qx-python` 或 `tools/build_python_wheel.*`），而不是把"没构建"当成失败。

这里守的是 Python 这一侧**自己**的契约：四条入口存在、进出一份 JSON 文档、失败时抛的异常
文本就是 `AppError` 文档（而不是某个 Python 侧自造的形状）。三入口之间的**逐字节等价**
由 `crates/qx-cli/tests/app_three_entrypoints.rs` 那一条守——两边分工，不重复。
"""

from __future__ import annotations

import json
import asyncio
import csv
import os
import shutil
import tempfile
import sys
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path
from typing import Any


def _load_native():
    """找到**带应用层入口**的那一份扩展；找不到就返回 None（调用方 skip）。

    搜索顺序里 `CARGO_TARGET_DIR` 排在前面：本仓的构建产物可能落在仓库外的目标目录里，而
    `<root>/target/<profile>` 里躺着的是上一次构建的旧 `.pyd`——旧世代能 import，但没有
    `app_*` 三条入口，所以候选必须**按名字探一次**再采用（`available()` 只回答"能 import"，
    按它判断会把"扩展是旧世代"读成"扩展可用"）。
    """
    root = Path(__file__).resolve().parents[2]
    loaded = sys.modules.get("qianxing_bridge._qianxing_native")
    loaded_path = Path(getattr(loaded, "__file__", "")).resolve() if loaded else None
    # Reuse only the per-process copy made below from the newest target artifact.
    # A package-local wheel extension can expose the older app entrypoints while
    # lacking newer schema fields, so method-name presence alone is not a version check.
    process_copy = root / "target" / f"python-test-native-{os.getpid()}"
    if (
        loaded is not None
        and hasattr(loaded, "app_assemble_dataset_bundle")
        and loaded_path is not None
        and loaded_path.parent == process_copy.resolve()
    ):
        # Reuse the already imported build. On Windows the loaded DLL is locked, so copying
        # the same source into another per-process directory fails with WinError 32.
        return loaded
    if loaded is not None:
        # Prefer the newest target artifact over an importable but possibly stale wheel.
        sys.modules.pop("qianxing_bridge._qianxing_native", None)
    directories = []
    for profile in ("release", "debug"):
        for base in (os.environ.get("CARGO_TARGET_DIR"), str(root / "target")):
            if base:
                directories.append(Path(base) / profile)
    directories.append(root / "python" / "qianxing_bridge")
    candidates = []
    for directory in directories:
        for pattern in ("_qianxing_native*.pyd", "_qianxing_native*.so", "_qianxing_native*.dll"):
            candidates.extend(directory.glob(pattern))
    # A locally built cdylib is named .dll on Windows, while wheel tooling renames it to .pyd.
    # Prefer the newest binary across profiles so an old release .pyd cannot mask a fresh debug DLL.
    for path in sorted(candidates, key=lambda item: item.stat().st_mtime_ns, reverse=True):
        import_dir = path.parent
        if path.suffix.lower() == ".dll":
            # Keep concurrent test runners from overwriting a loaded extension.
            import_dir = root / "target" / f"python-test-native-{os.getpid()}"
            import_dir.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, import_dir / "_qianxing_native.pyd")
        sys.path.insert(0, str(import_dir))
        sys.modules.pop("_qianxing_native", None)
        try:
            import _qianxing_native
        except ImportError:
            continue
        if all(
            hasattr(_qianxing_native, name)
            for name in (
                "app_assemble_dataset_bundle",
                "app_validate_dataset",
                "app_run_backtest",
                "app_run_backtest_with_strategy",
                "app_start_backtest_with_strategy",
                "app_run_backtest_with_strategy_slots",
                "app_start_backtest_with_strategy_slots",
                "app_run_depth_backtest_with_strategy",
                "app_start_depth_backtest_with_strategy",
                "app_run_depth_backtest_with_strategy_slots",
                "app_start_depth_backtest_with_strategy_slots",
                "app_run_multi_account_backtest",
                "app_start_multi_account_backtest",
                "app_run_multi_account_joint_backtest",
                "app_start_multi_account_joint_backtest",
                "app_run_multi_account_depth_backtest",
                "app_start_multi_account_depth_backtest",
                "app_start_backtest",
                "app_verify_run",
                "app_compare_runs",
                "app_plan_risk_parity",
                "app_plan_capital_allocation",
                "app_plan_algo_execution",
                "app_run_experiment",
                "app_start_experiment",
                "app_run_experiment_with_strategy",
                "app_start_experiment_with_strategy",
                "app_run_depth_backtest",
                "app_start_depth_backtest",
                "app_run_depth_backtest_with_strategy",
                "app_start_depth_backtest_with_strategy",
                "app_run_depth_backtest_with_strategy_slots",
                "app_start_depth_backtest_with_strategy_slots",
                "app_run_depth_experiment",
                "app_start_depth_experiment",
                "app_run_depth_experiment_with_strategy",
                "app_start_depth_experiment_with_strategy",
                "app_run_multi_leg_backtest",
                "app_start_multi_leg_backtest",
                "app_run_multi_market_backtest",
                "app_start_multi_market_backtest",
                "app_run_multi_market_backtest_with_strategy_slots",
                "app_start_multi_market_backtest_with_strategy_slots",
                "app_run_multi_market_depth_backtest",
                "app_start_multi_market_depth_backtest",
                "app_run_multi_market_depth_backtest_with_strategy_slots",
                "app_start_multi_market_depth_backtest_with_strategy_slots",
                "app_run_multi_leg_experiment",
                "app_start_multi_leg_experiment",
                "app_run_multi_market_experiment",
                "app_start_multi_market_experiment",
                "app_run_multi_market_experiment_with_strategy",
                "app_start_multi_market_experiment_with_strategy",
                "app_run_multi_account_joint_backtest",
                "app_start_multi_account_joint_backtest",
                "app_verify_depth_run",
            )
        ):
            # The bridge imports its bundled extension by the qualified package name.
            # Alias the freshly probed developer build to that name so an older .pyd
            # left in python/qianxing_bridge cannot shadow the current target binary.
            sys.modules["qianxing_bridge._qianxing_native"] = _qianxing_native
            return _qianxing_native
    return None


class AppUseCaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(__file__).resolve().parents[2]
        cls.native = _load_native()
        if cls.native is None:
            cls.bridge = None
            return
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import app

        cls.bridge = app

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory(
            prefix="qx-app-use-cases-", dir=self.root / "target"
        )
        self.output_dir = Path(self.temp_dir.name)

    def test_bridge_exposes_a_shared_run_handle_for_every_async_use_case(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import app, native

        spec = {"schema_version": 1, "run_id": "python-run-handle"}
        handle = object()
        for wrapper, entrypoint in (
            ("start_backtest", "app_start_backtest"),
            ("start_multi_account_backtest", "app_start_multi_account_backtest"),
            ("start_multi_account_joint_backtest", "app_start_multi_account_joint_backtest"),
            ("start_multi_account_depth_backtest", "app_start_multi_account_depth_backtest"),
            ("start_experiment", "app_start_experiment"),
            ("start_depth_backtest", "app_start_depth_backtest"),
            ("start_depth_experiment", "app_start_depth_experiment"),
            ("start_multi_leg_backtest", "app_start_multi_leg_backtest"),
            ("start_multi_leg_experiment", "app_start_multi_leg_experiment"),
        ):
            with self.subTest(wrapper=wrapper), patch.object(
                native, entrypoint, return_value=handle
            ) as invoke:
                self.assertIs(getattr(app, wrapper)(spec), handle)
                invoke.assert_called_once_with(json.dumps(spec, ensure_ascii=False))

    def test_multi_account_joint_bridge_delegates_sync_and_async_to_rust(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import app, native

        spec = {"schema_version": 1, "run_id": "python-joint"}
        result = {"run_id": "python-joint", "accounts": []}
        with patch.object(
            native,
            "app_run_multi_account_joint_backtest",
            return_value=json.dumps(result),
        ) as run:
            self.assertEqual(app.run_multi_account_joint_backtest(spec), result)
            run.assert_called_once_with(json.dumps(spec, ensure_ascii=False))

    def test_strategy_experiment_bridge_connects_sync_and_async_native_entries(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import app, native

        spec = {"schema_version": 1, "experiment_id": "strategy-grid"}
        callback = lambda candidate: candidate
        identity = {
            "strategy_id": "custom",
            "strategy_version": "v1",
            "source_sha256": "a" * 64,
        }
        expected_payload = json.dumps(spec, ensure_ascii=False)
        with patch.object(
            native, "app_run_experiment_with_strategy", return_value='{"ok":true}'
        ) as run:
            self.assertEqual(
                app.run_experiment_with_strategy(spec, callback, **identity), {"ok": True}
            )
            run.assert_called_once_with(expected_payload, "custom", "v1", "a" * 64, unittest.mock.ANY)
        handle = object()
        with patch.object(native, "app_start_experiment_with_strategy", return_value=handle) as start:
            self.assertIs(app.start_experiment_with_strategy(spec, callback, **identity), handle)
            start.assert_called_once_with(expected_payload, "custom", "v1", "a" * 64, unittest.mock.ANY)

    def test_quick_research_spec_bridge_calls_the_rust_app_preflight(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import app, native

        spec = {"schema_version": 1, "experiment_id": "quick-research"}
        expected_payload = json.dumps(spec, ensure_ascii=False)
        normalized = {**spec, "baselines": ["buy-and-hold"]}
        with patch.object(
            native,
            "app_prepare_quick_research_spec_v1",
            return_value=json.dumps(normalized, ensure_ascii=False),
            create=True,
        ) as prepare:
            self.assertEqual(app.prepare_quick_research_spec(spec), normalized)
            prepare.assert_called_once_with(expected_payload)

    def test_native_quick_research_entrypoint_calls_the_bundled_extension(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import native

        payload = '{"experiment_id":"quick-native"}'
        extension = SimpleNamespace(
            app_prepare_quick_research_spec_v1=lambda value: f"normalized:{value}"
        )
        with patch.object(native, "_extension", return_value=extension):
            self.assertEqual(
                native.app_prepare_quick_research_spec_v1(payload),
                f"normalized:{payload}",
            )

    def tearDown(self):
        self.temp_dir.cleanup()

    def _require(self):
        if self.bridge is None:
            self.skipTest("build qx-python to run the app use-case test")

    def test_capital_allocation_uses_deterministic_rust_portfolio_use_case(self):
        self._require()
        spec = {
            "schema_version": 1,
            "portfolio_id": "portfolio-python",
            "portfolio_budget_raw": 120,
            "accounts": [
                {"account_id": "account-a", "available_cash_raw": 100},
                {"account_id": "account-b", "available_cash_raw": 100},
            ],
            "requests": [
                {"strategy_id": "strategy-z", "account_id": "account-b", "requested_cash_raw": 80, "priority": 0},
                {"strategy_id": "strategy-a", "account_id": "account-a", "requested_cash_raw": 80, "priority": 0},
            ],
        }
        result = self.bridge.plan_capital_allocation(spec)
        self.assertEqual(result["portfolio_id"], "portfolio-python")
        self.assertEqual(
            [(item["strategy_id"], item["allocated_cash_raw"]) for item in result["allocations"]],
            [("strategy-a", 80), ("strategy-z", 40)],
        )

    def test_python_cli_routes_compare_runs_into_the_sdk(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing.cli import _parser

        args = _parser().parse_args(["compare-runs", "comparison.json"])
        self.assertEqual(args.command, "compare-runs")
        self.assertEqual(args.input, "comparison.json")
        experiment = _parser().parse_args(["run-experiment", "experiment.json"])
        self.assertEqual(experiment.command, "run-experiment")
        self.assertEqual(experiment.input, "experiment.json")
        multi_leg_experiment = _parser().parse_args(
            ["run-multi-leg-experiment", "multi-leg-experiment.json"]
        )
        self.assertEqual(multi_leg_experiment.command, "run-multi-leg-experiment")
        self.assertEqual(multi_leg_experiment.input, "multi-leg-experiment.json")
        multi_market_experiment = _parser().parse_args(
            ["run-multi-market-experiment", "multi-market-experiment.json"]
        )
        self.assertEqual(multi_market_experiment.command, "run-multi-market-experiment")
        self.assertEqual(multi_market_experiment.input, "multi-market-experiment.json")
        depth_experiment = _parser().parse_args(
            ["run-depth-experiment", "depth-experiment.json"]
        )
        self.assertEqual(depth_experiment.command, "run-depth-experiment")
        self.assertEqual(depth_experiment.input, "depth-experiment.json")
        depth = _parser().parse_args(["depth-backtest", "depth.json"])
        self.assertEqual(depth.command, "depth-backtest")
        self.assertEqual(depth.input, "depth.json")

    def test_doctor_reports_complete_native_use_case_catalog(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import doctor
        from qianxing_bridge import native

        required_entrypoints = {
            entrypoint
            for _use_case, entrypoints in native.APP_USE_CASE_ENTRYPOINTS
            for entrypoint in entrypoints
        }
        self.assertLessEqual(required_entrypoints, set(native.APP_ENTRYPOINTS))
        extension = SimpleNamespace(
            **{entrypoint: object() for entrypoint in native.APP_ENTRYPOINTS}
        )
        with patch.object(native, "_extension", return_value=extension):
            report = doctor()

        expected_use_cases = [
            use_case for use_case, _entrypoints in native.APP_USE_CASE_ENTRYPOINTS
        ]
        self.assertEqual(report["implemented_use_cases"], expected_use_cases)
        self.assertIn("report.read.depth.v1", report["implemented_use_cases"])
        self.assertIn("report.read.multi-leg.facts.v1", report["implemented_use_cases"])
        self.assertIn("report.read.multi-leg.event-page.v1", report["implemented_use_cases"])
        self.assertIn("backtest.multi-account.bar.v1", report["implemented_use_cases"])
        self.assertIn("backtest.multi-account.depth.v1", report["implemented_use_cases"])
        self.assertIn("backtest.multi-account.joint.v1", report["implemented_use_cases"])
        self.assertIn("backtest.multi-market.bar.v1", report["implemented_use_cases"])
        self.assertIn("backtest.multi-market.depth.v1", report["implemented_use_cases"])
        self.assertIn("backtest.multi-leg.bar.v1", report["implemented_use_cases"])
        self.assertIn("experiment.grid.multi-leg.bar.v1", report["implemented_use_cases"])
        self.assertFalse(any("multi-leg" in item for item in report["limitations"]))

        partial_extension = SimpleNamespace(app_run_multi_market_backtest=object())
        with patch.object(native, "_extension", return_value=partial_extension):
            self.assertNotIn(
                "backtest.multi-market.bar.v1", native.app_implemented_use_cases()
            )

        old_extension = SimpleNamespace(
            **{
                entrypoint: object()
                for entrypoint in native.APP_ENTRYPOINTS
                if entrypoint != "app_prepare_quick_research_spec_v1"
            }
        )
        with patch.object(native, "_extension", return_value=old_extension):
            self.assertFalse(native.app_available())

    def test_paper_and_live_clients_submit_the_shared_rust_control_contract(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import LiveClient, PaperClient, TradeCommand

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return b'{"status":"Accepted","command_id":41}'

        command = TradeCommand(
            command_id=41,
            request_id="sdk-41",
            operator_id="untrusted-body-identity",
            reason="SDK contract test",
            kind="PauseStrategy",
            target="strategy-1",
            payload={},
        )
        with patch("qianxing.app.urlopen", return_value=Response()) as open_url:
            paper_receipt = PaperClient(
                "https://localhost:8443", headers={"Authorization": "Bearer paper-token"}
            ).submit(command)
            live_receipt = LiveClient(
                "https://localhost:8443", headers={"X-Authenticated-Session": "session-7"}
            ).submit(command)

        self.assertEqual(paper_receipt["status"], "Accepted")
        self.assertEqual(live_receipt["command_id"], 41)
        requests = [call.args[0] for call in open_url.call_args_list]
        self.assertEqual(requests[0].full_url, "https://localhost:8443/app/paper/commands")
        self.assertEqual(requests[1].full_url, "https://localhost:8443/app/live/commands")
        self.assertEqual(json.loads(requests[0].data)["workflow"], "paper")
        self.assertEqual(json.loads(requests[1].data)["command"]["request_id"], "sdk-41")
        self.assertEqual(requests[0].get_header("Authorization"), "Bearer paper-token")
        self.assertEqual(requests[1].get_header("X-authenticated-session"), "session-7")
        with self.assertRaisesRegex(ValueError, "requires HTTPS"):
            LiveClient("http://localhost:8443")
        with self.assertRaisesRegex(ValueError, "Content-Type"):
            PaperClient("http://localhost:8443", headers={"Content-Type": "text/plain"})
        with self.assertRaisesRegex(ValueError, "valid string"):
            PaperClient("http://localhost:8443", headers={"X-Auth": "bad\r\nvalue"})

    def test_python_http_app_client_runs_and_polls_rust_remote_run_handle(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient, RemoteRunHandle

        class Response:
            def __init__(self, body):
                self.body = json.dumps(body).encode("utf-8")

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return self.body

        replies = [
            Response({"run_id": "remote-1", "status": "running"}),
            Response({"run_id": "remote-1", "status": "running"}),
            Response(
                {
                    "run_id": "remote-1",
                    "status": "succeeded",
                    "result": {
                        "run_id": "remote-1",
                        "instrument": "BTCUSDT.BINANCE",
                        "tier": "l1",
                        "result_hash": "result",
                        "data_fingerprint": "data",
                        "fills": 0,
                        "equity_points": 2,
                        "return_bps": 12,
                        "max_drawdown_bps": 0,
                        "artifacts": {},
                    },
                    "error": None,
                }
            ),
            Response(
                {
                    "run_id": "remote-1",
                    "status": "succeeded",
                    "result": {
                        "run_id": "remote-1",
                        "instrument": "BTCUSDT.BINANCE",
                        "tier": "l1",
                        "result_hash": "result",
                        "data_fingerprint": "data",
                        "fills": 0,
                        "equity_points": 2,
                        "return_bps": 12,
                        "max_drawdown_bps": 0,
                        "artifacts": {},
                    },
                    "error": None,
                }
            ),
            Response({"run_id": "remote-1", "status": "cancelling"}),
        ]
        with patch("qianxing.app.urlopen", side_effect=replies) as open_url:
            client = AppClient("https://localhost:8443/api", headers={"Authorization": "Bearer sdk"})
            handle = client.start_depth_backtest({"schema_version": 1, "run_id": "remote-1"})
            self.assertIsInstance(handle, RemoteRunHandle)
            self.assertEqual(handle.run_id, "remote-1")
            self.assertEqual(handle.wait(timeout=1.0, poll_interval=0.001), "succeeded")
            self.assertEqual(handle.result().return_bps, 12)
            cancelled = handle.cancel()

        self.assertEqual(cancelled["status"], "cancelling")
        requests = [call.args[0] for call in open_url.call_args_list]
        self.assertEqual(requests[0].method, "POST")
        self.assertEqual(requests[0].full_url, "https://localhost:8443/api/app/depth-backtest/start")
        self.assertEqual(json.loads(requests[0].data)["run_id"], "remote-1")
        self.assertEqual(requests[1].method, "GET")
        self.assertEqual(requests[1].full_url, "https://localhost:8443/api/app/runs/remote-1")
        self.assertEqual(requests[-1].full_url, "https://localhost:8443/api/app/runs/remote-1/cancel")

    def test_python_remote_events_use_rust_cursor_and_validate_page_shape(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient

        class Response:
            def __init__(self, body):
                self.body = json.dumps(body).encode("utf-8")

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return self.body

        page = [{"seq": 8, "cursor": "8:abcd", "event": {"kind": "OrderAccepted"}}]
        with patch("qianxing.app.urlopen", side_effect=[Response(page), Response([])]) as open_url:
            client = AppClient("https://localhost")
            self.assertEqual(client.get_live_events(7), page)
            self.assertEqual(client.get_live_events(), [])

        requests = [call.args[0] for call in open_url.call_args_list]
        self.assertEqual([request.method for request in requests], ["GET", "GET"])
        self.assertEqual(
            [request.full_url for request in requests],
            ["https://localhost/events/live?after=7", "https://localhost/events/live"],
        )
        with self.assertRaises(ValueError):
            client.get_live_events(True)
        with self.assertRaises(ValueError):
            client.get_live_events(-1)
        with self.assertRaises(ValueError):
            client.get_live_events(1 << 64)

    def test_both_run_handles_default_to_a_finite_wait(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        import inspect

        from qianxing import RemoteRunHandle, RunHandle
        from qianxing.app import DEFAULT_RUN_WAIT_MS

        # 方案 §9：`wait()` 默认有限。原生句柄原来的缺省是 None（= 一直等），而远端句柄是 30 s——
        # 同一个 SDK 的两条腿对"等多久"给了两个相反的答案。这里把缺省钉住：谁把原生缺省改回
        # None，这条用例就红；`None` 仍然可用，但它从此必须是调用方**写出来**的请求。
        self.assertGreater(DEFAULT_RUN_WAIT_MS, 0)
        native_default = inspect.signature(RunHandle.wait).parameters["timeout_ms"].default
        self.assertIsNotNone(native_default)
        self.assertEqual(native_default, DEFAULT_RUN_WAIT_MS)
        remote_default = inspect.signature(RemoteRunHandle.wait).parameters["timeout"].default
        self.assertIsNotNone(remote_default)
        self.assertGreater(remote_default, 0)

    def test_remote_run_handle_rejects_unsafe_ids_and_surfaces_rust_app_errors(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient, DataUnavailableError, RemoteRunHandle

        with self.assertRaisesRegex(ValueError, "run_id"):
            RemoteRunHandle(AppClient("https://localhost"), "../escape")
        bounded = RemoteRunHandle(AppClient("https://localhost"), "safe-run")
        with self.assertRaisesRegex(ValueError, "finite and positive"):
            bounded.wait(timeout=float("inf"))
        with self.assertRaisesRegex(ValueError, "finite and positive"):
            bounded.wait(timeout=1.0, poll_interval=float("nan"))

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return json.dumps(
                    {
                        "run_id": "remote-failed",
                        "status": "failed",
                        "result": None,
                        "error": {
                            "category": "DATA_UNAVAILABLE",
                            "message": "dataset missing",
                            "correlation_id": "remote-failed",
                        },
                    }
                ).encode("utf-8")

        with patch("qianxing.app.urlopen", return_value=Response()):
            handle = RemoteRunHandle(AppClient("https://localhost"), "remote-failed")
            with self.assertRaises(DataUnavailableError) as caught:
                handle.result()
        self.assertEqual(caught.exception.correlation_id, "remote-failed")

    def test_python_remote_paper_algo_lifecycle_routes(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient

        client = AppClient("https://localhost")
        observed = []

        def request_json(route, *, method, payload=None):
            observed.append((route, method, payload))
            return {"accepted": True}

        client._request_json = request_json
        parent = {"parent_id": "parent-1"}
        self.assertTrue(client.start_paper_algo_execution({"spec": {}})["accepted"])
        self.assertEqual(client.get_paper_algo_execution("parent-1"), {"accepted": True})
        self.assertEqual(client.cancel_paper_algo_execution("parent-1"), {"accepted": True})
        self.assertEqual(
            client.list_paper_algo_executions(after_parent_id="parent-0", limit=7),
            {"accepted": True},
        )
        self.assertEqual(
            [entry[0] for entry in observed],
            [
                "/app/paper/algo-executions/start",
                "/app/paper/algo-executions/get",
                "/app/paper/algo-executions/cancel",
                "/app/paper/algo-executions/list",
            ],
        )
        self.assertEqual(observed[1][2], parent)
        self.assertEqual(observed[3][2]["limit"], 7)

    def test_python_http_app_client_covers_every_registered_rust_use_case_route(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient, RemoteRunHandle

        class Response:
            def __init__(self, body):
                self.body = json.dumps(body).encode("utf-8")

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return self.body

        cases = [
            ("validate_dataset", "/app/validate-dataset", False),
            ("run_backtest", "/app/backtest", False),
            ("run_provider_backtest_with_manifest", "/app/provider-backtest", False),
            ("run_provider_depth_backtest", "/app/provider-depth-backtest", False),
            ("read_provider_run_report_v1", "/app/read-provider-report", False),
            ("read_provider_depth_run_report_v1", "/app/read-provider-depth-report", False),
            ("read_multi_account_joint_report_v1", "/app/read-multi-account-joint-report", False),
            ("read_multi_leg_research_report_v1", "/app/read-multi-leg-report", False),
            ("provider_preflight", "/app/capabilities/providers/preflight", False),
            ("run_multi_account_backtest", "/app/multi-account-backtest", False),
            ("start_multi_account_backtest", "/app/multi-account-backtest/start", True),
            ("run_multi_account_joint_backtest", "/app/multi-account-joint-backtest", False),
            ("start_multi_account_joint_backtest", "/app/multi-account-joint-backtest/start", True),
            ("run_multi_account_depth_backtest", "/app/multi-account-depth-backtest", False),
            ("start_multi_account_depth_backtest", "/app/multi-account-depth-backtest/start", True),
            ("start_backtest", "/app/backtest/start", True),
            ("verify_run", "/app/verify", False),
            ("compare_runs", "/app/compare-runs", False),
            ("plan_risk_parity", "/app/plan-risk-parity", False),
            ("plan_algo_execution", "/app/plan-algo-execution", False),
            ("plan_capital_allocation", "/app/plan-capital-allocation", False),
            ("run_experiment", "/app/run-experiment", False),
            ("start_experiment", "/app/run-experiment/start", True),
            ("run_depth_backtest", "/app/depth-backtest", False),
            ("start_depth_backtest", "/app/depth-backtest/start", True),
            ("verify_depth_run", "/app/verify-depth", False),
            ("run_depth_experiment", "/app/run-depth-experiment", False),
            ("start_depth_experiment", "/app/run-depth-experiment/start", True),
            ("run_multi_leg_backtest", "/app/multi-leg-backtest", False),
            ("start_multi_leg_backtest", "/app/multi-leg-backtest/start", True),
            ("run_multi_market_backtest", "/app/multi-market-backtest", False),
            ("start_multi_market_backtest", "/app/multi-market-backtest/start", True),
            ("run_multi_market_depth_backtest", "/app/multi-market-depth-backtest", False),
            ("start_multi_market_depth_backtest", "/app/multi-market-depth-backtest/start", True),
            ("run_multi_market_experiment", "/app/run-multi-market-experiment", False),
            ("start_multi_market_experiment", "/app/run-multi-market-experiment/start", True),
            ("run_multi_market_depth_experiment", "/app/run-multi-market-depth-experiment", False),
            ("start_multi_market_depth_experiment", "/app/run-multi-market-depth-experiment/start", True),
            ("run_multi_leg_experiment", "/app/run-multi-leg-experiment", False),
            ("start_multi_leg_experiment", "/app/run-multi-leg-experiment/start", True),
        ]
        results = {
            "validate_dataset": {
                "dataset_id": "dataset",
                "instrument": "BTCUSDT.BINANCE",
                "rows": 2,
                "content_fingerprint": "fingerprint",
                "usable": True,
                "gaps": [],
            },
            "run_backtest": {
                "run_id": "run",
                "instrument": "BTCUSDT.BINANCE",
                "result_hash": "result",
                "data_fingerprint": "data",
                "fills": 0,
                "equity_points": 2,
                "return_bps": 0,
                "max_drawdown_bps": 0,
                "artifacts": {},
            },
            "run_provider_backtest_with_manifest": {
                "outcome": {},
                "dataset_verdict": {},
                "manifest": {},
                "manifest_path": "manifest.json",
            },
            "run_provider_depth_backtest": {
                "outcome": {},
                "provider_id": "provider",
                "provider_version": "v1",
                "request_id": "request",
                "retry_chain": ["provider"],
                "source_hash": "0123456789abcdef",
                "depth_path": "depth.json",
            },
            "read_provider_run_report_v1": {
                "schema_version": 1,
                "report": {"schema_version": 1, "run_id": "bar"},
                "dataset_verdict": {},
                "manifest": {},
            },
            "read_provider_depth_run_report_v1": {
                "schema_version": 1,
                "report": {"schema_version": 1, "run_id": "depth"},
                "provider_id": "provider",
                "provider_version": "v1",
                "request_id": "request",
                "retry_chain": ["provider"],
                "source_hash": "0123456789abcdef",
                "query_digest": "0123456789abcdef",
                "depth_input_hash": "0123456789abcdef",
            },
            "read_multi_account_joint_report_v1": {
                "schema_version": 1,
                "run_id": "joint",
                "settlement_currency": "USDT",
                "spec_fingerprint": "0123456789abcdef",
                "verification": {"verified": True},
                "summary": {},
                "equity": {"rows": []},
                "fills": {"rows": []},
                "accounts": [],
            },
            "read_multi_leg_research_report_v1": {
                "schema_version": 1,
                "run_id": "multi-leg",
                "strategy_id": "pairs",
                "data_fingerprint": "0123456789abcdef",
                "summary": {},
                "execution": {},
                "markets": [],
                "equity": {"rows": []},
                "fills": {"rows": []},
                "events": {"rows": []},
            },
            "provider_preflight": {
                "schema_version": 1,
                "request_fingerprint": "0123456789abcdef",
                "matched": True,
                "candidates": [],
            },
            "run_multi_account_backtest": {
                "batch_id": "batch",
                "settlement_currency": "USDT",
                "spec_fingerprint": "fingerprint",
                "runs": [],
                "summary": {
                    "accounts_total": 0,
                    "accounts_succeeded": 0,
                    "accounts_failed": 0,
                    "successful_initial_cash_raw": 0,
                    "successful_fills": 0,
                    "weighted_return_bps": None,
                    "complete": True,
                },
            },
            "start_multi_account_backtest": {"run_id": "batch", "status": "starting"},
            "run_multi_account_joint_backtest": {
                "run_id": "joint",
                "settlement_currency": "USDT",
                "spec_fingerprint": "0123456789abcdef",
                "accounts_total": 2,
                "markets_total": 2,
                "initial_cash_raw": 200,
                "final_equity_raw": 200,
                "return_bps": 0,
                "max_drawdown_bps": 0,
                "successful_fills": 0,
                "equity_path": "joint-equity.json",
                "fills_path": "joint-fills.json",
                "artifact_path": "joint-portfolio.json",
                "accounts": [],
            },
            "start_multi_account_joint_backtest": {"run_id": "joint", "status": "starting"},
            "run_multi_account_depth_backtest": {
                "batch_id": "depth-batch",
                "settlement_currency": "USDT",
                "spec_fingerprint": "fingerprint",
                "runs": [],
                "summary": {},
            },
            "start_multi_account_depth_backtest": {"run_id": "depth-batch", "status": "starting"},
            "plan_risk_parity": {
                "schema_version": 1,
                "portfolio_id": "portfolio",
                "plan_hash": "0123456789abcdef",
                "plan": {"allocations": [], "orders": []},
            },
            "plan_algo_execution": {"schema_version": 1, "method": "twap", "parent_id": "p", "children": []},
            "plan_capital_allocation": {"schema_version": 1, "portfolio_id": "p", "allocations": []},
            "verify_run": {
                "run_id": "run",
                "result_hash": "result",
                "data_fingerprint": "data",
                "verified": True,
                "checks": [],
                "mismatches": [],
            },
            "compare_runs": {
                "schema_version": 1,
                "instrument": "BTCUSDT.BINANCE",
                "data_fingerprint": "data",
                "runs": [],
            },
            "run_experiment": {
                "schema_version": 1,
                "experiment_id": "experiment",
                "total_candidates": 0,
                "completed_candidates": 0,
                "succeeded_candidates": 0,
                "failed_candidates": 0,
                "spec_fingerprint": "fingerprint",
                "candidates": [],
                "comparison": None,
                "artifact_path": "report.json",
            },
            "run_depth_backtest": {
                "run_id": "depth",
                "instrument": "BTCUSDT.BINANCE",
                "tier": "l1",
                "result_hash": "result",
                "data_fingerprint": "data",
                "fills": 0,
                "equity_points": 2,
                "return_bps": 0,
                "max_drawdown_bps": 0,
                "artifacts": {},
            },
            "verify_depth_run": {
                "run_id": "depth",
                "result_hash": "result",
                "data_fingerprint": "data",
                "verified": True,
                "checks": [],
                "mismatches": [],
            },
            "run_depth_experiment": {
                "schema_version": 1,
                "experiment_id": "experiment",
                "spec_fingerprint": "fingerprint",
                "total_candidates": 0,
                "completed_candidates": 0,
                "succeeded_candidates": 0,
                "failed_candidates": 0,
                "candidates": [],
                "ranking": [],
                "artifact_path": "report.json",
            },
            "run_multi_leg_backtest": {
                "run_id": "pair",
                "primary_instrument": "BTCUSDT.BINANCE",
                "reference_instrument": "ETHUSDT.BINANCE",
                "primary_result_hash": "primary",
                "reference_result_hash": "reference",
                "combined_return_bps": 0,
                "primary_fills": 0,
                "reference_fills": 0,
                "attribution_artifact": "pair.json",
            },
            "run_multi_market_backtest": {
                "run_id": "portfolio",
                "account_id": "shared",
                "spec_fingerprint": "spec",
                "data_fingerprint": "data",
                "shared_result_hash": "result",
                "markets_total": 3,
                "strategies_total": 3,
                "successful_fills": 0,
                "equity_points": 1,
                "final_equity_raw": 100,
                "return_bps": 0,
                "max_drawdown_bps": 0,
                "artifact_path": "portfolio.json",
            },
            "run_multi_market_depth_backtest": {
                "run_id": "portfolio-depth",
                "account_id": "shared",
                "spec_fingerprint": "spec",
                "data_fingerprint": "data",
                "shared_result_hash": "result",
                "markets_total": 2,
                "successful_fills": 0,
                "pending_orders": 0,
                "equity_points": 2,
                "final_equity_raw": 100,
                "return_bps": 0,
                "max_drawdown_bps": 0,
                "artifact_path": "portfolio-depth.json",
            },
            "run_multi_market_experiment": {
                "schema_version": 1,
                "experiment_id": "experiment",
                "target_strategy_id": "slot-a",
                "spec_fingerprint": "fingerprint",
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "candidates": [],
                "ranking": [],
                "artifact_path": "report.json",
            },
            "start_multi_market_experiment": {"run_id": "experiment", "status": "starting"},
            "run_multi_market_depth_experiment": {
                "schema_version": 1,
                "experiment_id": "experiment",
                "target_strategy_id": "slot-a",
                "spec_fingerprint": "fingerprint",
                "total_candidates": 1,
                "completed_candidates": 1,
                "succeeded_candidates": 1,
                "failed_candidates": 0,
                "candidates": [],
                "ranking": [],
                "artifact_path": "report.json",
            },
            "start_multi_market_depth_experiment": {"run_id": "experiment", "status": "starting"},
            "run_multi_leg_experiment": {
                "schema_version": 1,
                "experiment_id": "experiment",
                "spec_fingerprint": "fingerprint",
                "total_candidates": 0,
                "completed_candidates": 0,
                "succeeded_candidates": 0,
                "failed_candidates": 0,
                "candidates": [],
                "ranking": [],
                "artifact_path": "report.json",
            },
        }
        replies = [
            Response({"run_id": "contract-run"})
            if is_start
            else Response(results[method])
            for method, _route, is_start in cases
        ]
        with patch(
            "qianxing.app.urlopen",
            side_effect=replies,
        ) as open_url:
            client = AppClient("https://localhost")
            for method, _route, is_start in cases:
                payload = {"schema_version": 1}
                if method in {"run_experiment", "start_experiment"}:
                    payload.update({
                        "experiment_id": "remote-classic-selection",
                        "base": self._spec("unused-remote-selection", self.output_dir),
                        "parameter_space": [],
                        "strategy_kinds": ["ema_cross", "sma_cross"],
                    })
                result = getattr(client, method)(payload)
                self.assertEqual(isinstance(result, RemoteRunHandle), is_start)

        requests = [call.args[0] for call in open_url.call_args_list]
        self.assertEqual(len(requests), len(cases))
        for request, (_method, route, _is_start) in zip(requests, cases):
            self.assertEqual(request.method, "POST")
            self.assertEqual(request.full_url, f"https://localhost{route}")
        for index in (
            index for index, (method, _route, _is_start) in enumerate(cases)
            if method in {"run_experiment", "start_experiment"}
        ):
            payload = json.loads(open_url.call_args_list[index].args[0].data)
            self.assertEqual(payload["strategy_kinds"], ["ema_cross", "sma_cross"])

    def test_research_artifacts_posts_completed_outcome_to_detail_route(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return b'{"verified":true,"equity":[],"fills":[],"events":[]}'

        outcome = {
            "run_id": "run-1",
            "instrument": "BTCUSDT.BINANCE",
            "result_hash": "result",
            "data_fingerprint": "data",
            "fills": 0,
            "equity_points": 1,
            "return_bps": 0,
            "max_drawdown_bps": 0,
            "artifacts": {
                "run_manifest": "manifest.json",
                "summary": "summary.json",
                "equity": "equity.csv",
                "fills": "fills.csv",
                "event_trace": "events.jsonl",
            },
        }
        with patch("qianxing.app.urlopen", return_value=Response()) as open_url:
            client = AppClient("https://localhost")
            result = client.research_artifacts(outcome)
        self.assertTrue(result["verified"])
        sent = open_url.call_args.args[0]
        self.assertEqual(sent.full_url, "https://localhost/app/research-artifacts")
        self.assertEqual(sent.method, "POST")
        self.assertEqual(json.loads(sent.data), outcome)

    def test_async_multi_leg_report_uses_the_versioned_remote_route(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return b'{"schema_version":1,"run_id":"multi-leg","events":{"rows":[]}}'

        request = {
            "schema_version": 1,
            "outcome": {"run_id": "multi-leg"},
        }
        with patch("qianxing.app.urlopen", return_value=Response()) as open_url:
            client = AppClient("https://localhost")
            result = asyncio.run(client.async_read_multi_leg_research_report_v1(request))
        self.assertEqual(result["run_id"], "multi-leg")
        sent = open_url.call_args.args[0]
        self.assertEqual(sent.full_url, "https://localhost/app/read-multi-leg-report")
        self.assertEqual(sent.method, "POST")
        self.assertEqual(json.loads(sent.data), request)

    def test_async_provider_preflight_delegates_to_the_same_remote_route(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import AppClient

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return b'{"schema_version":1,"request_fingerprint":"0123456789abcdef","matched":false,"candidates":[]}'

        request = {
            "kind": "Bar",
            "asset_class": "crypto",
            "instrument_set": ["BTCUSDT.BINANCE"],
            "field_set": ["close"],
            "frequency": "1m",
            "start": 1,
            "end": 10,
            "as_of": 10,
        }
        with patch("qianxing.app.urlopen", return_value=Response()) as open_url:
            client = AppClient("https://localhost")
            response = asyncio.run(client.async_provider_preflight(request))
        self.assertFalse(response["matched"])
        sent = open_url.call_args.args[0]
        self.assertEqual(sent.full_url, "https://localhost/app/capabilities/providers/preflight")
        self.assertEqual(sent.method, "POST")
        self.assertEqual(json.loads(sent.data), request)

    def _spec(self, run_id: str, output_dir: Path, bars: str | None = None) -> dict:
        return {
            "schema_version": 1,
            "run_id": run_id,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": bars or str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000,
            "seed": 20261010,
            "output_dir": str(output_dir),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "py-sma",
                "quantity_raw": 1_000_000_000,
                "fast_window": 2,
                "slow_window": 3,
            },
        }

    def test_python_strategy_callback_runs_shared_kernel_and_replay_verifier(self):
        self._require()
        scale = 1_000_000_000
        timestamps, prices = [], []
        for index in range(60):
            phase = index % 20
            prices.append((100 + phase * 2 if phase < 10 else 100 + (20 - phase) * 2) * scale)
            timestamps.append(1_000 + index * 1_000)
        bars_path = self.output_dir / "python-strategy-bars.json"
        bars_path.write_text(json.dumps({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "python-custom-strategy-v1",
            "ts": timestamps,
            "open_raw": prices,
            "high_raw": [price + scale for price in prices],
            "low_raw": [price - scale for price in prices],
            "close_raw": prices,
            "volume_raw": [scale] * len(prices),
        }), encoding="utf-8")
        spec = self._spec("python-custom-strategy", self.output_dir / "custom", str(bars_path))
        spec["strategy"]["strategy_id"] = "python-custom-strategy"
        spec["strategy"]["fast_window"] = 5
        spec["strategy"]["slow_window"] = 20
        spec["market_spec"] = {
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "product": "spot",
            "base_currency": "BTC",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size": 1_000_000_000,
            "linear": False,
            "inverse": False,
            "price_tick": 1,
            "qty_step": 1,
            "min_qty": 1,
            "min_notional_raw": 0,
            "max_leverage": 1,
            "maintenance_margin_bps": 0,
            "valid_from": 0,
            "valid_to": None,
            "exact_rules": {
                "contract_size": {"coefficient": "1", "scale": 0},
                "price_tick": {"coefficient": "1", "scale": 8},
                "quantity_step": {"coefficient": "1", "scale": 8},
                "minimum_quantity": {"coefficient": "1", "scale": 8},
                "minimum_notional": {"coefficient": "0", "scale": 0},
            },
        }
        spec["fee_schedule"] = {"maker_fee_bps": 10, "taker_fee_bps": 10}

        from qianxing_strategy import BaseStrategy

        class BuyOnce(BaseStrategy):
            def on_init(self, context):
                self.emitted = False
                self.lifecycle = ["init"]

            def on_start(self, context):
                self.lifecycle.append("start")

            def on_bar(self, context, bar):
                self.lifecycle.append("bar")
                intents = []
                if not self.emitted:
                    self.emitted = True
                    intents.append(self.order_intent(
                        intent_id=1,
                        instrument=bar["instrument"],
                        side="buy",
                        qty_exact="0.001",
                    ))
                return self.decision(
                    context,
                    "Bar",
                    bar,
                    intents=intents,
                    confidence=10_000,
                )

            def on_order_update(self, context, update):
                self.lifecycle.append(f"update:{update['event']}")
                return None

            def on_stop(self):
                self.lifecycle.append("stop")

        from qianxing.app import (
            AsyncCoreClient,
            ConflictError,
            run_backtest_with_strategy,
            run_backtest_with_strategy_slots,
            start_backtest_with_strategy,
            start_backtest_with_strategy_slots,
            read_run_report_v1,
            verify_run,
        )

        callback_strategy = BuyOnce()
        outcome = run_backtest_with_strategy(
            spec,
            callback_strategy,
            strategy_id="python-custom-strategy",
            strategy_version="python-test-v1",
            source_sha256="a" * 64,
        )
        self.assertGreater(outcome.fills, 0)
        events = [
            json.loads(line)
            for line in Path(outcome.artifacts["event_trace"])
            .read_text(encoding="utf-8")
            .splitlines()
            if line.strip()
        ]
        custom_fills = [
            event["kind"]["Filled"]["fill"]
            for event in events
            if "Filled" in event.get("kind", {})
            and event["kind"]["Filled"]["fill"].get("strategy_id")
            == "python-custom-strategy"
        ]
        self.assertTrue(custom_fills)
        self.assertEqual(
            custom_fills[0]["exact_amounts"]["quantity"],
            {"coefficient": "1", "scale": 3},
        )
        import csv

        with Path(outcome.artifacts["fills"]).open(
            encoding="utf-8", newline=""
        ) as fills_file:
            fill_rows = list(csv.DictReader(fills_file))
        exact_fill_rows = [
            row
            for row in fill_rows
            if row["strategy_id"] == "python-custom-strategy"
        ]
        self.assertEqual(len(exact_fill_rows), 1)
        self.assertEqual(exact_fill_rows[0]["qty_raw"], "0")
        self.assertEqual(exact_fill_rows[0]["qty_exact"], "0.001")
        self.assertTrue(exact_fill_rows[0]["price_exact"])
        self.assertIsInstance(
            json.loads(exact_fill_rows[0]["exact_fees_json"] or "[]"), list
        )
        self.assertEqual(callback_strategy.lifecycle[:2], ["init", "start"])
        self.assertTrue(any(item.startswith("update:") for item in callback_strategy.lifecycle))
        self.assertEqual(callback_strategy.lifecycle[-1], "stop")
        verification = verify_run(outcome)
        self.assertTrue(verification.verified, verification.mismatches)
        report = read_run_report_v1(outcome)
        self.assertEqual(report["schema_version"], 1)
        self.assertTrue(report["verification"]["verified"])
        self.assertEqual(report["summary"]["run_id"], outcome.run_id)
        self.assertEqual(report["equity"]["total"], outcome.equity_points)
        self.assertEqual(report["fills"]["total"], outcome.fills)
        exact_report_fills = [
            row
            for row in report["fills"]["rows"]
            if row.get("strategy_id") == "python-custom-strategy"
        ]
        self.assertEqual(len(exact_report_fills), 1)
        self.assertEqual(exact_report_fills[0]["qty_exact"], "0.001")
        summary = json.loads(Path(outcome.artifacts["summary"]).read_text(encoding="utf-8"))
        self.assertEqual(summary["strategy_version"], "python-test-v1")

        portfolio_spec = {
            **spec,
            "run_id": "python-custom-strategy-portfolio",
            "output_dir": str(self.output_dir / "custom-portfolio"),
            "strategy": dict(spec["strategy"]),
        }
        secondary = dict(spec["strategy"])
        secondary.update({"kind": "momentum", "strategy_id": "python-builtin-secondary"})
        portfolio_spec["portfolio"] = {
            "primary_priority": 0,
            "primary_reserved_capital_raw": 50_000_000_000_000,
            "additional_strategies": [{
                "priority": 1,
                "reserved_capital_raw": 50_000_000_000_000,
                "strategy": secondary,
            }],
        }
        portfolio_outcome = run_backtest_with_strategy(
            portfolio_spec,
            BuyOnce(),
            strategy_id="python-custom-strategy",
            strategy_version="python-test-v1",
            source_sha256="a" * 64,
        )
        self.assertGreater(portfolio_outcome.fills, 0)
        portfolio_fills = Path(portfolio_outcome.artifacts["fills"]).read_text(encoding="utf-8")
        self.assertTrue(any("python-custom-strategy" in line for line in portfolio_fills.splitlines()[1:]))
        self.assertTrue(any("python-builtin-secondary" in line for line in portfolio_fills.splitlines()[1:]))
        self.assertTrue(verify_run(portfolio_outcome).verified)

        portfolio_handle_spec = {
            **portfolio_spec,
            "run_id": "python-custom-strategy-portfolio-handle",
            "output_dir": str(self.output_dir / "custom-portfolio-handle"),
        }
        portfolio_handle = start_backtest_with_strategy(
            portfolio_handle_spec,
            BuyOnce(),
            strategy_id="python-custom-strategy",
            strategy_version="python-test-v1",
            source_sha256="a" * 64,
        )
        self.assertEqual(portfolio_handle.wait(timeout_ms=30_000), "succeeded")
        portfolio_async_outcome = portfolio_handle.result()
        self.assertIsNotNone(portfolio_async_outcome)
        self.assertTrue(verify_run(portfolio_async_outcome).verified)

        # Python authored candidates enter the same Rust-owned experiment loop. The factory is
        # invoked once per candidate and the cancellable handle uses the same callback path.
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentParameterSpace,
            RunExperimentSpec,
            run_experiment_with_strategy,
            start_experiment_with_strategy,
        )

        experiment_spec = RunExperimentSpec(
            experiment_id="python-custom-strategy-grid",
            base=BacktestSpec(
                **{
                    **spec,
                    "strategy": BuiltinStrategySpec(**spec["strategy"]),
                }
            ),
            parameter_space=(ExperimentParameterSpace("quantity_raw", (500_000, 1_000_000)),),
        )
        candidate_quantities = []

        def candidate_factory(candidate):
            quantity = candidate["strategy"]["quantity_raw"]
            candidate_quantities.append(quantity)

            class BuyOnceWithQuantity:
                def __init__(self):
                    self.emitted = False

                def on_event(self, context, event):
                    bar = event["Bar"]
                    intents = []
                    if not self.emitted:
                        self.emitted = True
                        intents.append({
                            "intent_id": 1,
                            "instrument": bar["instrument"],
                            "side": "Buy",
                            "qty": quantity,
                            "limit": None,
                            "policy": None,
                            "reduce_only": False,
                            "post_only": False,
                        })
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

            return BuyOnceWithQuantity()

        identity = {
            "strategy_id": "python-custom-strategy",
            "strategy_version": "python-grid-v1",
            "source_sha256": "c" * 64,
        }
        experiment = run_experiment_with_strategy(
            experiment_spec,
            candidate_factory,
            **identity,
        )
        self.assertEqual(candidate_quantities, [500_000, 1_000_000])
        self.assertEqual(experiment.completed_candidates, 2)
        self.assertEqual(experiment.succeeded_candidates, 2, repr(experiment.candidates))
        self.assertTrue(all(candidate.outcome.fills > 0 for candidate in experiment.candidates))

        handle_spec = RunExperimentSpec(
            **{
                **experiment_spec.__dict__,
                "experiment_id": "python-custom-strategy-grid-handle",
                "base": BacktestSpec(
                    **{
                        **experiment_spec.base.__dict__,
                        "output_dir": str(self.output_dir / "custom-strategy-grid-handle"),
                    }
                ),
            }
        )
        handle = start_experiment_with_strategy(
            handle_spec,
            lambda candidate: candidate_factory(candidate),
            **identity,
        )
        self.assertEqual(handle.wait(timeout_ms=30_000), "succeeded")
        self.assertEqual(handle.result().succeeded_candidates, 2)

        async_spec = RunExperimentSpec(
            experiment_id="python-custom-strategy-grid-async",
            base=BacktestSpec(
                **{
                    **experiment_spec.base.__dict__,
                    "output_dir": str(self.output_dir / "custom-strategy-grid-async"),
                }
            ),
            parameter_space=experiment_spec.parameter_space,
        )
        async_client = AsyncCoreClient()
        async_result = asyncio.run(async_client.async_run_experiment_with_strategy(
            async_spec, candidate_factory, **identity
        ))
        self.assertEqual(async_result.succeeded_candidates, 2)
        async_handle = asyncio.run(async_client.async_start_experiment_with_strategy(
            RunExperimentSpec(
                experiment_id="python-custom-strategy-grid-async-handle",
                base=BacktestSpec(
                    **{
                        **experiment_spec.base.__dict__,
                        "output_dir": str(self.output_dir / "custom-strategy-grid-async-handle"),
                    }
                ),
                parameter_space=experiment_spec.parameter_space,
            ),
            candidate_factory,
            **identity,
        ))
        self.assertEqual(asyncio.run(async_handle.async_wait(30)), "succeeded")
        self.assertEqual((asyncio.run(async_handle.async_result())).succeeded_candidates, 2)

        callback_portfolio = {
            **portfolio_spec,
            "run_id": "python-callback-portfolio",
            "output_dir": str(self.output_dir / "callback-portfolio"),
            "strategy": dict(portfolio_spec["strategy"]),
        }
        callback_portfolio["strategy"]["strategy_id"] = "python-custom-primary"
        callback_secondary = dict(secondary)
        callback_secondary["strategy_id"] = "python-custom-secondary"
        callback_portfolio["portfolio"] = {
            "primary_priority": 0,
            "primary_reserved_capital_raw": 50_000_000_000_000,
            "additional_strategies": [{
                "priority": 1,
                "reserved_capital_raw": 50_000_000_000_000,
                "strategy": callback_secondary,
            }],
        }
        bindings = [
            {
                "strategy_id": "python-custom-primary",
                "strategy_version": "callback-v1",
                "source_sha256": "b" * 64,
                "strategy": BuyOnce(),
            },
            {
                "strategy_id": "python-custom-secondary",
                "strategy_version": "callback-v2",
                "source_sha256": "c" * 64,
                "strategy": BuyOnce(),
            },
        ]
        callback_portfolio_outcome = run_backtest_with_strategy_slots(
            callback_portfolio, bindings
        )
        callback_fills = Path(callback_portfolio_outcome.artifacts["fills"]).read_text(encoding="utf-8")
        self.assertTrue(any("python-custom-primary" in line for line in callback_fills.splitlines()[1:]))
        self.assertTrue(any("python-custom-secondary" in line for line in callback_fills.splitlines()[1:]))
        self.assertTrue(verify_run(callback_portfolio_outcome).verified)
        changed_bindings = [dict(binding) for binding in bindings]
        changed_bindings[1]["source_sha256"] = "d" * 64
        with self.assertRaises(ConflictError):
            run_backtest_with_strategy_slots(callback_portfolio, changed_bindings)

        callback_handle_spec = {
            **callback_portfolio,
            "run_id": "python-callback-portfolio-handle",
            "output_dir": str(self.output_dir / "callback-portfolio-handle"),
        }
        callback_handle = start_backtest_with_strategy_slots(
            callback_handle_spec,
            [dict(binding, strategy=BuyOnce()) for binding in bindings],
        )
        self.assertEqual(callback_handle.wait(timeout_ms=30_000), "succeeded")
        self.assertTrue(verify_run(callback_handle.result()).verified)

        async def check_async_facade():
            client = AsyncCoreClient()
            async_spec = {
                **callback_portfolio,
                "run_id": "python-callback-portfolio-async",
                "output_dir": str(self.output_dir / "callback-portfolio-async"),
            }
            async_result = await client.async_run_backtest_with_strategy_slots(
                async_spec, [dict(binding, strategy=BuyOnce()) for binding in bindings]
            )
            self.assertTrue(verify_run(async_result).verified)
            handle_spec = {
                **callback_portfolio,
                "run_id": "python-callback-portfolio-async-handle",
                "output_dir": str(self.output_dir / "callback-portfolio-async-handle"),
            }
            async_handle = await client.async_start_backtest_with_strategy_slots(
                handle_spec, [dict(binding, strategy=BuyOnce()) for binding in bindings]
            )
            self.assertEqual(await async_handle.async_wait(timeout=30), "succeeded")
            async_handle_result = await async_handle.async_result()
            self.assertTrue(verify_run(async_handle_result).verified)

        asyncio.run(check_async_facade())

        async_spec = dict(spec)
        async_spec["run_id"] = "python-custom-strategy-handle"
        async_spec["output_dir"] = str(self.output_dir / "custom-handle")
        handle = start_backtest_with_strategy(
            async_spec,
            BuyOnce(),
            strategy_id="python-custom-strategy",
            strategy_version="python-test-v1",
            source_sha256="a" * 64,
        )
        self.assertEqual(handle.wait(timeout_ms=30_000), "succeeded")
        async_outcome = handle.result()
        self.assertIsNotNone(async_outcome)
        self.assertGreater(async_outcome.fills, 0)
        self.assertTrue(verify_run(async_outcome).verified)

    def test_python_strategy_warmup_gates_orders_in_the_shared_rust_backtest(self):
        self._require()
        scale = 1_000_000_000
        bars_path = self.output_dir / "python-warmup-bars.json"
        prices = [100 * scale + index * scale for index in range(7)]
        bars_path.write_text(json.dumps({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "python-warmup-v1",
            "ts": [1_000 + index * 1_000 for index in range(len(prices))],
            "open_raw": prices,
            "high_raw": [price + scale for price in prices],
            "low_raw": [price - scale for price in prices],
            "close_raw": prices,
            "volume_raw": [scale] * len(prices),
        }), encoding="utf-8")
        spec = self._spec(
            "python-warmup-gate", self.output_dir / "warmup-gate", str(bars_path)
        )
        spec["strategy"]["strategy_id"] = "python-warmup-gate"
        spec["strategy"]["fast_window"] = 5
        spec["strategy"]["slow_window"] = 20

        from qianxing import run_backtest_with_strategy, verify_run
        from qianxing_strategy import BaseStrategy

        class BuyOnFirstDecision(BaseStrategy):
            def __init__(self):
                self.completed = []
                self.seen = 0
                self.emitted = False
                self.ready = False

            def history_requirements(self):
                return {"warmup_bars": 3}

            def on_warmup_complete(self, context, instrument, observed_bars):
                self.completed.append((instrument, observed_bars))
                self.ready = True

            def on_bar(self, context, bar):
                self.seen += 1
                intents = []
                if self.ready and not self.emitted:
                    self.emitted = True
                    intents.append({
                        "intent_id": 1,
                        "instrument": bar["instrument"],
                        "side": "Buy",
                        "qty": 1_000_000,
                        "limit": None,
                        "policy": None,
                        "reduce_only": False,
                        "post_only": False,
                    })
                return self.decision(
                    context,
                    "Bar",
                    bar,
                    intents=intents,
                )

        strategy = BuyOnFirstDecision()
        outcome = run_backtest_with_strategy(
            spec,
            strategy,
            strategy_id="python-warmup-gate",
            strategy_version="python-test-v1",
            source_sha256="b" * 64,
        )
        self.assertEqual(strategy.seen, 6)
        self.assertEqual(
            strategy.completed,
            [({"symbol": "BTCUSDT", "venue": "BINANCE"}, 3)],
        )
        with Path(outcome.artifacts["fills"]).open(encoding="utf-8", newline="") as stream:
            fills = list(csv.DictReader(stream))
        self.assertEqual(len(fills), 1)
        self.assertGreaterEqual(int(fills[0]["ts"]), 4_000)
        self.assertTrue(verify_run(outcome).verified)

    def test_python_order_update_decision_returns_to_the_rust_fill_loop(self):
        self._require()
        scale = 1_000_000_000
        bars_path = self.output_dir / "python-order-update-bars.json"
        prices = [100 * scale] * 4
        bars_path.write_text(json.dumps({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "python-order-update-v1",
            "ts": [1_000, 2_000, 3_000, 4_000],
            "open_raw": prices,
            "high_raw": prices,
            "low_raw": prices,
            "close_raw": prices,
            "volume_raw": [10 * scale] * len(prices),
        }), encoding="utf-8")
        spec = self._spec(
            "python-order-update-roundtrip", self.output_dir / "order-update", str(bars_path)
        )
        spec["strategy"]["strategy_id"] = "python-order-update"
        spec["strategy"]["fast_window"] = 5
        spec["strategy"]["slow_window"] = 20

        class FillResponsive:
            def __init__(self):
                self.emitted = False
                self.updates = []
                self.instrument = None

            def on_event(self, context, event):
                bar = event["Bar"]
                self.instrument = bar["instrument"]
                intents = []
                if not self.emitted:
                    self.emitted = True
                    intents.append({
                        "intent_id": 1,
                        "instrument": bar["instrument"],
                        "side": "Buy",
                        "qty": scale,
                        "limit": None,
                        "policy": None,
                        "reduce_only": False,
                        "post_only": False,
                    })
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

            def on_order_update(self, context, update):
                self.updates.append((
                    context["positions"].get("BTCUSDT.BINANCE", 0),
                    context["cash"].get("USDT", 0),
                    context["available_margin_raw"],
                    update,
                ))
                if len(self.updates) > 1:
                    return None
                return {
                    "schema_version": 1,
                    "request_id": "close-after-fill",
                    "strategy_id": context["strategy_id"],
                    "signal_id": 2,
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": [{
                        "intent_id": 2,
                        "instrument": self.instrument,
                        "side": "Sell",
                        "qty": scale,
                        "limit": None,
                        "policy": None,
                        "reduce_only": True,
                        "post_only": False,
                    }],
                }

        from qianxing import run_backtest_with_strategy, verify_run

        strategy = FillResponsive()
        outcome = run_backtest_with_strategy(
            spec,
            strategy,
            strategy_id="python-order-update",
            strategy_version="python-test-v1",
            source_sha256="b" * 64,
        )
        self.assertEqual(outcome.fills, 2)
        self.assertEqual(len(strategy.updates), 2)
        self.assertEqual(strategy.updates[0][0], scale)
        self.assertLess(strategy.updates[0][1], spec["initial_cash_raw"])
        self.assertEqual(strategy.updates[0][2], strategy.updates[0][1])
        self.assertEqual(strategy.updates[0][3]["schema_version"], 1)
        self.assertEqual(strategy.updates[0][3]["event"], "fill")
        self.assertTrue(verify_run(outcome).verified)

    def test_python_orderbook_order_update_decision_returns_to_the_rust_fill_loop(self):
        self._require()
        from qianxing import BuiltinStrategySpec, DepthBacktestSpec

        scale = 1_000_000_000
        snapshots = []
        for index, mid in enumerate((100, 101, 102), start=1):
            snapshots.append({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": index * 1_000,
                "sequence": index,
                "bids": [{"price": (mid - 1) * scale, "qty": 10 * scale}],
                "asks": [{"price": (mid + 1) * scale, "qty": 10 * scale}],
            })
        depth_path = self.output_dir / "python-orderbook-order-update.json"
        depth_path.write_text(json.dumps({
            "schema_version": 1,
            "source": "python-orderbook-order-update-v1",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots,
        }), encoding="utf-8")
        strategy_id = "python-orderbook-order-update"
        spec = DepthBacktestSpec(
            run_id="python-orderbook-order-update",
            depth_path=str(depth_path),
            tier="l2",
            settlement_currency="USDT",
            initial_cash_raw=10_000 * scale,
            fee_bps=10,
            latency_snapshots=0,
            queue_position_bps=0,
            market_impact_bps=0,
            output_dir=str(self.output_dir / "orderbook-order-update"),
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id=strategy_id),
        )

        class FillResponsiveBook:
            def __init__(self):
                self.submitted = False
                self.updates = []
                self.instrument = None

            def on_event(self, context, event):
                _, item = next(iter(event.items()))
                self.instrument = item["instrument"]
                intents = []
                if not self.submitted:
                    self.submitted = True
                    intents.append({
                        "intent_id": 1,
                        "instrument": item["instrument"],
                        "side": "Buy",
                        "qty": scale,
                        "limit": None,
                        "policy": None,
                        "reduce_only": False,
                        "post_only": False,
                    })
                return {
                    "schema_version": 1,
                    "request_id": f"{context['strategy_id']}:{item['ts']}",
                    "strategy_id": context["strategy_id"],
                    "signal_id": item["ts"],
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": intents,
                }

            def on_order_update(self, context, update):
                position = context["positions"].get("BTCUSDT.BINANCE", 0)
                cash = context["cash"].get("USDT", 0)
                self.updates.append((position, cash, update))
                if position == 0:
                    return None
                return {
                    "schema_version": 1,
                    "request_id": "close-orderbook-fill",
                    "strategy_id": context["strategy_id"],
                    "signal_id": 2,
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": [{
                        "intent_id": 2,
                        "instrument": self.instrument,
                        "side": "Sell",
                        "qty": scale,
                        "limit": None,
                        "policy": None,
                        "reduce_only": True,
                        "post_only": False,
                    }],
                }

        callback = FillResponsiveBook()
        outcome = self.bridge.run_depth_backtest_with_strategy(
            spec.to_dict(), callback,
            strategy_id=strategy_id,
            strategy_version="python-depth-fill-v1",
            source_sha256="c" * 64,
        )
        self.assertEqual(outcome["fills"], 2)
        self.assertEqual([event[0] for event in callback.updates], [scale, 0])
        self.assertGreater(callback.updates[0][1], 0)
        self.assertLess(callback.updates[0][1], spec.initial_cash_raw)
        self.assertEqual(callback.updates[0][2]["schema_version"], 1)
        self.assertEqual(callback.updates[0][2]["event"], "fill")
        self.assertTrue(self.bridge.verify_depth_run(outcome)["verified"])

    def test_thirteen_classic_builtin_strategies_run_through_python_rust_kernel(self):
        self._require()
        scale = 1_000_000_000
        timestamps, opens, highs, lows, closes, volumes = [], [], [], [], [], []
        for index in range(1, 91):
            up = (index // 5) % 7 < 4
            spike = 3 if index % 9 == 0 else 1
            body = spike * (2 + index % 13) * scale
            drift = (40 + index // 3) * scale
            open_raw = drift if up else drift + body
            close_raw = drift + body if up else drift
            upper_wick = 0 if index % 6 == 0 else (1 + index % 4) * scale
            lower_wick = 0 if index % 7 == 0 else (1 + index % 5) * scale
            timestamps.append(1_000 + (index - 1) * 1_000)
            opens.append(open_raw)
            highs.append(max(open_raw, close_raw) + upper_wick)
            lows.append(min(open_raw, close_raw) - lower_wick)
            closes.append(close_raw)
            volumes.append((100 + index % 7 * 13) * scale)
        # Drive a lower-band entry and a later upper-band exit for the conservative spot model.
        bollinger_closes = list(closes)
        bollinger_closes[14] = 5 * scale
        bollinger_closes[45] = 250 * scale

        builtin_names = (
            "sma_cross", "ema_cross", "macd", "rsi", "bollinger",
            "donchian_breakout", "momentum", "mean_reversion", "grid", "atr_trend",
            "keltner_trend", "vwap_reversion", "volatility_breakout",
        )
        for kind in builtin_names:
            with self.subTest(strategy=kind):
                selected_closes = bollinger_closes if kind == "bollinger" else closes
                selected_opens = selected_closes if kind == "bollinger" else opens
                selected_highs = selected_closes if kind == "bollinger" else highs
                selected_lows = selected_closes if kind == "bollinger" else lows
                bars_path = self.output_dir / f"{kind}-bars.json"
                bars_path.write_text(json.dumps({
                    "schema_version": 1,
                    "instrument": "BTCUSDT.BINANCE",
                    "source": f"python-{kind}-conformance-v1",
                    "ts": timestamps,
                    "open_raw": selected_opens,
                    "high_raw": selected_highs,
                    "low_raw": selected_lows,
                    "close_raw": selected_closes,
                    "volume_raw": volumes,
                }), encoding="utf-8")
                spec = self._spec(f"py-{kind}", self.output_dir / kind, str(bars_path))
                spec["strategy"].update({
                    "kind": kind,
                    "strategy_id": f"python-{kind}",
                    "fast_window": 5,
                    "slow_window": 20,
                    "period": 14,
                    "threshold_bps": 100,
                })
                outcome = self.bridge.run_backtest(spec)
                self.assertEqual(outcome["run_id"], f"py-{kind}")
                self.assertGreater(outcome["fills"], 0, kind)
                self.assertEqual(len(outcome["result_hash"]), 16)

        # The same selector also runs the whole classic set as one Rust-owned parameter
        # experiment and returns the shared comparison; it does not loop in Python to rank runs.
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentSplitPlan,
            RunExperimentSpec,
            run_experiment,
        )

        selection_spec = RunExperimentSpec(
            experiment_id="py-classic-strategy-selection",
            base=BacktestSpec(
                run_id="unused-classic-selection",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(bars_path),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(self.output_dir / "classic-selection"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="py-classic-selection",
                    fast_window=5,
                    slow_window=20,
                    period=14,
                    threshold_bps=100,
                ),
            ),
            parameter_space=(),
            strategy_kinds=builtin_names,
        )
        selected = run_experiment(selection_spec)
        self.assertEqual(selected.total_candidates, len(builtin_names))
        self.assertEqual(selected.succeeded_candidates, len(builtin_names))
        self.assertEqual(
            [candidate.strategy_kind for candidate in selected.candidates],
            sorted(builtin_names),
        )
        self.assertEqual(len(selected.comparison.runs), len(builtin_names))

        oos = run_experiment(
            RunExperimentSpec(
                experiment_id="py-classic-strategy-selection-oos",
                base=selection_spec.base,
                parameter_space=(),
                strategy_kinds=builtin_names,
                split_plan=ExperimentSplitPlan(
                    train_bars=40, test_bars=20, walk_forward=True, step_bars=20
                ),
            )
        )
        self.assertIsNotNone(oos.out_of_sample)
        self.assertEqual(len(oos.out_of_sample.folds), 2)
        for fold in oos.out_of_sample.folds:
            self.assertIn(fold.selected_strategy_kind, builtin_names)
            self.assertEqual(
                fold.selected_strategy_kind,
                oos.candidates[fold.selected_ordinal - 1].strategy_kind,
            )

    def test_the_shared_use_cases_run_end_to_end_from_python(self):
        self._require()
        out = self.output_dir
        verdict = self.bridge.validate_dataset(
            {
                "schema_version": 1,
                "dataset_id": "py-dataset",
                "bars_path": str(
                    self.root / "deploy" / "qianxing.bar-frame.example.json"
                ),
            }
        )
        self.assertTrue(verdict["usable"], verdict["gaps"])
        self.assertEqual(verdict["rows"], 70)

        spec = self._spec("py-run", out)
        spec["account_id"] = "acct.python-test"
        outcome = self.bridge.run_backtest(spec)
        self.assertGreater(outcome["fills"], 0)
        self.assertEqual(outcome["equity_points"], 70)
        self.assertEqual(len(outcome["result_hash"]), 16)
        self.assertEqual(outcome["account_id"], "acct.python-test")
        summary = json.loads(Path(outcome["artifacts"]["summary"]).read_text(encoding="utf-8"))
        self.assertEqual(summary["account_id"], "acct.python-test")

        verification = self.bridge.verify_run(outcome)
        self.assertTrue(verification["verified"], verification["mismatches"])
        self.assertEqual(verification["result_hash"], outcome["result_hash"])

        second_spec = self._spec("py-run-alt", out / "alt")
        second_spec["strategy"]["strategy_id"] = "py-sma-alt"
        second_spec["strategy"]["fast_window"] = 3
        second_spec["strategy"]["slow_window"] = 4
        second = self.bridge.run_backtest(second_spec)
        comparison = self.bridge.compare_runs({
            "schema_version": 1,
            "runs": [
                {key: outcome[key] for key in ("run_id", "instrument", "data_fingerprint", "result_hash", "return_bps", "max_drawdown_bps")},
                {key: second[key] for key in ("run_id", "instrument", "data_fingerprint", "result_hash", "return_bps", "max_drawdown_bps")},
            ],
        })
        self.assertEqual([item["rank"] for item in comparison["runs"]], [1, 2])
        self.assertEqual(comparison["data_fingerprint"], outcome["data_fingerprint"])

        from qianxing import CompareRunsSpec, ComparedRun, compare_runs

        typed = compare_runs(
            CompareRunsSpec(
                runs=tuple(
                    ComparedRun(
                        run_id=item["run_id"],
                        instrument=item["instrument"],
                        data_fingerprint=item["data_fingerprint"],
                        result_hash=item["result_hash"],
                        return_bps=item["return_bps"],
                        max_drawdown_bps=item["max_drawdown_bps"],
                    )
                    for item in (outcome, second)
                )
            )
        )
        self.assertEqual([item.rank for item in typed.runs], [1, 2])

    def test_fixed_notional_sizing_runs_inside_the_shared_rust_kernel(self):
        self._require()
        spec = self._spec("py-notional-sizing", self.output_dir / "notional")
        spec["strategy"]["target_notional_raw"] = 1_000_000_000_000
        outcome = self.bridge.run_backtest(spec)
        self.assertGreater(outcome["fills"], 0)
        fills_path = Path(outcome["artifacts"]["fills"])
        quantities = [
            int(row.split(",")[2])
            for row in fills_path.read_text(encoding="utf-8").splitlines()[1:]
            if row.strip()
        ]
        self.assertTrue(quantities)
        self.assertTrue(
            all(quantity > 0 and quantity != 1_000_000_000 for quantity in quantities),
            f"名义金额应按价格换算数量，而非使用固定 quantity_raw: {quantities}",
        )

    def test_typed_experiment_rejects_quantity_grid_shadowed_by_rust_sizing(self):
        self._require()
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentParameterSpace,
            InvalidInputError,
            RunExperimentSpec,
            run_experiment,
        )

        spec = RunExperimentSpec(
            experiment_id="py-quantity-sizing-conflict",
            base=BacktestSpec(
                run_id="unused-py-quantity-sizing-conflict",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(self.output_dir / "quantity-sizing-conflict"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="py-quantity-sizing-conflict",
                    target_notional_raw=10_000_000_000,
                    fast_window=2,
                    slow_window=3,
                ),
            ),
            parameter_space=(
                ExperimentParameterSpace("quantity_raw", (500_000_000, 1_000_000_000)),
            ),
        )
        with self.assertRaises(InvalidInputError) as raised:
            run_experiment(spec)
        self.assertEqual(raised.exception.category, "INVALID_INPUT")
        self.assertIn("quantity_raw 参数维度与策略 sizing 模式冲突", str(raised.exception))

    def test_available_margin_sizing_uses_the_live_spot_cash_snapshot(self):
        self._require()
        spec = self._spec("py-margin-sizing", self.output_dir / "margin")
        spec["strategy"]["target_available_margin_bps"] = 1_000
        outcome = self.bridge.run_backtest(spec)
        self.assertGreater(outcome["fills"], 0)
        fills_path = Path(outcome["artifacts"]["fills"])
        quantities = [
            int(row.split(",")[2])
            for row in fills_path.read_text(encoding="utf-8").splitlines()[1:]
            if row.strip()
        ]
        self.assertTrue(quantities)
        self.assertTrue(
            all(quantity > 0 and quantity != 1_000_000_000 for quantity in quantities),
            f"资金比例 sizing 应由当前可用现金和可见价格换算数量: {quantities}",
        )

    def test_volatility_budget_sizing_runs_inside_the_shared_rust_kernel(self):
        self._require()
        spec = self._spec("py-volatility-sizing", self.output_dir / "volatility")
        spec["strategy"]["target_volatility_bps"] = 25
        outcome = self.bridge.run_backtest(spec)
        self.assertGreater(outcome["fills"], 0)
        fills_path = Path(outcome["artifacts"]["fills"])
        quantities = [
            int(row.split(",")[2])
            for row in fills_path.read_text(encoding="utf-8").splitlines()[1:]
            if row.strip()
        ]
        self.assertTrue(quantities)
        self.assertTrue(
            all(quantity > 0 and quantity != 1_000_000_000 for quantity in quantities),
            f"波动率预算 sizing 应由可用资金与可见 ATR 换算数量: {quantities}",
        )

    def test_typed_python_sdk_runs_kelly_backtest_and_sweeps_the_fraction(self):
        self._require()
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentParameterRange,
            ExperimentParameterSpace,
            ExperimentSearchPlan,
            ExperimentSplitPlan,
            RunExperimentSpec,
            run_experiment,
            run_backtest,
        )

        bars_path = self.output_dir / "kelly-bars.json"
        close = 100_000_000_000
        closes = []
        for index in range(70):
            if index:
                close = close - close // 1_000 if index % 10 == 0 else close + close // 100
            closes.append(close)
        frame = {
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "python-kelly-sdk-v1",
            "ts": [1_000 + index * 1_000 for index in range(70)],
            "open_raw": closes,
            "high_raw": closes,
            "low_raw": closes,
            "close_raw": closes,
            "volume_raw": [1_000_000_000] * 70,
        }
        bars_path.write_text(json.dumps(frame), encoding="utf-8")
        strategy = BuiltinStrategySpec(
            kind="momentum",
            strategy_id="py-kelly-sdk",
            period=2,
            threshold_bps=0,
            target_kelly_fraction_bps=5_000,
            kelly_lookback_bars=30,
        )
        base = BacktestSpec(
            run_id="py-kelly-sdk",
            instrument="BTCUSDT.BINANCE",
            bars_path=str(bars_path),
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000_000,
            output_dir=str(self.output_dir / "kelly"),
            seed=17,
            strategy=strategy,
        )
        outcome = run_backtest(base)
        self.assertGreater(outcome.fills, 0)
        summary = json.loads(Path(outcome.artifacts["summary"]).read_text(encoding="utf-8"))
        self.assertEqual(summary["strategy_version"], "builtin-momentum-kelly-v1")

        experiment = RunExperimentSpec(
            experiment_id="py-kelly-grid",
            base=base,
            parameter_space=(
                ExperimentParameterSpace(
                    name="target_kelly_fraction_bps", values=(2_500, 5_000)
                ),
            ),
            split_plan=ExperimentSplitPlan(
                train_bars=40, test_bars=10, walk_forward=True, step_bars=10
            ),
            search=ExperimentSearchPlan(algorithm="random", seed=17, budget=2),
        )
        result = run_experiment(experiment)
        self.assertEqual(result.succeeded_candidates, 2)
        self.assertEqual(len({candidate.outcome.result_hash for candidate in result.candidates}), 2)
        self.assertEqual(result.search.algorithm, "random")
        self.assertEqual([trial.proposal for trial in result.search.trials], ["random", "random"])

        continuous_experiment = RunExperimentSpec(
            experiment_id="py-kelly-continuous",
            base=base,
            parameter_space=(),
            split_plan=ExperimentSplitPlan(
                train_bars=40, test_bars=10, walk_forward=True, step_bars=10
            ),
            search=ExperimentSearchPlan(
                algorithm="tpe_kde",
                seed=18,
                budget=5,
                continuous_parameter_space=(
                    ExperimentParameterRange(
                        "target_kelly_fraction_bps", 1_000, 9_000, 1_000
                    ),
                ),
            ),
        )
        continuous_result = run_experiment(continuous_experiment)
        self.assertEqual(continuous_result.succeeded_candidates, 5)
        self.assertEqual(
            [trial.proposal for trial in continuous_result.search.trials[:4]],
            ["tpe_kde_continuous_random_startup"] * 4,
        )
        self.assertEqual(
            continuous_result.search.trials[4].proposal, "tpe_kde_continuous"
        )
        self.assertEqual(
            len({trial.parameters["target_kelly_fraction_bps"] for trial in continuous_result.search.trials}),
            5,
        )

    def test_run_handle_starts_waits_and_takes_result_from_rust(self):
        self._require()
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import start_backtest

        handle = start_backtest(self._spec("py-run-handle", self.output_dir))
        self.assertEqual(handle.run_id, "py-run-handle")
        self.assertEqual(handle.wait(timeout_ms=10_000), "succeeded")
        outcome = handle.result()
        self.assertIsNotNone(outcome)
        self.assertGreater(outcome.fills, 0)
        self.assertIsNone(handle.result(), "outcome is delivered only once")
        events = handle.events()
        self.assertEqual([event.kind for event in events], ["started", "succeeded"])
        self.assertEqual([event.seq for event in events], [1, 2])
        self.assertEqual(events[1].run_id, handle.run_id)
        self.assertEqual([event.kind for event in handle.events(after_seq=1)], ["succeeded"])
        with self.assertRaises(ValueError):
            handle.events(after_seq=-1)

    def test_parameter_grid_runs_and_compares_candidates_through_rust(self):
        self._require()
        spec = {
            "schema_version": 1,
            "experiment_id": "py-grid",
            "base": self._spec("unused", self.output_dir),
            "parameter_space": [{"name": "fast_window", "values": [1, 2]}],
        }
        result = self.bridge.run_experiment(spec)
        self.assertEqual(result["total_candidates"], 2)
        self.assertEqual(result["completed_candidates"], 2)
        self.assertEqual(result["succeeded_candidates"], 2)
        self.assertEqual(result["failed_candidates"], 0)
        self.assertEqual(len(result["comparison"]["runs"]), 2)
        self.assertTrue(Path(result["artifact_path"]).is_file())
        self.assertTrue(Path(result["run_manifest"]).is_file())
        self.assertTrue(Path(result["run_record"]).is_file())
        self.assertTrue(Path(result["artifact_locations"]["experiment_report"]).is_file())
        self.assertNotEqual(result["artifact_locations"]["experiment_report"], result["artifact_path"])
        self.assertEqual(result["artifact_locations"]["run_manifest"], result["run_manifest"])
        self.assertTrue(Path(result["artifact_locations"]["experiment_input"]).is_file())
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "run-experiment-result-v1.json").read_text(encoding="utf-8"))
        resources = {
            "https://qianxing.local/schemas/backtest-outcome-v1.json": "backtest-outcome-v1.json",
            "https://qianxing.local/schemas/compare-runs-result-v1.json": "compare-runs-result-v1.json",
        }
        schema_registry = Registry()
        for uri, name in resources.items():
            payload = json.loads((Path(__file__).parents[2] / "schemas" / name).read_text(encoding="utf-8"))
            schema_registry = schema_registry.with_resource(uri, Resource.from_contents(payload))
        with Path(result["artifact_path"]).open(encoding="utf-8") as report_file:
            Draft202012Validator(result_schema, registry=schema_registry).validate(json.load(report_file))

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentSplitPlan,
            ExperimentParameterSpace,
            RunExperimentSpec,
            run_experiment,
            start_experiment,
        )

        typed = run_experiment(
            RunExperimentSpec(
                experiment_id="py-typed-grid",
                base=BacktestSpec(
                    run_id="unused",
                    instrument="BTCUSDT.BINANCE",
                    bars_path=str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
                    settlement_currency="USDT",
                    initial_cash_raw=100_000_000_000_000,
                    output_dir=str(self.output_dir / "typed"),
                    strategy=BuiltinStrategySpec(
                        kind="sma_cross",
                        strategy_id="py-typed-sma",
                        fast_window=2,
                        slow_window=4,
                    ),
                ),
                parameter_space=(ExperimentParameterSpace("fast_window", (2, 3)),),
                split_plan=ExperimentSplitPlan(
                    train_bars=20,
                    test_bars=10,
                    walk_forward=True,
                    step_bars=20,
                    purge_bars=1,
                    embargo_bars=10,
                ),
                baselines=("buy-and-hold", "zero-signal"),
            )
        )
        self.assertEqual(typed.completed_candidates, 2)
        self.assertEqual(typed.succeeded_candidates, 2)
        self.assertEqual(typed.failed_candidates, 0)
        self.assertEqual(len(typed.comparison.runs), 2)
        self.assertIsNotNone(typed.out_of_sample)
        self.assertGreaterEqual(len(typed.out_of_sample.folds), 1)
        self.assertEqual(typed.out_of_sample.folds[0].purge_range, (20, 21))
        self.assertEqual(typed.out_of_sample.folds[0].test_range, (21, 31))
        self.assertEqual(typed.out_of_sample.folds[0].embargo_range, (31, 41))
        with Path(typed.artifact_path).open(encoding="utf-8") as report_file:
            Draft202012Validator(result_schema, registry=schema_registry).validate(json.load(report_file))
        self.assertEqual(
            [row.name for row in typed.baselines], ["buy-and-hold", "zero-signal"]
        )

        handle = start_experiment(
            RunExperimentSpec(
                experiment_id="py-typed-grid-handle",
                base=BacktestSpec(
                    run_id="unused-handle",
                    instrument="BTCUSDT.BINANCE",
                    bars_path=str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
                    settlement_currency="USDT",
                    initial_cash_raw=100_000_000_000_000,
                    output_dir=str(self.output_dir / "typed-handle"),
                    strategy=BuiltinStrategySpec(
                        kind="sma_cross",
                        strategy_id="py-typed-sma-handle",
                        fast_window=2,
                        slow_window=4,
                    ),
                ),
                parameter_space=(ExperimentParameterSpace("fast_window", (2, 3)),),
            )
        )
        self.assertEqual(handle.wait(timeout_ms=10_000), "succeeded")
        async_result = handle.result()
        self.assertIsNotNone(async_result)
        self.assertEqual(async_result.completed_candidates, 2)
        self.assertIsNone(handle.result())

    def test_typed_python_sdk_quick_selects_classic_strategies_through_rust_experiment(self):
        self._require()
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            RunExperimentSpec,
            run_experiment,
        )

        kinds = (
            "sma_cross", "ema_cross", "macd", "rsi", "bollinger",
            "donchian_breakout", "momentum", "mean_reversion", "grid", "atr_trend",
            "keltner_trend", "vwap_reversion", "volatility_breakout",
        )
        result = run_experiment(
            RunExperimentSpec(
                experiment_id="py-classic-strategy-quick-select",
                base=BacktestSpec(
                    run_id="unused-classic-selection",
                    instrument="BTCUSDT.BINANCE",
                    bars_path=str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
                    settlement_currency="USDT",
                    initial_cash_raw=100_000_000_000_000,
                    output_dir=str(self.output_dir / "classic-selection"),
                    strategy=BuiltinStrategySpec(
                        kind="sma_cross",
                        strategy_id="py-classic-selection",
                        fast_window=2,
                        slow_window=4,
                    ),
                ),
                parameter_space=(),
                strategy_kinds=kinds,
            )
        )
        self.assertEqual(result.total_candidates, len(kinds))
        self.assertEqual(result.succeeded_candidates, len(kinds))
        self.assertEqual(
            [candidate.strategy_kind for candidate in result.candidates], sorted(kinds)
        )
        self.assertTrue(all(candidate.outcome is not None for candidate in result.candidates))
        self.assertEqual(len(result.comparison.runs), len(kinds))

    def test_l1_tick_and_typed_sdk_use_the_shared_rust_application_kernel(self):
        self._require()
        scale = 1_000_000_000
        snapshots = []
        for index in range(48):
            phase = index % 20
            mid = 100 + (phase * 2 if phase < 10 else (20 - phase) * 2)
            snapshots.append(
                {
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "ts": 1000 + index * 1000,
                    "sequence": index + 1,
                    "bids": [{"price": (mid - 1) * 1_000_000_000, "qty": 100_000_000_000}],
                    "asks": [{"price": (mid + 1) * 1_000_000_000, "qty": 100_000_000_000}],
                }
            )
        depth_path = self.output_dir / "ticks.json"
        depth_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "source": "python-sdk-contract",
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "snapshots": snapshots,
                }
            ),
            encoding="utf-8",
        )
        payload = {
            "schema_version": 1,
            "run_id": "python-tick-run",
            "depth_path": str(depth_path),
            "tier": "l1",
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000,
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": str(self.output_dir / "depth-runs"),
            "account_id": "acct.python-tick",
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "py-tick-sma",
                "quantity_raw": 1_000_000_000,
                "fast_window": 2,
                "slow_window": 3,
            },
        }
        outcome = self.bridge.run_depth_backtest(payload)
        self.assertEqual(outcome["tier"], "l1")
        self.assertEqual(outcome["account_id"], "acct.python-tick")
        self.assertEqual(outcome["equity_points"], 48)
        self.assertGreater(outcome["fills"], 0)
        self.assertTrue(Path(outcome["artifacts"]["run_manifest"]).is_file())
        depth_summary = json.loads(Path(outcome["artifacts"]["summary"]).read_text(encoding="utf-8"))
        self.assertEqual(depth_summary["account_id"], "acct.python-tick")

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BuiltinStrategySpec,
            DepthBacktestSpec,
            InstrumentId,
            MarketMakingSpec,
            OrderPolicy,
            PortfolioBacktestConfig,
            PortfolioStrategySpec,
            TradingInstrumentSpec,
            run_depth_backtest,
            read_depth_run_report_v1,
            start_depth_backtest,
            verify_depth_run,
        )

        legacy_depth_spec = DepthBacktestSpec(
            run_id="python-legacy-depth-shape",
            depth_path=str(depth_path),
            tier="l1",
            settlement_currency="USDT",
            initial_cash_raw=1_000_000_000_000_000,
            output_dir=str(self.output_dir / "legacy-depth-shape"),
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id="legacy-depth"),
        ).to_dict()
        self.assertNotIn("market_spec", legacy_depth_spec)
        self.assertNotIn("order_policy", legacy_depth_spec)
        self.assertNotIn("market_making", legacy_depth_spec)
        market_making_payload = MarketMakingSpec(
            spread_bps=50,
            quote_qty_raw=1_000_000_000,
            max_inventory_raw=5_000_000_000,
            tick_size_raw=10_000_000,
            inventory_skew_bps=25,
        ).to_dict()
        self.assertEqual(market_making_payload["inventory_skew_bps"], 25)
        legacy_positional_spec = DepthBacktestSpec(
            "python-legacy-positional-depth",
            str(depth_path),
            "l1",
            "USDT",
            1_000_000_000_000_000,
            str(self.output_dir / "legacy-positional-depth"),
            BuiltinStrategySpec(kind="sma_cross", strategy_id="legacy-positional-depth"),
            0,
            0,
            0,
            0,
            1,
        )
        self.assertEqual(legacy_positional_spec.schema_version, 1)

        typed = run_depth_backtest(
            DepthBacktestSpec(
                run_id="python-typed-tick-run",
                depth_path=str(depth_path),
                tier="l1",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / "typed-depth-runs"),
                account_id="acct.typed-tick",
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="py-typed-tick-sma",
                    fast_window=2,
                    slow_window=3,
                ),
                market_spec=TradingInstrumentSpec(
                    instrument=InstrumentId(symbol="BTCUSDT", venue="BINANCE"),
                    product="perpetual",
                    base_currency="BTC",
                    quote_currency="USDT",
                    settlement_currency="USDT",
                    contract_size=1_000_000_000,
                    linear=True,
                    inverse=False,
                    price_tick=1,
                    qty_step=1,
                    min_qty=1,
                    max_leverage=100,
                    maintenance_margin_bps=500,
                    valid_from=1000,
                    valid_to=49000,
                ),
                order_policy=OrderPolicy(margin_mode="cross", leverage=2),
            )
        )
        self.assertEqual(typed.tier, "l1")
        self.assertEqual(typed.account_id, "acct.typed-tick")
        self.assertEqual(typed.equity_points, 48)
        typed_summary = json.loads(Path(typed.artifacts["summary"]).read_text(encoding="utf-8"))
        self.assertEqual(typed_summary["market_spec"]["product"], "perpetual")
        self.assertEqual(typed_summary["order_policy"]["leverage"], 2)
        self.assertTrue(verify_depth_run(typed).verified)
        typed_report = read_depth_run_report_v1(typed)
        self.assertEqual(typed_report["schema_version"], 1)
        self.assertTrue(typed_report["verification"]["verified"])
        self.assertEqual(typed_report["run_id"], typed.run_id)
        self.assertEqual(typed_report["equity"]["total"], typed.equity_points)
        self.assertEqual(typed_report["fills"]["total"], typed.fills)

        kelly_snapshots = []
        for index in range(48):
            mid = 100 + index * 2 + (5 if index % 2 else 0)
            kelly_snapshots.append(
                {
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "ts": 1000 + index * 1000,
                    "sequence": index + 1,
                    "bids": [{"price": (mid - 1) * scale, "qty": 100 * scale}],
                    "asks": [{"price": (mid + 1) * scale, "qty": 100 * scale}],
                }
            )
        kelly_depth_path = self.output_dir / "kelly-depth.json"
        kelly_depth_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "source": "python-sdk-kelly-depth-contract",
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "snapshots": kelly_snapshots,
                }
            ),
            encoding="utf-8",
        )

        kelly_depth_spec = DepthBacktestSpec(
            run_id="python-depth-kelly-sizing",
            depth_path=str(kelly_depth_path),
            tier="l2",
            settlement_currency="USDT",
            initial_cash_raw=1_000_000_000_000_000,
            output_dir=str(self.output_dir / "depth-kelly-sizing"),
            strategy=BuiltinStrategySpec(
                kind="momentum",
                strategy_id="py-depth-kelly",
                fast_window=2,
                slow_window=3,
                period=2,
                threshold_bps=1,
                target_kelly_fraction_bps=5_000,
                kelly_lookback_bars=30,
            ),
        )
        kelly_depth = run_depth_backtest(kelly_depth_spec)
        self.assertGreater(kelly_depth.fills, 0)
        self.assertTrue(verify_depth_run(kelly_depth).verified)
        kelly_handle = start_depth_backtest(
            DepthBacktestSpec(
                run_id="python-depth-kelly-handle",
                depth_path=str(kelly_depth_path),
                tier="l1",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / "depth-kelly-handle"),
                strategy=BuiltinStrategySpec(
                    kind="momentum",
                    strategy_id="py-depth-kelly-handle",
                    fast_window=2,
                    slow_window=3,
                    period=2,
                    threshold_bps=1,
                    target_kelly_fraction_bps=5_000,
                    kelly_lookback_bars=30,
                ),
            )
        )
        self.assertEqual(kelly_handle.wait(timeout_ms=30_000), "succeeded")
        self.assertGreater(kelly_handle.result().fills, 0)

        typed_portfolio = run_depth_backtest(
            DepthBacktestSpec(
                run_id="python-depth-portfolio",
                depth_path=str(depth_path),
                tier="l2",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / "depth-portfolio"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="python-depth-sma",
                    fast_window=2,
                    slow_window=3,
                ),
                portfolio=PortfolioBacktestConfig(
                    primary_priority=0,
                    primary_reserved_capital_raw=500_000_000_000_000,
                    additional_strategies=(
                        PortfolioStrategySpec(
                            priority=1,
                            reserved_capital_raw=500_000_000_000_000,
                            strategy=BuiltinStrategySpec(
                                kind="momentum",
                                strategy_id="python-depth-momentum",
                                fast_window=2,
                                slow_window=3,
                            ),
                        ),
                    ),
                ),
            )
        )
        self.assertGreater(typed_portfolio.fills, 0)
        portfolio_fills = Path(typed_portfolio.artifacts["fills"]).read_text(encoding="utf-8")
        portfolio_owners = {
            row.split(",")[6]
            for row in portfolio_fills.splitlines()[1:]
            if row.strip()
        }
        self.assertIn("python-depth-sma", portfolio_owners)
        self.assertIn("python-depth-momentum", portfolio_owners)
        portfolio_summary = json.loads(
            Path(typed_portfolio.artifacts["summary"]).read_text(encoding="utf-8")
        )
        self.assertEqual(
            portfolio_summary["strategy_ids"],
            ["python-depth-sma", "python-depth-momentum"],
        )
        self.assertEqual(
            sum(portfolio_summary["strategy_fills"].values()),
            typed_portfolio.fills,
        )
        self.assertTrue(verify_depth_run(typed_portfolio).verified)

        market_maker = run_depth_backtest(
            DepthBacktestSpec(
                run_id="python-typed-market-making",
                depth_path=str(depth_path),
                tier="l2",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / "typed-market-making"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross", strategy_id="py-market-making", fast_window=2, slow_window=3
                ),
                fee_bps=10,
                market_making=MarketMakingSpec(
                    spread_bps=100,
                    quote_qty_raw=1_000_000_000,
                    max_inventory_raw=5_000_000_000,
                    tick_size_raw=10_000_000,
                    inventory_skew_bps=50,
                ),
            )
        )
        self.assertGreater(market_maker.fills, 0)
        self.assertTrue(verify_depth_run(market_maker).verified)

        depth_handle_payload = {**payload, "run_id": "python-tick-handle-run"}
        depth_handle_payload["manifest_run_id"] = (
            "depth-backtest:l1:sma_cross:BTCUSDT.BINANCE"
        )
        depth_handle_payload["output_dir"] = str(self.output_dir / "depth-handle-runs")
        handle = start_depth_backtest(depth_handle_payload)
        self.assertEqual(handle.run_id, "python-tick-handle-run")
        self.assertEqual(handle.wait(timeout_ms=10_000), "succeeded")
        depth_result = handle.result()
        self.assertIsNotNone(depth_result)
        self.assertEqual(depth_result.tier, "l1")
        self.assertEqual(depth_result.equity_points, 48)
        self.assertEqual(
            depth_result.manifest_run_id,
            "depth-backtest:l1:sma_cross:BTCUSDT.BINANCE",
        )
        self.assertTrue(verify_depth_run(depth_result).verified)
        self.assertIsNone(handle.result(), "depth outcome is delivered only once")

        depth_experiment_payload = {
            "schema_version": 1,
            "experiment_id": "python-tick-grid",
            "base": {**payload, "run_id": "unused-python-tick-grid"},
            "parameter_space": [{"name": "quantity_raw", "values": [1_000_000_000, 2_000_000_000]}],
        }
        depth_experiment = self.bridge.run_depth_experiment(depth_experiment_payload)
        self.assertEqual(depth_experiment["completed_candidates"], 2)
        self.assertEqual(depth_experiment["failed_candidates"], 0)
        self.assertEqual(depth_experiment["succeeded_candidates"], 2)
        self.assertTrue(Path(depth_experiment["artifact_path"]).is_file())
        self.assertTrue(Path(depth_experiment["artifact_locations"]["experiment_input"]).is_file())
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "run-depth-experiment-result-v1.json").read_text(encoding="utf-8"))
        depth_result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "depth-backtest-outcome-v1.json").read_text(encoding="utf-8"))
        schema_registry = Registry().with_resource(
            "https://qianxing.local/schemas/depth-backtest-outcome-v1.json",
            Resource.from_contents(depth_result_schema),
        )
        with Path(depth_experiment["artifact_path"]).open(encoding="utf-8") as report_file:
            Draft202012Validator(result_schema, registry=schema_registry).validate(json.load(report_file))
        depth_experiment_handle = self.bridge.start_depth_experiment(
            {
                **depth_experiment_payload,
                "experiment_id": "python-tick-grid-handle",
                "base": {
                    **depth_experiment_payload["base"],
                    "output_dir": str(self.output_dir / "tick-grid-handle"),
                },
            }
        )
        self.assertEqual(depth_experiment_handle.wait(10_000), "succeeded")
        self.assertIsNotNone(depth_experiment_handle.result_json())
        self.assertIsNone(depth_experiment_handle.result_json())

        from qianxing import (
            AsyncCoreClient,
            ExperimentParameterSpace,
            RunDepthExperimentSpec,
            run_depth_backtest_with_strategy,
            run_depth_backtest_with_strategy_slots,
            run_depth_experiment,
            start_depth_backtest_with_strategy,
            start_depth_backtest_with_strategy_slots,
            start_depth_experiment,
            PortfolioBacktestConfig,
            PortfolioStrategySpec,
        )

        typed_depth_experiment = RunDepthExperimentSpec(
            experiment_id="python-typed-tick-grid",
            base=DepthBacktestSpec(
                run_id="unused-python-typed-tick-grid",
                depth_path=str(depth_path),
                tier="l1",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / "typed-tick-grid"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross", strategy_id="py-typed-tick-grid", fast_window=2, slow_window=3
                ),
            ),
            parameter_space=(ExperimentParameterSpace("quantity_raw", (1_000_000_000,)),),
        )
        typed_result = run_depth_experiment(typed_depth_experiment)
        self.assertEqual(typed_result.succeeded_candidates, 1)
        selected_depth_strategies = (
            "sma_cross", "ema_cross", "macd", "rsi", "bollinger",
            "donchian_breakout", "momentum", "mean_reversion", "grid", "atr_trend",
        )
        typed_selection = RunDepthExperimentSpec(
            experiment_id="python-typed-depth-strategy-selection",
            base=DepthBacktestSpec(
                **{
                    **typed_depth_experiment.base.__dict__,
                    "run_id": "unused-python-typed-depth-selection",
                    "output_dir": str(self.output_dir / "typed-depth-selection"),
                }
            ),
            parameter_space=(),
            strategy_kinds=selected_depth_strategies,
        )
        self.assertEqual(
            typed_selection.to_dict()["strategy_kinds"], sorted(selected_depth_strategies)
        )
        selection_result = run_depth_experiment(typed_selection)
        self.assertEqual(selection_result.total_candidates, 10)
        self.assertEqual(selection_result.succeeded_candidates, 10)
        self.assertEqual(
            [candidate.strategy_kind for candidate in selection_result.candidates],
            sorted(selected_depth_strategies),
        )

        class CustomDepthStrategy:
            def __init__(self):
                self.emitted = False
                self.events = []

            def on_event(self, context, event):
                name, item = next(iter(event.items()))
                self.events.append(name)
                intents = []
                if not self.emitted:
                    self.emitted = True
                    intents.append({
                        "intent_id": 1,
                        "instrument": item["instrument"],
                        "side": "Buy",
                        "qty": 1_000_000_000,
                        "limit": None,
                        "policy": None,
                        "reduce_only": False,
                        "post_only": False,
                    })
                return {
                    "schema_version": 1,
                    "request_id": f"{context['strategy_id']}:{item['ts']}",
                    "strategy_id": context["strategy_id"],
                    "signal_id": item["ts"],
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": intents,
                }

        for tier, levels, expected_event in (("l1", 1, "Tick"), ("l2", 2, "OrderBook")):
            custom_spec = DepthBacktestSpec(
                run_id=f"python-custom-depth-{tier}",
                depth_path=str(depth_path),
                tier=tier,
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / f"custom-depth-{tier}"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross", strategy_id=f"python-custom-{tier}"
                ),
            )
            if levels == 2:
                l2_snapshots = []
                for index in range(48):
                    mid = 100 + index % 9
                    l2_snapshots.append({
                        "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                        "ts": 1_000 + index * 1_000,
                        "sequence": index + 1,
                        "bids": [
                            {"price": (mid - 1 - level) * scale, "qty": 100 * scale}
                            for level in range(2)
                        ],
                        "asks": [
                            {"price": (mid + 1 + level) * scale, "qty": 100 * scale}
                            for level in range(2)
                        ],
                    })
                l2_path = self.output_dir / "custom-depth-l2.json"
                l2_path.write_text(json.dumps({
                    "schema_version": 1,
                    "source": "python-custom-depth-strategy",
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "snapshots": l2_snapshots,
                }), encoding="utf-8")
                custom_spec = DepthBacktestSpec(
                    **{**custom_spec.__dict__, "depth_path": str(l2_path)}
                )
            callback = CustomDepthStrategy()
            custom_outcome = run_depth_backtest_with_strategy(
                custom_spec,
                callback,
                strategy_id=f"python-custom-{tier}",
                strategy_version="python-depth-v1",
                source_sha256="d" * 64,
            )
            self.assertGreater(custom_outcome.fills, 0)
            self.assertIn(expected_event, callback.events)
            self.assertTrue(verify_depth_run(custom_outcome).verified)
            if tier == "l1":
                handle_spec = DepthBacktestSpec(
                    **{
                        **custom_spec.__dict__,
                        "run_id": "python-custom-depth-l1-handle",
                        "output_dir": str(self.output_dir / "custom-depth-l1-handle"),
                    }
                )
                handle = start_depth_backtest_with_strategy(
                    handle_spec,
                    CustomDepthStrategy(),
                    strategy_id="python-custom-l1",
                    strategy_version="python-depth-v1",
                    source_sha256="e" * 64,
                )
                self.assertEqual(handle.wait(timeout_ms=30_000), "succeeded")
                self.assertGreater(handle.result().fills, 0)

            portfolio_spec = DepthBacktestSpec(
                **{
                    **custom_spec.__dict__,
                    "run_id": f"python-custom-depth-portfolio-{tier}",
                    "output_dir": str(self.output_dir / f"custom-depth-portfolio-{tier}"),
                    "portfolio": PortfolioBacktestConfig(
                        primary_priority=0,
                        primary_reserved_capital_raw=500_000_000_000_000,
                        additional_strategies=(
                            PortfolioStrategySpec(
                                priority=1,
                                reserved_capital_raw=500_000_000_000_000,
                                strategy=BuiltinStrategySpec(
                                    kind="momentum", strategy_id=f"python-custom-child-{tier}"
                                ),
                            ),
                        ),
                    ),
                }
            )
            primary_callback = CustomDepthStrategy()
            child_callback = CustomDepthStrategy()
            callback_slots = [
                {
                    "strategy_id": f"python-custom-{tier}",
                    "strategy_version": "python-depth-v2",
                    "source_sha256": "f" * 64,
                    "strategy": primary_callback,
                },
                {
                    "strategy_id": f"python-custom-child-{tier}",
                    "strategy_version": "python-depth-v2",
                    "source_sha256": "a" * 64,
                    "strategy": child_callback,
                },
            ]
            portfolio_outcome = run_depth_backtest_with_strategy_slots(
                portfolio_spec, callback_slots
            )
            self.assertGreaterEqual(portfolio_outcome.fills, 2)
            self.assertTrue(verify_depth_run(portfolio_outcome).verified)
            self.assertTrue(all(event == expected_event for event in primary_callback.events))
            self.assertTrue(all(event == expected_event for event in child_callback.events))
            portfolio_summary = json.loads(
                Path(portfolio_outcome.artifacts["summary"]).read_text(encoding="utf-8")
            )
            self.assertEqual(
                set(portfolio_summary["strategy_ids"]),
                {f"python-custom-{tier}", f"python-custom-child-{tier}"},
            )
            if tier == "l1":
                handle_spec = DepthBacktestSpec(
                    **{
                        **portfolio_spec.__dict__,
                        "run_id": "python-custom-depth-portfolio-handle",
                        "output_dir": str(self.output_dir / "custom-depth-portfolio-handle"),
                    }
                )
                handle = start_depth_backtest_with_strategy_slots(
                    handle_spec,
                    [
                        {**callback_slots[0], "strategy": CustomDepthStrategy(), "source_sha256": "b" * 64},
                        {**callback_slots[1], "strategy": CustomDepthStrategy(), "source_sha256": "c" * 64},
                    ],
                )
                self.assertEqual(handle.wait(timeout_ms=30_000), "succeeded")
                self.assertGreaterEqual(handle.result().fills, 2)

                async def verify_depth_async_facade():
                    client = AsyncCoreClient()
                    async_spec = DepthBacktestSpec(
                        **{
                            **portfolio_spec.__dict__,
                            "run_id": "python-custom-depth-portfolio-async",
                            "output_dir": str(self.output_dir / "custom-depth-portfolio-async"),
                        }
                    )
                    async_bindings = [
                        {
                            **callback_slots[0],
                            "strategy": CustomDepthStrategy(),
                            "source_sha256": "4" * 64,
                        },
                        {
                            **callback_slots[1],
                            "strategy": CustomDepthStrategy(),
                            "source_sha256": "5" * 64,
                        },
                    ]
                    async_outcome = await client.async_run_depth_backtest_with_strategy_slots(
                        async_spec, async_bindings
                    )
                    self.assertTrue(verify_depth_run(async_outcome).verified)
                    handle_spec = DepthBacktestSpec(
                        **{
                            **portfolio_spec.__dict__,
                            "run_id": "python-custom-depth-portfolio-async-handle",
                            "output_dir": str(
                                self.output_dir / "custom-depth-portfolio-async-handle"
                            ),
                        }
                    )
                    async_handle = await client.async_start_depth_backtest_with_strategy_slots(
                        handle_spec,
                        [
                            {
                                **async_bindings[0],
                                "strategy": CustomDepthStrategy(),
                                "source_sha256": "6" * 64,
                            },
                            {
                                **async_bindings[1],
                                "strategy": CustomDepthStrategy(),
                                "source_sha256": "7" * 64,
                            },
                        ],
                    )
                    self.assertEqual(await async_handle.async_wait(timeout=30), "succeeded")
                    self.assertTrue(verify_depth_run(await async_handle.async_result()).verified)

                asyncio.run(verify_depth_async_facade())
        typed_handle = start_depth_experiment(
            RunDepthExperimentSpec(
                experiment_id="python-typed-tick-grid-handle",
                base=DepthBacktestSpec(
                    **{
                        **typed_depth_experiment.base.__dict__,
                        "run_id": "unused-python-typed-tick-grid-handle",
                        "output_dir": str(self.output_dir / "typed-tick-grid-handle"),
                    }
                ),
                parameter_space=typed_depth_experiment.parameter_space,
            )
        )
        self.assertEqual(typed_handle.wait(10_000), "succeeded")
        self.assertIsNotNone(typed_handle.result())
        self.assertIsNone(typed_handle.result())

        from qianxing.cli import main as python_cli

        outcome_file = self.output_dir / "depth-outcome.json"
        outcome_file.write_text(json.dumps(typed.__dict__), encoding="utf-8")
        from contextlib import redirect_stdout
        from io import StringIO

        stdout = StringIO()
        with redirect_stdout(stdout):
            self.assertEqual(python_cli(["verify-depth", str(outcome_file)]), 0)
        self.assertTrue(json.loads(stdout.getvalue())["verified"])

    def test_python_depth_parameter_experiment_uses_fresh_strategies_in_rust(self):
        self._require()
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            AsyncCoreClient,
            BuiltinStrategySpec,
            DepthBacktestSpec,
            ExperimentParameterSpace,
            RunDepthExperimentSpec,
            run_depth_experiment_with_strategy,
            start_depth_experiment_with_strategy,
        )

        scale = 1_000_000_000
        snapshots = []
        for index in range(48):
            phase = index % 20
            mid = 100 + (phase * 2 if phase < 10 else (20 - phase) * 2)
            snapshots.append({
                "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                "ts": 1000 + index * 1000,
                "sequence": index + 1,
                "bids": [{"price": (mid - 1) * scale, "qty": 100 * scale}],
                "asks": [{"price": (mid + 1) * scale, "qty": 100 * scale}],
            })
        depth_path = self.output_dir / "custom-depth-grid.json"
        depth_path.write_text(json.dumps({
            "schema_version": 1,
            "source": "python-depth-grid-factory",
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "snapshots": snapshots,
        }), encoding="utf-8")

        def experiment(experiment_id):
            return RunDepthExperimentSpec(
                experiment_id=experiment_id,
                base=DepthBacktestSpec(
                    run_id="unused-depth-grid",
                    depth_path=str(depth_path),
                    tier="l1",
                    settlement_currency="USDT",
                    initial_cash_raw=1_000_000_000_000_000,
                    output_dir=str(self.output_dir / experiment_id),
                    strategy=BuiltinStrategySpec(
                        kind="sma_cross", strategy_id="python-depth-grid",
                        quantity_raw=scale, fast_window=2, slow_window=3,
                    ),
                ),
                parameter_space=(ExperimentParameterSpace("quantity_raw", (scale, 2 * scale)),),
            )

        produced = []

        def factory(candidate):
            quantity = candidate["strategy"]["quantity_raw"]
            produced.append(quantity)

            class BuyFirstTick:
                def __init__(self):
                    self.sent = False

                def on_event(self, context, event):
                    _, tick = next(iter(event.items()))
                    intents = []
                    if not self.sent:
                        self.sent = True
                        intents.append({
                            "intent_id": 1,
                            "instrument": tick["instrument"],
                            "side": "Buy",
                            "qty": quantity,
                            "limit": None,
                            "policy": None,
                            "reduce_only": False,
                            "post_only": False,
                        })
                    return {
                        "schema_version": 1,
                        "request_id": f"{context['strategy_id']}:{tick['ts']}",
                        "strategy_id": context["strategy_id"],
                        "signal_id": tick["ts"],
                        "confidence": 10_000,
                        "priority": 0,
                        "expires_at": 0,
                        "intents": intents,
                    }

            return BuyFirstTick()

        identity = {
            "strategy_id": "python-depth-grid",
            "strategy_version": "depth-grid-v1",
            "source_sha256": "f" * 64,
        }
        result = run_depth_experiment_with_strategy(experiment("python-depth-grid"), factory, **identity)
        self.assertEqual(produced, [scale, 2 * scale])
        self.assertEqual(result.completed_candidates, 2)
        self.assertEqual(result.succeeded_candidates, 2)
        self.assertTrue(all(candidate.outcome.fills > 0 for candidate in result.candidates))

        handle = start_depth_experiment_with_strategy(
            experiment("python-depth-grid-handle"), factory, **identity
        )
        self.assertEqual(handle.wait(timeout_ms=30_000), "succeeded")
        self.assertEqual(handle.result().succeeded_candidates, 2)

        async_client = AsyncCoreClient()
        async_result = asyncio.run(async_client.async_run_depth_experiment_with_strategy(
            experiment("python-depth-grid-async"), factory, **identity
        ))
        self.assertEqual(async_result.succeeded_candidates, 2)
        async_handle = asyncio.run(async_client.async_start_depth_experiment_with_strategy(
            experiment("python-depth-grid-async-handle"), factory, **identity
        ))
        self.assertEqual(asyncio.run(async_handle.async_wait(30)), "succeeded")
        self.assertEqual(asyncio.run(async_handle.async_result()).succeeded_candidates, 2)
        fills = sum(candidate.outcome.fills for candidate in result.candidates)
        self.assertGreater(fills, 0)
        print(
            f"custom_depth_grid=candidates={result.completed_candidates} "
            f"succeeded={result.succeeded_candidates} fills={fills} "
            "factory_instances_per_candidate=true run_handle=true asyncio=true"
        )

    def test_multi_account_backtest_uses_the_shared_rust_run_handle(self):
        self._require()
        from qianxing_bridge.native import app_available

        self.assertTrue(app_available(), "multi-account lifecycle must pass the generation probe")
        bars = self.root / "deploy" / "qianxing.bar-frame.example.json"
        member = lambda account, run: {
            "schema_version": 1,
            "run_id": run,
            "account_id": account,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": str(bars),
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000,
            "seed": 7,
            "output_dir": str(self.output_dir / account),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": f"{account}-sma",
                "quantity_raw": 1_000_000_000,
                "fast_window": 2,
                "slow_window": 3,
            },
        }
        payload = {
            "schema_version": 1,
            "batch_id": "python-multi-account-handle",
            "runs": [member("account-a", "python-account-a"), member("account-b", "python-account-b")],
        }
        handle = self.bridge.start_multi_account_backtest(payload)
        self.assertEqual(handle.run_id, payload["batch_id"])
        self.assertEqual(handle.wait(10_000), "succeeded")
        result = json.loads(handle.result_json())
        self.assertEqual(result["summary"]["accounts_succeeded"], 2)
        self.assertGreater(result["summary"]["successful_fills"], 0)
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import read_multi_account_report_v1

        report = read_multi_account_report_v1(result)
        self.assertEqual(report["schema_version"], 1)
        self.assertTrue(report["verification"]["verified"])
        self.assertEqual(report["summary"]["accounts_succeeded"], 2)
        self.assertEqual(len(report["accounts"]), 2)
        self.assertIsNone(handle.result_json(), "batch result is delivered once")

        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            MultiAccountBacktestSpec,
            start_multi_account_backtest,
        )

        typed_run = lambda account, run: BacktestSpec(
            run_id=run,
            account_id=account,
            instrument="BTCUSDT.BINANCE",
            bars_path=str(bars),
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000_000,
            seed=7,
            output_dir=str(self.output_dir / f"typed-{account}"),
            strategy=BuiltinStrategySpec(
                kind="sma_cross",
                strategy_id=f"{account}-typed-sma",
                quantity_raw=1_000_000_000,
                fast_window=2,
                slow_window=3,
            ),
        )
        typed_spec = MultiAccountBacktestSpec(
            batch_id="python-typed-multi-account",
            runs=(typed_run("typed-a", "typed-run-a"), typed_run("typed-b", "typed-run-b")),
        )
        typed_handle = start_multi_account_backtest(typed_spec)
        self.assertEqual(typed_handle.wait(10_000), "succeeded")
        typed_result = typed_handle.result()
        self.assertEqual(typed_result.summary["accounts_succeeded"], 2)
        typed_report = read_multi_account_report_v1(typed_result)
        self.assertTrue(typed_report["verification"]["verified"])
        self.assertEqual(typed_report["summary"]["accounts_succeeded"], 2)
        self.assertIsNone(typed_handle.result(), "typed batch result is delivered once")

    def test_multi_account_depth_start_translates_native_errors(self):
        from qianxing.app import InvalidInputError, start_multi_account_depth_backtest

        payload = json.dumps({"category": "INVALID_INPUT", "message": "invalid depth batch"})
        with patch(
            "qianxing.app.native.app_start_multi_account_depth_backtest",
            side_effect=RuntimeError(payload),
        ):
            with self.assertRaises(InvalidInputError) as raised:
                start_multi_account_depth_backtest({})
        self.assertEqual(raised.exception.payload["message"], "invalid depth batch")

    def test_multi_account_depth_sync_translates_native_errors(self):
        from qianxing.app import InvalidInputError, run_multi_account_depth_backtest

        payload = json.dumps({"category": "INVALID_INPUT", "message": "invalid depth batch"})
        with patch(
            "qianxing.app.native.app_run_multi_account_depth_backtest",
            side_effect=RuntimeError(payload),
        ):
            with self.assertRaises(InvalidInputError) as raised:
                run_multi_account_depth_backtest({})
        self.assertEqual(raised.exception.payload["message"], "invalid depth batch")

    def test_multi_account_depth_batch_uses_shared_rust_kernel_and_typed_sdk(self):
        self._require()
        from qianxing_bridge.native import app_available

        self.assertTrue(app_available(), "depth batch functions must pass the generation probe")
        depth = self.root / "deploy" / "qianxing.depth-frame.l1.example.json"
        depth_b = self.output_dir / "eth-okx-depth.json"
        depth_b.write_text(
            depth.read_text(encoding="utf-8").replace("BTCUSDT", "ETHUSDT").replace("BINANCE", "OKX"),
            encoding="utf-8",
        )
        def member(account: str, run_id: str, data_path: Path = depth) -> dict[str, Any]:
            return {
                "schema_version": 1,
                "run_id": run_id,
                "account_id": account,
                "depth_path": str(data_path),
                "tier": "l1",
                "settlement_currency": "USDT",
                "initial_cash_raw": 1_000_000_000_000_000,
                "fee_bps": 0,
                "latency_snapshots": 0,
                "queue_position_bps": 0,
                "market_impact_bps": 0,
                "output_dir": str(self.output_dir / account),
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": f"{account}-depth-sma",
                    "quantity_raw": 1_000_000_000,
                    "fast_window": 2,
                    "slow_window": 3,
                },
            }
        payload = {
            "schema_version": 1,
            "batch_id": "python-depth-multi-account",
            "runs": [member("account-b", "depth-account-b", depth_b), member("account-a", "depth-account-a")],
        }
        result = self.bridge.run_multi_account_depth_backtest(payload)
        self.assertEqual([run["account_id"] for run in result["runs"]], ["account-a", "account-b"])
        self.assertEqual(result["summary"]["accounts_succeeded"], 2)
        self.assertGreater(result["summary"]["successful_fills"], 0)
        self.assertEqual(result["runs"][0]["outcome"]["instrument"], "BTCUSDT.BINANCE")
        self.assertEqual(result["runs"][1]["outcome"]["instrument"], "ETHUSDT.OKX")
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import read_multi_account_report_v1

        depth_report = read_multi_account_report_v1(result)
        self.assertTrue(depth_report["verification"]["verified"])
        self.assertEqual(depth_report["summary"]["accounts_succeeded"], 2)
        self.assertEqual(len(depth_report["accounts"]), 2)

        handle = self.bridge.start_multi_account_depth_backtest(payload)
        self.assertEqual(handle.wait(10_000), "succeeded")
        self.assertEqual(json.loads(handle.result_json())["summary"]["accounts_succeeded"], 2)

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BuiltinStrategySpec, DepthBacktestSpec, MultiAccountDepthBacktestSpec,
            read_multi_account_report_v1, start_multi_account_depth_backtest,
        )
        typed_run = lambda account, run_id: DepthBacktestSpec(
            run_id=run_id, account_id=account, depth_path=str(depth), tier="l1",
            settlement_currency="USDT", initial_cash_raw=1_000_000_000_000_000,
            output_dir=str(self.output_dir / f"typed-{account}"),
            strategy=BuiltinStrategySpec(kind="sma_cross", strategy_id=f"{account}-typed",
                quantity_raw=1_000_000_000, fast_window=2, slow_window=3),
        )
        typed_spec = MultiAccountDepthBacktestSpec(
            batch_id="python-typed-depth-multi-account",
            runs=(typed_run("typed-a", "typed-depth-a"), typed_run("typed-b", "typed-depth-b")),
        )
        typed_handle = start_multi_account_depth_backtest(typed_spec)
        self.assertEqual(typed_handle.wait(10_000), "succeeded")
        typed_result = typed_handle.result()
        self.assertEqual(typed_result.summary["accounts_succeeded"], 2)
        typed_depth_report = read_multi_account_report_v1(typed_result)
        self.assertTrue(typed_depth_report["verification"]["verified"])
        self.assertEqual(typed_depth_report["summary"]["accounts_succeeded"], 2)

    def test_multi_leg_backtest_uses_the_shared_rust_application_flow_and_handle(self):
        self._require()
        from qianxing_bridge.native import app_available

        self.assertTrue(app_available(), "multi-leg functions must pass the generation probe")
        fixture_root = self.root / "deploy"
        payload = {
            "schema_version": 1,
            "run_id": "python-multi-leg",
            "primary_bars_path": str(fixture_root / "qianxing.bar-frame.pairs-primary.example.json"),
            "reference_bars_path": str(fixture_root / "qianxing.bar-frame.pairs-reference.example.json"),
            "settlement_currency": "USDT",
            "primary_initial_cash_raw": 1_000_000_000_000_000,
            "reference_initial_cash_raw": 1_000_000_000_000_000,
            "seed": 7,
            "output_dir": str(self.output_dir / "multi-leg"),
            "account_id": "acct.multi-python",
            "strategy": {
                "kind": "pairs_arbitrage",
                "strategy_id": "py-pairs",
                "quantity_raw": 1_000_000_000,
                "fast_window": 5,
                "slow_window": 20,
                "period": 14,
                "threshold_bps": 100,
            },
        }
        first = self.bridge.run_multi_leg_backtest(payload)
        second = self.bridge.run_multi_leg_backtest(payload)
        self.assertEqual(first, second)
        self.assertEqual(first["account_id"], "acct.multi-python")
        self.assertGreater(first["primary_fills"], 0)
        self.assertGreater(first["reference_fills"], 0)
        self.assertTrue(Path(first["attribution_artifact"]).is_file())

        facts = self.bridge.read_multi_leg_report_facts_v1(first)
        self.assertEqual(facts["schema_version"], 1)
        self.assertEqual(facts["account_id"], first["account_id"])
        self.assertEqual(
            facts["total_fills"], first["primary_fills"] + first["reference_fills"]
        )
        self.assertEqual(len(facts["event_hash"]), 16)

        handle_payload = {**payload, "run_id": "python-multi-leg-handle"}
        handle_payload["output_dir"] = str(self.output_dir / "multi-leg-handle")
        handle = self.bridge.start_multi_leg_backtest(handle_payload)
        self.assertEqual(handle.run_id, "python-multi-leg-handle")
        self.assertEqual(handle.wait(10_000), "succeeded")
        self.assertIsNotNone(handle.result_json())
        self.assertIsNone(handle.result_json(), "multi-leg result is delivered once")

        from qianxing import (
            BuiltinStrategySpec,
            ExperimentParameterSpace,
            FundingRateEvent,
            InstrumentId,
            MultiLegBacktestSpec,
            RunMultiLegExperimentSpec,
            TradingInstrumentSpec,
            run_multi_leg_experiment_with_strategy,
            run_multi_leg_backtest_with_strategy,
            start_multi_leg_experiment_with_strategy,
            start_multi_leg_backtest_with_strategy,
            run_multi_leg_backtest,
            start_multi_leg_backtest,
        )

        def derivative_spec(instrument: str, inverse: bool, contract_size: int):
            symbol, venue = instrument.split(".")
            return TradingInstrumentSpec(
                instrument=InstrumentId(symbol=symbol, venue=venue),
                product="perpetual",
                base_currency=symbol[:3],
                quote_currency=symbol[3:],
                settlement_currency="USDT",
                contract_size=contract_size,
                linear=not inverse,
                inverse=inverse,
                price_tick=1_000,
                qty_step=1,
                min_qty=1,
                max_leverage=20,
                maintenance_margin_bps=500,
                valid_from=0,
            )

        fixed_notional = MultiLegBacktestSpec(
            run_id="python-fixed-notional-pair",
            primary_bars_path=str(fixture_root / "qianxing.bar-frame.pairs-primary.example.json"),
            reference_bars_path=str(fixture_root / "qianxing.bar-frame.pairs-reference.example.json"),
            settlement_currency="USDT",
            primary_initial_cash_raw=10_000 * 1_000_000_000,
            reference_initial_cash_raw=10_000 * 1_000_000_000,
            output_dir=str(self.output_dir / "fixed-notional-pair"),
            seed=7,
            strategy=BuiltinStrategySpec(
                kind="pairs_arbitrage",
                strategy_id="python-fixed-notional-pair",
                target_notional_raw=100 * 1_000_000_000,
            ),
            primary_market_spec=derivative_spec(
                "BTCUSDT.BINANCE", False, 1_000_000_000
            ),
            reference_market_spec=derivative_spec(
                "ETHUSDT.BINANCE", False, 1_000_000_000
            ),
            funding_bps=-100,
        )
        sized = run_multi_leg_backtest(fixed_notional)
        self.assertGreater(sized.primary_fills, 0)
        self.assertGreater(sized.reference_fills, 0)
        sized_artifact = json.loads(Path(sized.attribution_artifact).read_text(encoding="utf-8"))
        self.assertTrue(sized_artifact["execution_funding_events"])
        self.assertTrue(any(
            event.get("kind", {}).get("LedgerApplied", {}).get("entry", {}).get("kind") == "Funding"
            for event in sized_artifact["shared_event_log"]
        ))

        class CustomPairStrategy:
            def __init__(self, quantity=1_000_000_000):
                self.seen = set()
                self.events = []
                self.quantity = quantity

            def on_event(self, context, event):
                name, item = next(iter(event.items()))
                self.events.append(name)
                instrument = item["instrument"]
                key = f"{instrument['symbol']}.{instrument['venue']}"
                intents = []
                if key not in self.seen:
                    self.seen.add(key)
                    intents.append({
                        "intent_id": len(self.seen),
                        "instrument": instrument,
                        "side": "Buy",
                        "qty": self.quantity,
                        "limit": None,
                        "policy": None,
                        "reduce_only": False,
                        "post_only": False,
                    })
                return {
                    "schema_version": 1,
                    "request_id": f"{context['strategy_id']}:{item['ts']}",
                    "strategy_id": context["strategy_id"],
                    "signal_id": item["ts"],
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": intents,
                }

        callback_spec = MultiLegBacktestSpec(
            run_id="python-custom-pair",
            primary_bars_path=payload["primary_bars_path"],
            reference_bars_path=payload["reference_bars_path"],
            settlement_currency="USDT",
            primary_initial_cash_raw=1_000_000_000_000_000,
            reference_initial_cash_raw=1_000_000_000_000_000,
            output_dir=str(self.output_dir / "custom-pair"),
            seed=7,
            strategy=BuiltinStrategySpec(kind="pairs_arbitrage", strategy_id="python-custom-pair"),
            primary_market_spec=derivative_spec("BTCUSDT.BINANCE", False, 1_000_000_000),
            reference_market_spec=derivative_spec("ETHUSDT.BINANCE", False, 1_000_000_000),
            funding_events=(
                FundingRateEvent(
                    instrument=InstrumentId(symbol="BTCUSDT", venue="BINANCE"),
                    settlement_ts=8 * 60 * 60 * 1_000,
                    rate_bps=100,
                ),
                FundingRateEvent(
                    instrument=InstrumentId(symbol="BTCUSDT", venue="BINANCE"),
                    settlement_ts=16 * 60 * 60 * 1_000,
                    rate_bps=-50,
                ),
            ),
        )
        callback = CustomPairStrategy()
        callback_outcome = run_multi_leg_backtest_with_strategy(
            callback_spec,
            callback,
            strategy_id="python-custom-pair",
            strategy_version="python-pair-v1",
            source_sha256="e" * 64,
        )
        self.assertGreater(callback_outcome.primary_fills, 0)
        self.assertGreater(callback_outcome.reference_fills, 0)
        callback_artifact = json.loads(
            Path(callback_outcome.attribution_artifact).read_text(encoding="utf-8")
        )
        self.assertTrue(
            {8 * 60 * 60 * 1_000, 16 * 60 * 60 * 1_000}.issubset(
                {event["ts"] for event in callback_artifact["execution_funding_events"]}
            )
        )
        self.assertEqual(
            callback_artifact["funding_rate_input"], "caller_supplied_historical_events"
        )
        self.assertEqual(
            callback_artifact["funding_rate_events"],
            [
                {
                    "instrument": "BTCUSDT.BINANCE",
                    "settlement_ts": 8 * 60 * 60 * 1_000,
                    "rate_bps": 100,
                },
                {
                    "instrument": "BTCUSDT.BINANCE",
                    "settlement_ts": 16 * 60 * 60 * 1_000,
                    "rate_bps": -50,
                },
            ],
        )
        expected_callbacks = sum(
            len(json.loads(Path(path).read_text(encoding="utf-8"))["ts"]) - 1
            for path in (payload["primary_bars_path"], payload["reference_bars_path"])
        )
        self.assertEqual(
            callback.events.count("Bar"),
            expected_callbacks,
            "each market's first Bar is warmup-only and is not sent to the strategy",
        )
        callback_handle_spec = MultiLegBacktestSpec(
            **{
                **callback_spec.__dict__,
                "run_id": "python-custom-pair-handle",
                "output_dir": str(self.output_dir / "custom-pair-handle"),
            }
        )
        callback_handle = start_multi_leg_backtest_with_strategy(
            callback_handle_spec,
            CustomPairStrategy(),
            strategy_id="python-custom-pair",
            strategy_version="python-pair-v1",
            source_sha256="e" * 64,
        )
        self.assertEqual(callback_handle.wait(10_000), "succeeded")
        self.assertGreater(callback_handle.result().primary_fills, 0)

        experiment_spec = RunMultiLegExperimentSpec(
            experiment_id="python-custom-pair-grid",
            base=MultiLegBacktestSpec(
                **{
                    **callback_spec.__dict__,
                    "run_id": "python-custom-pair-grid-base",
                    "output_dir": str(self.output_dir / "custom-pair-grid"),
                }
            ),
            parameter_space=(ExperimentParameterSpace(
                name="quantity_raw", values=(500_000_000, 1_000_000_000),
            ),),
        )
        factory_quantities = []

        def strategy_factory(candidate):
            quantity = candidate["strategy"]["quantity_raw"]
            factory_quantities.append(quantity)
            return CustomPairStrategy(quantity)

        experiment = run_multi_leg_experiment_with_strategy(
            experiment_spec,
            strategy_factory,
            strategy_id="python-custom-pair",
            strategy_version="python-pair-v1",
            source_sha256="e" * 64,
        )
        self.assertEqual(factory_quantities, [500_000_000, 1_000_000_000])
        self.assertEqual(experiment.completed_candidates, 2)
        self.assertEqual(experiment.succeeded_candidates, 2)
        self.assertTrue(Path(experiment.artifact_locations["experiment_input"]).is_file())
        self.assertTrue(all(
            candidate.outcome.primary_fills > 0 and candidate.outcome.reference_fills > 0
            for candidate in experiment.candidates
        ))
        experiment_handle_spec = RunMultiLegExperimentSpec(
            **{
                **experiment_spec.__dict__,
                "experiment_id": "python-custom-pair-grid-handle",
                "base": MultiLegBacktestSpec(
                    **{
                        **experiment_spec.base.__dict__,
                        "output_dir": str(self.output_dir / "custom-pair-grid-handle"),
                    }
                ),
            }
        )
        experiment_handle = start_multi_leg_experiment_with_strategy(
            experiment_handle_spec,
            lambda candidate: CustomPairStrategy(candidate["strategy"]["quantity_raw"]),
            strategy_id="python-custom-pair",
            strategy_version="python-pair-v1",
            source_sha256="e" * 64,
        )
        self.assertEqual(experiment_handle.wait(10_000), "succeeded")
        self.assertEqual(experiment_handle.result().succeeded_candidates, 2)

        typed_spec = MultiLegBacktestSpec(
            run_id="python-typed-multi-leg",
            primary_bars_path=payload["primary_bars_path"],
            reference_bars_path=payload["reference_bars_path"],
            settlement_currency="USDT",
            primary_initial_cash_raw=1_000_000_000_000_000,
            reference_initial_cash_raw=1_000_000_000_000_000,
            output_dir=str(self.output_dir / "typed-multi-leg"),
            account_id="acct.typed-pairs",
            seed=7,
            strategy=BuiltinStrategySpec(
                kind="pairs_arbitrage",
                strategy_id="py-typed-pairs",
                quantity_raw=1_000_000_000,
                fast_window=5,
                slow_window=20,
                period=14,
                threshold_bps=100,
            ),
            primary_market_spec=TradingInstrumentSpec(
                instrument=InstrumentId(symbol="BTCUSDT", venue="BINANCE"),
                product="spot",
                base_currency="BTC",
                quote_currency="USDT",
                settlement_currency="USDT",
                contract_size=1_000_000_000,
                linear=True,
                inverse=False,
                price_tick=1_000,
                qty_step=1,
                min_qty=1,
                max_leverage=1,
                maintenance_margin_bps=0,
                valid_from=0,
            ),
            reference_market_spec=TradingInstrumentSpec(
                instrument=InstrumentId(symbol="ETHUSDT", venue="BINANCE"),
                product="spot",
                base_currency="ETH",
                quote_currency="USDT",
                settlement_currency="USDT",
                contract_size=1_000_000_000,
                linear=True,
                inverse=False,
                price_tick=1_000,
                qty_step=1,
                min_qty=1,
                max_leverage=1,
                maintenance_margin_bps=0,
                valid_from=0,
            ),
            fill_model="one_tick_slippage",
        )
        typed_outcome = run_multi_leg_backtest(typed_spec)
        self.assertEqual(typed_outcome.account_id, "acct.typed-pairs")
        self.assertGreater(typed_outcome.primary_fills, 0)
        typed_handle = start_multi_leg_backtest(
            MultiLegBacktestSpec(
                **{
                    **typed_spec.__dict__,
                    "run_id": "python-typed-multi-leg-handle",
                    "output_dir": str(self.output_dir / "typed-multi-leg-handle"),
                }
            )
        )
        self.assertEqual(typed_handle.wait(10_000), "succeeded")
        self.assertIsNotNone(typed_handle.result())
        self.assertIsNone(typed_handle.result(), "typed multi-leg result is delivered once")

        experiment_payload = {
            "schema_version": 1,
            "experiment_id": "python-multi-leg-grid",
            "base": {**payload, "run_id": "unused-multi-leg-grid"},
            "parameter_space": [{"name": "quantity_raw", "values": [500_000_000, 1_000_000_000]}],
        }
        experiment = self.bridge.run_multi_leg_experiment(experiment_payload)
        self.assertEqual(experiment["completed_candidates"], 2)
        self.assertEqual(experiment["failed_candidates"], 0)
        self.assertEqual(experiment["succeeded_candidates"], 2)
        self.assertTrue(Path(experiment["artifact_path"]).is_file())
        self.assertTrue(Path(experiment["artifact_locations"]["experiment_input"]).is_file())
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "run-multi-leg-experiment-result-v1.json").read_text(encoding="utf-8"))
        multi_leg_result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "multi-leg-backtest-result-v1.json").read_text(encoding="utf-8"))
        schema_registry = Registry().with_resource(
            "https://qianxing.local/schemas/multi-leg-backtest-result-v1.json",
            Resource.from_contents(multi_leg_result_schema),
        )
        with Path(experiment["artifact_path"]).open(encoding="utf-8") as report_file:
            Draft202012Validator(result_schema, registry=schema_registry).validate(json.load(report_file))
        experiment_handle = self.bridge.start_multi_leg_experiment(
            {
                **experiment_payload,
                "experiment_id": "python-multi-leg-grid-handle",
                "base": {
                    **experiment_payload["base"],
                    "output_dir": str(self.output_dir / "multi-leg-grid-handle"),
                },
            }
        )
        self.assertEqual(experiment_handle.wait(10_000), "succeeded")
        self.assertIsNotNone(experiment_handle.result_json())
        self.assertIsNone(experiment_handle.result_json())

        from qianxing import (
            ExperimentParameterSpace,
            RunMultiLegExperimentSpec,
            run_multi_leg_experiment,
            start_multi_leg_experiment,
        )

        typed_experiment_spec = RunMultiLegExperimentSpec(
            experiment_id="python-typed-multi-leg-grid",
            base=typed_spec,
            parameter_space=(ExperimentParameterSpace("quantity_raw", (1_000_000_000,)),),
        )
        typed_experiment = run_multi_leg_experiment(typed_experiment_spec)
        self.assertEqual(typed_experiment.succeeded_candidates, 1)
        multi_leg_kinds = (
            "pairs_arbitrage", "basis_arbitrage", "cross_venue_arbitrage", "spot_futures_arbitrage",
        )
        typed_selection_spec = RunMultiLegExperimentSpec(
            experiment_id="python-typed-multi-leg-selection",
            base=MultiLegBacktestSpec(
                **{
                    **typed_spec.__dict__,
                    "run_id": "unused-typed-multi-leg-selection",
                    "output_dir": str(self.output_dir / "typed-multi-leg-selection"),
                }
            ),
            parameter_space=(),
            strategy_kinds=multi_leg_kinds,
        )
        self.assertEqual(typed_selection_spec.to_dict()["strategy_kinds"], sorted(multi_leg_kinds))
        typed_selection = run_multi_leg_experiment(typed_selection_spec)
        self.assertEqual(typed_selection.total_candidates, 4)
        self.assertEqual(typed_selection.succeeded_candidates, 4)
        self.assertEqual(
            [candidate.strategy_kind for candidate in typed_selection.candidates],
            sorted(multi_leg_kinds),
        )
        typed_experiment_handle = start_multi_leg_experiment(
            RunMultiLegExperimentSpec(
                experiment_id="python-typed-multi-leg-grid-handle",
                base=MultiLegBacktestSpec(
                    **{
                        **typed_spec.__dict__,
                        "run_id": "unused-typed-multi-leg-grid-handle",
                        "output_dir": str(self.output_dir / "typed-multi-leg-grid-handle"),
                    }
                ),
                parameter_space=typed_experiment_spec.parameter_space,
            )
        )
        self.assertEqual(typed_experiment_handle.wait(10_000), "succeeded")
        self.assertIsNotNone(typed_experiment_handle.result())
        self.assertIsNone(typed_experiment_handle.result())

    def test_multi_market_sdk_runs_one_shared_rust_ledger_and_run_handle(self):
        self._require()
        from qianxing_bridge.native import app_available

        self.assertTrue(app_available(), "N-market functions must pass generation probe")
        markets = []
        for index, instrument in enumerate(
            ("BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT")
        ):
            bars_path = self.output_dir / f"multi-market-{index}.json"
            closes = [
                (100 + ((bar % 20) if bar % 20 < 10 else (20 - bar % 20)) * 2)
                * 1_000_000_000
                for bar in range(80)
            ]
            bars_path.write_text(
                json.dumps({
                    "schema_version": 1,
                    "instrument": instrument,
                    "source": "python-multi-market-test-v1",
                    "ts": [1_000 + bar * 1_000 for bar in range(80)],
                    "open_raw": closes,
                    "high_raw": [value + 1_000_000_000 for value in closes],
                    "low_raw": [value - 1_000_000_000 for value in closes],
                    "close_raw": closes,
                    "volume_raw": [1_000_000_000] * 80,
                }),
                encoding="utf-8",
            )
            markets.append({
                "instrument": instrument,
                "bars_path": str(bars_path),
                "priority": index,
                "strategies": [{
                    "priority": 0,
                    "reserved_capital_raw": 20_000_000_000_000,
                    "strategy": {
                        "kind": "sma_cross",
                        "strategy_id": f"py-sma-{index}",
                        "quantity_raw": 1_000_000_000,
                        "fast_window": 2,
                        "slow_window": 3,
                    },
                }],
            })
        payload = {
            "schema_version": 1,
            "run_id": "python-multi-market",
            "account_id": "acct.multi-python",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000,
            "seed": 7,
            "output_dir": str(self.output_dir / "multi-market-out"),
            "markets": markets,
        }
        result = self.bridge.run_multi_market_backtest(payload)
        from jsonschema import Draft202012Validator

        result_schema = json.loads(
            (Path(__file__).resolve().parents[2] / "schemas" / "multi-market-backtest-result-v1.json").read_text(encoding="utf-8")
        )
        Draft202012Validator(result_schema).validate(result)
        self.assertEqual(result["markets_total"], 3)
        self.assertEqual(result["strategies_total"], 3)
        self.assertGreater(result["successful_fills"], 0)
        artifact = json.loads(Path(result["artifact_path"]).read_text(encoding="utf-8"))
        self.assertTrue(artifact["replay_verified"])
        self.assertEqual(len(artifact["market_data"]), 3)

        handle = self.bridge.start_multi_market_backtest(
            {**payload, "run_id": "python-multi-market-handle",
             "output_dir": str(self.output_dir / "multi-market-handle")}
        )
        self.assertEqual(handle.wait(10_000), "succeeded")
        handle_payload = handle.result_json()
        self.assertIsNotNone(handle_payload)
        self.assertEqual(json.loads(handle_payload)["markets_total"], 3)
        Draft202012Validator(result_schema).validate(json.loads(handle_payload))
        print(
            "MULTI_MARKET_CUSTOM_OBSERVED markets=3 custom_slots=1 "
            "callback_version=python-test-v1 replay_verified=true run_handle=succeeded"
        )
        self.assertIsNone(handle.result_json(), "N-market result is delivered once")

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BuiltinStrategySpec,
            DataUnavailableError,
            InvalidInputError,
            MultiMarketBacktestMarket,
            MultiMarketBacktestSpec,
            PortfolioStrategySpec,
            run_multi_market_backtest,
        )
        from dataclasses import replace
        typed = MultiMarketBacktestSpec(
            run_id="python-multi-market-typed",
            account_id="acct.multi-python",
            settlement_currency="USDT",
            initial_cash_raw=100_000_000_000_000,
            output_dir=str(self.output_dir / "multi-market-typed"),
            markets=tuple(
                MultiMarketBacktestMarket(
                    instrument=item["instrument"],
                    bars_path=item["bars_path"],
                    priority=item["priority"],
                    strategies=(PortfolioStrategySpec(
                        priority=0,
                        reserved_capital_raw=20_000_000_000_000,
                        strategy=BuiltinStrategySpec(
                            kind="sma_cross", strategy_id=f"typed-{item['priority']}",
                            fast_window=2, slow_window=3,
                        ),
                    ),),
                ) for item in markets
            ),
        )
        invalid_manifest_market = replace(
            typed.markets[0], dataset_manifest_path=""
        )
        invalid_manifest_spec = replace(
            typed,
            run_id="python-multi-market-empty-manifest",
            output_dir=str(self.output_dir / "multi-market-empty-manifest"),
            markets=(invalid_manifest_market, *typed.markets[1:]),
        )
        with self.assertRaisesRegex(InvalidInputError, "dataset_manifest_path"):
            run_multi_market_backtest(invalid_manifest_spec)
        typed_result = run_multi_market_backtest(typed)
        self.assertEqual(typed_result.markets_total, 3)

        from qianxing import (
            ExperimentParameterSpace,
            RunMultiMarketExperimentSpec,
            run_multi_market_experiment,
            start_multi_market_experiment,
        )
        experiment_spec = RunMultiMarketExperimentSpec(
            experiment_id="python-multi-market-parameter-grid",
            target_strategy_id="typed-0",
            base=typed,
            parameter_space=(ExperimentParameterSpace("fast_window", (1, 2)),),
            strategy_kinds=("sma_cross", "ema_cross"),
        )
        experiment_result = run_multi_market_experiment(experiment_spec)
        self.assertEqual(experiment_result.total_candidates, 4)
        self.assertEqual(experiment_result.succeeded_candidates, 4)
        self.assertIsNotNone(experiment_result.canonical_run_spec_fingerprint)
        self.assertIsNotNone(experiment_result.input_data_fingerprint)
        self.assertIsNotNone(experiment_result.result_hash)
        self.assertIsNotNone(experiment_result.run_manifest)
        self.assertIsNotNone(experiment_result.run_record)
        experiment_input_path = Path(experiment_result.artifact_locations["experiment_input"])
        experiment_input = experiment_input_path.read_bytes()
        run_record = json.loads(Path(experiment_result.run_record).read_text(encoding="utf-8"))
        experiment_input_ref = next(
            ref for ref in run_record["artifact_refs"] if ref["name"] == "experiment_input"
        )
        import hashlib
        self.assertEqual(experiment_input_ref["byte_len"], len(experiment_input))
        self.assertEqual(
            experiment_input_ref["artifact_id"], hashlib.sha256(experiment_input).hexdigest()
        )
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "run-multi-market-experiment-result-v1.json").read_text(encoding="utf-8"))
        backtest_result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "multi-market-backtest-result-v1.json").read_text(encoding="utf-8"))
        registry = Registry().with_resource(
            "https://qianxing.local/schemas/multi-market-backtest-result-v1.json",
            Resource.from_contents(backtest_result_schema),
        )
        with Path(experiment_result.artifact_path).open(encoding="utf-8") as report_file:
            Draft202012Validator(result_schema, registry=registry).validate(json.load(report_file))
        self.assertTrue(all(
            candidate.outcome.markets_total == 3
            for candidate in experiment_result.candidates
            if candidate.outcome is not None
        ))
        async_handle = start_multi_market_experiment(
            {**experiment_spec.to_dict(), "experiment_id": "python-multi-market-grid-handle"}
        )
        self.assertEqual(async_handle.wait(10_000), "succeeded")
        self.assertEqual(async_handle.result().total_candidates, 4)

        class NoopStrategy:
            def __init__(self):
                self.lifecycle = []

            def on_init(self, context):
                self.lifecycle.append("init")

            def on_start(self, context):
                self.lifecycle.append("start")

            def on_event(self, context, event):
                self.lifecycle.append("event")
                event_body = next(iter(event.values()))
                timestamp = event_body["ts"]
                return {
                    "schema_version": 1,
                    "request_id": f"{context['strategy_id']}:{timestamp}",
                    "strategy_id": context["strategy_id"],
                    "signal_id": timestamp,
                    "confidence": 0,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": [],
                }

            def on_stop(self):
                self.lifecycle.append("stop")

        from qianxing_bridge.app import (
            run_multi_market_backtest_with_strategy_slots,
            start_multi_market_backtest_with_strategy_slots,
        )
        custom_payload = {
            **payload,
            "run_id": "python-multi-market-custom",
            "output_dir": str(self.output_dir / "multi-market-custom"),
        }
        custom_payload["markets"] = [dict(market) for market in markets]
        custom_payload["markets"][0]["strategies"] = [dict(markets[0]["strategies"][0])]
        custom_payload["markets"][0]["strategies"][0]["strategy"] = {
            "kind": "sma_cross", "strategy_id": "py-sma-0", "quantity_raw": 1_000_000_000,
            "fast_window": 5, "slow_window": 20,
        }
        callback_strategy = NoopStrategy()
        binding = {
            "strategy_id": "py-sma-0",
            "strategy_version": "python-test-v1",
            "source_sha256": "a" * 64,
            "strategy": callback_strategy,
        }
        custom_result = run_multi_market_backtest_with_strategy_slots(custom_payload, [binding])
        self.assertEqual(callback_strategy.lifecycle[:2], ["init", "start"])
        self.assertTrue("event" in callback_strategy.lifecycle)
        self.assertEqual(callback_strategy.lifecycle[-1], "stop")
        custom_artifact = json.loads(Path(custom_result["artifact_path"]).read_text(encoding="utf-8"))
        self.assertEqual(custom_artifact["strategy_identities"][0]["strategy_version"], "python-test-v1")
        self.assertTrue(custom_artifact["replay_verified"])
        handle = start_multi_market_backtest_with_strategy_slots(
            {**custom_payload, "run_id": "python-multi-market-custom-handle",
             "output_dir": str(self.output_dir / "multi-market-custom-handle")}, [binding]
        )
        self.assertEqual(handle.wait(10_000), "succeeded")
        self.assertEqual(json.loads(handle.result_json())["markets_total"], 3)

        custom_experiment = RunMultiMarketExperimentSpec(
            experiment_id="python-multi-market-custom-grid",
            target_strategy_id="typed-0",
            base=typed,
            parameter_space=(ExperimentParameterSpace("fast_window", (1, 2)),),
        )
        from qianxing import run_multi_market_experiment_with_strategy
        custom_grid = run_multi_market_experiment_with_strategy(
            custom_experiment,
            lambda _candidate_json: NoopStrategy(),
            strategy_id="typed-0",
            strategy_version="python-grid-v1",
            source_sha256="b" * 64,
        )
        self.assertEqual(custom_grid.succeeded_candidates, 2, custom_grid.candidates)
        custom_grid_artifact = json.loads(Path(
            custom_grid.candidates[0].outcome.artifact_path
        ).read_text(encoding="utf-8"))
        self.assertEqual(custom_grid_artifact["strategy_identities"][0]["strategy_version"], "python-grid-v1")

        from qianxing import CallbackParameterSpace
        callback_experiment = RunMultiMarketExperimentSpec(
            experiment_id="python-multi-market-callback-grid",
            target_strategy_id="typed-0",
            base=typed,
            callback_parameter_space=(CallbackParameterSpace("risk_scale", (2, 4)),),
        )
        callback_parameters = []

        def callback_factory(_candidate, parameters):
            callback_parameters.append(parameters["risk_scale"])
            return NoopStrategy()

        callback_grid = run_multi_market_experiment_with_strategy(
            callback_experiment,
            callback_factory,
            strategy_id="typed-0",
            strategy_version="python-callback-grid-v1",
            source_sha256="c" * 64,
        )
        self.assertEqual(callback_grid.succeeded_candidates, 2, callback_grid.candidates)
        self.assertEqual(callback_parameters, [2, 4])

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    def test_multi_market_depth_typed_sdk_shares_cash_across_python_strategy_slots(self):
        self._require()
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BuiltinStrategySpec,
            DataUnavailableError,
            InstrumentId,
            MultiMarketDepthBacktestMarket,
            MultiMarketDepthBacktestSpec,
            QueuePositionFifoVolumeEstimate,
            ExperimentParameterSpace,
            CallbackParameterSpace,
            RunMultiMarketDepthExperimentSpec,
            run_multi_market_depth_backtest,
            TradingInstrumentSpec,
            run_multi_market_depth_backtest_with_strategy_slots,
            start_multi_market_depth_backtest,
            start_multi_market_depth_backtest_with_strategy_slots,
            run_multi_market_depth_experiment_with_strategy,
            start_multi_market_depth_experiment_with_strategy,
        )

        scale = 1_000_000_000
        markets = []
        for market_index, (symbol, venue, base, ask) in enumerate((
            ("BTCUSDT", "BINANCE", "BTC", 80),
            ("ETHUSDT", "BYBIT", "ETH", 60),
        )):
            instrument = InstrumentId(symbol, venue)
            snapshots = []
            for snapshot_index in range(48):
                phase = snapshot_index % 20
                mid = ask + (phase * 2 if phase < 10 else (20 - phase) * 2)
                ts = 1_000 + snapshot_index * 1_000
                snapshots.append({
                    "instrument": instrument.to_dict(),
                    "ts": ts,
                    "sequence": snapshot_index + 1,
                    "bids": [{"price": (mid - 1) * scale, "qty": 2 * scale}],
                    "asks": [{"price": (mid + 1) * scale, "qty": 2 * scale}],
                })
            depth_path = self.output_dir / f"multi-market-depth-{market_index}.json"
            depth_path.write_text(json.dumps({
                "schema_version": 1,
                "source": f"python-multi-market-depth-{market_index}",
                "instrument": instrument.to_dict(),
                "snapshots": snapshots,
            }), encoding="utf-8")
            market_spec = TradingInstrumentSpec(
                instrument=instrument,
                product="spot",
                base_currency=base,
                quote_currency="USDT",
                settlement_currency="USDT",
                contract_size=scale,
                linear=False,
                inverse=False,
                price_tick=1,
                qty_step=1,
                min_qty=1,
                max_leverage=1,
                maintenance_margin_bps=0,
                valid_from=1,
            )
            markets.append(MultiMarketDepthBacktestMarket(
                instrument=f"{symbol}.{venue}",
                depth_path=str(depth_path),
                priority=1 - market_index,
                market_spec=market_spec,
                dataset_manifest_path=None,
                data_tier="l1" if market_index == 0 else None,
                queue_position_model=(
                    QueuePositionFifoVolumeEstimate(volume_advance_bps=0)
                    if market_index == 1
                    else None
                ),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id=f"python-l2-{market_index}",
                ),
            ))
        spec = MultiMarketDepthBacktestSpec(
            run_id="python-multi-market-depth-slots",
            account_id="acct.python-multi-market-depth",
            settlement_currency="USDT",
            initial_cash_raw=100 * scale,
            output_dir=str(self.output_dir / "multi-market-depth-out"),
            markets=tuple(markets),
        )

        from dataclasses import replace
        missing_manifest_market = replace(markets[0], dataset_manifest_path="missing-depth-manifest.json")
        missing_manifest_spec = replace(
            spec,
            run_id="python-multi-market-depth-missing-manifest",
            output_dir=str(self.output_dir / "multi-market-depth-missing-manifest"),
            markets=(missing_manifest_market, markets[1]),
        )
        with self.assertRaises(DataUnavailableError):
            run_multi_market_depth_backtest(missing_manifest_spec)

        class BuyOnce:
            def __init__(self):
                self.submitted = False
                self.event_kinds = []

            def on_event(self, context, event):
                self.event_kinds.append(next(iter(event)))
                item = next(iter(event.values()))
                intents = []
                if not self.submitted:
                    self.submitted = True
                    intents.append({
                        "intent_id": 1,
                        "instrument": item["instrument"],
                        "side": "Buy",
                        "qty": scale,
                        "limit": None,
                        "policy": None,
                        "reduce_only": False,
                        "post_only": False,
                    })
                return {
                    "schema_version": 1,
                    "request_id": f"{context['strategy_id']}:{item['ts']}",
                    "strategy_id": context["strategy_id"],
                    "signal_id": item["ts"],
                    "confidence": 10_000,
                    "priority": 0,
                    "expires_at": 0,
                    "intents": intents,
                }

        bindings = [{
            "strategy_id": f"python-l2-{index}",
            "strategy_version": "python-l2-test-v1",
            "source_sha256": str(index + 1) * 64,
            "strategy": BuyOnce(),
        } for index in range(2)]
        outcome = run_multi_market_depth_backtest_with_strategy_slots(spec, bindings)
        result_schema = json.loads(
            (Path(__file__).resolve().parents[2] / "schemas" / "multi-market-depth-backtest-result-v1.json").read_text(encoding="utf-8")
        )
        from jsonschema import Draft202012Validator

        Draft202012Validator(result_schema).validate(json.loads(json.dumps(outcome.__dict__)))
        self.assertEqual(outcome.markets_total, 2)
        self.assertEqual(outcome.successful_fills, 1)
        artifact = json.loads(Path(outcome.artifact_path).read_text(encoding="utf-8"))
        self.assertTrue(artifact["replay_verified"])
        self.assertEqual(artifact["market_data"][0]["data_tier"], "l1")
        self.assertEqual(artifact["market_data"][1]["data_tier"], "l2")
        self.assertEqual(len(bindings[0]["strategy"].event_kinds), 48)
        self.assertEqual(set(bindings[0]["strategy"].event_kinds), {"Tick"})
        self.assertEqual(artifact["per_market_fills"]["ETHUSDT.BYBIT"], 1)
        self.assertEqual(artifact["per_market_fills"]["BTCUSDT.BINANCE"], 0)

        builtin_spec = MultiMarketDepthBacktestSpec(
            **{**spec.__dict__, "run_id": "python-multi-market-depth-handle",
               "output_dir": str(self.output_dir / "multi-market-depth-handle")}
        )
        handle = start_multi_market_depth_backtest(builtin_spec)
        self.assertEqual(handle.wait(10_000), "succeeded")
        builtin_outcome = handle.result()
        Draft202012Validator(result_schema).validate(json.loads(json.dumps(builtin_outcome.__dict__)))
        self.assertEqual(builtin_outcome.markets_total, 2)
        self.assertGreater(builtin_outcome.successful_fills, 0)
        self.assertIsNone(handle.result(), "multi-market depth result is delivered once")

        custom_handle_spec = MultiMarketDepthBacktestSpec(
            **{**spec.__dict__, "run_id": "python-multi-market-depth-custom-handle",
               "output_dir": str(self.output_dir / "multi-market-depth-custom-handle")}
        )
        custom_handle = start_multi_market_depth_backtest_with_strategy_slots(
            custom_handle_spec,
            [{**binding, "strategy": BuyOnce()} for binding in bindings],
        )
        self.assertEqual(custom_handle.wait(10_000), "succeeded")
        self.assertEqual(custom_handle.result().successful_fills, 1)
        experiment_spec = RunMultiMarketDepthExperimentSpec(
            experiment_id="python-multi-market-depth-experiment",
            target_strategy_id="python-l2-0",
            base=spec,
            callback_parameter_space=(
                CallbackParameterSpace("risk_scale", (1, 2)),
            ),
        )
        run_args = {
            "strategy_id": "python-l2-0",
            "strategy_version": "python-l2-experiment-v1",
            "source_sha256": "a" * 64,
        }
        experiment = run_multi_market_depth_experiment_with_strategy(
            experiment_spec, lambda _candidate, parameters: BuyOnce(), **run_args
        )
        self.assertEqual(experiment.completed_candidates, 2)
        self.assertEqual(experiment.succeeded_candidates, 2, repr(experiment.candidates))
        self.assertTrue(Path(experiment.artifact_locations["experiment_input"]).is_file())
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "run-multi-market-depth-experiment-result-v1.json").read_text(encoding="utf-8"))
        depth_result_schema = json.loads((Path(__file__).parents[2] / "schemas" / "multi-market-depth-backtest-result-v1.json").read_text(encoding="utf-8"))
        registry = Registry().with_resource(
            "https://qianxing.local/schemas/multi-market-depth-backtest-result-v1.json",
            Resource.from_contents(depth_result_schema),
        )
        with Path(experiment.artifact_path).open(encoding="utf-8") as report_file:
            Draft202012Validator(result_schema, registry=registry).validate(json.load(report_file))
        experiment_handle = start_multi_market_depth_experiment_with_strategy(
            RunMultiMarketDepthExperimentSpec(
                **{**experiment_spec.__dict__,
                   "experiment_id": "python-multi-market-depth-experiment-handle"}
            ),
            lambda _candidate, parameters: BuyOnce(),
            **run_args,
        )
        self.assertEqual(experiment_handle.wait(10_000), "succeeded")
        self.assertEqual(experiment_handle.result().succeeded_candidates, 2)
        print(
            "MULTI_MARKET_DEPTH_PYTHON_OBSERVED markets=2 fills=1 "
            "callback_slots=2 replay_verified=true custom_run_handle=succeeded "
            "parameter_candidates=2 strategy_factory=true experiment_run_handle=succeeded "
            "builtin_run_handle=succeeded builtin_fills=true"
        )

    def test_a_failure_raises_the_same_document_the_other_entrypoints_print(self):
        self._require()
        spec = self._spec("py-missing", self.output_dir, "nope.json")
        with self.assertRaises(Exception) as caught:
            self.bridge.run_backtest(spec)
        payload = self.bridge.app_error_payload(caught.exception)
        self.assertIsNotNone(payload, f"异常文本必须是 AppError 文档：{caught.exception}")
        self.assertEqual(payload["category"], "DATA_UNAVAILABLE")
        self.assertEqual(payload["source_code"], "io:NotFound")
        self.assertEqual(payload["correlation_id"], "py-missing")
        self.assertFalse(payload["safe_to_retry"])

    def test_a_short_dataset_is_a_verdict_not_an_exception(self):
        self._require()
        frame = {
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "py-test",
            "ts": [1000],
            "open_raw": [100_000_000_000],
            "high_raw": [101_000_000_000],
            "low_raw": [99_000_000_000],
            "close_raw": [100_500_000_000],
            "volume_raw": [1_000_000_000],
        }
        path = self.root / "target" / "py-app-test" / "short-frame.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(frame), encoding="utf-8")
        verdict = self.bridge.validate_dataset(
            {"schema_version": 1, "dataset_id": "py-short", "bars_path": str(path)}
        )
        self.assertFalse(verdict["usable"])
        self.assertTrue(verdict["gaps"], "判了不可用就必须给出理由")


if __name__ == "__main__":
    unittest.main()
