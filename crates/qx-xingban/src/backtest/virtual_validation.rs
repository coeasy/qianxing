use super::VirtualTradingConfig;

pub(super) fn validate_virtual_events(
    config: &VirtualTradingConfig,
) -> Result<(), qx_core::QxError> {
    if config.liquidation_fee_bp < 0 {
        return Err(qx_core::QxError::BusinessViolation(
            "强平费用基点不能为负".into(),
        ));
    }
    if config
        .delivery
        .iter()
        .any(|event| event.settlement_price.raw() <= 0)
    {
        return Err(qx_core::QxError::BusinessViolation(
            "交割结算价必须为正".into(),
        ));
    }
    if config
        .fx_rates
        .iter()
        .any(|(currency, rate)| currency.trim().is_empty() || rate.raw() <= 0)
    {
        return Err(qx_core::QxError::BusinessViolation(
            "跨币种抵押品汇率必须有非空币种且为正".into(),
        ));
    }
    if config.collateral.values().any(|amount| amount.raw() < 0)
        || config
            .collateral
            .keys()
            .any(|currency| currency.trim().is_empty())
    {
        return Err(qx_core::QxError::BusinessViolation(
            "初始抵押品币种不能为空且金额不能为负".into(),
        ));
    }
    for events in [
        config
            .funding
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
        config
            .interest
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
        config
            .delivery
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
    ] {
        if events.windows(2).any(|window| window[0] > window[1]) {
            return Err(qx_core::QxError::BusinessViolation(
                "虚拟交易事件必须按 ts 非递减配置".into(),
            ));
        }
    }
    Ok(())
}
