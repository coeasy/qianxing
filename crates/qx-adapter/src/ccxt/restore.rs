use super::*;

impl CcxtProcessVenue {
    pub fn restore_order(&mut self, order: Order, remote_id: impl Into<String>) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)?;
        let remote_id = remote_id.into();
        if remote_id.trim().is_empty() {
            return Err(QxError::ReconcileRequired(
                "CCXT 恢复订单缺少 remote order id".into(),
            ));
        }
        let client_id = order.client_id;
        if let Some(existing) = self.orders.get(&client_id) {
            if existing != &order || self.remote_ids.get(&client_id) != Some(&remote_id) {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 恢复订单 {client_id} 与本地订单身份冲突"
                )));
            }
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
        self.orders.insert(client_id, order);
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(())
    }
}
