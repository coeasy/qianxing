"""Real local HTTP fault injection for Python remote client behavior."""
import asyncio
import json
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing import AppClient, OutcomeUnknownError, PaperClient, QianxingError, RemoteRunHandle


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.server.seen.append(("GET", self.path, dict(self.headers)))
        self._respond()

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.server.seen.append(("POST", self.path, dict(self.headers), body))
        self._respond()

    def _respond(self):
        if self.server.drop:
            self.close_connection = True
            return
        body = self.server.body
        self.send_response(self.server.status)
        if self.server.location:
            self.send_header("Location", self.server.location)
        length = len(body) if self.server.declared_length is None else self.server.declared_length
        self.send_header("Content-Length", str(length))
        if self.server.duplicate_length:
            self.send_header("Content-Length", str(length))
        self.end_headers()
        self.wfile.write(body)


class RemoteSafetyTests(unittest.TestCase):
    def server(self, *, status=200, body=b"{}", location=None, drop=False):
        server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        server.seen, server.status, server.body = [], status, body
        server.location, server.drop = location, drop
        server.declared_length, server.duplicate_length = None, False
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        def cleanup():
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)
        self.addCleanup(cleanup)
        return server, f"http://127.0.0.1:{server.server_port}"

    def test_redirect_is_not_followed_and_credentials_do_not_escape(self):
        target, target_url = self.server()
        origin, origin_url = self.server(status=302, location=target_url + "/redirected")
        client = AppClient(origin_url, headers={"Authorization": "Bearer secret"})
        with self.assertRaises(QianxingError) as caught:
            client._request_json_value("/app/capabilities", method="GET")
        self.assertEqual(caught.exception.payload["http_status"], 302)
        self.assertEqual(len(origin.seen), 1)
        self.assertEqual(target.seen, [])
        with self.assertRaises(OutcomeUnknownError) as caught:
            PaperClient(origin_url, headers={"Authorization": "Bearer secret"}).submit({"request_id": "cmd-1", "command_id": 1})
        self.assertEqual(caught.exception.recovery_id, "cmd-1")
        self.assertEqual(len(origin.seen), 2)
        self.assertEqual(target.seen, [])

    def test_lost_mutation_response_preserves_id_and_does_not_retry(self):
        server, url = self.server(drop=True)
        client = AppClient(url, timeout=2)
        with self.assertRaises(OutcomeUnknownError) as caught:
            client.start_backtest({"run_id": "run-1"})
        self.assertEqual(caught.exception.recovery_id, "run-1")
        self.assertFalse(caught.exception.retry)
        self.assertEqual(len(server.seen), 1)
        self.assertEqual(server.seen[0][0:2], ("POST", "/app/backtest/start"))
        with self.assertRaises(QianxingError) as read_error:
            RemoteRunHandle(client, "run-1").document()
        self.assertNotIsInstance(read_error.exception, OutcomeUnknownError)

    def test_event_array_is_read_over_http_and_validated(self):
        events = [{"run_id": "run-1", "seq": 2, "kind": "succeeded", "status": "succeeded"}]
        server, url = self.server(body=json.dumps(events).encode())
        handle = RemoteRunHandle(AppClient(url), "run-1")
        self.assertEqual(handle.events(1), events)
        self.assertEqual(asyncio.run(handle.async_events(1)), events)
        self.assertEqual(server.seen[0][1], "/app/runs/run-1/events?after_seq=1")
        server.body = json.dumps([{**events[0], "run_id": "other"}]).encode()
        with self.assertRaises(QianxingError):
            handle.events(1)
        server.body = json.dumps([events[0], events[0]]).encode()
        with self.assertRaises(QianxingError):
            handle.events(1)
        with self.assertRaises(ValueError):
            handle.events(2**64)

    def test_oversized_and_malformed_mutation_responses_are_unknown(self):
        server, url = self.server(body=b"x" * 65)
        client = AppClient(url)
        with patch("qianxing.client_transport.MAX_HTTP_RESPONSE_BYTES", 64):
            with self.assertRaises(OutcomeUnknownError) as caught:
                client.start_backtest({"run_id": "run-2"})
        self.assertEqual(caught.exception.recovery_id, "run-2")
        server.body = b'{"run_id":"other","status":"running"}'
        with self.assertRaises(OutcomeUnknownError):
            client.start_backtest({"run_id": "run-2"})
        server.body = b'{"run_id":"run-2","status":"running"}'
        server.declared_length = len(server.body) + 5
        with self.assertRaises(OutcomeUnknownError):
            client.start_backtest({"run_id": "run-2"})
        server.declared_length = None
        server.duplicate_length = True
        with self.assertRaises(OutcomeUnknownError):
            client.start_backtest({"run_id": "run-2"})
        self.assertEqual(len(server.seen), 4)

    def test_paper_command_audit_is_a_validated_recovery_read(self):
        rows = [{"schema_version": 1, "command_id": 9, "request_id": "paper-9", "operator_id": "local-paper",
                 "command_digest": 123, "status": "Accepted", "result_code": "accepted", "ts": 10},
                {"schema_version": 1, "command_id": 10, "request_id": "paper-10", "operator_id": "local-paper",
                 "command_digest": 456, "status": "Executed", "result_code": "executed", "ts": 11}]
        def page(records):
            entries = [
                {"sequence": sequence, "record": record}
                for sequence, record in reversed(list(enumerate(records, start=1)))
            ]
            return {
                "schema_version": 1,
                "entries": entries,
                "next_cursor": None,
                "has_more": False,
                "snapshot_through_sequence": 2,
                "oldest_available_sequence": 1,
                "retired_records": 0,
            }

        server, url = self.server(body=json.dumps(page(rows)).encode())
        client = PaperClient(url)
        encoded_id = "paper +/10"
        encoded_row = dict(rows[1], request_id=encoded_id)
        server.body = json.dumps(page([encoded_row])).encode()
        self.assertEqual(client.get_command_audit(encoded_id), [encoded_row])
        server.body = json.dumps(page([rows[0]])).encode()
        self.assertEqual(asyncio.run(client.async_get_command_audit("paper-9")), [rows[0]])
        self.assertEqual(len(server.seen), 2)
        self.assertEqual(server.seen[0][1], "/control/audit?limit=100&request_id=paper+%2B%2F10")
        self.assertEqual(server.seen[1][1], "/control/audit?limit=100&request_id=paper-9")
        server.body = b'{"not":"an audit page"}'
        with self.assertRaises(QianxingError):
            client.get_command_audit()
        server.body = json.dumps(page([dict(rows[0], command_id=True)])).encode()
        with self.assertRaises(QianxingError):
            client.get_command_audit()
        server.body = json.dumps(page([dict(rows[0], unexpected=True)])).encode()
        with self.assertRaises(QianxingError):
            client.get_command_audit()
        server.body = json.dumps(page([dict(rows[0], status=[])] )).encode()
        with self.assertRaises(QianxingError):
            client.get_command_audit()

        oversized = page([dict(rows[0], request_id="x" * (1_048_576 + 1))])
        server.body = json.dumps(oversized).encode()
        with self.assertRaises(QianxingError):
            client.get_command_audit_page(limit=2)

        server.body = json.dumps(page(rows)).encode()
        with self.assertRaises(ValueError):
            client.get_command_audit_page(cursor="bad")

    def test_cancelled_async_start_exposes_unconfirmed_cleanup_and_is_bounded(self):
        started, release = threading.Event(), threading.Event()
        client = AppClient("http://127.0.0.1:1", timeout=0.05)

        def slow_start(_spec):
            started.set()
            release.wait(timeout=2)
            raise RuntimeError("delayed failure")

        async def exercise():
            task = asyncio.create_task(client._async_start(slow_start, {"run_id": "async-run"}))
            self.assertTrue(await asyncio.to_thread(started.wait, 1))
            task.cancel()
            try:
                # Do not use wait_for(task): on timeout it sends a second
                # cancellation to the task and can replace the first
                # CancelledError (including its cleanup evidence) on slower
                # Python 3.10 CI runners. asyncio.wait bounds observation
                # without mutating the cancellation under test.
                done, _ = await asyncio.wait({task}, timeout=1)
                if not done:
                    release.set()
                    await asyncio.wait({task}, timeout=2)
                self.assertIn(task, done, "async start cleanup exceeded its one-second bound")
                with self.assertRaises(asyncio.CancelledError) as caught:
                    await task
                error = caught.exception.remote_cleanup_error
                self.assertIsInstance(error, OutcomeUnknownError)
                self.assertEqual(error.recovery_id, "async-run")
            finally:
                release.set()
            await asyncio.sleep(0.05)

        asyncio.run(exercise())


if __name__ == "__main__":
    unittest.main()
