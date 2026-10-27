"""JSONL runner for user-defined Python strategies.

The configured module should expose ``on_event(request)`` and return either a
``StrategyOutput`` or a mapping accepted by ``StrategyOutput.from_dict``.
``on_decision(request)`` remains a legacy alias. The worker never receives
credentials and never owns a CCXT client.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib
import importlib.util
import inspect
import io
import json
import os
import sys
import time
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

from . import StrategyInput, StrategyOutput
from .columnar import decode_request as decode_columnar_request
from .errors import StrategyErrorV1
from .frame import REQUEST, RESPONSE, encode_frame, read_frame
from .ring import RingEmpty, SharedMemoryRing


def _load_strategy_module(reference: str):
    path = Path(reference)
    if path.exists() and path.suffix == ".py":
        path = path.resolve()
        parent = str(path.parent)
        # File strategies often keep indicators/helpers beside the entry file.
        # A dedicated worker process owns this import path for its full lifetime.
        if parent not in sys.path:
            sys.path.insert(0, parent)
        spec = importlib.util.spec_from_file_location("qianxing_user_strategy", path)
        if spec is None or spec.loader is None:
            raise ValueError(f"cannot load strategy module: {reference}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    else:
        module = importlib.import_module(reference)
    expected = os.environ.get("QX_STRATEGY_ARTIFACT_SHA256", "").strip()
    if expected:
        if len(expected) != 64 or any(char not in "0123456789abcdefABCDEF" for char in expected):
            raise ValueError("QX_STRATEGY_ARTIFACT_SHA256 must be 64 hex characters")
        module_path = getattr(module, "__file__", None)
        if not module_path:
            raise ValueError("strategy artifact fingerprint requires a file-backed module")
        artifact = Path(module_path)
        if not artifact.is_file():
            raise ValueError(f"strategy artifact is not a regular file: {artifact}")
        actual = hashlib.sha256(artifact.read_bytes()).hexdigest()
        if actual.lower() != expected.lower():
            raise ValueError(
                f"strategy artifact SHA-256 mismatch: expected={expected} actual={actual}"
            )
    return module


def _load_handler(reference: str) -> Callable[[StrategyInput], Any]:
    return _handler_from_module(_load_strategy_module(reference))


def _notify_module_setup_error(
    module: Any,
    strategy_spec: dict[str, Any],
    manifest: dict[str, Any] | None,
    error: Exception,
) -> None:
    """Use a module-level v1 observer when construction fails before an instance exists."""
    observer = getattr(module, "on_error_v1", None)
    if not callable(observer):
        return
    strategy = strategy_spec.get("strategy")
    strategy = strategy if isinstance(strategy, dict) else {}
    manifest = manifest if isinstance(manifest, dict) else {}
    strategy_id = (
        strategy.get("strategy_id")
        or strategy_spec.get("target_strategy_id")
        or manifest.get("strategy_id")
        or getattr(module, "STRATEGY_ID", None)
        or getattr(module, "__name__", "python-strategy")
    )
    strategy_version = (
        strategy.get("strategy_version")
        or strategy_spec.get("strategy_version")
        or manifest.get("strategy_version")
        or getattr(module, "STRATEGY_VERSION", None)
        or "unknown"
    )
    try:
        error_v1 = StrategyErrorV1.from_exception(
            stage="initialization",
            strategy_id=str(strategy_id),
            strategy_version=str(strategy_version),
            timestamp=None,
            instrument=None,
            error=error,
        )
    except (TypeError, ValueError):
        return
    try:
        observer(error_v1)
    except Exception as observer_error:  # noqa: BLE001 - original startup error remains primary.
        sys.stderr.write(
            "strategy module on_error_v1 hook failed: "
            f"{type(observer_error).__name__}; original initialization error retained\n"
        )
        sys.stderr.flush()


def _handler_from_module(
    module: Any,
    strategy_spec: dict[str, Any] | None = None,
    *,
    account_id: str | None = None,
    venue_id: str | None = None,
) -> Callable[[StrategyInput], Any]:
    if strategy_spec is not None:
        from qianxing.strategy_manifest import prepare_runtime_strategy_spec

        from .base import BaseStrategy
        from .class_adapter import BaseStrategyProcessAdapter

        execution_spec = dict(strategy_spec)
        data_level = execution_spec.pop("data_level", "bar")
        if data_level not in {"bar", "tick", "orderbook", "timer"}:
            raise ValueError(
                "BaseStrategy process mode supports data_level bar, tick, orderbook, or timer"
            )
        resolved_spec, manifest = prepare_runtime_strategy_spec(module, execution_spec)
        declared_data_level = None if manifest is None else manifest.get("data_level")
        if declared_data_level is not None and declared_data_level != data_level:
            raise ValueError(
                "runtime python_strategy_spec.data_level conflicts with STRATEGY_MANIFEST.data_level"
            )

        factory = getattr(module, "create_strategy", None)
        strategy_class = getattr(module, "STRATEGY_CLASS", None)
        if callable(factory):
            strategy_factory = lambda: factory(resolved_spec)
        elif isinstance(strategy_class, type) and issubclass(strategy_class, BaseStrategy):
            strategy_factory = _strategy_class_factory(strategy_class, resolved_spec)
        else:
            raise ValueError(
                "class process mode requires callable create_strategy(spec) or "
                "STRATEGY_CLASS derived from BaseStrategy"
            )
        try:
            strategy = strategy_factory()
        except Exception as error:
            _notify_module_setup_error(module, resolved_spec, manifest, error)
            raise
        if not isinstance(strategy, BaseStrategy):
            error = TypeError("create_strategy(spec) must return a BaseStrategy instance")
            _notify_module_setup_error(module, resolved_spec, manifest, error)
            raise error
        return BaseStrategyProcessAdapter(
            strategy,
            account_id=account_id,
            venue_id=venue_id,
            strategy_factory=strategy_factory,
            manifest_warmup_bars=(manifest or {}).get("warmup_bars"),
            data_level=data_level,
        )
    handler = getattr(module, "on_event", None)
    if not callable(handler):
        handler = getattr(module, "on_decision", None)
    if not callable(handler):
        raise ValueError(  # noqa: TRY004 - this reports invalid strategy configuration.
            "strategy module must expose callable on_event(request)"
        )
    error_handler = getattr(module, "on_error", None)
    error_v1_handler = getattr(module, "on_error_v1", None)
    snapshot_state_handler = getattr(module, "snapshot_state", None)
    restore_state_handler = getattr(module, "restore_state", None)
    if (snapshot_state_handler is None) != (restore_state_handler is None):
        raise ValueError("module state checkpoint requires both snapshot_state() and restore_state(state)")
    if snapshot_state_handler is not None and (
        not callable(snapshot_state_handler) or not callable(restore_state_handler)
    ):
        raise ValueError("snapshot_state and restore_state must both be callable")
    stateless_marker = getattr(module, "STRATEGY_STATELESS", False)
    if type(stateless_marker) is not bool:
        raise ValueError("STRATEGY_STATELESS must be a boolean")
    if stateless_marker and snapshot_state_handler is not None:
        raise ValueError("STRATEGY_STATELESS cannot be combined with snapshot_state/restore_state")
    return _ModuleFunctionHandler(
        handler,
        error_handler if callable(error_handler) else None,
        error_v1_handler if callable(error_v1_handler) else None,
        snapshot_state_handler=snapshot_state_handler,
        restore_state_handler=restore_state_handler,
        stateless=stateless_marker,
    )


class _ModuleFunctionHandler:
    """Preserve the function worker contract while exposing module error hooks."""

    def __init__(
        self,
        handler: Callable[[StrategyInput], Any],
        error_handler: Callable[[StrategyInput, Exception], Any] | None,
        error_v1_handler: Callable[[StrategyErrorV1], Any] | None,
        *,
        snapshot_state_handler: Callable[[], Any] | None = None,
        restore_state_handler: Callable[[Mapping[str, Any]], Any] | None = None,
        stateless: bool = False,
    ) -> None:
        self._handler = handler
        self._error_handler = error_handler
        self._error_v1_handler = error_v1_handler
        self._active_error_context: dict[str, Any] | None = None
        self._snapshot_state_handler = snapshot_state_handler
        self._restore_state_handler = restore_state_handler
        self._state_mode = (
            "module-function" if snapshot_state_handler is not None else "stateless" if stateless else None
        )
        self._rollback_state: dict[str, Any] | None = None
        if self._state_mode is not None:
            self.strategy_checkpoint = self._strategy_checkpoint

    def __call__(self, request: StrategyInput) -> Any:
        self._active_error_context = {
            "stage": "market_event",
            "timestamp": request.as_of,
            "instrument": request.instrument,
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
        }
        self._prepare_request_state(request)
        return self._handler(request)

    def _prepare_request_state(self, request: StrategyInput) -> None:
        if self._state_mode == "module-function":
            self._rollback_state = self._snapshot_module_state()
        checkpoint = request.strategy_checkpoint
        if checkpoint is None:
            return
        if self._state_mode is None:
            raise ValueError(
                "persistent module strategy must export snapshot_state/restore_state or declare STRATEGY_STATELESS = True"
            )
        required = {
            "schema_version",
            "strategy_id",
            "strategy_version",
            "adapter_state",
            "strategy_state",
        }
        if not isinstance(checkpoint, Mapping) or set(checkpoint) != required:
            raise ValueError("module strategy checkpoint schema or fields are invalid")
        if (
            checkpoint.get("schema_version") != 1
            or checkpoint.get("strategy_id") != request.strategy_id
            or checkpoint.get("strategy_version") != request.strategy_version
        ):
            raise ValueError("module strategy checkpoint identity/version does not match the request")
        adapter_state = checkpoint.get("adapter_state")
        strategy_state = checkpoint.get("strategy_state")
        if not isinstance(adapter_state, Mapping) or not isinstance(strategy_state, Mapping):
            raise ValueError("module strategy checkpoint states must be objects")
        bootstrap = {"schema_version": 1, "state_mode": "bootstrap"}
        expected_adapter = {"schema_version": 1, "state_mode": self._state_mode}
        if dict(adapter_state) == bootstrap:
            if strategy_state:
                raise ValueError("module strategy checkpoint bootstrap state must be empty")
            return
        if dict(adapter_state) != expected_adapter:
            raise ValueError("module strategy checkpoint adapter state does not match its declared mode")
        if self._state_mode == "stateless":
            if strategy_state:
                raise ValueError("stateless module strategy checkpoint state must be empty")
            return
        assert self._restore_state_handler is not None
        state = self._normalize_state(strategy_state, "strategy checkpoint")
        self._restore_state_handler(state)
        self._rollback_state = self._snapshot_module_state()

    def _snapshot_module_state(self) -> dict[str, Any]:
        assert self._snapshot_state_handler is not None
        return self._normalize_state(self._snapshot_state_handler(), "snapshot_state()")

    @staticmethod
    def _normalize_state(value: Any, label: str) -> dict[str, Any]:
        if not isinstance(value, Mapping) or any(not isinstance(key, str) for key in value):
            raise ValueError(f"{label} must return a string-keyed object")
        try:
            encoded = json.dumps(
                dict(value),
                ensure_ascii=False,
                sort_keys=True,
                separators=(",", ":"),
                allow_nan=False,
            ).encode("utf-8")
        except (TypeError, ValueError) as error:
            raise ValueError(f"{label} must contain finite JSON values") from error
        if len(encoded) > 64 * 1024:
            raise ValueError(f"{label} exceeds the 64 KiB snapshot limit")
        return json.loads(encoded)

    def _strategy_checkpoint(self, request: StrategyInput) -> dict[str, Any]:
        if self._state_mode == "module-function":
            strategy_state = self._snapshot_module_state()
        elif self._state_mode == "stateless":
            strategy_state = {}
        else:
            raise ValueError("module strategy has no durable state contract")
        checkpoint = {
            "schema_version": 1,
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "adapter_state": {"schema_version": 1, "state_mode": self._state_mode},
            "strategy_state": strategy_state,
        }
        encoded = json.dumps(
            checkpoint,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
        if len(encoded) > 64 * 1024:
            raise ValueError("module strategy checkpoint exceeds the 64 KiB snapshot limit")
        return checkpoint

    def on_error(self, request: StrategyInput, error: Exception) -> Any:
        diagnostic = self._active_error_context
        error_v1 = None
        if diagnostic is not None:
            error_v1 = StrategyErrorV1.from_exception(
                stage=diagnostic["stage"],
                strategy_id=diagnostic["strategy_id"],
                strategy_version=diagnostic["strategy_version"],
                timestamp=diagnostic["timestamp"],
                instrument=diagnostic["instrument"],
                error=error,
            )
        hook_error: Exception | None = None
        if error_v1 is not None and self._error_v1_handler is not None:
            try:
                self._error_v1_handler(error_v1)
            except Exception as caught:  # noqa: BLE001 - run both diagnostic hooks.
                hook_error = caught
        if self._error_handler is not None:
            try:
                self._error_handler(request, error)
            except Exception as caught:  # noqa: BLE001 - original request failure remains primary.
                if hook_error is None:
                    hook_error = caught
        if hook_error is not None:
            raise hook_error
        return None

    def commit(self) -> None:
        self._active_error_context = None
        self._rollback_state = None
        commit = getattr(self._handler, "commit", None)
        if callable(commit):
            commit()

    def rollback(self) -> None:
        self._active_error_context = None
        if self._rollback_state is not None and self._restore_state_handler is not None:
            try:
                self._restore_state_handler(self._rollback_state)
            finally:
                self._rollback_state = None
        rollback = getattr(self._handler, "rollback", None)
        if callable(rollback):
            rollback()

    def __getattr__(self, name: str) -> Any:
        # Keep optional function attributes such as commit/rollback/close visible
        # to the existing worker lifecycle helpers.
        return getattr(self._handler, name)


def _strategy_class_factory(
    strategy_class: type,
    resolved_spec: dict[str, Any],
) -> Callable[[], Any]:
    """Build class strategies with either the explicit spec or no constructor args.

    ``create_strategy(spec)`` remains the explicit factory contract. A direct
    ``STRATEGY_CLASS`` may either accept the normalized spec (positionally or as
    a keyword-only ``spec``) or use a no-argument constructor for strategies
    whose configuration lives in class defaults/constants.
    """
    try:
        signature = inspect.signature(strategy_class)
    except (TypeError, ValueError):
        return lambda: strategy_class(resolved_spec)

    try:
        signature.bind(resolved_spec)
    except TypeError:
        try:
            signature.bind()
        except TypeError as no_argument_error:
            raise TypeError(
                "STRATEGY_CLASS constructor must accept a spec argument or no arguments"
            ) from no_argument_error
        return lambda: strategy_class()
    else:
        return lambda: strategy_class(resolved_spec)


def _handle_line(
    handler: Callable[[StrategyInput], Any],
    line: str,
    diagnostic_handler: Callable[[StrategyInput], Any] | None = None,
    include_diagnostics: bool = False,
    checkpoint_handler: Callable[[StrategyInput], Any] | None = None,
) -> str:
    if os.environ.get("QX_STRATEGY_STDOUT_TO_STDERR") == "1":
        with contextlib.redirect_stdout(sys.stderr):
            return _handle_line_inner(
                handler,
                line,
                diagnostic_handler,
                include_diagnostics,
                checkpoint_handler,
            )
    return _handle_line_inner(
        handler,
        line,
        diagnostic_handler,
        include_diagnostics,
        checkpoint_handler,
    )


def _handle_line_inner(
    handler: Callable[[StrategyInput], Any], line: str,
    diagnostic_handler: Callable[[StrategyInput], Any] | None = None,
    include_diagnostics: bool = False,
    checkpoint_handler: Callable[[StrategyInput], Any] | None = None,
) -> str:
    request: StrategyInput | None = None
    try:
        request = StrategyInput.from_json(line)
        value = handler(request)
        if isinstance(value, StrategyOutput):
            output = value
        elif isinstance(value, dict):
            output = StrategyOutput.from_dict(value, request)
        else:
            raise ValueError(  # noqa: TRY004 - invalid strategy return is a contract error.
                "on_decision must return StrategyOutput or dict"
            )
        response = {"ok": True, "output": output.to_dict(request)}
        if include_diagnostics:
            if diagnostic_handler is None:
                response["diagnostic_state"] = None
            else:
                state = diagnostic_handler(request)
                if not isinstance(state, dict) or any(not isinstance(key, str) for key in state):
                    raise ValueError("diagnostic_state(request) must return a string-keyed dict")
                response["diagnostic_state"] = state
        if checkpoint_handler is not None:
            response["strategy_checkpoint"] = checkpoint_handler(request)
        encoded = json.dumps(
            response,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        )
        _commit_handler(handler)
        return encoded
    except Exception as error:  # noqa: BLE001 - strategy errors return one recoverable response.
        if request is not None:
            _notify_handler_error(handler, request, error)
        _rollback_handler(handler)
        return json.dumps(
            {"ok": False, "error": str(error)},
            separators=(",", ":"),
            ensure_ascii=False,
        )


def serve(
    handler: Callable[[StrategyInput], Any],
    diagnostic_handler: Callable[[StrategyInput], Any] | None = None,
    include_diagnostics: bool = False,
    checkpoint_handler: Callable[[StrategyInput], Any] | None = None,
) -> None:
    stop_reason = "shutdown"
    try:
        for line in sys.stdin:
            if line.strip():
                sys.stdout.write(
                    _handle_line(
                        handler,
                        line,
                        diagnostic_handler,
                        include_diagnostics,
                        checkpoint_handler,
                    )
                )
                sys.stdout.write("\n")
                sys.stdout.flush()
    except Exception:
        stop_reason = "failed"
        raise
    finally:
        _close_handler(handler, reason=stop_reason)


def serve_framed(
    handler: Callable[[StrategyInput], Any],
    checkpoint_handler: Callable[[StrategyInput], Any] | None = None,
) -> None:
    input_stream = sys.stdin.buffer
    output_stream = sys.stdout.buffer
    stop_reason = "shutdown"
    try:
        while True:
            frame = read_frame(input_stream)
            if frame is None:
                return
            kind, sequence, payload = frame
            if kind != REQUEST:
                raise ValueError(f"strategy worker expected request frame, got kind={kind}")
            response = _handle_line(
                handler,
                payload.decode("utf-8"),
                checkpoint_handler=checkpoint_handler,
            )
            output_stream.write(encode_frame(RESPONSE, sequence, response.encode("utf-8")))
            output_stream.flush()
    except Exception:
        stop_reason = "failed"
        raise
    finally:
        _close_handler(handler, reason=stop_reason)


# 空闲侧最多每这么多秒确认一次父进程还在（每次确认都要开一次进程句柄）。
PARENT_LIVENESS_PROBE_SECONDS = 1.0

_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_ERROR_ACCESS_DENIED = 5
_PROCESS_STILL_ACTIVE = 259


def _parent_process_alive(pid: int) -> bool:
    """父进程是否还在运行。

    问不出结论时一律回答"还在"：把健康的 worker 误杀比让它多活一会儿更糟。
    """
    if pid <= 0:
        return True
    if os.name == "nt":
        import ctypes

        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            # 打不开句柄：被安全策略拒绝时进程仍在，只有 pid 已不存在才当真没了。
            return ctypes.GetLastError() == _ERROR_ACCESS_DENIED
        try:
            exit_code = ctypes.c_ulong()
            if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
                return True
            return exit_code.value == _PROCESS_STILL_ACTIVE
        finally:
            kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    return True


def serve_shared(
    handler: Callable[[StrategyInput], Any],
    input_path: str,
    output_path: str,
    capacity: int,
    slot_bytes: int,
    columnar: bool = False,
    parent_pid: int = 0,
    checkpoint_handler: Callable[[StrategyInput], Any] | None = None,
) -> None:
    """Serve QXSF requests over two SPSC mmap rings.

    ``parent_pid`` is the Rust process that spawned this worker. The ring transport has
    no stdin to close, so the only normal exit is the parent's ``Drop`` killing us; if the
    parent is killed outright, the idle loop checks the parent's liveness and returns
    instead of spinning forever on the ring files.
    """
    stop_reason = "shutdown"
    try:
        with SharedMemoryRing(input_path, capacity, slot_bytes) as input_ring, SharedMemoryRing(
            output_path, capacity, slot_bytes
        ) as output_ring:
            next_parent_check = time.monotonic() + PARENT_LIVENESS_PROBE_SECONDS
            while True:
                try:
                    encoded = input_ring.try_pop()
                except RingEmpty:
                    time.sleep(0.001)
                    now = time.monotonic()
                    if parent_pid > 0 and now >= next_parent_check:
                        next_parent_check = now + PARENT_LIVENESS_PROBE_SECONDS
                        if not _parent_process_alive(parent_pid):
                            return
                    continue
                frame = read_frame(io.BytesIO(encoded))
                if frame is None:
                    raise ValueError("shared strategy request frame is empty")
                kind, sequence, payload = frame
                if kind != REQUEST:
                    raise ValueError(f"strategy worker expected request frame, got kind={kind}")
                if columnar:
                    request = decode_columnar_request(payload)
                    response = _handle_line(
                        handler,
                        request.to_json(),
                        checkpoint_handler=checkpoint_handler,
                    )
                else:
                    response = _handle_line(
                        handler,
                        payload.decode("utf-8"),
                        checkpoint_handler=checkpoint_handler,
                    )
                encoded_response = encode_frame(RESPONSE, sequence, response.encode("utf-8"))
                output_ring.push_wait(encoded_response, time.monotonic() + 30.0)
    except Exception:
        stop_reason = "failed"
        raise
    finally:
        _close_handler(handler, reason=stop_reason)


def _close_handler(
    handler: Callable[[StrategyInput], Any], *, reason: str = "host_stop"
) -> None:
    close = getattr(handler, "close", None)
    if callable(close):
        from .class_adapter import BaseStrategyProcessAdapter

        close_strategy = isinstance(handler, BaseStrategyProcessAdapter)

        def close_handler() -> None:
            if close_strategy:
                close(reason=reason)
            else:
                close()

        if os.environ.get("QX_STRATEGY_STDOUT_TO_STDERR") == "1":
            with contextlib.redirect_stdout(sys.stderr):
                close_handler()
        else:
            close_handler()


def _initialize_handlers(
    args: argparse.Namespace,
    parser: argparse.ArgumentParser,
) -> tuple[
    Callable[[StrategyInput], Any],
    Callable[[StrategyInput], Any] | None,
    Callable[[StrategyInput], Any] | None,
]:
    module = _load_strategy_module(args.module)
    strategy_spec_text = os.environ.get("QX_STRATEGY_SPEC_JSON")
    strategy_spec: dict[str, Any] | None = None
    if strategy_spec_text is not None:
        try:
            decoded_spec = json.loads(strategy_spec_text)
        except json.JSONDecodeError as error:
            parser.error(f"QX_STRATEGY_SPEC_JSON is invalid JSON: {error}")
        if not isinstance(decoded_spec, dict):
            parser.error("QX_STRATEGY_SPEC_JSON must be a JSON object")
        strategy_spec = decoded_spec
    handler = _handler_from_module(
        module,
        strategy_spec,
        account_id=os.environ.get("QX_STRATEGY_ACCOUNT_ID"),
        venue_id=os.environ.get("QX_STRATEGY_VENUE_ID"),
    )
    diagnostic_handler = (
        (
            getattr(handler, "diagnostic_state", None)
            if strategy_spec is not None
            else getattr(module, "diagnostic_state", None)
        )
        if args.include_diagnostics
        else None
    )
    if diagnostic_handler is not None and not callable(diagnostic_handler):
        parser.error("diagnostic_state must be callable when provided")
    checkpoint_handler = getattr(handler, "strategy_checkpoint", None)
    if checkpoint_handler is not None and not callable(checkpoint_handler):
        parser.error("strategy_checkpoint must be callable when provided")
    return handler, diagnostic_handler, checkpoint_handler


def _commit_handler(handler: Callable[[StrategyInput], Any]) -> None:
    commit = getattr(handler, "commit", None)
    if callable(commit):
        commit()


def _rollback_handler(handler: Callable[[StrategyInput], Any]) -> None:
    rollback = getattr(handler, "rollback", None)
    if callable(rollback):
        try:
            rollback()
        except Exception as error:  # noqa: BLE001 - failed strategy rollback is diagnostic only.
            sys.stderr.write(f"strategy rollback failed: {error}\n")
            sys.stderr.flush()


def _notify_handler_error(
    handler: Callable[[StrategyInput], Any],
    request: StrategyInput,
    error: Exception,
) -> None:
    notify = getattr(handler, "on_error", None)
    if not callable(notify):
        return
    try:
        notify(request, error)
    except Exception as hook_error:  # noqa: BLE001 - keep the request failure authoritative.
        sys.stderr.write(
            "strategy on_error hook failed: "
            f"{type(hook_error).__name__}; original request error retained\n"
        )
        sys.stderr.flush()


def main() -> int:
    parser = argparse.ArgumentParser(description="Qianxing Python strategy JSONL worker")
    parser.add_argument("--module", required=True, help="module name or .py file")
    parser.add_argument(
        "--include-diagnostics", action="store_true",
        help="include optional diagnostic_state(request) output in JSONL responses",
    )
    parser.add_argument(
        "--protocol",
        choices=("jsonl", "framed_json", "shared_memory_json", "shared_memory_columnar"),
        default="jsonl",
        help="strategy transport protocol",
    )
    parser.add_argument("--input-ring", help="shared-memory input ring path")
    parser.add_argument("--output-ring", help="shared-memory output ring path")
    parser.add_argument("--ring-capacity", type=int, default=1024)
    parser.add_argument("--ring-slot-bytes", type=int, default=64 * 1024)
    parser.add_argument(
        "--ready-file",
        help="optional startup marker written after the strategy module and handler initialize",
    )
    parser.add_argument(
        "--parent-pid",
        type=int,
        default=0,
        help="spawned-by process id; the shared-memory loop exits when it is gone (0 disables the check)",
    )
    args = parser.parse_args()
    if args.include_diagnostics and args.protocol != "jsonl":
        parser.error("--include-diagnostics is supported only with the JSONL protocol")
    if os.environ.get("QX_STRATEGY_STDOUT_TO_STDERR") == "1":
        with contextlib.redirect_stdout(sys.stderr):
            handler, diagnostic_handler, checkpoint_handler = _initialize_handlers(
                args, parser
            )
    else:
        handler, diagnostic_handler, checkpoint_handler = _initialize_handlers(
            args, parser
        )
    if args.ready_file:
        Path(args.ready_file).write_text("ready\n", encoding="utf-8")
    if args.protocol == "framed_json":
        serve_framed(handler, checkpoint_handler)
    elif args.protocol in ("shared_memory_json", "shared_memory_columnar"):
        if not args.input_ring or not args.output_ring:
            parser.error("shared_memory_json requires --input-ring and --output-ring")
        serve_shared(
            handler,
            args.input_ring,
            args.output_ring,
            args.ring_capacity,
            args.ring_slot_bytes,
            columnar=args.protocol == "shared_memory_columnar",
            parent_pid=args.parent_pid,
            checkpoint_handler=checkpoint_handler,
        )
    else:
        serve(handler, diagnostic_handler, args.include_diagnostics, checkpoint_handler)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
