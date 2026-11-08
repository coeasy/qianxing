//! qx-app use-case adapters invoked by the qx-cli app facade.

use super::*;

pub(super) fn compare(spec_path: &Path) -> Result<String, AppError> {
    let spec = CompareRunsSpec::from_json(&read_document(spec_path)?)?;
    compare_runs(&spec, &context("compare-runs"))?.to_json()
}

pub(super) fn risk_parity(spec_path: &Path) -> Result<String, AppError> {
    let spec = RiskParitySpec::from_json(&read_document(spec_path)?)?;
    plan_risk_parity(&spec, &context(&spec.portfolio_id))?.to_json()
}

pub(super) fn algo_execution(spec_path: &Path) -> Result<String, AppError> {
    let spec = AlgoExecutionSpec::from_json(&read_document(spec_path)?)?;
    plan_algo_execution(&spec, &context(&spec.parent_id))?.to_json()
}

pub(super) fn experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

pub(super) fn multi_leg_experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunMultiLegExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_multi_leg_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

pub(super) fn multi_market_experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunMultiMarketExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_multi_market_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

pub(super) fn depth_experiment(spec_path: &Path) -> Result<String, AppError> {
    let spec = RunDepthExperimentSpec::from_json(&read_document(spec_path)?)?;
    run_depth_experiment(&spec, &context(&spec.experiment_id))?.to_json()
}

pub(super) fn depth_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = DepthBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_depth_backtest(&spec, &context(&spec.run_id))?.to_json()
}

pub(super) fn multi_leg_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiLegBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_leg_backtest(&spec, &context(&spec.run_id))?.to_json()
}

pub(super) fn multi_account_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiAccountBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_account_backtest(&spec, &context(&spec.batch_id))?.to_json()
}

pub(super) fn multi_account_depth_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiAccountDepthBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_account_depth_backtest(&spec, &context(&spec.batch_id))?.to_json()
}

pub(super) fn multi_account_joint_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiAccountJointBacktestSpec::from_json(&read_document(spec_path)?)?;
    qx_app::run_multi_account_joint_backtest(&spec, &context(&spec.run_id))?.to_json()
}

pub(super) fn multi_market_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiMarketBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_market_backtest(&spec, &context(&spec.run_id))?.to_json()
}

pub(super) fn multi_market_depth_backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = MultiMarketDepthBacktestSpec::from_json(&read_document(spec_path)?)?;
    run_multi_market_depth_backtest(&spec, &context(&spec.run_id))?.to_json()
}

pub(super) fn verify_depth(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = DepthBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    verify_depth_run(&outcome, &context(&outcome.run_id))?.to_json()
}

pub(super) fn validate(spec_path: &Path) -> Result<String, AppError> {
    let spec = DatasetSpec::from_json(&read_document(spec_path)?)?;
    validate_dataset(&spec, &context(&spec.dataset_id))?.to_json()
}

pub(super) fn backtest(spec_path: &Path) -> Result<String, AppError> {
    let spec = BacktestSpec::from_json(&read_document(spec_path)?)?;
    run_backtest(&spec, &context(&spec.run_id))?.to_json()
}

pub(super) fn provider_backtest(runtime_path: &Path, spec_path: &Path) -> Result<String, AppError> {
    let runtime = RuntimeConfig::from_json(&read_document(runtime_path)?).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("运行时 Provider 配置无效: {error}"),
        )
    })?;
    let spec: ProviderBacktestSpec =
        serde_json::from_str(&read_document(spec_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("ProviderBacktestSpec JSON 无效: {error}"),
            )
        })?;
    spec.validate()?;
    let registry = qx_service::provider_registry::from_runtime_config(
        &runtime,
        runtime_path,
        &crate::python_interpreter(),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("装配 ProviderRegistry 失败: {error}"),
        )
    })?
    .ok_or_else(|| {
        AppError::new(
            AppErrorCategory::DataUnavailable,
            "运行时配置没有 api.data_providers，无法执行 Provider 回测",
        )
    })?;
    let outcome =
        run_provider_backtest_with_manifest(&registry, &spec, &context(&spec.backtest.run_id))?;
    serde_json::to_string(&outcome).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("ProviderBacktestOutcome 序列化失败: {error}"),
        )
    })
}

pub(super) fn provider_depth_backtest(
    runtime_path: &Path,
    spec_path: &Path,
) -> Result<String, AppError> {
    let runtime = RuntimeConfig::from_json(&read_document(runtime_path)?).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("运行时 Provider 配置无效: {error}"),
        )
    })?;
    let spec: ProviderDepthBacktestSpec = serde_json::from_str(&read_document(spec_path)?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("ProviderDepthBacktestSpec JSON 无效: {error}"),
            )
        })?;
    spec.validate()?;
    let registry = qx_service::provider_registry::from_runtime_config(
        &runtime,
        runtime_path,
        &crate::python_interpreter(),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("装配 ProviderRegistry 失败: {error}"),
        )
    })?
    .ok_or_else(|| {
        AppError::new(
            AppErrorCategory::DataUnavailable,
            "运行时配置没有 api.data_providers，无法执行 Provider 深度回测",
        )
    })?;
    let outcome = run_provider_depth_backtest(&registry, &spec, &context(&spec.backtest.run_id))?;
    serde_json::to_string(&outcome).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("ProviderDepthBacktestOutcome 序列化失败: {error}"),
        )
    })
}

pub(super) fn verify(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = BacktestOutcome::from_json(&read_document(outcome_path)?)?;
    verify_run(&outcome, &context(&outcome.run_id))?.to_json()
}
