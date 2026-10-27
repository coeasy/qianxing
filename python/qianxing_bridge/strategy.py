"""版本化 Python 策略 JSON 契约。

策略进程只能接收不可变定点输入并返回 Signal/Portfolio/OrderIntent 目标，不得直接访问
交易所、控制面或账簿。协议与 Rust ``qx_runtime`` 中的
``StrategyContractInput/Output`` 一一对应，便于 JSONL、Arrow 和其他语言复用。
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field, fields
from decimal import Decimal, InvalidOperation, ROUND_DOWN, localcontext
from typing import Any, Mapping, Sequence


SCHEMA_VERSION = 1
INPUT_V2_SCHEMA_VERSION = 2
INPUT_V3_SCHEMA_VERSION = 3
INPUT_V4_SCHEMA_VERSION = 4
INPUT_V5_SCHEMA_VERSION = 5
INPUT_V6_SCHEMA_VERSION = 6
INPUT_V7_SCHEMA_VERSION = 7
INPUT_V8_SCHEMA_VERSION = 8
INPUT_V9_SCHEMA_VERSION = 9
STRATEGY_CHECKPOINT_SCHEMA_VERSION = 1
ORDER_STATUS_VALUES = {
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

# Rust StrategyContractIntent::validate_for is the canonical vocabulary. Keep this
# Python mirror checked against that validator and the published JSON schema by
# strategy_intent_three_language_check in tools/check_architecture.py.
STRATEGY_INTENT_ENUMS = {
    "side": ("buy", "sell"),
    "position_side": ("net", "long", "short"),
    "margin_mode": ("cash", "cross", "isolated"),
    "position_mode": ("one_way", "hedge"),
}


def account_position_key(account_id: str, instrument: str) -> str:
    """Return the key for a route-scoped position in StrategyInput.positions_exact."""
    _require_text(account_id, "account_id")
    _require_text(instrument, "instrument")
    return f"{account_id}|{instrument}"
_ASCII_LOWER = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")


def _canonical_enum_case(value: Any) -> Any:
    """Normalize ASCII enum spelling exactly as Rust's to_ascii_lowercase does."""
    return value.translate(_ASCII_LOWER) if isinstance(value, str) else value


def _require_text(value: Any, name: str) -> str:
    text = str(value)
    if not text.strip():
        raise ValueError(f"{name} must be non-empty")
    return text


def _require_integer_range(value: Any, name: str, minimum: int, maximum: int) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise ValueError(f"{name} must be an integer")
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} is outside the supported integer range")
    return value


def _require_contract_text(value: Any, name: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"{name} must be a string")
    if not value.strip():
        raise ValueError(f"{name} must be non-empty")
    return value


def _validate_strategy_checkpoint(
    value: Mapping[str, Any], strategy_id: str, strategy_version: str
) -> None:
    allowed = {
        "schema_version",
        "strategy_id",
        "strategy_version",
        "adapter_state",
        "strategy_state",
    }
    unknown = sorted(str(key) for key in value if key not in allowed)
    if unknown or set(value) != allowed:
        detail = f": {', '.join(unknown)}" if unknown else ""
        raise ValueError(f"strategy_checkpoint has an invalid field set{detail}")
    if value.get("schema_version") != STRATEGY_CHECKPOINT_SCHEMA_VERSION:
        raise ValueError("strategy_checkpoint schema_version is unsupported")
    if value.get("strategy_id") != strategy_id or value.get("strategy_version") != strategy_version:
        raise ValueError("strategy_checkpoint does not match the active strategy identity/version")
    if not isinstance(value.get("adapter_state"), Mapping) or not isinstance(
        value.get("strategy_state"), Mapping
    ):
        raise ValueError("strategy_checkpoint adapter_state and strategy_state must be objects")
    try:
        encoded = json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise ValueError("strategy_checkpoint must contain finite JSON values") from error
    if len(encoded) > 64 * 1024:
        raise ValueError("strategy_checkpoint exceeds the 64 KiB limit")


def _reject_unknown_keys(value: Mapping[str, Any], model: type, name: str) -> None:
    """拒绝契约之外的键，而不是当成没写。

    与 Rust 侧 `StrategyContractOutput/StrategyContractIntent` 的 `deny_unknown_fields`
    同一口径：策略作者把 `post_only` 拼成 `post_onli` 时，静默丢掉等于那一腿按运行时
    默认档位成交，而作者以为自己在它上面开了 post-only。允许集取自 dataclass 字段，
    所以 SDK 加字段时这里不会变成第二份需要手工同步的清单。
    """
    if not isinstance(value, Mapping):
        raise ValueError(f"{name} must be an object")
    allowed = {item.name for item in fields(model)}
    unknown = sorted(str(key) for key in value if key not in allowed)
    if unknown:
        raise ValueError(f"{name} contains unknown key(s): {', '.join(unknown)}")


def _validate_order_status_update(
    value: Mapping[str, Any], strategy_id: str, as_of: int, index: int
) -> None:
    name = f"execution_updates[{index}]"
    allowed = {"schema_version", "event", "event_seq", "ts", "status", "order", "reason"}
    unknown = sorted(str(key) for key in value if key not in allowed)
    if unknown:
        raise ValueError(f"{name} contains unknown key(s): {', '.join(unknown)}")
    if int(value.get("schema_version", 0)) != 1 or value.get("event") != "order_status":
        raise ValueError(f"{name} schema_version or event is unsupported")
    event_seq = value.get("event_seq")
    ts = value.get("ts")
    if not isinstance(event_seq, int) or isinstance(event_seq, bool) or event_seq < 0:
        raise ValueError(f"{name}.event_seq must be a non-negative integer")
    if not isinstance(ts, int) or isinstance(ts, bool) or ts <= 0 or ts > as_of:
        raise ValueError(f"{name}.ts must be positive and no later than as_of")
    if value.get("status") not in ORDER_STATUS_VALUES:
        raise ValueError(f"{name}.status is unsupported")
    order = value.get("order")
    if not isinstance(order, Mapping):
        raise ValueError(f"{name}.order must be an object")
    order_allowed = {
        "order_id", "account_id", "instrument", "side", "qty_raw", "filled_raw",
        "strategy_id", "exact_amounts",
    }
    order_unknown = sorted(str(key) for key in order if key not in order_allowed)
    if order_unknown:
        raise ValueError(f"{name}.order contains unknown key(s): {', '.join(order_unknown)}")
    for key in ("order_id", "account_id", "instrument", "side", "qty_raw", "filled_raw", "strategy_id"):
        if key not in order:
            raise ValueError(f"{name}.order.{key} is required")
    if not isinstance(order["order_id"], int) or isinstance(order["order_id"], bool) or order["order_id"] <= 0:
        raise ValueError(f"{name}.order.order_id must be positive")
    if order["side"] not in ("Buy", "Sell"):
        raise ValueError(f"{name}.order.side is unsupported")
    if order["strategy_id"] != strategy_id:
        raise ValueError(f"{name}.order.strategy_id does not match input")
    for key in ("account_id", "instrument"):
        _require_text(order[key], f"{name}.order.{key}")
    for key in ("qty_raw", "filled_raw"):
        if not isinstance(order[key], int) or isinstance(order[key], bool) or order[key] < 0:
            raise ValueError(f"{name}.order.{key} must be a non-negative integer")
    exact_amounts = order.get("exact_amounts")
    if exact_amounts is None:
        if order["qty_raw"] <= 0 or order["filled_raw"] > order["qty_raw"]:
            raise ValueError(f"{name}.order quantity or fill is invalid")
    else:
        if not isinstance(exact_amounts, Mapping):
            raise ValueError(f"{name}.order.exact_amounts must be an object")
        exact_allowed = {
            "quantity", "limit_price", "filled", "quote_budget", "quote_filled",
            "market_reference_price",
        }
        exact_unknown = sorted(str(key) for key in exact_amounts if key not in exact_allowed)
        if exact_unknown:
            raise ValueError(f"{name}.order.exact_amounts contains unknown key(s): {', '.join(exact_unknown)}")
        for key in ("quantity", "limit_price", "filled", "quote_filled"):
            if key not in exact_amounts:
                raise ValueError(f"{name}.order.exact_amounts.{key} is required")
        if order["qty_raw"] != 0 or order["filled_raw"] != 0:
            raise ValueError(f"{name}.order exact amounts cannot mix with raw quantities")
        for key, amount in exact_amounts.items():
            if amount is None:
                continue
            if not isinstance(amount, Mapping) or set(amount) != {"coefficient", "scale"}:
                raise ValueError(f"{name}.order.exact_amounts.{key} is invalid")
            coefficient = amount["coefficient"]
            scale = amount["scale"]
            if not isinstance(coefficient, str) or not re.fullmatch(r"-?(?:0|[1-9][0-9]*)", coefficient):
                raise ValueError(f"{name}.order.exact_amounts.{key}.coefficient is invalid")
            if not isinstance(scale, int) or isinstance(scale, bool) or not 0 <= scale <= 18:
                raise ValueError(f"{name}.order.exact_amounts.{key}.scale is invalid")
    reason = value.get("reason")
    if reason is not None and (not isinstance(reason, str) or len(reason.encode("utf-8")) > 2048):
        raise ValueError(f"{name}.reason is invalid")


def _validate_fill_update_v2(
    value: Mapping[str, Any], strategy_id: str, as_of: int, index: int
) -> None:
    name = f"fill_updates[{index}]"
    allowed = {"schema_version", "event", "event_seq", "ts", "order", "fill"}
    unknown = sorted(str(key) for key in value if key not in allowed)
    if unknown:
        raise ValueError(f"{name} contains unknown key(s): {', '.join(unknown)}")
    if value.get("schema_version") != 2 or value.get("event") != "fill":
        raise ValueError(f"{name} schema_version or event is unsupported")
    event_seq = value.get("event_seq")
    ts = value.get("ts")
    if not isinstance(event_seq, int) or isinstance(event_seq, bool) or event_seq < 0:
        raise ValueError(f"{name}.event_seq must be a non-negative integer")
    if not isinstance(ts, int) or isinstance(ts, bool) or ts <= 0 or ts > as_of:
        raise ValueError(f"{name}.ts must be positive and no later than as_of")

    order = value.get("order")
    if not isinstance(order, Mapping):
        raise ValueError(f"{name}.order must be an object")
    order_allowed = {
        "order_id", "account_id", "instrument", "side", "qty_raw", "filled_raw",
        "strategy_id", "exact_amounts",
    }
    order_unknown = sorted(str(key) for key in order if key not in order_allowed)
    if order_unknown:
        raise ValueError(f"{name}.order contains unknown key(s): {', '.join(order_unknown)}")
    for key in ("order_id", "account_id", "instrument", "side", "qty_raw", "filled_raw", "strategy_id"):
        if key not in order:
            raise ValueError(f"{name}.order.{key} is required")
    if not isinstance(order["order_id"], int) or isinstance(order["order_id"], bool) or order["order_id"] <= 0:
        raise ValueError(f"{name}.order.order_id must be positive")
    if order["side"] not in ("Buy", "Sell"):
        raise ValueError(f"{name}.order.side is unsupported")
    if order["strategy_id"] != strategy_id:
        raise ValueError(f"{name}.order.strategy_id does not match input")
    for key in ("account_id", "instrument"):
        _require_text(order[key], f"{name}.order.{key}")
    for key in ("qty_raw", "filled_raw"):
        if not isinstance(order[key], int) or isinstance(order[key], bool) or order[key] < 0:
            raise ValueError(f"{name}.order.{key} must be a non-negative integer")

    fill = value.get("fill")
    if not isinstance(fill, Mapping):
        raise ValueError(f"{name}.fill must be an object")
    fill_allowed = {
        "order_id", "qty", "price", "fee", "ts", "account_id", "strategy_id",
        "signal_id", "intent_id", "venue_id", "venue_order_id", "rule_version",
        "fee_currency", "fees", "exact_fees", "exact_amounts",
    }
    fill_unknown = sorted(str(key) for key in fill if key not in fill_allowed)
    if fill_unknown:
        raise ValueError(f"{name}.fill contains unknown key(s): {', '.join(fill_unknown)}")
    for key in ("order_id", "qty", "price", "fee", "ts", "account_id"):
        if key not in fill:
            raise ValueError(f"{name}.fill.{key} is required")
    if fill["order_id"] != order["order_id"] or fill["account_id"] != order["account_id"]:
        raise ValueError(f"{name}.fill order/account identity does not match the order")
    if fill.get("strategy_id") not in (None, strategy_id):
        raise ValueError(f"{name}.fill.strategy_id does not match input")
    if fill["ts"] != ts:
        raise ValueError(f"{name}.fill.ts must match the update timestamp")
    for key in ("qty", "price", "fee"):
        if not isinstance(fill[key], int) or isinstance(fill[key], bool):
            raise ValueError(f"{name}.fill.{key} must be an integer")
    exact_amounts = fill.get("exact_amounts")
    if exact_amounts is None:
        if fill["qty"] <= 0 or fill["price"] <= 0:
            raise ValueError(f"{name}.fill quantity and price must be positive")
    elif not isinstance(exact_amounts, Mapping):
        raise ValueError(f"{name}.fill.exact_amounts must be an object")
    else:
        allowed_exact_amounts = {"quantity", "price", "quote_cost", "quote_budget_complete"}
        unknown_exact_amounts = sorted(
            str(key) for key in exact_amounts if key not in allowed_exact_amounts
        )
        if unknown_exact_amounts:
            raise ValueError(
                f"{name}.fill.exact_amounts contains unknown key(s): "
                f"{', '.join(unknown_exact_amounts)}"
            )
        for key in ("quantity", "price"):
            if key not in exact_amounts:
                raise ValueError(f"{name}.fill.exact_amounts.{key} is required")
        for key in ("quantity", "price", "quote_cost"):
            amount = exact_amounts.get(key)
            if amount is None:
                continue
            if not isinstance(amount, Mapping) or set(amount) != {"coefficient", "scale"}:
                raise ValueError(f"{name}.fill.exact_amounts.{key} is invalid")
            coefficient = amount["coefficient"]
            scale = amount["scale"]
            if (
                not isinstance(coefficient, str)
                or not re.fullmatch(r"(?:0|[1-9][0-9]*)", coefficient)
                or len(coefficient) > 39
                or int(coefficient) <= 0
                or int(coefficient) > (1 << 127) - 1
            ):
                raise ValueError(f"{name}.fill.exact_amounts.{key}.coefficient is invalid")
            if not isinstance(scale, int) or isinstance(scale, bool) or not 0 <= scale <= 18:
                raise ValueError(f"{name}.fill.exact_amounts.{key}.scale is invalid")
        complete = exact_amounts.get("quote_budget_complete", False)
        if not isinstance(complete, bool):
            raise ValueError(f"{name}.fill.exact_amounts.quote_budget_complete must be a boolean")
        if complete and exact_amounts.get("quote_cost") is None:
            raise ValueError(
                f"{name}.fill.exact_amounts.quote_budget_complete requires quote_cost"
            )


def _validate_positive_exact_decimal(value: Any, name: str) -> None:
    """Match Rust ExactDecimal's positive, finite, <=18-place decimal boundary."""
    if not isinstance(value, str) or not value.strip() or len(value) > 256:
        raise ValueError(f"{name} must be a positive exact decimal string")
    try:
        decimal = Decimal(value.strip())
    except InvalidOperation as error:
        raise ValueError(f"{name} must be a positive exact decimal string") from error
    if not decimal.is_finite() or decimal <= 0:
        raise ValueError(f"{name} must be a positive exact decimal string")
    sign, digits, exponent = decimal.as_tuple()
    if sign:
        raise ValueError(f"{name} must be positive")
    magnitude = int("".join(str(digit) for digit in digits) or "0")
    while exponent < 0 and magnitude and magnitude % 10 == 0:
        magnitude //= 10
        exponent += 1
    scale = max(-exponent, 0)
    if exponent > 100 or exponent < -100:
        raise ValueError(f"{name} exceeds the supported exact decimal range")
    coefficient = magnitude * (10**max(exponent, 0))
    if scale > 18 or coefficient > (1 << 127) - 1:
        raise ValueError(f"{name} exceeds the supported exact decimal range")


def _validate_signed_exact_decimal(value: Any, name: str) -> None:
    if not isinstance(value, str) or not value.strip() or len(value) > 256:
        raise ValueError(f"{name} must be a signed exact decimal string")
    try:
        decimal = Decimal(value.strip())
    except InvalidOperation as error:
        raise ValueError(f"{name} must be a signed exact decimal string") from error
    if not decimal.is_finite():
        raise ValueError(f"{name} must be a signed exact decimal string")
    sign, digits, exponent = decimal.as_tuple()
    magnitude = int("".join(str(digit) for digit in digits) or "0")
    while exponent < 0 and magnitude and magnitude % 10 == 0:
        magnitude //= 10
        exponent += 1
    scale = max(-exponent, 0)
    if exponent > 100 or exponent < -100 or scale > 18:
        raise ValueError(f"{name} exceeds the supported exact decimal range")
    coefficient = magnitude * (10**max(exponent, 0))
    if coefficient > ((1 << 127) if sign else (1 << 127) - 1):
        raise ValueError(f"{name} exceeds the supported exact decimal range")


def _validate_market_tick_update(value: Mapping[str, Any], as_of: int, index: int) -> None:
    name = f"market_ticks[{index}]"
    allowed = {
        "stream_id", "event_seq", "source_seq", "ts", "instrument",
        "bid_raw", "ask_raw", "bid_qty_raw", "ask_qty_raw",
        "bid_exact", "ask_exact", "bid_qty_exact", "ask_qty_exact",
    }
    if set(value) - allowed:
        unknown = sorted(str(key) for key in set(value) - allowed)
        raise ValueError(f"{name} contains unknown key(s): {', '.join(unknown)}")
    required = allowed - {"bid_exact", "ask_exact", "bid_qty_exact", "ask_qty_exact"}
    missing = sorted(required - set(value))
    if missing:
        raise ValueError(f"{name} is missing key(s): {', '.join(missing)}")
    _require_contract_text(value["stream_id"], f"{name}.stream_id")
    instrument = _require_contract_text(value["instrument"], f"{name}.instrument")
    symbol, separator, venue = instrument.rpartition(".")
    if not separator or not symbol.strip() or not venue.strip():
        raise ValueError(f"{name}.instrument must be a qualified instrument")
    for key in ("event_seq", "source_seq"):
        _require_integer_range(value[key], f"{name}.{key}", 0, (1 << 64) - 1)
    _require_integer_range(value["ts"], f"{name}.ts", 1, as_of)

    def validate_amount(raw_key: str, exact_key: str, *, allow_zero: bool) -> Decimal:
        exact_text = value.get(exact_key)
        raw_min = 0 if allow_zero or exact_text is not None else 1
        raw = _require_integer_range(
            value[raw_key], f"{name}.{raw_key}", raw_min, (1 << 127) - 1
        )
        if exact_text is None:
            digits = tuple(int(digit) for digit in str(raw))
            return Decimal((0, digits, -9))
        if not isinstance(exact_text, str) or not exact_text.strip() or len(exact_text) > 256:
            raise ValueError(f"{name}.{exact_key} must be an exact decimal string")
        try:
            exact = Decimal(exact_text)
        except InvalidOperation as error:
            raise ValueError(f"{name}.{exact_key} must be an exact decimal string") from error
        if not exact.is_finite() or exact < 0 or (not allow_zero and exact == 0):
            raise ValueError(f"{name}.{exact_key} must be positive")
        sign, digits, exponent = exact.as_tuple()
        if sign:
            raise ValueError(f"{name}.{exact_key} must not be negative")
        magnitude = int("".join(str(digit) for digit in digits) or "0")
        while exponent < 0 and magnitude and magnitude % 10 == 0:
            magnitude //= 10
            exponent += 1
        scale = max(-exponent, 0)
        coefficient = magnitude * (10 ** max(exponent, 0))
        if exponent > 100 or exponent < -100 or scale > 18 or coefficient > (1 << 127) - 1:
            raise ValueError(f"{name}.{exact_key} exceeds the supported exact decimal range")
        with localcontext() as decimal_context:
            decimal_context.prec = 100
            projected = int(
                (exact * Decimal(1_000_000_000)).to_integral_value(rounding=ROUND_DOWN)
            )
        if projected != raw:
            raise ValueError(f"{name}.{exact_key} conflicts with its raw projection")
        return exact

    bid = validate_amount("bid_raw", "bid_exact", allow_zero=False)
    ask = validate_amount("ask_raw", "ask_exact", allow_zero=False)
    validate_amount("bid_qty_raw", "bid_qty_exact", allow_zero=True)
    validate_amount("ask_qty_raw", "ask_qty_exact", allow_zero=True)
    if bid > ask:
        raise ValueError(f"{name}.bid_raw cannot exceed ask_raw")


def _validate_market_order_book(value: Mapping[str, Any], as_of: int, index: int) -> None:
    name = f"market_order_books[{index}]"
    allowed = {
        "stream_id", "event_seq", "source_seq", "ts", "instrument", "sequence", "bids", "asks",
    }
    _reject_mapping_keys(value, allowed, name)
    missing = sorted(allowed - set(value))
    if missing:
        raise ValueError(f"{name} is missing key(s): {', '.join(missing)}")
    _require_contract_text(value["stream_id"], f"{name}.stream_id")
    instrument = _require_contract_text(value["instrument"], f"{name}.instrument")
    symbol, separator, venue = instrument.rpartition(".")
    if not separator or not symbol.strip() or not venue.strip():
        raise ValueError(f"{name}.instrument must be a qualified instrument")
    _require_integer_range(value["event_seq"], f"{name}.event_seq", 0, (1 << 64) - 1)
    _require_integer_range(value["source_seq"], f"{name}.source_seq", 0, (1 << 64) - 1)
    _require_integer_range(value["ts"], f"{name}.ts", 1, as_of)
    _require_integer_range(value["sequence"], f"{name}.sequence", 1, (1 << 64) - 1)

    def side_prices(side: str, bid_side: bool) -> list[Decimal]:
        levels = value[side]
        if not isinstance(levels, list) or not 1 <= len(levels) <= 100:
            raise ValueError(f"{name}.{side} must contain between 1 and 100 levels")
        prices: list[Decimal] = []
        for level_index, level in enumerate(levels):
            row_name = f"{name}.{side}[{level_index}]"
            if not isinstance(level, Mapping):
                raise ValueError(f"{row_name} must be an object")
            _reject_mapping_keys(level, {"price_raw", "qty_raw", "price_exact", "qty_exact"}, row_name)
            if not {"price_raw", "qty_raw"}.issubset(level):
                raise ValueError(f"{row_name} requires price_raw and qty_raw")

            def amount(raw_key: str, exact_key: str) -> Decimal:
                exact_text = level.get(exact_key)
                raw = _require_integer_range(
                    level[raw_key], f"{row_name}.{raw_key}", 0, (1 << 127) - 1
                )
                if exact_text is None:
                    parsed = Decimal(raw) / Decimal(1_000_000_000)
                else:
                    _validate_signed_exact_decimal(exact_text, f"{row_name}.{exact_key}")
                    parsed = Decimal(exact_text)
                    if parsed <= 0:
                        raise ValueError(f"{row_name}.{exact_key} must be positive")
                    with localcontext() as context:
                        context.prec = 100
                        projected = int(
                            (parsed * Decimal(1_000_000_000)).to_integral_value(
                                rounding=ROUND_DOWN
                            )
                        )
                    if projected != raw:
                        raise ValueError(f"{row_name}.{exact_key} conflicts with its raw projection")
                if parsed <= 0:
                    raise ValueError(f"{row_name}.{raw_key} must be positive")
                return parsed

            prices.append(amount("price_raw", "price_exact"))
            amount("qty_raw", "qty_exact")
        for left, right in zip(prices, prices[1:]):
            if (bid_side and left <= right) or (not bid_side and left >= right):
                raise ValueError(f"{name}.{side} levels must be strictly price-sorted")
        return prices

    bids = side_prices("bids", True)
    asks = side_prices("asks", False)
    if bids[0] > asks[0]:
        raise ValueError(f"{name} best bid cannot exceed best ask")


def _validate_account_update(value: Mapping[str, Any], as_of: int, index: int) -> None:
    name = f"account_updates[{index}]"
    allowed = {
        "schema_version", "event", "event_seq", "ts", "account_id", "venue_id",
        "balances", "positions",
    }
    unknown = sorted(str(key) for key in value if key not in allowed)
    if unknown:
        raise ValueError(f"{name} contains unknown key(s): {', '.join(unknown)}")
    if value.get("schema_version") != 1:
        raise ValueError(f"{name}.schema_version is unsupported")
    _require_integer_range(value.get("event_seq"), f"{name}.event_seq", 0, (1 << 64) - 1)
    _require_integer_range(value.get("ts"), f"{name}.ts", 1, as_of)
    _require_contract_text(value.get("account_id"), f"{name}.account_id")
    _require_contract_text(value.get("venue_id"), f"{name}.venue_id")
    if value.get("event") == "account_snapshot":
        balances = value.get("balances")
        if not isinstance(balances, list) or "positions" in value:
            raise ValueError(f"{name} account_snapshot requires balances and forbids positions")
        assets: set[str] = set()
        for balance_index, balance in enumerate(balances):
            row_name = f"{name}.balances[{balance_index}]"
            if not isinstance(balance, Mapping):
                raise ValueError(f"{row_name} must be an object")
            _reject_mapping_keys(
                balance,
                {
                    "asset", "free", "locked", "borrowed", "net_cash_balance", "exact_amounts",
                },
                row_name,
            )
            asset = _require_contract_text(balance.get("asset"), f"{row_name}.asset")
            if asset in assets:
                raise ValueError(f"{name}.balances contains duplicate asset {asset}")
            assets.add(asset)
            for field_name in ("free", "locked", "borrowed"):
                _require_integer_range(
                    balance.get(field_name),
                    f"{row_name}.{field_name}",
                    -(1 << 127),
                    (1 << 127) - 1,
                )
            if balance.get("net_cash_balance") is not None:
                _require_integer_range(
                    balance["net_cash_balance"],
                    f"{row_name}.net_cash_balance",
                    -(1 << 127),
                    (1 << 127) - 1,
                )
            exact = balance.get("exact_amounts")
            if exact is not None:
                if not isinstance(exact, Mapping):
                    raise ValueError(f"{row_name}.exact_amounts must be an object or null")
                _reject_mapping_keys(
                    exact,
                    {"free", "locked", "borrowed", "net_cash_balance"},
                    f"{row_name}.exact_amounts",
                )
                for field_name in ("free", "locked", "borrowed"):
                    projected = _project_exact_decimal_to_raw(
                        exact.get(field_name), f"{row_name}.exact_amounts.{field_name}"
                    )
                    if projected != balance[field_name]:
                        raise ValueError(
                            f"{row_name}.exact_amounts.{field_name} conflicts with its raw projection"
                        )
                if exact.get("net_cash_balance") is not None:
                    projected = _project_exact_decimal_to_raw(
                        exact["net_cash_balance"],
                        f"{row_name}.exact_amounts.net_cash_balance",
                    )
                    if balance.get("net_cash_balance") is None or projected != balance["net_cash_balance"]:
                        raise ValueError(
                            f"{row_name}.exact_amounts.net_cash_balance conflicts with its raw projection"
                        )
    elif value.get("event") == "position_snapshot":
        positions = value.get("positions")
        if not isinstance(positions, list) or "balances" in value:
            raise ValueError(f"{name} position_snapshot requires positions and forbids balances")
        identities: set[tuple[str, str]] = set()
        for position_index, position in enumerate(positions):
            row_name = f"{name}.positions[{position_index}]"
            if not isinstance(position, Mapping):
                raise ValueError(f"{row_name} must be an object")
            _reject_mapping_keys(
                position,
                {
                    "instrument", "settlement_currency", "quantity", "average_price",
                    "mark_price", "liquidation_price", "unrealized_pnl", "initial_margin",
                    "maintenance_margin", "leverage", "margin_mode", "position_side",
                    "exact_amounts",
                },
                row_name,
            )
            instrument_value = position.get("instrument")
            if isinstance(instrument_value, str):
                instrument = _require_contract_text(instrument_value, f"{row_name}.instrument")
            elif isinstance(instrument_value, Mapping) and set(instrument_value) == {"symbol", "venue"}:
                instrument = (
                    _require_contract_text(instrument_value["symbol"], f"{row_name}.instrument.symbol")
                    + "."
                    + _require_contract_text(instrument_value["venue"], f"{row_name}.instrument.venue")
                )
            else:
                raise ValueError(f"{row_name}.instrument must be a qualified string or symbol/venue object")
            side = position.get("position_side") or "net"
            if not isinstance(side, str) or not side.strip():
                raise ValueError(f"{row_name}.position_side must be a non-empty string or null")
            identity = (instrument, side)
            if identity in identities:
                raise ValueError(f"{name}.positions contains duplicate instrument/side")
            identities.add(identity)
            quantity = position.get("quantity")
            _require_integer_range(
                quantity,
                f"{row_name}.quantity",
                -(1 << 127),
                (1 << 127) - 1,
            )
            exact = position.get("exact_amounts")
            raw_fields = {
                "quantity": "quantity",
                "average_price": "average_price",
                "mark_price": "mark_price",
                "liquidation_price": "liquidation_price",
                "unrealized_pnl": "unrealized_pnl",
                "initial_margin": "initial_margin",
                "maintenance_margin": "maintenance_margin",
            }
            for field_name in raw_fields.values():
                raw_value = position.get(field_name)
                if raw_value is not None:
                    _require_integer_range(
                        raw_value,
                        f"{row_name}.{field_name}",
                        -(1 << 127),
                        (1 << 127) - 1,
                    )
            if position.get("leverage") is not None:
                _require_integer_range(position["leverage"], f"{row_name}.leverage", 1, (1 << 32) - 1)
            for field_name in ("settlement_currency", "margin_mode", "position_side"):
                field_value = position.get(field_name)
                if field_value is not None:
                    _require_contract_text(field_value, f"{row_name}.{field_name}")
            if exact is not None:
                if not isinstance(exact, Mapping):
                    raise ValueError(f"{row_name}.exact_amounts must be an object or null")
                _reject_mapping_keys(exact, set(raw_fields), f"{row_name}.exact_amounts")
                if "quantity" not in exact:
                    raise ValueError(f"{row_name}.exact_amounts.quantity is required")
                for exact_name, raw_name in raw_fields.items():
                    if exact.get(exact_name) is None:
                        continue
                    projected = _project_exact_decimal_to_raw(
                        exact[exact_name], f"{row_name}.exact_amounts.{exact_name}"
                    )
                    raw_value = position.get(raw_name)
                    if raw_value is None or projected != raw_value:
                        raise ValueError(
                            f"{row_name}.exact_amounts.{exact_name} conflicts with its raw projection"
                        )
    else:
        raise ValueError(f"{name}.event is unsupported")


def _reject_mapping_keys(value: Mapping[str, Any], allowed: set[str], name: str) -> None:
    unknown = sorted(str(key) for key in value if key not in allowed)
    if unknown:
        raise ValueError(f"{name} contains unknown key(s): {', '.join(unknown)}")


def _exact_decimal_wire_parts(value: Any, name: str) -> tuple[int, int]:
    if not isinstance(value, Mapping) or set(value) != {"coefficient", "scale"}:
        raise ValueError(f"{name} must contain exactly coefficient and scale")
    coefficient_text = value["coefficient"]
    scale = value["scale"]
    if (
        not isinstance(coefficient_text, str)
        or not re.fullmatch(r"-?(?:0|[1-9][0-9]*)", coefficient_text)
        or len(coefficient_text) > 40
        or not isinstance(scale, int)
        or isinstance(scale, bool)
        or not 0 <= scale <= 18
    ):
        raise ValueError(f"{name} is not a supported ExactDecimal")
    coefficient = int(coefficient_text)
    if not -(1 << 127) <= coefficient <= (1 << 127) - 1:
        raise ValueError(f"{name}.coefficient is outside i128")
    if (coefficient == 0 and scale != 0) or (scale > 0 and coefficient % 10 == 0):
        raise ValueError(f"{name} must use normalized ExactDecimal encoding")
    return coefficient, scale


def _project_exact_decimal_to_raw(value: Any, name: str) -> int:
    coefficient, scale = _exact_decimal_wire_parts(value, name)
    if scale <= 9:
        projected = coefficient * (10 ** (9 - scale))
    else:
        divisor = 10 ** (scale - 9)
        projected = (abs(coefficient) // divisor) * (-1 if coefficient < 0 else 1)
    if not -(1 << 127) <= projected <= (1 << 127) - 1:
        raise ValueError(f"{name} raw projection is outside i128")
    return projected


def _compare_exact_decimal_wires(left: Any, right: Any, name: str) -> int:
    left_coefficient, left_scale = _exact_decimal_wire_parts(left, name)
    right_coefficient, right_scale = _exact_decimal_wire_parts(right, name)
    scale = max(left_scale, right_scale)
    left_value = left_coefficient * (10 ** (scale - left_scale))
    right_value = right_coefficient * (10 ** (scale - right_scale))
    return (left_value > right_value) - (left_value < right_value)


def _validate_exact_ohlcv_wire(value: Any, name: str) -> tuple[int, ...]:
    required = {"open", "high", "low", "close", "volume"}
    if not isinstance(value, Mapping) or set(value) != required:
        raise ValueError(f"{name} requires exactly open/high/low/close/volume")
    parts = {
        key: _exact_decimal_wire_parts(value[key], f"{name}.{key}")
        for key in required
    }
    for key in ("open", "high", "low", "close"):
        if parts[key][0] <= 0:
            raise ValueError(f"{name}.{key} must be positive")
    if parts["volume"][0] < 0:
        raise ValueError(f"{name}.volume must be non-negative")
    if any(
        _compare_exact_decimal_wires(value["high"], value[key], name) < 0
        for key in ("open", "low", "close")
    ) or any(
        _compare_exact_decimal_wires(value["low"], value[key], name) > 0
        for key in ("open", "close")
    ):
        raise ValueError(f"{name} high/low range does not contain open and close")
    return tuple(
        _project_exact_decimal_to_raw(value[key], f"{name}.{key}")
        for key in ("open", "high", "low", "close", "volume")
    )


@dataclass(frozen=True)
class StrategyBars:
    source: str
    ts: tuple[int, ...]
    open_raw: tuple[int, ...]
    high_raw: tuple[int, ...]
    low_raw: tuple[int, ...]
    close_raw: tuple[int, ...]
    volume_raw: tuple[int, ...]
    exact_ohlcv: tuple[Mapping[str, Any] | None, ...] | None = None

    def validate(self) -> None:
        _require_text(self.source, "bars.source")
        if not self.ts:
            raise ValueError("bars.ts must be non-empty")
        columns = (
            self.open_raw,
            self.high_raw,
            self.low_raw,
            self.close_raw,
            self.volume_raw,
        )
        if any(len(column) != len(self.ts) for column in columns):
            raise ValueError("strategy bars column length mismatch")
        if any(left >= right for left, right in zip(self.ts, self.ts[1:])):
            raise ValueError("strategy bars timestamps must be strictly increasing")
        if self.exact_ohlcv is not None:
            if len(self.exact_ohlcv) != len(self.ts):
                raise ValueError("strategy bars exact_ohlcv row count mismatch")
            found_exact = False
            raw_columns = (
                self.open_raw,
                self.high_raw,
                self.low_raw,
                self.close_raw,
                self.volume_raw,
            )
            for index, row in enumerate(self.exact_ohlcv):
                if row is None:
                    continue
                found_exact = True
                projected = _validate_exact_ohlcv_wire(row, f"bars.exact_ohlcv[{index}]")
                if projected != tuple(column[index] for column in raw_columns):
                    raise ValueError(
                        f"strategy bars exact_ohlcv raw projection mismatch at row {index}"
                    )
            if not found_exact:
                raise ValueError("strategy bars exact_ohlcv requires at least one exact row")

    @property
    def has_exact_ohlcv(self) -> bool:
        return self.exact_ohlcv is not None and any(
            row is not None for row in self.exact_ohlcv
        )

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        value = {
            "source": self.source,
            "ts": list(self.ts),
            "open_raw": list(self.open_raw),
            "high_raw": list(self.high_raw),
            "low_raw": list(self.low_raw),
            "close_raw": list(self.close_raw),
            "volume_raw": list(self.volume_raw),
        }
        if self.exact_ohlcv is not None:
            value["exact_ohlcv"] = [
                None if row is None else dict(row) for row in self.exact_ohlcv
            ]
        return value

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "StrategyBars":
        if not isinstance(value, Mapping):
            raise ValueError("bars must be an object")
        _reject_unknown_keys(value, cls, "bars")
        exact_value = value.get("exact_ohlcv")
        if exact_value is not None and not isinstance(exact_value, list):
            raise ValueError("bars.exact_ohlcv must be an array")
        if exact_value is not None and any(
            row is not None and not isinstance(row, Mapping) for row in exact_value
        ):
            raise ValueError("bars.exact_ohlcv rows must be objects or null")
        bars = cls(
            source=str(value["source"]),
            ts=tuple(int(item) for item in value["ts"]),
            open_raw=tuple(int(item) for item in value["open_raw"]),
            high_raw=tuple(int(item) for item in value["high_raw"]),
            low_raw=tuple(int(item) for item in value["low_raw"]),
            close_raw=tuple(int(item) for item in value["close_raw"]),
            volume_raw=tuple(int(item) for item in value["volume_raw"]),
            exact_ohlcv=(
                None
                if exact_value is None
                else tuple(None if row is None else dict(row) for row in exact_value)
            ),
        )
        bars.validate()
        return bars


@dataclass(frozen=True)
class StrategyInput:
    request_id: str
    strategy_id: str
    strategy_version: str
    data_fingerprint: str
    as_of: int
    instrument: str
    positions: Mapping[str, int] = field(default_factory=dict)
    positions_exact: Mapping[str, str] = field(default_factory=dict)
    cash: Mapping[str, int] = field(default_factory=dict)
    cash_exact: Mapping[str, str] = field(default_factory=dict)
    available_margin_raw: int | None = None
    available_margin_exact: str | None = None
    risk_state: str = ""
    research_targets: Mapping[str, int] = field(default_factory=dict)
    bars: StrategyBars | None = None
    schema_version: int = SCHEMA_VERSION
    market_bars: Mapping[str, StrategyBars] = field(default_factory=dict)
    execution_updates: tuple[Mapping[str, Any], ...] = field(default_factory=tuple)
    fill_updates: tuple[Mapping[str, Any], ...] = field(default_factory=tuple)
    strategy_checkpoint: Mapping[str, Any] | None = None
    market_ticks: tuple[Mapping[str, Any], ...] = field(default_factory=tuple)
    account_updates: tuple[Mapping[str, Any], ...] = field(default_factory=tuple)
    market_order_books: tuple[Mapping[str, Any], ...] = field(default_factory=tuple)

    def validate(self) -> None:
        if (
            isinstance(self.schema_version, bool)
            or not isinstance(self.schema_version, int)
            or self.schema_version not in (
                SCHEMA_VERSION,
                INPUT_V2_SCHEMA_VERSION,
                INPUT_V3_SCHEMA_VERSION,
                INPUT_V4_SCHEMA_VERSION,
                INPUT_V5_SCHEMA_VERSION,
                INPUT_V6_SCHEMA_VERSION,
                INPUT_V7_SCHEMA_VERSION,
                INPUT_V8_SCHEMA_VERSION,
                INPUT_V9_SCHEMA_VERSION,
            )
        ):
            raise ValueError(f"unsupported strategy schema_version: {self.schema_version}")
        if self.schema_version == SCHEMA_VERSION and (
            self.execution_updates or self.fill_updates or self.strategy_checkpoint is not None
            or self.account_updates or self.market_order_books
        ):
            raise ValueError(
                "strategy input v1 cannot contain execution updates, fills, checkpoint, or account updates"
            )
        if self.schema_version == INPUT_V2_SCHEMA_VERSION:
            if not self.execution_updates:
                raise ValueError("strategy input v2 requires non-empty execution_updates")
            if self.fill_updates or self.strategy_checkpoint is not None or self.account_updates or self.market_order_books:
                raise ValueError(
                    "strategy input v2 cannot contain fills, checkpoint, or account updates"
                )
        if self.schema_version == INPUT_V3_SCHEMA_VERSION and (
            not self.fill_updates or self.strategy_checkpoint is not None or self.account_updates or self.market_order_books
        ):
            raise ValueError(
                "strategy input v3 requires non-empty fill_updates and cannot contain checkpoint or account updates"
            )
        if self.schema_version == INPUT_V4_SCHEMA_VERSION and (
            self.strategy_checkpoint is None or self.account_updates or self.market_order_books
        ):
            raise ValueError(
                "strategy input v4 requires strategy_checkpoint and cannot contain account updates"
            )
        has_exact_ohlcv = (
            self.bars is not None and self.bars.has_exact_ohlcv
        ) or any(bars.has_exact_ohlcv for bars in self.market_bars.values())
        if self.schema_version == INPUT_V5_SCHEMA_VERSION and not has_exact_ohlcv:
            raise ValueError("strategy input v5 requires exact OHLCV")
        if self.schema_version not in (
            INPUT_V5_SCHEMA_VERSION, INPUT_V6_SCHEMA_VERSION, INPUT_V7_SCHEMA_VERSION,
            INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION,
        ) and has_exact_ohlcv:
            raise ValueError("strategy input v1-v4 cannot contain exact OHLCV")
        if self.schema_version == INPUT_V6_SCHEMA_VERSION and not self.market_ticks:
            raise ValueError("strategy input v6 requires non-empty market_ticks")
        if self.schema_version == INPUT_V6_SCHEMA_VERSION and self.account_updates:
            raise ValueError("strategy input v6 cannot contain account_updates")
        if self.schema_version == INPUT_V7_SCHEMA_VERSION and not self.account_updates:
            raise ValueError("strategy input v7 requires non-empty account_updates")
        if self.schema_version == INPUT_V7_SCHEMA_VERSION and self.market_order_books:
            raise ValueError("strategy input v7 cannot contain market_order_books; use v8")
        if self.schema_version == INPUT_V8_SCHEMA_VERSION and not self.market_order_books:
            raise ValueError("strategy input v8 requires non-empty market_order_books")
        has_exact_account_facts = bool(self.cash_exact) or self.available_margin_exact is not None
        if has_exact_account_facts and self.schema_version != INPUT_V9_SCHEMA_VERSION:
            raise ValueError("exact cash/margin requires strategy input v9")
        if self.schema_version == INPUT_V9_SCHEMA_VERSION and not has_exact_account_facts:
            raise ValueError("strategy input v9 requires non-empty cash_exact or available_margin_exact")
        if self.schema_version not in (INPUT_V6_SCHEMA_VERSION, INPUT_V7_SCHEMA_VERSION, INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION) and self.market_ticks:
            raise ValueError("strategy input v1-v5 cannot contain market_ticks")
        if self.schema_version not in (INPUT_V7_SCHEMA_VERSION, INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION) and self.account_updates:
            raise ValueError("strategy input v1-v6 cannot contain account_updates")
        if self.schema_version not in (INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION) and self.market_order_books:
            raise ValueError("strategy input v1-v7 cannot contain market_order_books")
        for value, name in (
            (self.request_id, "request_id"),
            (self.strategy_id, "strategy_id"),
            (self.strategy_version, "strategy_version"),
            (self.data_fingerprint, "data_fingerprint"),
            (self.instrument, "instrument"),
            (self.risk_state, "risk_state"),
        ):
            _require_text(value, name)
        if self.as_of <= 0:
            raise ValueError("as_of must be positive")
        if self.available_margin_raw is not None and self.available_margin_raw < 0:
            raise ValueError("available_margin_raw cannot be negative")
        if not isinstance(self.cash_exact, Mapping):
            raise ValueError("cash_exact must be a mapping")
        for mapping_name, mapping in (
            ("positions", self.positions),
            ("research_targets", self.research_targets),
            ("positions_exact", self.positions_exact),
            ("cash_exact", self.cash_exact),
        ):
            for instrument in mapping:
                _require_text(instrument, f"{mapping_name}.instrument")
        for instrument, quantity in self.positions_exact.items():
            _validate_signed_exact_decimal(quantity, f"positions_exact[{instrument}]")
            if instrument in self.positions:
                projected_raw = int(
                    (Decimal(quantity) * Decimal(1_000_000_000)).to_integral_value(
                        rounding=ROUND_DOWN
                    )
                )
                if projected_raw != self.positions[instrument]:
                    raise ValueError(
                        f"positions_exact[{instrument}] conflicts with the raw position projection"
                    )
        for currency in self.cash_exact:
            _require_text(currency, "cash_exact.currency")
        for currency, amount in self.cash_exact.items():
            _validate_signed_exact_decimal(amount, f"cash_exact[{currency}]")
            if currency not in self.cash:
                raise ValueError(f"cash_exact[{currency}] is missing its raw cash projection")
            projected_raw = int(
                (Decimal(amount) * Decimal(1_000_000_000)).to_integral_value(
                    rounding=ROUND_DOWN
                )
            )
            if projected_raw != self.cash[currency]:
                raise ValueError(f"cash_exact[{currency}] conflicts with the raw cash projection")
        if self.available_margin_exact is not None:
            _validate_signed_exact_decimal(
                self.available_margin_exact, "available_margin_exact"
            )
            exact_margin = Decimal(self.available_margin_exact)
            if exact_margin < 0:
                raise ValueError("available_margin_exact cannot be negative")
            if self.available_margin_raw is None:
                raise ValueError("available_margin_exact is missing its raw projection")
            projected_raw = int(
                (exact_margin * Decimal(1_000_000_000)).to_integral_value(
                    rounding=ROUND_DOWN
                )
            )
            if projected_raw != self.available_margin_raw:
                raise ValueError("available_margin_exact conflicts with the raw projection")
        if self.bars is not None:
            self.bars.validate()
        for instrument, bars in self.market_bars.items():
            _require_text(instrument, "market_bars.instrument")
            if instrument == self.instrument:
                raise ValueError("market_bars cannot duplicate the primary instrument")
            bars.validate()
            if bars.ts[-1] > self.as_of:
                raise ValueError(f"market_bars[{instrument}] contains a bar after as_of")
        for index, update in enumerate(self.execution_updates):
            if not isinstance(update, Mapping):
                raise ValueError(f"execution_updates[{index}] must be an object")
            _validate_order_status_update(update, self.strategy_id, self.as_of, index)
        for index, update in enumerate(self.fill_updates):
            if not isinstance(update, Mapping):
                raise ValueError(f"fill_updates[{index}] must be an object")
            _validate_fill_update_v2(update, self.strategy_id, self.as_of, index)
        if len(self.market_ticks) > 10_000:
            raise ValueError("market_ticks cannot exceed 10000 events")
        previous_tick_key: tuple[int, str, int] | None = None
        tick_identities: set[tuple[str, int]] = set()
        for index, tick in enumerate(self.market_ticks):
            if not isinstance(tick, Mapping):
                raise ValueError(f"market_ticks[{index}] must be an object")
            _validate_market_tick_update(tick, self.as_of, index)
            identity = (tick["stream_id"], tick["event_seq"])
            if identity in tick_identities:
                raise ValueError("market_ticks contains a duplicate EventLog identity")
            tick_identities.add(identity)
            key = (tick["ts"], tick["stream_id"], tick["event_seq"])
            if previous_tick_key is not None and previous_tick_key >= key:
                raise ValueError("market_ticks must be ordered by timestamp, stream, and sequence")
            previous_tick_key = key
        if len(self.market_order_books) > 10_000:
            raise ValueError("market_order_books cannot exceed 10000 events")
        previous_book_key: tuple[int, str, int] | None = None
        book_identities: set[tuple[str, int]] = set()
        for index, book in enumerate(self.market_order_books):
            if not isinstance(book, Mapping):
                raise ValueError(f"market_order_books[{index}] must be an object")
            _validate_market_order_book(book, self.as_of, index)
            identity = (book["stream_id"], book["event_seq"])
            if identity in book_identities:
                raise ValueError("market_order_books contains a duplicate EventLog identity")
            book_identities.add(identity)
            key = (book["ts"], book["stream_id"], book["event_seq"])
            if previous_book_key is not None and previous_book_key >= key:
                raise ValueError("market_order_books must be ordered by timestamp, stream, and sequence")
            previous_book_key = key
        account_identities: set[tuple[str, str, int]] = set()
        previous_account_key: tuple[int, str, str, int] | None = None
        for index, update in enumerate(self.account_updates):
            if not isinstance(update, Mapping):
                raise ValueError(f"account_updates[{index}] must be an object")
            _validate_account_update(update, self.as_of, index)
            identity = (update["account_id"], update["venue_id"], update["event_seq"])
            if identity in account_identities:
                raise ValueError("account_updates contains a duplicate EventLog identity")
            account_identities.add(identity)
            key = (update["ts"], update["account_id"], update["venue_id"], update["event_seq"])
            if previous_account_key is not None and previous_account_key >= key:
                raise ValueError("account_updates must be ordered by time, account, venue, and sequence")
            previous_account_key = key
        if self.strategy_checkpoint is not None:
            _validate_strategy_checkpoint(
                self.strategy_checkpoint,
                self.strategy_id,
                self.strategy_version,
            )

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        result = {
            "schema_version": self.schema_version,
            "request_id": self.request_id,
            "strategy_id": self.strategy_id,
            "strategy_version": self.strategy_version,
            "data_fingerprint": self.data_fingerprint,
            "as_of": self.as_of,
            "instrument": self.instrument,
            "positions": dict(self.positions),
            "positions_exact": dict(self.positions_exact),
            "cash": dict(self.cash),
            "available_margin_raw": self.available_margin_raw,
            "risk_state": self.risk_state,
            "research_targets": dict(self.research_targets),
            "bars": None if self.bars is None else self.bars.to_dict(),
            "market_bars": {
                instrument: bars.to_dict() for instrument, bars in self.market_bars.items()
            },
        }
        if self.schema_version in (
            INPUT_V2_SCHEMA_VERSION,
            INPUT_V3_SCHEMA_VERSION,
            INPUT_V4_SCHEMA_VERSION,
            INPUT_V5_SCHEMA_VERSION,
            INPUT_V6_SCHEMA_VERSION,
            INPUT_V7_SCHEMA_VERSION,
            INPUT_V8_SCHEMA_VERSION,
            INPUT_V9_SCHEMA_VERSION,
        ) and self.execution_updates:
            result["execution_updates"] = [dict(update) for update in self.execution_updates]
        if self.schema_version in (
            INPUT_V3_SCHEMA_VERSION,
            INPUT_V4_SCHEMA_VERSION,
            INPUT_V5_SCHEMA_VERSION,
            INPUT_V6_SCHEMA_VERSION,
            INPUT_V7_SCHEMA_VERSION,
            INPUT_V8_SCHEMA_VERSION,
            INPUT_V9_SCHEMA_VERSION,
        ) and self.fill_updates:
            result["fill_updates"] = [dict(update) for update in self.fill_updates]
        if self.strategy_checkpoint is not None:
            result["strategy_checkpoint"] = dict(self.strategy_checkpoint)
        if self.schema_version in (INPUT_V6_SCHEMA_VERSION, INPUT_V7_SCHEMA_VERSION, INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION) and self.market_ticks:
            result["market_ticks"] = [dict(tick) for tick in self.market_ticks]
        if self.schema_version in (INPUT_V7_SCHEMA_VERSION, INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION) and self.account_updates:
            result["account_updates"] = [dict(update) for update in self.account_updates]
        if self.schema_version in (INPUT_V8_SCHEMA_VERSION, INPUT_V9_SCHEMA_VERSION):
            result["market_order_books"] = [dict(book) for book in self.market_order_books]
        if self.schema_version == INPUT_V9_SCHEMA_VERSION:
            if self.cash_exact:
                result["cash_exact"] = dict(self.cash_exact)
            if self.available_margin_exact is not None:
                result["available_margin_exact"] = self.available_margin_exact
        return result

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":"), ensure_ascii=False)

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "StrategyInput":
        bars_value = value.get("bars")
        result = cls(
            schema_version=value.get("schema_version", 0),
            request_id=str(value["request_id"]),
            strategy_id=str(value["strategy_id"]),
            strategy_version=str(value["strategy_version"]),
            data_fingerprint=str(value["data_fingerprint"]),
            as_of=int(value["as_of"]),
            instrument=str(value["instrument"]),
            positions={str(key): int(item) for key, item in dict(value.get("positions", {})).items()},
            positions_exact={
                str(key): str(item)
                for key, item in dict(value.get("positions_exact", {})).items()
            },
            cash={str(key): int(item) for key, item in dict(value.get("cash", {})).items()},
            cash_exact={
                str(key): str(item)
                for key, item in dict(value.get("cash_exact", {})).items()
            },
            available_margin_raw=(
                None if value.get("available_margin_raw") is None else int(value["available_margin_raw"])
            ),
            available_margin_exact=(
                None
                if value.get("available_margin_exact") is None
                else str(value["available_margin_exact"])
            ),
            risk_state=str(value["risk_state"]),
            research_targets={
                str(key): int(item) for key, item in dict(value.get("research_targets", {})).items()
            },
            bars=None if bars_value is None else StrategyBars.from_dict(bars_value),
            market_bars={
                str(instrument): StrategyBars.from_dict(bars)
                for instrument, bars in dict(value.get("market_bars", {})).items()
            },
            execution_updates=tuple(
                dict(update) for update in value.get("execution_updates", [])
            ),
            fill_updates=tuple(dict(update) for update in value.get("fill_updates", [])),
            strategy_checkpoint=(
                None
                if value.get("strategy_checkpoint") is None
                else dict(value["strategy_checkpoint"])
            ),
            market_ticks=tuple(dict(tick) for tick in value.get("market_ticks", [])),
            account_updates=tuple(dict(update) for update in value.get("account_updates", [])),
            market_order_books=tuple(dict(book) for book in value.get("market_order_books", [])),
        )
        _reject_unknown_keys(value, cls, "strategy input")
        result.validate()
        return result

    @classmethod
    def from_json(cls, payload: str) -> "StrategyInput":
        value = json.loads(payload)
        if not isinstance(value, dict):
            raise ValueError("strategy input must be a JSON object")
        return cls.from_dict(value)


@dataclass(frozen=True)
class StrategyIntent:
    """跨语言可交换的单笔订单意图；最终订单仍由 Rust Risk/OMS 创建。"""

    intent_id: int
    instrument: str
    side: str
    qty_raw: int
    account_id: str | None = None
    limit_price_raw: int | None = None
    reduce_only: bool = False
    post_only: bool = False
    position_side: str | None = None
    margin_mode: str | None = None
    position_mode: str | None = None
    leverage: int | None = None
    qty_exact: str | None = None
    limit_price_exact: str | None = None
    quote_budget_exact: str | None = None

    def __post_init__(self) -> None:
        # Normalize at construction/from_dict so in-memory bridge objects already
        # carry the same canonical tokens that Rust serializes and the schema lists.
        for name in STRATEGY_INTENT_ENUMS:
            value = getattr(self, name)
            if value is not None:
                object.__setattr__(self, name, _canonical_enum_case(value))

    def validate(self) -> None:
        _require_integer_range(self.intent_id, "intent.intent_id", 1, (1 << 64) - 1)
        _require_integer_range(self.qty_raw, "intent.qty_raw", -(1 << 127), (1 << 127) - 1)
        if self.limit_price_raw is not None:
            _require_integer_range(
                self.limit_price_raw,
                "intent.limit_price_raw",
                -(1 << 127),
                (1 << 127) - 1,
            )
        if self.leverage is not None:
            _require_integer_range(self.leverage, "intent.leverage", 0, (1 << 32) - 1)
            if self.leverage == 0:
                raise ValueError("intent.leverage must be positive")
        if not isinstance(self.reduce_only, bool) or not isinstance(self.post_only, bool):
            raise ValueError("intent reduce_only and post_only must be booleans")
        if self.intent_id <= 0:
            raise ValueError("intent_id must be positive")
        _require_contract_text(self.instrument, "intent.instrument")
        if self.account_id is not None:
            _require_contract_text(self.account_id, "intent.account_id")
        if self.side not in STRATEGY_INTENT_ENUMS["side"]:
            raise ValueError("intent.side must be buy or sell")
        if self.quote_budget_exact is not None:
            if (
                self.side != "buy"
                or self.qty_raw != 0
                or self.qty_exact is not None
                or self.limit_price_raw is not None
                or self.limit_price_exact is not None
            ):
                raise ValueError(
                    "intent.quote_budget_exact requires a standalone buy market order with qty_raw=0"
                )
            _validate_positive_exact_decimal(
                self.quote_budget_exact, "intent.quote_budget_exact"
            )
        elif self.qty_exact is None:
            if self.qty_raw <= 0:
                raise ValueError("intent.qty_raw must be positive")
        else:
            if self.qty_raw != 0:
                raise ValueError("intent.qty_raw must be 0 when qty_exact is set")
            _validate_positive_exact_decimal(self.qty_exact, "intent.qty_exact")
        if self.limit_price_exact is None and self.limit_price_raw is not None and self.limit_price_raw <= 0:
            raise ValueError("intent.limit_price_raw must be positive")
        if self.limit_price_exact is not None:
            if self.qty_exact is None or self.limit_price_raw is not None:
                raise ValueError("intent.limit_price_exact requires qty_exact and excludes limit_price_raw")
            _validate_positive_exact_decimal(self.limit_price_exact, "intent.limit_price_exact")
        if (
            self.position_side is not None
            and self.position_side not in STRATEGY_INTENT_ENUMS["position_side"]
        ):
            raise ValueError("intent.position_side must be net, long or short")
        # 三个衍生品档位与 Rust StrategyContractIntent::validate 同口径：空值表示沿用运行时
        # 主策略配置，非空值必须是运行时认识的规范小写形式，leverage 不允许 0（V12 §16 第三遍）。
        if (
            self.margin_mode is not None
            and self.margin_mode not in STRATEGY_INTENT_ENUMS["margin_mode"]
        ):
            raise ValueError("intent.margin_mode must be cash, cross or isolated")
        if (
            self.position_mode is not None
            and self.position_mode not in STRATEGY_INTENT_ENUMS["position_mode"]
        ):
            raise ValueError("intent.position_mode must be one_way or hedge")

    def to_dict(self) -> dict[str, Any]:
        self.validate()
        return {
            "intent_id": self.intent_id,
            "instrument": self.instrument,
            "account_id": self.account_id,
            "side": self.side,
            "qty_raw": self.qty_raw,
            "qty_exact": self.qty_exact,
            "quote_budget_exact": self.quote_budget_exact,
            "limit_price_raw": self.limit_price_raw,
            "limit_price_exact": self.limit_price_exact,
            "reduce_only": self.reduce_only,
            "post_only": self.post_only,
            "position_side": self.position_side,
            "margin_mode": self.margin_mode,
            "position_mode": self.position_mode,
            "leverage": self.leverage,
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "StrategyIntent":
        _reject_unknown_keys(value, cls, "strategy intent")
        required = {"intent_id", "instrument", "side", "qty_raw"}
        missing = sorted(required - set(value))
        if missing:
            raise ValueError(f"strategy intent missing required key(s): {', '.join(missing)}")
        result = cls(
            intent_id=value["intent_id"],
            instrument=value["instrument"],
            account_id=value.get("account_id"),
            side=value["side"],
            qty_raw=value["qty_raw"],
            qty_exact=None if value.get("qty_exact") is None else value["qty_exact"],
            quote_budget_exact=(
                None
                if value.get("quote_budget_exact") is None
                else value["quote_budget_exact"]
            ),
            limit_price_raw=value.get("limit_price_raw"),
            limit_price_exact=(
                None
                if value.get("limit_price_exact") is None
                else value["limit_price_exact"]
            ),
            reduce_only=value.get("reduce_only", False),
            post_only=value.get("post_only", False),
            position_side=value.get("position_side"),
            margin_mode=value.get("margin_mode"),
            position_mode=value.get("position_mode"),
            leverage=value.get("leverage"),
        )
        result.validate()
        return result


@dataclass(frozen=True)
class StrategyOutput:
    request_id: str
    strategy_id: str
    signal_id: int
    instrument: str
    target_qty: int
    target_qty_exact: str | None = None
    confidence: int = 0
    priority: int = 0
    expires_at: int = 0
    intents: tuple[StrategyIntent, ...] = field(default_factory=tuple)
    schema_version: int = SCHEMA_VERSION

    @classmethod
    def for_target_position(
        cls,
        request: StrategyInput,
        *,
        signal_id: int,
        target_qty: int | None = None,
        target_qty_exact: str | None = None,
        confidence: int = 0,
        priority: int = 0,
        expires_at: int = 0,
    ) -> "StrategyOutput":
        """Build an absolute signed-position decision bound to its input identity.

        Supply exactly one of ``target_qty`` (core i128 raw units) or
        ``target_qty_exact`` (a signed decimal string for scale-aware markets).
        Rebalancing, market-rule checks, risk and order creation remain runtime
        responsibilities.
        """
        if (target_qty is None) == (target_qty_exact is None):
            raise ValueError("provide exactly one of target_qty or target_qty_exact")
        output = cls(
            request_id=request.request_id,
            strategy_id=request.strategy_id,
            signal_id=signal_id,
            instrument=request.instrument,
            target_qty=0 if target_qty is None else target_qty,
            target_qty_exact=target_qty_exact,
            confidence=confidence,
            priority=priority,
            expires_at=expires_at,
        )
        output.validate_for(request)
        return output

    @classmethod
    def for_intents(
        cls,
        request: StrategyInput,
        *,
        signal_id: int,
        intents: Sequence[StrategyIntent],
        confidence: int = 0,
        priority: int = 0,
        expires_at: int = 0,
    ) -> "StrategyOutput":
        """Build a multi-order decision bound to the request's identity fields."""
        if not isinstance(intents, Sequence) or isinstance(intents, (str, bytes)):
            raise ValueError("intents must be a sequence of StrategyIntent values")
        output = cls(
            request_id=request.request_id,
            strategy_id=request.strategy_id,
            signal_id=signal_id,
            instrument=request.instrument,
            target_qty=0,
            confidence=confidence,
            priority=priority,
            expires_at=expires_at,
            intents=tuple(intents),
        )
        output.validate_for(request)
        return output

    def validate_for(self, request: StrategyInput) -> None:
        # 六种失败各说各话，而不是念成同一句 "identity or expiry does not match input"：
        # 返回 dict 时漏写 `schema_version`（`from_dict` 缺省读成 0）的作者按那句话去查
        # request_id，永远查不出问题；`StrategyInput.validate` 早就是一臂一句，输出侧不该更钝。
        request.validate()
        if (
            not isinstance(self.schema_version, int)
            or isinstance(self.schema_version, bool)
            or self.schema_version != SCHEMA_VERSION
        ):
            raise ValueError(
                f"strategy output schema_version must be {SCHEMA_VERSION}, got {self.schema_version}"
            )
        for name, value in (
            ("request_id", self.request_id),
            ("strategy_id", self.strategy_id),
            ("instrument", self.instrument),
        ):
            _require_contract_text(value, f"strategy output {name}")
        if self.request_id != request.request_id:
            raise ValueError("strategy output request_id does not match input")
        if self.strategy_id != request.strategy_id:
            raise ValueError("strategy output strategy_id does not match input")
        if not isinstance(self.signal_id, int) or isinstance(self.signal_id, bool):
            raise ValueError("strategy output signal_id must be an integer")
        if self.signal_id <= 0:
            raise ValueError("strategy output signal_id must be positive")
        if self.signal_id > (1 << 64) - 1:
            raise ValueError("strategy output signal_id exceeds u64")
        if self.instrument != request.instrument:
            raise ValueError("strategy output instrument does not match input")
        _require_integer_range(
            self.target_qty, "strategy output target_qty", -(1 << 127), (1 << 127) - 1
        )
        _require_integer_range(
            self.confidence, "strategy output confidence", -(1 << 127), (1 << 127) - 1
        )
        _require_integer_range(
            self.priority, "strategy output priority", -(1 << 31), (1 << 31) - 1
        )
        _require_integer_range(
            self.expires_at, "strategy output expires_at", 0, (1 << 64) - 1
        )
        if self.expires_at != 0 and self.expires_at < request.as_of:
            raise ValueError("strategy output expires_at must not precede input as_of")
        if self.target_qty_exact is not None:
            if self.target_qty != 0:
                raise ValueError("target_qty must be zero when target_qty_exact is set")
            _validate_signed_exact_decimal(self.target_qty_exact, "target_qty_exact")
            if self.intents:
                raise ValueError("target_qty_exact cannot be combined with intents")
        if not isinstance(self.intents, (tuple, list)):
            raise ValueError("strategy output intents must be a sequence")
        intent_ids: set[int] = set()
        for intent in self.intents:
            if not isinstance(intent, StrategyIntent):
                raise ValueError("strategy output intents must contain StrategyIntent values")
            intent.validate()
            if intent.intent_id in intent_ids:
                raise ValueError("strategy output contains duplicate intent_id")
            intent_ids.add(intent.intent_id)

    def to_dict(self, request: StrategyInput) -> dict[str, Any]:
        self.validate_for(request)
        return {
            "schema_version": self.schema_version,
            "request_id": self.request_id,
            "strategy_id": self.strategy_id,
            "signal_id": self.signal_id,
            "instrument": self.instrument,
            "target_qty": self.target_qty,
            "target_qty_exact": self.target_qty_exact,
            "confidence": self.confidence,
            "priority": self.priority,
            "expires_at": self.expires_at,
            "intents": [intent.to_dict() for intent in self.intents],
        }

    def to_json(self, request: StrategyInput) -> str:
        return json.dumps(self.to_dict(request), separators=(",", ":"), ensure_ascii=False)

    @classmethod
    def from_dict(cls, value: Mapping[str, Any], request: StrategyInput) -> "StrategyOutput":
        _reject_unknown_keys(value, cls, "strategy output")
        required = {
            "request_id",
            "strategy_id",
            "signal_id",
            "instrument",
            "target_qty",
            "confidence",
            "priority",
            "expires_at",
        }
        missing = sorted(required - set(value))
        if missing:
            raise ValueError(f"strategy output missing required key(s): {', '.join(missing)}")
        intents = value.get("intents", [])
        if not isinstance(intents, list):
            raise ValueError("strategy output intents must be an array")
        result = cls(
            schema_version=value.get("schema_version", 0),
            request_id=value["request_id"],
            strategy_id=value["strategy_id"],
            signal_id=value["signal_id"],
            instrument=value["instrument"],
            target_qty=value["target_qty"],
            target_qty_exact=(
                None if value.get("target_qty_exact") is None else value["target_qty_exact"]
            ),
            confidence=value.get("confidence", 0),
            priority=value.get("priority", 0),
            expires_at=value.get("expires_at", 0),
            intents=tuple(StrategyIntent.from_dict(item) for item in intents),
        )
        result.validate_for(request)
        return result

    @classmethod
    def from_json(cls, payload: str, request: StrategyInput) -> "StrategyOutput":
        value = json.loads(payload)
        if not isinstance(value, dict):
            raise ValueError("strategy output must be a JSON object")
        return cls.from_dict(value, request)
