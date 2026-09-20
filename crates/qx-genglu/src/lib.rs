//! # qx-genglu — 更路
//!
//! 审计与对账。更路簿是明清航海者记录航线、针位、更数的手抄本——
//! 用它命名"事件日志与审计"是精准的：都是**不可篡改的航行事实记录**。

use qx_core::Fill;
use std::collections::BTreeMap;

mod reconcile;
pub use reconcile::*;

/// 最大回撤（定点比例，0..1e9 表示 0..100%）。
pub fn max_drawdown(equity: &[i128]) -> i128 {
    let mut peak = 0i128;
    let mut mdd = 0i128;
    for &e in equity {
        if e > peak {
            peak = e;
        }
        if peak > 0 {
            let dd = ((peak - e) * 1_000_000_000) / peak;
            if dd > mdd {
                mdd = dd;
            }
        }
    }
    mdd
}

/// 总收益（定点比例）。
pub fn total_return(equity: &[i128]) -> i128 {
    match (equity.first(), equity.last()) {
        (Some(&f), Some(&l)) if f != 0 => ((l - f) * 1_000_000_000) / f,
        _ => 0,
    }
}

/// 夏普比率。**仅用于报告展示，允许 f64**——不进入撮合与记账路径。
pub fn sharpe_ratio(returns: &[f64], periods_per_year: f64, risk_free: f64) -> Option<f64> {
    if returns.len() < 2 {
        return None;
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / (n - 1.0);
    let sd = var.sqrt();
    if sd == 0.0 {
        return None;
    }
    Some((mean - risk_free / periods_per_year) / sd * periods_per_year.sqrt())
}

/// 绩效汇总。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub n_fills: usize,
    pub total_fee: i128,
    pub total_return: i128,
    pub max_drawdown: i128,
    pub final_equity: i128,
}

/// 分析器：把成交流归约为绩效指标。
#[derive(Default)]
pub struct BasicAnalyser {
    pub total_fee: i128,
    pub n_fills: usize,
}

impl BasicAnalyser {
    pub fn on_fill(&mut self, f: &Fill) {
        self.n_fills += 1;
        self.total_fee += f.fee.raw();
    }

    pub fn report(&self, equity: &[i128]) -> Metrics {
        Metrics {
            n_fills: self.n_fills,
            total_fee: self.total_fee,
            total_return: total_return(equity),
            max_drawdown: max_drawdown(equity),
            final_equity: *equity.last().unwrap_or(&0),
        }
    }
}

/// 成交归因链：不改变 Fill 事实，只在运营层保存来源。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FillAttribution {
    pub fill: Fill,
    pub strategy_id: String,
    pub signal_id: Option<u64>,
    pub intent_id: u64,
    pub account_id: String,
    pub rule_version: String,
}

#[derive(Default)]
pub struct AttributionBook {
    fills: Vec<FillAttribution>,
}

impl AttributionBook {
    pub fn record(&mut self, item: FillAttribution) {
        self.fills.push(item);
    }
    pub fn all(&self) -> &[FillAttribution] {
        &self.fills
    }
    pub fn total_by_strategy(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for f in &self.fills {
            *out.entry(f.strategy_id.clone()).or_insert(0) += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawdown_computed() {
        let eq = vec![100, 120, 90, 130];
        let dd = max_drawdown(&eq);
        // peak 120 -> 90，回撤 25%
        assert_eq!(dd, 250_000_000);
    }

    #[test]
    fn total_return_signed() {
        assert_eq!(total_return(&[100, 110]), 100_000_000); // +10%
        assert_eq!(total_return(&[100, 90]), -100_000_000); // -10%
    }

    #[test]
    fn attribution_keeps_strategy_chain() {
        let mut book = AttributionBook::default();
        book.record(FillAttribution {
            fill: Fill {
                order_id: 1,
                qty: qx_core::Quantity::from_i64(1),
                price: qx_core::Price::from_i64(1),
                fee: qx_core::Money::ZERO,
                ts: 1,
                ..Fill::default()
            },
            strategy_id: "s1".into(),
            signal_id: Some(9),
            intent_id: 1,
            account_id: "a".into(),
            rule_version: "r1".into(),
        });
        assert_eq!(book.total_by_strategy().get("s1"), Some(&1));
    }
}
