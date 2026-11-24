//! Binance Spot remote order reconciliation and unknown-outcome recovery.
//!
//! This module owns remote snapshots, order-group identity recovery, missing-trade
//! retrieval, and replayable reconciliation facts. `BinanceSpotVenue` remains the
//! sole local order/fill state owner.

use super::*;

impl BinanceSpotVenue {
    pub fn fetch_remote_snapshot(&mut self) -> QxResult<Vec<VenueOrderSnapshot>> {
        qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES.require_query()?;
        let mut symbols = self.reconcile_symbols.clone();
        if symbols.is_empty() {
            symbols = self
                .orders
                .values()
                .map(|order| order.instrument.symbol().to_ascii_uppercase())
                .collect();
        }
        let mut wires = Vec::new();
        if symbols.is_empty() {
            let response = self.request_with_weight(
                "GET",
                "/api/v3/openOrders",
                BTreeMap::new(),
                self.last_event_ts,
                80,
            )?;
            wires = parse_success(response, "openOrders")?;
        } else {
            for symbol in symbols {
                let mut next_order_id: Option<u64> = None;
                let mut previous_last_order_id: Option<u64> = None;
                loop {
                    let mut parameters = BTreeMap::new();
                    parameters.insert("limit".into(), "1000".into());
                    parameters.insert("symbol".into(), symbol.clone());
                    if let Some(order_id) = next_order_id {
                        parameters.insert("orderId".into(), order_id.to_string());
                    }
                    let response = self.request_with_weight(
                        "GET",
                        "/api/v3/allOrders",
                        parameters,
                        self.last_event_ts,
                        20,
                    )?;
                    let mut page: Vec<BinanceSnapshotWire> = parse_success(response, "allOrders")?;
                    let page_len = page.len();
                    let last_order_id = page.last().map(|order| order.order_id);
                    wires.append(&mut page);
                    if page_len < 1_000 {
                        break;
                    }
                    let last_order_id = last_order_id.ok_or_else(|| {
                        QxError::ReconcileRequired("Binance allOrders 分页返回空 orderId".into())
                    })?;
                    if last_order_id == 0 || previous_last_order_id == Some(last_order_id) {
                        return Err(QxError::ReconcileRequired(
                            "Binance allOrders 分页游标未前进".into(),
                        ));
                    }
                    previous_last_order_id = Some(last_order_id);
                    next_order_id = Some(last_order_id.checked_add(1).ok_or_else(|| {
                        QxError::ReconcileRequired("Binance orderId 分页游标溢出".into())
                    })?);
                }
            }
        }
        wires
            .into_iter()
            .map(|wire| {
                let client_order_id =
                    parse_client_order_id(&wire.client_order_id).ok_or_else(|| {
                        QxError::ReconcileRequired(format!(
                            "Binance 快照包含无法映射的 clientOrderId: {}",
                            wire.client_order_id
                        ))
                    })?;
                if wire.order_id == 0 {
                    return Err(QxError::ReconcileRequired(
                        "Binance 快照缺少有效 orderId".into(),
                    ));
                }
                let remote_order_id = wire.order_id.to_string();
                if self
                    .venue_order_ids
                    .insert(client_order_id, remote_order_id.clone())
                    .is_some_and(|existing| existing != remote_order_id)
                {
                    return Err(QxError::ReconcileRequired(format!(
                        "Binance 快照中的 clientOrderId {} 对应多个 orderId",
                        client_order_id
                    )));
                }
                let _ = wire.symbol; // 归属只认 clientOrderId（`qx-<u64>` 全局唯一）；symbol 是交易所回显，显式丢弃以保持该字段被读
                let (filled, filled_exact) =
                    parse_exact_quantity_projection(&wire.executed_qty, "executedQty")?;
                Ok(VenueOrderSnapshot {
                    client_order_id,
                    status: map_status(&wire.status)?,
                    filled,
                    filled_exact: Some(filled_exact),
                })
            })
            .collect()
    }

    /// Recover a native OCO list identity from Binance after a worker restart.
    /// The returned child IDs are checked against the durable local intent before
    /// callers are allowed to clear a missing-list-identity barrier.
    pub fn recover_order_group_identity(
        &mut self,
        group: &OrderGroupState,
    ) -> QxResult<(String, BTreeMap<u64, String>)> {
        if group.intent.kind != OrderGroupKind::Oco
            || group.intent.execution != OrderGroupExecution::NativeRequired
        {
            return Err(QxError::BusinessViolation(
                "Binance 仅支持恢复原生 OCO 订单组身份".into(),
            ));
        }
        let (expected_list_client_id, _) = binance_oco_order_list_parameters(group)?;
        let mut parameters = BTreeMap::new();
        parameters.insert("origClientOrderId".into(), expected_list_client_id.clone());
        let response = self.request_readonly_params_with_weight(
            "/api/v3/orderList",
            parameters,
            self.last_event_ts,
            4,
        )?;
        let wire: BinanceOrderListLookupWire = parse_success(response, "OCO order list lookup")?;
        let expected_symbol = group
            .legs
            .first()
            .ok_or_else(|| QxError::BusinessViolation("Binance OCO 订单组为空".into()))?
            .order
            .instrument
            .symbol()
            .to_ascii_uppercase();
        if wire.order_list_id < 0
            || wire.contingency_type != "OCO"
            || wire.list_client_order_id != expected_list_client_id
            || !wire.symbol.eq_ignore_ascii_case(&expected_symbol)
            || wire.orders.len() != group.legs.len()
        {
            return Err(QxError::ReconcileRequired(
                "Binance OCO 列表查询结果与本地订单组身份不匹配".into(),
            ));
        }
        let expected_client_ids = group
            .legs
            .iter()
            .map(|leg| (client_order_id(leg.order.client_id), leg.order.client_id))
            .collect::<BTreeMap<_, _>>();
        let mut child_order_ids = BTreeMap::new();
        let mut seen_client_ids = BTreeSet::new();
        for child in wire.orders {
            let Some(client_id) = expected_client_ids.get(&child.client_order_id).copied() else {
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 列表包含非本地意图中的子单".into(),
                ));
            };
            if child.order_id == 0
                || !seen_client_ids.insert(client_id)
                || child_order_ids
                    .insert(client_id, child.order_id.to_string())
                    .is_some()
            {
                return Err(QxError::ReconcileRequired(
                    "Binance OCO 列表子单 ID 缺失或重复".into(),
                ));
            }
        }
        if seen_client_ids.len() != group.legs.len() {
            return Err(QxError::ReconcileRequired(
                "Binance OCO 列表未覆盖全部本地子腿".into(),
            ));
        }
        Ok((wire.order_list_id.to_string(), child_order_ids))
    }

    /// Recover missing fills for one order from Binance `myTrades`. The complete remote history
    /// must account for the snapshot total, and locally recorded trade IDs must account for the
    /// restored OMS fill total, before this method mutates the connector's order state.
    pub fn fetch_missing_order_fills(
        &mut self,
        client_order_id: u64,
        remote_filled: ExactDecimal,
        recorded_trade_ids: &BTreeSet<String>,
    ) -> QxResult<Vec<(String, Fill)>> {
        qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES.require_query()?;
        let order = self
            .orders
            .get(&client_order_id)
            .cloned()
            .ok_or_else(|| QxError::ReconcileRequired("Binance 成交回补订单不存在".into()))?;
        if order.status.is_terminal() {
            return Err(QxError::ReconcileRequired(
                "Binance 终态本地订单不能回补成交".into(),
            ));
        }
        let remote_order_id = self
            .venue_order_ids
            .get(&client_order_id)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 成交回补缺少 orderId".into()))?;
        let symbol = order.instrument.symbol().to_ascii_uppercase();
        let mut trades = Vec::new();
        let mut next_trade_id: Option<u64> = None;
        let mut previous_last_trade_id: Option<u64> = None;
        loop {
            let mut parameters = BTreeMap::new();
            parameters.insert("limit".into(), "1000".into());
            parameters.insert("orderId".into(), remote_order_id.to_string());
            parameters.insert("symbol".into(), symbol.clone());
            if let Some(trade_id) = next_trade_id {
                parameters.insert("fromId".into(), trade_id.to_string());
            }
            let response = self.request_readonly_params_with_weight(
                "/api/v3/myTrades",
                parameters,
                self.last_event_ts,
                5,
            )?;
            let mut page: Vec<BinanceAccountTradeWire> = parse_success(response, "myTrades")?;
            let page_len = page.len();
            let last_trade_id = page.last().map(|trade| trade.trade_id);
            trades.append(&mut page);
            if page_len < 1_000 {
                break;
            }
            let last_trade_id = last_trade_id.ok_or_else(|| {
                QxError::ReconcileRequired("Binance myTrades 分页缺少 trade id".into())
            })?;
            if last_trade_id == 0 || previous_last_trade_id == Some(last_trade_id) {
                return Err(QxError::ReconcileRequired(
                    "Binance myTrades 分页游标未前进".into(),
                ));
            }
            previous_last_trade_id = Some(last_trade_id);
            next_trade_id = Some(last_trade_id.checked_add(1).ok_or_else(|| {
                QxError::ReconcileRequired("Binance trade id 分页游标溢出".into())
            })?);
        }
        trades.sort_by_key(|trade| trade.trade_id);
        if trades
            .windows(2)
            .any(|pair| pair[0].trade_id == pair[1].trade_id)
        {
            return Err(QxError::ReconcileRequired(
                "Binance myTrades 返回重复 trade id".into(),
            ));
        }
        let mut remote_total = ExactDecimal::ZERO;
        let mut recorded_total = ExactDecimal::ZERO;
        let mut missing_total = ExactDecimal::ZERO;
        let mut missing = Vec::new();
        for trade in trades {
            if trade.order_id != remote_order_id
                || !trade.symbol.eq_ignore_ascii_case(&symbol)
                || trade.is_buyer != (order.side == Side::Buy)
            {
                return Err(QxError::ReconcileRequired(format!(
                    "Binance myTrades 与本地订单身份不匹配 client_order_id={client_order_id} trade_id={}",
                    trade.trade_id
                )));
            }
            let quantity = parse_exact_decimal(&trade.qty, "myTrades qty")?;
            if !quantity.is_positive() {
                return Err(QxError::ReconcileRequired(
                    "Binance myTrades 数量必须为正数".into(),
                ));
            }
            remote_total = remote_total.checked_add(quantity).ok_or_else(|| {
                QxError::ReconcileRequired("Binance myTrades 累计成交溢出".into())
            })?;
            let trade_id = trade.trade_id.to_string();
            if recorded_trade_ids.contains(&trade_id) {
                recorded_total = recorded_total.checked_add(quantity).ok_or_else(|| {
                    QxError::ReconcileRequired("Binance 已入账成交累计溢出".into())
                })?;
            } else {
                missing_total = missing_total.checked_add(quantity).ok_or_else(|| {
                    QxError::ReconcileRequired("Binance 待回补成交累计溢出".into())
                })?;
                missing.push((trade_id, trade));
            }
        }
        let local_filled = if let Some(amounts) = order.exact_amounts {
            amounts.filled
        } else {
            ExactDecimal::from_parts(order.filled.raw(), 9).map_err(|error| {
                QxError::ReconcileRequired(format!("本地累计成交精度转换失败: {error}"))
            })?
        };
        let expected_missing = remote_filled
            .checked_sub(local_filled)
            .ok_or_else(|| QxError::ReconcileRequired("远端累计成交小于本地".into()))?;
        if remote_total != remote_filled
            || recorded_total != local_filled
            || missing_total != expected_missing
        {
            return Err(QxError::ReconcileRequired(format!(
                "Binance myTrades 无法证明完整回补 remote={remote_total} recorded={recorded_total} local={local_filled} missing={missing_total} expected_missing={expected_missing} snapshot={remote_filled}"
            )));
        }
        let venue_order_id = remote_order_id.to_string();
        let mut fills = Vec::with_capacity(missing.len());
        for (trade_id, trade) in missing {
            let wire = BinanceFillWire {
                price: trade.price,
                qty: trade.qty,
                commission: trade.commission,
                commission_asset: trade.commission_asset,
                trade_id: trade.trade_id,
                transact_time: trade.transact_time,
            };
            match self.ingest_fill_wire(client_order_id, &venue_order_id, wire)? {
                Some(VenueEvent::Fill(fill)) => fills.push((trade_id, *fill)),
                Some(_) => {
                    return Err(QxError::Invariant(
                        "Binance 成交回补生成了非 Fill venue event".into(),
                    ));
                }
                None => {
                    return Err(QxError::ReconcileRequired(
                        "Binance myTrades 成交已在连接器中去重，但 EventLog 身份缺失".into(),
                    ));
                }
            }
        }
        Ok(fills)
    }

    pub fn reconcile_remote(
        &mut self,
        remote: &[VenueOrderSnapshot],
    ) -> QxResult<Vec<crate::AdapterReconcileIssue>> {
        qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES.require_reconciliation()?;
        if !self.connected || !matches!(self.state, ConnectorState::Snapshotting) {
            return Err(QxError::VenueState("Binance Spot 不在重连对账阶段".into()));
        }
        let remote = remote.iter().try_fold(BTreeMap::new(), |mut map, order| {
            if map.insert(order.client_order_id, order).is_some() {
                return Err(QxError::ReconcileRequired(
                    "Binance 快照包含重复 client_order_id".into(),
                ));
            }
            Ok(map)
        })?;
        // venue 专属过滤：本地终态订单不参与对账（事件日志已收口，无需再向柜台核对）。
        let local_facts: Vec<_> = self
            .orders
            .iter()
            .filter(|(_, order)| !order.status.is_terminal())
            .map(|(id, order)| crate::reconcile::local_order_fact(id, order))
            .collect();
        let mut issues = crate::reconcile::reconcile_issues(
            &local_facts,
            &crate::reconcile::remote_order_facts(&remote),
        );
        // venue 专属过滤：`allOrders` 会返回账户历史终态单，或本地已以终态收口的订单，
        // 它们都不属于本 worker 待核对的活跃订单，不应把连接器永久钉在 ReconcileRequired。
        issues.retain(|issue| match issue {
            crate::AdapterReconcileIssue::MissingLocally { client_order_id } => {
                !self.orders.contains_key(client_order_id)
                    && !remote
                        .get(client_order_id)
                        .is_some_and(|order| order.status.is_terminal())
            }
            _ => true,
        });
        self.state = if issues.is_empty() {
            ConnectorState::Live
        } else {
            ConnectorState::ReconcileRequired
        };
        Ok(issues)
    }

    pub fn fetch_and_reconcile(&mut self) -> QxResult<Vec<crate::AdapterReconcileIssue>> {
        let remote = self.fetch_remote_snapshot()?;
        self.reconcile_remote(&remote)
    }
}
