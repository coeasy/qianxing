//! Batch isolated-account Bar backtests through the existing shared engine.

use super::{
    attach,
    guard::guard_panics,
    run_backtest::run_backtest_inner,
    run_depth_backtest::{run_inner as run_depth_inner, DEPTH_RUNTIME_VERSION},
};
mod canonical_spec;
mod depth_canonical_spec;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{
    MultiAccountBacktestResult, MultiAccountBacktestSpec, MultiAccountBacktestSummary,
    MultiAccountDepthBacktestResult, MultiAccountDepthBacktestSpec,
    MultiAccountDepthBacktestSummary, MultiAccountDepthRunResult, MultiAccountRunResult,
    MULTI_ACCOUNT_BACKTEST_RESULT_SCHEMA_VERSION,
    MULTI_ACCOUNT_DEPTH_BACKTEST_RESULT_SCHEMA_VERSION,
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

/// Verify the batch identity, ordered account set, child run manifests and aggregate summary.
/// Historical results without a parent RunManifest are accepted only when every account failed.
pub(crate) fn verify_multi_account_backtest_result(
    result: &MultiAccountBacktestResult,
) -> Result<(), AppError> {
    verify_multi_account_backtest_result_with_reader(
        result,
        &crate::artifact_store::FileArtifactReader,
    )
}

pub(crate) fn verify_multi_account_backtest_result_with_reader(
    result: &MultiAccountBacktestResult,
    reader: &dyn crate::artifact_store::ArtifactReader,
) -> Result<(), AppError> {
    canonical_spec::verify_multi_account_backtest_result(result, reader)
        .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))
}

pub(crate) fn verify_multi_account_depth_backtest_result(
    result: &MultiAccountDepthBacktestResult,
    reader: &dyn crate::artifact_store::ArtifactReader,
) -> Result<(), AppError> {
    depth_canonical_spec::verify_multi_account_depth_backtest_result(result, reader)
        .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))
}

pub(super) fn run_depth_batch_inner(
    spec: &MultiAccountDepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<MultiAccountDepthBacktestResult>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;
    let mut canonical = spec.clone();
    canonical
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut canonical.runs {
        run.depth_path.clear();
        run.output_dir.clear();
        // These are storage/provenance locators; the content identity and quality fingerprint
        // are recorded by each successful child RunManifest.
        run.quality_manifest_path = None;
        run.runtime_config_path = None;
        run.market_spec_path = None;
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
                "通过校验的多账户深度回测成员缺少 account_id",
            )
        })?;
        ordered.insert(account_id, run);
    }
    let settlement_currency = spec.runs[0].settlement_currency.clone();
    let mut results = Vec::with_capacity(ordered.len());
    let mut weighted_returns = crate::report_analysis::WeightedReturnAccumulatorV1::default();
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
        match run_depth_inner(run, &member_context, cancellation) {
            Ok(Some(outcome)) => {
                weighted_returns
                    .push(outcome.return_bps, run.initial_cash_raw)
                    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
                successful_fills =
                    successful_fills.checked_add(outcome.fills).ok_or_else(|| {
                        AppError::new(AppErrorCategory::InternalInvariant, "多账户成交数求和溢出")
                    })?;
                results.push(MultiAccountDepthRunResult {
                    account_id: account_id.to_string(),
                    outcome: Some(outcome),
                    error_json: None,
                });
            }
            Ok(None) => return Ok(None),
            Err(error) => results.push(MultiAccountDepthRunResult {
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
    let successful_initial_cash_raw = weighted_returns.initial_cash_raw();
    let weighted_return_bps = weighted_returns
        .finish()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let mut result = MultiAccountDepthBacktestResult {
        schema_version: MULTI_ACCOUNT_DEPTH_BACKTEST_RESULT_SCHEMA_VERSION,
        result_kind: "multi_account_depth".into(),
        batch_id: spec.batch_id.clone(),
        settlement_currency,
        spec_fingerprint,
        summary: MultiAccountDepthBacktestSummary {
            accounts_total: results.len(),
            accounts_succeeded,
            accounts_failed,
            successful_initial_cash_raw,
            successful_fills,
            weighted_return_bps,
            complete: accounts_failed == 0,
        },
        runs: results,
        run_manifest: None,
    };
    result.run_manifest = depth_canonical_spec::build_multi_account_depth_run_manifest(
        spec,
        &result,
        context.code_commit(),
        super::run_depth_backtest::DEPTH_RUNTIME_VERSION,
    )?;
    depth_canonical_spec::verify_multi_account_depth_backtest_result(
        &result,
        &crate::artifact_store::FileArtifactReader,
    )
    .map_err(|message| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多账户 Depth 父结果发布前自校验失败: {message}"),
        )
    })?;
    Ok(Some(result))
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
    // Local paths are deployment details. Dataset identity is carried by each outcome's content
    // fingerprint, so transport adapters may resolve roots without changing this configuration id.
    let spec_fingerprint = canonical_spec::legacy_spec_fingerprint(spec)?;

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
    let mut weighted_returns = crate::report_analysis::WeightedReturnAccumulatorV1::default();
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
                weighted_returns
                    .push(outcome.return_bps, run.initial_cash_raw)
                    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
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
    let successful_initial_cash_raw = weighted_returns.initial_cash_raw();
    let weighted_return_bps = weighted_returns
        .finish()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;

    let mut result = MultiAccountBacktestResult {
        schema_version: MULTI_ACCOUNT_BACKTEST_RESULT_SCHEMA_VERSION,
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
        run_manifest: None,
    };
    result.run_manifest = canonical_spec::build_multi_account_run_manifest(
        spec,
        &result,
        context.code_commit(),
        &format!("qx-app-{}", env!("CARGO_PKG_VERSION")),
    )?;
    Ok(Some(result))
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
