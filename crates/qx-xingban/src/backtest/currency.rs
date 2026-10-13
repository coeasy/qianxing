use qx_core::{QxError, TradingInstrumentSpec, TradingProduct};

pub(super) fn normalize_and_validate(
    currency: String,
    instrument: &qx_core::InstrumentId,
    spec: Option<&TradingInstrumentSpec>,
) -> Result<String, QxError> {
    let currency = currency.trim().to_ascii_uppercase();
    if currency.is_empty() {
        return Err(QxError::BusinessViolation("回测账户结算币不能为空".into()));
    }
    if let Some(spec) = spec {
        if spec.instrument != *instrument {
            return Err(QxError::BusinessViolation(
                "回测产品规格 instrument 与配置不一致".into(),
            ));
        }
        spec.validate()?;
        let cash_currency = if spec.product == TradingProduct::Spot {
            &spec.quote_currency
        } else {
            &spec.settlement_currency
        };
        if !cash_currency.eq_ignore_ascii_case(&currency) {
            return Err(QxError::BusinessViolation(format!(
                "回测账户币种 {currency} 与 {:?} 市场现金/结算币种 {cash_currency} 不一致",
                spec.product,
            )));
        }
    }
    Ok(currency)
}
