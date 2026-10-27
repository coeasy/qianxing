use super::{Ledger, PositionState};
use crate::error::{QxError, QxResult};
use crate::numeric::{DecimalRounding, ExactDecimal, Price, SCALE};
use crate::trading::TradingInstrumentSpec;
use std::collections::BTreeMap;

impl Ledger {
    pub(super) fn convert_equity_currency_exact(
        amount: ExactDecimal,
        currency: &str,
        reporting_currency: &str,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<ExactDecimal> {
        if currency
            .trim()
            .eq_ignore_ascii_case(reporting_currency.trim())
        {
            return Ok(amount);
        }
        let currency_key = currency.trim().to_ascii_uppercase();
        let rate = fx_rates.get(&currency_key).ok_or_else(|| {
            QxError::ReconcileRequired(format!("缺少 {currency}->{reporting_currency} 抵押品汇率"))
        })?;
        if rate.raw() <= 0 {
            return Err(QxError::ReconcileRequired(format!(
                "{currency}->{reporting_currency} 抵押品汇率必须为正"
            )));
        }
        let exact_rate = ExactDecimal::from_parts(rate.raw(), 9)
            .map_err(|error| QxError::Invariant(error.into()))?;
        amount
            .checked_mul_at_scale(
                exact_rate,
                ExactDecimal::MAX_SCALE,
                DecimalRounding::TowardZero,
            )
            .ok_or_else(|| QxError::Invariant("精确跨币种权益换算溢出".into()))
    }

    pub(super) fn ensure_only_instrument_is_valued(
        &self,
        account_id: &str,
        instrument: &crate::InstrumentId,
    ) -> QxResult<()> {
        let has_uncovered_position =
            self.iter_positions()
                .any(|((account, held_instrument), position)| {
                    account == account_id
                        && held_instrument != instrument
                        && !position.quantity.is_zero()
                })
                || self
                    .iter_hedge_positions()
                    .any(|((account, held_instrument, _), position)| {
                        account == account_id
                            && held_instrument != instrument
                            && !position.quantity.is_zero()
                    });
        if has_uncovered_position {
            return Err(QxError::ReconcileRequired(
                "单市场估值发现同账户其他非零持仓；必须提供完整 market_specs 后再估值".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn convert_equity_currency(
        amount: i128,
        currency: &str,
        reporting_currency: &str,
        fx_rates: &BTreeMap<String, Price>,
    ) -> QxResult<i128> {
        let currency_key = currency.trim().to_ascii_uppercase();
        let rate = if currency_key.eq_ignore_ascii_case(reporting_currency.trim()) {
            SCALE
        } else {
            fx_rates
                .get(&currency_key)
                .ok_or_else(|| {
                    QxError::ReconcileRequired(format!(
                        "缺少 {currency}->{reporting_currency} 抵押品汇率"
                    ))
                })?
                .raw()
        };
        if rate <= 0 {
            return Err(QxError::ReconcileRequired(format!(
                "{currency}->{reporting_currency} 抵押品汇率必须为正"
            )));
        }
        amount
            .checked_mul(rate)
            .and_then(|value| value.checked_div(SCALE))
            .ok_or_else(|| QxError::Invariant("跨币种权益换算溢出".into()))
    }

    pub(super) fn market_position_value<'a>(
        spec: &'a TradingInstrumentSpec,
        position: &PositionState,
        mark: &Price,
    ) -> QxResult<(i128, &'a str)> {
        match spec.product {
            crate::TradingProduct::Spot => {
                let quantity = position
                    .quantity
                    .raw()
                    .checked_abs()
                    .ok_or_else(|| QxError::Invariant("现货持仓绝对值溢出".into()))?;
                let notional = spec.notional(quantity, mark.raw())?;
                Ok((
                    if position.quantity.raw() < 0 {
                        -notional
                    } else {
                        notional
                    },
                    &spec.quote_currency,
                ))
            }
            crate::TradingProduct::Margin
            | crate::TradingProduct::Perpetual
            | crate::TradingProduct::Future => Ok((
                spec.unrealized_pnl(
                    position.quantity.raw(),
                    position.average_entry.raw(),
                    mark.raw(),
                )?,
                &spec.settlement_currency,
            )),
            crate::TradingProduct::Option => Err(QxError::BusinessViolation(
                "多市场估值暂不支持期权：期权合约身份尚未建模".into(),
            )),
        }
    }

    pub(super) fn market_position_value_exact<'a>(
        spec: &'a TradingInstrumentSpec,
        position: &PositionState,
        mark: ExactDecimal,
    ) -> QxResult<(ExactDecimal, &'a str)> {
        let quantity = position.exact_quantity.unwrap_or_else(|| {
            ExactDecimal::from_parts(position.quantity.raw(), 9).expect("legacy scale is supported")
        });
        if quantity.is_zero() {
            return Ok((
                ExactDecimal::ZERO,
                match spec.product {
                    crate::TradingProduct::Spot => &spec.quote_currency,
                    _ => &spec.settlement_currency,
                },
            ));
        }
        let average_entry = position.exact_average_entry.unwrap_or_else(|| {
            ExactDecimal::from_parts(position.average_entry.raw(), 9)
                .expect("legacy scale is supported")
        });
        match spec.product {
            crate::TradingProduct::Spot => {
                let positive_quantity = if quantity.is_negative() {
                    quantity
                        .checked_neg()
                        .ok_or_else(|| QxError::Invariant("精确现货持仓绝对值溢出".into()))?
                } else {
                    quantity
                };
                let value = spec.notional_exact(positive_quantity, mark)?;
                Ok((
                    if quantity.is_negative() {
                        value
                            .checked_neg()
                            .ok_or_else(|| QxError::Invariant("精确现货持仓市值取负溢出".into()))?
                    } else {
                        value
                    },
                    &spec.quote_currency,
                ))
            }
            crate::TradingProduct::Margin
            | crate::TradingProduct::Perpetual
            | crate::TradingProduct::Future => Ok((
                spec.unrealized_pnl_exact(quantity, average_entry, mark)?,
                &spec.settlement_currency,
            )),
            crate::TradingProduct::Option => Err(QxError::BusinessViolation(
                "多市场估值暂不支持期权：期权合约身份尚未建模".into(),
            )),
        }
    }

    pub(super) fn position_pnl(
        position: &PositionState,
        mark: &Price,
        multiplier: i128,
    ) -> Option<i128> {
        mark.raw()
            .checked_sub(position.average_entry.raw())?
            .checked_mul(position.quantity.raw())?
            .checked_div(SCALE)?
            .checked_mul(multiplier)
    }
}
