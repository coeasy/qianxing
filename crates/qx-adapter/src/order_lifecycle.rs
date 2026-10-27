//! Binance 与 CCXT 共用的订单生命周期规则。
//!
//! 这里只承载协议无关的订单规格校验、提交状态判定和成交身份首次登记；
//! remote id、累计成本和 venue 回报映射仍由各协议适配器负责。

use qx_core::{Order, OrderStatus, QxError, QxResult};
use std::collections::BTreeSet;

pub(crate) trait OrderLifecycle {
    fn validate_order_spec(order: &Order) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)
    }

    fn can_submit(order: &Order) -> bool {
        matches!(
            order.status,
            OrderStatus::PendingSubmit | OrderStatus::Submitted
        )
    }

    /// Cancellation is only safe when local state is known and nonterminal.
    /// Adapters keep their terminal no-op behavior, then use this rule to fail
    /// closed on `Unknown` before issuing a remote cancel request.
    fn can_cancel(order: &Order) -> bool {
        !order.status.is_terminal() && order.status != OrderStatus::Unknown
    }

    fn validate_cancel_state(order: &Order, venue: &str) -> QxResult<()> {
        if Self::can_cancel(order) {
            Ok(())
        } else {
            Err(QxError::ReconcileRequired(format!(
                "未知订单状态不能直接撤 {venue} 订单"
            )))
        }
    }

    /// 返回 true 表示成交身份首次出现；false 表示应按重复回报处理。
    fn remember_fill<K: Ord>(ledger: &mut BTreeSet<K>, key: K) -> bool {
        ledger.insert(key)
    }

    fn fill_seen<K: Ord>(ledger: &BTreeSet<K>, key: &K) -> bool {
        ledger.contains(key)
    }

    fn reconciliation_trigger(error: &QxError) -> bool {
        error.contract().reconcile_required
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Price, Quantity, Side};

    struct TestVenue;
    impl OrderLifecycle for TestVenue {}

    fn order(status: OrderStatus) -> Order {
        Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(1)),
            status,
            filled: Quantity::ZERO,
            account_id: "acct".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }
    }

    #[test]
    fn submission_rule_accepts_only_pending_or_submitted_orders() {
        assert!(TestVenue::validate_order_spec(&order(OrderStatus::PendingSubmit)).is_ok());

        let mut invalid = order(OrderStatus::PendingSubmit);
        invalid.qty = Quantity::ZERO;
        assert!(matches!(
            TestVenue::validate_order_spec(&invalid),
            Err(QxError::BusinessViolation(message)) if message == "订单数量必须为正"
        ));

        assert!(TestVenue::can_submit(&order(OrderStatus::PendingSubmit)));
        assert!(TestVenue::can_submit(&order(OrderStatus::Submitted)));
        assert!(!TestVenue::can_submit(&order(OrderStatus::Working)));
    }

    #[test]
    fn common_order_lifecycle_accepts_well_formed_exact_values() {
        use qx_core::{ExactDecimal, ExactOrderAmounts};

        let mut order = order(OrderStatus::PendingSubmit);
        order.qty = Quantity::ZERO;
        order.limit = None;
        order.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::ZERO,
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        assert!(TestVenue::validate_order_spec(&order).is_ok());
    }

    #[test]
    fn fill_identity_is_recorded_once() {
        let mut ledger = BTreeSet::new();
        assert!(TestVenue::remember_fill(&mut ledger, (7_u64, 3_u64)));
        assert!(!TestVenue::remember_fill(&mut ledger, (7_u64, 3_u64)));
    }

    #[test]
    fn reconciliation_trigger_follows_the_shared_error_contract() {
        assert!(TestVenue::reconciliation_trigger(&QxError::Ambiguous(
            "unknown".into()
        )));
        assert!(TestVenue::reconciliation_trigger(
            &QxError::ReconcileRequired("drift".into())
        ));
        assert!(!TestVenue::reconciliation_trigger(&QxError::Permanent(
            "rejected".into()
        )));
        assert!(!TestVenue::reconciliation_trigger(
            &QxError::ResourceExhausted("rate limited".into())
        ));
    }
}
