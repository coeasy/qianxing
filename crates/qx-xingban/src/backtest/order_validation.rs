//! Shared order-level checks used by the Bar backtest execution path.

use qx_core::{InstrumentId, Ledger, Order, Side};

pub(super) fn reduce_only_order_allowed(
    ledger: &Ledger,
    account_id: &str,
    instrument: &InstrumentId,
    order: &Order,
    order_qty: i128,
) -> bool {
    let policy = order.policy.unwrap_or_default();
    let current = if policy.position_mode == qx_core::PositionMode::Hedge {
        ledger
            .position_for_side(account_id, instrument, policy.position_side)
            .quantity
            .raw()
    } else {
        ledger.position_for(account_id, instrument).quantity.raw()
    };
    if current == 0 || order_qty <= 0 {
        return false;
    }
    let delta = match order.side {
        Side::Buy => order_qty,
        Side::Sell => -order_qty,
    };
    current.signum() != delta.signum() && order_qty <= current.saturating_abs()
}
