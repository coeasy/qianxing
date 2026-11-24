//! Binance Spot 具体连接器。
//!
//! 该模块只负责 Binance 协议边界（参数编码/签名、订单 JSON、快照及用户事件映射）；订单事实通过 `VenueEvent` 返回，不直接修改 OMS、Ledger 或 Kernel。

use super::order_lifecycle::OrderLifecycle;
use super::venue_cache::evict_stale_terminal_orders;
use super::{HttpRequest, HttpResponse, HttpTransport, TlsWebSocketUserStream, WebSocketPoll};
use qx_core::{
    DecimalRounding, ExactDecimal, ExactFillAmounts, ExactFillFee, Fill, FillFee, InstrumentId,
    Money, Order, OrderGroupExecution, OrderGroupKind, OrderGroupLegRole, OrderGroupState,
    OrderStatus, Price, Quantity, QxError, QxResult, Side, TradingInstrumentSpec, TradingProduct,
    SCALE,
};
use qx_guanxing::QuoteTick;
use qx_zhenlu::{
    ConnectorState, RateLimiter, Venue, VenueEvent, VenueOrderGroupSubmission, VenueOrderSnapshot,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_HOST: &str = "api.binance.com";
const DEFAULT_PORT: u16 = 443;
const DEFAULT_WS_PATH: &str = "/ws-api/v3";
const TESTNET_HOST: &str = "testnet.binance.vision";

mod auth;
pub use auth::{BinanceSpotAuth, BinanceSpotCredentials};

mod user_stream;
pub use user_stream::{
    run_binance_user_stream, run_binance_user_stream_observing,
    run_binance_user_stream_with_config_loader_observing, BinanceSpotUserStream, BinanceStreamPoll,
    BinanceStreamRetryPolicy, BinanceStreamRunReport, BinanceUserStreamRunConfig,
    BinanceUserStreamSession,
};

mod market_data;
use market_data::{
    parse_exchange_base_assets, parse_spot_market_rules, validate_binance_exact_notional_filter,
    validate_binance_exact_price_filter, validate_binance_exact_qty_filter,
    validate_binance_instrument, validate_binance_percent_bounds, BinanceSpotMarketRules,
};
pub use market_data::{BinanceQuotePoll, BinanceSpotMarketData, BinanceSpotMarketStream};
mod orders;
#[cfg(test)]
use orders::binance_group_client_order_id;
use orders::{
    binance_oco_order_list_cancel_parameters, binance_oco_order_list_parameters, client_order_id,
    parse_client_order_id, BinanceOrderListCancelContext, BinanceOrderListLookupWire,
};
mod account_snapshot;
pub use account_snapshot::BinanceBalance;
mod reconcile;

fn form_encode(parameters: &BTreeMap<String, String>) -> String {
    form_encode_pairs(
        &parameters
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>(),
    )
}

fn form_encode_pairs(parameters: &[(String, String)]) -> String {
    parameters
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceFillWire {
    price: String,
    qty: String,
    commission: String,
    #[serde(rename = "commissionAsset", default)]
    commission_asset: String,
    #[serde(rename = "tradeId", default)]
    trade_id: u64,
    #[serde(rename = "transactTime", default)]
    transact_time: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceSnapshotWire {
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    status: String,
    #[serde(rename = "executedQty")]
    executed_qty: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceAccountTradeWire {
    symbol: String,
    #[serde(rename = "id")]
    trade_id: u64,
    #[serde(rename = "orderId")]
    order_id: u64,
    price: String,
    qty: String,
    commission: String,
    #[serde(rename = "commissionAsset", default)]
    commission_asset: String,
    #[serde(rename = "time")]
    transact_time: u64,
    #[serde(rename = "isBuyer")]
    is_buyer: bool,
}

const BINANCE_MARKET_RULES_MAX_AGE_MS: u64 = 60_000;
const BINANCE_REFERENCE_PRICE_MAX_AGE_MS: u64 = 1_000;

/// Binance Spot REST + `executionReport` 用户事件适配器。
type BinanceSeenFillKey = (u64, u64, i128, u8, i128, u8, i128, u8);

pub struct BinanceSpotVenue {
    id: String,
    auth: BinanceSpotAuth,
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    connected: bool,
    state: ConnectorState,
    /// 本地订单状态、远端订单号映射与成交去重键共用同一封顶策略：终态订单在超过
    /// `MAX_CACHED_ORDERS + ORDER_EVICT_HYSTERESIS` 后按 client_id 升序退场，派生索引同步级联。
    /// 退场即升级对账：被退订单再收到回报会落到「成交对应订单不存在」→ ReconcileRequired。
    orders: BTreeMap<u64, Order>,
    venue_order_ids: BTreeMap<u64, String>,
    recorded_trade_ids: BTreeMap<u64, BTreeSet<String>>,
    seen_fill_keys: BTreeSet<BinanceSeenFillKey>,
    rate_limiter: RateLimiter,
    market_rules: BTreeMap<String, BinanceSpotMarketRules>,
    /// Cached symbol-to-base-asset map used for Binance's account-wide MAX_POSITION filter.
    exchange_base_assets: Option<(u64, BTreeMap<String, String>)>,
    reference_prices: BTreeMap<(String, u32), (u64, ExactDecimal)>,
    market_data: BinanceSpotMarketData,
    balances: BTreeMap<String, BinanceBalance>,
    reconcile_symbols: BTreeSet<String>,
    pending_order_list_cancel: Option<BinanceOrderListCancelContext>,
    last_event_ts: u64,
    reconnects: u64,
}

impl OrderLifecycle for BinanceSpotVenue {}

impl BinanceSpotVenue {
    pub fn new(
        id: impl Into<String>,
        auth: BinanceSpotAuth,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self::with_endpoint(id, auth, transport, DEFAULT_HOST, DEFAULT_PORT)
    }

    pub fn testnet(
        id: impl Into<String>,
        auth: BinanceSpotAuth,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self::with_endpoint(id, auth, transport, TESTNET_HOST, DEFAULT_PORT)
    }

    pub fn with_endpoint(
        id: impl Into<String>,
        auth: BinanceSpotAuth,
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        let host = host.into();
        Self {
            id: id.into(),
            auth,
            market_data: BinanceSpotMarketData::with_endpoint(
                Arc::clone(&transport),
                host.clone(),
                port,
            ),
            balances: BTreeMap::new(),
            reconcile_symbols: BTreeSet::new(),
            pending_order_list_cancel: None,
            transport,
            host,
            port,
            connected: true,
            state: ConnectorState::Live,
            orders: BTreeMap::new(),
            venue_order_ids: BTreeMap::new(),
            recorded_trade_ids: BTreeMap::new(),
            seen_fill_keys: BTreeSet::new(),
            rate_limiter: RateLimiter::new(6_000, 6_000, 0),
            market_rules: BTreeMap::new(),
            exchange_base_assets: None,
            reference_prices: BTreeMap::new(),
            last_event_ts: 0,
            reconnects: 0,
        }
    }

    /// 设置重连对账要覆盖的 Spot symbol 集合。Binance `openOrders` 只返回当前未完成订单，
    /// 恢复不了中断期间已成交/撤销/过期的本地订单；对账 worker 提供自己的标的，连接器改用
    /// `allOrders` 拉全量历史，未配置时退化为 EventLog 的 symbol，无本地订单才用 `openOrders`。
    pub fn set_reconcile_symbols<I, S>(&mut self, symbols: I) -> QxResult<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = BTreeSet::new();
        for symbol in symbols {
            let symbol = symbol.as_ref().trim().to_ascii_uppercase();
            if symbol.is_empty() || !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return Err(QxError::BusinessViolation(format!(
                    "Binance 对账 symbol 非法: {symbol}"
                )));
            }
            normalized.insert(symbol);
        }
        self.reconcile_symbols = normalized;
        Ok(())
    }

    /// Route the next cancellation of `active_client_order_id` through Binance's native
    /// order-list endpoint. The context is one-shot and tied to the durable OCO identity so
    /// it cannot silently broaden a single-order cancellation into a list cancellation.
    pub fn arm_order_group_cancel(
        &mut self,
        group: &OrderGroupState,
        active_client_order_id: u64,
    ) -> QxResult<()> {
        if self.pending_order_list_cancel.is_some() {
            return Err(QxError::Invariant(
                "Binance OCO 列表撤销上下文尚未消费".into(),
            ));
        }
        let (order_list_id, list_client_order_id, symbol, parameters) =
            binance_oco_order_list_cancel_parameters(group)?;
        let child_client_order_ids = group
            .legs
            .iter()
            .map(|leg| client_order_id(leg.order.client_id))
            .collect::<BTreeSet<_>>();
        if group.intent.kind != OrderGroupKind::Oco
            || group.intent.execution != OrderGroupExecution::NativeRequired
            || group.legs.len() != 2
            || child_client_order_ids.len() != 2
            || !parameters.contains_key("orderListId")
            || !parameters.contains_key("listClientOrderId")
            || !group
                .legs
                .iter()
                .any(|leg| leg.order.client_id == active_client_order_id)
            || group.legs.iter().any(|leg| {
                !leg.order.instrument.symbol().eq_ignore_ascii_case(&symbol)
                    || !leg.order.instrument.venue.is_binance()
                    || self.orders.get(&leg.order.client_id).is_none_or(|order| {
                        order.account_id != leg.order.account_id
                            || order.instrument != leg.order.instrument
                    })
            })
        {
            return Err(QxError::ReconcileRequired(
                "Binance OCO 撤销身份与已恢复子单不一致".into(),
            ));
        }
        self.pending_order_list_cancel = Some(BinanceOrderListCancelContext {
            order_list_id,
            list_client_order_id,
            symbol,
            parameters,
            active_client_order_id,
            child_client_order_ids,
        });
        Ok(())
    }

    /// 从可恢复的 Kernel/EventLog 重新装载本地订单跟踪状态。
    ///
    /// Binance 用户流本身只携带远端回报，不会携带策略、账户和本地订单
    /// 全量形状；重启后必须先恢复这些订单，再接受 `executionReport`，否则
    /// 未知 `clientOrderId` 会被错误地当作可忽略事件。
    pub fn restore_orders<I>(&mut self, orders: I) -> QxResult<()>
    where
        I: IntoIterator<Item = Order>,
    {
        for order in orders {
            order.validate().map_err(QxError::BusinessViolation)?;
            if let Some(existing) = self.orders.get(&order.client_id) {
                if existing != &order {
                    return Err(QxError::ReconcileRequired(format!(
                        "恢复订单 {} 与连接器本地状态冲突",
                        order.client_id
                    )));
                }
                continue;
            }
            self.orders.insert(order.client_id, order);
        }
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(())
    }

    /// Restore EventLog trade identities so cancel-time race recovery can emit only fills
    /// that are missing from the durable ledger.
    pub fn restore_trade_ids<I>(&mut self, trade_ids: I) -> QxResult<()>
    where
        I: IntoIterator<Item = (u64, BTreeSet<String>)>,
    {
        for (client_order_id, ids) in trade_ids {
            if !self.orders.contains_key(&client_order_id) {
                return Err(QxError::ReconcileRequired(format!(
                    "恢复 Binance 成交 ID 时订单不存在 client_order_id={client_order_id}"
                )));
            }
            if ids.iter().any(|id| id.trim().is_empty()) {
                return Err(QxError::ReconcileRequired(format!(
                    "恢复 Binance 成交 ID 含空值 client_order_id={client_order_id}"
                )));
            }
            self.recorded_trade_ids
                .entry(client_order_id)
                .or_default()
                .extend(ids);
        }
        Ok(())
    }

    /// 用共享 EventLog 的最新订单状态同步已存在的连接器缓存。
    ///
    /// `restore_orders` 用于启动时的严格一致性检查；多进程 UserStream/Reconciler
    /// 在运行中看到其他 worker 追加的 Accepted/Fill 后，必须显式调用本方法，
    /// 不能把本地缓存的旧状态误判成恢复冲突。
    pub fn refresh_orders<I>(&mut self, orders: I) -> QxResult<()>
    where
        I: IntoIterator<Item = Order>,
    {
        for order in orders {
            order.validate().map_err(QxError::BusinessViolation)?;
            self.orders.insert(order.client_id, order);
        }
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.state = ConnectorState::ReconcileRequired;
    }

    pub fn reconnect(&mut self) {
        self.connected = true;
        self.state = ConnectorState::Snapshotting;
        self.reconnects += 1;
    }

    pub fn fetch_book_ticker(
        &mut self,
        instrument: &InstrumentId,
        receive_ts: u64,
    ) -> QxResult<QuoteTick> {
        self.market_data.fetch_book_ticker(instrument, receive_ts)
    }

    /// Latest source event timestamp observed from the private stream or its reducer fallback.
    pub fn last_event_timestamp_ms(&self) -> Option<u64> {
        (self.last_event_ts > 0).then_some(self.last_event_ts)
    }

    /// 解析并应用 Binance Spot `executionReport`/listenKeyExpired 用户事件。
    /// 非订单类账户事件返回空列表，由上层账户快照流处理。
    pub fn ingest_user_event(&mut self, payload: &str) -> QxResult<Vec<VenueEvent>> {
        let value: Value = serde_json::from_str(payload)
            .map_err(|error| QxError::Permanent(format!("Binance 用户事件 JSON 非法: {error}")))?;
        let event_type = value.get("e").and_then(Value::as_str).unwrap_or_default();
        if event_type == "listenKeyExpired" {
            self.disconnect();
            return Err(QxError::ReconcileRequired(
                "Binance listenKey 已过期，必须重新建立用户流并对账".into(),
            ));
        }
        if event_type != "executionReport" {
            if event_type == "outboundAccountPosition" {
                self.ingest_account_position(&value)?;
            }
            return Ok(Vec::new());
        }
        let client_order_id = value
            .get("c")
            .and_then(Value::as_str)
            .and_then(parse_client_order_id)
            .ok_or_else(|| {
                QxError::ReconcileRequired("Binance 回报缺少可映射 clientOrderId".into())
            })?;
        let venue_order_id = value
            .get("i")
            .and_then(Value::as_u64)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 回报缺少 orderId".into()))?
            .to_string();
        let event_ts = value
            .get("E")
            .and_then(Value::as_u64)
            .or_else(|| value.get("T").and_then(Value::as_u64))
            .unwrap_or(self.last_event_ts);
        self.last_event_ts = self.last_event_ts.max(event_ts);
        self.bind_remote_order(client_order_id, venue_order_id.clone())?;
        let mut events = Vec::new();
        if value.get("x").and_then(Value::as_str) == Some("TRADE") {
            let fill = BinanceFillWire {
                price: json_string(&value, "L")?,
                qty: json_string(&value, "l")?,
                commission: json_string(&value, "n")?,
                commission_asset: value
                    .get("N")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                trade_id: value.get("t").and_then(Value::as_u64).ok_or_else(|| {
                    QxError::ReconcileRequired("Binance 回报缺少 trade_id(t)".into())
                })?,
                transact_time: value.get("T").and_then(Value::as_u64).unwrap_or(event_ts),
            };
            if let Some(event) = self.ingest_fill_wire(client_order_id, &venue_order_id, fill)? {
                events.push(event);
            }
        }
        match value.get("X").and_then(Value::as_str).unwrap_or_default() {
            "NEW" if events.is_empty() => {
                let order = self
                    .orders
                    .get_mut(&client_order_id)
                    .ok_or(QxError::Invariant("accepted order disappeared".into()))?;
                if matches!(
                    order.status,
                    OrderStatus::Submitted | OrderStatus::PendingSubmit
                ) {
                    order
                        .status
                        .transition(OrderStatus::Accepted)
                        .map_err(QxError::Invariant)?;
                    events.push(VenueEvent::Accepted {
                        client_order_id,
                        venue_order_id,
                        ts: event_ts,
                    });
                }
            }
            "CANCELED" => {
                if self.mark_cancelled(client_order_id)? {
                    events.push(VenueEvent::Cancelled {
                        client_order_id,
                        ts: event_ts,
                    });
                }
            }
            "FILLED" => self.mark_remote_filled(client_order_id)?,
            "EXPIRED" | "REJECTED" => {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(format!(
                    "Binance 订单 {} 进入 {}，需要上层处理终态事实",
                    client_order_id,
                    value.get("X").and_then(Value::as_str).unwrap_or_default()
                )));
            }
            _ => {}
        }
        Ok(events)
    }

    fn mark_remote_filled(&mut self, client_order_id: u64) -> QxResult<()> {
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance FILLED 订单本地不存在".into()))?;
        let complete = if let Some(amounts) = order.exact_amounts {
            if amounts.quote_budget.is_some() {
                amounts.filled.is_positive() && amounts.quote_filled.is_positive()
            } else {
                amounts.filled == amounts.quantity
            }
        } else {
            order.filled == order.qty
        };
        if !complete {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 已报告 FILLED，但本地成交事实未覆盖订单数量".into(),
            ));
        }
        if order.status != OrderStatus::Filled {
            order
                .status
                .transition(OrderStatus::Filled)
                .map_err(QxError::Invariant)?;
        }
        Ok(())
    }

    pub fn venue_order_id(&self, client_order_id: u64) -> Option<&str> {
        self.venue_order_ids
            .get(&client_order_id)
            .map(String::as_str)
    }

    /// 封顶退场后级联清派生索引：远端订单号与成交去重键必须跟着退场订单一起消失。
    fn forget_orders(&mut self, evicted: &BTreeSet<u64>) {
        if evicted.is_empty() {
            return;
        }
        self.venue_order_ids
            .retain(|client_id, _| !evicted.contains(client_id));
        self.recorded_trade_ids
            .retain(|client_id, _| !evicted.contains(client_id));
        self.seen_fill_keys.retain(|key| !evicted.contains(&key.0));
    }

    fn bind_remote_order(&mut self, client_order_id: u64, venue_order_id: String) -> QxResult<()> {
        if !self.orders.contains_key(&client_order_id) {
            return Err(QxError::ReconcileRequired(format!(
                "Binance 用户回报对应未知本地订单 {}",
                client_order_id
            )));
        }
        self.venue_order_ids.insert(client_order_id, venue_order_id);
        Ok(())
    }

    fn ingest_fill_wire(
        &mut self,
        client_order_id: u64,
        venue_order_id: &str,
        wire: BinanceFillWire,
    ) -> QxResult<Option<VenueEvent>> {
        let quantity_exact = parse_exact_decimal(&wire.qty, "fill qty")?;
        let price_exact = parse_exact_decimal(&wire.price, "fill price")?;
        let fee_exact = parse_exact_decimal(&wire.commission, "commission")?;
        let fee_currency = (!wire.commission_asset.trim().is_empty())
            .then(|| wire.commission_asset.trim().to_ascii_uppercase());
        if fee_exact.is_negative() {
            return Err(QxError::ReconcileRequired(
                "Binance 成交手续费不能为负数".into(),
            ));
        }
        if !fee_exact.is_zero() && fee_currency.is_none() {
            return Err(QxError::ReconcileRequired(
                "Binance 非零成交手续费缺少 commissionAsset".into(),
            ));
        }
        let is_exact_order = self
            .orders
            .get(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 成交对应订单不存在".into()))?
            .exact_amounts
            .is_some();
        let (qty, price, fee) = if is_exact_order {
            (
                Quantity::from_raw(exact_decimal_projection(quantity_exact)?),
                Price::from_raw(exact_decimal_projection(price_exact)?),
                Money::from_raw(exact_decimal_projection(fee_exact)?),
            )
        } else {
            (
                parse_quantity(&wire.qty, "fill qty")?,
                parse_price(&wire.price, "fill price")?,
                parse_money(&wire.commission, "commission")?,
            )
        };
        let key = (
            client_order_id,
            wire.trade_id,
            quantity_exact.coefficient(),
            quantity_exact.scale(),
            price_exact.coefficient(),
            price_exact.scale(),
            fee_exact.coefficient(),
            fee_exact.scale(),
        );
        if Self::fill_seen(&self.seen_fill_keys, &key) {
            return Ok(None);
        }
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 成交对应订单不存在".into()))?;
        if order.status.is_terminal() {
            // 本地订单已终态（撤单/成完/拒绝），远端却又推来成交：这是真实的状态分歧，
            // 只能按未知结果交给对账，绝不能伪造一条成交事实把终态订单改写。
            return Err(QxError::ReconcileRequired(format!(
                "Binance 订单 {} 本地已终态 {:?} 但远端仍有成交回报",
                client_order_id, order.status
            )));
        }
        if !quantity_exact.is_positive() || !price_exact.is_positive() {
            return Err(QxError::ReconcileRequired(
                "Binance 成交数量和价格必须为正数".into(),
            ));
        }
        let quote_cost = quantity_exact
            .checked_mul_at_scale(
                price_exact,
                ExactDecimal::MAX_SCALE,
                if order.side == Side::Buy {
                    DecimalRounding::AwayFromZero
                } else {
                    DecimalRounding::TowardZero
                },
            )
            .ok_or_else(|| QxError::ReconcileRequired("Binance 精确成交计价金额溢出".into()))?;
        if let Some(mut amounts) = order.exact_amounts {
            if let Some(budget) = amounts.quote_budget {
                let cumulative_quote =
                    amounts
                        .quote_filled
                        .checked_add(quote_cost)
                        .ok_or_else(|| {
                            QxError::ReconcileRequired("Binance quoteOrderQty 累计溢出".into())
                        })?;
                if cumulative_quote
                    .checked_cmp(budget)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::ReconcileRequired(
                        "Binance quoteOrderQty 成交花费超过委托预算".into(),
                    ));
                }
                amounts.quote_filled = cumulative_quote;
            } else {
                let cumulative = amounts.filled.checked_add(quantity_exact).ok_or_else(|| {
                    QxError::ReconcileRequired("Binance 精确累计成交量溢出".into())
                })?;
                if cumulative
                    .checked_cmp(amounts.quantity)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::ReconcileRequired(
                        "Binance 成交数量超过本地订单剩余量".into(),
                    ));
                }
                amounts.filled = cumulative;
            }
            order.exact_amounts = Some(amounts);
        } else if qty.raw() <= 0 || qty.raw() > order.remaining().raw() {
            // 回报越界时不占用去重键：否则一次虚假回报会让该笔成交在对账后永远无法
            // 被重新接受（去重键只在回报真正落到本地状态后才登记）。
            return Err(QxError::ReconcileRequired(
                "Binance 成交数量超过本地订单剩余量".into(),
            ));
        }
        if matches!(order.status, OrderStatus::Submitted | OrderStatus::Accepted) {
            order
                .status
                .transition(OrderStatus::Working)
                .map_err(QxError::Invariant)?;
        }
        let next_status = if let Some(amounts) = order.exact_amounts {
            if amounts.quote_budget.is_some() {
                OrderStatus::PartiallyFilled
            } else if amounts.filled == amounts.quantity {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            }
        } else {
            order.filled = Quantity::from_raw(order.filled.raw() + qty.raw());
            if order.filled == order.qty {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            }
        };
        if !(order.status == OrderStatus::CancelPending
            && next_status == OrderStatus::PartiallyFilled)
        {
            order
                .status
                .transition(next_status)
                .map_err(QxError::Invariant)?;
        }
        let mut fill = Fill {
            order_id: client_order_id,
            qty: if is_exact_order { Quantity::ZERO } else { qty },
            price: if is_exact_order { Price::ZERO } else { price },
            fee: if is_exact_order { Money::ZERO } else { fee },
            ts: wire.transact_time,
            account_id: order.account_id.clone(),
            fee_currency: fee_currency.clone(),
            fees: (!is_exact_order)
                .then(|| FillFee {
                    amount: fee,
                    currency: fee_currency.clone(),
                    source_trade_ids: vec![wire.trade_id.to_string()],
                })
                .into_iter()
                .collect(),
            ..Fill::default()
        };
        if is_exact_order {
            fill.exact_amounts = Some(ExactFillAmounts {
                quantity: quantity_exact,
                price: price_exact,
                quote_cost: quote_cost.is_positive().then_some(quote_cost),
                quote_budget_complete: false,
            });
            fill.exact_fees.push(ExactFillFee {
                amount: fee_exact,
                currency: fee_currency.clone(),
                source_trade_ids: vec![wire.trade_id.to_string()],
            });
        }
        order.trace_fill(&mut fill, Some(&self.id), Some(venue_order_id));
        self.last_event_ts = self.last_event_ts.max(fill.ts);
        Self::remember_fill(&mut self.seen_fill_keys, key);
        Ok(Some(VenueEvent::Fill(Box::new(fill))))
    }

    /// 应用远端撤单回报，返回本地是否真的推进到 `Cancelled`：重复回报返回 `false`（幂等）；
    /// 本地已因成交/拒绝进终态时返回错误（真实分歧，只能交对账，绝不能把终态改写）。
    fn mark_cancelled(&mut self, client_order_id: u64) -> QxResult<bool> {
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 取消回报对应订单不存在".into()))?;
        if order.status == OrderStatus::Cancelled {
            return Ok(false);
        }
        if order.status.is_terminal() {
            return Err(QxError::ReconcileRequired(format!(
                "Binance 订单 {} 本地已终态 {:?} 但远端回报撤单",
                client_order_id, order.status
            )));
        }
        if !matches!(order.status, OrderStatus::CancelPending) {
            order
                .status
                .transition(OrderStatus::CancelPending)
                .map_err(QxError::Invariant)?;
        }
        order
            .status
            .transition(OrderStatus::Cancelled)
            .map_err(QxError::Invariant)?;
        Ok(true)
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        parameters: BTreeMap<String, String>,
        now_ms: u64,
    ) -> QxResult<HttpResponse> {
        self.request_with_weight(method, path, parameters, now_ms, 1)
    }

    fn request_with_weight(
        &mut self,
        method: &str,
        path: &str,
        parameters: BTreeMap<String, String>,
        now_ms: u64,
        weight: u64,
    ) -> QxResult<HttpResponse> {
        if !self.connected {
            return Err(QxError::Ambiguous("Binance Spot 连接中断".into()));
        }
        if !self
            .rate_limiter
            .try_acquire(weight, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted("Binance REST 请求限频".into()));
        }
        let request = self
            .auth
            .signed_request(method, &self.host, self.port, path, parameters);
        match self.transport.send(request) {
            Ok(response) if (200..300).contains(&response.status) => Ok(response),
            Ok(response) if response.status == 429 || response.status == 418 => {
                Err(QxError::ResourceExhausted(format!(
                    "Binance HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )))
            }
            Ok(response) if response.status >= 500 => {
                let error = QxError::Ambiguous(format!(
                    "Binance HTTP {}，订单结果未知: {}",
                    response.status,
                    error_message(&response.body)
                ));
                if Self::reconciliation_trigger(&error) {
                    self.state = ConnectorState::ReconcileRequired;
                }
                Err(error)
            }
            Ok(response) => Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Err(error) => {
                let error = QxError::Ambiguous(error);
                if Self::reconciliation_trigger(&error) {
                    self.state = ConnectorState::ReconcileRequired;
                }
                Err(error)
            }
        }
    }

    /// Signed read-only REST call whose failures must not put the order connector into an
    /// ambiguous-submit state. It is used for balance/open-order observations before a write.
    fn request_readonly_with_weight(
        &mut self,
        path: &str,
        now_ms: u64,
        weight: u64,
    ) -> QxResult<HttpResponse> {
        self.request_readonly_params_with_weight(path, BTreeMap::new(), now_ms, weight)
    }

    fn request_readonly_params_with_weight(
        &mut self,
        path: &str,
        parameters: BTreeMap<String, String>,
        now_ms: u64,
        weight: u64,
    ) -> QxResult<HttpResponse> {
        if !self.connected {
            return Err(QxError::VenueState(
                "Binance Spot 连接中断，无法读取账户状态".into(),
            ));
        }
        if !self
            .rate_limiter
            .try_acquire(weight, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted("Binance REST 请求限频".into()));
        }
        let request = self
            .auth
            .signed_request("GET", &self.host, self.port, path, parameters);
        match self.transport.send(request) {
            Ok(response) if (200..300).contains(&response.status) => Ok(response),
            Ok(response) if response.status == 429 || response.status == 418 => {
                Err(QxError::ResourceExhausted(format!(
                    "Binance HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )))
            }
            Ok(response) if response.status >= 500 => Err(QxError::Transient(format!(
                "Binance 只读账户请求 HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Ok(response) => Err(QxError::Permanent(format!(
                "Binance HTTP {}: {}",
                response.status,
                error_message(&response.body)
            ))),
            Err(error) => Err(QxError::Transient(format!(
                "Binance 只读账户请求失败: {error}"
            ))),
        }
    }

    fn current_spot_base_assets(&mut self, now_ms: u64) -> QxResult<BTreeMap<String, String>> {
        if let Some((fetched_at_ms, cached)) = &self.exchange_base_assets {
            if now_ms.saturating_sub(*fetched_at_ms) <= BINANCE_MARKET_RULES_MAX_AGE_MS {
                return Ok(cached.clone());
            }
        }
        if !self
            .rate_limiter
            .try_acquire(20, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance exchangeInfo 公共请求限频".into(),
            ));
        }
        let response = self
            .transport
            .send(HttpRequest {
                method: "GET".into(),
                host: self.host.clone(),
                port: self.port,
                path: "/api/v3/exchangeInfo".into(),
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .map_err(|error| {
                QxError::Transient(format!("Binance exchangeInfo 公共请求失败: {error}"))
            })?;
        if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if response.status >= 500 {
            return Err(QxError::Transient(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        let base_assets = parse_exchange_base_assets(&response.body)?;
        self.exchange_base_assets = Some((now_ms, base_assets.clone()));
        Ok(base_assets)
    }

    fn current_spot_market_rules(&mut self, symbol: &str) -> QxResult<BinanceSpotMarketRules> {
        let symbol = symbol.to_ascii_uppercase();
        if symbol.is_empty() || !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return Err(QxError::BusinessViolation(format!(
                "Binance Spot symbol 非法: {symbol}"
            )));
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(rules) = self.market_rules.get(&symbol) {
            if now_ms.saturating_sub(rules.fetched_at_ms) <= BINANCE_MARKET_RULES_MAX_AGE_MS {
                return Ok(rules.clone());
            }
        }
        if !self
            .rate_limiter
            .try_acquire(20, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance exchangeInfo 公共请求限频".into(),
            ));
        }

        // Symbol is restricted to ASCII alphanumerics above, so it can be inserted directly
        // into this public query without credentials, signatures, or user-controlled separators.
        let request = HttpRequest {
            method: "GET".into(),
            host: self.host.clone(),
            port: self.port,
            path: format!("/api/v3/exchangeInfo?symbol={symbol}"),
            body: String::new(),
            headers: BTreeMap::new(),
        };
        let response = self.transport.send(request).map_err(|error| {
            QxError::Transient(format!("Binance exchangeInfo 公共请求失败: {error}"))
        })?;
        if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if response.status >= 500 {
            return Err(QxError::Transient(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "Binance exchangeInfo HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        }
        let rules = parse_spot_market_rules(&response.body, &symbol, now_ms)?;
        self.market_rules.insert(symbol, rules.clone());
        Ok(rules)
    }

    /// Fetch the exact reference window required by an exchange filter. `/avgPrice` only
    /// certifies the window it returns, so a mismatch fails closed instead of reusing a
    /// superficially recent but semantically different price.
    fn current_spot_reference_exact_price(
        &mut self,
        symbol: &str,
        avg_price_mins: u32,
    ) -> QxResult<ExactDecimal> {
        let symbol = symbol.to_ascii_uppercase();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let key = (symbol.clone(), avg_price_mins);
        if let Some((fetched_at_ms, price)) = self.reference_prices.get(&key) {
            if now_ms.saturating_sub(*fetched_at_ms) <= BINANCE_REFERENCE_PRICE_MAX_AGE_MS {
                return Ok(*price);
            }
        }
        let reference_path = format!("/api/v3/referencePrice?symbol={symbol}");
        if !self
            .rate_limiter
            .try_acquire(2, now_ms.saturating_mul(1_000_000))
        {
            return Err(QxError::ResourceExhausted(
                "Binance Spot referencePrice 请求限频".into(),
            ));
        }
        let response = self
            .transport
            .send(HttpRequest {
                method: "GET".into(),
                host: self.host.clone(),
                port: self.port,
                path: reference_path,
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .map_err(|error| {
                QxError::Transient(format!("Binance Spot referencePrice 请求失败: {error}"))
            })?;
        let mut reference_price = None;
        if (200..300).contains(&response.status) {
            let value: Value = serde_json::from_str(&response.body).map_err(|error| {
                QxError::Transient(format!("Binance Spot referencePrice JSON 非法: {error}"))
            })?;
            let actual_symbol = value
                .get("symbol")
                .and_then(Value::as_str)
                .ok_or_else(|| QxError::Transient("Binance referencePrice 缺少 symbol".into()))?;
            if actual_symbol != symbol {
                return Err(QxError::ReconcileRequired(format!(
                    "Binance referencePrice symbol 不匹配: expected={symbol}, actual={actual_symbol}"
                )));
            }
            if let Some(value) = value.get("referencePrice").filter(|value| !value.is_null()) {
                let price = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_number().map(ToString::to_string))
                    .ok_or_else(|| {
                        QxError::Transient("Binance referencePrice 字段不是十进制值".into())
                    })?;
                let price = parse_exact_decimal(&price, "referencePrice")?;
                if !price.is_positive() {
                    return Err(QxError::Transient(
                        "Binance referencePrice 必须为正数或 null".into(),
                    ));
                }
                reference_price = Some(price);
            }
        } else if response.status == 429 || response.status == 418 {
            return Err(QxError::ResourceExhausted(format!(
                "Binance Spot referencePrice HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        } else if response.status >= 500 {
            return Err(QxError::Transient(format!(
                "Binance Spot referencePrice HTTP {}: {}",
                response.status,
                error_message(&response.body)
            )));
        } else {
            let error_value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
            if error_value.get("code").and_then(Value::as_i64) != Some(-2043) {
                return Err(QxError::Permanent(format!(
                    "Binance Spot referencePrice HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
        }
        let price = if let Some(reference_price) = reference_price {
            reference_price
        } else {
            if !self
                .rate_limiter
                .try_acquire(1, now_ms.saturating_mul(1_000_000))
            {
                return Err(QxError::ResourceExhausted(
                    "Binance Spot 均价回退请求限频".into(),
                ));
            }
            let endpoint = if avg_price_mins == 0 {
                "/api/v3/ticker/price"
            } else {
                "/api/v3/avgPrice"
            };
            let response = self
                .transport
                .send(HttpRequest {
                    method: "GET".into(),
                    host: self.host.clone(),
                    port: self.port,
                    path: format!("{endpoint}?symbol={symbol}"),
                    body: String::new(),
                    headers: BTreeMap::new(),
                })
                .map_err(|error| {
                    QxError::Transient(format!("Binance Spot 均价回退请求失败: {error}"))
                })?;
            if response.status == 429 || response.status == 418 {
                return Err(QxError::ResourceExhausted(format!(
                    "Binance Spot 均价回退 HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
            if response.status >= 500 {
                return Err(QxError::Transient(format!(
                    "Binance Spot 均价回退 HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
            if !(200..300).contains(&response.status) {
                return Err(QxError::Permanent(format!(
                    "Binance Spot 均价回退 HTTP {}: {}",
                    response.status,
                    error_message(&response.body)
                )));
            }
            let value: Value = serde_json::from_str(&response.body).map_err(|error| {
                QxError::Transient(format!("Binance Spot 均价回退 JSON 非法: {error}"))
            })?;
            if avg_price_mins == 0 {
                let actual_symbol = value
                    .get("symbol")
                    .and_then(Value::as_str)
                    .ok_or_else(|| QxError::Transient("Binance ticker/price 缺少 symbol".into()))?;
                if actual_symbol != symbol {
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance ticker/price symbol 不匹配: expected={symbol}, actual={actual_symbol}"
                    )));
                }
            } else {
                let actual_mins = value
                    .get("mins")
                    .and_then(Value::as_u64)
                    .and_then(|mins| u32::try_from(mins).ok())
                    .ok_or_else(|| QxError::Transient("Binance avgPrice 缺少有效 mins".into()))?;
                if actual_mins != avg_price_mins {
                    return Err(QxError::Transient(format!(
                        "Binance avgPrice 窗口为 {actual_mins} 分钟，但交易过滤器要求 {avg_price_mins} 分钟；拒绝使用不匹配的参考价"
                    )));
                }
            }
            let price = json_string(&value, "price")?;
            parse_exact_decimal(&price, "filter average price")?
        };
        if !price.is_positive() {
            return Err(QxError::Transient(
                "Binance Spot 过滤器参考价必须为正数".into(),
            ));
        }
        self.reference_prices.insert(key, (now_ms, price));
        Ok(price)
    }

    fn validate_percent_price_filters(
        &mut self,
        rules: &BinanceSpotMarketRules,
        symbol: &str,
        side: Side,
        price: ExactDecimal,
    ) -> QxResult<()> {
        if let Some(filter) = &rules.percent_price {
            let reference =
                self.current_spot_reference_exact_price(symbol, filter.avg_price_mins)?;
            validate_binance_percent_bounds(
                price,
                reference,
                filter.multiplier_down,
                filter.multiplier_up,
                "PERCENT_PRICE",
            )?;
        }
        if let Some(filter) = &rules.percent_price_by_side {
            let reference =
                self.current_spot_reference_exact_price(symbol, filter.avg_price_mins)?;
            let (down, up) = match side {
                Side::Buy => (filter.bid_multiplier_down, filter.bid_multiplier_up),
                Side::Sell => (filter.ask_multiplier_down, filter.ask_multiplier_up),
            };
            validate_binance_percent_bounds(price, reference, down, up, "PERCENT_PRICE_BY_SIDE")?;
        }
        Ok(())
    }

    fn validate_max_position_filter(
        &mut self,
        rules: &BinanceSpotMarketRules,
        order: &Order,
    ) -> QxResult<()> {
        let Some(maximum) = rules.exact_max_position else {
            return Ok(());
        };
        if order.side != Side::Buy {
            return Ok(());
        }
        if order.quote_budget_decimal().is_some() {
            return Err(QxError::BusinessViolation(
                "Binance MAX_POSITION 存在时无法仅凭 quoteOrderQty 证明买入基础币数量不会超限"
                    .into(),
            ));
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        // Read open orders first and the account balance second. If one of the observed buys
        // fills between the two snapshots, the later balance includes the fill while the earlier
        // open-order snapshot still reserves its remaining quantity, which is conservative.
        let open_orders = self.fetch_open_orders_for_max_position(now_ms)?;
        let base_assets = self.current_spot_base_assets(now_ms)?;
        let balances = self.fetch_account_balances(now_ms)?;
        let base_asset = rules.base_asset.trim().to_ascii_uppercase();
        let balance = balances
            .iter()
            .find(|balance| balance.asset.eq_ignore_ascii_case(&base_asset));
        let mut current_position = balance
            .map(|balance| balance.free_exact.checked_add(balance.locked_exact))
            .unwrap_or(Some(ExactDecimal::ZERO))
            .ok_or_else(|| {
                QxError::ReconcileRequired("Binance MAX_POSITION 余额合计溢出".into())
            })?;
        if current_position.is_negative() {
            return Err(QxError::ReconcileRequired(
                "Binance MAX_POSITION 账户余额为负".into(),
            ));
        }

        let mut seen_orders = BTreeSet::new();
        for open in open_orders {
            let symbol = open.symbol.trim().to_ascii_uppercase();
            if symbol.is_empty() || !symbol.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return Err(QxError::ReconcileRequired(
                    "Binance openOrders 包含非法 symbol，无法校验 MAX_POSITION".into(),
                ));
            }
            if open.order_id == 0 || !seen_orders.insert((symbol.clone(), open.order_id)) {
                return Err(QxError::ReconcileRequired(
                    "Binance openOrders 包含无效或重复 orderId".into(),
                ));
            }
            if !open.side.eq_ignore_ascii_case("BUY") {
                if !open.side.eq_ignore_ascii_case("SELL") {
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance open order {} side 非法",
                        open.order_id
                    )));
                }
                continue;
            }
            let order_base = base_assets.get(&symbol).ok_or_else(|| {
                QxError::ReconcileRequired(format!(
                    "Binance exchangeInfo 缺少 open order {symbol} 的 baseAsset，无法校验 MAX_POSITION"
                ))
            })?;
            if !order_base.eq_ignore_ascii_case(&base_asset) {
                continue;
            }
            let original = parse_exact_decimal(&open.original_quantity, "open order origQty")?;
            let executed = parse_exact_decimal(&open.executed_quantity, "open order executedQty")?;
            if !original.is_positive()
                || executed.is_negative()
                || executed
                    .checked_cmp(original)
                    .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::ReconcileRequired(format!(
                    "Binance open order {} 数量不一致，无法校验 MAX_POSITION",
                    open.order_id
                )));
            }
            let remaining = original.checked_sub(executed).ok_or_else(|| {
                QxError::ReconcileRequired("Binance MAX_POSITION 在途买单数量溢出".into())
            })?;
            current_position = current_position.checked_add(remaining).ok_or_else(|| {
                QxError::ReconcileRequired("Binance MAX_POSITION 在途买单合计溢出".into())
            })?;
        }

        let projected = current_position
            .checked_add(order.quantity_decimal())
            .ok_or_else(|| QxError::BusinessViolation("Binance MAX_POSITION 投影溢出".into()))?;
        if projected
            .checked_cmp(maximum)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(QxError::BusinessViolation(format!(
                "Binance MAX_POSITION 超限: 当前余额与全部交易对未成交买单加本单为 {projected}，上限为 {maximum} {base_asset}"
            )));
        }
        Ok(())
    }
}

impl Venue for BinanceSpotVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &'static qx_zhenlu::venue_capabilities::VenueCapabilities {
        &qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES
    }

    fn validate_order_group(&mut self, group: &OrderGroupState) -> QxResult<()> {
        self.validate_order_group_impl(group)
    }

    fn submit_order_group(
        &mut self,
        group: &OrderGroupState,
        ts: u64,
    ) -> QxResult<VenueOrderGroupSubmission> {
        self.submit_order_group_impl(group, ts)
    }

    fn validate_instrument_spec(&mut self, spec: &TradingInstrumentSpec) -> QxResult<()> {
        spec.validate()?;
        self.execution_readiness()?;
        if spec.product != TradingProduct::Spot
            || spec.contract_size != SCALE
            || spec.linear
            || spec.inverse
            || spec.max_leverage != 1
            || spec.maintenance_margin_bps != 0
            || !spec
                .settlement_currency
                .eq_ignore_ascii_case(&spec.quote_currency)
        {
            return Err(QxError::BusinessViolation(
                "Binance Spot 冻结规格包含不支持的产品、合约或杠杆字段".into(),
            ));
        }
        validate_binance_instrument(&spec.instrument)?;
        if !spec.instrument.venue.is_binance() {
            return Err(QxError::BusinessViolation(format!(
                "Binance Spot 规格 venue 不匹配: {}",
                spec.instrument.venue
            )));
        }
        let symbol = spec.instrument.symbol().to_ascii_uppercase();
        let live = self.current_spot_market_rules(&symbol)?;
        if live.status != "TRADING" {
            return Err(QxError::VenueState(format!(
                "Binance Spot {symbol} 当前状态为 {}",
                live.status
            )));
        }
        let rule_mismatch = if let Some(exact) = &spec.exact_rules {
            exact.contract_size != ExactDecimal::ONE
                || exact.price_tick != live.exact_price_tick
                || exact.quantity_step != live.exact_qty_step
                || exact.minimum_quantity != live.exact_min_qty
                || exact.minimum_notional != live.exact_min_notional
                || spec.price_tick != exact_decimal_projection(exact.price_tick)?
                || spec.qty_step != exact_decimal_projection(exact.quantity_step)?
                || spec.min_qty != exact_decimal_projection(exact.minimum_quantity)?
                || spec.min_notional_raw != exact_decimal_projection(exact.minimum_notional)?
        } else {
            live.price_tick != spec.price_tick
                || live.qty_step != spec.qty_step
                || live.min_qty != spec.min_qty
                || live.min_notional != spec.min_notional_raw
        };
        let mismatch = !live.base_asset.eq_ignore_ascii_case(&spec.base_currency)
            || !live.quote_asset.eq_ignore_ascii_case(&spec.quote_currency)
            || rule_mismatch;
        if mismatch {
            return Err(QxError::BusinessViolation(format!(
                "Binance Spot {symbol} 当前交易规则与冻结规格不一致；拒绝提交订单"
            )));
        }
        Ok(())
    }

    fn validate_order_constraints(
        &mut self,
        spec: &TradingInstrumentSpec,
        order: &Order,
    ) -> QxResult<()> {
        self.validate_instrument_spec(spec)?;
        if order.instrument != spec.instrument {
            return Err(QxError::BusinessViolation(
                "Binance Spot 订单 instrument 与冻结规格不一致".into(),
            ));
        }
        let symbol = spec.instrument.symbol().to_ascii_uppercase();
        let rules = self.current_spot_market_rules(&symbol)?;
        if rules.status != "TRADING" {
            return Err(QxError::VenueState(format!(
                "Binance Spot {symbol} 当前状态为 {}",
                rules.status
            )));
        }
        let quote_budget = order.quote_budget_decimal();
        let quantity = if quote_budget.is_some() {
            if order.side != Side::Buy || order.limit_price_decimal().is_some() {
                return Err(QxError::BusinessViolation(
                    "Binance Spot quoteOrderQty 仅支持市价买单".into(),
                ));
            }
            None
        } else {
            Some(order.quantity_decimal())
        };
        let limit_price = order.limit_price_decimal();
        if let Some(budget) = quote_budget {
            spec.validate_exact_quote_budget(budget)?;
            if budget
                .checked_cmp(rules.exact_min_notional)
                .is_some_and(|ordering| ordering.is_lt())
            {
                return Err(QxError::BusinessViolation(
                    "Binance Spot 订单名义金额低于交易所最小限制".into(),
                ));
            }
        } else if let Some(quantity) = quantity {
            spec.validate_exact_order_precision(quantity, limit_price)?;
            validate_binance_exact_qty_filter(
                quantity,
                rules.exact_qty_step,
                rules.exact_min_qty,
                rules.exact_max_qty,
                "LOT_SIZE",
            )?;
            if limit_price.is_none() {
                if let Some(step) = rules.exact_market_qty_step {
                    validate_binance_exact_qty_filter(
                        quantity,
                        step,
                        rules.exact_market_min_qty.unwrap_or(ExactDecimal::ZERO),
                        rules.exact_market_max_qty.unwrap_or(ExactDecimal::ZERO),
                        "MARKET_LOT_SIZE",
                    )?;
                }
            }
            if let Some(price) = limit_price {
                validate_binance_exact_price_filter(
                    price,
                    rules.exact_price_tick,
                    rules.exact_min_price,
                    rules.exact_max_price,
                )?;
                self.validate_percent_price_filters(&rules, &symbol, order.side, price)?;
            }
        }
        for filter in &rules.notional_filters {
            let notional = if let Some(budget) = quote_budget {
                Some((budget, budget))
            } else if let Some(quantity) = quantity {
                if let Some(price) = limit_price {
                    Some((
                        quantity
                            .checked_mul_at_scale(
                                price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::TowardZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                        quantity
                            .checked_mul_at_scale(
                                price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::AwayFromZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                    ))
                } else if filter.apply_min_to_market || filter.apply_max_to_market {
                    let reference_price =
                        self.current_spot_reference_exact_price(&symbol, filter.avg_price_mins)?;
                    Some((
                        quantity
                            .checked_mul_at_scale(
                                reference_price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::TowardZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                        quantity
                            .checked_mul_at_scale(
                                reference_price,
                                ExactDecimal::MAX_SCALE,
                                DecimalRounding::AwayFromZero,
                            )
                            .ok_or_else(|| {
                                QxError::BusinessViolation("Binance Spot 名义金额计算溢出".into())
                            })?,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((min_check, max_check)) = notional {
                if filter.apply_min_to_market || limit_price.is_some() {
                    validate_binance_exact_notional_filter(
                        min_check,
                        filter.exact_min_notional,
                        None,
                        true,
                        false,
                    )?;
                }
                if filter.apply_max_to_market || limit_price.is_some() {
                    validate_binance_exact_notional_filter(
                        max_check,
                        None,
                        filter.exact_max_notional,
                        false,
                        true,
                    )?;
                }
            }
        }
        self.validate_max_position_filter(&rules, order)?;
        Ok(())
    }

    fn execution_readiness(&self) -> QxResult<()> {
        if !self.connected {
            return Err(QxError::VenueState("Binance Spot 连接中断".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState(format!(
                "Binance Spot 连接器状态为 {:?}，尚未完成恢复对账",
                self.state
            )));
        }
        Ok(())
    }

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.submit_order_impl(order, ts)
    }

    fn cancel(&mut self, local_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.cancel_order_impl(local_id, ts)
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.orders
            .values()
            .map(|order| VenueOrderSnapshot {
                client_order_id: order.client_id,
                status: order.status,
                filled: order.filled,
                filled_exact: order.exact_amounts.map(|amounts| amounts.filled),
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

fn json_string(value: &Value, key: &str) -> QxResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| QxError::ReconcileRequired(format!("Binance 用户事件缺少 {key}")))
}

fn parse_success<T: for<'de> Deserialize<'de>>(
    response: HttpResponse,
    operation: &str,
) -> QxResult<T> {
    serde_json::from_str(&response.body)
        .map_err(|error| QxError::Permanent(format!("Binance {operation} JSON 非法: {error}")))
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("msg").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

fn parse_quantity(value: &str, field: &str) -> QxResult<Quantity> {
    Quantity::from_dec(value)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 定点格式非法")))
}

fn parse_exact_decimal(value: &str, field: &str) -> QxResult<ExactDecimal> {
    ExactDecimal::parse(value)
        .map_err(|error| QxError::Permanent(format!("Binance {field} 精确格式非法: {error}")))
}

fn exact_decimal_projection(value: ExactDecimal) -> QxResult<i128> {
    value
        .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| QxError::Permanent("Binance 十进制值无法投影到兼容 raw 范围".into()))
}

fn parse_exact_quantity_projection(value: &str, field: &str) -> QxResult<(Quantity, ExactDecimal)> {
    let exact = ExactDecimal::parse(value).map_err(|error| {
        QxError::ReconcileRequired(format!("Binance {field} 精确格式非法: {error}"))
    })?;
    if exact.is_negative() {
        return Err(QxError::ReconcileRequired(format!(
            "Binance {field} 成交数量不能为负"
        )));
    }
    let one = ExactDecimal::from_parts(1, 0).map_err(|error| QxError::Invariant(error.into()))?;
    let raw = exact
        .checked_mul_at_scale(one, 9, DecimalRounding::TowardZero)
        .and_then(|projected| projected.to_raw(9).ok())
        .ok_or_else(|| {
            QxError::ReconcileRequired(format!("Binance {field} 无法投影到兼容 raw 范围"))
        })?;
    Ok((Quantity::from_raw(raw), exact))
}

fn parse_price(value: &str, field: &str) -> QxResult<Price> {
    Price::from_dec(value)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 定点格式非法")))
}

fn parse_money(value: &str, field: &str) -> QxResult<Money> {
    Money::from_dec(value)
        .ok_or_else(|| QxError::Permanent(format!("Binance {field} 定点格式非法")))
}

fn map_status(value: &str) -> QxResult<OrderStatus> {
    match value {
        "NEW" => Ok(OrderStatus::Working),
        "PARTIALLY_FILLED" => Ok(OrderStatus::PartiallyFilled),
        "FILLED" => Ok(OrderStatus::Filled),
        "CANCELED" => Ok(OrderStatus::Cancelled),
        "EXPIRED" => Ok(OrderStatus::Expired),
        "REJECTED" => Ok(OrderStatus::Rejected),
        other => Err(QxError::ReconcileRequired(format!(
            "Binance 未知订单状态: {other}"
        ))),
    }
}

#[cfg(test)]
#[path = "binance/tests.rs"]
mod tests;
