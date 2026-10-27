"""Persistent strategy process executed inside an OpenSandbox guest."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from types import SimpleNamespace

from .worker import _close_handler, _handle_line, _initialize_handlers

_MAX_QUEUE_FILE_BYTES = 16 * 1024 * 1024


def _atomic_write(path: Path, content: bytes) -> None:
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("wb") as stream:
        stream.write(content)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def serve(module_path: str, ready_file: Path, queue_root: Path) -> None:
    parser = argparse.ArgumentParser(description="OpenSandbox strategy guest")
    args = SimpleNamespace(module=module_path, include_diagnostics=False)
    if os.environ.get("QX_STRATEGY_STDOUT_TO_STDERR") == "1":
        import contextlib

        with contextlib.redirect_stdout(sys.stderr):
            handler, _, checkpoint_handler = _initialize_handlers(args, parser)
    else:
        handler, _, checkpoint_handler = _initialize_handlers(args, parser)

    requests = queue_root / "requests"
    responses = queue_root / "responses"
    requests.mkdir(parents=True, exist_ok=True)
    responses.mkdir(parents=True, exist_ok=True)
    _atomic_write(ready_file, b"ready\n")
    handler_closed = False
    try:
        while True:
            pending = sorted(requests.glob("*.json"), key=lambda item: item.name)
            if not pending:
                time.sleep(0.02)
                continue
            request_path = pending[0]
            if request_path.stat().st_size > _MAX_QUEUE_FILE_BYTES:
                raise ValueError("OpenSandbox strategy request exceeded the size limit")
            envelope = json.loads(request_path.read_text(encoding="utf-8"))
            request_id = envelope.get("request_id")
            if not isinstance(request_id, str) or request_id != request_path.stem:
                raise ValueError(
                    "OpenSandbox strategy request id does not match its file"
                )
            if envelope.get("close") is True:
                _close_handler(handler)
                handler_closed = True
                _atomic_write(
                    responses.joinpath(request_id + ".json"),
                    b'{"ok":true,"stopped":true}',
                )
                request_path.unlink(missing_ok=True)
                return
            payload = envelope.get("payload")
            if not isinstance(payload, str):
                raise TypeError("OpenSandbox strategy request payload must be text")
            response = _handle_line(
                handler,
                payload,
                checkpoint_handler=checkpoint_handler,
            )
            encoded = response.encode("utf-8")
            if len(encoded) > _MAX_QUEUE_FILE_BYTES:
                raise ValueError(
                    "OpenSandbox strategy response exceeded the size limit"
                )
            _atomic_write(responses.joinpath(request_id + ".json"), encoded)
            request_path.unlink(missing_ok=True)
    finally:
        if not handler_closed:
            _close_handler(handler)


def main() -> int:
    parser = argparse.ArgumentParser(description="Qianxing OpenSandbox strategy guest")
    parser.add_argument(
        "--module", required=True, help="remote .py strategy entrypoint"
    )
    parser.add_argument("--ready-file", required=True)
    parser.add_argument("--queue-root", required=True)
    args = parser.parse_args()
    try:
        serve(args.module, Path(args.ready_file), Path(args.queue_root))
        return 0
    except Exception as error:  # noqa: BLE001 - guest failures cross the provider boundary.
        sys.stderr.write(f"[OpenSandbox Python Strategy] {error}\n")
        sys.stderr.flush()
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
