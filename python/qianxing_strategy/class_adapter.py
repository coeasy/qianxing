"""Adapt a ``BaseStrategy`` class to the live Strategy process contract.

The process protocol still owns request identity and response validation. This
adapter only dispatches the callback SDK's lifecycle and typed hooks, then
normalizes their decisions back into the existing ``StrategyOutput`` shape.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
import sys
from typing import Any

from qianxing_bridge.strategy import StrategyInput, StrategyOutput

from .errors import StrategyErrorV1
from .base import (
    BaseStrategy,
    StrategyDataPortal,
    _dispatch_on_error,
    _dispatch_on_stop,
    declared_warmup_bars,
)


class BaseStrategyProcessAdapter:
    """Stateful callback adapter for a single Python strategy worker process."""

    def __init__(
        self,
        strategy: BaseStrategy,
        *,
        account_id: str | None = None,
        venue_id: str | None = None,
        strategy_factory: Any | None = None,
        manifest_warmup_bars: int | None = None,
        data_level: str = "bar",
    ) -> None:
        if not isinstance(strategy, BaseStrategy):
            raise TypeError("class process mode requires a qianxing_strategy.BaseStrategy instance")
        if account_id is not None and (not isinstance(account_id, str) or not account_id.strip()):
            raise ValueError("strategy account_id must be a non-empty string")
        if venue_id is not None and (not isinstance(venue_id, str) or not venue_id.strip()):
            raise ValueError("strategy venue_id must be a non-empty string")
        if data_level not in {"bar", "tick", "orderbook", "timer"}:
            raise ValueError("BaseStrategy process data_level must be bar, tick, orderbook, or timer")
        self.strategy = strategy
        self.account_id = account_id
        self.venue_id = venue_id
        self._strategy_factory = strategy_factory
        self._manifest_warmup_bars = manifest_warmup_bars
        self._data_level = data_level
        self._in_request = False
        self._poisoned = False
        self._started = False
        self._checkpoint_restored_this_worker = False
        self._stopped = False
        self._last_context: dict[str, Any] | None = None
        self._last_instrument: str | None = None
        self._error_context: dict[str, Any] | None = None
        self._warmup_bars = 0
        self._history_requirements_checked = False
        self._bars_seen: dict[str, int] = {}
        self._last_bar_ts: dict[str, int] = {}
        self._warmup_completed: set[str] = set()
        self._update_cursors: dict[tuple[str, str], int] = {}
        self._seen_update_kinds_at_cursor: dict[tuple[str, str], set[str]] = {}

    @staticmethod
    def _resolve_warmup(
        strategy: BaseStrategy,
        manifest_warmup_bars: int | None = None,
    ) -> int:
        manifest = getattr(type(strategy), "STRATEGY_MANIFEST", None)
        manifest_warmup: int | None = None
        if manifest is not None:
            if not isinstance(manifest, Mapping):
                raise ValueError("STRATEGY_MANIFEST must be a mapping")
            raw_warmup = manifest.get("warmup_bars", 0)
            if isinstance(raw_warmup, bool) or not isinstance(raw_warmup, int) or raw_warmup < 0:
                raise ValueError("STRATEGY_MANIFEST.warmup_bars must be a non-negative integer")
            manifest_warmup = raw_warmup
        if manifest_warmup_bars is not None:
            if (
                isinstance(manifest_warmup_bars, bool)
                or not isinstance(manifest_warmup_bars, int)
                or manifest_warmup_bars < 0
            ):
                raise ValueError("manifest warmup_bars must be a non-negative integer")
            if manifest_warmup is not None and manifest_warmup != manifest_warmup_bars:
                raise ValueError(
                    "module and class STRATEGY_MANIFEST.warmup_bars conflict"
                )
            manifest_warmup = manifest_warmup_bars
        declared = declared_warmup_bars(strategy)
        if manifest_warmup is not None and declared is not None and manifest_warmup != declared:
            raise ValueError(
                "strategy history_requirements.warmup_bars conflicts with "
                "STRATEGY_MANIFEST.warmup_bars"
            )
        return declared if declared is not None else (manifest_warmup or 0)

    def __call__(self, request: StrategyInput) -> dict[str, Any]:
        self._error_context = None
        if self._poisoned:
            raise RuntimeError("BaseStrategy worker is poisoned after a failed strategy reset")
        if self._in_request:
            raise RuntimeError("BaseStrategy worker received a nested strategy request")
        self._in_request = True
        request.validate()
        # Lifecycle hooks run before the request's historical events are replayed.
        # They must not see the full frozen snapshot and seed state with future Bars.
        context = self._context(request, request.as_of, include_history=False)
        if not self._history_requirements_checked:
            self._track_callback_error(
                "history_requirements", context, request.instrument, request.as_of
            )
            self._warmup_bars = self._resolve_warmup(
                self.strategy,
                self._manifest_warmup_bars,
            )
            if (
                self._data_level in {"tick", "orderbook", "timer"}
                and self._warmup_bars != 0
            ):
                raise ValueError(
                    "Runtime Tick/Timer/OrderBook BaseStrategy must use warmup_bars=0"
                )
            self._history_requirements_checked = True
        if request.strategy_checkpoint is not None:
            self._restore_checkpoint(request.strategy_checkpoint, request, context)
        self._validate_bars(request)
        if not self._started:
            self._track_callback_error("initialization", context, request.instrument, request.as_of)
            self.strategy.on_init(context)
            self._track_callback_error("start", context, request.instrument, request.as_of)
            self.strategy.on_start(context)
            self._started = True
            self._checkpoint_restored_this_worker = True
            if self._data_level in {"tick", "orderbook", "timer"}:
                instrument = request.instrument
                self._complete_warmup(
                    request,
                    instrument,
                    request.as_of,
                    0,
                    include_history=False,
                )
            elif self._warmup_bars == 0:
                for instrument in self._instruments(request):
                    self._complete_warmup(
                        request,
                        instrument,
                        request.as_of,
                        0,
                        include_history=False,
                    )

        decisions: list[Mapping[str, Any]] = []
        decisions.extend(self._dispatch_updates(request))
        if self._data_level == "tick":
            decisions.extend(self._dispatch_ticks(request))
        elif self._data_level == "orderbook":
            decisions.extend(self._dispatch_order_books(request))
        elif self._data_level == "timer":
            decisions.extend(self._dispatch_timer(request))
        else:
            decisions.extend(self._dispatch_bars(request))
        output = self._merge_decisions(request, decisions)
        StrategyOutput.from_dict(output, request)
        self._last_context = dict(context)
        self._last_instrument = request.instrument
        return output

    def diagnostic_state(self, _request: StrategyInput) -> dict[str, Any] | None:
        state = self.strategy.diagnostic_state()
        if state is None:
            return None
        if not isinstance(state, dict) or any(not isinstance(key, str) for key in state):
            raise ValueError("diagnostic_state() must return a string-keyed object or None")
        # Enforce the same bounded JSON snapshot accepted by callback diagnostics.
        encoded = json.dumps(
            state,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
        if len(encoded) > 64 * 1024:
            raise ValueError("diagnostic_state() exceeds the 64 KiB snapshot limit")
        return state

    def strategy_checkpoint(self, request: StrategyInput) -> dict[str, Any]:
        strategy_state = self.strategy.snapshot_state()
        if not isinstance(strategy_state, Mapping) or any(
            not isinstance(key, str) for key in strategy_state
        ):
            raise ValueError("snapshot_state() must return a string-keyed object")
        adapter_state = {
            "schema_version": 1,
            "bars_seen": dict(self._bars_seen),
            "last_bar_ts": dict(self._last_bar_ts),
            "warmup_completed": sorted(self._warmup_completed),
            "update_cursors": [
                {
                    "account_id": account_id,
                    "venue_id": venue_id,
                    "event_seq": event_seq,
                    "kinds": sorted(self._seen_update_kinds_at_cursor.get((account_id, venue_id), set())),
                }
                for (account_id, venue_id), event_seq in sorted(self._update_cursors.items())
            ],
        }
        checkpoint = {
            "schema_version": 1,
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "adapter_state": adapter_state,
            "strategy_state": dict(strategy_state),
        }
        try:
            encoded = json.dumps(
                checkpoint,
                ensure_ascii=False,
                sort_keys=True,
                separators=(",", ":"),
                allow_nan=False,
            ).encode("utf-8")
        except (TypeError, ValueError) as error:
            raise ValueError("strategy checkpoint must contain finite JSON values") from error
        if len(encoded) > 64 * 1024:
            raise ValueError("strategy checkpoint exceeds the 64 KiB snapshot limit")
        return checkpoint

    def _restore_checkpoint(
        self,
        checkpoint: Mapping[str, Any],
        request: StrategyInput,
        context: Mapping[str, Any],
    ) -> None:
        required = {
            "schema_version",
            "strategy_id",
            "strategy_version",
            "adapter_state",
            "strategy_state",
        }
        if set(checkpoint) != required or checkpoint.get("schema_version") != 1:
            raise ValueError("strategy checkpoint schema or fields are invalid")
        if (
            checkpoint.get("strategy_id") != request.strategy_id
            or checkpoint.get("strategy_version") != request.strategy_version
        ):
            raise ValueError("strategy checkpoint identity/version does not match the request")
        adapter = checkpoint.get("adapter_state")
        user_state = checkpoint.get("strategy_state")
        if not isinstance(adapter, Mapping) or not isinstance(user_state, Mapping):
            raise ValueError("strategy checkpoint states must be objects")
        if adapter.get("schema_version") != 1:
            raise ValueError("strategy adapter checkpoint schema_version is unsupported")
        allowed_adapter_fields = {
            "schema_version",
            "bars_seen",
            "last_bar_ts",
            "warmup_completed",
            "update_cursors",
        }
        if set(adapter) != allowed_adapter_fields:
            raise ValueError("strategy adapter checkpoint fields are invalid")

        bars_seen = adapter["bars_seen"]
        last_bar_ts = adapter["last_bar_ts"]
        warmup_completed = adapter["warmup_completed"]
        cursors = adapter["update_cursors"]
        if not isinstance(bars_seen, Mapping) or not isinstance(last_bar_ts, Mapping):
            raise ValueError("strategy adapter checkpoint bar state must be objects")
        parsed_bars_seen: dict[str, int] = {}
        for instrument, count in bars_seen.items():
            if (
                not isinstance(instrument, str)
                or isinstance(count, bool)
                or not isinstance(count, int)
                or count < 0
            ):
                raise ValueError("strategy adapter checkpoint bars_seen is invalid")
            parsed_bars_seen[instrument] = count
        parsed_last_bar_ts: dict[str, int] = {}
        for instrument, timestamp in last_bar_ts.items():
            if (
                not isinstance(instrument, str)
                or isinstance(timestamp, bool)
                or not isinstance(timestamp, int)
                or timestamp <= 0
            ):
                raise ValueError("strategy adapter checkpoint last_bar_ts is invalid")
            parsed_last_bar_ts[instrument] = timestamp
        if not isinstance(warmup_completed, list) or any(
            not isinstance(instrument, str) for instrument in warmup_completed
        ) or len(set(warmup_completed)) != len(warmup_completed):
            raise ValueError("strategy adapter checkpoint warmup_completed is invalid")
        if not isinstance(cursors, list):
            raise ValueError("strategy adapter checkpoint update_cursors must be a list")
        parsed_cursors: dict[tuple[str, str], int] = {}
        parsed_kinds: dict[tuple[str, str], set[str]] = {}
        for item in cursors:
            if not isinstance(item, Mapping) or set(item) != {
                "account_id",
                "venue_id",
                "event_seq",
                "kinds",
            }:
                raise ValueError("strategy adapter checkpoint update cursor fields are invalid")
            account_id = item["account_id"]
            venue_id = item["venue_id"]
            event_seq = item["event_seq"]
            kinds = item["kinds"]
            scope = (account_id, venue_id)
            if (
                not isinstance(account_id, str)
                or not account_id.strip()
                or not isinstance(venue_id, str)
                or not venue_id.strip()
                or isinstance(event_seq, bool)
                or not isinstance(event_seq, int)
                or event_seq < 0
                or not isinstance(kinds, list)
                or any(
                    kind not in {
                        "order_status",
                        "fill",
                        "account_snapshot",
                        "position_snapshot",
                    }
                    for kind in kinds
                )
                or len(set(kinds)) != len(kinds)
                or scope in parsed_cursors
            ):
                raise ValueError("strategy adapter checkpoint update cursor is invalid")
            parsed_cursors[scope] = event_seq
            parsed_kinds[scope] = set(kinds)

        self.strategy.restore_state(user_state)
        self._bars_seen = parsed_bars_seen
        self._last_bar_ts = parsed_last_bar_ts
        self._warmup_completed = set(warmup_completed)
        self._update_cursors = parsed_cursors
        self._seen_update_kinds_at_cursor = parsed_kinds
        self._started = True
        self._stopped = False
        if not self._checkpoint_restored_this_worker:
            self._track_callback_error(
                "initialization", context, request.instrument, request.as_of
            )
            self.strategy.on_restore(context)
            self._checkpoint_restored_this_worker = True

    def close(self, *, reason: str = "host_stop") -> None:
        if self._started and not self._stopped:
            self._stopped = True
            context = self._last_context or {}
            try:
                _dispatch_on_stop(self.strategy, context, reason)
            except Exception as error:
                try:
                    error_v1 = StrategyErrorV1.from_context(
                        context,
                        error,
                        stage="stop",
                        instrument=self._last_instrument,
                    )
                except (TypeError, ValueError):
                    error_v1 = None
                try:
                    _dispatch_on_error(self.strategy, context, error, error_v1)
                except Exception as hook_error:
                    sys.stderr.write(
                        "strategy on_error hook failed: "
                        f"{type(hook_error).__name__}; original stop error retained\n"
                    )
                    sys.stderr.flush()
                raise

    def on_error(self, request: StrategyInput, error: Exception) -> None:
        """Forward a failed request to the current strategy without changing its response."""
        context = self._context(request, request.as_of, include_history=False)
        diagnostic = self._error_context
        error_v1 = None
        if diagnostic is not None:
            try:
                error_v1 = StrategyErrorV1.from_context(
                    context,
                    error,
                    stage=diagnostic["stage"],
                    timestamp=diagnostic["timestamp"],
                    instrument=diagnostic["instrument"],
                )
            except (KeyError, TypeError, ValueError):
                error_v1 = None
        _dispatch_on_error(self.strategy, context, error, error_v1)

    def _track_callback_error(
        self,
        stage: str,
        context: Mapping[str, Any],
        instrument: str | None = None,
        timestamp: int | None = None,
    ) -> None:
        self._error_context = {
            "stage": stage,
            "timestamp": timestamp if timestamp is not None else context.get("as_of"),
            "instrument": instrument,
        }

    def commit(self) -> None:
        self._in_request = False
        self._error_context = None

    def rollback(self) -> None:
        if not self._in_request:
            return
        if self._strategy_factory is None:
            self._poisoned = True
            self._in_request = False
            return
        try:
            strategy = self._strategy_factory()
            if not isinstance(strategy, BaseStrategy):
                raise TypeError("create_strategy(spec) must return a BaseStrategy instance")
            self.strategy = strategy
            self._started = False
            self._checkpoint_restored_this_worker = False
            self._stopped = False
            self._bars_seen.clear()
            self._last_bar_ts.clear()
            self._warmup_completed.clear()
            self._update_cursors.clear()
            self._seen_update_kinds_at_cursor.clear()
            self._error_context = None
            self._warmup_bars = 0
            self._history_requirements_checked = False
            self._in_request = False
        except Exception:
            self._poisoned = True
            self._in_request = False
            raise

    def _dispatch_updates(self, request: StrategyInput) -> list[Mapping[str, Any]]:
        pending = []
        for priority, kind, updates in (
            (0, "order_status", request.execution_updates),
            (1, "fill", request.fill_updates),
        ):
            for update in updates:
                order = update["order"]
                _, venue_id = self._split_instrument(order["instrument"])
                pending.append(
                    (
                        update["ts"],
                        order["account_id"],
                        venue_id,
                        update["event_seq"],
                        priority,
                        order["order_id"],
                        kind,
                        update,
                    )
                )
        for update in request.account_updates:
            kind = update["event"]
            pending.append(
                (
                    update["ts"],
                    update["account_id"],
                    update["venue_id"],
                    update["event_seq"],
                    2,
                    0,
                    kind,
                    update,
                )
            )
        pending.sort(key=lambda item: item[:6])
        decisions: list[Mapping[str, Any]] = []
        cursor_snapshot = dict(self._update_cursors)
        kinds_snapshot = {
            scope: set(kinds)
            for scope, kinds in self._seen_update_kinds_at_cursor.items()
        }
        seen_in_request: set[tuple[tuple[str, str], int, str, int]] = set()
        processed_kinds: dict[tuple[str, str], dict[int, set[str]]] = {}
        for _, account_id, venue_id, event_seq, _, identity, kind, update in pending:
            scope = (account_id, venue_id)
            cursor = cursor_snapshot.get(scope, -1)
            if event_seq < cursor or (
                event_seq == cursor and kind in kinds_snapshot.get(scope, set())
            ):
                continue
            update_key = (scope, event_seq, kind, identity)
            if update_key in seen_in_request:
                continue
            seen_in_request.add(update_key)
            processed_kinds.setdefault(scope, {}).setdefault(event_seq, set()).add(kind)
            if kind in {"order_status", "fill"}:
                instrument = update["order"]["instrument"]
                context = self._context(
                    request,
                    update["ts"],
                    account_id=account_id,
                    venue_id=venue_id,
                    instrument=instrument,
                )
                self._track_callback_error(
                    "order_update" if kind == "order_status" else "fill_update",
                    context,
                    instrument,
                    update["ts"],
                )
                result = (
                    self.strategy.on_order_status_update(context, update)
                    if kind == "order_status"
                    else self.strategy.on_fill_update(context, update)
                )
                decision = self._decision_mapping(result)
                if decision is not None:
                    decisions.append(decision)
                continue

            context = self._context(
                request,
                update["ts"],
                account_id=account_id,
                venue_id=venue_id,
                event_seq=event_seq,
                include_account_state=False,
            )
            if kind == "account_snapshot":
                self._track_callback_error(
                    "account_update", context, None, update["ts"]
                )
                results = (self.strategy.on_account(context, update),)
            elif kind == "position_snapshot":
                self._track_callback_error(
                    "account_update", context, None, update["ts"]
                )
                results = [self.strategy.on_positions_snapshot(context, update)]
                for position in update["positions"]:
                    instrument = BaseStrategy._instrument_key(position["instrument"])
                    position_context = self._context(
                        request,
                        update["ts"],
                        account_id=account_id,
                        venue_id=venue_id,
                        instrument=instrument,
                        event_seq=event_seq,
                        include_account_state=False,
                    )
                    self._track_callback_error(
                        "account_update", position_context, instrument, update["ts"]
                    )
                    results.append(self.strategy.on_position(position_context, position))
            else:
                raise ValueError(f"unsupported StrategyAccountUpdate event: {kind!r}")
            for result in results:
                decision = self._decision_mapping(result)
                if decision is not None:
                    decisions.append(decision)
        for scope, kinds_by_seq in processed_kinds.items():
            previous_cursor = cursor_snapshot.get(scope, -1)
            latest_seq = max(kinds_by_seq)
            latest_kinds = kinds_by_seq[latest_seq]
            if latest_seq > previous_cursor:
                self._update_cursors[scope] = latest_seq
                self._seen_update_kinds_at_cursor[scope] = set(latest_kinds)
            elif latest_seq == previous_cursor:
                self._seen_update_kinds_at_cursor[scope] = (
                    kinds_snapshot.get(scope, set()) | latest_kinds
                )
        return decisions

    def _dispatch_bars(self, request: StrategyInput) -> list[Mapping[str, Any]]:
        frames: dict[str, Any] = {}
        if request.bars is not None:
            frames[request.instrument] = request.bars
        frames.update(request.market_bars)
        events: list[tuple[int, str, Mapping[str, Any]]] = []
        for instrument, frame in frames.items():
            for index, timestamp in enumerate(frame.ts):
                if timestamp <= self._last_bar_ts.get(instrument, 0):
                    continue
                symbol, venue = self._split_instrument(instrument)
                bar = {
                    "instrument": {"symbol": symbol, "venue": venue},
                    "ts": timestamp,
                    "open_raw": frame.open_raw[index],
                    "high_raw": frame.high_raw[index],
                    "low_raw": frame.low_raw[index],
                    "close_raw": frame.close_raw[index],
                    "volume_raw": frame.volume_raw[index],
                }
                if frame.exact_ohlcv is not None:
                    exact_row = frame.exact_ohlcv[index]
                    if exact_row is not None:
                        bar["exact_ohlcv"] = dict(exact_row)
                events.append((timestamp, instrument, {"Bar": bar}))
        if not events:
            return []
        events.sort(key=lambda item: (item[0], item[1]))
        latest_ts = events[-1][0]
        decisions: list[Mapping[str, Any]] = []
        classic_bar_lifecycle = (
            type(self.strategy).on_event is BaseStrategy.on_event
            and type(self.strategy).on_bar is BaseStrategy.on_bar
        )
        for timestamp, instrument, event in events:
            count = self._bars_seen.get(instrument, 0) + 1
            self._bars_seen[instrument] = count
            self._last_bar_ts[instrument] = timestamp
            context = self._context(request, timestamp)
            if self._warmup_bars == 0 and instrument not in self._warmup_completed:
                self._complete_warmup(request, instrument, timestamp, 0)
            if count == self._warmup_bars and self._warmup_bars > 0:
                self._complete_warmup(request, instrument, timestamp, count)
            if classic_bar_lifecycle:
                bar = event["Bar"]
                if count < self._warmup_bars:
                    self._track_callback_error(
                        "market_event", context, instrument, timestamp
                    )
                    result = self.strategy.prenext(context, bar)
                elif count == max(1, self._warmup_bars):
                    self._track_callback_error(
                        "market_event", context, instrument, timestamp
                    )
                    result = self.strategy.nextstart(context, bar)
                else:
                    self._track_callback_error(
                        "market_event", context, instrument, timestamp
                    )
                    result = self.strategy.next(context, bar)
            else:
                self._track_callback_error(
                    "market_event", context, instrument, timestamp
                )
                result = self.strategy.on_event(context, event)
            decision = self._decision_mapping(result)
            if decision is not None and count >= self._warmup_bars and timestamp == latest_ts:
                decisions.append(decision)
        return decisions

    def _dispatch_ticks(self, request: StrategyInput) -> list[Mapping[str, Any]]:
        decisions: list[Mapping[str, Any]] = []
        for index, tick_value in enumerate(request.market_ticks):
            instrument = tick_value["instrument"]
            event_tick = dict(tick_value)
            event_tick["instrument"] = self._instrument_object(instrument)
            event = {"Tick": event_tick}
            context = self._context(request, tick_value["ts"], instrument=instrument)
            self._track_callback_error(
                "market_event", context, instrument, tick_value["ts"]
            )
            result = self.strategy.on_event(context, event)
            decision = self._decision_mapping(result)
            if decision is not None and index == len(request.market_ticks) - 1:
                decisions.append(decision)
        return decisions

    def _dispatch_order_books(self, request: StrategyInput) -> list[Mapping[str, Any]]:
        decisions: list[Mapping[str, Any]] = []
        for index, book_value in enumerate(request.market_order_books):
            instrument = book_value["instrument"]
            payload = dict(book_value)
            payload["instrument"] = self._instrument_object(instrument)
            event = {"OrderBook": payload}
            context = self._context(request, book_value["ts"], instrument=instrument)
            self._track_callback_error(
                "market_event", context, instrument, book_value["ts"]
            )
            decision = self._decision_mapping(self.strategy.on_event(context, event))
            if decision is not None and index == len(request.market_order_books) - 1:
                decisions.append(decision)
        return decisions

    def _dispatch_timer(self, request: StrategyInput) -> list[Mapping[str, Any]]:
        """Deliver one scheduler-triggered Timer event for this committed job request."""
        timer = {"name": "scheduler", "ts": request.as_of}
        event = {"Timer": timer}
        context = self._context(request, request.as_of)
        self._track_callback_error("market_event", context, None, request.as_of)
        decision = self._decision_mapping(self.strategy.on_event(context, event))
        return [] if decision is None else [decision]

    @staticmethod
    def _validate_bars(request: StrategyInput) -> None:
        frames = dict(request.market_bars)
        if request.bars is not None:
            frames[request.instrument] = request.bars
        for instrument, frame in frames.items():
            if frame.ts and frame.ts[-1] > request.as_of:
                raise ValueError(f"strategy bars for {instrument} contain a Bar after as_of")

    def _complete_warmup(
        self,
        request: StrategyInput,
        instrument: str,
        as_of: int,
        observed_bars: int,
        *,
        include_history: bool = True,
    ) -> None:
        if instrument in self._warmup_completed:
            return
        context = self._context(request, as_of, include_history=include_history)
        self._track_callback_error("warmup_complete", context, instrument, as_of)
        self.strategy.on_warmup_complete(
            context,
            self._instrument_object(instrument),
            observed_bars,
        )
        self._warmup_completed.add(instrument)

    def _context(
        self,
        request: StrategyInput,
        as_of: int,
        *,
        account_id: str | None = None,
        venue_id: str | None = None,
        instrument: str | None = None,
        include_history: bool = True,
        include_account_state: bool = True,
        event_seq: int | None = None,
    ) -> dict[str, Any]:
        context_instrument = request.instrument if instrument is None else instrument
        symbol, instrument_venue = self._split_instrument(context_instrument)
        frames: dict[str, Any] = {}
        if include_history:
            frames.update(request.market_bars)
            if request.bars is not None:
                frames[request.instrument] = request.bars
        context = {
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "request_id": request.request_id,
            "account_id": account_id or self.account_id or "runtime",
            "venue_id": venue_id or self.venue_id or instrument_venue,
            "instrument": {"symbol": symbol, "venue": instrument_venue},
            "data_fingerprint": request.data_fingerprint,
            "as_of": as_of,
            "positions": dict(request.positions) if include_account_state else {},
            "positions_exact": dict(request.positions_exact) if include_account_state else {},
            "cash": dict(request.cash) if include_account_state else {},
            "cash_exact": dict(request.cash_exact) if include_account_state else {},
            "available_margin_raw": (
                request.available_margin_raw if include_account_state else None
            ),
            "available_margin_exact": (
                request.available_margin_exact if include_account_state else None
            ),
            "risk_state": request.risk_state if include_account_state else "",
            "research_targets": dict(request.research_targets) if include_account_state else {},
            "data": StrategyDataPortal(frames, as_of),
        }
        if event_seq is not None:
            context["event_seq"] = event_seq
        return context

    @staticmethod
    def _instruments(request: StrategyInput) -> tuple[str, ...]:
        names = set(request.market_bars)
        if request.bars is not None:
            names.add(request.instrument)
        return tuple(sorted(names))

    @staticmethod
    def _instrument_object(instrument: str) -> dict[str, str]:
        symbol, venue = BaseStrategyProcessAdapter._split_instrument(instrument)
        return {"symbol": symbol, "venue": venue}

    @staticmethod
    def _split_instrument(instrument: str) -> tuple[str, str]:
        symbol, separator, venue = instrument.rpartition(".")
        if not separator or not symbol or not venue:
            raise ValueError(f"invalid instrument identity: {instrument}")
        return symbol, venue

    @staticmethod
    def _decision_mapping(value: Any) -> Mapping[str, Any] | None:
        if value is None:
            return None
        if isinstance(value, str):
            value = json.loads(value)
        if not isinstance(value, Mapping):
            raise ValueError("BaseStrategy callbacks must return a decision object or None")
        return value

    def _merge_decisions(
        self,
        request: StrategyInput,
        decisions: list[Mapping[str, Any]],
    ) -> dict[str, Any]:
        intents: list[dict[str, Any]] = []
        intent_ids: set[int] = set()
        last: Mapping[str, Any] | None = None
        for decision in decisions:
            unknown = set(decision) - {
                "schema_version",
                "request_id",
                "strategy_id",
                "signal_id",
                "confidence",
                "priority",
                "expires_at",
                "intents",
            }
            if unknown:
                raise ValueError(
                    "BaseStrategy decision contains unsupported field(s): "
                    + ", ".join(sorted(str(key) for key in unknown))
                )
            for field, expected in (
                ("request_id", request.request_id),
                ("strategy_id", request.strategy_id),
            ):
                supplied = decision.get(field)
                if supplied is not None and supplied != expected:
                    raise ValueError(f"BaseStrategy decision {field} does not match the request")
            current_intents = decision.get("intents", [])
            if not isinstance(current_intents, list):
                raise ValueError("BaseStrategy decision intents must be an array")
            for intent in current_intents:
                if not isinstance(intent, Mapping):
                    raise ValueError("BaseStrategy decision intents must contain objects")
                normalized_intent = self._normalize_intent(intent)
                intent_id = normalized_intent.get("intent_id")
                if isinstance(intent_id, bool) or not isinstance(intent_id, int) or intent_id <= 0:
                    raise ValueError("BaseStrategy intent_id must be a positive integer")
                if intent_id in intent_ids:
                    raise ValueError(f"BaseStrategy emitted duplicate intent_id: {intent_id}")
                intent_ids.add(intent_id)
                intents.append(normalized_intent)
            last = decision
        exact_key = request.instrument
        raw_key = request.instrument
        if self.account_id:
            scoped_key = f"{self.account_id}|{request.instrument}"
            exact_key = scoped_key if scoped_key in request.positions_exact else exact_key
            raw_key = scoped_key if scoped_key in request.positions else raw_key
        current_exact = request.positions_exact.get(exact_key)
        current_raw = request.positions.get(raw_key, 0)
        if intents:
            target_qty = 0
            target_qty_exact = None
        elif current_exact is not None:
            target_qty = 0
            target_qty_exact = current_exact
        else:
            target_qty = current_raw
            target_qty_exact = None
        return {
            "schema_version": 1,
            "request_id": request.request_id,
            "strategy_id": request.strategy_id,
            "signal_id": request.as_of if last is None else last.get("signal_id", request.as_of),
            "instrument": request.instrument,
            "target_qty": target_qty,
            "target_qty_exact": target_qty_exact,
            "confidence": 0 if last is None else last.get("confidence", 0),
            "priority": 0 if last is None else last.get("priority", 0),
            "expires_at": 0 if last is None else last.get("expires_at", 0),
            "intents": intents,
        }

    @classmethod
    def _normalize_intent(cls, intent: Mapping[str, Any]) -> dict[str, Any]:
        """Translate BaseStrategy's native SDK helper fields into process wire fields."""
        allowed = {
            "intent_id",
            "instrument",
            "account_id",
            "side",
            "qty",
            "qty_raw",
            "qty_exact",
            "limit",
            "limit_price_raw",
            "limit_price_exact",
            "quote_budget",
            "quote_budget_exact",
            "policy",
            "reduce_only",
            "post_only",
        }
        unknown = set(intent) - allowed
        if unknown:
            raise ValueError(
                "BaseStrategy intent contains unsupported field(s): "
                + ", ".join(sorted(str(key) for key in unknown))
            )
        instrument = intent.get("instrument")
        if isinstance(instrument, Mapping):
            symbol, venue = instrument.get("symbol"), instrument.get("venue")
            if not isinstance(symbol, str) or not symbol.strip() or not isinstance(venue, str) or not venue.strip():
                raise ValueError("BaseStrategy intent instrument must contain symbol and venue")
            instrument = f"{symbol}.{venue}"
        if not isinstance(instrument, str) or not instrument.strip():
            raise ValueError("BaseStrategy intent instrument must be a non-empty string")

        policy = intent.get("policy")
        if policy is not None and not isinstance(policy, Mapping):
            raise ValueError("BaseStrategy intent policy must be an object or None")
        policy = {} if policy is None else policy
        policy_unknown = set(policy) - {
            "reduce_only",
            "position_side",
            "margin_mode",
            "position_mode",
            "leverage",
            "post_only",
        }
        if policy_unknown:
            raise ValueError(
                "BaseStrategy intent policy contains unsupported field(s): "
                + ", ".join(sorted(str(key) for key in policy_unknown))
            )

        def policy_value(name: str, direct: Any = None) -> Any:
            selected = direct if direct is not None else policy.get(name)
            if direct is not None and name in policy and policy[name] != direct:
                raise ValueError(f"BaseStrategy intent {name} conflicts with policy.{name}")
            return selected

        qty_raw = intent.get("qty_raw", intent.get("qty", 0))
        limit_raw = intent.get("limit_price_raw", intent.get("limit"))
        qty_exact = intent.get("qty_exact")
        limit_exact = intent.get("limit_price_exact")
        quote_budget = intent.get("quote_budget_exact", intent.get("quote_budget"))
        result = {
            "intent_id": intent.get("intent_id"),
            "instrument": instrument,
            "account_id": intent.get("account_id"),
            "side": str(intent.get("side", "")).lower(),
            "qty_raw": qty_raw,
            "qty_exact": cls._decimal_wire(qty_exact, "qty_exact"),
            "quote_budget_exact": cls._decimal_wire(quote_budget, "quote_budget"),
            "limit_price_raw": limit_raw,
            "limit_price_exact": cls._decimal_wire(limit_exact, "limit_price_exact"),
            "reduce_only": policy_value("reduce_only", intent.get("reduce_only")),
            "post_only": policy_value("post_only", intent.get("post_only")),
            "position_side": policy_value("position_side"),
            "margin_mode": policy_value("margin_mode"),
            "position_mode": policy_value("position_mode"),
            "leverage": policy_value("leverage"),
        }
        if result["reduce_only"] is None:
            result["reduce_only"] = False
        if result["post_only"] is None:
            result["post_only"] = False
        for key in ("position_side", "margin_mode", "position_mode"):
            value = result[key]
            if value is not None:
                if not isinstance(value, str):
                    raise ValueError(f"BaseStrategy intent {key} must be a string")
                result[key] = value.strip().lower()
        return result

    @staticmethod
    def _decimal_wire(value: Any, name: str) -> str | None:
        if value is None:
            return None
        if isinstance(value, str):
            return value
        if not isinstance(value, Mapping) or set(value) != {"coefficient", "scale"}:
            raise ValueError(f"BaseStrategy intent {name} must be a decimal string or coefficient/scale")
        coefficient = value["coefficient"]
        scale = value["scale"]
        digits = coefficient[1:] if isinstance(coefficient, str) and coefficient.startswith("-") else coefficient
        if (
            not isinstance(coefficient, str)
            or not coefficient
            or not digits.isdigit()
            or (len(digits) > 1 and digits.startswith("0"))
            or isinstance(scale, bool)
            or not isinstance(scale, int)
            or not 0 <= scale <= 18
        ):
            raise ValueError(f"BaseStrategy intent {name} coefficient/scale is invalid")
        negative = coefficient.startswith("-")
        digits = coefficient[1:] if negative else coefficient
        if scale == 0:
            decimal = digits
        else:
            digits = digits.rjust(scale + 1, "0")
            decimal = f"{digits[:-scale]}.{digits[-scale:]}"
        return f"-{decimal}" if negative else decimal
