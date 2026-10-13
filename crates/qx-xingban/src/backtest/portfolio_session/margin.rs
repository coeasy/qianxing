use super::runtime::RuntimeMarket;
use qx_core::{InstrumentId, Ledger, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

pub(super) fn other_market_margin(
    ledger: &Ledger,
    account_id: &str,
    target: &InstrumentId,
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<InstrumentId, Price>,
    markets: &BTreeMap<InstrumentId, RuntimeMarket<'_>>,
) -> Result<i128, qx_core::QxError> {
    let mut total = 0_i128;
    for (instrument, spec) in specs {
        if instrument == target || !spec.product.supports_leverage() {
            continue;
        }
        let mark = marks.get(instrument).ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "共享账户保证金估值缺少标记价 {instrument}"
            ))
        })?;
        let net_qty = ledger.position_for(account_id, instrument).quantity.raw();
        let long_qty = ledger
            .position_for_side(account_id, instrument, qx_core::PositionSide::Long)
            .quantity
            .raw();
        let short_qty = ledger
            .position_for_side(account_id, instrument, qx_core::PositionSide::Short)
            .quantity
            .raw();
        let qty = if long_qty != 0 || short_qty != 0 {
            long_qty
                .checked_abs()
                .and_then(|long| {
                    short_qty
                        .checked_abs()
                        .and_then(|short| long.checked_add(short))
                })
                .ok_or_else(|| {
                    qx_core::QxError::Invariant("共享账户 hedge 保证金数量溢出".into())
                })?
        } else {
            net_qty
                .checked_abs()
                .ok_or_else(|| qx_core::QxError::Invariant("共享账户保证金持仓数量溢出".into()))?
        };
        if qty == 0 {
            continue;
        }
        let margin = markets
            .get(instrument)
            .expect("spec belongs to a market")
            .margin
            .instrument_initial_margin(spec, qty, mark.raw().abs(), 1)?;
        total = total
            .checked_add(margin)
            .ok_or_else(|| qx_core::QxError::Invariant("共享账户保证金合计溢出".into()))?;
    }
    Ok(total)
}
