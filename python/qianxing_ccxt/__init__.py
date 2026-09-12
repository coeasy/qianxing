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
import time
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation, ROUND_DOWN
from enum import Enum
from typing import Any, Mapping, Sequence

from qianxing_bridge import BarFrame


SCALE = 1_000_000_000


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

    name = type(error).__name__.lower()
    if any(token in name for token in ("ratelimit", "ddos", "throttle")):
        return CcxtErrorClass.RATE_LIMITED
    if any(token in name for token in ("timeout", "network", "connection", "unavailable")):
        return CcxtErrorClass.RETRYABLE
    if any(token in name for token in ("authentication", "permission", "credential", "nonce")):
        return CcxtErrorClass.AUTHENTICATION
    if any(token in name for token in ("badrequest", "invalidorder", "arguments", "symbol")):
        return CcxtErrorClass.INVALID
    if "notupported" in name or "notimplemented" in name:
        return CcxtErrorClass.UNSUPPORTED
    if "exchange" in name or "order" in name:
        return CcxtErrorClass.EXCHANGE
    return CcxtErrorClass.UNKNOWN


def _raw_decimal(value: Any, *, field: str) -> int:
    try:
        decimal = Decimal(str(value))
    except (InvalidOperation, ValueError) as error:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不是有效数字: {value}") from error
    if not decimal.is_finite() or decimal < 0:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 必须是非负有限数字: {value}")
    scaled = decimal * SCALE
    if scaled != scaled.to_integral_value():
        raise CcxtConnectorError(
            CcxtErrorClass.INVALID,
            f"{field} 超过牵星定点精度 1e-9: {value}",
        )
    return int(scaled)


def _signed_raw_decimal(value: Any, *, field: str) -> int:
    """把允许正负号的账单金额转成定点整数。"""
    try:
        decimal = Decimal(str(value))
    except (InvalidOperation, ValueError) as error:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不是有效数字: {value}") from error
    if not decimal.is_finite():
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 必须是有限数字: {value}")
    scaled = decimal * SCALE
    if scaled != scaled.to_integral_value():
        raise CcxtConnectorError(
            CcxtErrorClass.INVALID,
            f"{field} 超过牵星定点精度 1e-9: {value}",
        )
    return int(scaled)


def _decimal_from_raw(value: int, *, field: str) -> str:
    if value < 0:
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"{field} 不能为负: {value}")
    return format(Decimal(value) / SCALE, "f")


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

    def validate(self) -> None:
        if not self.exchange_id.strip() or not self.exchange_id.replace("_", "").isalnum():
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "exchange_id 非法")
        if self.timeout_ms <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "timeout_ms 必须大于 0")
        if not self.default_type.strip():
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "default_type 不能为空")
        if not 0 <= self.ws_max_retries <= 10:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "ws_max_retries 必须在 0..10 内")
        if not 0 <= self.ws_retry_backoff_ms <= 60_000:
            raise CcxtConnectorError(
                CcxtErrorClass.INVALID, "ws_retry_backoff_ms 必须在 0..60000 内"
            )


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
    qty_step_raw: int | None
    min_qty_raw: int | None
    max_qty_raw: int | None
    max_leverage: int | None
    maintenance_margin_bps: int | None

    @property
    def instrument(self) -> str:
        return f"{self.symbol}.{self.exchange_id.upper()}"


@dataclass(frozen=True)
class CcxtLeverageTier:
    symbol: str
    tier: int
    min_notional_raw: int
    max_notional_raw: int | None
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
    timestamp_ms: int | None
    raw: Mapping[str, Any]


@dataclass(frozen=True)
class CcxtPosition:
    exchange_id: str
    symbol: str
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
    timestamp_ms: int | None
    raw: Mapping[str, Any]


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
        "bid_raw": _raw_decimal(value.get("bid", 0), field="stream.bid"),
        "ask_raw": _raw_decimal(value.get("ask", 0), field="stream.ask"),
        "bid_qty_raw": _raw_decimal(value.get("bidVolume", 0), field="stream.bidVolume"),
        "ask_qty_raw": _raw_decimal(value.get("askVolume", 0), field="stream.askVolume"),
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
                    "未安装公共 CCXT Pro；请安装 qianxing-bridge[ccxt-pro] 或 ccxtpro 包",
                    cause=error,
                ) from error
    exchange_type = getattr(module, config.exchange_id, None)
    if exchange_type is None:
        raise CcxtConnectorError(
            CcxtErrorClass.UNSUPPORTED,
            f"CCXT Pro 不支持交易所: {config.exchange_id}",
        )
    exchange = exchange_type(_ccxt_constructor_params(config))
    if config.sandbox and hasattr(exchange, "set_sandbox_mode"):
        exchange.set_sandbox_mode(True)
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
        if config.sandbox and hasattr(self.exchange, "set_sandbox_mode"):
            self.exchange.set_sandbox_mode(True)
        self._markets: Mapping[str, Mapping[str, Any]] | None = None

    @staticmethod
    def _load_ccxt() -> Any:
        try:
            return importlib.import_module("ccxt")
        except ImportError as error:
            raise CcxtConnectorError(
                CcxtErrorClass.UNSUPPORTED,
                "未安装公共 ccxt；请安装 qianxing-bridge[ccxt] 或 ccxt 包",
                cause=error,
            ) from error

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
        precision = market.get("precision")
        precision = precision if isinstance(precision, Mapping) else {}
        price_tick_raw = self._precision_step_raw(precision.get("price"))
        qty_step_raw = self._precision_step_raw(precision.get("amount"))
        limits = market.get("limits")
        limits = limits if isinstance(limits, Mapping) else {}
        amount_limits = limits.get("amount")
        amount_limits = amount_limits if isinstance(amount_limits, Mapping) else {}
        min_qty_raw = (
            None
            if amount_limits.get("min") is None
            else _raw_decimal(amount_limits["min"], field="limits.amount.min")
        )
        max_qty_raw = (
            None
            if amount_limits.get("max") is None
            else _raw_decimal(amount_limits["max"], field="limits.amount.max")
        )
        leverage_limits = limits.get("leverage")
        leverage_limits = leverage_limits if isinstance(leverage_limits, Mapping) else {}
        max_leverage_value = leverage_limits.get("max", market.get("maxLeverage"))
        max_leverage = None
        if max_leverage_value is not None:
            try:
                leverage_decimal = Decimal(str(max_leverage_value))
                if leverage_decimal.is_finite() and leverage_decimal >= 1 and leverage_decimal == leverage_decimal.to_integral_value():
                    max_leverage = int(leverage_decimal)
            except (InvalidOperation, ValueError):
                max_leverage = None
        maintenance_margin_bps = self._maintenance_margin_bps(market)
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
            qty_step_raw=qty_step_raw,
            min_qty_raw=min_qty_raw,
            max_qty_raw=max_qty_raw,
            max_leverage=max_leverage,
            maintenance_margin_bps=maintenance_margin_bps,
        )

    def _precision_step_raw(self, value: Any) -> int | None:
        if value is None:
            return None
        mode = getattr(self.exchange, "precisionMode", None)
        tick_size_mode = getattr(self.exchange, "TICK_SIZE", 4)
        decimal_places_mode = getattr(self.exchange, "DECIMAL_PLACES", 2)
        if mode == tick_size_mode:
            return _raw_decimal(value, field="precision")
        if mode == decimal_places_mode:
            try:
                places = int(value)
            except (TypeError, ValueError):
                return None
            if 0 <= places <= 9:
                return 10 ** (9 - places)
            return None
        # SIGNIFICANT_DIGITS 无法脱离具体价格无损转换成固定 tick，交给部署侧覆盖。
        return None

    @staticmethod
    def _maintenance_margin_bps(market: Mapping[str, Any]) -> int | None:
        rate = market.get("maintenanceMarginRate")
        if rate is not None:
            try:
                decimal = Decimal(str(rate)) * Decimal(10_000)
                if decimal.is_finite() and decimal >= 0 and decimal == decimal.to_integral_value():
                    return int(decimal)
            except (InvalidOperation, ValueError):
                return None
        percent = market.get("maintenanceMarginPercent")
        if percent is not None:
            try:
                decimal = Decimal(str(percent)) * Decimal(100)
                if decimal.is_finite() and decimal >= 0 and decimal == decimal.to_integral_value():
                    return int(decimal)
            except (InvalidOperation, ValueError):
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
            previous = since
            for row in batch:
                if not isinstance(row, Sequence) or len(row) < 6:
                    raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT OHLCV 行格式非法")
                timestamp = int(row[0])
                if timestamp < start_ms or timestamp > end_ms:
                    continue
                rows[timestamp] = row
                previous = max(previous, timestamp)
            if len(batch) < limit or previous <= since:
                break
            since = previous + 1
            if since > end_ms:
                break
        ordered = [rows[timestamp] for timestamp in sorted(rows)]
        if not ordered:
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT OHLCV 查询为空")
        return BarFrame(
            instrument=instrument,
            source=f"ccxt:{self.config.exchange_id}:{market.symbol}:{timeframe}",
            ts=tuple(int(row[0]) for row in ordered),
            open_raw=tuple(_raw_decimal(row[1], field="open") for row in ordered),
            high_raw=tuple(_raw_decimal(row[2], field="high") for row in ordered),
            low_raw=tuple(_raw_decimal(row[3], field="low") for row in ordered),
            close_raw=tuple(_raw_decimal(row[4], field="close") for row in ordered),
            volume_raw=tuple(_raw_decimal(row[5], field="volume") for row in ordered),
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
            bid_raw=_raw_decimal(bid, field="bid"),
            ask_raw=_raw_decimal(ask, field="ask"),
            bid_qty_raw=_raw_decimal(0 if bid_qty is None else bid_qty, field="bidVolume"),
            ask_qty_raw=_raw_decimal(0 if ask_qty is None else ask_qty, field="askVolume"),
            last_raw=_raw_decimal(ticker.get("last"), field="last"),
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

    def create_order(
        self,
        instrument: str,
        *,
        side: str,
        order_type: str,
        amount_raw: int,
        price_raw: int | None = None,
        client_order_id: str | None = None,
        params: Mapping[str, Any] | None = None,
    ) -> CcxtOrder:
        market = self.resolve_market(instrument)
        if side not in {"buy", "sell"} or order_type not in {"market", "limit"} or amount_raw <= 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单 side/type/amount 非法")
        if market.qty_step_raw and amount_raw % market.qty_step_raw != 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量不符合交易所 qty step")
        if market.min_qty_raw is not None and amount_raw < market.min_qty_raw:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量小于交易所最小数量")
        if market.max_qty_raw is not None and amount_raw > market.max_qty_raw:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单数量超过交易所最大数量")
        if order_type == "limit" and (price_raw is None or price_raw <= 0):
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "限价单必须提供正 price")
        if price_raw is not None and market.price_tick_raw and price_raw % market.price_tick_raw != 0:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "订单价格不符合交易所 price tick")
        call_params = dict(params or {})
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

    def set_leverage(
        self,
        instrument: str,
        leverage: int,
        *,
        margin_mode: str | None = None,
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
        rate = Decimal(str(value.get("fundingRate", 0))) * 10_000
        if rate != rate.to_integral_value():
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "fundingRate 超过基点精度")
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
        params: Mapping[str, Any] | None = None,
    ) -> tuple[CcxtLeverageTier, ...]:
        market = self.resolve_market(instrument)
        if not market.contract:
            raise CcxtConnectorError(CcxtErrorClass.INVALID, "非合约市场没有 leverage tiers")
        result = self._call("fetch_leverage_tiers", [market.symbol], dict(params or {}))
        if not isinstance(result, Mapping):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT leverage tiers 返回类型非法")
        rows = result.get(market.symbol)
        if rows is None:
            rows = result.get(market.market_id)
        if not isinstance(rows, Sequence):
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT leverage tiers 缺少 symbol 档位")
        tiers: list[CcxtLeverageTier] = []
        for index, value in enumerate(rows, start=1):
            if not isinstance(value, Mapping):
                raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT leverage tier 行格式非法")
            max_notional = value.get("maxNotional")
            max_notional_raw = None if max_notional is None else _raw_decimal(max_notional, field="maxNotional")
            min_notional_raw = _raw_decimal(value.get("minNotional", 0), field="minNotional")
            max_leverage = value.get("maxLeverage")
            max_leverage_int = None
            if max_leverage is not None:
                try:
                    parsed = Decimal(str(max_leverage))
                    if parsed.is_finite() and parsed >= 1 and parsed == parsed.to_integral_value():
                        max_leverage_int = int(parsed)
                except (InvalidOperation, ValueError):
                    max_leverage_int = None
            maintenance = value.get("maintenanceMarginRate", value.get("maintenanceMarginPercent", 0))
            maintenance_decimal = Decimal(str(maintenance))
            maintenance_bps = int(
                maintenance_decimal * (Decimal(10_000) if "maintenanceMarginRate" in value else Decimal(100))
            )
            initial = value.get("initialMarginRate", value.get("initialMarginPercent"))
            if initial is None:
                initial_bps = 0 if max_leverage_int is None else (10_000 + max_leverage_int - 1) // max_leverage_int
            else:
                initial_decimal = Decimal(str(initial))
                initial_bps = int(
                    initial_decimal * (Decimal(10_000) if "initialMarginRate" in value else Decimal(100))
                )
            if maintenance_bps < 0 or initial_bps < 0:
                raise CcxtConnectorError(CcxtErrorClass.INVALID, "CCXT leverage tier 保证金率不能为负")
            tiers.append(
                CcxtLeverageTier(
                    symbol=market.symbol,
                    tier=int(value.get("tier", index)),
                    min_notional_raw=min_notional_raw,
                    max_notional_raw=max_notional_raw,
                    initial_margin_bps=initial_bps,
                    maintenance_margin_bps=maintenance_bps,
                    max_leverage=max_leverage_int,
                )
            )
        tiers.sort(key=lambda tier: (tier.max_notional_raw is None, tier.max_notional_raw or 0, tier.tier))
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

    def _normalize_order(self, result: Any, market: CcxtMarket) -> CcxtOrder:
        if not isinstance(result, Mapping) or not str(result.get("id", "")).strip():
            raise CcxtConnectorError(CcxtErrorClass.EXCHANGE, "CCXT order 返回缺少 id")
        amount = result.get("amount", 0)
        filled = result.get("filled", 0)
        remaining = result.get("remaining")
        amount_raw = _raw_decimal(amount, field="amount")
        filled_raw = _raw_decimal(filled, field="filled")
        remaining_raw = _raw_decimal(
            amount if remaining is None else remaining,
            field="remaining",
        )
        fee_raw, fee_currency = self._normalize_fee(result)
        return CcxtOrder(
            exchange_id=self.config.exchange_id,
            order_id=str(result["id"]),
            client_order_id=str(result.get("clientOrderId") or ""),
            symbol=market.symbol,
            status=str(result.get("status") or "unknown"),
            side=str(result.get("side") or "unknown"),
            order_type=str(result.get("type") or "unknown"),
            amount_raw=amount_raw,
            filled_raw=filled_raw,
            remaining_raw=remaining_raw,
            price_raw=None if result.get("price") is None else _raw_decimal(result["price"], field="price"),
            average_raw=None if result.get("average") is None else _raw_decimal(result["average"], field="average"),
            fee_raw=fee_raw,
            fee_currency=fee_currency,
            timestamp_ms=None if result.get("timestamp") is None else int(result["timestamp"]),
            raw=result,
        )

    @classmethod
    def _normalize_fee(cls, value: Mapping[str, Any]) -> tuple[int, str | None]:
        fee = value.get("fee")
        if isinstance(fee, Mapping) and fee.get("cost") is not None:
            return (
                _raw_decimal(fee["cost"], field="fee.cost"),
                None if fee.get("currency") is None else str(fee["currency"]),
            )
        fees = value.get("fees")
        if isinstance(fees, Sequence):
            total = 0
            currency: str | None = None
            for item in fees:
                if not isinstance(item, Mapping) or item.get("cost") is None:
                    continue
                total += _raw_decimal(item["cost"], field="fees.cost")
                if currency is None and item.get("currency") is not None:
                    currency = str(item["currency"])
            return total, currency
        return 0, None

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
        fee_raw, fee_currency = self._normalize_fee(value)
        return CcxtTrade(
            exchange_id=self.config.exchange_id,
            trade_id=trade_id,
            order_id=None if value.get("order") is None else str(value["order"]),
            symbol=market.symbol,
            side=str(value.get("side") or "unknown"),
            amount_raw=_raw_decimal(amount, field="trade.amount"),
            price_raw=_raw_decimal(price, field="trade.price"),
            fee_raw=fee_raw,
            fee_currency=fee_currency,
            timestamp_ms=None if value.get("timestamp") is None else int(value["timestamp"]),
            raw=value,
        )

    def _market_by_ccxt_symbol(self, symbol: str) -> CcxtMarket:
        for market in self.load_markets().values():
            if str(market.get("symbol", "")) == symbol:
                return self.resolve_market(f"{symbol}.{self.config.exchange_id}")
        raise CcxtConnectorError(CcxtErrorClass.INVALID, f"CCXT market 不存在: {symbol}")

    def _normalize_position(self, value: Mapping[str, Any], market: CcxtMarket) -> CcxtPosition:
        def optional_raw(key: str) -> int | None:
            return None if value.get(key) is None else _raw_decimal(value[key], field=key)

        leverage = value.get("leverage")
        return CcxtPosition(
            exchange_id=self.config.exchange_id,
            symbol=market.symbol,
            side=str(value.get("side") or "unknown"),
            contracts_raw=_raw_decimal(value.get("contracts", 0), field="contracts"),
            contract_size_raw=market.contract_size_raw
            if value.get("contractSize") is None
            else _raw_decimal(value["contractSize"], field="contractSize"),
            entry_price_raw=optional_raw("entryPrice"),
            mark_price_raw=optional_raw("markPrice"),
            liquidation_price_raw=optional_raw("liquidationPrice"),
            unrealized_pnl_raw=optional_raw("unrealizedPnl"),
            initial_margin_raw=optional_raw("initialMargin"),
            maintenance_margin_raw=optional_raw("maintenanceMargin"),
            leverage=None if leverage is None else int(leverage),
            margin_mode=None if value.get("marginMode") is None else str(value["marginMode"]),
            hedged=None if value.get("hedged") is None else bool(value["hedged"]),
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
    "CcxtLeverageTier",
    "CcxtOrder",
    "CcxtPosition",
    "CcxtFundingRate",
    "CcxtCashflow",
    "CcxtTrade",
    "CcxtTicker",
    "classify_ccxt_error",
    "create_ccxt_pro_exchange",
    "normalize_stream_event",
    "watch_stream",
    "watch_ohlcv",
]
