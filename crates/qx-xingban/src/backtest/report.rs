use crate::{AshareIssuerCapitalSnapshot, ProtectiveExitFill};
use qx_core::{EventLog, ExactDecimal, Ledger};
use std::collections::BTreeMap;

pub struct BacktestReport {
    pub fills: Vec<qx_core::Fill>,
    /// One model evaluation per eligible pending order and market observation.
    pub execution_attempts: Vec<crate::ExecutionAttempt>,
    pub equity: Vec<i128>,
    /// Scale-aware marked equity, parallel to the legacy 1e-9 `equity` projection.
    pub equity_exact: Vec<ExactDecimal>,
    /// Exact starting capital used by this run (legacy inputs are represented at scale 9).
    pub initial_cash_exact: ExactDecimal,
    /// 现金基准曲线；没有外部基准序列时显式标记为 flat-cash。
    pub benchmark_equity: Vec<i128>,
    pub positions: Vec<i128>,
    /// Protective exits are keyed to their completed-bar trigger and actual next-bar fills.
    pub protective_exit_fills: Vec<ProtectiveExitFill>,
    /// 回测期间生效的发行人股本事实；它不属于账户 Ledger。
    pub issuer_capital_snapshots: Vec<AshareIssuerCapitalSnapshot>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    /// Fee totals retain their currency and decimal scale; raw totals above remain legacy projections.
    pub fees_exact_by_currency: BTreeMap<String, ExactDecimal>,
    /// Exact turnover in `turnover_currency`, including fills below the 1e-9 raw grid.
    pub turnover_exact: ExactDecimal,
    pub turnover_currency: String,
    pub max_drawdown_raw: i128, // 衍生于 max_drawdown_bps；登记了零生产读者 limitation。
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub assumptions: Vec<String>,
    pub event_log: EventLog,
    pub ledger: Ledger,
    pub fill_model_descriptor: crate::FillModelDescriptor,
    pub model_descriptors: Vec<String>,
    pub seed: u64,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
}

impl BacktestReport {
    /// Currency-aware scale-preserving metrics for summaries and research readers.
    pub fn exact_metrics_json(&self) -> serde_json::Value {
        let final_equity = self
            .equity_exact
            .last()
            .copied()
            .unwrap_or(self.initial_cash_exact);
        let mut peak = None::<ExactDecimal>;
        let mut max_drawdown = ExactDecimal::ZERO;
        let mut max_drawdown_bps = 0_i128;
        for equity in &self.equity_exact {
            let current_peak = peak.map_or(*equity, |value| {
                if value
                    .checked_cmp(*equity)
                    .is_some_and(|ordering| ordering.is_lt())
                {
                    *equity
                } else {
                    value
                }
            });
            peak = Some(current_peak);
            if let Some(drawdown) = current_peak.checked_sub(*equity) {
                if drawdown
                    .checked_cmp(max_drawdown)
                    .is_some_and(|ordering| ordering.is_gt())
                {
                    max_drawdown = drawdown;
                }
                if current_peak.is_positive() {
                    max_drawdown_bps = max_drawdown_bps
                        .max(exact_ratio_bps(drawdown, current_peak).unwrap_or_default());
                }
            }
        }
        let return_amount = final_equity
            .checked_sub(self.initial_cash_exact)
            .unwrap_or(ExactDecimal::ZERO);
        serde_json::json!({
            "equity": {
                "initial": self.initial_cash_exact.to_string(),
                "final": final_equity.to_string(),
                "return": return_amount.to_string(),
                "return_bps": exact_ratio_bps(return_amount, self.initial_cash_exact).map(|value| value.to_string()),
                "max_drawdown": max_drawdown.to_string(),
                "max_drawdown_bps": max_drawdown_bps.to_string(),
            },
            "turnover": {
                "amount": self.turnover_exact.to_string(),
                "currency": self.turnover_currency.clone(),
            },
            "fees_by_currency": self.fees_exact_by_currency.iter().map(|(currency, amount)| {
                (currency.clone(), amount.to_string())
            }).collect::<BTreeMap<_, _>>(),
        })
    }
}

pub(crate) fn exact_ratio_bps(numerator: ExactDecimal, denominator: ExactDecimal) -> Option<i128> {
    if !denominator.is_positive() {
        return None;
    }
    numerator
        .checked_mul_at_scale(
            ExactDecimal::from_parts(10_000, 0).ok()?,
            ExactDecimal::MAX_SCALE,
            qx_core::DecimalRounding::TowardZero,
        )?
        .checked_div_at_scale(denominator, 0, qx_core::DecimalRounding::TowardZero)
        .map(ExactDecimal::coefficient)
}

#[cfg(test)]
mod tests {
    use super::exact_ratio_bps;
    use qx_core::{DecimalRounding, ExactDecimal};

    #[test]
    fn exact_basis_points_use_values_below_the_legacy_raw_grid() {
        let initial = ExactDecimal::parse("0.0000000019").unwrap();
        let final_value = ExactDecimal::parse("0.0000000020").unwrap();
        let pnl = final_value.checked_sub(initial).unwrap();

        let exact = exact_ratio_bps(pnl, initial).unwrap();
        let legacy_initial = initial
            .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
            .unwrap()
            .to_raw(9)
            .unwrap();
        let legacy_final = final_value
            .checked_mul_at_scale(ExactDecimal::ONE, 9, DecimalRounding::TowardZero)
            .unwrap()
            .to_raw(9)
            .unwrap();
        let legacy = (legacy_final - legacy_initial) * 10_000 / legacy_initial;

        assert_eq!(exact, 526);
        assert_eq!(legacy, 10_000);
    }
}
