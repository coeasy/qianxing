use super::{StrategyContext, StrategyDecision, STRATEGY_API_VERSION};
use qx_core::{Order, OrderStatus, OrderTrace, Quantity};

impl StrategyDecision {
    pub fn validate_for(&self, context: &StrategyContext, event_ts: u64) -> Result<(), String> {
        context.validate()?;
        if self.schema_version != STRATEGY_API_VERSION
            || self.request_id.trim().is_empty()
            || self.strategy_id != context.strategy_id
            || self.signal_id == 0
            || self.intents.iter().any(|intent| intent.validate().is_err())
            || (self.expires_at != 0 && self.expires_at < event_ts)
        {
            return Err("StrategyDecision 身份、版本、有效期或订单意图非法".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for intent in &self.intents {
            intent.validate()?;
            if !ids.insert(intent.intent_id) {
                return Err("StrategyDecision 存在重复 intent_id".into());
            }
        }
        Ok(())
    }

    /// 将决定转换为核心订单；该函数只做协议转换，调用方仍必须执行
    /// RiskGate/OMS 校验，不能把返回值视为已提交订单。
    pub fn to_orders(&self, context: &StrategyContext) -> Result<Vec<Order>, String> {
        let mut orders = Vec::with_capacity(self.intents.len());
        self.append_orders(context, &mut orders)?;
        Ok(orders)
    }

    /// Append this decision's orders to a caller-owned buffer without creating an
    /// intermediate `Vec`. On failure, the buffer is restored to its original length.
    pub fn append_orders(
        &self,
        context: &StrategyContext,
        orders: &mut Vec<Order>,
    ) -> Result<(), String> {
        self.validate_for(context, context.as_of)?;
        let original_len = orders.len();
        for intent in &self.intents {
            let mut policy = intent.policy.unwrap_or_default();
            policy.reduce_only = intent.reduce_only;
            policy.post_only = intent.post_only;
            let exact_amounts = intent
                .qty_exact
                .map(|quantity| qx_core::ExactOrderAmounts {
                    quantity,
                    limit_price: intent.limit_price_exact,
                    filled: qx_core::ExactDecimal::ZERO,
                    quote_budget: None,
                    quote_filled: qx_core::ExactDecimal::ZERO,
                    market_reference_price: None,
                })
                .or_else(|| {
                    intent
                        .quote_budget
                        .map(|budget| qx_core::ExactOrderAmounts {
                            quantity: qx_core::ExactDecimal::ZERO,
                            limit_price: None,
                            filled: qx_core::ExactDecimal::ZERO,
                            quote_budget: Some(budget),
                            quote_filled: qx_core::ExactDecimal::ZERO,
                            market_reference_price: None,
                        })
                });
            let order = Order {
                client_id: intent.intent_id,
                instrument: intent.instrument.clone(),
                side: intent.side,
                qty: intent.qty,
                limit: intent.limit,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: context.account_id.clone(),
                trace: Some(OrderTrace {
                    strategy_id: Some(context.strategy_id.clone()),
                    signal_id: Some(self.signal_id),
                    intent_id: Some(intent.intent_id),
                    rule_version: Some(context.strategy_version.clone()),
                }),
                policy: Some(policy),

                exact_amounts,
            };
            if let Err(error) = order.validate() {
                orders.truncate(original_len);
                return Err(format!("StrategyDecision 转核心订单失败: {error}"));
            }
            orders.push(order);
        }
        Ok(())
    }
}
