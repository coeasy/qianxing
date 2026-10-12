from __future__ import annotations

import asyncio
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing.app import AsyncCoreClient, AsyncRunHandle, AppClient, PaperClient, RemoteRunHandle, RunEvent


class AsyncSdkTests(unittest.TestCase):
    def test_async_core_methods_delegate_to_the_native_contract(self):
        from qianxing import app

        client = AsyncCoreClient()
        spec = {"schema_version": 1, "request_id": "local-async"}
        methods = (
            ("async_validate_dataset", "validate_dataset"),
            ("async_run_backtest", "run_backtest"),
            ("async_verify_run", "verify_run"),
            ("async_compare_runs", "compare_runs"),
            ("async_run_experiment", "run_experiment"),
            ("async_run_depth_backtest", "run_depth_backtest"),
            ("async_verify_depth_run", "verify_depth_run"),
            ("async_run_depth_experiment", "run_depth_experiment"),
            ("async_run_multi_leg_backtest", "run_multi_leg_backtest"),
            ("async_run_multi_leg_experiment", "run_multi_leg_experiment"),
        )
        for async_name, sync_name in methods:
            with self.subTest(async_method=async_name), patch.object(
                app, sync_name, return_value=object()
            ) as sync_method:
                result = asyncio.run(getattr(client, async_name)(spec))
                self.assertIs(result, sync_method.return_value)
                sync_method.assert_called_once_with(spec)

    def test_async_core_start_returns_lifecycle_handle_for_same_rust_run(self):
        class FakeHandle:
            run_id = "local-run-1"
            status = "running"

            def __init__(self):
                self.cancelled = False

            def cancel(self):
                self.cancelled = True

            def wait(self, timeout_ms):
                return "succeeded"

            def result(self):
                return {"value": 11}

            def events(self, after_seq):
                return [RunEvent(2, self.run_id, "succeeded", "succeeded")] if after_seq == 1 else []

        native_handle = FakeHandle()
        client = AsyncCoreClient()
        with patch("qianxing.app.start_backtest", return_value=native_handle):
            handle = asyncio.run(client.async_start_backtest({"request_id": "local-start"}))
        self.assertIsInstance(handle, AsyncRunHandle)
        self.assertEqual(handle.run_id, "local-run-1")
        self.assertEqual(asyncio.run(handle.async_status()), "running")
        self.assertEqual(asyncio.run(handle.async_wait(timeout=0)), "succeeded")
        self.assertEqual(asyncio.run(handle.async_result()), {"value": 11})
        self.assertEqual(asyncio.run(handle.async_events(after_seq=1))[0].seq, 2)
        asyncio.run(handle.async_cancel())
        self.assertTrue(native_handle.cancelled)

    def test_every_async_core_start_maps_to_the_existing_native_start_function(self):
        from qianxing import app

        client = AsyncCoreClient()
        marker = object()
        spec = {"schema_version": 1, "request_id": "local-start-parity"}
        methods = (
            ("async_start_backtest", "start_backtest"),
            ("async_start_experiment", "start_experiment"),
            ("async_start_depth_backtest", "start_depth_backtest"),
            ("async_start_depth_experiment", "start_depth_experiment"),
            ("async_start_multi_leg_backtest", "start_multi_leg_backtest"),
            ("async_start_multi_leg_experiment", "start_multi_leg_experiment"),
        )
        for async_name, sync_name in methods:
            with self.subTest(async_method=async_name), patch.object(
                app, sync_name, return_value=marker
            ) as sync_method:
                result = asyncio.run(getattr(client, async_name)(spec))
                self.assertEqual(result._handle, marker)
                sync_method.assert_called_once_with(spec)

    def test_async_core_wait_rejects_unbounded_or_negative_time_values(self):
        handle = AsyncRunHandle(object())
        with self.assertRaises(ValueError):
            asyncio.run(handle.async_wait(timeout=float("inf")))

    def test_async_app_methods_delegate_all_registered_use_cases_to_sync_contract(self):
        client = AppClient("http://127.0.0.1:8080")
        marker = object()
        spec = {"schema_version": 1, "request_id": "async-parity"}
        methods = (
            ("async_validate_dataset", "validate_dataset"),
            ("async_run_backtest", "run_backtest"),
            ("async_start_backtest", "start_backtest"),
            ("async_verify_run", "verify_run"),
            ("async_compare_runs", "compare_runs"),
            ("async_run_experiment", "run_experiment"),
            ("async_start_experiment", "start_experiment"),
            ("async_run_depth_backtest", "run_depth_backtest"),
            ("async_start_depth_backtest", "start_depth_backtest"),
            ("async_verify_depth_run", "verify_depth_run"),
            ("async_run_depth_experiment", "run_depth_experiment"),
            ("async_start_depth_experiment", "start_depth_experiment"),
            ("async_run_multi_leg_backtest", "run_multi_leg_backtest"),
            ("async_start_multi_leg_backtest", "start_multi_leg_backtest"),
            ("async_run_multi_leg_experiment", "run_multi_leg_experiment"),
            ("async_start_multi_leg_experiment", "start_multi_leg_experiment"),
        )
        for async_name, sync_name in methods:
            with self.subTest(async_method=async_name), patch.object(
                client, sync_name, return_value=marker
            ) as sync_method:
                result = asyncio.run(getattr(client, async_name)(spec))
                self.assertIs(result, marker)
                sync_method.assert_called_once_with(spec)

    def test_async_live_event_reader_delegates_cursor_to_sync_client_method(self):
        client = AppClient("http://127.0.0.1:8080")
        marker = [{"seq": 12}]
        with patch.object(client, "get_live_events", return_value=marker) as get_events:
            self.assertIs(asyncio.run(client.async_get_live_events(11)), marker)
        get_events.assert_called_once_with(11)

    def test_async_paper_submit_uses_the_same_trade_command_method(self):
        client = PaperClient("http://127.0.0.1:8080")
        command = {"request_id": "paper-async-1"}
        receipt = {"status": "Accepted"}
        with patch.object(client, "submit", return_value=receipt) as submit:
            self.assertEqual(asyncio.run(client.async_submit(command)), receipt)
        submit.assert_called_once_with(command)

    def test_async_run_handle_wait_cancel_and_result_share_remote_documents(self):
        client = AppClient("http://127.0.0.1:8080")
        handle = RemoteRunHandle(client, "run-async-1")
        with patch.object(
            client,
            "_request_json",
            side_effect=[{"status": "running"}, {"status": "succeeded"}],
        ):
            self.assertEqual(asyncio.run(handle.async_wait(timeout=1, poll_interval=0.001)), "succeeded")

        terminal = {"run_id": handle.run_id, "status": "succeeded", "result": {"value": 7}}
        with patch.object(client, "_request_json", return_value=terminal):
            self.assertEqual(asyncio.run(handle.async_result()), {"value": 7})
        with patch.object(client, "_request_json", return_value={"status": "cancelling"}) as request:
            self.assertEqual(asyncio.run(handle.async_cancel()), {"status": "cancelling"})
        self.assertEqual(request.call_args.kwargs["method"], "POST")

    def test_async_run_handle_wait_has_a_finite_positive_bound(self):
        handle = RemoteRunHandle(AppClient("http://127.0.0.1:8080"), "run-async-2")
        with self.assertRaises(ValueError):
            asyncio.run(handle.async_wait(timeout=0))


if __name__ == "__main__":
    unittest.main()
