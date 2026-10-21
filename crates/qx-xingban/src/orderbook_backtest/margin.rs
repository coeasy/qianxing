/// Spot strategies see settled cash. Derivative strategies see a backtest estimate built
/// from marked equity and the same portfolio exposure facts used by order admission.
pub(super) fn strategy_available_margin_raw(
    spec: Option<&qx_core::TradingInstrumentSpec>,
    reference_price: Option<qx_core::Price>,
    settled_cash_raw: i128,
    fee_bps: i128,
    ledger: &qx_core::Ledger,
    account_id: &str,
    open_orders: &[qx_core::Order],
) -> Result<Option<i128>, qx_core::QxError> {
    let Some(spec) = spec.filter(|spec| spec.product.supports_leverage()) else {
        return Ok(Some(settled_cash_raw.max(0)));
    };
    let Some(reference_price) = reference_price else {
        return Ok(None);
    };
    super::funding::simulated_available_margin_raw(
        spec,
        reference_price,
        fee_bps,
        ledger,
        account_id,
        open_orders,
    )
    .map(Some)
}
