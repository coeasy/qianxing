use super::PortfolioBarSessionReport;
use qx_core::{DecimalRounding, ExactDecimal, ReplayVerifier};

impl PortfolioBarSessionReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn verify(&self) -> Result<(), qx_core::QxError> {
        self.event_log.validate()?;
        ReplayVerifier::verify(self.event_log.events(), &self.ledger)?;
        let initial_cash_projection = self
            .initial_cash_exact
            .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
            .and_then(|projected| projected.to_raw(9).ok());
        if !self.initial_cash_exact.is_positive()
            || initial_cash_projection != Some(self.initial_cash_raw)
            || self.ledger.entries().first().is_none_or(|entry| {
                entry.exact_amounts.map(|amounts| amounts.amount) != Some(self.initial_cash_exact)
            })
        {
            return Err(qx_core::QxError::Invariant(
                "多市场精确初始资金与原始投影/账簿入金不一致".into(),
            ));
        }
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

        if self.equity_exact.len() != self.equity.len()
            || self.instrument_equity_contributions_exact.len() != self.market_specs.len()
            || self
                .instrument_equity_contributions_exact
                .keys()
                .ne(self.market_specs.keys())
        {
            return Err(qx_core::QxError::Invariant(
                "多市场 exact 权益曲线或逐标的 exact 归因集合不完整".into(),
            ));
        }
        let initial_cash = self.initial_cash_exact;
        for (index, ((equity_ts, _), (equity_index, exact_ts, _))) in
            self.equity.iter().zip(&self.equity_exact).enumerate()
        {
            if *equity_index != index as u64 || equity_ts != exact_ts {
                return Err(qx_core::QxError::Invariant(
                    "多市场 exact 权益时间轴与 raw 权益曲线不一致".into(),
                ));
            }
        }
        let Some(exact_reference_curve) =
            self.instrument_equity_contributions_exact.values().next()
        else {
            return Err(qx_core::QxError::Invariant(
                "多市场 exact 权益贡献采样曲线为空".into(),
            ));
        };
        if exact_reference_curve.is_empty()
            || exact_reference_curve.len() > 1_000
            || exact_reference_curve
                .windows(2)
                .any(|pair| pair[0].0 >= pair[1].0)
            || self
                .instrument_equity_contributions_exact
                .values()
                .any(|curve| curve.len() != exact_reference_curve.len())
            || exact_reference_curve.first().map(|point| point.0) != Some(0)
            || exact_reference_curve.last().map(|point| point.0)
                != Some(self.equity_exact.len().saturating_sub(1) as u64)
        {
            return Err(qx_core::QxError::Invariant(
                "多市场 exact 权益贡献采样点数或首尾索引无效".into(),
            ));
        }
        for (sample_index, (equity_index, ts, _)) in exact_reference_curve.iter().enumerate() {
            let Some((exact_equity_index, exact_ts, exact_equity)) = usize::try_from(*equity_index)
                .ok()
                .and_then(|index| self.equity_exact.get(index))
            else {
                return Err(qx_core::QxError::Invariant(
                    "多市场 exact 权益贡献采样索引越界".into(),
                ));
            };
            if exact_equity_index != equity_index || exact_ts != ts {
                return Err(qx_core::QxError::Invariant(
                    "多市场 exact 权益贡献采样时间轴不一致".into(),
                ));
            }
            let mut reconstructed = initial_cash;
            for curve in self.instrument_equity_contributions_exact.values() {
                let (candidate_index, candidate_ts, contribution) = curve[sample_index];
                if candidate_index != *equity_index || candidate_ts != *ts {
                    return Err(qx_core::QxError::Invariant(
                        "多市场逐标的 exact 归因曲线采样轴不一致".into(),
                    ));
                }
                reconstructed = reconstructed.checked_add(contribution).ok_or_else(|| {
                    qx_core::QxError::Invariant("多市场 exact 归因求和溢出".into())
                })?;
            }
            if reconstructed != *exact_equity {
                return Err(qx_core::QxError::Invariant(format!(
                    "多市场逐标的 exact 归因之和与 exact 权益不一致: index={equity_index}"
                )));
            }
        }
        Ok(())
    }
}
