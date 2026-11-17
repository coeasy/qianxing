import asyncio
import contextlib
import io
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing import AppClient
from qianxing import cli, strategy_project


class StrategyDeploymentClientTests(unittest.TestCase):
    def setUp(self):
        self.client = AppClient("http://127.0.0.1:18082")
        self.row = {"schema_version": 1, "worker_id": "strategy 1", "state": "running",
                    "trading_enabled": None, "blocking_reasons": ["live_readiness_not_evaluated"]}

    def test_worker_filter_is_encoded_and_operational_state_is_retained(self):
        with patch.object(self.client, "_request_json_value", return_value=[self.row]) as request:
            result = self.client.get_strategy_deployments("strategy 1")
        self.assertIsNone(result[0]["trading_enabled"])
        path = urlsplit(request.call_args.args[0])
        self.assertEqual(path.path, "/strategies/deployments")
        self.assertEqual(parse_qs(path.query), {"worker_id": ["strategy 1"]})
        self.assertEqual(request.call_args.kwargs["method"], "GET")

    def test_bad_worker_filter_does_not_make_a_request(self):
        with patch.object(self.client, "_request_json_value") as request:
            for value in ("", " ", 1, True):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    self.client.get_strategy_deployments(value)
        request.assert_not_called()

    def test_malformed_response_is_rejected(self):
        for value in ({}, [None], [{**self.row, "schema_version": True}],
                      [{**self.row, "state": []}], [{**self.row, "trading_enabled": "false"}],
                      [{**self.row, "blocking_reasons": [1]}]):
            with self.subTest(value=value), patch.object(self.client, "_request_json_value", return_value=value):
                with self.assertRaisesRegex(RuntimeError, "invalid strategy deployment status"):
                    self.client.get_strategy_deployments()

    def test_response_cannot_silently_switch_worker(self):
        with patch.object(self.client, "_request_json_value", return_value=[self.row]):
            with self.assertRaises(RuntimeError):
                self.client.get_strategy_deployments("another-worker")

    def test_async_facade_uses_the_same_read_contract(self):
        with patch.object(self.client, "_request_json_value", return_value=[self.row]):
            self.assertEqual(asyncio.run(self.client.async_get_strategy_deployments("strategy 1")), [self.row])

    def test_project_selects_manifest_worker_without_importing_strategy(self):
        with patch.object(strategy_project, "load_strategy_project", return_value=(Path.cwd(), {"project_id": "demo", "worker_id": "strategy 1"})), patch("qianxing.app.AppClient") as client:
            client.return_value.get_strategy_deployments.return_value = [self.row]
            result = strategy_project.project_deployments("demo", "http://localhost:18082")
        client.return_value.get_strategy_deployments.assert_called_once_with("strategy 1")
        self.assertEqual(result["profile"], "server")
        self.assertEqual(result["deployments"], [self.row])

    def test_project_cli_delegates_observation_query(self):
        with patch.object(cli, "project_deployments", return_value={"deployments": [self.row]}) as query, contextlib.redirect_stdout(io.StringIO()) as output:
            code = cli.main(["project", "deployments", "demo", "--api-url", "http://localhost:18082", "--worker-id", "strategy 1"])
        self.assertEqual(code, 0)
        query.assert_called_once_with("demo", "http://localhost:18082", worker_id="strategy 1", ssl_context=None)
        self.assertEqual(json.loads(output.getvalue())["deployments"], [self.row])

    def test_project_cli_rejects_key_without_certificate_before_request(self):
        with patch.object(cli, "project_deployments") as query, contextlib.redirect_stderr(io.StringIO()):
            code = cli.main(["project", "deployments", "demo", "--api-url", "https://localhost:18082", "--client-key", "key.pem"])
        self.assertEqual(code, 2)
        query.assert_not_called()


if __name__ == "__main__":
    unittest.main()
