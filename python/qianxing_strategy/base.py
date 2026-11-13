"""Optional ergonomic event strategy base for the Rust-backed callback API.

The base class is deliberately only a helper. Strategies may keep implementing
``on_event(context, event)`` directly, and all returned decisions still pass
through the Rust risk, OMS, execution, ledger and replay pipeline.
"""

from __future__ import annotations

import inspect
from bisect import bisect_right
from collections import deque
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from typing import Any, ClassVar, Literal, TypedDict

from .errors import StrategyErrorV1


def _raw_decimal_1e9(value: int) -> Decimal:
    """Build a Decimal at the core raw scale without context rounding."""
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError("raw amount must be an integer")
    if not -(1 << 127) <= value <= (1 << 127) - 1:
        raise ValueError("raw amount is outside i128")
    digits = tuple(int(digit) for digit in str(abs(value)))
    return Decimal((1 if value < 0 else 0, digits, -9))


class StrategyDataPortal:
    """Read-only, point-in-time view over Bar history visible to one callback.

    Frames may be the columnar ``StrategyBars`` wire type or a sequence of Bar
    mappings accumulated by the in-process backtest callback. Results are copied
    mappings, so strategy code cannot mutate the host's frozen input.
    """

    _ROW_FIELDS = frozenset(
        {
            "instrument",
            "ts",
            "open_raw",
            "high_raw",
            "low_raw",
            "close_raw",
            "volume_raw",
            "exact_ohlcv",
        }
    )
    _VALUE_FIELDS = _ROW_FIELDS - {"instrument", "ts"}
    _MAX_HISTORY_ROWS = 10_000

    def __init__(self, frames: Mapping[str, Any], as_of: int) -> None:
        if isinstance(as_of, bool) or not isinstance(as_of, int) or as_of <= 0:
            raise ValueError("StrategyDataPortal as_of must be a positive integer")
        self._frames = frames
        self.as_of = as_of

    @property
    def instruments(self) -> tuple[str, ...]:
        return tuple(sorted(self._frames))

    def history(
        self,
        instrument: str | Mapping[str, Any],
        *,
        length: int,
        fields: Sequence[str] | None = None,
    ) -> tuple[Mapping[str, Any], ...]:
        """Return up to ``length`` latest Bars with ``ts <= as_of``.

        OHLCV values use the core ``*_raw`` scale; ``exact_ohlcv`` is included
        when the frozen source carries lossless decimal values. The count is
        explicit and bounded to keep accidental full-history copies predictable.
        """
        if (
            isinstance(length, bool)
            or not isinstance(length, int)
            or not 1 <= length <= self._MAX_HISTORY_ROWS
        ):
            raise ValueError(
                f"history length must be an integer in 1..={self._MAX_HISTORY_ROWS}"
            )
        key = BaseStrategy._instrument_key(instrument)
        frame = self._frames.get(key)
        if frame is None:
            return ()
        selected_fields = self._fields(fields)
        if all(
            hasattr(frame, name)
            for name in (
                "ts",
                "open_raw",
                "high_raw",
                "low_raw",
                "close_raw",
                "volume_raw",
            )
        ):
            timestamps = frame.ts
            end = bisect_right(timestamps, self.as_of)
            start = max(0, end - length)
            rows = []
            for index in range(start, end):
                row: dict[str, Any] = {
                    "instrument": self._instrument_object(key),
                    "ts": timestamps[index],
                }
                for field in selected_fields:
                    if field == "exact_ohlcv":
                        exact_rows = getattr(frame, "exact_ohlcv", None)
                        exact = None if exact_rows is None else exact_rows[index]
                        row[field] = None if exact is None else dict(exact)
                    else:
                        row[field] = getattr(frame, field)[index]
                rows.append(row)
        else:
            if not isinstance(frame, (Sequence, deque)) or isinstance(
                frame, (str, bytes, bytearray)
            ):
                raise TypeError(
                    f"history frame for {key} must be StrategyBars or a Bar sequence"
                )
            visible: list[Mapping[str, Any]] = []
            for source in reversed(frame):
                if isinstance(source, Mapping) and self._visible(source):
                    visible.append(source)
                    if len(visible) == length:
                        break
            rows = []
            for source in reversed(visible):
                source_instrument = source.get("instrument")
                if isinstance(source_instrument, Mapping):
                    instrument_value = dict(source_instrument)
                elif isinstance(source_instrument, str):
                    instrument_value = self._instrument_object(
                        BaseStrategy._instrument_key(source_instrument)
                    )
                else:
                    instrument_value = self._instrument_object(key)
                row = {"instrument": instrument_value, "ts": source["ts"]}
                for field in selected_fields:
                    value = source.get(field)
                    row[field] = (
                        dict(value)
                        if field == "exact_ohlcv" and isinstance(value, Mapping)
                        else value
                    )
                rows.append(row)
        return tuple(rows)

    def current(
        self,
        instrument: str | Mapping[str, Any],
        *,
        field: str | None = None,
    ) -> Any:
        """Return the latest visible Bar, or one raw/exact field, if available."""
        if field is not None and field not in self._ROW_FIELDS:
            raise ValueError(f"unsupported Bar field: {field}")
        rows = self.history(
            instrument,
            length=1,
            fields=None if field is None or field in {"instrument", "ts"} else (field,),
        )
        if not rows:
            return None
        return rows[-1] if field is None else rows[-1].get(field)

    def history_decimal(
        self,
        instrument: str | Mapping[str, Any],
        *,
        length: int,
        field: Literal["open", "high", "low", "close", "volume"],
    ) -> tuple[Decimal, ...]:
        """Return exact OHLCV history as ``Decimal`` values.

        Source ``exact_ohlcv`` values take precedence. Raw-only data is decoded
        at the core's 1e-9 scale without passing through binary floating point.
        """
        if not isinstance(field, str) or field not in {
            "open",
            "high",
            "low",
            "close",
            "volume",
        }:
            raise ValueError(f"unsupported exact OHLCV field: {field}")
        rows = self.history(
            instrument,
            length=length,
            fields=(f"{field}_raw", "exact_ohlcv"),
        )
        return tuple(BaseStrategy.bar_decimal(row, field) for row in rows)

    def current_decimal(
        self,
        instrument: str | Mapping[str, Any],
        *,
        field: Literal["open", "high", "low", "close", "volume"],
    ) -> Decimal | None:
        """Return one exact current OHLCV value, if a Bar is visible."""
        values = self.history_decimal(instrument, length=1, field=field)
        return None if not values else values[-1]

    @classmethod
    def _fields(cls, fields: Sequence[str] | None) -> tuple[str, ...]:
        if fields is None:
            return tuple(sorted(cls._VALUE_FIELDS))
        if isinstance(fields, (str, bytes, bytearray)) or not isinstance(
            fields, Sequence
        ):
            raise TypeError("history fields must be a sequence of Bar field names")
        selected = tuple(fields)
        if any(
            not isinstance(field, str) or field not in cls._VALUE_FIELDS
            for field in selected
        ):
            raise ValueError("history fields contain an unsupported Bar field")
        if len(set(selected)) != len(selected):
            raise ValueError("history fields must not contain duplicates")
        return selected

    def _visible(self, row: Mapping[str, Any]) -> bool:
        timestamp = row.get("ts")
        return (
            isinstance(timestamp, int)
            and not isinstance(timestamp, bool)
            and timestamp <= self.as_of
        )

    @staticmethod
    def _instrument_object(key: str) -> dict[str, str]:
        symbol, separator, venue = key.rpartition(".")
        if not separator or not symbol or not venue:
            raise ValueError(f"invalid instrument identity: {key}")
        return {"symbol": symbol, "venue": venue}


class _StrategyOrderUpdateRequired(TypedDict):
    """Stable order identity and scale-aware amounts in a strategy update."""

    order_id: int
    account_id: str
    instrument: str
    side: Literal["Buy", "Sell"]
    qty_raw: int
    filled_raw: int
    strategy_id: str


class StrategyOrderUpdate(_StrategyOrderUpdateRequired, total=False):
    exact_amounts: Mapping[str, Any]


class StrategyUpdate(TypedDict):
    """Versioned execution fact delivered after the host commits a fill."""

    schema_version: Literal[1]
    event: Literal["fill"]
    order: StrategyOrderUpdate
    fill: Mapping[str, Any]


class StrategyFillUpdateV2(TypedDict):
    """Committed fill fact bound to its authoritative EventLog sequence."""

    schema_version: Literal[2]
    event: Literal["fill"]
    event_seq: int
    ts: int
    order: StrategyOrderUpdate
    fill: Mapping[str, Any]


class StrategyOrderStatusUpdate(TypedDict):
    """Versioned order-state fact delivered after the OMS/EventLog commits it."""

    schema_version: Literal[1]
    event: Literal["order_status"]
    event_seq: int
    ts: int
    status: Literal[
        "pending_submit",
        "submitted",
        "accepted",
        "working",
        "partially_filled",
        "filled",
        "cancel_pending",
        "cancelled",
        "rejected",
        "expired",
        "unknown",
    ]
    order: StrategyOrderUpdate
    reason: str | None


@dataclass(frozen=True)
class StrategyWarmupBinding:
    """Internal binding that carries manifest warmup into a callback."""

    strategy: Any
    warmup_bars: int
    history_requirements_checked: bool = True


def declared_warmup_bars(strategy: Any) -> int | None:
    """Read an optional strategy declaration without confusing it with a manifest default."""
    provider = getattr(strategy, "history_requirements", None)
    if provider is None:
        return None
    if not callable(provider):
        raise TypeError("history_requirements must be callable")
    requirements = provider()
    if requirements is None:
        return None
    if not isinstance(requirements, Mapping) or set(requirements) != {"warmup_bars"}:
        raise ValueError("history_requirements must return None or {'warmup_bars': non-negative integer}")
    warmup_bars = requirements["warmup_bars"]
    if (
        isinstance(warmup_bars, bool)
        or not isinstance(warmup_bars, int)
        or warmup_bars < 0
        or warmup_bars > (1 << 64) - 1
    ):
        raise ValueError("history_requirements.warmup_bars must be a u64")
    return warmup_bars


def bind_strategy_warmup(
    strategy: Any,
    warmup_bars: int,
    *,
    defer_history_requirements: bool = False,
) -> StrategyWarmupBinding:
    """Bind a manifest warmup, optionally deferring user hook validation to callback init."""
    if (
        isinstance(warmup_bars, bool)
        or not isinstance(warmup_bars, int)
        or warmup_bars < 0
        or warmup_bars > (1 << 64) - 1
    ):
        raise ValueError("manifest warmup_bars must be a u64")
    if defer_history_requirements:
        return StrategyWarmupBinding(strategy, warmup_bars, False)
    declared = declared_warmup_bars(strategy)
    if declared is not None and declared != warmup_bars:
        raise ValueError(
            "strategy history_requirements.warmup_bars conflicts with STRATEGY_MANIFEST.warmup_bars"
        )
    return StrategyWarmupBinding(strategy, warmup_bars, True)


def _dispatch_on_stop(strategy: Any, context: Mapping[str, Any], reason: str) -> None:
    """Call contextual stop hooks while preserving legacy no-argument strategies."""
    handler = getattr(strategy, "on_stop", None)
    if not callable(handler):
        return
    values = {"context": context, "reason": reason}
    signature = inspect.signature(handler)
    args: list[Any] = []
    kwargs: dict[str, Any] = {}
    for parameter in signature.parameters.values():
        if parameter.name not in values:
            if parameter.kind is inspect.Parameter.VAR_KEYWORD:
                kwargs.update(values)
            continue
        if parameter.kind is inspect.Parameter.POSITIONAL_ONLY:
            args.append(values[parameter.name])
        elif parameter.kind in {
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        }:
            kwargs[parameter.name] = values[parameter.name]
    try:
        signature.bind(*args, **kwargs)
    except TypeError:
        # Existing subclasses commonly override ``on_stop(self)``.
        signature.bind()
        handler()
    else:
        handler(*args, **kwargs)


def _dispatch_on_error(
    strategy: Any,
    context: Mapping[str, Any],
    error: Exception,
    error_v1: StrategyErrorV1 | None = None,
) -> None:
    """Call normalized and legacy error hooks; callers preserve the original failure."""
    hook_error: Exception | None = None
    standardized = getattr(strategy, "on_error_v1", None)
    if error_v1 is not None and callable(standardized):
        try:
            standardized(error_v1)
        except Exception as caught:  # noqa: BLE001 - diagnostic hook failures are secondary.
            hook_error = caught
    handler = getattr(strategy, "on_error", None)
    if callable(handler):
        try:
            handler(context, error)
        except Exception as caught:  # noqa: BLE001 - still run the v1 observer independently.
            if hook_error is None:
                hook_error = caught
    if hook_error is not None:
        raise hook_error


class BaseStrategy:
    """Dispatch shared market events to typed Python hooks.

    Override ``on_bar``, ``on_tick``, ``on_order_book`` or ``on_timer``. Bar-only
    strategies may instead use Backtrader-style ``prenext``, ``nextstart`` and
    ``next`` hooks; ``nextstart`` defaults to ``next`` on the first ready Bar.
    Hooks may return a ``StrategyDecision`` mapping/string or ``None``. ``None`` means
    an explicit no-intent decision for market events. ``on_order_update`` may
    return ``None`` to indicate that no follow-up decision is queued.

    ``on_init`` and ``on_start`` receive an immutable-context-shaped dictionary;
    ``history_requirements`` may declare ``warmup_bars``. For a requirement of N
    Bars, the first N-1 Bars update state with their order intents suppressed;
    when the Nth Bar becomes visible, ``on_warmup_complete`` runs before that
    Bar's event hook, whose decision is eligible for admission. A zero-Bar
    warmup notification runs from ``on_start``. ``on_order_update`` dispatches
    committed fills to ``on_trade`` and committed order transitions to
    ``on_order``. ``on_error(context, error)`` is called when a local callback
    request fails and by the Runtime worker before strategy state is rolled back;
    errors from that hook do not replace the original request failure. ``on_stop`` may accept optional
    ``context`` and ``reason`` parameters; legacy no-argument overrides remain
    supported. An orderly Runtime worker EOF reports ``reason="shutdown"``;
    direct adapter closure defaults to ``host_stop``. The class does not
    require calling ``super``. File-based subclasses may be exported as ``STRATEGY_CLASS`` and
    declare the same versioned ``STRATEGY_MANIFEST`` on the class; an optional
    module- or class-level ``validate_config(config)`` is applied by the shared
    strategy-file preflight and search path.
    """

    _EVENT_HOOKS: ClassVar[dict[str, str]] = {
        "Bar": "on_bar",
        "Tick": "on_tick",
        "OrderBook": "on_order_book",
        "Timer": "on_timer",
    }

    def on_init(self, context: Mapping[str, Any]) -> None:
        """Called once when a fresh strategy instance enters a run."""

    def on_start(self, context: Mapping[str, Any]) -> None:
        """Called immediately before the first event, after host setup."""

    def history_requirements(self) -> Mapping[str, int] | None:
        """Declare callback history needs; a file manifest remains the run-level authority."""
        return None

    def on_warmup_complete(
        self,
        context: Mapping[str, Any],
        instrument: Any | None,
        observed_bars: int,
    ) -> None:
        """Called before the Bar hook that reaches the declared warmup count."""

    def prenext(
        self, context: Mapping[str, Any], bar: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Update state for a Bar before the declared warmup is complete.

        Returned decisions are ignored during warmup. Override this together
        with ``next`` when using the Backtrader-style Bar lifecycle.
        """
        return None

    def nextstart(
        self, context: Mapping[str, Any], bar: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Handle the first Bar that is eligible to produce an intent."""
        return self.next(context, bar)

    def next(
        self, context: Mapping[str, Any], bar: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Handle a ready Bar after warmup; override for classic Bar strategies."""
        return None

    def on_event(
        self,
        context: Mapping[str, Any],
        event: Mapping[str, Any],
    ) -> Mapping[str, Any] | str:
        if not isinstance(event, Mapping) or len(event) != 1:
            raise ValueError("MarketEvent must be a single tagged object")
        kind, payload = next(iter(event.items()))
        hook_name = self._EVENT_HOOKS.get(kind)
        if hook_name is None or not isinstance(payload, Mapping):
            raise ValueError(f"unsupported or malformed MarketEvent: {kind!r}")
        if kind == "Timer":
            timer_name = payload.get("name")
            timestamp = payload.get("ts")
            if not isinstance(timer_name, str) or not timer_name.strip():
                raise ValueError("Timer MarketEvent name must be a non-empty string")
            if (
                isinstance(timestamp, bool)
                or not isinstance(timestamp, int)
                or timestamp <= 0
                or timestamp > (1 << 64) - 1
            ):
                raise ValueError("Timer MarketEvent.ts must be a positive u64 integer")
        hook = getattr(self, hook_name)
        decision = hook(context, payload)
        if decision is None:
            return self.empty_decision(context, kind, payload)
        if isinstance(decision, (Mapping, str)):
            return decision
        raise TypeError(f"{hook_name} must return a decision mapping/string or None")

    def on_bar(
        self, context: Mapping[str, Any], bar: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Handle one Bar; the default emits an empty decision."""
        return None

    def on_tick(
        self, context: Mapping[str, Any], tick: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Handle one L1 Tick; the default emits an empty decision."""
        return None

    def on_order_book(
        self, context: Mapping[str, Any], book: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Handle one sequenced order-book snapshot."""
        return None

    def on_timer(
        self, context: Mapping[str, Any], timer: Mapping[str, Any]
    ) -> Mapping[str, Any] | str | None:
        """Handle one engine-time timer event."""
        return None

    def on_order_update(
        self,
        context: Mapping[str, Any],
        update: StrategyUpdate,
    ) -> Mapping[str, Any] | str | None:
        """Handle a committed fill fact.

        This remains fill-only for compatibility with existing callback code.
        """
        if update.get("event") != "fill":
            raise ValueError(f"unsupported StrategyUpdate event: {update.get('event')!r}")
        trade = update.get("fill")
        if not isinstance(trade, Mapping):
            raise ValueError("fill StrategyUpdate must contain a fill mapping")
        return self.on_trade(context, trade)

    def on_fill_update(
        self,
        context: Mapping[str, Any],
        update: StrategyFillUpdateV2,
    ) -> Mapping[str, Any] | str | None:
        """Handle a committed fill with its EventLog sequence.

        The default projects to the legacy v1 update before dispatching, so
        subclasses that only override ``on_order_update`` or ``on_trade`` keep
        their established callback shape. New strategies can override this hook
        to persist or deduplicate fills by ``event_seq``.
        """
        legacy: StrategyUpdate = {
            "schema_version": 1,
            "event": "fill",
            "order": update["order"],
            "fill": update["fill"],
        }
        return self.on_order_update(context, legacy)

    def on_order_status_update(
        self,
        context: Mapping[str, Any],
        update: StrategyOrderStatusUpdate,
    ) -> Mapping[str, Any] | str | None:
        """Handle one committed OMS order-state transition."""
        self._validate_order_status_update(update)
        return self.on_order(context, update)

    def on_order(
        self,
        context: Mapping[str, Any],
        update: StrategyOrderStatusUpdate,
    ) -> Mapping[str, Any] | str | None:
        """Handle one committed order status transition."""
        return None

    def on_account(
        self,
        context: Mapping[str, Any],
        update: Mapping[str, Any],
    ) -> Mapping[str, Any] | str | None:
        """Handle one complete exchange-reported balance snapshot.

        ``update["balances"]`` is the authoritative observation for this callback,
        including an empty list. The observation does not mutate the host ledger.
        Account values in ``context`` are omitted for this callback so a delayed
        snapshot cannot expose newer account state.
        """
        return None

    def on_position(
        self,
        context: Mapping[str, Any],
        position: Mapping[str, Any],
    ) -> Mapping[str, Any] | str | None:
        """Handle one row from a complete exchange-reported position snapshot."""
        return None

    def on_positions_snapshot(
        self,
        context: Mapping[str, Any],
        update: Mapping[str, Any],
    ) -> Mapping[str, Any] | str | None:
        """Handle a complete position set, including an empty set.

        The snapshot hook runs once before the per-position ``on_position`` hooks,
        allowing strategies to clear cached positions when the set is empty.
        """
        return None

    def on_trade(
        self,
        context: Mapping[str, Any],
        trade: Mapping[str, Any],
    ) -> Mapping[str, Any] | str | None:
        """Handle one fill after the host has committed it to OMS and ledger."""
        return None

    def diagnostic_state(self) -> dict[str, Any] | None:
        """Return a compact, deterministic JSON snapshot for strategy-bias diagnostics.

        The callback diagnostic worker accepts at most 64 KiB. This read-only
        snapshot is not a persistence or restart checkpoint.
        """
        return None

    def snapshot_state(self) -> dict[str, Any]:
        """Return JSON-compatible user state for durable Runtime worker recovery.

        The default snapshots instance attributes. Override this together with
        ``restore_state`` when attributes contain non-JSON objects or should not
        be persisted. The host enforces a 64 KiB limit and commits the snapshot
        with the corresponding execution cursor.
        """
        return dict(vars(self))

    def restore_state(self, state: Mapping[str, Any]) -> None:
        """Restore a prior ``snapshot_state`` result before replaying a request."""
        if not isinstance(state, Mapping) or any(not isinstance(key, str) for key in state):
            raise ValueError("strategy checkpoint state must be a string-keyed object")
        self.__dict__.clear()
        self.__dict__.update(state)

    def on_restore(self, context: Mapping[str, Any]) -> None:
        """Called once after a worker process restores a committed checkpoint."""

    def on_error(
        self,
        context: Mapping[str, Any],
        error: Exception,
    ) -> None:
        """Observe a failed strategy worker request before its state is rolled back.

        The host keeps the original request error as the worker response even if
        this hook also raises. A retry may use a newly constructed strategy
        instance, so use this hook for diagnosis/cleanup rather than durable state.
        """

    def on_error_v1(self, error: StrategyErrorV1) -> None:
        """Observe the shared, versioned error DTO while keeping ``on_error`` compatible."""

    @staticmethod
    def _validate_order_status_update(update: Mapping[str, Any]) -> None:
        statuses = {
            "pending_submit",
            "submitted",
            "accepted",
            "working",
            "partially_filled",
            "filled",
            "cancel_pending",
            "cancelled",
            "rejected",
            "expired",
            "unknown",
        }
        if update.get("schema_version") != 1:
            raise ValueError("unsupported StrategyOrderStatusUpdate schema_version")
        if (
            isinstance(update.get("event_seq"), bool)
            or not isinstance(update.get("event_seq"), int)
            or update["event_seq"] < 0
            or isinstance(update.get("ts"), bool)
            or not isinstance(update.get("ts"), int)
            or update["ts"] <= 0
        ):
            raise ValueError("order_status update event_seq must be non-negative and ts positive")
        if update.get("status") not in statuses:
            raise ValueError("order_status update has an unsupported status")
        if not isinstance(update.get("order"), Mapping):
            raise ValueError("order_status update must contain an order mapping")
        reason = update.get("reason")
        if reason is not None and (
            not isinstance(reason, str) or len(reason.encode("utf-8")) > 2048
        ):
            raise ValueError("order_status update reason must be at most 2048 UTF-8 bytes")

    def on_stop(
        self,
        context: Mapping[str, Any] | None = None,
        reason: str = "host_stop",
    ) -> None:
        """Called once when the host stops the run; runtime worker EOF uses host_stop."""

    def decision(
        self,
        context: Mapping[str, Any],
        event_kind: str,
        event: Mapping[str, Any],
        *,
        intents: Sequence[Mapping[str, Any]] = (),
        confidence: int = 0,
        priority: int = 0,
        expires_at: int = 0,
        request_id: str | None = None,
        signal_id: int | None = None,
    ) -> dict[str, Any]:
        """Build a protocol decision; Rust performs canonical validation."""
        strategy_id = context.get("strategy_id")
        if not isinstance(strategy_id, str) or not strategy_id.strip():
            raise ValueError("StrategyContext.strategy_id is required")
        timestamp = event.get("ts")
        if isinstance(timestamp, bool) or not isinstance(timestamp, int) or timestamp <= 0:
            raise ValueError("MarketEvent.ts must be a positive integer")
        sequence = getattr(self, "_qx_event_sequence", 0) + 1
        self._qx_event_sequence = sequence
        return {
            "schema_version": 1,
            "request_id": request_id or context.get("request_id") or (
                f"{strategy_id}:py:{event_kind}:{timestamp}:{sequence}"
            ),
            "strategy_id": strategy_id,
            "signal_id": timestamp if signal_id is None else signal_id,
            "confidence": confidence,
            "priority": priority,
            "expires_at": expires_at,
            "intents": [dict(intent) for intent in intents],
        }

    def empty_decision(
        self,
        context: Mapping[str, Any],
        event_kind: str,
        event: Mapping[str, Any],
    ) -> dict[str, Any]:
        """Return a deterministic no-intent decision for an unimplemented hook."""
        return self.decision(context, event_kind, event)

    def next_intent_id(self) -> int:
        """Return a deterministic, strategy-instance-local intent identifier."""
        sequence = getattr(self, "_qx_intent_sequence", 0) + 1
        if sequence > (1 << 64) - 1:
            raise OverflowError("strategy intent sequence exceeds u64")
        self._qx_intent_sequence = sequence
        return sequence

    def buy(
        self,
        instrument: str | Mapping[str, Any],
        *,
        qty_raw: int | None = None,
        qty_exact: str | None = None,
        limit_price_raw: int | None = None,
        limit_price_exact: str | None = None,
        quote_budget: str | None = None,
        policy: Mapping[str, Any] | None = None,
        reduce_only: bool = False,
        post_only: bool = False,
        intent_id: int | None = None,
    ) -> dict[str, Any]:
        """Build a buy intent for the current decision; it does not submit it."""
        return self.order_intent(
            intent_id=self.next_intent_id() if intent_id is None else intent_id,
            instrument=instrument,
            side="buy",
            qty_raw=qty_raw,
            qty_exact=qty_exact,
            limit_price_raw=limit_price_raw,
            limit_price_exact=limit_price_exact,
            quote_budget=quote_budget,
            policy=policy,
            reduce_only=reduce_only,
            post_only=post_only,
        )

    def sell(
        self,
        instrument: str | Mapping[str, Any],
        *,
        qty_raw: int | None = None,
        qty_exact: str | None = None,
        limit_price_raw: int | None = None,
        limit_price_exact: str | None = None,
        policy: Mapping[str, Any] | None = None,
        reduce_only: bool = False,
        post_only: bool = False,
        intent_id: int | None = None,
    ) -> dict[str, Any]:
        """Build a sell intent for the current decision; it does not submit it."""
        return self.order_intent(
            intent_id=self.next_intent_id() if intent_id is None else intent_id,
            instrument=instrument,
            side="sell",
            qty_raw=qty_raw,
            qty_exact=qty_exact,
            limit_price_raw=limit_price_raw,
            limit_price_exact=limit_price_exact,
            policy=policy,
            reduce_only=reduce_only,
            post_only=post_only,
        )

    def short(
        self,
        instrument: str | Mapping[str, Any],
        *,
        qty_raw: int | None = None,
        qty_exact: str | None = None,
        limit_price_raw: int | None = None,
        limit_price_exact: str | None = None,
        policy: Mapping[str, Any] | None = None,
        post_only: bool = False,
        intent_id: int | None = None,
    ) -> dict[str, Any]:
        """Build a sell-side short-entry intent; product capability stays in Rust.

        Hedge-mode strategies must include ``position_mode=hedge`` and
        ``position_side=short`` in ``policy``. Spot sells remain limited to
        available inventory; opening shorts require a supported derivative
        product and pass the execution core's capability and risk checks.
        """
        return self.sell(
            instrument,
            qty_raw=qty_raw,
            qty_exact=qty_exact,
            limit_price_raw=limit_price_raw,
            limit_price_exact=limit_price_exact,
            policy=policy,
            post_only=post_only,
            intent_id=intent_id,
        )

    def cover(
        self,
        instrument: str | Mapping[str, Any],
        *,
        qty_raw: int | None = None,
        qty_exact: str | None = None,
        limit_price_raw: int | None = None,
        limit_price_exact: str | None = None,
        policy: Mapping[str, Any] | None = None,
        post_only: bool = False,
        intent_id: int | None = None,
    ) -> dict[str, Any]:
        """Build a reduce-only buy intent to cover a short position.

        Hedge-mode strategies must include ``position_mode=hedge`` and
        ``position_side=short`` in ``policy`` so the venue closes the short leg.
        """
        return self.buy(
            instrument,
            qty_raw=qty_raw,
            qty_exact=qty_exact,
            limit_price_raw=limit_price_raw,
            limit_price_exact=limit_price_exact,
            policy=policy,
            reduce_only=True,
            post_only=post_only,
            intent_id=intent_id,
        )

    @staticmethod
    def position_qty(
        context: Mapping[str, Any], instrument: str | Mapping[str, Any]
    ) -> Decimal:
        """Return the signed current position, preferring its lossless decimal value.

        Raw-only position snapshots use the core 1e-9 quantity scale. A route-scoped
        ``account_id|SYMBOL.VENUE`` key is preferred when present.
        """
        key = BaseStrategy._instrument_key(instrument)
        account_id = context.get("account_id")
        scoped_key = f"{account_id}|{key}" if isinstance(account_id, str) and account_id else None
        exact = context.get("positions_exact", {})
        raw = context.get("positions", {})
        if not isinstance(exact, Mapping) or not isinstance(raw, Mapping):
            raise ValueError("StrategyContext positions must be mappings")
        selected_key = scoped_key if scoped_key and (scoped_key in exact or scoped_key in raw) else key
        exact_value = exact.get(selected_key)
        if exact_value is not None:
            if isinstance(exact_value, Mapping):
                quantity = BaseStrategy._exact_wire_decimal(
                    exact_value, "StrategyContext exact position"
                )
            elif isinstance(exact_value, (str, Decimal)):
                try:
                    quantity = Decimal(exact_value)
                except InvalidOperation as error:
                    raise ValueError(
                        "StrategyContext exact position is not a decimal"
                    ) from error
            else:
                raise ValueError(
                    "StrategyContext exact position must be a decimal string or ExactDecimal"
                )
            if not quantity.is_finite():
                raise ValueError("StrategyContext exact position must be finite")
            return quantity
        raw_value = raw.get(selected_key, 0)
        if isinstance(raw_value, bool) or not isinstance(raw_value, int):
            raise ValueError("StrategyContext raw position must be an integer")
        return _raw_decimal_1e9(raw_value)

    @staticmethod
    def cash_balance(context: Mapping[str, Any], currency: str) -> Decimal:
        """Return a currency cash balance, preferring its scale-aware StrategyInput v9 value."""
        exact = context.get("cash_exact", {})
        raw = context.get("cash", {})
        if not isinstance(exact, Mapping) or not isinstance(raw, Mapping):
            raise ValueError("StrategyContext cash balances must be mappings")
        value = exact.get(currency)
        if value is not None:
            try:
                amount = Decimal(value)
            except (InvalidOperation, TypeError) as error:
                raise ValueError("StrategyContext exact cash balance is not a decimal") from error
            if not amount.is_finite():
                raise ValueError("StrategyContext exact cash balance must be finite")
            return amount
        raw_value = raw.get(currency, 0)
        if isinstance(raw_value, bool) or not isinstance(raw_value, int):
            raise ValueError("StrategyContext raw cash balance must be an integer")
        return _raw_decimal_1e9(raw_value)

    @staticmethod
    def available_margin(context: Mapping[str, Any]) -> Decimal | None:
        """Return available buying power, preferring its exact v9 sidecar when available."""
        exact = context.get("available_margin_exact")
        if exact is not None:
            try:
                amount = Decimal(exact)
            except (InvalidOperation, TypeError) as error:
                raise ValueError("StrategyContext exact available margin is not a decimal") from error
            if not amount.is_finite() or amount < 0:
                raise ValueError("StrategyContext exact available margin must be finite and non-negative")
            return amount
        raw = context.get("available_margin_raw")
        if raw is None:
            return None
        if isinstance(raw, bool) or not isinstance(raw, int) or raw < 0:
            raise ValueError("StrategyContext available_margin_raw must be a non-negative integer")
        return _raw_decimal_1e9(raw)

    @staticmethod
    def history(
        context: Mapping[str, Any],
        instrument: str | Mapping[str, Any] | None = None,
        *,
        length: int,
        fields: Sequence[str] | None = None,
    ) -> tuple[Mapping[str, Any], ...]:
        """Read the latest point-in-time Bars from the callback context.

        ``length`` is required so strategies make the amount of history they
        consume explicit. Use ``history_requirements`` separately to declare
        how many initial Bars must be suppressed during warmup.
        """
        selected = instrument if instrument is not None else context.get("instrument")
        if not isinstance(selected, (str, Mapping)):
            raise ValueError(
                "StrategyContext must identify an instrument for history()"
            )
        data = context.get("data")
        if not isinstance(data, StrategyDataPortal):
            raise ValueError("StrategyContext does not provide the Bar data portal")
        return data.history(selected, length=length, fields=fields)

    @staticmethod
    def current(
        context: Mapping[str, Any],
        instrument: str | Mapping[str, Any] | None = None,
        *,
        field: str | None = None,
    ) -> Any:
        """Read the latest point-in-time Bar or one of its raw/exact fields."""
        selected = instrument if instrument is not None else context.get("instrument")
        if not isinstance(selected, (str, Mapping)):
            raise ValueError(
                "StrategyContext must identify an instrument for current()"
            )
        data = context.get("data")
        if not isinstance(data, StrategyDataPortal):
            raise ValueError("StrategyContext does not provide the Bar data portal")
        return data.current(selected, field=field)

    @staticmethod
    def bar_decimal(
        bar: Mapping[str, Any],
        field: Literal["open", "high", "low", "close", "volume"],
    ) -> Decimal:
        """Decode one Bar OHLCV field exactly, preferring the source sidecar.

        ``field`` uses the familiar OHLCV name. If no exact source value is
        available, the corresponding ``*_raw`` integer is interpreted at the
        core's 1e-9 scale. Neither path accepts or creates a float.
        """
        if not isinstance(bar, Mapping):
            raise TypeError("bar must be a mapping")
        if not isinstance(field, str) or field not in {
            "open",
            "high",
            "low",
            "close",
            "volume",
        }:
            raise ValueError(f"unsupported exact OHLCV field: {field}")
        exact = bar.get("exact_ohlcv")
        if exact is not None:
            if not isinstance(exact, Mapping) or field not in exact:
                raise ValueError(f"bar exact_ohlcv is missing {field}")
            return BaseStrategy._exact_wire_decimal(exact[field], f"{field}")
        raw = bar.get(f"{field}_raw")
        if isinstance(raw, bool) or not isinstance(raw, int):
            raise ValueError(f"bar {field}_raw must be an integer")
        return _raw_decimal_1e9(raw)

    @staticmethod
    def history_decimal(
        context: Mapping[str, Any],
        instrument: str | Mapping[str, Any] | None = None,
        *,
        length: int,
        field: Literal["open", "high", "low", "close", "volume"],
    ) -> tuple[Decimal, ...]:
        """Read exact point-in-time OHLCV values from the callback context."""
        selected = instrument if instrument is not None else context.get("instrument")
        if not isinstance(selected, (str, Mapping)):
            raise ValueError(
                "StrategyContext must identify an instrument for history_decimal()"
            )
        data = context.get("data")
        if not isinstance(data, StrategyDataPortal):
            raise ValueError("StrategyContext does not provide the Bar data portal")
        return data.history_decimal(selected, length=length, field=field)

    @staticmethod
    def current_decimal(
        context: Mapping[str, Any],
        instrument: str | Mapping[str, Any] | None = None,
        *,
        field: Literal["open", "high", "low", "close", "volume"],
    ) -> Decimal | None:
        """Read one exact point-in-time OHLCV value from callback context."""
        selected = instrument if instrument is not None else context.get("instrument")
        if not isinstance(selected, (str, Mapping)):
            raise ValueError(
                "StrategyContext must identify an instrument for current_decimal()"
            )
        data = context.get("data")
        if not isinstance(data, StrategyDataPortal):
            raise ValueError("StrategyContext does not provide the Bar data portal")
        return data.current_decimal(selected, field=field)

    @staticmethod
    def _exact_wire_decimal(value: Any, name: str) -> Decimal:
        if not isinstance(value, Mapping) or set(value) != {"coefficient", "scale"}:
            raise ValueError(f"{name} must contain coefficient and scale")
        coefficient_text = value["coefficient"]
        scale = value["scale"]
        digits_text = (
            coefficient_text[1:]
            if isinstance(coefficient_text, str) and coefficient_text.startswith("-")
            else coefficient_text
        )
        if (
            not isinstance(coefficient_text, str)
            or not coefficient_text
            or len(coefficient_text) > 40
            or coefficient_text == "-0"
            or (coefficient_text.startswith("0") and len(coefficient_text) > 1)
            or (coefficient_text.startswith("-0") and len(coefficient_text) > 2)
            or not digits_text.isascii()
            or not digits_text.isdigit()
            or isinstance(scale, bool)
            or not isinstance(scale, int)
            or not 0 <= scale <= 18
        ):
            raise ValueError(f"{name} is not a supported ExactDecimal")
        coefficient = int(coefficient_text)
        if not -(1 << 127) <= coefficient <= (1 << 127) - 1:
            raise ValueError(f"{name} coefficient is outside i128")
        if (coefficient == 0 and scale != 0) or (scale > 0 and coefficient % 10 == 0):
            raise ValueError(f"{name} must use normalized ExactDecimal encoding")
        digits = tuple(int(digit) for digit in str(abs(coefficient)))
        return Decimal((1 if coefficient < 0 else 0, digits, -scale))

    def close_position(
        self,
        context: Mapping[str, Any],
        instrument: str | Mapping[str, Any],
        *,
        policy: Mapping[str, Any] | None = None,
        intent_id: int | None = None,
    ) -> dict[str, Any] | None:
        """Build a reduce-only intent for a nonzero net position, or return None.

        Hedge-mode long/short legs need explicit venue position-side selection and
        must be closed with ``buy``/``sell`` plus an explicit policy instead.
        """
        quantity = self.position_qty(context, instrument)
        if quantity == 0:
            return None
        exact_quantity = format(abs(quantity), "f")
        if quantity > 0:
            return self.sell(
                instrument,
                qty_exact=exact_quantity,
                policy=policy,
                reduce_only=True,
                intent_id=intent_id,
            )
        return self.buy(
            instrument,
            qty_exact=exact_quantity,
            policy=policy,
            reduce_only=True,
            intent_id=intent_id,
        )

    @staticmethod
    def _instrument_key(instrument: str | Mapping[str, Any]) -> str:
        if isinstance(instrument, str):
            symbol, separator, venue = instrument.rpartition(".")
        elif isinstance(instrument, Mapping):
            symbol, venue = instrument.get("symbol"), instrument.get("venue")
            separator = "."
        else:
            raise TypeError("instrument must be SYMBOL.VENUE or an instrument mapping")
        if not separator or not isinstance(symbol, str) or not symbol.strip() or not isinstance(venue, str) or not venue.strip():
            raise ValueError("instrument must contain non-empty symbol and venue")
        return f"{symbol}.{venue}"

    def order_intent(
        self,
        *,
        intent_id: int,
        instrument: str | Mapping[str, Any],
        side: Literal["buy", "sell", "Buy", "Sell"],
        qty_raw: int | None = None,
        qty_exact: str | None = None,
        limit_price_raw: int | None = None,
        limit_price_exact: str | None = None,
        quote_budget: str | None = None,
        policy: Mapping[str, Any] | None = None,
        reduce_only: bool = False,
        post_only: bool = False,
    ) -> dict[str, Any]:
        """Build one Rust ``StrategyOrderIntent`` mapping without submitting it.

        Raw quantities/prices use the core 1e-9 scale. Exact amounts and a
        quote-currency budget must be decimal strings so Python floats can never
        enter the deterministic order contract. All intents still pass Rust
        validation, account capability checks, risk and OMS.
        """
        if isinstance(intent_id, bool) or not isinstance(intent_id, int) or not 0 < intent_id <= (1 << 64) - 1:
            raise ValueError("intent_id must be a positive u64")
        if isinstance(side, str):
            normalized_side = side.strip().lower()
        else:
            normalized_side = ""
        if normalized_side not in {"buy", "sell"}:
            raise ValueError("side must be buy or sell")
        if not isinstance(reduce_only, bool) or not isinstance(post_only, bool):
            raise TypeError("reduce_only and post_only must be bool")

        if isinstance(instrument, str):
            symbol, separator, venue = instrument.rpartition(".")
            if not separator:
                raise ValueError("instrument string must use SYMBOL.VENUE form")
            instrument_value: dict[str, Any] = {"symbol": symbol, "venue": venue}
        elif isinstance(instrument, Mapping):
            instrument_value = dict(instrument)
            symbol = instrument_value.get("symbol")
            venue = instrument_value.get("venue")
        else:
            raise TypeError("instrument must be SYMBOL.VENUE or an instrument mapping")
        if not isinstance(symbol, str) or not symbol.strip() or not isinstance(venue, str) or not venue.strip():
            raise ValueError("instrument must contain non-empty symbol and venue")

        raw_limit = self._positive_raw(limit_price_raw, "limit_price_raw", optional=True)
        raw_qty = self._positive_raw(qty_raw, "qty_raw", optional=True)
        qty_exact_value = (
            None if qty_exact is None else self._exact_decimal_wire(qty_exact, "qty_exact")
        )
        limit_exact_value = (
            None
            if limit_price_exact is None
            else self._exact_decimal_wire(limit_price_exact, "limit_price_exact")
        )
        quote_budget_value = (
            None if quote_budget is None else self._exact_decimal_wire(quote_budget, "quote_budget")
        )

        if quote_budget_value is not None:
            if (
                normalized_side != "buy"
                or raw_qty is not None
                or qty_exact_value is not None
                or raw_limit is not None
                or limit_exact_value is not None
                or post_only
            ):
                raise ValueError("quote_budget is only valid for a standalone non-post-only market buy")
            encoded_qty = 0
        else:
            if (raw_qty is None) == (qty_exact_value is None):
                raise ValueError("provide exactly one of qty_raw or qty_exact")
            if qty_exact_value is not None and raw_limit is not None:
                raise ValueError("qty_exact cannot be combined with limit_price_raw")
            if limit_exact_value is not None and qty_exact_value is None:
                raise ValueError("limit_price_exact requires qty_exact")
            encoded_qty = 0 if qty_exact_value is not None else raw_qty

        if policy is not None and not isinstance(policy, Mapping):
            raise TypeError("policy must be a mapping or None")
        return {
            "intent_id": intent_id,
            "instrument": instrument_value,
            "side": "Buy" if normalized_side == "buy" else "Sell",
            "qty": encoded_qty,
            "qty_exact": qty_exact_value,
            "limit": raw_limit,
            "limit_price_exact": limit_exact_value,
            "quote_budget": quote_budget_value,
            "policy": None if policy is None else dict(policy),
            "reduce_only": reduce_only,
            "post_only": post_only,
        }

    @staticmethod
    def _positive_raw(value: int | None, name: str, *, optional: bool = False) -> int | None:
        if value is None and optional:
            return None
        if isinstance(value, bool) or not isinstance(value, int) or not 0 < value <= (1 << 127) - 1:
            raise ValueError(f"{name} must be a positive i128")
        return value

    @staticmethod
    def _exact_decimal_wire(value: str, name: str) -> dict[str, int | str]:
        if not isinstance(value, str):
            raise TypeError(f"{name} must be a decimal string; floats are not accepted")
        try:
            decimal_value = Decimal(value)
        except InvalidOperation as error:
            raise ValueError(f"{name} is not a valid decimal string") from error
        if not decimal_value.is_finite() or decimal_value <= 0:
            raise ValueError(f"{name} must be a finite positive decimal")

        sign, raw_digits, exponent = decimal_value.as_tuple()
        digits = list(raw_digits)
        while exponent < 0 and len(digits) > 1 and digits[-1] == 0:
            digits.pop()
            exponent += 1
        if sign:
            raise ValueError(f"{name} must be positive")
        if exponent < -18:
            raise ValueError(f"{name} precision exceeds 18 decimal places")
        if exponent > 38:
            raise ValueError(f"{name} coefficient exceeds i128")

        coefficient = int("".join(str(digit) for digit in digits))
        scale = max(0, -exponent)
        if exponent > 0:
            coefficient *= 10**exponent
        if coefficient > (1 << 127) - 1:
            raise ValueError(f"{name} coefficient exceeds i128")
        return {"coefficient": str(coefficient), "scale": scale}
