use super::*;
use qx_core::{ExactFillAmounts, ExactFillFee, ExactOrderAmounts};

impl CcxtProcessVenue {
    pub(super) fn sync_exact_order(
        &mut self,
        client_order_id: u64,
        ts: u64,
        order: &Order,
        remote_id: &str,
        remote: &Value,
    ) -> QxResult<Vec<VenueEvent>> {
        let local_amounts = order
            .exact_amounts
            .ok_or_else(|| QxError::Invariant("精确 CCXT 同步收到旧精度订单".into()))?;
        let quote_budget = local_amounts.quote_budget;
        let remote_amount = exact_decimal_field(remote, "amount_exact")?;
        if quote_budget.is_none() && remote_amount != local_amounts.quantity {
            return Err(QxError::ReconcileRequired(
                "CCXT exact amount 与本地订单数量不一致".into(),
            ));
        }
        let filled = exact_decimal_field(remote, "filled_exact")?;
        let prior_filled = local_amounts.filled;
        if filled
            .checked_cmp(prior_filled)
            .is_none_or(|ordering| ordering.is_lt())
            || (quote_budget.is_none()
                && filled
                    .checked_cmp(local_amounts.quantity)
                    .is_none_or(|ordering| ordering.is_gt()))
        {
            return Err(QxError::ReconcileRequired(
                "CCXT exact filled 与本地订单数量不一致".into(),
            ));
        }

        let remote_status = remote
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_ascii_lowercase();
        let filled_is_zero = filled.is_zero();
        let fully_filled = quote_budget.is_none()
            && filled
                .checked_cmp(local_amounts.quantity)
                .is_some_and(|ordering| ordering.is_eq());
        match remote_status.as_str() {
            "rejected" if filled_is_zero => {
                return Err(QxError::Permanent("CCXT 订单被交易所拒绝".into()));
            }
            "rejected" => {
                return Err(QxError::ReconcileRequired(
                    "CCXT 订单状态为 rejected 但已有 exact 成交，禁止静默归约".into(),
                ));
            }
            "open" | "new" | "live" | "pending" | "accepted" | "partially_filled"
            | "partially-filled"
                if !fully_filled => {}
            "canceled" | "cancelled" | "expired" => {}
            "closed" | "filled" | "done" if fully_filled || quote_budget.is_some() => {}
            "closed" | "filled" | "done" => {
                return Err(QxError::ReconcileRequired(
                    "CCXT exact 订单已关闭但成交数量不足，无法判断剩余数量是否已撤销".into(),
                ));
            }
            "open" | "new" | "live" | "pending" | "accepted" | "partially_filled"
            | "partially-filled" => {
                return Err(QxError::ReconcileRequired(
                    "CCXT exact 订单仍为未完成但成交数量已达到订单数量".into(),
                ));
            }
            _ => {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 返回未知 exact 订单状态: {remote_status}"
                )));
            }
        }

        let result = self.call_or_reconcile(json!({
            "op": "fetch_my_trades",
            "instrument": order.instrument.to_string(),
            "order_id": remote_id,
        }))?;
        let trades = result
            .get("trades")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                QxError::ReconcileRequired("CCXT exact 成交恢复响应缺少 trades 数组".into())
            })?;
        let prior_seen = self
            .seen_trade_ids
            .get(&client_order_id)
            .cloned()
            .unwrap_or_default();
        let mut seen = BTreeSet::new();
        let mut cumulative_qty = ExactDecimal::ZERO;
        let mut prior_qty = ExactDecimal::ZERO;
        let mut cumulative_cost = ExactDecimal::ZERO;
        let mut prior_cost = ExactDecimal::ZERO;
        let mut cumulative_fees = BTreeMap::<String, ExactDecimal>::new();
        let mut pending_fills = Vec::new();

        for trade in trades {
            let trade_id = trade
                .get("trade_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|trade_id| !trade_id.is_empty())
                .ok_or_else(|| QxError::ReconcileRequired("CCXT exact trade 缺少 trade_id".into()))?
                .to_string();
            if !seen.insert(trade_id.clone()) {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT exact 成交历史包含重复 trade_id: {trade_id}"
                )));
            }
            let trade_ts = trade
                .get("timestamp_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    QxError::ReconcileRequired(format!("CCXT exact trade {trade_id} 缺少成交时间"))
                })?;
            let quantity = exact_decimal_field(trade, "amount_exact")?;
            let price = exact_decimal_field(trade, "price_exact")?;
            let quote_cost = trade
                .get("cost_exact")
                .and_then(Value::as_str)
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| {
                    QxError::ReconcileRequired(format!(
                        "CCXT exact trade {trade_id} 成本非法: {error}"
                    ))
                })?;
            if !quantity.is_positive() || !price.is_positive() {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT exact trade {trade_id} 的数量或价格非法"
                )));
            }
            cumulative_qty = cumulative_qty
                .checked_add(quantity)
                .ok_or_else(|| QxError::ReconcileRequired("CCXT exact 成交数量累计溢出".into()))?;
            if let Some(cost) = quote_cost {
                if !cost.is_positive() {
                    return Err(QxError::ReconcileRequired(format!(
                        "CCXT exact trade {trade_id} 的计价币成本非法"
                    )));
                }
                cumulative_cost = cumulative_cost.checked_add(cost).ok_or_else(|| {
                    QxError::ReconcileRequired("CCXT exact 成交计价币成本累计溢出".into())
                })?;
            } else if quote_budget.is_some() {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT quote-budget trade {trade_id} 缺少 exact cost"
                )));
            }

            let mut exact_fees = parse_exact_fees(trade)?;
            if exact_fees.is_empty() {
                // Retain the trade identity even when the venue reports no commission so a
                // restart can distinguish a repeated trade from a new fill at the same tick.
                exact_fees.push(ExactFillFee {
                    amount: ExactDecimal::ZERO,
                    currency: None,
                    source_trade_ids: vec![trade_id.clone()],
                });
            } else {
                for fee in &mut exact_fees {
                    if fee.amount.is_zero() {
                        fee.source_trade_ids.push(trade_id.clone());
                        continue;
                    }
                    if fee
                        .currency
                        .as_deref()
                        .is_none_or(|currency| currency.trim().is_empty())
                    {
                        return Err(QxError::ReconcileRequired(format!(
                            "CCXT exact trade {trade_id} 的非零手续费缺少币种"
                        )));
                    }
                    fee.source_trade_ids.push(trade_id.clone());
                }
            }
            for fee in &exact_fees {
                if fee.amount.is_zero() {
                    continue;
                }
                let currency = fee
                    .currency
                    .as_deref()
                    .map(str::trim)
                    .filter(|currency| !currency.is_empty())
                    .ok_or_else(|| {
                        QxError::ReconcileRequired(format!(
                            "CCXT exact trade {trade_id} 的手续费币种为空"
                        ))
                    })?
                    .to_ascii_uppercase();
                let total = cumulative_fees
                    .entry(currency)
                    .or_insert(ExactDecimal::ZERO);
                *total = total.checked_add(fee.amount).ok_or_else(|| {
                    QxError::ReconcileRequired("CCXT exact 手续费累计溢出".into())
                })?;
            }
            if prior_seen.contains(&trade_id) {
                prior_qty = prior_qty.checked_add(quantity).ok_or_else(|| {
                    QxError::ReconcileRequired("CCXT 已记 exact 成交数量累计溢出".into())
                })?;
                if let Some(cost) = quote_cost {
                    prior_cost = prior_cost.checked_add(cost).ok_or_else(|| {
                        QxError::ReconcileRequired("CCXT 已记 exact 成交成本累计溢出".into())
                    })?;
                }
            } else {
                let mut fill = Fill {
                    order_id: client_order_id,
                    ts: trade_ts,
                    account_id: order.account_id.clone(),
                    venue_id: Some(self.id.clone()),
                    venue_order_id: Some(remote_id.to_string()),
                    exact_fees,
                    exact_amounts: Some(ExactFillAmounts {
                        quantity,
                        price,
                        quote_cost,
                        quote_budget_complete: false,
                    }),
                    ..Fill::default()
                };
                order.trace_fill(&mut fill, Some(&self.id), Some(remote_id));
                pending_fills.push((trade_id, fill));
            }
        }

        if cumulative_qty != filled || prior_qty != prior_filled {
            return Err(QxError::ReconcileRequired(format!(
                "CCXT exact trade history does not reconcile: total={} local_prior={} remote_filled={} local_filled={}",
                cumulative_qty, prior_qty, filled, prior_filled
            )));
        }
        if let Some(budget) = quote_budget {
            if prior_cost != local_amounts.quote_filled {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT quote-budget trade history 与本地计价币累计花费不一致: trade_prior={prior_cost} local_prior={}",
                    local_amounts.quote_filled
                )));
            }
            if cumulative_cost
                .checked_cmp(budget)
                .is_none_or(|ordering| ordering.is_gt())
            {
                return Err(QxError::ReconcileRequired(
                    "CCXT quote-budget 成交总花费超过订单预算".into(),
                ));
            }
            if let Some(remote_cost) = remote
                .get("cost_exact")
                .and_then(Value::as_str)
                .map(ExactDecimal::parse)
                .transpose()
                .map_err(|error| {
                    QxError::ReconcileRequired(format!(
                        "CCXT quote-budget 订单累计成本非法: {error}"
                    ))
                })?
            {
                if remote_cost != cumulative_cost {
                    return Err(QxError::ReconcileRequired(
                        "CCXT quote-budget 订单累计成本与逐笔成交不一致".into(),
                    ));
                }
            }
        }

        let reported_fees = parse_exact_fees(remote)?;
        if !reported_fees.is_empty() {
            let mut order_fee_totals = BTreeMap::new();
            add_exact_fee_rows(&mut order_fee_totals, &reported_fees)?;
            if order_fee_totals != cumulative_fees {
                return Err(QxError::ReconcileRequired(
                    "CCXT exact 订单累计费用与逐笔成交费用不一致".into(),
                ));
            }
        }
        let prior_fees = self
            .exact_cumulative_fees
            .get(&client_order_id)
            .cloned()
            .unwrap_or_default();
        for (currency, prior) in &prior_fees {
            if cumulative_fees
                .get(currency)
                .and_then(|current| current.checked_cmp(*prior))
                .is_none_or(|ordering| ordering.is_lt())
            {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT exact 成交手续费历史低于持久基线: {currency}"
                )));
            }
        }
        for (currency, current) in &cumulative_fees {
            let prior = prior_fees
                .get(currency)
                .copied()
                .unwrap_or(ExactDecimal::ZERO);
            let delta = current
                .checked_sub(prior)
                .ok_or_else(|| QxError::ReconcileRequired("CCXT exact 手续费差值溢出".into()))?;
            let unseen_trade_fee =
                pending_fills
                    .iter()
                    .try_fold(ExactDecimal::ZERO, |total, (_, fill)| {
                        let trade_total = fill
                            .exact_fees
                            .iter()
                            .filter(|fee| {
                                fee.currency.as_deref().is_some_and(|fee_currency| {
                                    fee_currency.eq_ignore_ascii_case(currency)
                                })
                            })
                            .try_fold(ExactDecimal::ZERO, |subtotal, fee| {
                                subtotal.checked_add(fee.amount)
                            })?;
                        total.checked_add(trade_total)
                    });
            if unseen_trade_fee != Some(delta) {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT exact trade fees changed outside unseen fills: {currency}"
                )));
            }
        }
        for currency in prior_fees.keys() {
            if !cumulative_fees.contains_key(currency)
                && prior_fees
                    .get(currency)
                    .is_some_and(|amount| !amount.is_zero())
            {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT exact 手续费历史缺少已记币种: {currency}"
                )));
            }
        }

        if (filled != prior_filled
            || (quote_budget.is_some() && local_amounts.quote_filled != cumulative_cost))
            && order.status.is_terminal()
        {
            return Err(QxError::ReconcileRequired(format!(
                "CCXT exact 订单 {client_order_id} 本地已终态 {:?}，但远端成交量已变化",
                order.status
            )));
        }
        let mut events = pending_fills
            .into_iter()
            .map(|(_, fill)| VenueEvent::Fill(fill))
            .collect::<Vec<_>>();
        let mut next = order.clone();
        next.exact_amounts = Some(ExactOrderAmounts {
            filled,
            quote_filled: if quote_budget.is_some() {
                cumulative_cost
            } else {
                local_amounts.quote_filled
            },
            ..local_amounts
        });
        let quote_fully_spent = quote_budget.is_some_and(|budget| {
            cumulative_cost
                .checked_cmp(budget)
                .is_some_and(|ordering| ordering.is_eq())
        });
        let venue_terminal_success = matches!(remote_status.as_str(), "closed" | "filled" | "done");
        let quote_partial_closed = quote_budget.is_some()
            && matches!(
                remote_status.as_str(),
                "closed" | "filled" | "done" | "canceled" | "cancelled" | "expired"
            )
            && !quote_fully_spent;
        if quote_fully_spent && !venue_terminal_success {
            return Err(QxError::ReconcileRequired(
                "CCXT quote-budget 订单已花完预算但远端仍显示未完成".into(),
            ));
        }
        if fully_filled || quote_fully_spent {
            next.status = OrderStatus::Filled;
        } else if quote_partial_closed
            || matches!(remote_status.as_str(), "canceled" | "cancelled" | "expired")
        {
            if !order.status.is_terminal() {
                events.push(VenueEvent::Cancelled {
                    client_order_id,
                    ts,
                });
            }
            next.status = OrderStatus::Cancelled;
        } else if !filled_is_zero && next.status != OrderStatus::CancelPending {
            next.status = OrderStatus::PartiallyFilled;
        }
        self.exact_cumulative_fees
            .insert(client_order_id, cumulative_fees);
        let trade_ids = self.seen_trade_ids.entry(client_order_id).or_default();
        trade_ids.extend(seen);
        self.orders.insert(client_order_id, next);
        Ok(events)
    }
}

fn exact_decimal_field(value: &Value, field: &str) -> QxResult<ExactDecimal> {
    let text = value.get(field).and_then(Value::as_str).ok_or_else(|| {
        QxError::ReconcileRequired(format!("CCXT 响应缺少精确十进制字符串 {field}"))
    })?;
    ExactDecimal::parse(text).map_err(|error| {
        QxError::ReconcileRequired(format!("CCXT 精确十进制字段 {field} 非法: {error}"))
    })
}

fn parse_exact_fees(value: &Value) -> QxResult<Vec<ExactFillFee>> {
    let rows = value
        .get("exact_fees")
        .and_then(Value::as_array)
        .ok_or_else(|| QxError::ReconcileRequired("CCXT 响应缺少 exact_fees 数组".into()))?;
    rows.iter()
        .map(|row| {
            let amount = exact_decimal_field(row, "amount")?;
            let currency = row
                .get("currency")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .map(str::to_ascii_uppercase);
            Ok(ExactFillFee {
                amount,
                currency,
                source_trade_ids: Vec::new(),
            })
        })
        .collect()
}

fn add_exact_fee_rows(
    totals: &mut BTreeMap<String, ExactDecimal>,
    fees: &[ExactFillFee],
) -> QxResult<()> {
    for fee in fees {
        if fee.amount.is_zero() {
            continue;
        }
        let currency = fee
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|currency| !currency.is_empty())
            .ok_or_else(|| QxError::ReconcileRequired("CCXT exact 手续费缺少币种".into()))?
            .to_ascii_uppercase();
        let total = totals.entry(currency).or_insert(ExactDecimal::ZERO);
        *total = total
            .checked_add(fee.amount)
            .ok_or_else(|| QxError::ReconcileRequired("CCXT exact 手续费累计溢出".into()))?;
    }
    Ok(())
}
