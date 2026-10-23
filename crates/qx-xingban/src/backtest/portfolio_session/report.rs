use super::PortfolioBarSessionReport;
use qx_core::ReplayVerifier;

impl PortfolioBarSessionReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn verify(&self) -> Result<(), qx_core::QxError> {
        self.event_log.validate()?;
        ReplayVerifier::verify(self.event_log.events(), &self.ledger)?;
        if self.instrument_equity_contributions.len() != self.market_specs.len()
            || self
                .instrument_equity_contributions
                .keys()
                .ne(self.market_specs.keys())
        {
            return Err(qx_core::QxError::Invariant(
                "多市场权益贡献的标的集合与市场规格不一致".into(),
            ));
        }
        let Some(reference_curve) = self.instrument_equity_contributions.values().next() else {
            return Err(qx_core::QxError::Invariant(
                "多市场权益贡献采样曲线为空".into(),
            ));
        };
        if reference_curve.is_empty()
            || reference_curve.len() > 1_000
            || reference_curve
                .windows(2)
                .any(|pair| pair[0].0 >= pair[1].0)
            || self
                .instrument_equity_contributions
                .values()
                .any(|curve| curve.len() != reference_curve.len())
            || reference_curve.first().map(|point| point.0) != Some(0)
            || reference_curve.last().map(|point| point.0)
                != Some(self.equity.len().saturating_sub(1) as u64)
        {
            return Err(qx_core::QxError::Invariant(
                "多市场权益贡献采样点数或首尾索引无效".into(),
            ));
        }
        for (sample_index, (equity_index, ts, _)) in reference_curve.iter().enumerate() {
            let Some((equity_ts, equity_raw)) = usize::try_from(*equity_index)
                .ok()
                .and_then(|index| self.equity.get(index))
            else {
                return Err(qx_core::QxError::Invariant(
                    "多市场权益贡献采样索引越界".into(),
                ));
            };
            if equity_ts != ts {
                return Err(qx_core::QxError::Invariant(
                    "多市场权益贡献采样时间轴与共享权益曲线不一致".into(),
                ));
            }
            let mut reconstructed = self.initial_cash_raw;
            for curve in self.instrument_equity_contributions.values() {
                let (candidate_index, candidate_ts, contribution_raw) = curve[sample_index];
                if candidate_index != *equity_index || candidate_ts != *ts {
                    return Err(qx_core::QxError::Invariant(
                        "多市场权益贡献曲线采样轴不一致".into(),
                    ));
                }
                reconstructed = reconstructed
                    .checked_add(contribution_raw)
                    .ok_or_else(|| qx_core::QxError::Invariant("多市场权益贡献求和溢出".into()))?;
            }
            if reconstructed != *equity_raw {
                return Err(qx_core::QxError::Invariant(format!(
                    "多市场权益贡献第 {equity_index} 个采样点与共享权益不守恒"
                )));
            }
        }
        Ok(())
    }
}
