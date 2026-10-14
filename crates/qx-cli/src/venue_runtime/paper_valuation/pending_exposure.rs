use crate::*;

pub(super) fn pending_order_gross_notional(
    orders: &[Order],
    current_order: &Order,
    account_id: &str,
    spec: &TradingInstrumentSpec,
    marks: &BTreeMap<InstrumentId, Price>,
) -> Result<i128, String> {
    orders
        .iter()
        .filter(|pending| {
            pending.client_id != current_order.client_id
                && pending.account_id == account_id
                && pending.instrument == spec.instrument
                && !pending.status.is_terminal()
                && pending.remaining().raw() > 0
        })
        .try_fold(0_i128, |total, pending| {
            let price = pending
                .limit
                .or_else(|| marks.get(&pending.instrument).copied())
                .ok_or_else(|| {
                    format!(
                        "FAIL_CLOSED: 在途订单 {} 缺少名义额风控参考价",
                        pending.client_id
                    )
                })?;
            let pending_notional = spec
                .notional(pending.remaining().raw(), price.raw())
                .map_err(|error| {
                    format!("计算在途订单 {} 的名义额失败: {error:?}", pending.client_id)
                })?;
            total
                .checked_add(pending_notional)
                .ok_or_else(|| "在途订单名义额合计溢出".to_string())
        })
}
