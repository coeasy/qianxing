//! Batch isolated-account Bar backtests through the existing shared engine.

use super::{attach, guard::guard_panics, run_backtest::run_backtest_inner};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{
    MultiAccountBacktestResult, MultiAccountBacktestSpec, MultiAccountBacktestSummary,
    MultiAccountRunResult,
};
use qx_core::Fnv1a;
use std::collections::BTreeMap;

/// Run independently funded accounts in stable account-id order.
///
/// Each member reuses `run_backtest`; this orchestration does not implement a
/// second matcher, ledger, or risk engine. Account failures are isolated and
/// returned beside successful runs. Cross-account capital netting is not part
/// of this v1 contract.
pub fn run_multi_account_backtest(
    spec: &MultiAccountBacktestSpec,
    context: &RunContext,
) -> Result<MultiAccountBacktestResult, AppError> {
    context.require(CallerCapability::Research, "多账户独立回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步多账户回测意外返回取消状态",
                ),
                &correlation_id,
            )
        })
}

impl MultiAccountBacktestSpec {
    /// Start a cancellable batch worker. Cancellation is forwarded to the
    /// active account backtest and checked between accounts.
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiAccountBacktestResult> {
        let run_id = context.correlation_id().to_string();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多账户独立回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

fn run_inner(
    spec: &MultiAccountBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<MultiAccountBacktestResult>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;
    let mut canonical = spec.clone();
    canonical
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut canonical.runs {
        // Local paths are deployment details. Dataset identity is carried
        // by each outcome's content fingerprint, so transport adapters may
        // safely resolve roots without changing the batch configuration id.
        run.bars_path.clear();
        run.output_dir.clear();
    }
    let input = canonical.to_json()?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&input);
    let spec_fingerprint = format!("{:016x}", hasher.finish());

    let mut ordered: BTreeMap<&str, &_> = BTreeMap::new();
    for run in &spec.runs {
        let account_id = run.account_id.as_deref().ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "通过校验的多账户回测成员缺少 account_id",
            )
        })?;
        ordered.insert(account_id, run);
    }

    let settlement_currency = spec.runs[0].settlement_currency.clone();
    let mut results = Vec::with_capacity(ordered.len());
    let mut successful_initial_cash_raw = 0_i128;
    let mut weighted_return_numerator = 0_i128;
    let mut successful_fills = 0_u64;

    for (account_id, run) in ordered {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let member_context = RunContext::new(
            context.capability(),
            format!("{}-{account_id}", context.correlation_id()),
        )
        .with_code_commit(context.code_commit());
        match run_backtest_inner(run, &member_context, cancellation) {
            Ok(Some(outcome)) => {
                successful_initial_cash_raw = successful_initial_cash_raw
                    .checked_add(run.initial_cash_raw)
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "多账户成功本金求和溢出",
                        )
                    })?;
                let weighted = i128::from(outcome.return_bps)
                    .checked_mul(run.initial_cash_raw)
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "多账户加权收益乘法溢出",
                        )
                    })?;
                weighted_return_numerator = weighted_return_numerator
                    .checked_add(weighted)
                    .ok_or_else(|| {
                        AppError::new(
                            AppErrorCategory::InternalInvariant,
                            "多账户加权收益求和溢出",
                        )
                    })?;
                successful_fills =
                    successful_fills.checked_add(outcome.fills).ok_or_else(|| {
                        AppError::new(AppErrorCategory::InternalInvariant, "多账户成交数求和溢出")
                    })?;
                results.push(MultiAccountRunResult {
                    account_id: account_id.to_string(),
                    outcome: Some(outcome),
                    error_json: None,
                });
            }
            Ok(None) => return Ok(None),
            Err(error) => results.push(MultiAccountRunResult {
                account_id: account_id.to_string(),
                outcome: None,
                error_json: Some(attach(error, member_context.correlation_id()).to_json()),
            }),
        }
    }

    let accounts_succeeded = results
        .iter()
        .filter(|result| result.outcome.is_some())
        .count();
    let accounts_failed = results.len() - accounts_succeeded;
    let weighted_return_bps = if successful_initial_cash_raw == 0 {
        None
    } else {
        let value = weighted_return_numerator / successful_initial_cash_raw;
        Some(i32::try_from(value).map_err(|_| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "多账户加权收益超出 i32 范围",
            )
        })?)
    };

    Ok(Some(MultiAccountBacktestResult {
        batch_id: spec.batch_id.clone(),
        settlement_currency,
        spec_fingerprint,
        summary: MultiAccountBacktestSummary {
            accounts_total: results.len(),
            accounts_succeeded,
            accounts_failed,
            successful_initial_cash_raw,
            successful_fills,
            weighted_return_bps,
            complete: accounts_failed == 0,
        },
        runs: results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_batch_stops_before_reading_inputs_or_creating_results() {
        let cancellation = crate::run_handle::CancellationToken::default();
        cancellation.cancel();
        let spec = MultiAccountBacktestSpec {
            schema_version: 999,
            batch_id: "cancelled-batch".into(),
            runs: Vec::new(),
        };
        let context = RunContext::new(CallerCapability::Research, "cancelled-batch");
        assert!(matches!(
            run_inner(&spec, &context, Some(&cancellation)),
            Ok(None)
        ));
    }
}
