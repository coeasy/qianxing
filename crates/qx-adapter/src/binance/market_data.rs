//! Binance Spot 行情与交易规则边界。
//!
//! 本模块拥有公开 book-ticker 快照/流、交易对过滤器解析与精确下单约束。
//! 订单生命周期和账户事实仍由 `BinanceSpotVenue` 持有。

use super::{
    error_message, exact_decimal_projection, parse_exact_decimal, parse_exact_quantity_projection,
    parse_price, percent_encode, HttpRequest, HttpTransport, TlsWebSocketUserStream, WebSocketPoll,
    DEFAULT_HOST, DEFAULT_PORT, TESTNET_HOST,
};
use qx_core::{ExactDecimal, InstrumentId, QxError, QxResult};
use qx_guanxing::QuoteTick;
use qx_zhenlu::RateLimiter;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct BinanceSpotMarketRules {
    pub(super) fetched_at_ms: u64,
    pub(super) status: String,
    pub(super) base_asset: String,
    pub(super) quote_asset: String,
    pub(super) price_tick: i128,
    pub(super) qty_step: i128,
    pub(super) min_qty: i128,
    pub(super) min_notional: i128,
    pub(super) notional_filters: Vec<BinanceSpotNotionalFilter>,
    pub(super) percent_price: Option<BinanceSpotPercentPriceFilter>,
    pub(super) percent_price_by_side: Option<BinanceSpotPercentPriceBySideFilter>,
    pub(super) exact_price_tick: ExactDecimal,
    pub(super) exact_min_price: ExactDecimal,
    pub(super) exact_max_price: ExactDecimal,
    pub(super) exact_qty_step: ExactDecimal,
    pub(super) exact_min_qty: ExactDecimal,
    pub(super) exact_max_qty: ExactDecimal,
    pub(super) exact_market_qty_step: Option<ExactDecimal>,
    pub(super) exact_market_min_qty: Option<ExactDecimal>,
    pub(super) exact_market_max_qty: Option<ExactDecimal>,
    pub(super) exact_min_notional: ExactDecimal,
    pub(super) exact_max_position: Option<ExactDecimal>,
}

#[derive(Clone, Debug)]
pub(super) struct BinanceSpotNotionalFilter {
    pub(super) apply_min_to_market: bool,
    pub(super) apply_max_to_market: bool,
    pub(super) avg_price_mins: u32,
    pub(super) exact_min_notional: Option<ExactDecimal>,
    pub(super) exact_max_notional: Option<ExactDecimal>,
}

#[derive(Clone, Debug)]
pub(super) struct BinanceSpotPercentPriceFilter {
    pub(super) multiplier_down: ExactDecimal,
    pub(super) multiplier_up: ExactDecimal,
    pub(super) avg_price_mins: u32,
}

#[derive(Clone, Debug)]
pub(super) struct BinanceSpotPercentPriceBySideFilter {
    pub(super) bid_multiplier_down: ExactDecimal,
    pub(super) bid_multiplier_up: ExactDecimal,
    pub(super) ask_multiplier_down: ExactDecimal,
    pub(super) ask_multiplier_up: ExactDecimal,
    pub(super) avg_price_mins: u32,
}

/// 行情流一次读取窗口的结局；静默交给调用方判 staleness，不作为致命错误上抛。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BinanceQuotePoll {
    Quote(Box<QuoteTick>),
    Idle,
    Closed,
}

/// Binance Spot 公共 L1 行情快照与 `bookTicker` 流。
pub struct BinanceSpotMarketData {
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    rate_limiter: RateLimiter,
}

impl BinanceSpotMarketData {
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        Self::with_endpoint(transport, DEFAULT_HOST, DEFAULT_PORT)
    }

    pub fn testnet(transport: Arc<dyn HttpTransport>) -> Self {
        Self::with_endpoint(transport, TESTNET_HOST, DEFAULT_PORT)
    }

    pub fn with_endpoint(
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        Self {
            transport,
            host: host.into(),
            port,
            rate_limiter: RateLimiter::new(6_000, 6_000, 0),
        }
    }

    pub fn fetch_book_ticker(
        &mut self,
        instrument: &InstrumentId,
        receive_ts: u64,
    ) -> QxResult<QuoteTick> {
        if !self
            .rate_limiter
            .try_acquire(2, receive_ts.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance 公共行情请求限频".into(),
            ));
        }
        validate_binance_instrument(instrument)?;
        let request = HttpRequest {
            method: "GET".into(),
            host: self.host.clone(),
            port: self.port,
            path: format!(
                "/api/v3/ticker/bookTicker?symbol={}",
                percent_encode(&instrument.symbol().to_ascii_uppercase())
            ),
            body: String::new(),
            headers: BTreeMap::new(),
        };
        let response = self.transport.send(request).map_err(QxError::Ambiguous)?;
        if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        let (raw_symbol, quote) = parse_book_ticker(&response.body, receive_ts)?;
        if raw_symbol != instrument.symbol().to_ascii_uppercase() {
            return Err(QxError::ReconcileRequired(format!(
                "Binance 行情 symbol 不匹配: expected={}, actual={raw_symbol}",
                instrument.symbol()
            )));
        }
        Ok(quote)
    }
}

/// Binance 公共 `bookTicker` 单品种原始 WSS 流。
pub struct BinanceSpotMarketStream {
    stream: TlsWebSocketUserStream,
    instrument: InstrumentId,
}

impl BinanceSpotMarketStream {
    pub fn connect_with_endpoint(
        instrument: InstrumentId,
        host: impl Into<String>,
        port: u16,
        timeout: std::time::Duration,
    ) -> Result<Self, String> {
        validate_binance_instrument(&instrument).map_err(|error| match error {
            QxError::Permanent(message) => message,
            other => format!("Binance instrument 非法: {other:?}"),
        })?;
        let path = format!(
            "/ws/{}@bookTicker",
            instrument.symbol().to_ascii_lowercase()
        );
        let host = host.into();
        let stream = TlsWebSocketUserStream::connect(host, port, path, timeout)?;
        Ok(Self { stream, instrument })
    }

    pub fn instrument(&self) -> &InstrumentId {
        &self.instrument
    }

    pub fn recv_quote(&mut self, receive_ts: u64) -> Result<BinanceQuotePoll, String> {
        let message = match self.stream.recv_message()? {
            WebSocketPoll::Message(message) => message,
            // 一个窗口里没有行情 ≠ 链路坏了：静默要交给调用方做 staleness 判定，
            // 而不能顺着 `?` 把 worker 判成致命退出。
            WebSocketPoll::Idle => return Ok(BinanceQuotePoll::Idle),
            WebSocketPoll::Closed => return Ok(BinanceQuotePoll::Closed),
        };
        if message.opcode != 0x1 {
            return Err("Binance bookTicker 不是 JSON 文本".into());
        }
        let payload = String::from_utf8(message.payload)
            .map_err(|error| format!("Binance bookTicker 不是 UTF-8: {error}"))?;
        let (symbol, quote) = parse_book_ticker(&payload, receive_ts)
            .map_err(|error| format!("Binance bookTicker 解析失败: {error}"))?;
        if symbol != self.instrument.symbol().to_ascii_uppercase() {
            return Err(format!(
                "Binance bookTicker symbol 不匹配: expected={}, actual={symbol}",
                self.instrument.symbol()
            ));
        }
        Ok(BinanceQuotePoll::Quote(Box::new(quote)))
    }

    pub fn close(&mut self) -> Result<(), String> {
        self.stream.close()
    }
}

pub(super) fn validate_binance_instrument(instrument: &InstrumentId) -> QxResult<()> {
    if !instrument.venue.is_binance() {
        return Err(QxError::Permanent(format!(
            "Binance Venue 不接受 instrument venue: {}",
            instrument.venue
        )));
    }
    if instrument.symbol().is_empty()
        || !instrument
            .symbol()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(QxError::Permanent("Binance Spot symbol 非法".into()));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceBookTickerWire {
    #[serde(rename = "s", alias = "symbol")]
    symbol: String,
    #[serde(rename = "b", alias = "bidPrice")]
    bid: String,
    #[serde(rename = "B", alias = "bidQty")]
    bid_qty: String,
    #[serde(rename = "a", alias = "askPrice")]
    ask: String,
    #[serde(rename = "A", alias = "askQty")]
    ask_qty: String,
    #[serde(rename = "u", default)]
    update_id: u64,
    #[serde(rename = "E", default)]
    event_ts: u64,
}

pub(super) fn parse_book_ticker(payload: &str, receive_ts: u64) -> QxResult<(String, QuoteTick)> {
    let wire: BinanceBookTickerWire = serde_json::from_str(payload)
        .map_err(|error| QxError::Permanent(format!("Binance bookTicker JSON 非法: {error}")))?;
    let bid = parse_price(&wire.bid, "bidPrice")?;
    let (bid_qty, bid_qty_exact) = parse_exact_quantity_projection(&wire.bid_qty, "bidQty")?;
    let ask = parse_price(&wire.ask, "askPrice")?;
    let (ask_qty, ask_qty_exact) = parse_exact_quantity_projection(&wire.ask_qty, "askQty")?;
    if !bid_qty_exact.is_positive() || !ask_qty_exact.is_positive() {
        return Err(QxError::ReconcileRequired(
            "Binance bookTicker bidQty/askQty 必须为正".into(),
        ));
    }
    let quote = QuoteTick::new(
        if wire.event_ts == 0 {
            receive_ts
        } else {
            wire.event_ts
        },
        bid,
        bid_qty,
        ask,
        ask_qty,
        wire.update_id,
    )
    .with_exact_sizes(bid_qty_exact, ask_qty_exact);
    if quote.is_crossed() {
        return Err(QxError::ReconcileRequired(
            "Binance bookTicker 出现 crossed quote".into(),
        ));
    }
    Ok((wire.symbol.to_ascii_uppercase(), quote))
}

pub(super) fn parse_spot_market_rules(
    body: &str,
    expected_symbol: &str,
    fetched_at_ms: u64,
) -> QxResult<BinanceSpotMarketRules> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| QxError::Permanent(format!("Binance exchangeInfo JSON 非法: {error}")))?;
    let symbols = value
        .get("symbols")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 symbols 列表".into()))?;
    let market = symbols
        .iter()
        .find(|item| item.get("symbol").and_then(Value::as_str) == Some(expected_symbol))
        .ok_or_else(|| {
            QxError::BusinessViolation(format!(
                "Binance exchangeInfo 未返回交易对 {expected_symbol}"
            ))
        })?;
    let status = market
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 market status".into()))?
        .to_owned();
    let base_asset = market
        .get("baseAsset")
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 baseAsset".into()))?
        .to_owned();
    let quote_asset = market
        .get("quoteAsset")
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 quoteAsset".into()))?
        .to_owned();
    let filters = market
        .get("filters")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 filters 列表".into()))?;

    let mut price_tick = None;
    let mut qty_step = None;
    let mut min_qty = None;
    let mut min_notional = 0_i128;
    let mut notional_filters = Vec::new();
    let mut percent_price = None;
    let mut percent_price_by_side = None;
    let mut exact_price_tick = None;
    let mut exact_min_price = None;
    let mut exact_max_price = None;
    let mut exact_qty_step = None;
    let mut exact_min_qty = None;
    let mut exact_max_qty = None;
    let mut exact_market_qty_step = None;
    let mut exact_market_min_qty = None;
    let mut exact_market_max_qty = None;
    let mut exact_min_notional = ExactDecimal::ZERO;
    let mut exact_max_position = None;
    for filter in filters {
        match filter.get("filterType").and_then(Value::as_str) {
            Some("PRICE_FILTER") => {
                let tick = filter
                    .get("tickSize")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance PRICE_FILTER 缺少 tickSize".into())
                    })?;
                let minimum = filter
                    .get("minPrice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance PRICE_FILTER 缺少 minPrice".into())
                    })?;
                let maximum = filter
                    .get("maxPrice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance PRICE_FILTER 缺少 maxPrice".into())
                    })?;
                let exact_tick = parse_exact_decimal(tick, "PRICE_FILTER.tickSize")?;
                let exact_min = parse_exact_decimal(minimum, "PRICE_FILTER.minPrice")?;
                let exact_max = parse_exact_decimal(maximum, "PRICE_FILTER.maxPrice")?;
                price_tick = Some(exact_decimal_projection(exact_tick)?);
                exact_price_tick = Some(exact_tick);
                exact_min_price = Some(exact_min);
                exact_max_price = Some(exact_max);
            }
            Some("LOT_SIZE") => {
                let step = filter
                    .get("stepSize")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Permanent("Binance LOT_SIZE 缺少 stepSize".into()))?;
                let minimum = filter
                    .get("minQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Permanent("Binance LOT_SIZE 缺少 minQty".into()))?;
                let maximum = filter
                    .get("maxQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Permanent("Binance LOT_SIZE 缺少 maxQty".into()))?;
                let exact_step = parse_exact_decimal(step, "LOT_SIZE.stepSize")?;
                let exact_min = parse_exact_decimal(minimum, "LOT_SIZE.minQty")?;
                let exact_max = parse_exact_decimal(maximum, "LOT_SIZE.maxQty")?;
                qty_step = Some(exact_decimal_projection(exact_step)?);
                min_qty = Some(exact_decimal_projection(exact_min)?);
                exact_qty_step = Some(exact_step);
                exact_min_qty = Some(exact_min);
                exact_max_qty = Some(exact_max);
            }
            Some("MARKET_LOT_SIZE") => {
                let step = filter
                    .get("stepSize")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MARKET_LOT_SIZE 缺少 stepSize".into())
                    })?;
                let minimum = filter
                    .get("minQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MARKET_LOT_SIZE 缺少 minQty".into())
                    })?;
                let maximum = filter
                    .get("maxQty")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MARKET_LOT_SIZE 缺少 maxQty".into())
                    })?;
                exact_market_qty_step =
                    Some(parse_exact_decimal(step, "MARKET_LOT_SIZE.stepSize")?);
                exact_market_min_qty =
                    Some(parse_exact_decimal(minimum, "MARKET_LOT_SIZE.minQty")?);
                exact_market_max_qty =
                    Some(parse_exact_decimal(maximum, "MARKET_LOT_SIZE.maxQty")?);
            }
            Some("MIN_NOTIONAL" | "NOTIONAL") => {
                let value = filter
                    .get("minNotional")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance NOTIONAL filter 缺少 minNotional".into())
                    })?;
                let exact_minimum = parse_exact_decimal(value, "NOTIONAL.minNotional")?;
                let raw = exact_decimal_projection(exact_minimum)?;
                if exact_minimum
                    .checked_cmp(exact_min_notional)
                    .is_some_and(|ordering| ordering.is_gt())
                {
                    exact_min_notional = exact_minimum;
                }
                min_notional = min_notional.max(raw);
                let avg_price_mins = json_u32(filter, "avgPriceMins", "NOTIONAL.avgPriceMins")?;
                let (exact_max_notional, apply_min_to_market, apply_max_to_market) =
                    if filter.get("filterType").and_then(Value::as_str) == Some("MIN_NOTIONAL") {
                        let apply = filter
                            .get("applyToMarket")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance MIN_NOTIONAL 缺少 applyToMarket".into())
                            })?;
                        (None, apply, false)
                    } else {
                        let max_value = filter
                            .get("maxNotional")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance NOTIONAL 缺少 maxNotional".into())
                            })?;
                        let max_exact = parse_exact_decimal(max_value, "NOTIONAL.maxNotional")?;
                        let apply_min = filter
                            .get("applyMinToMarket")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance NOTIONAL 缺少 applyMinToMarket".into())
                            })?;
                        let apply_max = filter
                            .get("applyMaxToMarket")
                            .and_then(Value::as_bool)
                            .ok_or_else(|| {
                                QxError::Permanent("Binance NOTIONAL 缺少 applyMaxToMarket".into())
                            })?;
                        (Some(max_exact), apply_min, apply_max)
                    };
                notional_filters.push(BinanceSpotNotionalFilter {
                    apply_min_to_market,
                    apply_max_to_market,
                    avg_price_mins,
                    exact_min_notional: exact_minimum.is_positive().then_some(exact_minimum),
                    exact_max_notional,
                });
            }
            Some("MAX_POSITION") => {
                if exact_max_position.is_some() {
                    return Err(QxError::Permanent(
                        "Binance exchangeInfo 重复包含 MAX_POSITION".into(),
                    ));
                }
                let max_position = filter
                    .get("maxPosition")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        QxError::Permanent("Binance MAX_POSITION 缺少 maxPosition".into())
                    })?;
                let max_position = parse_exact_decimal(max_position, "MAX_POSITION.maxPosition")?;
                if !max_position.is_positive() {
                    return Err(QxError::Permanent(
                        "Binance MAX_POSITION.maxPosition 必须为正数".into(),
                    ));
                }
                exact_max_position = Some(max_position);
            }
            Some("PERCENT_PRICE") => {
                if percent_price.is_some() {
                    return Err(QxError::Permanent(
                        "Binance exchangeInfo 重复包含 PERCENT_PRICE".into(),
                    ));
                }
                percent_price = Some(BinanceSpotPercentPriceFilter {
                    multiplier_down: parse_filter_decimal(
                        filter,
                        "multiplierDown",
                        "PERCENT_PRICE.multiplierDown",
                    )?,
                    multiplier_up: parse_filter_decimal(
                        filter,
                        "multiplierUp",
                        "PERCENT_PRICE.multiplierUp",
                    )?,
                    avg_price_mins: json_u32(filter, "avgPriceMins", "PERCENT_PRICE.avgPriceMins")?,
                });
            }
            Some("PERCENT_PRICE_BY_SIDE") => {
                if percent_price_by_side.is_some() {
                    return Err(QxError::Permanent(
                        "Binance exchangeInfo 重复包含 PERCENT_PRICE_BY_SIDE".into(),
                    ));
                }
                percent_price_by_side = Some(BinanceSpotPercentPriceBySideFilter {
                    bid_multiplier_down: parse_filter_decimal(
                        filter,
                        "bidMultiplierDown",
                        "PERCENT_PRICE_BY_SIDE.bidMultiplierDown",
                    )?,
                    bid_multiplier_up: parse_filter_decimal(
                        filter,
                        "bidMultiplierUp",
                        "PERCENT_PRICE_BY_SIDE.bidMultiplierUp",
                    )?,
                    ask_multiplier_down: parse_filter_decimal(
                        filter,
                        "askMultiplierDown",
                        "PERCENT_PRICE_BY_SIDE.askMultiplierDown",
                    )?,
                    ask_multiplier_up: parse_filter_decimal(
                        filter,
                        "askMultiplierUp",
                        "PERCENT_PRICE_BY_SIDE.askMultiplierUp",
                    )?,
                    avg_price_mins: json_u32(
                        filter,
                        "avgPriceMins",
                        "PERCENT_PRICE_BY_SIDE.avgPriceMins",
                    )?,
                });
            }
            _ => {}
        }
    }
    let price_tick = price_tick
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少有效 PRICE_FILTER".into()))?;
    let qty_step = qty_step.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.stepSize".into())
    })?;
    let min_qty = min_qty.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.minQty".into())
    })?;
    let exact_price_tick = exact_price_tick
        .filter(|value| value.is_positive())
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少有效 PRICE_FILTER".into()))?;
    let exact_min_price = exact_min_price.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少 PRICE_FILTER.minPrice".into())
    })?;
    let exact_max_price = exact_max_price.ok_or_else(|| {
        QxError::Permanent("Binance exchangeInfo 缺少 PRICE_FILTER.maxPrice".into())
    })?;
    let exact_qty_step = exact_qty_step
        .filter(|value| value.is_positive())
        .ok_or_else(|| {
            QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.stepSize".into())
        })?;
    let exact_min_qty = exact_min_qty
        .filter(|value| value.is_positive())
        .ok_or_else(|| {
            QxError::Permanent("Binance exchangeInfo 缺少有效 LOT_SIZE.minQty".into())
        })?;
    let exact_max_qty = exact_max_qty
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 LOT_SIZE.maxQty".into()))?;
    Ok(BinanceSpotMarketRules {
        fetched_at_ms,
        status,
        base_asset,
        quote_asset,
        price_tick,
        qty_step,
        min_qty,
        min_notional,
        notional_filters,
        percent_price,
        percent_price_by_side,
        exact_price_tick,
        exact_min_price,
        exact_max_price,
        exact_qty_step,
        exact_min_qty,
        exact_max_qty,
        exact_market_qty_step,
        exact_market_min_qty,
        exact_market_max_qty,
        exact_min_notional,
        exact_max_position,
    })
}

pub(super) fn parse_exchange_base_assets(body: &str) -> QxResult<BTreeMap<String, String>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| QxError::Permanent(format!("Binance exchangeInfo JSON 非法: {error}")))?;
    let symbols = value
        .get("symbols")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 缺少 symbols 列表".into()))?;
    let mut base_assets = BTreeMap::new();
    for market in symbols {
        let symbol = market
            .get("symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .ok_or_else(|| QxError::Permanent("Binance exchangeInfo 包含非法 symbol".into()))?
            .to_ascii_uppercase();
        let base_asset = market
            .get("baseAsset")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphanumeric()))
            .ok_or_else(|| {
                QxError::Permanent(format!("Binance exchangeInfo {symbol} 缺少有效 baseAsset"))
            })?
            .to_ascii_uppercase();
        if base_assets.insert(symbol.clone(), base_asset).is_some() {
            return Err(QxError::Permanent(format!(
                "Binance exchangeInfo 重复包含交易对 {symbol}"
            )));
        }
    }
    if base_assets.is_empty() {
        return Err(QxError::Permanent(
            "Binance exchangeInfo 未返回任何交易对".into(),
        ));
    }
    Ok(base_assets)
}

pub(super) fn json_u32(value: &Value, key: &str, field: &str) -> QxResult<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 缺少有效非负整数")))
}

pub(super) fn parse_filter_decimal(
    value: &Value,
    key: &str,
    field: &str,
) -> QxResult<ExactDecimal> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 缺少十进制字符串")))?;
    let decimal = ExactDecimal::parse(text)
        .map_err(|error| QxError::Permanent(format!("Binance {field} 非法: {error}")))?;
    if !decimal.is_positive() {
        return Err(QxError::Permanent(format!("Binance {field} 必须为正数")));
    }
    Ok(decimal)
}

pub(super) fn validate_binance_exact_qty_filter(
    qty: ExactDecimal,
    step: ExactDecimal,
    minimum: ExactDecimal,
    maximum: ExactDecimal,
    filter: &str,
) -> QxResult<()> {
    if !qty.is_positive() || !step.is_positive() {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量或 {filter}.stepSize 非法"
        )));
    }
    if minimum.is_positive()
        && qty
            .checked_cmp(minimum)
            .is_none_or(|ordering| ordering.is_lt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量低于 {filter}.minQty"
        )));
    }
    if qty.is_multiple_of(step) != Some(true) {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量不符合 {filter}.stepSize"
        )));
    }
    if maximum.is_positive()
        && qty
            .checked_cmp(maximum)
            .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 数量超过 {filter}.maxQty"
        )));
    }
    Ok(())
}

pub(super) fn validate_binance_exact_price_filter(
    price: ExactDecimal,
    tick: ExactDecimal,
    minimum: ExactDecimal,
    maximum: ExactDecimal,
) -> QxResult<()> {
    if !price.is_positive() || !tick.is_positive() {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价或 PRICE_FILTER.tickSize 非法".into(),
        ));
    }
    if minimum.is_positive()
        && price
            .checked_cmp(minimum)
            .is_none_or(|ordering| ordering.is_lt())
    {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价低于 PRICE_FILTER.minPrice".into(),
        ));
    }
    if maximum.is_positive()
        && price
            .checked_cmp(maximum)
            .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价超过 PRICE_FILTER.maxPrice".into(),
        ));
    }
    if price.is_multiple_of(tick) != Some(true) {
        return Err(QxError::BusinessViolation(
            "Binance Spot 限价不符合 PRICE_FILTER.tickSize".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_binance_exact_notional_filter(
    notional: ExactDecimal,
    min_notional: Option<ExactDecimal>,
    max_notional: Option<ExactDecimal>,
    apply_minimum: bool,
    apply_maximum: bool,
) -> QxResult<()> {
    if !notional.is_positive() {
        return Err(QxError::BusinessViolation(
            "Binance Spot 名义金额必须为正数".into(),
        ));
    }
    if apply_minimum {
        if let Some(minimum) = min_notional.filter(|value| value.is_positive()) {
            if notional
                .checked_cmp(minimum)
                .is_none_or(|ordering| ordering.is_lt())
            {
                return Err(QxError::BusinessViolation(
                    "Binance Spot 订单名义金额低于 MIN_NOTIONAL/NOTIONAL.minNotional".into(),
                ));
            }
        }
    }
    if apply_maximum {
        if let Some(maximum) = max_notional.filter(|value| value.is_positive()) {
            if notional
                .checked_cmp(maximum)
                .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::BusinessViolation(
                    "Binance Spot 订单名义金额超过 NOTIONAL.maxNotional".into(),
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_binance_percent_bounds(
    price: ExactDecimal,
    reference: ExactDecimal,
    multiplier_down: ExactDecimal,
    multiplier_up: ExactDecimal,
    filter: &str,
) -> QxResult<()> {
    if multiplier_down
        .checked_cmp(multiplier_up)
        .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::Permanent(format!(
            "Binance {filter} multiplier 区间非法"
        )));
    }
    let minimum = reference
        .checked_mul(multiplier_down)
        .ok_or_else(|| QxError::BusinessViolation(format!("Binance {filter} 下界计算溢出")))?;
    let maximum = reference
        .checked_mul(multiplier_up)
        .ok_or_else(|| QxError::BusinessViolation(format!("Binance {filter} 上界计算溢出")))?;
    if price
        .checked_cmp(minimum)
        .is_none_or(|ordering| ordering.is_lt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 限价低于 {filter} 动态价格下界"
        )));
    }
    if price
        .checked_cmp(maximum)
        .is_none_or(|ordering| ordering.is_gt())
    {
        return Err(QxError::BusinessViolation(format!(
            "Binance Spot 限价超过 {filter} 动态价格上界"
        )));
    }
    Ok(())
}
