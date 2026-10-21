use super::{PortfolioBarEngine, PortfolioBarMarket, PortfolioBarSessionConfig};
use crate::fill::DataTier;
use qx_guanxing::{QualityGate, Verdict};
use std::collections::BTreeSet;

impl<'a> PortfolioBarEngine<'a> {
    pub fn new(
        config: PortfolioBarSessionConfig,
        markets: Vec<PortfolioBarMarket<'a>>,
    ) -> Result<Self, qx_core::QxError> {
        if markets.len() < 2 {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 至少需要两个市场".into(),
            ));
        }
        if config.account_id.trim().is_empty() || config.initial_cash.raw() <= 0 {
            return Err(qx_core::QxError::BusinessViolation(
                "共享 Bar session 账户身份和初始资金必须有效".into(),
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
        Ok(Self { config, markets })
    }
}
