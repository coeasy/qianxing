"""公共 CCXT 连接层。

该模块只依赖 CCXT 的统一 Python API，不在项目内重复实现交易所签名、REST
路径或 WebSocket 协议。核心系统接收的是已经归一化的 BarFrame、行情、订单
确认和账户余额；交易所差异保留在 CCXT 的 market/params 字段与错误分类中。

CCXT Pro 是可选能力：REST 数据源和交易执行只需要 ``ccxt``，实时 ``watch_*``
流由调用方显式注入已创建的 CCXT Pro exchange 实例；JSONL worker 会对网络/限频错误
执行有限次重建连接和退避，不会重试认证、参数或未支持能力错误。
"""

from __future__ import annotations

import importlib
import hashlib
import inspect
import json
from fractions import Fraction
from math import gcd
import time
from dataclasses import dataclass
from enum import Enum
from typing import Any, Mapping, Sequence

from qianxing_bridge import BarFrame, native


MAX_OHLCV_ROWS = 100_000
SUPPORTED_CCXT_VERSION = "4.5.89"
LIVE_MARKET_METADATA_MAX_AGE_SECONDS = 60


def __getattr__(name: str) -> Any:
    if name == "SCALE":
        return native.raw_scale()
    raise AttributeError(name)


class CcxtErrorClass(str, Enum):
    RETRYABLE = "retryable"
    RATE_LIMITED = "rate_limited"
    AUTHENTICATION = "authentication"
    EXCHANGE = "exchange"
    UNSUPPORTED = "unsupported"
    INVALID = "invalid"
    UNKNOWN = "unknown"


class CcxtConnectorError(RuntimeError):
    def __init__(self, error_class: CcxtErrorClass, message: str, *, cause: Exception | None = None):
        super().__init__(message)
        self.error_class = error_class
        self.cause = cause


def classify_ccxt_error(error: Exception) -> CcxtErrorClass:
    """把 CCXT 异常映射到运行时稳定的错误类别。"""
    return CcxtErrorClass(native.classify_ccxt_error(type(error).__name__))


def _raw_decimal(value: Any, *, field: str) -> int:
    try:
        return int(native.decimal_to_raw_exact(str(value), False))
    except (ValueError, OverflowError) as error:
        if "precision" in str(error):
            message = f"{field} 超过牵星定点精度 1e-9: {value}"
        elif "non-negative" in str(error):
            message = f"{field} 必须是非负有限数字: {value}"
        else:
            message = f"{field} 不是有效数字: {value}"
        raise CcxtConnectorError(CcxtErrorClass.INVALID, message) from error


def _raw_decimal_projection(value: Any, *, field: str) -> int:
    """Project a decimal onto the legacy 1e-9 raw grid toward zero, without float math."""
    try:
        scaled = Fraction(str(value)) * int(native.raw_scale())
        raw = abs(scaled.numerator) // scaled.denominator
        if scaled < 0:
            raw = -raw
        if not -(1 << 127) <= raw < (1 << 127):
            raise OverflowError("raw projection out of range")
        return raw
    except (TypeError, ValueError, ZeroDivisionError, OverflowError) as error:
        raise CcxtConnectorError(
            CcxtErrorClass.INVALID, f"{field} 无法投影到兼容 raw 字段"
        ) from error


def _decimal_fraction(value: Any, *, field: str) -> tuple[int, int]:
    try:
        fraction = Fraction(str(value))
        if fraction < 0:
            raise ValueError("must be a finite non-negative decimal")
        return fraction.numerator, fraction.denominator
    except (TypeError, ValueError, ZeroDivisionError, OverflowError) as error:
        raise CcxtConnectorError(
            CcxtErrorClass.INVALID, f"{field} 不是有效的非负有限数字"
        ) from error


def _exact_decimal_text(
    value: Any, *, field: str, allow_negative: bool = False
) -> str | None:
    """Format a finite decimal as canonical text without float arithmetic or rounding."""
    try:
        fraction = Fraction(str(value))
        if fraction < 0 and not allow_negative:
            raise ValueError("must be non-negative")
        negative = fraction < 0
        numerator = abs(fraction.numerator)
        denominator = fraction.denominator
        twos = fives = 0
        while denominator % 2 == 0:
            denominator //= 2
            twos += 1
        while denominator % 5 == 0:
            denominator //= 5
            fives += 1
        if denominator != 1:
            return None
        scale = max(twos, fives)
        if scale > 18:
            return None
        coefficient = numerator * (2 ** (scale - twos)) * (5 ** (scale - fives))
        if coefficient > ((1 << 127) if negative else (1 << 127) - 1):
            return None
        while scale and coefficient and coefficient % 10 == 0:
            coefficient //= 10
            scale -= 1
        sign = "-" if negative and coefficient else ""
        if scale == 0:
            return f"{sign}{coefficient}"
        digits = str(coefficient).rjust(scale + 1, "0")
        return f"{sign}{digits[:-scale]}.{digits[-scale:]}"
    except (TypeError, ValueError, ZeroDivisionError, OverflowError):
        return None


def _exact_decimal_wire_text(value: Any, *, field: str, allow_negative: bool = False) -> str:
    """Decode an exact Rust decimal object or a plain decimal string without float conversion."""
    if isinstance(value, Mapping):
        coefficient = value.get("coefficient")
        scale = value.get("scale")
        if isinstance(scale, bool) or not isinstance(scale, int) or not 0 <= scale <= 18:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 精确小数 wire 非法")
        try:
            parsed_coefficient = int(coefficient)
        except (TypeError, ValueError, OverflowError) as error:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID, f"{field} 精确小数 coefficient 非法"
            ) from error
        if str(parsed_coefficient) != coefficient:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID, f"{field} 精确小数 coefficient 必须规范编码"
            )
        if (parsed_coefficient == 0 and scale != 0) or (
            parsed_coefficient != 0 and scale > 0 and parsed_coefficient % 10 == 0
        ):
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID, f"{field} 精确小数必须使用规范 scale"
            )
        magnitude = str(abs(parsed_coefficient))
        negative = parsed_coefficient < 0
        digits = magnitude.rjust(scale + 1, "0")
        text = ("-" if negative else "") + (
            digits if scale == 0 else f"{digits[:-scale]}.{digits[-scale:]}"
        )
    elif isinstance(value, str):
        text = value
    else:
        raise CcxtConnectorError(
            CcxtErrorClass.INVALID,
            f"{field} 必须使用精确小数字符串或 coefficient/scale 对象",
        )
    normalized = _exact_decimal_text(text, field=field, allow_negative=allow_negative)
    if normalized is None:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不是有效的 18 位精确十进制")
    return normalized


def _exact_precision_step(value: Any, *, mode: Any, tick_size_mode: Any, decimal_places_mode: Any) -> str | None:
    if value is None:
        return None
    if mode == tick_size_mode:
        text = _exact_decimal_text(value, field="precision")
        if text is None or Fraction(text) <= 0:
            return None
        return text


    if mode == decimal_places_mode:
        try:
            places_fraction = Fraction(str(value))
        except (TypeError, ValueError, OverflowError):
            return None
        if places_fraction.denominator != 1:
            return None
        places = places_fraction.numerator
        if places < 0 or places > 18:
            return None
        return _exact_decimal_text(Fraction(1, 10**places), field="precision")
    # Significant-digit precision has no fixed step independent of order value.
    return None


def _exact_decimal_wire(value: Any, *, field: str) -> dict[str, str | int]:
    text = _exact_decimal_text(value, field=field)
    if text is None:
        raise CcxtConnectorError(
            CcxtErrorClass.INVALID,
            f"{field} 超过牵星支持的 18 位精确小数范围: {value}",
        )
    unsigned = text.lstrip("-")
    if "." in unsigned:
        whole, fraction = unsigned.split(".", 1)
        coefficient = whole + fraction
        scale = len(fraction)
    else:
        coefficient = unsigned
        scale = 0
    if text.startswith("-"):
        coefficient = "-" + coefficient
    return {"coefficient": str(int(coefficient)), "scale": scale}


def _raw_decimal_bound(value: Any, *, field: str, round_up: bool) -> int | None:
    """Map a non-negative exchange bound to the 1e-9 grid conservatively.

    Minimum bounds round upward and maximum bounds downward. A positive bound finer than
    1e-9 can therefore become raw zero; callers must preserve that zero as a real bound,
    because it means no positive internal order can satisfy the exchange limit.
    """
    numerator, denominator = _decimal_fraction(value, field=field)
    if numerator == 0:
        return None
    raw, remainder = divmod(numerator * native.raw_scale(), denominator)
    if round_up and remainder:
        raw += 1
    if raw > (1 << 127) - 1:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 超出 i128 范围")
    return raw


def _raw_lattice_step(value: Any, *, field: str) -> int | None:
    """Return the smallest 1e-9 raw step that is exactly on the exchange tick lattice.

    This is the exact intersection of the exchange's rational tick and the core's 1e-9
    representable grid. It never rounds the exchange tick. For example, a 3e-10 tick maps
    to a raw step of 3, while a 1e-10 tick maps to 1.
    """
    numerator, denominator = _decimal_fraction(value, field=field)
    if numerator == 0:
        return None
    scaled_numerator = numerator * native.raw_scale()
    step = scaled_numerator // gcd(denominator, scaled_numerator)
    if step <= 0 or step > (1 << 127) - 1:
        raise CcxtConnectorError(CcxtErrorClass.UNSUPPORTED, f"{field} 无法映射到牵星定点网格")
    return step


def _market_order_notional_raw(market: "CcxtMarket", amount_raw: int, price_raw: int) -> int:
    scale = native.raw_scale()
    if market.inverse:
        return amount_raw * market.contract_size_raw // scale
    return amount_raw * market.contract_size_raw * price_raw // (scale * scale)


def _market_order_notional_ceil_raw(
    market: "CcxtMarket", amount_raw: int, price_raw: int
) -> int:
    """Round exposure upward so tier selection cannot understate fractional raw units."""
    scale = native.raw_scale()
    if market.inverse:
        numerator, denominator = amount_raw * market.contract_size_raw, scale
    else:
        numerator = amount_raw * market.contract_size_raw * price_raw
        denominator = scale * scale
    return (numerator + denominator - 1) // denominator


def _signed_raw_decimal(value: Any, *, field: str) -> int:
    """把允许正负号的账单金额转成定点整数。"""
    try:
        return int(native.decimal_to_raw_exact(str(value), True))
    except (ValueError, OverflowError) as error:
        if "precision" in str(error):
            message = f"{field} 超过牵星定点精度 1e-9: {value}"
        else:
            message = f"{field} 不是有效有限数字: {value}"
        raise CcxtConnectorError(CcxtErrorClass.INVALID, message) from error


def _decimal_from_raw(value: int, *, field: str) -> str:
    try:
        return native.raw_to_decimal(str(value), False)
    except (ValueError, OverflowError) as error:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不能为负或超出范围: {value}") from error


def _decimal_to_integer(value: Any, multiplier: int, *, field: str) -> int:
    try:
        return native.decimal_to_scaled_integer(str(value), multiplier)
    except (ValueError, OverflowError) as error:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不是可精确转换的数字: {value}") from error


def _positive_integer(value: Any, *, field: str) -> int:
    try:
        parsed = native.decimal_to_integer_exact(str(value))
    except (TypeError, ValueError, OverflowError) as error:
        raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"{field} 缺失或非法") from error
    if parsed <= 0:
        raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"{field} 缺失或非法")
    return parsed


@dataclass(frozen=True)
class CcxtConfig:
    exchange_id: str
    api_key: str = ""
    secret: str = ""
    password: str = ""
    uid: str = ""
    sandbox: bool = False
    enable_rate_limit: bool = True
    timeout_ms: int = 30_000
    default_type: str = "spot"
    options: Mapping[str, Any] | None = None
    ws_max_retries: int = 3
    ws_retry_backoff_ms: int = 250
    account_model: str = ""

    def validate(self) -> None:
        if not self.exchange_id.strip() or not self.exchange_id.replace("_", "").isalnum():
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "exchange_id 非法")
        if self.timeout_ms <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "timeout_ms 必须大于 0")
        if not self.default_type.strip():
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "default_type 不能为空")
        if self.account_model and self.account_model not in {
            "spot",
            "usd_m_futures",
            "unified",
            "classic",
            "uta",
        }:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "account_model 不受支持")
        if not 0 <= self.ws_max_retries <= 10:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "ws_max_retries 必须在 0..10 内")
        if not 0 <= self.ws_retry_backoff_ms <= 60_000:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID, "ws_retry_backoff_ms 必须在 0..60000 内"
            )


def _apply_sandbox_mode(exchange: Any, config: CcxtConfig) -> None:
    if not config.sandbox:
        return
    setter = getattr(exchange, "set_sandbox_mode", None)
    if not callable(setter):
        raise CcxtConnectorError(
            CcxtErrorClass.UNSUPPORTED,
            f"CCXT {config.exchange_id} 未提供 sandbox 切换能力，拒绝按沙盒配置启动",
        )
    try:
        setter(True)
    except Exception as error:
        raise CcxtConnectorError(
            CcxtErrorClass.UNSUPPORTED,
            f"CCXT {config.exchange_id} 无法启用 sandbox 模式，拒绝按沙盒配置启动",
            cause=error,
        ) from error


@dataclass(frozen=True)
class CcxtMarket:
    exchange_id: str
    symbol: str
    market_id: str
    base: str
    quote: str
    market_type: str
    active: bool | None
    contract: bool
    linear: bool | None
    inverse: bool | None
    contract_size_raw: int
    settle: str | None
    expiry_ms: int | None
    price_tick_raw: int | None
    min_price_raw: int | None
    max_price_raw: int | None
    qty_step_raw: int | None
    market_order_qty_step_raw: int | None
    min_qty_raw: int | None
    market_order_min_qty_raw: int | None
    min_notional_raw: int | None
    market_order_min_notional_raw: int | None
    max_notional_raw: int | None
    market_order_max_notional_raw: int | None
    market_order_notional_filter_present: bool
    max_qty_raw: int | None
    market_order_max_qty_raw: int | None
    max_leverage: int | None
    maintenance_margin_bps: int | None
    # Decimal strings retain exchange precision beyond the legacy 1e-9 raw grid.
    # Exact Spot request formatting consumes these rules; Rust keeps live order admission
    # closed until risk, fill, and ledger paths are all scale-aware.
    exact_rules: Mapping[str, str] | None

    @property
    def instrument(self) -> str:
        return f"{self.symbol}.{self.exchange_id.upper()}"


@dataclass(frozen=True)
class CcxtLeverageTier:
    symbol: str
    tier: int
    min_notional_raw: int
    max_notional_raw: int | None
    min_notional_exact: str
    max_notional_exact: str | None
    exposure_unit: str
    initial_margin_bps: int
    maintenance_margin_bps: int
    max_leverage: int | None


@dataclass(frozen=True)
class CcxtOrder:
    exchange_id: str
    order_id: str
    client_order_id: str
    symbol: str
    status: str
    side: str
    order_type: str
    amount_raw: int
    filled_raw: int
    remaining_raw: int
    price_raw: int | None
    average_raw: int | None
    fee_raw: int
    fee_currency: str | None
    fees: tuple[tuple[int, str], ...]
    timestamp_ms: int | None
    raw: Mapping[str, Any]
    amount_exact: str
    filled_exact: str
    remaining_exact: str
    price_exact: str | None
    average_exact: str | None
    cost_exact: str | None
    exact_fees: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class CcxtPosition:
    exchange_id: str
    symbol: str
    settlement_currency: str | None
    side: str
    contracts_raw: int
    contract_size_raw: int
    entry_price_raw: int | None
    mark_price_raw: int | None
    liquidation_price_raw: int | None
    unrealized_pnl_raw: int | None
    initial_margin_raw: int | None
    maintenance_margin_raw: int | None
    leverage: int | None
    margin_mode: str | None
    hedged: bool | None
    contracts_exact: str | None
    contract_size_exact: str | None
    entry_price_exact: str | None
    mark_price_exact: str | None
    liquidation_price_exact: str | None
    unrealized_pnl_exact: str | None
    initial_margin_exact: str | None
    maintenance_margin_exact: str | None
    raw: Mapping[str, Any]


@dataclass(frozen=True)
class CcxtFundingRate:
    exchange_id: str
    symbol: str
    timestamp_ms: int | None
    funding_rate_bps: int
    next_funding_timestamp_ms: int | None
    raw: Mapping[str, Any]


@dataclass(frozen=True)
class CcxtCashflow:
    exchange_id: str
    external_id: str
    currency: str
    kind: str
    amount_raw: int
    timestamp_ms: int | None
    raw: Mapping[str, Any]


@dataclass(frozen=True)
class CcxtTrade:
    exchange_id: str
    trade_id: str
    order_id: str | None
    symbol: str
    side: str
    amount_raw: int
    price_raw: int
    fee_raw: int
    fee_currency: str | None
    fees: tuple[tuple[int, str], ...]
    timestamp_ms: int | None
    raw: Mapping[str, Any]
    amount_exact: str
    price_exact: str
    cost_exact: str | None
    exact_fees: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class CcxtTicker:
    exchange_id: str
    symbol: str
    timestamp_ms: int
    bid_raw: int
    ask_raw: int
    bid_qty_raw: int
    ask_qty_raw: int
    last_raw: int
    bid_exact: str | None = None
    ask_exact: str | None = None
    bid_qty_exact: str | None = None
    ask_qty_exact: str | None = None


@dataclass(frozen=True)
class CcxtOrderBookLevel:
    price_raw: int
    qty_raw: int
    price_exact: str
    qty_exact: str


@dataclass(frozen=True)
class CcxtOrderBook:
    exchange_id: str
    symbol: str
    timestamp_ms: int | None
    sequence: int | None
    bids: tuple[CcxtOrderBookLevel, ...]
    asks: tuple[CcxtOrderBookLevel, ...]


def _ccxt_constructor_params(config: CcxtConfig) -> dict[str, Any]:
    return {
        "apiKey": config.api_key,
        "secret": config.secret,
        "password": config.password,
        "uid": config.uid,
        "enableRateLimit": config.enable_rate_limit,
        "timeout": config.timeout_ms,
        "options": {"defaultType": config.default_type, **dict(config.options or {})},
    }


def _stream_symbol(instrument: str | None) -> str | None:
    if instrument is None:
        return None
    symbol, separator, _ = str(instrument).rpartition(".")
    if not separator or not symbol.strip():
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"Instrument {instrument} 非法")
    return symbol


def _stream_order(value: Any, exchange_id: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT Pro order 事件格式非法")
    order_id = str(value.get("id") or "").strip()
    if not order_id:
        raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT Pro order 事件缺少 id")
    amount = value.get("amount", 0)
    filled = value.get("filled", 0)
    remaining = value.get("remaining")
    return {
        "exchange_id": exchange_id,
        "order_id": order_id,
        "client_order_id": str(value.get("clientOrderId") or ""),
        "instrument": f"{str(value.get('symbol') or '').strip()}.{exchange_id.upper()}",
        "symbol": str(value.get("symbol") or ""),
        "status": str(value.get("status") or "unknown"),
        "side": str(value.get("side") or "unknown"),
        "order_type": str(value.get("type") or "unknown"),
        "amount_raw": _raw_decimal(amount, field="stream.amount"),
        "filled_raw": _raw_decimal(filled, field="stream.filled"),
        "remaining_raw": _raw_decimal(
            amount if remaining is None else remaining, field="stream.remaining"
        ),
        "price_raw": None
        if value.get("price") is None
        else _raw_decimal(value["price"], field="stream.price"),
        "average_raw": None
        if value.get("average") is None
        else _raw_decimal(value["average"], field="stream.average"),
        "fee_raw": 0,
        "fee_currency": None,
        "fees": [],
        "timestamp_ms": None if value.get("timestamp") is None else int(value["timestamp"]),
        "raw": value,
    }


def _stream_ticker(value: Any, exchange_id: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT Pro ticker 事件格式非法")
    timestamp = value.get("timestamp")
    if timestamp is None:
        raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT Pro ticker 缺少 timestamp")
    return {
        "exchange_id": exchange_id,
        "symbol": str(value.get("symbol") or ""),
        "instrument": f"{str(value.get('symbol') or '').strip()}.{exchange_id.upper()}",
        "timestamp_ms": int(timestamp),
        "bid_raw": _raw_decimal_projection(value.get("bid", 0), field="stream.bid"),
        "ask_raw": _raw_decimal_projection(value.get("ask", 0), field="stream.ask"),
        "bid_qty_raw": _raw_decimal_projection(value.get("bidVolume") or 0, field="stream.bidVolume"),
        "ask_qty_raw": _raw_decimal_projection(value.get("askVolume") or 0, field="stream.askVolume"),
        "bid_exact": _exact_decimal_text(value.get("bid"), field="stream.bid"),
        "ask_exact": _exact_decimal_text(value.get("ask"), field="stream.ask"),
        "bid_qty_exact": None
        if value.get("bidVolume") is None
        else _exact_decimal_text(value["bidVolume"], field="stream.bidVolume"),
        "ask_qty_exact": None
        if value.get("askVolume") is None
        else _exact_decimal_text(value["askVolume"], field="stream.askVolume"),
        "last_raw": _raw_decimal(value.get("last", 0), field="stream.last"),
        "raw": value,
    }


async def _await_if_needed(value: Any) -> Any:
    return await value if inspect.isawaitable(value) else value


def create_ccxt_pro_exchange(config: CcxtConfig, *, ccxt_pro_module: Any | None = None) -> Any:
    """按同一 CcxtConfig 创建公共 CCXT Pro exchange 实例。"""

    config.validate()
    module = ccxt_pro_module
    if module is None:
        try:
            module = importlib.import_module("ccxt.pro")
        except ImportError:
            try:
                module = importlib.import_module("ccxtpro")
            except ImportError as error:
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    "未安装公共 CCXT Pro；请安装 qianxing[ccxt-pro] 或 ccxtpro 包",
                    cause=error,
                ) from error
    exchange_type = getattr(module, config.exchange_id, None)
    if exchange_type is None:
        raise CcxtConnectorError(
            CcxtErrorClass.UNSUPPORTED,
            f"CCXT Pro 不支持交易所: {config.exchange_id}",
        )
    exchange = exchange_type(_ccxt_constructor_params(config))
    _apply_sandbox_mode(exchange, config)
    return exchange


async def watch_stream(
    exchange: Any,
    stream: str,
    *,
    instrument: str | None = None,
    timeframe: str = "1m",
    limit: int | None = None,
    params: Mapping[str, Any] | None = None,
) -> Any:
    """调用公共 CCXT Pro 的单次 watch_*，不在此层伪造轮询。"""

    name = stream.strip().lower()
    method_name = {
        "ohlcv": "watch_ohlcv",
        "ticker": "watch_ticker",
        "orders": "watch_orders",
        "my_trades": "watch_my_trades",
        "balance": "watch_balance",
        "positions": "watch_positions",
    }.get(name)
    if method_name is None:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"未知 CCXT Pro stream: {stream}")
    method = getattr(exchange, method_name, None)
    if method is None or not callable(method):
        raise CcxtConnectorError(
            CcxtErrorClass.UNSUPPORTED,
            f"当前连接未提供 CCXT Pro {method_name}",
        )
    symbol = _stream_symbol(instrument)
    call_params = dict(params or {})
    try:
        if name == "ohlcv":
            return await _await_if_needed(method(symbol, timeframe, None, limit, call_params))
        if name == "ticker":
            return await _await_if_needed(method(symbol, call_params))
        if name in {"orders", "my_trades"}:
            return await _await_if_needed(method(symbol, None, limit, call_params))
        if name == "positions":
            symbols = None if symbol is None else [symbol]
            return await _await_if_needed(method(symbols, call_params))
        return await _await_if_needed(method(call_params))
    except CcxtConnectorError:
        raise
    except Exception as error:
        category = classify_ccxt_error(error)
        raise CcxtConnectorError(category, f"CCXT Pro {method_name} 失败: {error}", cause=error) from error


def normalize_stream_event(
    stream: str, payload: Any, *, exchange_id: str, received_ts: int | None = None
) -> dict[str, Any]:
    """将 CCXT Pro 单次回报封装成可审计 JSONL 事件。"""

    received = int(time.time() * 1_000) if received_ts is None else int(received_ts)
    name = stream.strip().lower()
    values = payload if isinstance(payload, list) else [payload]
    if name == "orders":
        events = [_stream_order(value, exchange_id) for value in values]
    elif name == "ticker":
        events = [_stream_ticker(value, exchange_id) for value in values]
    else:
        events = values
    return {
        "stream": name,
        "exchange_id": exchange_id,
        "received_ts": received,
        "events": events,
    }


class CcxtExchangeClient:
    """基于公共 CCXT Python API 的多交易所客户端。

    ``exchange`` 和 ``ccxt_module`` 参数用于注入测试 double；生产调用不传入时
    才会惰性加载公共 ``ccxt`` 包并按 exchange_id 实例化对应交易所。
    """

    def __init__(
        self,
        config: CcxtConfig,
        *,
        exchange: Any | None = None,
        ccxt_module: Any | None = None,
    ) -> None:
        config.validate()
        self.config = config
        if exchange is not None:
            self.exchange = exchange
        else:
            module = ccxt_module or self._load_ccxt()
            exchange_type = getattr(module, config.exchange_id, None)
            if exchange_type is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    f"CCXT 不支持交易所: {config.exchange_id}",
                )
            params: dict[str, Any] = {
                "apiKey": config.api_key,
                "secret": config.secret,
                "password": config.password,
                "uid": config.uid,
                "enableRateLimit": config.enable_rate_limit,
                "timeout": config.timeout_ms,
                "options": {"defaultType": config.default_type, **dict(config.options or {})},
            }
            self.exchange = exchange_type(params)
        _apply_sandbox_mode(self.exchange, config)
        self._markets: Mapping[str, Mapping[str, Any]] | None = None
        self._markets_loaded_at: float | None = None

    @staticmethod
    def _load_ccxt() -> Any:
        try:
            module = importlib.import_module("ccxt")
        except ImportError as error:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "未安装公共 ccxt；请安装 qianxing[ccxt] 或 ccxt 包",
                cause=error,
            ) from error
        version = getattr(module, "__version__", None)
        if version != SUPPORTED_CCXT_VERSION:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"当前 CCXT 版本 {version!r} 未经过交易参数契约核验；"
                f"本版本只支持 {SUPPORTED_CCXT_VERSION}，请安装 qianxing[ccxt]",
            )
        return module

    def _call(self, method: str, *args: Any, **kwargs: Any) -> Any:
        function = getattr(self.exchange, method, None)
        if function is None or not callable(function):
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"交易所 {self.config.exchange_id} 不支持 CCXT 方法 {method}",
            )
        try:
            return function(*args, **kwargs)
        except CcxtConnectorError:
            raise
        except Exception as error:
            category = classify_ccxt_error(error)
            raise CcxtConnectorError(category, f"CCXT {method} 失败: {error}", cause=error) from error

    def load_markets(self, *, reload: bool = False) -> Mapping[str, Mapping[str, Any]]:
        if self._markets is None or reload:
            markets = self._call("load_markets", reload)
            if not isinstance(markets, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT markets 返回类型非法")
            self._markets = markets
            self._markets_loaded_at = time.monotonic()
        return self._markets

    def resolve_market(self, instrument: str) -> CcxtMarket:
        text = instrument.strip()
        symbol_text, separator, venue = text.rpartition(".")
        if not separator or venue.upper() != self.config.exchange_id.upper():
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"Instrument {instrument} 未绑定当前 CCXT exchange {self.config.exchange_id}",
            )
        markets = self.load_markets()
        candidates = list(markets.values())
        market = markets.get(symbol_text)
        if market is None:
            upper = symbol_text.upper()
            market = next(
                (
                    candidate
                    for candidate in candidates
                    if str(candidate.get("symbol", "")).upper() == upper
                    or str(candidate.get("id", "")).upper() == upper
                    or (
                        str(candidate.get("base", "")).upper()
                        + str(candidate.get("quote", "")).upper()
                    )
                    == upper.replace("/", "")
                ),
                None,
            )
        if market is None:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, f"CCXT market 不存在: {instrument}")
        symbol = str(market.get("symbol", "")).strip()
        market_id = str(market.get("id", symbol)).strip()
        base = str(market.get("base", "")).strip()
        quote = str(market.get("quote", "")).strip()
        if not symbol or not base or not quote:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"CCXT market 字段不完整: {market}")
        market_type = str(market.get("type") or market.get("spot") and "spot" or self.config.default_type)
        contract = bool(market.get("contract", False))
        # CCXT spot markets commonly expose ``contractSize: None`` while
        # omitted contractSize and explicit one both mean one base unit.
        contract_size = market.get("contractSize")
        if contract_size is None:
            contract_size = 1
        contract_size_raw = _raw_decimal(contract_size, field="contractSize")
        limits = market.get("limits")
        limits = limits if isinstance(limits, Mapping) else {}
        precision = market.get("precision")
        precision = precision if isinstance(precision, Mapping) else {}
        price_tick_raw = self._precision_step_raw(precision.get("price"))
        qty_step_raw = self._precision_step_raw(precision.get("amount"))
        price_limits = limits.get("price")
        price_limits = price_limits if isinstance(price_limits, Mapping) else {}
        min_price_raw = (
            None
            if price_limits.get("min") is None
            else _raw_decimal_bound(
                price_limits["min"], field="limits.price.min", round_up=True
            )
        )
        max_price_raw = (
            None
            if price_limits.get("max") is None
            else _raw_decimal_bound(
                price_limits["max"], field="limits.price.max", round_up=False
            )
        )
        market_min_price_raw, market_max_price_raw = self._market_price_constraints(market)
        if market_min_price_raw is not None:
            min_price_raw = market_min_price_raw
        if market_max_price_raw is not None:
            max_price_raw = market_max_price_raw
        (
            market_qty_step_raw,
            market_min_qty_raw,
            market_max_qty_raw,
            filter_min_notional_raw,
            filter_max_notional_raw,
            market_min_notional_raw,
            market_max_notional_raw,
            market_notional_filter_present,
        ) = self._market_order_constraints(market)
        amount_limits = limits.get("amount")
        amount_limits = amount_limits if isinstance(amount_limits, Mapping) else {}
        min_qty_raw = (
            None
            if amount_limits.get("min") is None
            else _raw_decimal_bound(
                amount_limits["min"], field="limits.amount.min", round_up=True
            )
        )
        cost_limits = limits.get("cost")
        cost_limits = cost_limits if isinstance(cost_limits, Mapping) else {}
        min_notional_raw = (
            None
            if filter_min_notional_raw is None and cost_limits.get("min") is None
            else filter_min_notional_raw
            if filter_min_notional_raw is not None
            else _raw_decimal_bound(
                cost_limits["min"], field="limits.cost.min", round_up=True
            )
        )
        max_notional_raw = (
            None
            if filter_max_notional_raw is None and cost_limits.get("max") is None
            else filter_max_notional_raw
            if filter_max_notional_raw is not None
            else _raw_decimal_bound(
                cost_limits["max"], field="limits.cost.max", round_up=False
            )
        )
        max_qty_raw = (
            None
            if amount_limits.get("max") is None
            else _raw_decimal_bound(
                amount_limits["max"], field="limits.amount.max", round_up=False
            )
        )
        leverage_limits = limits.get("leverage")
        leverage_limits = leverage_limits if isinstance(leverage_limits, Mapping) else {}
        max_leverage_value = leverage_limits.get("max", market.get("maxLeverage"))
        max_leverage = None
        if max_leverage_value is not None:
            try:
                parsed_leverage = _decimal_to_integer(max_leverage_value, 1, field="maxLeverage")
                if parsed_leverage >= 1:
                    max_leverage = parsed_leverage
            except CcxtConnectorError:
                max_leverage = None
        maintenance_margin_bps = self._maintenance_margin_bps(market)
        precision_mode = getattr(self.exchange, "precisionMode", None)
        tick_size_mode = getattr(self.exchange, "TICK_SIZE", 4)
        decimal_places_mode = getattr(self.exchange, "DECIMAL_PLACES", 2)
        exact_contract_size = _exact_decimal_text(contract_size, field="contractSize")
        exact_price_tick = _exact_precision_step(
            precision.get("price"),
            mode=precision_mode,
            tick_size_mode=tick_size_mode,
            decimal_places_mode=decimal_places_mode,
        )
        exact_quantity_step = _exact_precision_step(
            precision.get("amount"),
            mode=precision_mode,
            tick_size_mode=tick_size_mode,
            decimal_places_mode=decimal_places_mode,
        )
        exact_minimum_quantity = None
        if exact_quantity_step is not None:
            exact_minimum_quantity = _exact_decimal_text(
                amount_limits.get("min")
                if amount_limits.get("min") is not None
                else exact_quantity_step,
                field="limits.amount.min",
            )
            if exact_minimum_quantity is None or Fraction(exact_minimum_quantity) < Fraction(exact_quantity_step):
                exact_minimum_quantity = exact_quantity_step
        exact_notional_candidates = []
        info = market.get("info")
        filters = info.get("filters") if isinstance(info, Mapping) else None
        if isinstance(filters, Sequence) and not isinstance(filters, (str, bytes)):
            for item in filters:
                if not isinstance(item, Mapping):
                    continue
                if item.get("filterType") == "MIN_NOTIONAL":
                    candidate = _exact_decimal_text(
                        item.get("minNotional"), field="MIN_NOTIONAL.minNotional"
                    )
                    if candidate is not None and Fraction(candidate) > 0:
                        exact_notional_candidates.append(candidate)
                elif item.get("filterType") == "NOTIONAL":
                    candidate = _exact_decimal_text(
                        item.get("minNotional"), field="NOTIONAL.minNotional"
                    )
                    if candidate is not None and Fraction(candidate) > 0:
                        exact_notional_candidates.append(candidate)
        if cost_limits.get("min") is not None:
            candidate = _exact_decimal_text(cost_limits["min"], field="limits.cost.min")
            if candidate is not None and Fraction(candidate) > 0:
                exact_notional_candidates.append(candidate)
        exact_minimum_notional = "0"
        if exact_notional_candidates:
            exact_minimum_notional = max(exact_notional_candidates, key=Fraction)
        exact_rules = None
        exact_rule_values = (
            exact_contract_size,
            exact_price_tick,
            exact_quantity_step,
            exact_minimum_quantity,
            exact_minimum_notional,
        )
        if all(value is not None for value in exact_rule_values):
            exact_rules = {
                "contract_size": exact_contract_size,
                "price_tick": exact_price_tick,
                "quantity_step": exact_quantity_step,
                "minimum_quantity": exact_minimum_quantity,
                "minimum_notional": exact_minimum_notional,
            }
        return CcxtMarket(
            exchange_id=self.config.exchange_id,
            symbol=symbol,
            market_id=market_id,
            base=base,
            quote=quote,
            market_type=market_type,
            active=market.get("active"),
            contract=contract,
            linear=market.get("linear"),
            inverse=market.get("inverse"),
            contract_size_raw=contract_size_raw,
            settle=None if market.get("settle") is None else str(market["settle"]),
            expiry_ms=None if market.get("expiry") is None else int(market["expiry"]),
            price_tick_raw=price_tick_raw,
            min_price_raw=min_price_raw,
            max_price_raw=max_price_raw,
            qty_step_raw=qty_step_raw,
            market_order_qty_step_raw=market_qty_step_raw,
            min_qty_raw=min_qty_raw,
            market_order_min_qty_raw=market_min_qty_raw,
            min_notional_raw=min_notional_raw,
            market_order_min_notional_raw=market_min_notional_raw,
            max_notional_raw=max_notional_raw,
            market_order_max_notional_raw=market_max_notional_raw,
            market_order_notional_filter_present=market_notional_filter_present,
            max_qty_raw=max_qty_raw,
            market_order_max_qty_raw=market_max_qty_raw,
            max_leverage=max_leverage,
            maintenance_margin_bps=maintenance_margin_bps,
            exact_rules=exact_rules,
        )

    def validate_live_market_spec(
        self, instrument: str, expected_spec: Mapping[str, Any]
    ) -> None:
        """Fail closed if live CCXT metadata drifted from the frozen Rust risk spec."""
        if not isinstance(expected_spec, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "expected_spec 必须是 object")
        metadata_age = (
            None
            if self._markets_loaded_at is None
            else time.monotonic() - self._markets_loaded_at
        )
        if metadata_age is None or metadata_age > LIVE_MARKET_METADATA_MAX_AGE_SECONDS:
            # Reload all products at most once per short cache window per worker. Reusing an
            # old CCXT market cache indefinitely would make the frozen-spec comparison vacuous.
            self.load_markets(reload=True)
        market = self.resolve_market(instrument)
        mismatches: list[str] = []

        expected_product = str(expected_spec.get("product", "")).strip().lower()
        product_types = {
            "spot": {"spot"},
            "margin": {"margin"},
            "perpetual": {"swap", "perpetual"},
            "future": {"future", "futures"},
            "option": {"option", "options"},
        }
        if market.market_type.lower() not in product_types.get(expected_product, set()):
            mismatches.append(
                f"product 期望 {expected_product!r}，交易所返回 {market.market_type!r}"
            )
        if market.active is False:
            mismatches.append(f"市场 {market.symbol} 已停用")

        for field, actual in (
            ("base_currency", market.base),
            ("quote_currency", market.quote),
            ("settlement_currency", market.settle or market.quote),
        ):
            expected = str(expected_spec.get(field, "")).strip()
            if not expected or expected.upper() != actual.upper():
                mismatches.append(f"{field} 冻结值 {expected!r} 与实时值 {actual!r} 不一致")

        def expected_raw(field: str) -> int:
            value = expected_spec.get(field)
            if isinstance(value, bool) or not isinstance(value, (int, str)):
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID, f"expected_spec.{field} 必须是整数或十进制整数串"
                )
            try:
                return int(value)
            except (TypeError, ValueError) as error:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID, f"expected_spec.{field} 不是有效整数"
                ) from error

        for field, actual in (
            ("contract_size_raw", market.contract_size_raw),
            ("price_tick_raw", market.price_tick_raw),
            ("qty_step_raw", market.qty_step_raw),
        ):
            expected = expected_raw(field)
            if actual is None or expected != actual:
                mismatches.append(
                    f"{field} 冻结值 {expected} 与实时值 {actual!r} 不一致"
                )

        for field, actual in (
            ("min_qty_raw", market.min_qty_raw),
            ("min_notional_raw", market.min_notional_raw),
        ):
            if actual is not None:
                expected = expected_raw(field)
                if expected != actual:
                    mismatches.append(
                        f"{field} 冻结值 {expected} 与实时值 {actual} 不一致"
                    )

        expected_exact_rules = expected_spec.get("exact_rules")
        if expected_exact_rules is not None:
            rule_fields = (
                "contract_size",
                "price_tick",
                "quantity_step",
                "minimum_quantity",
                "minimum_notional",
            )
            if not isinstance(expected_exact_rules, Mapping):
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    "expected_spec.exact_rules 必须是 object",
                )
            actual_exact_rules = market.exact_rules
            for field in rule_fields:
                expected_rule = expected_exact_rules.get(field)
                if isinstance(expected_rule, Mapping):
                    try:
                        coefficient = int(expected_rule["coefficient"])
                        scale = int(expected_rule["scale"])
                        if scale < 0 or scale > 18:
                            raise ValueError("scale out of range")
                        expected_rule = _exact_decimal_text(
                            Fraction(coefficient, 10**scale), field=f"exact_rules.{field}"
                        )
                    except (KeyError, TypeError, ValueError, OverflowError):
                        expected_rule = None
                else:
                    expected_rule = _exact_decimal_text(
                        expected_rule, field=f"exact_rules.{field}"
                    )
                if expected_rule is None:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"expected_spec.exact_rules.{field} 不是规范精确十进制",
                    )
                actual_rule = (
                    None if actual_exact_rules is None else actual_exact_rules.get(field)
                )
                if actual_rule != expected_rule:
                    mismatches.append(
                        f"exact_rules.{field} 冻结值 {expected_rule} 与实时值 {actual_rule!r} 不一致"
                    )

        if expected_product in {"perpetual", "future"}:
            for field, actual in (("linear", market.linear), ("inverse", market.inverse)):
                expected = expected_spec.get(field)
                if not isinstance(expected, bool) or actual is None or expected != actual:
                    mismatches.append(
                        f"{field} 冻结值 {expected!r} 与实时值 {actual!r} 不一致"
                    )
            if not market.contract:
                mismatches.append("冻结规格声明为合约，但实时市场不是合约")
            for field, actual in (
                ("max_leverage", market.max_leverage),
                ("maintenance_margin_bps", market.maintenance_margin_bps),
            ):
                if actual is not None:
                    expected = expected_spec.get(field)
                    if isinstance(expected, bool) or not isinstance(expected, int) or expected != actual:
                        mismatches.append(
                            f"{field} 冻结值 {expected!r} 与实时值 {actual!r} 不一致"
                        )
        elif market.contract:
            mismatches.append("冻结规格声明为非合约，但实时市场是合约")

        if mismatches:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                "冻结风控规格与交易所当前市场元数据不一致，拒绝提交: " + "；".join(mismatches),
            )

    @staticmethod
    def _market_order_constraints(
        market: Mapping[str, Any],
    ) -> tuple[
        int | None,
        int | None,
        int | None,
        int | None,
        int | None,
        int | None,
        int | None,
        bool,
    ]:
        """Read Binance's market-only filters when CCXT exposes them in raw market info.

        Other venues use the unified amount/cost limits below. A zero filter value means that
        the exchange does not apply that particular bound.
        """
        info = market.get("info")
        filters = info.get("filters") if isinstance(info, Mapping) else None
        if not isinstance(filters, Sequence) or isinstance(filters, (str, bytes)):
            return None, None, None, None, None, None, None, False

        market_lot = next(
            (
                item
                for item in filters
                if isinstance(item, Mapping) and item.get("filterType") == "MARKET_LOT_SIZE"
            ),
            None,
        )

        def positive_filter_raw(value: Any, *, field: str) -> int | None:
            if value is None:
                return None
            return _raw_decimal_bound(value, field=field, round_up=True)

        def maximum_filter_raw(value: Any, *, field: str) -> int | None:
            if value is None:
                return None
            return _raw_decimal_bound(value, field=field, round_up=False)

        qty_step = (
            None
            if market_lot is None
            else _raw_lattice_step(
                market_lot.get("stepSize"), field="MARKET_LOT_SIZE.stepSize"
            )
        )
        min_qty = (
            None
            if market_lot is None
            else positive_filter_raw(market_lot.get("minQty"), field="MARKET_LOT_SIZE.minQty")
        )
        max_qty = (
            None
            if market_lot is None
            else maximum_filter_raw(market_lot.get("maxQty"), field="MARKET_LOT_SIZE.maxQty")
        )

        filter_min_notional = None
        filter_max_notional = None
        market_min_notional = None
        market_max_notional = None
        notional_filter_present = False
        for item in filters:
            if not isinstance(item, Mapping):
                continue
            filter_type = item.get("filterType")
            if filter_type == "MIN_NOTIONAL":
                notional_filter_present = True
                filter_min_notional = positive_filter_raw(
                    item.get("minNotional"), field="MIN_NOTIONAL.minNotional"
                )
                if str(item.get("applyToMarket", "true")).lower() == "true":
                    market_min_notional = filter_min_notional
                break
            if filter_type == "NOTIONAL":
                notional_filter_present = True
                filter_min_notional = positive_filter_raw(
                    item.get("minNotional"), field="NOTIONAL.minNotional"
                )
                filter_max_notional = maximum_filter_raw(
                    item.get("maxNotional"), field="NOTIONAL.maxNotional"
                )
                if str(item.get("applyMinToMarket", "true")).lower() == "true":
                    market_min_notional = filter_min_notional
                if str(item.get("applyMaxToMarket", "true")).lower() == "true":
                    market_max_notional = filter_max_notional
                break
        return (
            qty_step,
            min_qty,
            max_qty,
            filter_min_notional,
            filter_max_notional,
            market_min_notional,
            market_max_notional,
            notional_filter_present,
        )

    @staticmethod
    def _market_price_constraints(
        market: Mapping[str, Any],
    ) -> tuple[int | None, int | None]:
        """Read Binance PRICE_FILTER bounds when the unified limits omit them."""
        info = market.get("info")
        filters = info.get("filters") if isinstance(info, Mapping) else None
        if not isinstance(filters, Sequence) or isinstance(filters, (str, bytes)):
            return None, None
        price_filter = next(
            (
                item
                for item in filters
                if isinstance(item, Mapping) and item.get("filterType") == "PRICE_FILTER"
            ),
            None,
        )
        if price_filter is None:
            return None, None

        def positive_filter_raw(value: Any, *, field: str) -> int | None:
            if value is None:
                return None
            return _raw_decimal_bound(value, field=field, round_up=True)

        def maximum_filter_raw(value: Any, *, field: str) -> int | None:
            if value is None:
                return None
            return _raw_decimal_bound(value, field=field, round_up=False)

        return (
            positive_filter_raw(price_filter.get("minPrice"), field="PRICE_FILTER.minPrice"),
            maximum_filter_raw(price_filter.get("maxPrice"), field="PRICE_FILTER.maxPrice"),
        )

    def _precision_step_raw(self, value: Any) -> int | None:
        if value is None:
            return None
        mode = getattr(self.exchange, "precisionMode", None)
        tick_size_mode = getattr(self.exchange, "TICK_SIZE", 4)
        decimal_places_mode = getattr(self.exchange, "DECIMAL_PLACES", 2)
        if mode == tick_size_mode:
            return _raw_lattice_step(value, field="precision")
        if mode == decimal_places_mode:
            try:
                places = int(value)
            except (TypeError, ValueError):
                return None
            if places < 0:
                return None
            return 10 ** max(0, 9 - places)
        # SIGNIFICANT_DIGITS 无法脱离具体价格无损转换成固定 tick，交给部署侧覆盖。
        return None

    @staticmethod
    def _maintenance_margin_bps(market: Mapping[str, Any]) -> int | None:
        rate = market.get("maintenanceMarginRate")
        if rate is not None:
            try:
                parsed = _decimal_to_integer(rate, 10_000, field="maintenanceMarginRate")
                if parsed >= 0:
                    return parsed
            except CcxtConnectorError:
                return None
        percent = market.get("maintenanceMarginPercent")
        if percent is not None:
            try:
                parsed = _decimal_to_integer(percent, 100, field="maintenanceMarginPercent")
                if parsed >= 0:
                    return parsed
            except CcxtConnectorError:
                return None
        return None

    def fetch_ohlcv(
        self,
        instrument: str,
        *,
        timeframe: str = "1m",
        start_ms: int,
        end_ms: int,
        limit: int = 1_000,
        params: Mapping[str, Any] | None = None,
        max_pages: int = 1_000,
    ) -> BarFrame:
        if start_ms < 0 or end_ms < start_ms or limit <= 0 or max_pages <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "OHLCV 查询边界非法")
        market = self.resolve_market(instrument)
        rows: dict[int, Sequence[Any]] = {}
        since = start_ms
        for _ in range(max_pages):
            batch = self._call(
                "fetch_ohlcv",
                market.symbol,
                timeframe,
                since,
                limit,
                dict(params or {}),
            )
            if not batch:
                break
            if len(batch) > limit:
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    "CCXT OHLCV exchange returned more rows than the requested page limit",
                )
            previous = since
            reached_end = False
            for row in batch:
                if not isinstance(row, Sequence) or len(row) < 6:
                    raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT OHLCV 行格式非法")
                timestamp = int(row[0])
                if timestamp > end_ms:
                    reached_end = True
                    continue
                if timestamp < start_ms:
                    continue
                if timestamp not in rows and len(rows) >= MAX_OHLCV_ROWS:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"OHLCV query exceeds the {MAX_OHLCV_ROWS}-row safety limit",
                    )
                rows[timestamp] = row
                previous = max(previous, timestamp)
            if len(batch) < limit or reached_end:
                break
            if previous <= since:
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    "CCXT OHLCV pagination made no progress; refusing partial history",
                )
            since = previous + 1
            if since > end_ms:
                break
        else:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                f"CCXT OHLCV pagination reached max_pages={max_pages}; refusing partial history",
            )
        ordered = [rows[timestamp] for timestamp in sorted(rows)]
        if not ordered:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT OHLCV 查询为空")
        return BarFrame(
            instrument=instrument,
            source=f"ccxt:{self.config.exchange_id}:{market.symbol}:{timeframe}",
            ts=tuple(int(row[0]) for row in ordered),
            open_raw=tuple(_raw_decimal_projection(row[1], field="open") for row in ordered),
            high_raw=tuple(_raw_decimal_projection(row[2], field="high") for row in ordered),
            low_raw=tuple(_raw_decimal_projection(row[3], field="low") for row in ordered),
            close_raw=tuple(_raw_decimal_projection(row[4], field="close") for row in ordered),
            volume_raw=tuple(_raw_decimal_projection(row[5], field="volume") for row in ordered),
            exact_ohlcv=tuple(
                {
                    name: _exact_decimal_wire(row[index], field=name)
                    for name, index in (
                        ("open", 1),
                        ("high", 2),
                        ("low", 3),
                        ("close", 4),
                        ("volume", 5),
                    )
                }
                for row in ordered
            ),
        )

    def fetch_ticker(self, instrument: str, *, params: Mapping[str, Any] | None = None) -> CcxtTicker:
        market = self.resolve_market(instrument)
        ticker = self._call("fetch_ticker", market.symbol, dict(params or {}))
        if not isinstance(ticker, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT ticker 返回类型非法")
        timestamp = ticker.get("timestamp")
        bid = ticker.get("bid")
        ask = ticker.get("ask")
        bid_qty = ticker.get("bidVolume")
        ask_qty = ticker.get("askVolume")

        # CCXT 的部分合约市场（例如 Binance USDⓈ-M）会返回 last，
        # 但 ticker 的 bid/ask 为 None。统一层不能用 last 冒充盘口，
        # 因此仅在报价缺失或时间戳缺失时回退到订单簿首档。
        if bid is None or ask is None or timestamp is None:
            try:
                order_book = self._call(
                    "fetch_order_book", market.symbol, 5, dict(params or {})
                )
                if not isinstance(order_book, Mapping):
                    raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order book 返回类型非法")
                if timestamp is None:
                    timestamp = order_book.get("timestamp")
                if bid is None:
                    bid, bid_qty = self._order_book_top(order_book, "bids", "bid")
                if ask is None:
                    ask, ask_qty = self._order_book_top(order_book, "asks", "ask")
            except CcxtConnectorError as error:
                raise CcxtConnectorError(
                    error.error_class,
                    f"CCXT ticker 缺少 bid/ask 或 timestamp，订单簿回退失败: {error}",
                    cause=error,
                ) from error
        if timestamp is None:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT ticker 缺少 timestamp")
        if bid is None or ask is None:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT ticker 缺少有效 bid/ask")
        return CcxtTicker(
            exchange_id=self.config.exchange_id,
            symbol=market.symbol,
            timestamp_ms=int(timestamp),
            bid_raw=_raw_decimal_projection(bid, field="bid"),
            ask_raw=_raw_decimal_projection(ask, field="ask"),
            bid_qty_raw=_raw_decimal_projection(0 if bid_qty is None else bid_qty, field="bidVolume"),
            ask_qty_raw=_raw_decimal_projection(0 if ask_qty is None else ask_qty, field="askVolume"),
            last_raw=_raw_decimal_projection(0 if ticker.get("last") is None else ticker.get("last"), field="last"),
            bid_exact=_exact_decimal_text(bid, field="bid"),
            ask_exact=_exact_decimal_text(ask, field="ask"),
            bid_qty_exact=None
            if bid_qty is None
            else _exact_decimal_text(bid_qty, field="bidVolume"),
            ask_qty_exact=None
            if ask_qty is None
            else _exact_decimal_text(ask_qty, field="askVolume"),
        )

    def fetch_order_book(
        self,
        instrument: str,
        *,
        limit: int = 20,
        params: Mapping[str, Any] | None = None,
    ) -> CcxtOrderBook:
        if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= 100:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "order book limit 必须在 1..=100")
        market = self.resolve_market(instrument)
        book = self._call("fetch_order_book", market.symbol, limit, dict(params or {}))
        if not isinstance(book, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order book 返回类型非法")

        def levels(side: str) -> tuple[CcxtOrderBookLevel, ...]:
            values = book.get(side)
            if not isinstance(values, Sequence) or isinstance(values, (str, bytes)) or not values:
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"CCXT order book 缺少非空 {side}")
            result: list[CcxtOrderBookLevel] = []
            for index, row in enumerate(values[:limit]):
                if not isinstance(row, Sequence) or isinstance(row, (str, bytes)) or len(row) < 2:
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE, f"CCXT order book {side}[{index}] 格式非法"
                    )
                price_exact = _exact_decimal_text(row[0], field=f"{side}[{index}].price")
                qty_exact = _exact_decimal_text(row[1], field=f"{side}[{index}].quantity")
                if price_exact is None or qty_exact is None:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"CCXT order book {side}[{index}] 超出 18 位精确小数范围",
                    )
                if Fraction(price_exact) <= 0 or Fraction(qty_exact) <= 0:
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        f"CCXT order book {side}[{index}] 价格和数量必须为正",
                    )
                result.append(
                    CcxtOrderBookLevel(
                        price_raw=_raw_decimal_projection(row[0], field=f"{side}[{index}].price"),
                        qty_raw=_raw_decimal_projection(row[1], field=f"{side}[{index}].quantity"),
                        price_exact=price_exact,
                        qty_exact=qty_exact,
                    )
                )
            return tuple(result)

        bids = levels("bids")
        asks = levels("asks")
        if any(Fraction(left.price_exact) <= Fraction(right.price_exact) for left, right in zip(bids, bids[1:])):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order book bids 未按价格降序排列")
        if any(Fraction(left.price_exact) >= Fraction(right.price_exact) for left, right in zip(asks, asks[1:])):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order book asks 未按价格升序排列")
        if Fraction(bids[0].price_exact) > Fraction(asks[0].price_exact):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order book 买卖盘交叉")
        timestamp = book.get("timestamp")
        timestamp_ms = (
            int(timestamp)
            if isinstance(timestamp, (int, float)) and not isinstance(timestamp, bool) and timestamp > 0
            else None
        )
        sequence = book.get("nonce")
        if isinstance(sequence, bool) or not isinstance(sequence, int) or sequence <= 0:
            sequence = None
        return CcxtOrderBook(
            exchange_id=self.config.exchange_id,
            symbol=market.symbol,
            timestamp_ms=timestamp_ms,
            sequence=sequence,
            bids=bids,
            asks=asks,
        )

    @staticmethod
    def _order_book_top(
        order_book: Mapping[str, Any], side: str, field: str
    ) -> tuple[Any, Any]:
        levels = order_book.get(side)
        if not isinstance(levels, Sequence) or isinstance(levels, (str, bytes)) or not levels:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE, f"CCXT order book 缺少非空 {field} 档位"
            )
        level = levels[0]
        if not isinstance(level, Sequence) or isinstance(level, (str, bytes)) or len(level) < 2:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"CCXT order book {field} 档位格式非法")
        if level[0] is None:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"CCXT order book {field} 价格为空")
        return level[0], level[1]

    @staticmethod
    def _order_is_reduce_only(order: CcxtOrder) -> bool:
        """Return true only when the exchange response explicitly marks a close order.

        Unknown order shapes are counted as exposure by the leverage preflight. That can
        conservatively reject an opening order, but it must not silently undercount risk.
        """
        raw = order.raw
        info = raw.get("info") if isinstance(raw, Mapping) else None
        candidates = [raw]
        if isinstance(info, Mapping):
            candidates.append(info)
        for candidate in candidates:
            for key in ("reduceOnly", "reduce_only", "closeOrder"):
                value = candidate.get(key)
                if value is True or str(value).strip().lower() in {"true", "1"}:
                    return True
            trade_side = str(candidate.get("tradeSide", "")).strip().lower()
            if trade_side == "close":
                return True
        return False

    def _validate_contract_leverage_tier(
        self,
        instrument: str,
        market: CcxtMarket,
        *,
        side: str,
        hedged: bool,
        amount_raw: int | None,
        amount_exact: str | None,
        price_raw: int | None,
        price_exact: str | None,
        leverage: int,
        reduce_only: bool,
        margin_mode: str | None,
    ) -> None:
        """Fail closed when known same-market exposure exceeds the requested tier.

        This is a pre-submit guard, not an exchange reservation: the exchange remains the
        final authority because another client can change positions between these reads and
        the order request.
        """
        if not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "杠杆档位预检只适用于合约市场")
        if isinstance(leverage, bool) or not isinstance(leverage, int) or leverage < 1:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "合约订单必须提供正整数 risk_leverage")
        if side not in {"buy", "sell"}:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "合约订单档位预检缺少有效 side")
        if not isinstance(hedged, bool):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "合约订单 hedged 必须为布尔值")
        if reduce_only:
            return
        if (amount_raw is None) == (amount_exact is None):
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                "档位预检必须且只能提供 amount_raw 或 amount_exact",
            )
        amount = (
            Fraction(amount_exact)
            if amount_exact is not None
            else Fraction(amount_raw, native.raw_scale())
        )
        if amount <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量必须大于 0")
        limit_price = (
            Fraction(price_exact)
            if price_exact is not None
            else None
            if price_raw is None
            else Fraction(price_raw, native.raw_scale())
        )

        tiers = self.fetch_leverage_tiers(instrument, margin_mode=margin_mode)
        if not tiers:
            raise CcxtConnectorError(CcxtErrorClass.UNSUPPORTED, "交易所未返回合约杠杆档位，拒绝开仓")
        exposure_unit = tiers[0].exposure_unit
        if any(tier.exposure_unit != exposure_unit for tier in tiers):
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                "交易所杠杆档位混用了不同敞口单位，拒绝开仓",
            )
        if exposure_unit == "quote_notional" and not market.inverse:
            ticker = self.fetch_ticker(instrument)
            if ticker.bid_exact is None or ticker.ask_exact is None:
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "杠杆档位预检缺少有效合约参考价")
            reference_price = max(Fraction(ticker.bid_exact), Fraction(ticker.ask_exact))
            if reference_price <= 0:
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "杠杆档位预检缺少有效合约参考价")
        elif exposure_unit == "contracts" or (exposure_unit == "quote_notional" and market.inverse):
            reference_price_raw = 0
            reference_price = Fraction(0)
        else:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"不支持杠杆档位敞口单位: {exposure_unit}",
            )

        positions = self.fetch_positions([instrument])
        open_orders = self.fetch_open_orders(instrument)

        exact_rules = market.exact_rules or {}
        contract_size_text = exact_rules.get("contract_size")
        contract_size = (
            Fraction(contract_size_text)
            if contract_size_text is not None
            else Fraction(market.contract_size_raw, native.raw_scale())
        )

        def exposure(amount_value: Fraction, price_value: Fraction) -> Fraction:
            if exposure_unit == "contracts":
                return amount_value
            if exposure_unit == "quote_notional":
                notional = amount_value * contract_size
                return notional if market.inverse else notional * price_value
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"不支持杠杆档位敞口单位: {exposure_unit}",
            )

        candidate_price = max(reference_price, limit_price or Fraction(0))
        candidate_exposure = exposure(amount, candidate_price)
        bitget_hedge_tiers = self.config.exchange_id.lower() == "bitget" and hedged
        if bitget_hedge_tiers:
            # Bitget tiers use the larger directional exposure in hedge mode, including
            # each side's ordinary open orders. Summing both legs incorrectly promotes a
            # balanced hedge into a higher tier and can reject a valid opening order.
            directional_exposure = {"long": Fraction(0), "short": Fraction(0)}
            for position in positions:
                position_amount = (
                    Fraction(position.contracts_exact)
                    if position.contracts_exact is not None
                    else Fraction(position.contracts_raw, native.raw_scale())
                )
                if position_amount == 0:
                    continue
                position_side = position.side.strip().lower()
                if position_side not in directional_exposure:
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        f"持仓 {position.symbol} 缺少 Bitget Hedge 方向，拒绝估算杠杆档位",
                    )
                position_mark = (
                    Fraction(position.mark_price_exact)
                    if position.mark_price_exact is not None
                    else None
                    if position.mark_price_raw is None
                    else Fraction(position.mark_price_raw, native.raw_scale())
                )
                if exposure_unit == "quote_notional" and not market.inverse and (
                    position_mark is None or position_mark <= 0
                ):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        f"持仓 {position.symbol} 缺少有效 mark price，拒绝估算杠杆档位",
                    )
                # Inverse contracts are face-value contracts: contracts × contractSize
                # already yields quote-currency notional and does not require a mark price.
                position_price = Fraction(0) if market.inverse else position_mark
                assert position_price is not None
                directional_exposure[position_side] += exposure(
                    abs(position_amount), position_price
                )
            for order in open_orders:
                remaining = Fraction(order.remaining_exact)
                if remaining <= 0 or self._order_is_reduce_only(order):
                    continue
                order_side = order.side.strip().lower()
                if order_side not in {"buy", "sell"}:
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        f"挂单 {order.order_id} 缺少 Bitget Hedge 方向，拒绝估算杠杆档位",
                    )
                position_side = "long" if order_side == "buy" else "short"
                order_price = max(
                    reference_price,
                    Fraction(order.price_exact)
                    if order.price_exact is not None
                    else Fraction(0)
                    if order.price_raw is None
                    else Fraction(order.price_raw, native.raw_scale()),
                )
                directional_exposure[position_side] += exposure(
                    remaining, order_price
                )
            candidate_side = "long" if side == "buy" else "short"
            directional_exposure[candidate_side] += candidate_exposure
            projected_notional = max(directional_exposure.values())
        else:
            projected_notional = candidate_exposure
            for position in positions:
                position_amount = (
                    Fraction(position.contracts_exact)
                    if position.contracts_exact is not None
                    else Fraction(position.contracts_raw, native.raw_scale())
                )
                if position_amount == 0:
                    continue
                position_mark = (
                    Fraction(position.mark_price_exact)
                    if position.mark_price_exact is not None
                    else None
                    if position.mark_price_raw is None
                    else Fraction(position.mark_price_raw, native.raw_scale())
                )
                if exposure_unit == "quote_notional" and not market.inverse and (
                    position_mark is None or position_mark <= 0
                ):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        f"持仓 {position.symbol} 缺少有效 mark price，拒绝估算杠杆档位",
                    )
                position_price = Fraction(0) if market.inverse else position_mark
                assert position_price is not None
                projected_notional += exposure(abs(position_amount), position_price)
            for order in open_orders:
                remaining = Fraction(order.remaining_exact)
                if remaining <= 0 or self._order_is_reduce_only(order):
                    continue
                order_price = max(
                    reference_price,
                    Fraction(order.price_exact)
                    if order.price_exact is not None
                    else Fraction(0)
                    if order.price_raw is None
                    else Fraction(order.price_raw, native.raw_scale()),
                )
                projected_notional += exposure(remaining, order_price)

        matching = [
            tier
            for tier in tiers
            if projected_notional >= Fraction(tier.min_notional_exact)
            and (
                tier.max_notional_exact is None
                or projected_notional < Fraction(tier.max_notional_exact)
            )
        ]
        if len(matching) != 1:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                "当前持仓、未结订单和本次订单的合计名义额无法唯一匹配交易所杠杆档位",
            )
        max_leverage = matching[0].max_leverage
        if max_leverage is None:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "交易所适用杠杆档位缺少 maxLeverage，拒绝开仓",
            )
        if leverage > max_leverage:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"合计名义额对应交易所第 {matching[0].tier} 档，最高杠杆 {max_leverage}x，订单要求 {leverage}x",
            )

    def validate_contract_order_leverage_tier(
        self,
        instrument: str,
        *,
        side: str,
        hedged: bool,
        amount_raw: int | None = None,
        amount_exact: str | None = None,
        price_raw: int | None,
        price_exact: str | None = None,
        leverage: int,
        reduce_only: bool,
        margin_mode: str,
    ) -> None:
        """Run the read-only order leverage check separately from the exchange write."""
        amount = (
            Fraction(amount_exact)
            if amount_exact is not None
            else Fraction(amount_raw or 0, native.raw_scale())
        )
        if amount <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量必须大于 0")
        if margin_mode not in {"cross", "isolated"}:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "risk_margin_mode 必须为 cross 或 isolated")
        market = self.resolve_market(instrument)
        self._validate_contract_leverage_tier(
            instrument,
            market,
            side=side,
            hedged=hedged,
            amount_raw=amount_raw,
            amount_exact=amount_exact,
            price_raw=price_raw,
            price_exact=price_exact,
            leverage=leverage,
            reduce_only=reduce_only,
            margin_mode=margin_mode,
        )

    def validate_binance_spot_max_position(
        self,
        instrument: str,
        *,
        side: str,
        amount_unit: str,
        amount_raw: int | None = None,
        amount_exact: str | None = None,
    ) -> None:
        """Check Binance Spot MAX_POSITION with exact account and open-order values.

        The CCXT order path must preserve Binance's account-wide base-asset rule,
        including pending BUYs on other spot pairs. Raw Binance ``info`` fields are
        used for quantities so this check never depends on binary floats.
        """
        if self.config.exchange_id.lower() != "binance":
            return
        market = self.resolve_market(instrument)
        if market.contract or market.market_type.lower() != "spot":
            return
        if side.lower() != "buy":
            return

        raw_market = self.load_markets().get(market.symbol)
        if not isinstance(raw_market, Mapping):
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"Binance market metadata 缺失，无法校验 MAX_POSITION: {market.symbol}",
            )
        market_info = raw_market.get("info")
        filters = market_info.get("filters") if isinstance(market_info, Mapping) else None
        if filters is None:
            # Older CCXT market fixtures may omit raw info. For Binance live markets,
            # missing filter metadata cannot prove whether this rule applies.
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"Binance market 缺少原始 filters，无法校验 MAX_POSITION: {market.symbol}",
            )
        if not isinstance(filters, Sequence) or isinstance(filters, (str, bytes)):
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE, "Binance market filters 返回类型非法"
            )
        matching_filters = [
            row
            for row in filters
            if isinstance(row, Mapping) and row.get("filterType") == "MAX_POSITION"
        ]
        if len(matching_filters) > 1:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                "Binance market 重复包含 MAX_POSITION filter",
            )
        if not matching_filters:
            return
        maximum_text = _exact_decimal_text(
            matching_filters[0].get("maxPosition"), field="MAX_POSITION.maxPosition"
        )
        if maximum_text is None or Fraction(maximum_text) <= 0:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                "Binance MAX_POSITION.maxPosition 缺失、非精确或非正数",
            )
        if amount_unit == "quote":
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "Binance MAX_POSITION 生效时不能从 quote budget 证明基础币买入量安全",
            )
        if amount_unit != "base":
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                "Binance Spot MAX_POSITION 预检要求 base 数量单位",
            )
        if (amount_raw is None) == (amount_exact is None):
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                "MAX_POSITION 预检必须且只能提供一种精确订单数量",
            )
        current_amount = Fraction(
            _exact_decimal_wire_text(amount_exact, field="amount_exact")
            if amount_exact is not None
            else _decimal_from_raw(amount_raw, field="amount")
        )
        if current_amount <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "买入数量必须大于 0")

        # Fetch open orders before the balance snapshot. If an order fills between
        # these reads, it can be counted twice and conservatively rejected, never
        # silently omitted from the projected position.
        open_orders = self._call("fetch_open_orders", None, None, None, {})
        if not isinstance(open_orders, Sequence) or isinstance(open_orders, (str, bytes)):
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE, "Binance open orders 返回类型非法"
            )
        markets = self.load_markets()
        base_by_symbol: dict[str, str] = {}
        for row in markets.values():
            if not isinstance(row, Mapping) or bool(row.get("contract", False)):
                continue
            row_type = str(row.get("type") or ("spot" if row.get("spot") else "")).lower()
            if row_type != "spot":
                continue
            base = str(row.get("base") or "").strip()
            symbol = str(row.get("symbol") or "").strip()
            market_id = str(row.get("id") or "").strip()
            if not base or not symbol or not market_id:
                continue
            base_by_symbol[symbol.upper()] = base.upper()
            base_by_symbol[market_id.upper()] = base.upper()

        position = Fraction(0)
        seen_orders: set[tuple[str, str]] = set()
        for order in open_orders:
            if not isinstance(order, Mapping):
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE, "Binance open orders 包含非法记录"
                )
            info = order.get("info")
            info = info if isinstance(info, Mapping) else {}
            order_side = str(info.get("side") or order.get("side") or "").lower()
            if order_side == "sell":
                continue
            if order_side != "buy":
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    "Binance open order 缺少可识别的 side，无法安全计算 MAX_POSITION",
                )
            symbol = str(info.get("symbol") or order.get("symbol") or "").strip().upper()
            base = base_by_symbol.get(symbol)
            if not symbol or base is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    f"Binance open BUY 缺少可识别的现货 base asset: {symbol!r}",
                )
            if base != market.base.upper():
                continue
            order_id = str(info.get("orderId") or order.get("id") or "").strip()
            if not order_id:
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    "Binance open BUY 缺少 order id，无法安全计算 MAX_POSITION",
                )
            identity = (symbol, order_id)
            if identity in seen_orders:
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    "Binance openOrders 包含重复委托，无法安全计算 MAX_POSITION",
                )
            seen_orders.add(identity)
            original = self._required_raw_exchange_decimal(
                info.get("origQty"), field=f"openOrders[{order_id}].origQty"
            )
            executed = self._required_raw_exchange_decimal(
                info.get("executedQty"), field=f"openOrders[{order_id}].executedQty"
            )
            remaining = original - executed
            if original <= 0 or executed < 0 or remaining < 0:
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    f"Binance open BUY 数量非法: {order_id}",
                )
            position += remaining

        balance = self._call("fetch_balance", {})
        if not isinstance(balance, Mapping):
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE, "Binance balance 返回类型非法"
            )
        balance_info = balance.get("info")
        balances = balance_info.get("balances") if isinstance(balance_info, Mapping) else None
        if not isinstance(balances, Sequence) or isinstance(balances, (str, bytes)):
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                "Binance 原始 balance 缺少 balances，无法安全计算 MAX_POSITION",
            )
        matching_balances = [
            row
            for row in balances
            if isinstance(row, Mapping)
            and str(row.get("asset") or "").upper() == market.base.upper()
        ]
        if len(matching_balances) > 1:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                f"Binance 原始 balance 重复包含资产 {market.base}",
            )
        if matching_balances:
            row = matching_balances[0]
            free = self._required_raw_exchange_decimal(
                row.get("free"), field=f"balance.{market.base}.free"
            )
            locked = self._required_raw_exchange_decimal(
                row.get("locked"), field=f"balance.{market.base}.locked"
            )
            position += free + locked
        if position < 0:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE, "Binance MAX_POSITION 账户持仓为负"
            )
        projected = position + current_amount
        if projected > Fraction(maximum_text):
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"Binance MAX_POSITION 超限: 当前余额、全部交易对未成交买单与本单合计 {projected}，上限为 {maximum_text} {market.base}",
            )

    @staticmethod
    def _required_raw_exchange_decimal(value: Any, *, field: str) -> Fraction:
        """Parse an exchange's raw decimal string without accepting float values."""
        if not isinstance(value, str):
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                f"{field} 缺少原始十进制字符串，无法无损计算",
            )
        text = _exact_decimal_text(value, field=field)
        if text is None:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE, f"{field} 不是有效的精确十进制"
            )
        return Fraction(text)

    def create_order(
        self,
        instrument: str,
        *,
        side: str,
        order_type: str,
        amount_raw: int | None = None,
        amount_exact: str | None = None,
        amount_unit: str | None = None,
        price_raw: int | None = None,
        price_exact: str | None = None,
        risk_leverage: int | None = None,
        risk_reduce_only: bool = False,
        risk_margin_mode: str | None = None,
        risk_preflight_verified: bool = False,
        client_order_id: str | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> CcxtOrder:
        market = self.resolve_market(instrument)
        if amount_exact is not None:
            if amount_raw is not None or price_raw is not None:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    "精确下单必须只携带 amount_exact/price_exact，不能混用 raw 金额",
                )
            return self._create_exact_order(
                market,
                instrument=instrument,
                side=side,
                order_type=order_type,
                amount_exact=amount_exact,
                amount_unit=amount_unit,
                price_exact=price_exact,
                risk_leverage=risk_leverage,
                risk_reduce_only=risk_reduce_only,
                risk_margin_mode=risk_margin_mode,
                risk_preflight_verified=risk_preflight_verified,
                client_order_id=client_order_id,
                params=params,
            )
        if amount_raw is None or price_exact is not None:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                "旧精度下单必须提供 amount_raw，且不能单独携带 price_exact",
            )
        if side not in {"buy", "sell"} or order_type not in {"market", "limit"} or amount_raw <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单 side/type/amount 非法")
        if amount_unit is not None:
            expected_amount_unit = "contracts" if market.contract else "base"
            if amount_unit != expected_amount_unit:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    f"订单数量单位 {amount_unit!r} 与市场 {market.symbol} 的 {expected_amount_unit!r} 单位不一致",
                )
        is_market_order = order_type == "market"
        if (
            self.config.exchange_id.lower() == "bitget"
            and market.market_type == "spot"
            and is_market_order
            and side == "buy"
        ):
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "Bitget 现货市价买单按 quote 币金额下单，与当前 base 数量订单契约不兼容；请使用限价单，或等待 quote-budget 订单意图支持",
            )
        if market.active is False:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"交易所市场 {market.symbol} 当前未开放交易",
            )
        qty_step_raw = market.qty_step_raw
        min_qty_raw = market.min_qty_raw
        max_qty_raw = market.max_qty_raw
        if is_market_order:
            if market.market_order_qty_step_raw is not None:
                qty_step_raw = market.market_order_qty_step_raw
            if market.market_order_min_qty_raw is not None:
                min_qty_raw = market.market_order_min_qty_raw
            # Zero is a meaningful conservative maximum after a positive exchange
            # bound finer than SCALE is floored onto the core grid. Do not use `or`
            # here: it would silently discard that impossible-to-satisfy filter.
            if market.market_order_max_qty_raw is not None:
                max_qty_raw = market.market_order_max_qty_raw
        if qty_step_raw and amount_raw % qty_step_raw != 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量不符合交易所 qty step")
        self._require_exact_exchange_precision(
            "amount_to_precision",
            market.symbol,
            amount_raw,
            field="amount",
            known_step=qty_step_raw,
        )
        if min_qty_raw is not None and amount_raw < min_qty_raw:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量小于交易所最小数量")
        if max_qty_raw is not None and amount_raw > max_qty_raw:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量超过交易所最大数量")
        if order_type == "limit" and (price_raw is None or price_raw <= 0):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "限价单必须提供正 price")
        if price_raw is not None and market.min_price_raw is not None and price_raw < market.min_price_raw:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单价格低于交易所最小价格")
        if price_raw is not None and market.max_price_raw is not None and price_raw > market.max_price_raw:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单价格超过交易所最大价格")
        if price_raw is not None and market.price_tick_raw and price_raw % market.price_tick_raw != 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单价格不符合交易所 price tick")
        if price_raw is not None:
            self._require_exact_exchange_precision(
                "price_to_precision",
                market.symbol,
                price_raw,
                field="price",
                known_step=market.price_tick_raw,
            )
        min_notional_raw = market.min_notional_raw
        max_notional_raw = market.max_notional_raw
        if is_market_order and market.market_order_notional_filter_present:
            min_notional_raw = market.market_order_min_notional_raw
            max_notional_raw = market.market_order_max_notional_raw
        if (min_notional_raw is not None and min_notional_raw > 0) or (
            max_notional_raw is not None
        ):
            reference_price_raw = price_raw
            if reference_price_raw is None:
                ticker = self.fetch_ticker(instrument)
                reference_price_raw = ticker.ask_raw if side == "buy" else ticker.bid_raw
            if reference_price_raw <= 0:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "缺少有效参考价，无法校验交易所名义额限制")
            notional_raw = _market_order_notional_raw(market, amount_raw, reference_price_raw)
            if min_notional_raw is not None and min_notional_raw > 0 and notional_raw < min_notional_raw:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单名义金额低于交易所最小限制")
            if max_notional_raw is not None and notional_raw > max_notional_raw:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单名义金额超过交易所最大限制")
        if risk_leverage is not None and not risk_preflight_verified:
            hedged = (params or {}).get("hedged", False)
            self._validate_contract_leverage_tier(
                instrument,
                market,
                side=side,
                hedged=hedged,
                amount_raw=amount_raw,
                amount_exact=None,
                price_raw=price_raw,
                price_exact=None,
                leverage=risk_leverage,
                reduce_only=risk_reduce_only,
                margin_mode=risk_margin_mode,
            )
        if (
            market.market_type.lower() == "spot"
            and not market.contract
            and self.config.exchange_id.lower() == "binance"
            and not risk_preflight_verified
        ):
            self.validate_binance_spot_max_position(
                instrument,
                side=side,
                amount_unit=amount_unit or "base",
                amount_raw=amount_raw,
            )
        call_params = dict(params or {})
        if market.market_type == "spot" and is_market_order and side == "buy":
            # Spot amount is expressed in base-asset units; derivative amount follows
            # CCXT's contract-count convention. OKX's spot market-buy default is
            # quote-currency sizing, so make the spot unit explicit.
            # Bitget spot/UTA market buys are quote-budget orders; silently forwarding the
            # base quantity would spend that number of quote coins, while multiplying by a
            # ticker price would turn the requested base size into an unbounded approximate
            # fill size. Reject until the order contract supports an explicit quote-budget
            # intent (or a price-capped base-quantity order) for Bitget.
            exchange_id = self.config.exchange_id.lower()
            if exchange_id == "okx":
                call_params.setdefault("tgtCcy", "base_ccy")
        if client_order_id:
            call_params.setdefault("clientOrderId", client_order_id)
        result = self._call(
            "create_order",
            market.symbol,
            order_type,
            side,
            _decimal_from_raw(amount_raw, field="amount"),
            None if price_raw is None else _decimal_from_raw(price_raw, field="price"),
            call_params,
        )
        return self._normalize_order(result, market)

    def _create_exact_order(
        self,
        market: CcxtMarket,
        *,
        instrument: str,
        side: str,
        order_type: str,
        amount_exact: str,
        amount_unit: str | None,
        price_exact: str | None,
        risk_leverage: int | None,
        risk_reduce_only: bool,
        risk_margin_mode: str | None,
        risk_preflight_verified: bool,
        client_order_id: str | None,
        params: Mapping[str, Any] | None,
    ) -> CcxtOrder:
        """Format and validate Spot and derivative requests without binary-float conversion."""
        if side not in {"buy", "sell"} or order_type not in {"market", "limit"}:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单 side/type 非法")
        is_spot = market.market_type == "spot" and not market.contract
        if not is_spot and not market.contract:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "精确保证金订单暂不支持，当前只开放 Spot 与有杠杆档位预检的合约",
            )
        expected_unit = "base" if is_spot else "contracts"
        quote_budget_order = amount_unit == "quote"
        if amount_unit not in {None, expected_unit, "quote"}:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"精确订单金额单位必须为 {expected_unit} 或 quote",
            )
        if is_spot and risk_leverage is not None:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "现货精确订单不能声明杠杆")
        if market.contract:
            if isinstance(risk_leverage, bool) or not isinstance(risk_leverage, int) or risk_leverage < 1:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "精确合约订单必须提供正整数杠杆")
            if risk_margin_mode not in {"cross", "isolated"}:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "精确合约订单必须提供 cross 或 isolated 保证金模式")
        elif risk_margin_mode is not None or risk_reduce_only:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "现货精确订单不能声明合约风控字段")
        amount_text = _exact_decimal_wire_text(amount_exact, field="amount")
        amount_value = Fraction(amount_text)
        if amount_value <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量必须大于 0")
        price_text = (
            None
            if price_exact is None
            else _exact_decimal_wire_text(price_exact, field="price")
        )
        if order_type == "limit" and (price_text is None or Fraction(price_text) <= 0):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "限价单必须提供正 price")
        if market.active is False:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"交易所市场 {market.symbol} 当前未开放交易",
            )
        exchange_id = self.config.exchange_id.lower()
        if is_spot and exchange_id == "bitget" and order_type == "market" and side == "buy" and not quote_budget_order:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "Bitget 现货市价买单需要 quote-budget 意图，不能按 base 数量精确下单",
            )

        exact_rules = market.exact_rules or {}
        if quote_budget_order:
            if not (
                is_spot
                and exchange_id == "bitget"
                and order_type == "market"
                and side == "buy"
                and price_exact is None
                and risk_leverage is None
                and risk_margin_mode is None
                and not risk_reduce_only
            ):
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    "quote 金额单位只支持 Bitget 现货市价买单",
                )
            amount_text = _exact_decimal_wire_text(amount_exact, field="quote_budget")
            amount_value = Fraction(amount_text)
            if amount_value <= 0 or market.active is False:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    "quote budget 必须为正且 Bitget 现货市场必须处于开放状态",
                )
            self._require_exact_exchange_precision_text(
                "cost_to_precision",
                market.symbol,
                amount_text,
                field="quote budget",
                known_step=None,
            )
            market_min = (
                market.market_order_min_notional_raw
                if market.market_order_notional_filter_present
                else market.min_notional_raw
            )
            market_max = (
                market.market_order_max_notional_raw
                if market.market_order_notional_filter_present
                else market.max_notional_raw
            )
            exact_minimum = exact_rules.get("minimum_notional")
            minimums = [
                Fraction(value, native.raw_scale())
                for value in (market_min,)
                if value is not None and value > 0
            ]
            if exact_minimum is not None and Fraction(exact_minimum) > 0:
                minimums.append(Fraction(exact_minimum))
            if minimums and amount_value < max(minimums):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "quote budget 小于交易所最小名义金额")
            if market_max is not None and amount_value > Fraction(market_max, native.raw_scale()):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "quote budget 超过交易所最大名义金额")

            call_params = dict(params or {})
            if client_order_id:
                call_params.setdefault("clientOrderId", client_order_id)
            if bool((self.config.options or {}).get("uta", False)):
                # CCXT's Bitget UTA request builder runs quote-sized market buys through
                # amount_to_precision (the base-quantity precision). Build its documented
                # SPOT wire shape directly so a small quote budget cannot be rounded on the
                # token's base-amount grid.
                request = {
                    "category": "SPOT",
                    "symbol": market.market_id,
                    "qty": amount_text,
                    "side": "buy",
                    "orderType": "market",
                }
                if client_order_id:
                    request["clientOid"] = str(client_order_id)
                response = self._call(
                    "private_uta_post_v3_trade_place_order", request
                )
                data = response.get("data") if isinstance(response, Mapping) else None
                if not isinstance(data, Mapping):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        "Bitget UTA place-order 响应缺少 data 对象",
                    )
                raw_market = self.load_markets().get(market.symbol)
                if not isinstance(raw_market, Mapping):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        "CCXT Bitget UTA 现货市场元数据缺失",
                    )
                parsed = self._call("parse_order", data, raw_market)
                return self._normalize_order(parsed, market)

            call_params["createMarketBuyOrderRequiresPrice"] = False
            result = self._call(
                "create_order", market.symbol, "market", "buy", amount_text, None, call_params
            )
            return self._normalize_order(result, market)

        quantity_step = exact_rules.get("quantity_step")
        if quantity_step is None and market.qty_step_raw:
            quantity_step = _decimal_from_raw(market.qty_step_raw, field="quantity step")
        if order_type == "market" and market.market_order_qty_step_raw:
            market_step = _decimal_from_raw(
                market.market_order_qty_step_raw, field="market quantity step"
            )
            self._require_exact_lattice(amount_text, market_step, field="market amount")
        if quantity_step is not None:
            self._require_exact_lattice(amount_text, quantity_step, field="amount")
        self._require_exact_exchange_precision_text(
            "amount_to_precision", market.symbol, amount_text, field="amount",
            known_step=quantity_step,
        )
        minimum_quantity = exact_rules.get("minimum_quantity")
        if minimum_quantity is not None and amount_value < Fraction(minimum_quantity):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量小于精确交易所最小量")
        raw_minimum_quantity = market.market_order_min_qty_raw if order_type == "market" else market.min_qty_raw
        raw_maximum_quantity = market.market_order_max_qty_raw if order_type == "market" else market.max_qty_raw
        if (
            raw_minimum_quantity is not None
            and (minimum_quantity is None or order_type == "market")
            and amount_value < Fraction(raw_minimum_quantity, native.raw_scale())
        ):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量小于交易所最小量")
        if raw_maximum_quantity is not None and amount_value > Fraction(raw_maximum_quantity, native.raw_scale()):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量超过交易所最大量")

        if price_text is not None:
            price_value = Fraction(price_text)
            price_tick = exact_rules.get("price_tick")
            if price_tick is None and market.price_tick_raw:
                price_tick = _decimal_from_raw(market.price_tick_raw, field="price tick")
            if price_tick is not None:
                self._require_exact_lattice(price_text, price_tick, field="price")
            self._require_exact_exchange_precision_text(
                "price_to_precision", market.symbol, price_text, field="price",
                known_step=price_tick,
            )
            if market.min_price_raw is not None and price_value < Fraction(market.min_price_raw, native.raw_scale()):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单价格低于交易所最小价格")
            if market.max_price_raw is not None and price_value > Fraction(market.max_price_raw, native.raw_scale()):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单价格超过交易所最大价格")

        min_notional_raw = market.market_order_min_notional_raw if order_type == "market" else market.min_notional_raw
        max_notional_raw = market.market_order_max_notional_raw if order_type == "market" else market.max_notional_raw
        min_notional_exact = exact_rules.get("minimum_notional")
        if min_notional_exact is not None and Fraction(min_notional_exact) <= 0:
            min_notional_exact = None
        if order_type == "market" and market.market_order_notional_filter_present:
            # The exact sidecar currently freezes the limit-order minimum, while Binance
            # exposes a separate applyToMarket/applyMinToMarket switch. Use the conservative
            # market raw bound (rounded upward) until that exact market filter is frozen too.
            min_notional_exact = None
        use_raw_minimum_notional = min_notional_raw is not None and (
            min_notional_exact is None
            or (order_type == "market" and market.market_order_notional_filter_present)
        )
        reference_price_text = price_text
        if order_type == "market" and (use_raw_minimum_notional or max_notional_raw is not None or min_notional_exact is not None):
            if max_notional_raw is not None:
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    "精确市价单的交易所最大名义额需要精确 ticker 价格，当前保持拒绝",
                )
            if reference_price_text is None:
                ticker = self.fetch_ticker(instrument)
                reference_price_text = ticker.ask_exact if side == "buy" else ticker.bid_exact
                if reference_price_text is None or Fraction(reference_price_text) <= 0:
                    raise CcxtConnectorError(CcxtErrorClass.INVALID, "缺少有效参考价")
        if use_raw_minimum_notional or max_notional_raw is not None or min_notional_exact is not None:
            if reference_price_text is None:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "限价名义额校验缺少价格")
            notional = amount_value * Fraction(reference_price_text)
            if market.contract:
                contract_size = exact_rules.get("contract_size")
                if contract_size is None:
                    raise CcxtConnectorError(
                        CcxtErrorClass.UNSUPPORTED,
                        "精确合约订单缺少精确 contractSize，拒绝名义额校验",
                    )
                notional *= Fraction(contract_size)
                if market.inverse:
                    notional = amount_value * Fraction(contract_size)
            if min_notional_exact is not None and notional < Fraction(min_notional_exact):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单名义金额低于精确最小限制")
            if use_raw_minimum_notional and notional < Fraction(min_notional_raw, native.raw_scale()):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单名义金额低于交易所最小限制")
            if max_notional_raw is not None and notional > Fraction(max_notional_raw, native.raw_scale()):
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单名义金额超过交易所最大限制")

        call_params = dict(params or {})
        if is_spot and order_type == "market" and side == "buy" and exchange_id == "okx":
            call_params.setdefault("tgtCcy", "base_ccy")
        if market.contract and not risk_preflight_verified:
            hedged = call_params.get("hedged", False)
            self.validate_contract_order_leverage_tier(
                instrument,
                side=side,
                hedged=hedged,
                amount_exact=amount_text,
                price_exact=price_text,
                leverage=risk_leverage,
                reduce_only=risk_reduce_only,
                margin_mode=risk_margin_mode,
            )
        if is_spot and exchange_id == "binance" and not risk_preflight_verified:
            self.validate_binance_spot_max_position(
                instrument,
                side=side,
                amount_unit=amount_unit or "base",
                amount_exact=amount_text,
            )
        if client_order_id:
            call_params.setdefault("clientOrderId", client_order_id)
        result = self._call(
            "create_order", market.symbol, order_type, side, amount_text, price_text, call_params
        )
        return self._normalize_order(result, market)

    @staticmethod
    def _require_exact_lattice(value: str, step: str, *, field: str) -> None:
        if Fraction(value) % Fraction(step) != 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不符合交易所精确步长")

    def _require_exact_exchange_precision_text(
        self,
        method_name: str,
        symbol: str,
        value_text: str,
        *,
        field: str,
        known_step: str | None,
    ) -> None:
        formatter = getattr(self.exchange, method_name, None)
        if not callable(formatter):
            if known_step is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    f"CCXT {method_name} 不可用，无法安全校验 {field} 精度",
                )
            return
        try:
            formatted = formatter(symbol, value_text)
            unchanged = Fraction(str(formatted)) == Fraction(value_text)
        except Exception as error:
            raise CcxtConnectorError(
                classify_ccxt_error(error),
                f"CCXT {method_name} 校验 {field} 失败: {error}",
                cause=error,
            ) from error
        if not unchanged:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"订单 {field} 经过 CCXT 精度格式化会改变数值，拒绝提交",
            )

    def _require_exact_exchange_precision(
        self,
        method_name: str,
        symbol: str,
        value_raw: int,
        *,
        field: str,
        known_step: int | None,
    ) -> None:
        """Refuse values CCXT would silently round before they reach an exchange.

        Fixed tick/decimal-place modes are checked locally as well. For significant-digit or
        unknown modes, the exchange formatter is the only available exactness oracle; if a
        formatter is absent, the order must fail closed instead of skipping the grid check.
        """
        formatter = getattr(self.exchange, method_name, None)
        if not callable(formatter):
            if known_step is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    f"CCXT {method_name} 不可用，无法安全校验 {field} 精度",
                )
            return
        requested = _decimal_from_raw(value_raw, field=field)
        try:
            formatted = formatter(symbol, requested)
            exact = Fraction(str(formatted)) == Fraction(requested)
        except (TypeError, ValueError, ArithmeticError) as error:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"CCXT {method_name} 无法精确校验 {field}",
                cause=error if isinstance(error, Exception) else None,
            ) from error
        except Exception as error:
            category = classify_ccxt_error(error)
            raise CcxtConnectorError(
                category,
                f"CCXT {method_name} 校验 {field} 失败: {error}",
                cause=error,
            ) from error
        if not exact:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"订单 {field} 经过 CCXT 精度格式化会改变数值，拒绝提交",
            )

    def set_margin_mode(
        self,
        instrument: str,
        margin_mode: str,
        *,
        params: Mapping[str, Any] | None = None,
    ) -> Any:
        if margin_mode not in {"cross", "isolated"}:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "margin_mode 必须为 cross 或 isolated")
        market = self.resolve_market(instrument)
        if not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "非合约市场不能设置合约 margin_mode")
        return self._call("set_margin_mode", margin_mode, market.symbol, dict(params or {}))

    def fetch_contract_settings(self, instrument: str) -> Mapping[str, Any]:
        """Read current account modes for a contract without changing account state.

        Position and margin modes are account settings on several venues and may not be
        changeable while positions or orders exist. Order submission therefore verifies the
        operator-configured state instead of attempting to toggle it as a per-order side effect.
        """
        market = self.resolve_market(instrument)
        if not market.contract:
            return {"contract": False, "symbol": market.symbol}

        if self.config.exchange_id.lower() == "bitget":
            options = getattr(self.exchange, "options", {})
            uta = bool(options.get("uta", False)) if isinstance(options, Mapping) else False
            if uta:
                response = self._call("privateUtaGetV3AccountSettings")
                data = response.get("data") if isinstance(response, Mapping) else None
                if not isinstance(data, Mapping):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE, "Bitget UTA account/settings 响应缺少 data"
                    )
                raw_position_mode = data.get("holdMode")
                configurations = data.get("symbolConfigList")
                matching = (
                    next(
                        (
                            row
                            for row in configurations
                            if isinstance(row, Mapping)
                            and str(row.get("symbol", "")).upper() == market.market_id.upper()
                        ),
                        None,
                    )
                    if isinstance(configurations, Sequence)
                    and not isinstance(configurations, (str, bytes))
                    else None
                )
                if matching is None:
                    raise CcxtConnectorError(
                        CcxtErrorClass.UNSUPPORTED,
                        "Bitget UTA 未返回该合约的 symbolConfigList，拒绝猜测保证金模式",
                    )
                raw_margin_mode = matching.get("marginMode")
                raw_long_leverage = matching.get("longLeverage", matching.get("leverage"))
                raw_short_leverage = matching.get("shortLeverage", matching.get("leverage"))
            else:
                margin = self._call("fetch_margin_mode", market.symbol, {})
                info = margin.get("info") if isinstance(margin, Mapping) else None
                if not isinstance(info, Mapping):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE, "Bitget fetch_margin_mode 响应缺少原始账户配置"
                    )
                raw_position_mode = info.get("posMode")
                raw_margin_mode = margin.get("marginMode") or info.get("marginMode")
                leverage = self._call("fetch_leverage", market.symbol, {})
                if not isinstance(leverage, Mapping):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE, "Bitget fetch_leverage 响应格式非法"
                    )
                raw_long_leverage = leverage.get("longLeverage")
                raw_short_leverage = leverage.get("shortLeverage")
        else:
            position = self._call("fetch_position_mode", market.symbol, {})
            margin = self._call("fetch_margin_mode", market.symbol, {})
            if not isinstance(position, Mapping) or not isinstance(margin, Mapping):
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE, "CCXT 合约模式查询返回格式非法"
                )
            hedged = position.get("hedged")
            if not isinstance(hedged, bool):
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE, "CCXT position mode 缺少 hedged 布尔值"
                )
            raw_position_mode = "hedge_mode" if hedged else "one_way_mode"
            raw_margin_mode = margin.get("marginMode")
            if self.config.exchange_id.lower() == "binance":
                leverages = self._call("fetch_leverages", [market.symbol], {})
                leverage = (
                    next(
                        (
                            row
                            for row in leverages
                            if isinstance(row, Mapping)
                            and str(row.get("symbol", "")).upper() == market.symbol.upper()
                        ),
                        None,
                    )
                    if isinstance(leverages, Sequence)
                    and not isinstance(leverages, (str, bytes))
                    else None
                )
                if leverage is None:
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE,
                        "Binance fetch_leverages 未返回目标合约配置，拒绝猜测杠杆",
                    )
            else:
                leverage = self._call(
                    "fetch_leverage", market.symbol, {"marginMode": raw_margin_mode}
                )
                if not isinstance(leverage, Mapping):
                    raise CcxtConnectorError(
                        CcxtErrorClass.EXCHANGE, "CCXT fetch_leverage 响应格式非法"
                    )
            raw_long_leverage = leverage.get("longLeverage")
            raw_short_leverage = leverage.get("shortLeverage")

        position_modes = {
            "hedge_mode": "hedge",
            "long_short_mode": "hedge",
            "one_way_mode": "one_way",
            "net_mode": "one_way",
        }
        margin_modes = {"cross": "cross", "crossed": "cross", "isolated": "isolated"}
        position_mode = position_modes.get(str(raw_position_mode or "").strip().lower())
        margin_mode = margin_modes.get(str(raw_margin_mode or "").strip().lower())
        if position_mode is None or margin_mode is None:
            raise CcxtConnectorError(
                CcxtErrorClass.EXCHANGE,
                "交易所未返回可识别的持仓模式或保证金模式，拒绝提交合约订单",
            )
        leverage_long = _positive_integer(raw_long_leverage, field="long leverage")
        leverage_short = _positive_integer(raw_short_leverage, field="short leverage")
        return {
            "contract": True,
            "symbol": market.symbol,
            "position_mode": position_mode,
            "margin_mode": margin_mode,
            "leverage_long": leverage_long,
            "leverage_short": leverage_short,
            **({"uta": uta} if self.config.exchange_id.lower() == "bitget" else {}),
        }

    def set_leverage(
        self,
        instrument: str,
        leverage: int,
        *,
        margin_mode: str | None = None,
        position_mode: str = "one_way",
        position_side: str = "net",
        params: Mapping[str, Any] | None = None,
    ) -> Any:
        if leverage < 1:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "leverage 必须大于等于 1")
        market = self.resolve_market(instrument)
        if not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "非合约市场不能设置 leverage")
        if market.max_leverage is not None and leverage > market.max_leverage:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "leverage 超过市场最大杠杆")
        call_params = dict(params or {})
        if margin_mode is not None:
            if margin_mode not in {"cross", "isolated"}:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "margin_mode 必须为 cross 或 isolated")
            call_params.setdefault("marginMode", margin_mode)
        if position_mode not in {"one_way", "hedge"}:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "position_mode 必须为 one_way 或 hedge")
        if position_side not in {"net", "long", "short"}:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "position_side 必须为 net、long 或 short")
        if position_mode == "hedge" and position_side == "net":
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "Hedge 杠杆设置必须指定 long 或 short")
        if position_mode == "hedge" and margin_mode == "isolated":
            if self.config.exchange_id.lower() == "okx":
                call_params.setdefault("posSide", position_side)
            elif self.config.exchange_id.lower() == "bitget":
                options = getattr(self.exchange, "options", {})
                uta = bool(options.get("uta", False)) if isinstance(options, Mapping) else False
                call_params.setdefault("posSide" if uta else "holdSide", position_side)
        return self._call("set_leverage", leverage, market.symbol, call_params)

    def set_position_mode(
        self,
        hedged: bool,
        *,
        instrument: str | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> Any:
        symbol = None if instrument is None else self.resolve_market(instrument).symbol
        return self._call("set_position_mode", hedged, symbol, dict(params or {}))

    def fetch_positions(
        self,
        instruments: Sequence[str] | None = None,
        *,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtPosition, ...]:
        symbols = None
        markets_by_symbol: dict[str, CcxtMarket] = {}
        if instruments is not None:
            symbols = []
            for instrument in instruments:
                market = self.resolve_market(instrument)
                symbols.append(market.symbol)
                markets_by_symbol[market.symbol] = market
        result = self._call("fetch_positions", symbols, dict(params or {}))
        if not isinstance(result, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT positions 返回类型非法")
        normalized: list[CcxtPosition] = []
        for value in result:
            if not isinstance(value, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT position 行格式非法")
            symbol = str(value.get("symbol", ""))
            market = markets_by_symbol.get(symbol)
            if market is None:
                market = self._market_by_ccxt_symbol(symbol)
            normalized.append(self._normalize_position(value, market))
        return tuple(normalized)

    def fetch_funding_rate(
        self,
        instrument: str,
        *,
        params: Mapping[str, Any] | None = None,
    ) -> CcxtFundingRate:
        market = self.resolve_market(instrument)
        if not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "非合约市场没有统一 funding rate")
        value = self._call("fetch_funding_rate", market.symbol, dict(params or {}))
        if not isinstance(value, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT funding rate 返回类型非法")
        rate = _decimal_to_integer(value.get("fundingRate", 0), 10_000, field="fundingRate")
        return CcxtFundingRate(
            exchange_id=self.config.exchange_id,
            symbol=market.symbol,
            timestamp_ms=None if value.get("timestamp") is None else int(value["timestamp"]),
            funding_rate_bps=int(rate),
            next_funding_timestamp_ms=None
            if value.get("nextFundingTimestamp") is None
            else int(value["nextFundingTimestamp"]),
            raw=value,
        )

    def fetch_ledger(
        self,
        *,
        code: str | None = None,
        since_ms: int | None = None,
        limit: int | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtCashflow, ...]:
        """读取统一 CCXT ledger，并只输出可安全归约的现金流水类型。"""
        if since_ms is not None and since_ms < 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "ledger since 必须非负")
        if limit is not None and limit <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "ledger limit 必须大于 0")
        result = self._call(
            "fetch_ledger",
            code,
            since_ms,
            limit,
            dict(params or {}),
        )
        if not isinstance(result, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT ledger 返回类型非法")
        normalized: list[CcxtCashflow] = []
        for value in result:
            if not isinstance(value, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT ledger 行格式非法")
            kind = self._ledger_kind(value)
            if kind is None:
                continue
            normalized.append(self._normalize_cashflow(value, kind, code=code))
        return tuple(normalized)

    def fetch_funding_history(
        self,
        instrument: str | None = None,
        *,
        since_ms: int | None = None,
        limit: int | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtCashflow, ...]:
        """读取合约资金费账单；不把 funding rate 观察误当成已结算现金。"""
        market = None if instrument is None else self.resolve_market(instrument)
        if market is not None and not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "非合约市场没有 funding history")
        result = self._call(
            "fetch_funding_history",
            None if market is None else market.symbol,
            since_ms,
            limit,
            dict(params or {}),
        )
        if not isinstance(result, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT funding history 返回类型非法")
        normalized: list[CcxtCashflow] = []
        for value in result:
            if not isinstance(value, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT funding history 行格式非法")
            currency = str(value.get("code") or (market.settle if market else "")).strip()
            if not currency:
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT funding history 缺少结算币种")
            normalized.append(
                self._normalize_cashflow(
                    value,
                    "funding",
                    code=currency,
                    amount_key="amount" if value.get("amount") is not None else "cost",
                )
            )
        return tuple(normalized)

    def fetch_leverage_tiers(
        self,
        instrument: str,
        *,
        margin_mode: str | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtLeverageTier, ...]:
        market = self.resolve_market(instrument)
        if not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "非合约市场没有 leverage tiers")
        exchange_id = self.config.exchange_id.lower()
        call_params = dict(params or {})
        exchange_has = getattr(self.exchange, "has", None)
        has_batch = exchange_has.get("fetchLeverageTiers") if isinstance(exchange_has, Mapping) else None
        has_market = (
            exchange_has.get("fetchMarketLeverageTiers")
            if isinstance(exchange_has, Mapping)
            else None
        )
        use_market_method = has_market is True and has_batch is False
        if use_market_method:
            if exchange_id == "okx" and margin_mode is not None:
                call_params.setdefault("marginMode", margin_mode)
            rows = self._call("fetch_market_leverage_tiers", market.symbol, call_params)
            exposure_unit = "contracts" if exchange_id == "okx" else "quote_notional"
        else:
            result = self._call("fetch_leverage_tiers", [market.symbol], call_params)
            if not isinstance(result, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT leverage tiers 返回类型非法")
            rows = result.get(market.symbol)
            if rows is None:
                rows = result.get(market.market_id)
            exposure_unit = (
                "contracts"
                if exchange_id == "binance" and market.inverse
                else "quote_notional"
            )
        if not isinstance(rows, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT leverage tiers 缺少 symbol 档位")
        tiers: list[CcxtLeverageTier] = []
        for index, value in enumerate(rows, start=1):
            if not isinstance(value, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT leverage tier 行格式非法")
            max_notional = value.get("maxNotional")
            # Exact strings are authoritative for tier admission. Keep conservative
            # compatibility projections so a venue boundary finer than 1e-9 does not
            # make the exact path fail while decoding the same metadata.
            max_notional_raw = (
                None
                if max_notional is None
                else _raw_decimal_bound(max_notional, field="maxNotional", round_up=True)
            )
            min_notional_raw = (
                _raw_decimal_bound(value.get("minNotional", 0), field="minNotional", round_up=False)
                or 0
            )
            min_notional_exact = _exact_decimal_text(
                value.get("minNotional", 0), field="minNotional"
            )
            max_notional_exact = (
                None
                if max_notional is None
                else _exact_decimal_text(max_notional, field="maxNotional")
            )
            max_leverage = value.get("maxLeverage")
            max_leverage_int = None
            if max_leverage is not None:
                try:
                    parsed_leverage = _decimal_to_integer(max_leverage, 1, field="maxLeverage")
                    if parsed_leverage >= 1:
                        max_leverage_int = parsed_leverage
                except CcxtConnectorError:
                    max_leverage_int = None
            maintenance = value.get("maintenanceMarginRate", value.get("maintenanceMarginPercent", 0))
            maintenance_bps = _decimal_to_integer(
                maintenance,
                10_000 if "maintenanceMarginRate" in value else 100,
                field="maintenanceMarginRate",
            )
            initial = value.get("initialMarginRate", value.get("initialMarginPercent"))
            if initial is None:
                initial_bps = 0 if max_leverage_int is None else (10_000 + max_leverage_int - 1) // max_leverage_int
            else:
                initial_bps = _decimal_to_integer(
                    initial,
                    10_000 if "initialMarginRate" in value else 100,
                    field="initialMarginRate",
                )
            if maintenance_bps < 0 or initial_bps < 0:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "CCXT leverage tier 保证金率不能为负")
            tiers.append(
                CcxtLeverageTier(
                    symbol=market.symbol,
                    tier=int(value.get("tier", index)),
                    min_notional_raw=min_notional_raw,
                    max_notional_raw=max_notional_raw,
                    min_notional_exact=min_notional_exact,
                    max_notional_exact=max_notional_exact,
                    exposure_unit=exposure_unit,
                    initial_margin_bps=initial_bps,
                    maintenance_margin_bps=maintenance_bps,
                    max_leverage=max_leverage_int,
                )
            )
        tiers.sort(
            key=lambda tier: (
                tier.max_notional_exact is None,
                Fraction(tier.max_notional_exact)
                if tier.max_notional_exact is not None
                else Fraction(0),
                tier.tier,
            )
        )
        return tuple(tiers)

    @staticmethod
    def _ledger_kind(value: Mapping[str, Any]) -> str | None:
        info = value.get("info")
        info_type = info.get("type") if isinstance(info, Mapping) else None
        text = str(value.get("type") or info_type or "").lower()
        if any(token in text for token in ("funding", "funding_fee", "fundingfee")):
            return "funding"
        if any(token in text for token in ("interest", "borrow", "loan")):
            return "interest"
        if any(token in text for token in ("settlement", "delivery", "realizedpnl", "realized_pnl")):
            return "settlement"
        if any(token in text for token in ("deposit", "withdraw", "transfer", "rebate")):
            return "transfer"
        return None

    def _normalize_cashflow(
        self,
        value: Mapping[str, Any],
        kind: str,
        *,
        code: str | None,
        amount_key: str = "amount",
    ) -> CcxtCashflow:
        amount = value.get(amount_key)
        if amount is None:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"CCXT {kind} 账单缺少 amount")
        currency = str(value.get("currency") or value.get("code") or code or "").strip()
        if not currency:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, f"CCXT {kind} 账单缺少 currency")
        external_id = str(
            value.get("id")
            or value.get("referenceId")
            or value.get("reference_id")
            or ""
        ).strip()
        if not external_id:
            payload = json.dumps(value, sort_keys=True, default=str, separators=(",", ":"))
            external_id = "hash-" + hashlib.sha256(payload.encode("utf-8")).hexdigest()
        timestamp = value.get("timestamp")
        return CcxtCashflow(
            exchange_id=self.config.exchange_id,
            external_id=external_id,
            currency=currency,
            kind=kind,
            amount_raw=_signed_raw_decimal(amount, field=f"{kind}.amount"),
            timestamp_ms=None if timestamp is None else int(timestamp),
            raw=value,
        )

    def fetch_open_orders(
        self,
        instrument: str | None = None,
        *,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtOrder, ...]:
        symbol = None if instrument is None else self.resolve_market(instrument)
        result = self._call(
            "fetch_open_orders",
            None if symbol is None else symbol.symbol,
            None,
            None,
            dict(params or {}),
        )
        if not isinstance(result, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT open orders 返回类型非法")
        return tuple(
            self._normalize_order(
                value,
                symbol or self._market_by_ccxt_symbol(str(value.get("symbol", ""))),
            )
            for value in result
        )

    def fetch_order(self, order_id: str, instrument: str, *, params: Mapping[str, Any] | None = None) -> CcxtOrder:
        market = self.resolve_market(instrument)
        return self._normalize_order(
            self._call("fetch_order", order_id, market.symbol, dict(params or {})), market
        )

    def fetch_my_trades(
        self,
        instrument: str,
        *,
        since_ms: int | None = None,
        limit: int | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtTrade, ...]:
        market = self.resolve_market(instrument)
        result = self._call(
            "fetch_my_trades",
            market.symbol,
            since_ms,
            limit,
            dict(params or {}),
        )
        if not isinstance(result, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT my trades 返回类型非法")
        return tuple(self._normalize_trade(value, market) for value in result)

    def fetch_order_trades(
        self,
        order_id: str,
        instrument: str,
        *,
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtTrade, ...]:
        """Fetch trades for exactly one order using the venue's CCXT filter key.

        CCXT does not provide a portable ``order`` parameter for ``fetch_my_trades``:
        Binance and Bitget use ``orderId`` while OKX uses ``ordId``. Even when the
        venue receives the filter, verify each returned trade's normalized order ID so
        an ignored/unsupported filter cannot charge another order's fees or cost basis.
        """
        normalized_order_id = str(order_id).strip()
        if not normalized_order_id:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "order_id 不能为空")
        market = self.resolve_market(instrument)
        order_filter = {
            "binance": "orderId",
            "okx": "ordId",
            "bitget": "orderId",
        }.get(self.config.exchange_id.lower())
        if order_filter is None:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                f"交易所 {self.config.exchange_id} 未配置按订单过滤逐笔成交的参数",
            )
        call_params = dict(params or {})
        call_params.pop("order", None)
        call_params[order_filter] = normalized_order_id
        # Fill history is paginated on the supported exchanges. Recovery must see all
        # fills for this order before using trade boundaries as a cumulative-cost proof.
        call_params.setdefault("paginate", True)
        result = self._call(
            "fetch_my_trades", market.symbol, None, None, call_params
        )
        if not isinstance(result, Sequence) or isinstance(result, (str, bytes)):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order trades 返回类型非法")
        trades: list[CcxtTrade] = []
        for value in result:
            if not isinstance(value, Mapping):
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE, "CCXT order trades 行格式非法"
                )
            trade = self._normalize_trade(value, market)
            if trade.order_id is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.EXCHANGE,
                    "CCXT 按订单查询成交返回缺少 order id，拒绝混入无法归属的成交",
                )
            if trade.order_id == normalized_order_id:
                trades.append(trade)
        return tuple(trades)

    def cancel_order(self, order_id: str, instrument: str, *, params: Mapping[str, Any] | None = None) -> CcxtOrder:
        market = self.resolve_market(instrument)
        return self._normalize_order(
            self._call("cancel_order", order_id, market.symbol, dict(params or {})), market
        )

    def fetch_balance(self, *, params: Mapping[str, Any] | None = None) -> Mapping[str, Any]:
        result = self._call("fetch_balance", dict(params or {}))
        if not isinstance(result, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT balance 返回类型非法")
        return result

    @staticmethod
    def exact_balance_fields(value: Mapping[str, Any]) -> dict[str, dict[str, str]]:
        """Preserve normalized balance values as decimal strings for Rust snapshots."""
        exact: dict[str, dict[str, str]] = {}
        for field in ("free", "used", "debt", "borrowed", "total"):
            amounts = value.get(field)
            if not isinstance(amounts, Mapping):
                continue
            rows: dict[str, str] = {}
            for asset, amount in amounts.items():
                if amount is None:
                    continue
                text = _exact_decimal_text(
                    amount, field=f"balance.{field}.{asset}", allow_negative=True
                )
                if text is None:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"balance.{field}.{asset} 无法无损编码为 18 位精确十进制",
                    )
                rows[str(asset)] = text
            exact[field] = rows
        return exact

    def _normalize_order(self, result: Any, market: CcxtMarket) -> CcxtOrder:
        if not isinstance(result, Mapping) or not str(result.get("id", "")).strip():
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order 返回缺少 id")
        amount = result.get("amount", 0)
        filled = result.get("filled", 0)
        remaining = result.get("remaining")
        amount_exact = self._required_exact_decimal(amount, field="amount")
        filled_exact = self._required_exact_decimal(filled, field="filled")
        amount_fraction = Fraction(amount_exact)
        filled_fraction = Fraction(filled_exact)
        if filled_fraction > amount_fraction:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "CCXT order filled 超过 amount")
        remaining_exact = (
            _exact_decimal_text(amount_fraction - filled_fraction, field="remaining")
            if remaining is None
            else self._required_exact_decimal(remaining, field="remaining")
        )
        if remaining_exact is None:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "CCXT order remaining 无法精确表示")
        price_exact = self._optional_exact_decimal(result.get("price"), field="price")
        average_exact = self._optional_exact_decimal(result.get("average"), field="average")
        cost_exact = self._optional_exact_decimal(result.get("cost"), field="cost")
        if cost_exact is None and filled_fraction > 0:
            execution_price = average_exact or price_exact
            if execution_price is not None:
                cost_exact = _exact_decimal_text(
                    filled_fraction * Fraction(execution_price), field="cost"
                )
        fee_raw, fee_currency, fees, exact_fees = self._normalize_fees_with_exact(result)
        return CcxtOrder(
            exchange_id=self.config.exchange_id,
            order_id=str(result["id"]),
            client_order_id=str(result.get("clientOrderId") or ""),
            symbol=market.symbol,
            status=str(result.get("status") or "unknown"),
            side=str(result.get("side") or "unknown"),
            order_type=str(result.get("type") or "unknown"),
            amount_raw=_raw_decimal_projection(amount_exact, field="amount"),
            filled_raw=_raw_decimal_projection(filled_exact, field="filled"),
            remaining_raw=_raw_decimal_projection(remaining_exact, field="remaining"),
            price_raw=None if price_exact is None else _raw_decimal_projection(price_exact, field="price"),
            average_raw=None if average_exact is None else _raw_decimal_projection(average_exact, field="average"),
            fee_raw=fee_raw,
            fee_currency=fee_currency,
            fees=fees,
            timestamp_ms=None if result.get("timestamp") is None else int(result["timestamp"]),
            raw=result,
            amount_exact=amount_exact,
            filled_exact=filled_exact,
            remaining_exact=remaining_exact,
            price_exact=price_exact,
            average_exact=average_exact,
            cost_exact=cost_exact,
            exact_fees=exact_fees,
        )

    @staticmethod
    def _required_exact_decimal(value: Any, *, field: str) -> str:
        text = _exact_decimal_text(value, field=field)
        if text is None:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID,
                f"CCXT {field} 无法无损编码为 18 位精确十进制",
            )
        return text

    @classmethod
    def _optional_exact_decimal(cls, value: Any, *, field: str) -> str | None:
        return None if value is None else cls._required_exact_decimal(value, field=field)

    @classmethod
    def _normalize_fees(
        cls, value: Mapping[str, Any]
    ) -> tuple[int, str | None, tuple[tuple[int, str], ...]]:
        fee_raw, fee_currency, fees, _ = cls._normalize_fees_with_exact(value)
        return fee_raw, fee_currency, fees

    @staticmethod
    def _raw_fee_amounts_by_currency(value: Mapping[str, Any]) -> dict[str, list[Fraction]]:
        """Read exact fee strings from CCXT's unmodified exchange ``info`` payload."""
        info = value.get("info")
        if not isinstance(info, Mapping):
            return {}
        rows_by_currency: dict[str, list[Fraction]] = {}

        def add(amount: Any, currency: Any) -> None:
            if not isinstance(amount, (str, int)) or isinstance(amount, bool) or currency is None:
                return
            code = str(currency).strip().upper()
            if not code:
                return
            text = _exact_decimal_text(amount, field="info.fee", allow_negative=True)
            if text is not None:
                rows_by_currency.setdefault(code, []).append(Fraction(text))

        fills = info.get("fills")
        if isinstance(fills, Sequence) and not isinstance(fills, (str, bytes, bytearray)):
            for fill in fills:
                if isinstance(fill, Mapping):
                    add(fill.get("commission"), fill.get("commissionAsset"))

        if not any(rows_by_currency.values()):
            add(info.get("commission"), info.get("commissionAsset"))

        fee_details = info.get("feeDetail")
        if isinstance(fee_details, Mapping):
            fee_details = [fee_details]
        if isinstance(fee_details, Sequence) and not isinstance(
            fee_details, (str, bytes, bytearray)
        ):
            for detail in fee_details:
                if not isinstance(detail, Mapping):
                    continue
                amount = detail.get("totalFee")
                if amount is None:
                    amount = detail.get("fee")
                currency = detail.get("feeCoin") or detail.get("feeCcy")
                add(amount, currency)

        if not any(rows_by_currency.values()):
            amount = info.get("fee")
            currency = (
                info.get("feeCcy")
                or info.get("feeCoin")
                or info.get("feeCurrency")
                or info.get("currency")
            )
            add(amount, currency)
        return rows_by_currency

    @classmethod
    def _exact_normalized_fee_total(
        cls,
        items: Sequence[Mapping[str, Any]],
        *,
        raw_info: Mapping[str, Any],
    ) -> tuple[tuple[str, str], ...]:
        costs_by_currency: dict[str, list[Any]] = {}
        for item in items:
            if item.get("cost") is None:
                continue
            currency = item.get("currency")
            normalized_currency = None if currency is None else str(currency).strip().upper() or None
            if normalized_currency is None:
                # Zero rows can be ignored without a currency. Non-zero values need one.
                zero_text = _exact_decimal_text(
                    item["cost"], field="fees.cost", allow_negative=True
                )
                if zero_text is not None and Fraction(zero_text) == 0:
                    continue
                raise CcxtConnectorError(
                    CcxtErrorClass.UNSUPPORTED,
                    "CCXT 返回非零手续费但缺少币种，不能安全记账",
                )
            costs_by_currency.setdefault(normalized_currency, []).append(item["cost"])

        raw_fees = cls._raw_fee_amounts_by_currency(raw_info)
        totals: dict[str, Fraction] = {}
        for currency, costs in costs_by_currency.items():
            if all(type(cost) is float and cost == 0.0 for cost in costs) and not raw_fees.get(currency):
                continue
            if any(type(cost) is float for cost in costs):
                if not all(type(cost) is float for cost in costs):
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        "CCXT 同币种费用混合原始十进制与 float，无法无损归并",
                    )
                signs = {-1 if cost < 0 else 1 for cost in costs if cost != 0.0}
                if len(signs) > 1:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        "CCXT 同币种费用包含 float 费用与返佣，无法无损归并",
                    )
                raw_values = raw_fees.get(currency, [])
                if not raw_values:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"CCXT {currency} 手续费已降为 float，原始 info 中没有可核对的精确金额",
                    )
                raw_total = sum((abs(amount) for amount in raw_values), Fraction(0))
                expected = float(raw_total) * (-1 if signs == {-1} else 1)
                observed = sum(costs)
                if expected != observed:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"CCXT {currency} 归一化手续费与原始 info 精确金额不匹配",
                    )
                total = raw_total if signs != {-1} else -raw_total
            else:
                total = Fraction(0)
                for cost in costs:
                    text = _exact_decimal_text(
                        cost, field="fees.cost", allow_negative=True
                    )
                    if text is None:
                        raise CcxtConnectorError(
                            CcxtErrorClass.INVALID,
                            "CCXT fees.cost 无法无损编码为 18 位精确十进制",
                        )
                    total += Fraction(text)
            if total:
                totals[currency] = total

        exact_charges: list[tuple[str, str]] = []
        for currency, total in sorted(totals.items()):
            text = _exact_decimal_text(total, field="fees.total", allow_negative=True)
            if text is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    "CCXT 手续费合计无法无损编码为 18 位精确十进制",
                )
            exact_charges.append((text, currency))
        return tuple(exact_charges)

    @classmethod
    def _normalize_fees_with_exact(
        cls, value: Mapping[str, Any]
    ) -> tuple[int, str | None, tuple[tuple[int, str], ...], tuple[tuple[str, str], ...]]:
        # CCXT can publish both a scalar `fee` and a detailed `fees[]` array. Prefer the
        # itemized array whenever it contains costs; adding both would double charge. Exact
        # values are retained for the new wire while the raw projection remains compatibility
        # metadata and is never sufficient to admit an exact order.
        fees = value.get("fees")
        if isinstance(fees, Sequence) and not isinstance(fees, (str, bytes, bytearray)):
            fee_items = [
                item
                for item in fees
                if isinstance(item, Mapping) and item.get("cost") is not None
            ]
            if fee_items:
                exact_charges = cls._exact_normalized_fee_total(
                    fee_items, raw_info=value
                )
                charges = tuple(
                    (_raw_decimal_projection(amount, field="fees.total"), currency)
                    for amount, currency in exact_charges
                )
                if len(charges) == 1:
                    return charges[0][0], charges[0][1], charges, exact_charges
                return 0, None, charges, exact_charges

        fee = value.get("fee")
        if isinstance(fee, Mapping) and fee.get("cost") is not None:
            exact_charges = cls._exact_normalized_fee_total(
                [fee], raw_info=value
            )
            charges = tuple(
                (_raw_decimal_projection(amount, field="fee.cost"), code)
                for amount, code in exact_charges
            )
            charges_exact = exact_charges
            fee_raw = charges[0][0] if charges else 0
            fee_currency = charges[0][1] if charges else None
            return fee_raw, fee_currency, charges, charges_exact
        return 0, None, (), ()

    def _normalize_trade(self, value: Any, market: CcxtMarket) -> CcxtTrade:
        if not isinstance(value, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT trade 行格式非法")
        trade_id = str(value.get("id") or "").strip()
        if not trade_id:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT trade 缺少 id")
        price = value.get("price")
        amount = value.get("amount")
        if price is None or amount is None:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT trade 缺少 price/amount")
        amount_exact = self._required_exact_decimal(amount, field="trade.amount")
        price_exact = self._required_exact_decimal(price, field="trade.price")
        if Fraction(amount_exact) <= 0 or Fraction(price_exact) <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "CCXT trade 数量和价格必须为正")
        cost_exact = self._optional_exact_decimal(value.get("cost"), field="trade.cost")
        if cost_exact is None:
            cost_exact = _exact_decimal_text(
                Fraction(amount_exact) * Fraction(price_exact), field="trade.cost"
            )
        fee_raw, fee_currency, fees, exact_fees = self._normalize_fees_with_exact(value)
        return CcxtTrade(
            exchange_id=self.config.exchange_id,
            trade_id=trade_id,
            order_id=None if value.get("order") is None else str(value["order"]),
            symbol=market.symbol,
            side=str(value.get("side") or "unknown"),
            amount_raw=_raw_decimal_projection(amount_exact, field="trade.amount"),
            price_raw=_raw_decimal_projection(price_exact, field="trade.price"),
            fee_raw=fee_raw,
            fee_currency=fee_currency,
            fees=fees,
            timestamp_ms=None if value.get("timestamp") is None else int(value["timestamp"]),
            raw=value,
            amount_exact=amount_exact,
            price_exact=price_exact,
            cost_exact=cost_exact,
            exact_fees=exact_fees,
        )

    def _market_by_ccxt_symbol(self, symbol: str) -> CcxtMarket:
        for market in self.load_markets().values():
            if str(market.get("symbol", "")) == symbol:
                return self.resolve_market(f"{symbol}.{self.config.exchange_id}")
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"CCXT market 不存在: {symbol}")

    def _normalize_position(self, value: Mapping[str, Any], market: CcxtMarket) -> CcxtPosition:
        def raw_candidates(key: str) -> tuple[str, ...]:
            info = value.get("info")
            if not isinstance(info, Mapping):
                return ()

            def source_text(candidate: Any) -> str | None:
                if candidate is None or candidate == "" or isinstance(candidate, float):
                    return None
                return str(candidate)

            def first(*keys: str) -> str | None:
                for candidate_key in keys:
                    candidate = source_text(info.get(candidate_key))
                    if candidate is not None:
                        return candidate
                return None

            def ccxt_divide(numerator: Any, denominator: Any) -> Fraction:
                quotient = Fraction(numerator) / Fraction(denominator)
                sign = -1 if quotient < 0 else 1
                coefficient = abs(quotient.numerator) * 10**18 // quotient.denominator
                return Fraction(sign * coefficient, 10**18)

            exchange_id = self.config.exchange_id.lower()
            if key == "contracts":
                if exchange_id == "binance":
                    amount = first("positionAmt")
                    exact_amount = None if amount is None else _exact_decimal_text(
                        abs(Fraction(amount)), field="position.contracts"
                    )
                    return () if exact_amount is None else (exact_amount,)
                if exchange_id == "okx":
                    amount = first("pos")
                    exact_amount = None if amount is None else _exact_decimal_text(
                        abs(Fraction(amount)), field="position.contracts"
                    )
                    return () if exact_amount is None else (exact_amount,)
                if exchange_id == "bitget":
                    amount = first("total", "openTotalPos")
                    contract_size = (market.exact_rules or {}).get("contract_size")
                    if amount is None or contract_size is None:
                        return ()
                    quotient = Fraction(amount) / Fraction(contract_size)
                    exact_quotient = _exact_decimal_text(quotient, field="position.contracts")
                    return () if exact_quotient is None else (exact_quotient,)
                return ()

            keys_by_exchange = {
                "binance": {
                    "entryPrice": ("entryPrice",),
                    "markPrice": ("markPrice",),
                    "liquidationPrice": ("liquidationPrice",),
                    "unrealizedPnl": ("unRealizedProfit",),
                    "initialMargin": ("positionInitialMargin", "initialMargin", "isolatedMargin"),
                    "maintenanceMargin": ("maintMargin",),
                },
                "okx": {
                    "entryPrice": ("avgPx", "openAvgPx"),
                    "markPrice": ("markPx",),
                    "liquidationPrice": ("liqPx",),
                    "unrealizedPnl": ("upl",),
                    "initialMargin": ("imr",),
                    "maintenanceMargin": ("mmr",),
                },
                "bitget": {
                    "entryPrice": ("openPriceAvg", "openAvgPrice", "avgPrice"),
                    "markPrice": ("markPrice",),
                    "liquidationPrice": ("liquidationPrice",),
                    "unrealizedPnl": ("unrealizedPL", "unrealisedPnl"),
                    "initialMargin": ("marginSize", "positionBalance"),
                },
            }
            exchange_keys = keys_by_exchange.get(exchange_id, {})
            source_keys = exchange_keys.get(key)
            candidates = (
                []
                if source_keys is None
                else [
                    str(info[source_key])
                    for source_key in source_keys
                    if source_text(info.get(source_key)) is not None
                ]
            )

            if exchange_id == "binance" and key == "initialMargin":
                notional = first("notional", "notionalValue")
                leverage = first("leverage")
                if notional is not None and leverage is not None:
                    leverage_value = int(leverage)
                    if leverage_value > 0:
                        # Binance CCXT derives initial margin from notional and a
                        # leverage ratio rounded down to 8 decimal places, then
                        # rounds the resulting amount down to 8 places as well.
                        ratio_raw = 10**8 // leverage_value
                        if 1000 % leverage_value != 0:
                            ratio_raw += 1
                        margin_raw = (
                            Fraction(notional).__abs__()
                            * Fraction(ratio_raw, 10**8)
                            * 10**8
                        )
                        candidates.append(
                            _exact_decimal_text(
                                Fraction(margin_raw.numerator // margin_raw.denominator, 10**8),
                                field="position.initialMargin",
                            )
                        )

            if exchange_id == "okx" and key == "initialMargin":
                leverage = first("lever")
                mode = first("mgnMode")
                if leverage is not None and Fraction(leverage) > 0:
                    if mode == "isolated" and market.inverse:
                        position_amount = first("pos")
                        entry_price = first("avgPx", "openAvgPx")
                        contract_size = (market.exact_rules or {}).get("contract_size")
                        if position_amount is not None and entry_price is not None and contract_size is not None:
                            base_exposure = (
                                abs(Fraction(position_amount)) * Fraction(contract_size)
                            )
                            candidates.append(
                                _exact_decimal_text(
                                    ccxt_divide(
                                        ccxt_divide(base_exposure, entry_price),
                                        leverage,
                                    ),
                                    field="position.initialMargin",
                                )
                            )
                    elif mode == "isolated":
                        notional = first("notionalUsd")
                        if notional is not None:
                            initial_margin_ratio = ccxt_divide(1, leverage)
                            candidates.append(
                                _exact_decimal_text(
                                    initial_margin_ratio * Fraction(notional),
                                    field="position.initialMargin",
                                )
                            )

            # CCXT derives Bitget's margin fields from the untouched API strings.
            # Rebuild those formulas with rational arithmetic instead of trusting its
            # float-normalized result.
            if exchange_id == "bitget" and key in {"initialMargin", "maintenanceMargin"}:
                amount = first("total", "openTotalPos")
                entry = first("openPriceAvg", "openAvgPrice", "avgPrice")
                mark = first("markPrice")
                leverage = first("leverage")
                margin_mode = first("marginMode")
                rate = first("keepMarginRate")
                if key == "initialMargin" and margin_mode not in {"crossed", "cross"}:
                    if amount is not None and entry is not None and leverage is not None:
                        candidates.append(
                            _exact_decimal_text(
                                    ccxt_divide(Fraction(amount) * Fraction(entry), leverage),
                                field="position.initialMargin",
                            )
                        )
                elif key == "maintenanceMargin" and all(
                    item is not None for item in (amount, mark, rate)
                ):
                    notional = Fraction(amount) * Fraction(mark)
                    candidates.append(
                        _exact_decimal_text(
                            notional * (Fraction(rate) + Fraction("0.0006")),
                            field="position.maintenanceMargin",
                        )
                    )
            return tuple(candidate for candidate in candidates if candidate is not None)

        def exact_value(key: str, *, signed: bool = False) -> str | None:
            normalized = value.get(key)
            if normalized is None:
                return None

            try:
                candidates = raw_candidates(key) if isinstance(normalized, float) else (str(normalized),)
            except (TypeError, ValueError, ZeroDivisionError, OverflowError) as error:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    f"position {key} 的交易所原始字段无效，无法精确恢复",
                ) from error
            if isinstance(normalized, float):
                raw_candidates_found = candidates
                matched_candidates = []
                for candidate in raw_candidates_found:
                    if _exact_decimal_text(candidate, field=key, allow_negative=signed) is None:
                        continue
                    try:
                        if float(candidate) == normalized:
                            matched_candidates.append(candidate)
                    except (TypeError, ValueError, OverflowError):
                        continue
                if not matched_candidates and key == "maintenanceMargin" and not raw_candidates_found:
                    # Some exchanges derive this informational field from private
                    # risk tiers that are not present in the position payload.
                    return None
                candidates = tuple(matched_candidates)
                if not candidates:
                    raise CcxtConnectorError(
                        CcxtErrorClass.INVALID,
                        f"position {key} 已被 CCXT 转成浮点数且无法从交易所原始字段精确恢复",
                    )
            text = _exact_decimal_text(candidates[0], field=key, allow_negative=signed)
            if text is None:
                raise CcxtConnectorError(
                    CcxtErrorClass.INVALID,
                    f"position {key} 无法无损编码为 18 位精确十进制",
                )
            return text

        leverage = value.get("leverage")
        contract_size_exact = (
            market.exact_rules.get("contract_size")
            if market.exact_rules is not None and market.exact_rules.get("contract_size") is not None
            else exact_value("contractSize")
        )
        contracts_exact = exact_value("contracts")
        entry_price_exact = exact_value("entryPrice")
        mark_price_exact = exact_value("markPrice")
        liquidation_price_exact = exact_value("liquidationPrice")
        unrealized_pnl_exact = exact_value("unrealizedPnl", signed=True)
        initial_margin_exact = exact_value("initialMargin", signed=True)
        maintenance_margin_exact = exact_value("maintenanceMargin", signed=True)
        return CcxtPosition(
            exchange_id=self.config.exchange_id,
            symbol=market.symbol,
            settlement_currency=market.settle,
            side=str(value.get("side") or "unknown"),
            contracts_raw=0 if contracts_exact is None else _raw_decimal_projection(contracts_exact, field="contracts"),
            contract_size_raw=market.contract_size_raw
            if contract_size_exact is None
            else _raw_decimal_projection(contract_size_exact, field="contractSize"),
            entry_price_raw=None if entry_price_exact is None else _raw_decimal_projection(entry_price_exact, field="entryPrice"),
            mark_price_raw=None if mark_price_exact is None else _raw_decimal_projection(mark_price_exact, field="markPrice"),
            liquidation_price_raw=None if liquidation_price_exact is None else _raw_decimal_projection(liquidation_price_exact, field="liquidationPrice"),
            unrealized_pnl_raw=None if unrealized_pnl_exact is None else _raw_decimal_projection(unrealized_pnl_exact, field="unrealizedPnl"),
            initial_margin_raw=None if initial_margin_exact is None else _raw_decimal_projection(initial_margin_exact, field="initialMargin"),
            maintenance_margin_raw=None if maintenance_margin_exact is None else _raw_decimal_projection(maintenance_margin_exact, field="maintenanceMargin"),
            leverage=None if leverage is None else int(leverage),
            margin_mode=None if value.get("marginMode") is None else str(value["marginMode"]),
            hedged=None if value.get("hedged") is None else bool(value["hedged"]),
            contracts_exact=contracts_exact,
            contract_size_exact=contract_size_exact,
            entry_price_exact=entry_price_exact,
            mark_price_exact=mark_price_exact,
            liquidation_price_exact=liquidation_price_exact,
            unrealized_pnl_exact=unrealized_pnl_exact,
            initial_margin_exact=initial_margin_exact,
            maintenance_margin_exact=maintenance_margin_exact,
            raw=value,
        )


async def watch_ohlcv(
    exchange: Any,
    instrument: str,
    *,
    timeframe: str = "1m",
    limit: int | None = None,
    params: Mapping[str, Any] | None = None,
) -> Any:
    """调用注入的 CCXT Pro exchange 的统一 ``watch_ohlcv`` 方法。"""

    return await watch_stream(
        exchange,
        "ohlcv",
        instrument=instrument,
        timeframe=timeframe,
        limit=limit,
        params=params,
    )


__all__ = [
    "CcxtConfig",
    "CcxtConnectorError",
    "CcxtErrorClass",
    "CcxtExchangeClient",
    "CcxtMarket",
    "CcxtOrderBook",
    "CcxtOrderBookLevel",
    "CcxtLeverageTier",
    "CcxtOrder",
    "CcxtPosition",
    "CcxtFundingRate",
    "CcxtCashflow",
    "CcxtTrade",
    "CcxtTicker",
    "classify_ccxt_error",
    "SCALE",
    "create_ccxt_pro_exchange",
    "normalize_stream_event",
    "watch_stream",
    "watch_ohlcv",
]

from .provider import CcxtProviderAdapter, MAX_PROVIDER_BAR_ROWS, register_ccxt_provider

__all__ += ["CcxtProviderAdapter", "MAX_PROVIDER_BAR_ROWS", "register_ccxt_provider"]
