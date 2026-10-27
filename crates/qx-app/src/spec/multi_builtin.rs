use super::{BacktestOutcome, BacktestSpec, BuiltinStrategySpec};
use crate::error::{AppError, AppErrorCategory};
use qx_core::TradingProduct;
use qx_xingban::BacktestReport;
use serde::{Deserialize, Serialize};

pub const MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION: u32 = 1;

/// Independent-account two-leg built-in backtest. Each leg keeps a complete single-leg
/// BacktestSpec so execution, costs, risk, margin and artifacts use the shared app contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiBuiltinBacktestSpec {
    pub schema_version: u32,
    pub strategy: BuiltinStrategySpec,
    pub primary: BacktestSpec,
    pub reference: BacktestSpec,
    #[serde(default)]
    pub funding_bps: i64,
    #[serde(default)]
    pub declared_product: Option<TradingProduct>,
}

impl MultiBuiltinBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION {
            return Err(invalid(format!(
                "MultiBuiltinBacktestSpec schema_version={} 本版本只认 {}",
                self.schema_version, MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION
            )));
        }
        self.strategy.validate()?;
        if !self.strategy.kind.needs_reference_leg() {
            return Err(invalid("多腿内置回测只接受需要 reference leg 的套利策略"));
        }
        self.primary.validate()?;
        self.reference.validate()?;
        if self.primary.initial_cash_exact.is_some() || self.reference.initial_cash_exact.is_some()
        {
            return Err(invalid(
                "MultiBuiltin 两腿的本金按各自定资规则生成，暂不接受 BacktestSpec v2 initial_cash_exact",
            ));
        }
        if self.primary.account_id.is_none() || self.reference.account_id.is_none() {
            return Err(invalid("多腿内置回测必须显式声明每条腿的独立账户"));
        }
        if self.primary.run_id == self.reference.run_id
            || self.primary.bars_path == self.reference.bars_path
            || self.primary.output_dir == self.reference.output_dir
            || self.primary.account_id == self.reference.account_id
        {
            return Err(invalid(
                "多腿内置回测必须使用不同 run、输入、输出目录和账户",
            ));
        }
        if self.primary.settlement_currency != self.reference.settlement_currency {
            return Err(invalid("多腿内置回测两条腿必须使用相同结算币种"));
        }
        if self.primary.portfolio.is_some()
            || self.reference.portfolio.is_some()
            || self.primary.protective_exit.is_some()
            || self.reference.protective_exit.is_some()
        {
            return Err(invalid(
                "多腿内置回测暂不支持单腿 portfolio 或 protective_exit",
            ));
        }
        if !(-10_000..=10_000).contains(&self.funding_bps) {
            return Err(invalid("多腿内置回测 funding_bps 必须在 -10000..=10000"));
        }
        if let Some(product) = self
            .declared_product
            .filter(|product| product.is_derivative())
            .filter(|_| self.primary.market_spec.is_none())
        {
            return Err(invalid(format!(
                "主腿衍生品（strategy.product={:?}）多腿回测必须提供 primary 腿的 market spec：缺它时名义额按现货乘数 1 记账，保证金与资金费也无从计提",
                product
            )));
        }
        if self.funding_bps != 0 {
            for (label, leg) in [("primary", &self.primary), ("reference", &self.reference)] {
                if leg.market_spec.is_none() {
                    return Err(invalid(format!(
                        "资金费只能按该腿 market spec 的名义额计提：{label} 腿没有 market spec，既算不出金额，也判不出它是现货还是衍生品。请补该腿的 spec 文件，或把 --funding-bps 设为 0"
                    )));
                }
            }
            if [&self.primary, &self.reference].iter().all(|leg| {
                !leg.market_spec
                    .as_ref()
                    .is_some_and(|spec| spec.product.is_derivative())
            }) {
                let products = [&self.primary, &self.reference]
                    .iter()
                    .zip(["primary", "reference"])
                    .map(|(leg, label)| {
                        format!(
                            "{label}={:?}",
                            leg.market_spec.as_ref().map(|spec| spec.product)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                return Err(invalid(format!(
                    "没有一条腿是衍生品（{products}），--funding-bps={} 无处计提：资金费只向持仓衍生品合约的一方收取，把这条声明落到现货腿上等于造一笔不存在的成本",
                    self.funding_bps
                )));
            }
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

pub struct MultiBuiltinLegRun {
    pub account_id: String,
    pub outcome: BacktestOutcome,
    pub report: BacktestReport,
    pub risk_rule_set_version: String,
    pub targets: std::collections::BTreeMap<u64, i128>,
}

pub struct MultiBuiltinBacktestRun {
    pub primary: MultiBuiltinLegRun,
    pub reference: MultiBuiltinLegRun,
    pub funding_bps: i64,
    pub attribution: MultiBuiltinAttribution,
}

/// Domain facts derived from both independent leg runs and their shared target schedule.
pub struct MultiBuiltinAttribution {
    pub combined_return_bps: i64,
    pub groups: Vec<qx_zhenlu::SpreadGroupAttribution>,
    pub totals: (i128, i128, i128, i128, i128),
    pub residual_fees_raw: i128,
    pub residual_filled_qty_raw: i128,
    pub pending_reconcile: Vec<crate::MultiLegPendingReconcile>,
    pub naked_filled_qty_raw: i128,
    pub leg_integrity: [crate::MultiLegLegIntegrity; 2],
    pub net_cost_raw: i128,
    pub cost_bps: Option<i64>,
    pub margin_model: String,
}
