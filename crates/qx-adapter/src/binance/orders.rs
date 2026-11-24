//! Binance Spot order submission, OCO lifecycle, cancellation, and response mapping.
//!
//! The venue keeps local order/fill state; this module owns the write-side exchange protocol.

use super::*;

#[derive(Clone, Debug, Deserialize)]
struct BinanceOrderWire {
    #[serde(rename = "orderId", default)]
    order_id: u64,
    #[serde(rename = "clientOrderId", default)]
    client_order_id: String,
    #[serde(default)]
    status: String,
    #[serde(rename = "executedQty", default)]
    executed_qty: String,
    #[serde(default)]
    fills: Vec<BinanceFillWire>,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceOrderListWire {
    #[serde(rename = "orderListId", default)]
    order_list_id: i64,
    #[serde(rename = "contingencyType", default)]
    contingency_type: String,
    #[serde(rename = "listStatusType", default)]
    list_status_type: String,
    #[serde(rename = "listOrderStatus", default)]
    list_order_status: String,
    #[serde(rename = "listClientOrderId", default)]
    list_client_order_id: String,
    #[serde(default)]
    symbol: String,
    #[serde(rename = "orderReports", default)]
    order_reports: Vec<BinanceOrderWire>,
}

#[derive(Clone, Debug)]
pub(super) struct BinanceOrderListCancelContext {
    pub(super) order_list_id: i64,
    pub(super) list_client_order_id: String,
    pub(super) symbol: String,
    pub(super) parameters: BTreeMap<String, String>,
    pub(super) active_client_order_id: u64,
    pub(super) child_client_order_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct BinanceOrderListLookupWire {
    #[serde(rename = "orderListId", default)]
    pub(super) order_list_id: i64,
    #[serde(rename = "contingencyType", default)]
    pub(super) contingency_type: String,
    #[serde(rename = "listClientOrderId", default)]
    pub(super) list_client_order_id: String,
    #[serde(default)]
    pub(super) symbol: String,
    #[serde(default)]
    pub(super) orders: Vec<BinanceOrderListChildWire>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct BinanceOrderListChildWire {
    #[serde(rename = "orderId", default)]
    pub(super) order_id: u64,
    #[serde(rename = "clientOrderId", default)]
    pub(super) client_order_id: String,
}

impl BinanceSpotVenue {
    pub(super) fn validate_order_group_impl(&mut self, group: &OrderGroupState) -> QxResult<()> {
        group
            .validate_persisted()
            .map_err(QxError::BusinessViolation)?;
        self.execution_readiness()?;
        if group.intent.kind != OrderGroupKind::Oco
            || group.intent.execution != OrderGroupExecution::NativeRequired
        {
            return Err(QxError::BusinessViolation(
                "Binance Spot 当前只支持显式 NativeRequired 的 OCO 订单组".into(),
            ));
        }
        let take_profit = group
            .legs
            .iter()
            .find(|leg| leg.role == OrderGroupLegRole::TakeProfit)
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 缺少止盈腿".into()))?;
        let stop_loss = group
            .legs
            .iter()
            .find(|leg| leg.role == OrderGroupLegRole::StopLoss)
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 缺少止损腿".into()))?;
        let symbol = take_profit.order.instrument.symbol().to_ascii_uppercase();
        validate_binance_instrument(&take_profit.order.instrument)?;
        if take_profit.order.instrument != stop_loss.order.instrument
            || !take_profit.order.instrument.venue.is_binance()
            || take_profit.order.side != stop_loss.order.side
        {
            return Err(QxError::BusinessViolation(
                "Binance OCO 子腿必须属于同一 Binance Spot 标的并使用同一方向".into(),
            ));
        }
        if group.legs.iter().any(|leg| {
            self.orders.contains_key(&leg.order.client_id)
                || self.venue_order_ids.contains_key(&leg.order.client_id)
        }) {
            return Err(QxError::ReconcileRequired(
                "Binance OCO 子腿已存在于连接器本地状态，先对账再提交".into(),
            ));
        }
        let rules = self.current_spot_market_rules(&symbol)?;
        if rules.status != "TRADING" {
            return Err(QxError::VenueState(format!(
                "Binance Spot {symbol} 当前状态为 {}",
                rules.status
            )));
        }
        let quantity = take_profit.order.quantity_decimal();
        validate_binance_exact_qty_filter(
            quantity,
            rules.exact_qty_step,
            rules.exact_min_qty,
            rules.exact_max_qty,
            "LOT_SIZE",
        )?;
        let take_profit_trigger = take_profit
            .order
            .exact_amounts
            .and_then(|amounts| amounts.conditional)
            .and_then(|intent| intent.trigger_price)
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 止盈腿缺少触发价".into()))?;
        let stop_loss_trigger = stop_loss
            .order
            .exact_amounts
            .and_then(|amounts| amounts.conditional)
            .and_then(|intent| intent.trigger_price)
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 止损腿缺少触发价".into()))?;
        validate_binance_exact_price_filter(
            take_profit_trigger,
            rules.exact_price_tick,
            rules.exact_min_price,
            rules.exact_max_price,
        )?;
        validate_binance_exact_price_filter(
            stop_loss_trigger,
            rules.exact_price_tick,
            rules.exact_min_price,
            rules.exact_max_price,
        )?;
        for leg in [take_profit, stop_loss] {
            if let Some(price) = leg.order.limit_price_decimal() {
                validate_binance_exact_price_filter(
                    price,
                    rules.exact_price_tick,
                    rules.exact_min_price,
                    rules.exact_max_price,
                )?;
            }
        }
        let relation = take_profit_trigger
            .checked_cmp(stop_loss_trigger)
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 触发价比较失败".into()))?;
        let valid_relation = match take_profit.order.side {
            Side::Sell => relation.is_gt(),
            Side::Buy => relation.is_lt(),
        };
        if !valid_relation {
            return Err(QxError::BusinessViolation(
                "Binance Spot OCO 止盈触发价必须高于止损价（Sell）或低于止损价（Buy）".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn submit_order_group_impl(
        &mut self,
        group: &OrderGroupState,
        ts: u64,
    ) -> QxResult<VenueOrderGroupSubmission> {
        self.validate_order_group_impl(group)?;
        let (list_client_order_id, parameters) = binance_oco_order_list_parameters(group)?;
        let response = self.request("POST", "/api/v3/orderList/oco", parameters, ts)?;
        let wire: BinanceOrderListWire = match parse_success(response, "OCO order list") {
            Ok(wire) => wire,
            Err(error) => {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(format!(
                    "Binance OCO 请求已发出但回报无法解析: {error:?}"
                )));
            }
        };
        if wire.order_list_id < 0
            || wire.list_client_order_id != list_client_order_id
            || wire.order_reports.len() != 2
        {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance OCO 响应缺少或篡改订单列表 ID/子单回报".into(),
            ));
        }
        let mut reports = BTreeMap::new();
        for report in wire.order_reports {
            let Some(client_id) = parse_client_order_id(&report.client_order_id) else {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 子单回报 clientOrderId 无法映射".into(),
                ));
            };
            if report.order_id == 0
                || !group
                    .legs
                    .iter()
                    .any(|leg| leg.order.client_id == client_id)
                || reports.insert(client_id, report).is_some()
            {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 子单回报与提交意图不匹配".into(),
                ));
            }
        }
        if group
            .legs
            .iter()
            .any(|leg| !reports.contains_key(&leg.order.client_id))
        {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance OCO 响应未覆盖全部子单".into(),
            ));
        }

        let mut events = Vec::new();
        for leg in &group.legs {
            let Some(report) = reports.remove(&leg.order.client_id) else {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 响应在归并时缺少子单".into(),
                ));
            };
            let venue_order_id = report.order_id.to_string();
            let mut accepted = leg.order.clone();
            accepted.status = OrderStatus::Accepted;
            self.orders.insert(leg.order.client_id, accepted);
            self.venue_order_ids
                .insert(leg.order.client_id, venue_order_id.clone());
            events.push(VenueEvent::Accepted {
                client_order_id: leg.order.client_id,
                venue_order_id: venue_order_id.clone(),
                ts,
            });
            let event_count_before_fills = events.len();
            for fill in report.fills {
                if let Some(event) =
                    self.ingest_fill_wire(leg.order.client_id, &venue_order_id, fill)?
                {
                    events.push(event);
                }
            }
            match report.status.as_str() {
                "NEW" => {}
                "PARTIALLY_FILLED" if events.len() > event_count_before_fills => {}
                "PARTIALLY_FILLED" => {
                    self.state = ConnectorState::ReconcileRequired;
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance OCO 子单 {} 报告 PARTIALLY_FILLED 但未返回成交事实",
                        leg.order.client_id
                    )));
                }
                "FILLED" => {
                    if events.len() == event_count_before_fills {
                        self.state = ConnectorState::ReconcileRequired;
                        return Err(QxError::ReconcileRequired(format!(
                            "Binance OCO 子单 {} 报告 FILLED 但未返回成交事实",
                            leg.order.client_id
                        )));
                    }
                    self.mark_remote_filled(leg.order.client_id)?;
                }
                "CANCELED" => {
                    if self.mark_cancelled(leg.order.client_id)? {
                        events.push(VenueEvent::Cancelled {
                            client_order_id: leg.order.client_id,
                            ts,
                        });
                    }
                }
                status => {
                    self.state = ConnectorState::ReconcileRequired;
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance OCO 子单 {} 进入未预期状态 {status}",
                        leg.order.client_id
                    )));
                }
            }
        }
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(VenueOrderGroupSubmission {
            venue_group_id: Some(wire.order_list_id.to_string()),
            group_status: None,
            events,
        })
    }

    pub(super) fn submit_order_impl(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_submit(&order, self.id())
            .map_err(QxError::BusinessViolation)?;
        Self::validate_order_spec(&order)?;
        if !Self::can_submit(&order) {
            return Err(QxError::BusinessViolation(
                "Binance Spot 只接受待提交或已提交订单".into(),
            ));
        }
        if !self.connected {
            return Err(QxError::Ambiguous("Binance Spot 连接中断".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState("Binance Spot 尚未完成对账".into()));
        }
        if let Some(venue_order_id) = self.venue_order_ids.get(&order.client_id) {
            return Ok(vec![VenueEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: venue_order_id.clone(),
                ts,
            }]);
        }
        if self.orders.contains_key(&order.client_id) {
            return Err(QxError::ReconcileRequired(
                "本地订单已存在但缺少 Binance 远端订单号".into(),
            ));
        }
        validate_binance_instrument(&order.instrument)?;
        let symbol = order.instrument.symbol().to_ascii_uppercase();
        let mut parameters = BTreeMap::new();
        parameters.insert("symbol".into(), symbol);
        parameters.insert(
            "side".into(),
            match order.side {
                Side::Buy => "BUY".into(),
                Side::Sell => "SELL".into(),
            },
        );
        parameters.insert(
            "type".into(),
            if order.limit_price_decimal().is_some() {
                "LIMIT".into()
            } else {
                "MARKET".into()
            },
        );
        if let Some(budget) = order.quote_budget_decimal() {
            parameters.insert("quoteOrderQty".into(), budget.to_string());
        } else {
            parameters.insert("quantity".into(), order.quantity_decimal().to_string());
        }
        parameters.insert("newClientOrderId".into(), client_order_id(order.client_id));
        parameters.insert("newOrderRespType".into(), "FULL".into());
        if let Some(price) = order.limit_price_decimal() {
            parameters.insert("timeInForce".into(), "GTC".into());
            parameters.insert("price".into(), price.to_string());
        }
        let response = self.request("POST", "/api/v3/order", parameters, ts)?;
        let wire: BinanceOrderWire = parse_success(response, "order")?;
        if wire.order_id == 0 || wire.client_order_id != client_order_id(order.client_id) {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 下单响应缺少或篡改 client/order id".into(),
            ));
        }
        let local_id = order.client_id;
        let mut accepted_order = order;
        accepted_order.status = OrderStatus::Accepted;
        self.orders.insert(local_id, accepted_order);
        let id = wire.order_id.to_string();
        self.venue_order_ids.insert(local_id, id.clone());
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        let mut events = vec![VenueEvent::Accepted {
            client_order_id: local_id,
            venue_order_id: id.clone(),
            ts,
        }];
        for fill in wire.fills {
            if let Some(event) = self.ingest_fill_wire(local_id, &id, fill)? {
                events.push(event);
            }
        }
        if wire.status == "FILLED" && events.len() == 1 {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 已报告 FILLED 但响应缺少成交事实".into(),
            ));
        }
        if wire.status == "FILLED" {
            self.mark_remote_filled(local_id)?;
        }
        Ok(events)
    }

    pub(super) fn cancel_order_impl(
        &mut self,
        local_id: u64,
        ts: u64,
    ) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_cancel()
            .map_err(QxError::BusinessViolation)?;
        let order = self
            .orders
            .get(&local_id)
            .ok_or_else(|| QxError::Permanent("Binance 本地订单不存在".into()))?
            .clone();
        if order.status.is_terminal() {
            return Ok(Vec::new());
        }
        Self::validate_cancel_state(&order, "Binance")?;
        let mut parameters = BTreeMap::new();
        validate_binance_instrument(&order.instrument)?;
        parameters.insert(
            "symbol".into(),
            order.instrument.symbol().to_ascii_uppercase(),
        );
        parameters.insert("origClientOrderId".into(), client_order_id(local_id));
        let wire: BinanceOrderWire = if let Some(context) = self.pending_order_list_cancel.take() {
            if context.active_client_order_id != local_id {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 列表撤销上下文与撤销子单不一致".into(),
                ));
            }
            let response = self.request(
                "DELETE",
                "/api/v3/orderList",
                context.parameters.clone(),
                ts,
            )?;
            let wire: BinanceOrderListWire = match parse_success(response, "cancel OCO order list")
            {
                Ok(wire) => wire,
                Err(error) => {
                    self.state = ConnectorState::ReconcileRequired;
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance OCO 列表撤销回报无法解析: {error:?}"
                    )));
                }
            };
            if wire.order_list_id != context.order_list_id
                || wire.contingency_type != "OCO"
                || wire.list_status_type != "ALL_DONE"
                || wire.list_order_status != "ALL_DONE"
                || wire.list_client_order_id != context.list_client_order_id
                || !wire.symbol.eq_ignore_ascii_case(&context.symbol)
                || wire.order_reports.len() != context.child_client_order_ids.len()
            {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 列表撤销响应身份、终态或子单数量不匹配".into(),
                ));
            }
            let mut reports = BTreeMap::new();
            for report in wire.order_reports {
                if report.order_id == 0
                    || !context
                        .child_client_order_ids
                        .contains(&report.client_order_id)
                    || !matches!(
                        report.status.as_str(),
                        "FILLED" | "CANCELED" | "EXPIRED" | "REJECTED"
                    )
                    || reports
                        .insert(report.client_order_id.clone(), report)
                        .is_some()
                {
                    self.state = ConnectorState::ReconcileRequired;
                    return Err(QxError::ReconcileRequired(
                        "Binance OCO 列表撤销响应包含未知、重复或非终态子单".into(),
                    ));
                }
            }
            if reports.len() != context.child_client_order_ids.len() {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 列表撤销响应未覆盖全部子单".into(),
                ));
            }
            let Some(report) = reports.remove(&client_order_id(local_id)) else {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 列表撤销响应缺少目标子单".into(),
                ));
            };
            report
        } else {
            let response = self.request("DELETE", "/api/v3/order", parameters, ts)?;
            parse_success(response, "cancel order")?
        };
        if wire.client_order_id != client_order_id(local_id) || wire.order_id == 0 {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 撤单响应订单身份与本地订单不一致".into(),
            ));
        }
        let remote_filled = parse_exact_decimal(&wire.executed_qty, "cancel executedQty")?;
        let local_filled = self
            .orders
            .get(&local_id)
            .and_then(|order| order.exact_amounts.map(|amounts| amounts.filled))
            .unwrap_or_else(|| {
                self.orders
                    .get(&local_id)
                    .and_then(|order| ExactDecimal::from_parts(order.filled.raw(), 9).ok())
                    .unwrap_or(ExactDecimal::ZERO)
            });
        if remote_filled
            .checked_cmp(local_filled)
            .is_none_or(|ordering| ordering.is_lt())
        {
            self.state = ConnectorState::ReconcileRequired;
            return Err(QxError::ReconcileRequired(
                "Binance 撤单响应累计成交小于本地成交".into(),
            ));
        }
        let recorded_trade_ids = self
            .recorded_trade_ids
            .get(&local_id)
            .cloned()
            .unwrap_or_default();
        let mut events = Vec::new();
        if remote_filled
            .checked_cmp(local_filled)
            .is_some_and(|ordering| ordering.is_gt())
        {
            match self.fetch_missing_order_fills(local_id, remote_filled, &recorded_trade_ids) {
                Ok(fills) => events.extend(
                    fills
                        .into_iter()
                        .map(|(_, fill)| VenueEvent::Fill(Box::new(fill))),
                ),
                Err(error) => {
                    self.state = ConnectorState::ReconcileRequired;
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance 撤单竞态成交回补失败: {error:?}"
                    )));
                }
            }
        }
        match wire.status.as_str() {
            "CANCELED" => {
                self.mark_cancelled(local_id)?;
                events.push(VenueEvent::Cancelled {
                    client_order_id: local_id,
                    ts,
                });
            }
            "FILLED" => {
                self.mark_remote_filled(local_id)?;
            }
            other => {
                self.state = ConnectorState::ReconcileRequired;
                return Err(QxError::ReconcileRequired(format!(
                    "Binance 撤单后订单状态为 {other}，结果待对账"
                )));
            }
        }
        Ok(events)
    }
}

pub(super) fn client_order_id(id: u64) -> String {
    format!("qx-{id}")
}

pub(super) fn binance_group_client_order_id(group_id: &str) -> String {
    // Stable non-cryptographic identifier constrained to Binance's client ID alphabet/length.
    let hash = group_id
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    format!("qxg-{hash:016x}")
}

pub(super) fn binance_oco_order_list_parameters(
    group: &OrderGroupState,
) -> QxResult<(String, BTreeMap<String, String>)> {
    let take_profit = group
        .legs
        .iter()
        .find(|leg| leg.role == OrderGroupLegRole::TakeProfit)
        .ok_or_else(|| QxError::BusinessViolation("Binance OCO 缺少止盈腿".into()))?;
    let stop_loss = group
        .legs
        .iter()
        .find(|leg| leg.role == OrderGroupLegRole::StopLoss)
        .ok_or_else(|| QxError::BusinessViolation("Binance OCO 缺少止损腿".into()))?;
    let take_profit_trigger = take_profit
        .order
        .exact_amounts
        .and_then(|amounts| amounts.conditional)
        .and_then(|intent| intent.trigger_price)
        .ok_or_else(|| QxError::BusinessViolation("Binance OCO 止盈腿缺少触发价".into()))?;
    let stop_loss_trigger = stop_loss
        .order
        .exact_amounts
        .and_then(|amounts| amounts.conditional)
        .and_then(|intent| intent.trigger_price)
        .ok_or_else(|| QxError::BusinessViolation("Binance OCO 止损腿缺少触发价".into()))?;
    let relation = take_profit_trigger
        .checked_cmp(stop_loss_trigger)
        .ok_or_else(|| QxError::BusinessViolation("Binance OCO 触发价比较失败".into()))?;
    let valid_relation = match take_profit.order.side {
        Side::Sell => relation.is_gt(),
        Side::Buy => relation.is_lt(),
    };
    if !valid_relation {
        return Err(QxError::BusinessViolation(
            "Binance Spot OCO 止盈触发价必须高于止损价（Sell）或低于止损价（Buy）".into(),
        ));
    }
    let (above, below) = if relation.is_gt() {
        (take_profit, stop_loss)
    } else {
        (stop_loss, take_profit)
    };
    let list_client_order_id = binance_group_client_order_id(&group.intent.group_id);
    let mut parameters = BTreeMap::new();
    validate_binance_instrument(&take_profit.order.instrument)?;
    parameters.insert(
        "symbol".into(),
        take_profit.order.instrument.symbol().to_ascii_uppercase(),
    );
    parameters.insert(
        "side".into(),
        match take_profit.order.side {
            Side::Buy => "BUY".into(),
            Side::Sell => "SELL".into(),
        },
    );
    parameters.insert(
        "quantity".into(),
        take_profit.order.quantity_decimal().to_string(),
    );
    parameters.insert("listClientOrderId".into(), list_client_order_id.clone());
    parameters.insert(
        "aboveClientOrderId".into(),
        client_order_id(above.order.client_id),
    );
    parameters.insert(
        "belowClientOrderId".into(),
        client_order_id(below.order.client_id),
    );
    parameters.insert("newOrderRespType".into(), "FULL".into());
    for (prefix, leg) in [("above", above), ("below", below)] {
        let conditional = leg
            .order
            .exact_amounts
            .and_then(|amounts| amounts.conditional)
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 条件腿意图缺失".into()))?;
        let kind = match (conditional.kind, leg.order.limit_price_decimal().is_some()) {
            (qx_core::ConditionalOrderKind::TakeProfit, true) => "TAKE_PROFIT_LIMIT",
            (qx_core::ConditionalOrderKind::TakeProfit, false) => "TAKE_PROFIT",
            (qx_core::ConditionalOrderKind::StopLoss, true) => "STOP_LOSS_LIMIT",
            (qx_core::ConditionalOrderKind::StopLoss, false) => "STOP_LOSS",
            _ => {
                return Err(QxError::BusinessViolation(
                    "Binance Spot OCO 仅支持止盈/止损条件腿".into(),
                ))
            }
        };
        parameters.insert(format!("{prefix}Type"), kind.into());
        parameters.insert(
            format!("{prefix}StopPrice"),
            conditional
                .trigger_price
                .ok_or_else(|| QxError::BusinessViolation("Binance OCO 缺少触发价".into()))?
                .to_string(),
        );
        if let Some(price) = leg.order.limit_price_decimal() {
            parameters.insert(format!("{prefix}Price"), price.to_string());
            parameters.insert(format!("{prefix}TimeInForce"), "GTC".into());
        }
    }
    Ok((list_client_order_id, parameters))
}

pub(super) fn binance_oco_order_list_cancel_parameters(
    group: &OrderGroupState,
) -> QxResult<(i64, String, String, BTreeMap<String, String>)> {
    if group.intent.kind != OrderGroupKind::Oco
        || group.intent.execution != OrderGroupExecution::NativeRequired
        || group.legs.len() != 2
    {
        return Err(QxError::BusinessViolation(
            "Binance 列表撤销仅支持已持久化的原生双腿 OCO".into(),
        ));
    }
    let order_list_id = group
        .venue_group_id
        .as_deref()
        .ok_or_else(|| QxError::ReconcileRequired("Binance OCO 缺少 orderListId".into()))?
        .parse::<i64>()
        .map_err(|_| QxError::ReconcileRequired("Binance OCO orderListId 非法".into()))?;
    if order_list_id < 0 {
        return Err(QxError::ReconcileRequired(
            "Binance OCO orderListId 必须为非负整数".into(),
        ));
    }
    let first = &group.legs[0].order.instrument;
    validate_binance_instrument(first)?;
    let symbol = first.symbol().to_ascii_uppercase();
    for leg in &group.legs[1..] {
        validate_binance_instrument(&leg.order.instrument)?;
        if !leg.order.instrument.symbol().eq_ignore_ascii_case(&symbol) {
            return Err(QxError::ReconcileRequired(
                "Binance OCO 子腿 symbol 不一致".into(),
            ));
        }
    }
    let list_client_order_id = binance_group_client_order_id(&group.intent.group_id);
    let mut parameters = BTreeMap::new();
    parameters.insert("symbol".into(), symbol.clone());
    parameters.insert("orderListId".into(), order_list_id.to_string());
    parameters.insert("listClientOrderId".into(), list_client_order_id.clone());
    Ok((order_list_id, list_client_order_id, symbol, parameters))
}

pub(super) fn parse_client_order_id(value: &str) -> Option<u64> {
    value.strip_prefix("qx-")?.parse().ok()
}
