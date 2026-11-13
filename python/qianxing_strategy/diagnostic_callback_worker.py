"""One-shot adapter for diagnosing ``create_strategy(spec)`` Bar callbacks.

The parent diagnostic runner launches this worker for each replay so every request
gets a fresh strategy instance. The adapter replays only bars visible before the
request's ``as_of`` time, while exposing the perturbed full frame through a temporary
``bars_path`` so strategies that read their configured frame can also be probed.
"""

from __future__ import annotations

import argparse
import importlib.util
import inspect
import json
import sys
import tempfile
from pathlib import Path
from typing import Any


PYTHON_ROOT = Path(__file__).resolve().parents[1]
if str(PYTHON_ROOT) not in sys.path:
    sys.path.insert(0, str(PYTHON_ROOT))

from qianxing_bridge.strategy import StrategyBars, StrategyInput
from qianxing_strategy import BaseStrategy, StrategyDataPortal, StrategyErrorV1
from qianxing_strategy.base import _dispatch_on_error
from qianxing_strategy.class_adapter import BaseStrategyProcessAdapter

MAX_DIAGNOSTIC_STATE_BYTES = 64 * 1024


def _load_factory(path: Path, *, include_module: bool = False):
    name = f"_qianxing_diagnostic_strategy_{abs(hash(str(path.resolve()))):x}"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ValueError(f"cannot load strategy file: {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    root = str(path.resolve().parent)
    if root not in sys.path:
        sys.path.insert(0, root)
    spec.loader.exec_module(module)
    factory = getattr(module, "create_strategy", None)
    strategy_class = getattr(module, "STRATEGY_CLASS", None)
    if factory is None and strategy_class is not None:
        from qianxing_strategy import BaseStrategy

        if not isinstance(strategy_class, type) or not issubclass(
            strategy_class, BaseStrategy
        ):
            raise ValueError("STRATEGY_CLASS must be a qianxing_strategy.BaseStrategy subclass")
        try:
            constructor = inspect.signature(strategy_class)
        except (TypeError, ValueError) as error:
            raise ValueError(
                "cannot inspect STRATEGY_CLASS constructor; export create_strategy(spec) instead"
            ) from error
        try:
            constructor.bind({})
        except TypeError:
            try:
                constructor.bind()
            except TypeError as error:
                raise ValueError(
                    "STRATEGY_CLASS constructor must accept the strategy spec or no arguments"
                ) from error

            def factory(_spec):
                return strategy_class()
        else:
            def factory(strategy_spec):
                return strategy_class(strategy_spec)
    if not callable(factory):
        raise ValueError(
            "strategy file must export STRATEGY_CLASS (a BaseStrategy subclass) "
            "or callable create_strategy(spec)"
        )
    return (factory, module) if include_module else factory


def _bar_frame(request: StrategyInput) -> dict[str, Any]:
    if request.bars is None:
        raise ValueError("callback diagnostics require a BarFrame")
    return {
        "instrument": request.instrument,
        **request.bars.to_dict(),
    }


def _callback_decision(strategy: Any, context: dict[str, Any], event: dict[str, Any]) -> dict[str, Any]:
    handler = getattr(strategy, "on_event", strategy)
    value = handler(context, event)
    if isinstance(value, str):
        value = json.loads(value)
    if not isinstance(value, dict):
        raise ValueError("Python strategy on_event must return a JSON decision object")
    return value


def _read_diagnostic_state(strategy: Any) -> dict[str, Any] | None:
    state_reader = getattr(strategy, "diagnostic_state", None)
    if not callable(state_reader):
        return None
    state = state_reader()
    if state is None:
        return None
    if not isinstance(state, dict):
        raise ValueError("diagnostic_state() must return a JSON object or None")
    if any(not isinstance(key, str) for key in state):
        raise ValueError("diagnostic_state() object keys must be strings")
    try:
        encoded = json.dumps(
            state,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError, UnicodeError) as error:
        raise ValueError("diagnostic_state() must contain only finite JSON values") from error
    if len(encoded) > MAX_DIAGNOSTIC_STATE_BYTES:
        raise ValueError(
            "diagnostic_state() exceeds the 64 KiB snapshot limit "
            f"({len(encoded)} bytes)"
        )
    return state


def _visible_frame(frame: StrategyBars, as_of: int) -> StrategyBars:
    end = sum(1 for timestamp in frame.ts if timestamp < as_of)
    source = frame.to_dict()
    return StrategyBars.from_dict(
        {
            key: value[:end]
            if isinstance(value, list) and len(value) == len(frame.ts)
            else value
            for key, value in source.items()
        }
    )


def _diagnostic_request(
    request: StrategyInput,
    auxiliary_market_bars: dict[str, StrategyBars] | None,
) -> StrategyInput:
    value = request.to_dict()
    assert request.bars is not None
    value["bars"] = _visible_frame(request.bars, request.as_of).to_dict()
    frames = dict(request.market_bars)
    frames.update(auxiliary_market_bars or {})
    value["market_bars"] = {
        instrument: _visible_frame(frame, request.as_of).to_dict()
        for instrument, frame in frames.items()
    }
    return StrategyInput.from_dict(value)


def _log_observer_failure(error: BaseException) -> None:
    sys.stderr.write(
        "[Qianxing diagnostics] strategy error observer failed: "
        f"{type(error).__name__}; original diagnostic error retained\n"
    )
    sys.stderr.flush()


def _notify_diagnostic_failure(
    strategy: Any | None,
    module: Any,
    request: StrategyInput,
    context: dict[str, Any],
    error: Exception,
    stage: str,
) -> None:
    """Notify available class/module observers without masking the diagnostic failure."""
    try:
        error_v1 = StrategyErrorV1.from_context(
            context,
            error,
            stage=stage,
            timestamp=context.get("as_of"),
            instrument=request.instrument,
        )
    except (TypeError, ValueError):
        error_v1 = None
    if strategy is not None:
        try:
            _dispatch_on_error(strategy, context, error, error_v1)
        except Exception as observer_error:
            _log_observer_failure(observer_error)
    standardized = getattr(module, "on_error_v1", None)
    if error_v1 is not None and callable(standardized):
        try:
            standardized(error_v1)
        except Exception as observer_error:
            _log_observer_failure(observer_error)
    legacy = getattr(module, "on_error", None)
    if callable(legacy):
        try:
            legacy(request, error)
        except Exception as observer_error:
            _log_observer_failure(observer_error)


def _handle(
    request: StrategyInput,
    strategy_file: Path,
    spec_template: dict[str, Any],
    auxiliary_market_bars: dict[str, StrategyBars] | None = None,
) -> dict[str, Any]:
    if request.bars is None:
        raise ValueError("callback diagnostics require bars")
    symbol, separator, venue = request.instrument.rpartition(".")
    if not separator or not symbol or not venue:
        raise ValueError(f"invalid instrument identity: {request.instrument}")
    visible = [index for index, timestamp in enumerate(request.bars.ts) if timestamp < request.as_of]
    if not visible:
        raise ValueError("no completed Bar is visible at this decision time")

    with tempfile.TemporaryDirectory(prefix="qianxing-strategy-diagnostic-") as temporary:
        directory = Path(temporary)
        spec_value = dict(spec_template)
        configured_markets = spec_value.get("markets")
        if isinstance(configured_markets, list):
            frames = {request.instrument: _bar_frame(request)}
            frames.update(
                {
                    instrument: {"instrument": instrument, **bars.to_dict()}
                    for instrument, bars in request.market_bars.items()
                }
            )
            frames.update(
                {
                    instrument: {"instrument": instrument, **bars.to_dict()}
                    for instrument, bars in (auxiliary_market_bars or {}).items()
                }
            )
            for index, market in enumerate(configured_markets):
                if not isinstance(market, dict) or not isinstance(market.get("instrument"), str):
                    raise ValueError(f"multi-market spec markets[{index}] has no instrument")
                instrument = market["instrument"]
                frame = frames.get(
                    instrument,
                    {
                        "instrument": instrument,
                        "source": "diagnostic-empty-peer-frame",
                        "ts": [],
                        "open_raw": [],
                        "high_raw": [],
                        "low_raw": [],
                        "close_raw": [],
                        "volume_raw": [],
                    },
                )
                frame_path = directory / f"market-{index}.json"
                frame_path.write_text(
                    json.dumps(frame, separators=(",", ":"), allow_nan=False),
                    encoding="utf-8",
                )
                market["bars_path"] = str(frame_path)
                if instrument == request.instrument and isinstance(spec_value.get("market"), dict):
                    spec_value["market"] = market
        else:
            frame_path = directory / "counterfactual-bars.json"
            frame_path.write_text(
                json.dumps(_bar_frame(request), separators=(",", ":"), allow_nan=False),
                encoding="utf-8",
            )
            spec_value["bars_path"] = str(frame_path)
        if "output_dir" in spec_value:
            spec_value["output_dir"] = str(directory / "diagnostic-output")
        factory, module = _load_factory(strategy_file, include_module=True)
        error_context = {
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "account_id": spec_value.get("account_id") or "app",
            "venue_id": venue,
            "data_fingerprint": request.data_fingerprint,
            "as_of": request.as_of,
            "positions": dict(request.positions),
            "positions_exact": dict(request.positions_exact),
            "cash": dict(request.cash),
            "cash_exact": dict(request.cash_exact),
            "available_margin_raw": request.available_margin_raw,
            "available_margin_exact": request.available_margin_exact,
            "risk_state": request.risk_state,
        }
        try:
            strategy = factory(spec_value)
        except Exception as error:
            _notify_diagnostic_failure(
                None, module, request, error_context, error, "initialization"
            )
            raise
        if strategy is None:
            error = ValueError("create_strategy(spec) returned None")
            _notify_diagnostic_failure(
                None, module, request, error_context, error, "initialization"
            )
            raise error

        if isinstance(strategy, BaseStrategy):
            bounded_request = _diagnostic_request(request, auxiliary_market_bars)
            adapter = BaseStrategyProcessAdapter(strategy, data_level="bar")
            try:
                decision = adapter(bounded_request)
                diagnostic_state = adapter.diagnostic_state(bounded_request)
                adapter.close(reason="host_stop")
            except Exception as error:
                if not adapter._stopped:
                    try:
                        adapter.on_error(bounded_request, error)
                    except Exception as observer_error:
                        _log_observer_failure(observer_error)
                diagnostic = adapter._error_context or {}
                module_context = dict(error_context)
                timestamp = diagnostic.get("timestamp")
                if isinstance(timestamp, int) and not isinstance(timestamp, bool):
                    module_context["as_of"] = timestamp
                _notify_diagnostic_failure(
                    None,
                    module,
                    bounded_request,
                    module_context,
                    error,
                    diagnostic.get("stage", "market_event"),
                )
                raise
            return {"output": decision, "diagnostic_state": diagnostic_state}

        cash = dict(request.cash)
        decision: dict[str, Any] | None = None
        context = error_context
        stage = "market_event"
        try:
            for index in visible:
                timestamp = request.bars.ts[index]
                context = {
                    **error_context,
                    "as_of": timestamp,
                    "data": StrategyDataPortal(
                        {
                            **request.market_bars,
                            **(auxiliary_market_bars or {}),
                            request.instrument: request.bars,
                        },
                        as_of=timestamp,
                    ),
                }
                event = {
                    "Bar": {
                        "instrument": {"symbol": symbol, "venue": venue},
                        "ts": timestamp,
                        "open_raw": request.bars.open_raw[index],
                        "high_raw": request.bars.high_raw[index],
                        "low_raw": request.bars.low_raw[index],
                        "close_raw": request.bars.close_raw[index],
                        "volume_raw": request.bars.volume_raw[index],
                    }
                }
                decision = _callback_decision(strategy, context, event)
            diagnostic_state = _read_diagnostic_state(strategy)
            return {"output": decision, "diagnostic_state": diagnostic_state}
        except Exception as error:
            _notify_diagnostic_failure(
                strategy, module, request, context, error, stage
            )
            raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strategy-file", required=True, type=Path)
    parser.add_argument("--spec-file", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        spec_template = json.loads(args.spec_file.read_text(encoding="utf-8"))
        if not isinstance(spec_template, dict):
            raise ValueError("BacktestSpec must be a JSON object")
        for line in sys.stdin:
            if not line.strip():
                continue
            raw = json.loads(line)
            if isinstance(raw, dict) and isinstance(raw.get("strategy_input"), dict):
                request = StrategyInput.from_dict(raw["strategy_input"])
                raw_market_bars = raw.get("market_bars", {})
                if not isinstance(raw_market_bars, dict):
                    raise ValueError("diagnostic market_bars must be an object")
                market_bars = {
                    str(instrument): StrategyBars.from_dict(frame)
                    for instrument, frame in raw_market_bars.items()
                }
            else:
                request = StrategyInput.from_dict(raw)
                market_bars = None
            response = _handle(
                request, args.strategy_file, spec_template, auxiliary_market_bars=market_bars
            )
            print(json.dumps({"ok": True, **response}, separators=(",", ":"), allow_nan=False), flush=True)
        return 0
    except Exception as error:
        print(
            json.dumps({"ok": False, "error": f"{type(error).__name__}: {error}"}),
            file=sys.stderr,
            flush=True,
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
