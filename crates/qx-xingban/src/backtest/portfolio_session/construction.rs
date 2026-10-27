use super::{PortfolioBarEngine, PortfolioBarMarket, PortfolioBarSessionConfig};
use crate::fill::DataTier;
use qx_guanxing::{QualityGate, Verdict};
use std::collections::BTreeSet;

impl<'a> PortfolioBarEngine<'a> {
    pub fn new(
        config: PortfolioBarSessionConfig,
        markets: Vec<PortfolioBarMarket<'a>>,
    ) -> Result<Self, qx_core::QxError> {
        if markets.is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 至少需要一个市场".into(),
            ));
        }
        if config.account_id.trim().is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 账户身份和初始资金必须有效".into(),
            ));
        }
        let initial_cash_exact = match config.initial_cash_exact {
            Some(amount) => amount,
            None if config.initial_cash.raw() > 0 => {
                qx_core::ExactDecimal::from_parts(config.initial_cash.raw(), 9)
                    .map_err(|error| qx_core::QxError::BusinessViolation(error.to_string()))?
            }
            None => {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享 Bar session 账户身份和初始资金必须有效".into(),
                ));
            }
        };
        let projected_raw = initial_cash_exact
            .checked_mul_at_scale(
                qx_core::ExactDecimal::ONE,
                9,
                qx_core::DecimalRounding::TowardZero,
            )
            .and_then(|value| value.to_raw(9).ok());
        if !initial_cash_exact.is_positive() || projected_raw != Some(config.initial_cash.raw()) {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session initial_cash_exact 必须为正，且与 initial_cash 的向零投影一致"
                    .into(),
            ));
        }
        let currency = config.currency.trim().to_ascii_uppercase();
        if currency.is_empty() {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 结算币种不能为空".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for market in &markets {
            if !seen.insert(market.instrument.clone()) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "共享 Bar session 存在重复标的 {}",
                    market.instrument
                )));
            }
            if market.spec.instrument != market.instrument {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享 Bar session 的 market spec 与标的不一致".into(),
                ));
            }
            market.spec.validate()?;
            market.fee.validate()?;
            if market.spec.product == qx_core::TradingProduct::Option {
                return Err(qx_core::QxError::BusinessViolation(
                    "共享 Bar session 不支持期权：期权合约身份与估值语义尚未建模".into(),
                ));
            }
            super::super::currency::normalize_and_validate(
                currency.clone(),
                &market.instrument,
                Some(&market.spec),
            )?;
            if !DataTier::Bar.supports(market.fill.tier()) {
                return Err(qx_core::QxError::BusinessViolation(format!(
                    "市场 {} 的撮合模型需要 {:?} 数据，当前 session 仅接收 Bar",
                    market.instrument,
                    market.fill.tier()
                )));
            }
            let quality = QualityGate::check(market.bars);
            if matches!(quality.verdict(), Verdict::Fail | Verdict::Quarantine) {
                return Err(qx_core::QxError::Permanent(format!(
                    "共享 Bar session 输入未通过质量门 {}: {:?}",
                    market.instrument, quality.issues
                )));
            }
        }
        let mut config = config;
        config.currency = currency;
        config.initial_cash_exact = Some(initial_cash_exact);
        Ok(Self { config, markets })
    }
}
