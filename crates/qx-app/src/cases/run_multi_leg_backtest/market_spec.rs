//! Contract identity checks for both legs of a multi-leg run.

use super::invalid;
use crate::AppError;
use qx_core::{InstrumentId, TradingInstrumentSpec};

pub(in crate::cases) fn validate_leg_spec<'a>(
    label: &str,
    market_spec: Option<&'a TradingInstrumentSpec>,
    instrument: &InstrumentId,
    settlement_currency: &str,
) -> Result<Option<&'a TradingInstrumentSpec>, AppError> {
    let Some(market) = market_spec else {
        return Ok(None);
    };
    if market.instrument != *instrument {
        return Err(invalid(format!(
            "{label} market spec instrument {} 与 BarFrame 标的 {instrument} 不一致",
            market.instrument
        )));
    }
    if market.settlement_currency != settlement_currency {
        return Err(invalid(format!(
            "{label} market spec 结算币 {} 与 spec settlement_currency {settlement_currency} 不一致",
            market.settlement_currency
        )));
    }
    Ok(Some(market))
}

pub(in crate::cases) fn execution_market_spec(
    declared: Option<&TradingInstrumentSpec>,
    instrument: &InstrumentId,
    settlement_currency: &str,
) -> TradingInstrumentSpec {
    declared
        .cloned()
        .unwrap_or_else(|| TradingInstrumentSpec::spot_default(instrument, settlement_currency))
}
