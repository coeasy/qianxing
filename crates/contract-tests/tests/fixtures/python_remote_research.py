"""Exercise Python's remote SDK against the running Rust API test service."""
import json
import sys

from qianxing import AppClient, BacktestOutcome, OutcomeUnknownError, PaperClient, RemoteRunHandle

client = AppClient(sys.argv[1], timeout=5)
spec = json.loads(sys.argv[2])
spec["run_id"] = "python-remote-run"
handle = client.start_backtest(spec)
assert handle.run_id == "python-remote-run"
assert handle.wait(timeout=10, poll_interval=0.01) == "succeeded"
events = handle.events()
assert len(events) >= 2
assert all(event["run_id"] == handle.run_id for event in events)
assert events[0]["kind"] == "started"
assert events[-1]["kind"] == "succeeded"
assert handle.events(events[-1]["seq"]) == []
outcome = handle.result()
assert outcome.run_id == handle.run_id
verified = client.verify_run(outcome)
assert verified.verified and verified.result_hash == outcome.result_hash

spec["run_id"] = "python-lost-response"
try:
    AppClient(sys.argv[3], timeout=5).start_backtest(spec)
    raise AssertionError("dropped response must not be treated as success")
except OutcomeUnknownError as error:
    assert error.recovery_id == "python-lost-response"
    assert error.retry is False
recovered = RemoteRunHandle(client, "python-lost-response", BacktestOutcome)
assert recovered.wait(timeout=10, poll_interval=0.01) == "succeeded"
recovered_outcome = recovered.result()
assert recovered_outcome.run_id == "python-lost-response"
assert client.verify_run(recovered_outcome).verified

paper = PaperClient(sys.argv[1], timeout=5)
command = {
    "command_id": 9001,
    "request_id": "python-paper-command",
    "operator_id": "caller-supplied-operator",
    "reason": "Python SDK remote Paper contract test",
    "kind": "PauseStrategy",
    "target": "strategy-1",
    "payload": {},
    "permission": "Trading",
    "dry_run": False,
}
receipt = paper.submit(command)
assert receipt["status"] == "Accepted"
assert receipt["operator_id"] == "local-paper"
assert paper.get_command_audit("python-paper-command") == [receipt]

lost_command = dict(command, command_id=9002, request_id="python-paper-lost-response")
try:
    PaperClient(sys.argv[4], timeout=5).submit(lost_command)
    raise AssertionError("dropped Paper receipt must not be treated as success")
except OutcomeUnknownError as error:
    assert error.recovery_id == "python-paper-lost-response"
    assert error.retry is False
recovered_audit = paper.get_command_audit("python-paper-lost-response")
assert len(recovered_audit) == 1
assert recovered_audit[0]["command_id"] == 9002
assert recovered_audit[0]["status"] == "Accepted"
print("python remote run, Paper command, events, verification and lost-response recovery passed")
