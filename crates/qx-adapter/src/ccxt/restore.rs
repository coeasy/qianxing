use super::*;

impl CcxtProcessVenue {
    pub fn restore_order(&mut self, order: Order, remote_id: impl Into<String>) -> QxResult<()> {
        self.restore_order_with_fee_state(order, remote_id, BTreeMap::new(), BTreeSet::new())
    }

    pub fn restore_order_with_fee_baseline(
        &mut self,
        order: Order,
        remote_id: impl Into<String>,
        fee_baseline: BTreeMap<String, i128>,
    ) -> QxResult<()> {
        self.restore_order_with_fee_state(order, remote_id, fee_baseline, BTreeSet::new())
    }

    pub fn restore_order_with_fee_state(
        &mut self,
        order: Order,
        remote_id: impl Into<String>,
        fee_baseline: BTreeMap<String, i128>,
        seen_trade_ids: BTreeSet<String>,
    ) -> QxResult<()> {
        self.restore_order_with_exact_fee_state(
            order,
            remote_id,
            fee_baseline,
            BTreeMap::new(),
            seen_trade_ids,
        )
    }

    pub fn restore_order_with_exact_fee_state(
        &mut self,
        order: Order,
        remote_id: impl Into<String>,
        fee_baseline: BTreeMap<String, i128>,
        exact_fee_baseline: BTreeMap<String, ExactDecimal>,
        seen_trade_ids: BTreeSet<String>,
    ) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)?;
        let remote_id = remote_id.into();
        if remote_id.trim().is_empty() {
            return Err(QxError::ReconcileRequired(
                "CCXT 恢复订单缺少 remote order id".into(),
            ));
        }
        let client_id = order.client_id;
        let fee_baseline = fee_baseline
            .into_iter()
            .map(|(currency, amount)| {
                let currency = currency.trim().to_ascii_uppercase();
                if currency.is_empty() {
                    Err(QxError::ReconcileRequired(
                        "CCXT 恢复手续费基线缺少币种".into(),
                    ))
                } else {
                    Ok((currency, amount))
                }
            })
            .collect::<QxResult<BTreeMap<_, _>>>()?;
        let exact_fee_baseline = exact_fee_baseline
            .into_iter()
            .map(|(currency, amount)| {
                let currency = currency.trim().to_ascii_uppercase();
                if currency.is_empty() {
                    Err(QxError::ReconcileRequired(
                        "CCXT 恢复精确手续费基线缺少币种".into(),
                    ))
                } else {
                    Ok((currency, amount))
                }
            })
            .collect::<QxResult<BTreeMap<_, _>>>()?;
        let seen_trade_ids = seen_trade_ids
            .into_iter()
            .map(|trade_id| {
                let trade_id = trade_id.trim().to_string();
                if trade_id.is_empty() {
                    Err(QxError::ReconcileRequired(
                        "CCXT 恢复成交费用去重状态包含空 trade ID".into(),
                    ))
                } else {
                    Ok(trade_id)
                }
            })
            .collect::<QxResult<BTreeSet<_>>>()?;
        if let Some(existing) = self.orders.get(&client_id) {
            if existing != &order || self.remote_ids.get(&client_id) != Some(&remote_id) {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 恢复订单 {client_id} 与本地订单身份冲突"
                )));
            }
            self.cumulative_fees
                .entry(client_id)
                .or_insert(fee_baseline);
            self.exact_cumulative_fees
                .entry(client_id)
                .or_insert(exact_fee_baseline);
            self.seen_trade_ids
                .entry(client_id)
                .or_insert(seen_trade_ids);
            return Ok(());
        }
        if self.remote_ids.contains_key(&client_id)
            || self
                .remote_ids
                .values()
                .any(|existing| existing == &remote_id)
        {
            return Err(QxError::ReconcileRequired(
                "CCXT 恢复订单 remote order id 已绑定到其他身份".into(),
            ));
        }
        self.remote_ids.insert(client_id, remote_id);
        self.cumulative_fees.insert(client_id, fee_baseline);
        self.exact_cumulative_fees
            .insert(client_id, exact_fee_baseline);
        self.seen_trade_ids.insert(client_id, seen_trade_ids);
        self.orders.insert(client_id, order);
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(())
    }
}
