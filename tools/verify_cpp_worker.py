"""Drive the C++ external strategy through the shared QXRB protocol."""

from __future__ import annotations

import io
import json
import subprocess
import struct
import sys
import tempfile
import time
from pathlib import Path

WORKSPACE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(WORKSPACE / "python"))

from qianxing_strategy.frame import REQUEST, RESPONSE, encode_frame, read_frame  # noqa: E402
from qianxing_strategy.ring import RingEmpty, SharedMemoryRing  # noqa: E402


def main() -> int:
    if len(sys.argv) not in (2, 3):
        raise SystemExit("usage: verify_cpp_worker.py <qianxing_strategy_jsonl> [shared_memory_json|shared_memory_columnar]")
    executable = Path(sys.argv[1]).resolve()
    protocol = sys.argv[2] if len(sys.argv) == 3 else "shared_memory_json"
    if protocol not in ("shared_memory_json", "shared_memory_columnar"):
        raise SystemExit(f"unsupported protocol: {protocol}")
    if not executable.exists():
        raise SystemExit(f"worker does not exist: {executable}")
    with tempfile.TemporaryDirectory(prefix="qianxing-cpp-ring-") as directory:
        root = Path(directory)
        input_path = root / "input.ring"
        output_path = root / "output.ring"
        capacity = 8
        slot_bytes = 4096
        with SharedMemoryRing.create(input_path, capacity, slot_bytes) as input_ring, SharedMemoryRing.create(
            output_path, capacity, slot_bytes
        ) as output_ring:
            process = subprocess.Popen(
                [
                    str(executable),
                    "--protocol",
                    protocol,
                    "--input-ring",
                    str(input_path),
                    "--output-ring",
                    str(output_path),
                    "--ring-capacity",
                    str(capacity),
                    "--ring-slot-bytes",
                    str(slot_bytes),
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                request = {
                    "schema_version": 1,
                    "request_id": "cpp-ring-smoke",
                    "strategy_id": "cpp-smoke",
                    "strategy_version": "v1",
                    "data_fingerprint": "smoke",
                    "as_of": 10,
                    "instrument": "BTCUSDT.BINANCE",
                    "positions": {},
                    "cash": {"USDT": 100},
                    "available_margin_raw": 100,
                    "risk_state": "verified",
                    "research_targets": {"BTCUSDT.BINANCE": 1},
                    "bars": {
                        "source": "cpp-smoke",
                        "ts": [8, 9],
                        "open_raw": [100, 101],
                        "high_raw": [102, 103],
                        "low_raw": [99, 100],
                        "close_raw": [101, 102],
                        "volume_raw": [10, 11],
                    },
                }
                if protocol == "shared_memory_columnar":
                    bars = request["bars"]
                    metadata = dict(request)
                    metadata["bars"] = None
                    metadata["__qx_bars_source"] = bars["source"]
                    metadata_bytes = json.dumps(metadata, separators=(",", ":")).encode()
                    payload = bytearray(
                        struct.pack("<4sHHII", b"QXCB", 1, 0, len(metadata_bytes), len(bars["ts"]))
                    )
                    payload.extend(metadata_bytes)
                    payload.extend(b"".join(struct.pack("<Q", value) for value in bars["ts"]))
                    for key in ("open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"):
                        payload.extend(
                            b"".join(int(value).to_bytes(16, "little", signed=True) for value in bars[key])
                        )
                    request_payload = bytes(payload)
                else:
                    request_payload = json.dumps(request, separators=(",", ":")).encode()
                encoded = encode_frame(REQUEST, 7, request_payload)
                input_ring.push_wait(encoded, time.monotonic() + 5)
                deadline = time.monotonic() + 5
                response = None
                while time.monotonic() < deadline:
                    try:
                        response = output_ring.try_pop()
                        break
                    except RingEmpty:
                        time.sleep(0.001)
                if response is None:
                    stderr = process.communicate(timeout=1)[1]
                    raise RuntimeError(f"C++ shared ring worker timed out: {stderr}")
                kind, sequence, payload = read_frame(io.BytesIO(response))
                if kind != RESPONSE or sequence != 7:
                    raise RuntimeError(f"unexpected C++ response kind={kind} sequence={sequence}")
                value = json.loads(payload)
                if value.get("ok") is not True:
                    raise RuntimeError(f"C++ strategy rejected request: {value}")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
    print("cpp shared-memory worker smoke: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
