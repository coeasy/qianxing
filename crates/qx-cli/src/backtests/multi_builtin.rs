//! 多内置策略（双腿套利）回测链：`run_multi_builtin_backtest`。CCXT OHLCV 入口交给 `single_strategy.rs::run_builtin_backtest`，与单标的链共用装配。
use super::{
    barframe_dataset_identity, market_spec_with_margin, multi_leg_integrity_summary,
    read_bar_frame_for_multi_backtest, read_runtime_config, render_number, Bar,
    BuiltinStrategyKind, MarketSpecLoad, Quantity, BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
    BAR_MATCHING_KERNEL, MULTI_LEG_KINDS,
};
use crate::build_identity;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_multi_builtin_backtest(
    strategy_name: &str,
    primary_path: &Path,
    reference_path: &Path,
    primary_spec_path: Option<&Path>,
    reference_spec_path: Option<&Path>,
    primary_dataset_manifest_path: Option<&Path>,
    reference_dataset_manifest_path: Option<&Path>,
    quantity: i64,
    funding_bps: i64,
    artifact_root: Option<&Path>,
    runtime_config_path: Option<&Path>,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("多腿内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if !MULTI_LEG_KINDS.contains(&kind) {
        return Err(format!("多腿回测只接受套利策略，当前为 {}", kind.name()));
    }
    // A run must use one immutable runtime-config snapshot: independently reopening the file for
    // risk, cost, fill and artifact identity can mix two versions if an operator edits it mid-run.
    let runtime = runtime_config_path.map(read_runtime_config).transpose()?;
    let primary_frame = read_bar_frame_for_multi_backtest(primary_path, "主腿")?;
    let reference_frame = read_bar_frame_for_multi_backtest(reference_path, "对冲腿")?;
    qx_app::validate_multi_builtin_frames(
        kind,
        Quantity::from_i64(quantity).raw(),
        &primary_frame,
        &reference_frame,
    )
    .map_err(|error| error.to_string())?;
    if primary_dataset_manifest_path.is_some() != reference_dataset_manifest_path.is_some() {
        return Err("多腿质量清单必须同时为主腿和对冲腿提供，或两者都不提供".into());
    }
    let primary_dataset_quality = primary_dataset_manifest_path
        .map(|path| {
            let (_, dataset) = barframe_dataset_identity(primary_path, &primary_frame)?;
            qx_app::artifact_store::verify_barframe_dataset_quality(
                path,
                primary_path,
                &primary_frame,
                &dataset,
            )
        })
        .transpose()?;
    let reference_dataset_quality = reference_dataset_manifest_path
        .map(|path| {
            let (_, dataset) = barframe_dataset_identity(reference_path, &reference_frame)?;
            qx_app::artifact_store::verify_barframe_dataset_quality(
                path,
                reference_path,
                &reference_frame,
                &dataset,
            )
        })
        .transpose()?;
    let primary_bars: Vec<Bar> = (&primary_frame).into();
    let reference_bars: Vec<Bar> = (&reference_frame).into();
    // 先解析规格：策略现金腿落在主腿账簿，币种从 spec 读取。
    let MarketSpecLoad {
        spec: primary_spec,
        app_margin_model: primary_margin_model,
        source: primary_spec_version,
        ..
    } = market_spec_with_margin(&primary_frame.instrument, primary_spec_path, "多腿")?;
    let MarketSpecLoad {
        spec: reference_spec,
        app_margin_model: reference_margin_model,
        source: reference_spec_version,
        ..
    } = market_spec_with_margin(&reference_frame.instrument, reference_spec_path, "多腿")?;
    // `margin_peak_raw=0` 可能表示现货，也可能表示衍生品漏 spec；本行与 `market_specs` 一起区分两者（V11 Q58）。
    let bindings = qx_service::backtest_bindings::multi_builtin_backtest_bindings_from_runtime(
        qx_service::backtest_bindings::MultiBuiltinBacktestBindingInput {
            kind,
            frames: [&primary_frame, &reference_frame],
            bars: [&primary_bars, &reference_bars],
            market_specs: [primary_spec.as_ref(), reference_spec.as_ref()],
            margin_models: [
                primary_margin_model.as_ref(),
                reference_margin_model.as_ref(),
            ],
            quantity,
            funding_bps,
            runtime: runtime.as_ref(),
            runtime_config_path,
        },
    )?;
    let qx_service::backtest_bindings::MultiBuiltinBacktestRuntimeBindings {
        app_inputs,
        primary_cash,
        reference_cash,
        risk_rule_set_version,
        risk_source,
        cost_source,
        fill_model_name,
        fill_model_source,
        signal_note,
    } = bindings;
    println!("[Multi · Signal] {signal_note} quantity={quantity}");
    let app_context = qx_app::RunContext::new(
        qx_app::CallerCapability::Research,
        format!("multi-builtin-{}", kind.name()),
    )
    .with_code_commit(build_identity::BUILD_REVISION);
    let app_run = qx_app::run_multi_builtin_backtest_with_inputs(app_inputs, &app_context)
        .map_err(|error| format!("qx-app 独立账本多腿回测失败: {}", error.to_json()))?;
    let primary_report = &app_run.primary.report;
    let reference_report = &app_run.reference.report;
    let attribution = &app_run.attribution;
    let leg_risk_versions = vec![
        app_run.primary.risk_rule_set_version.clone(),
        app_run.reference.risk_rule_set_version.clone(),
    ];
    if leg_risk_versions
        .iter()
        .any(|version| version != &risk_rule_set_version)
    {
        return Err(format!(
            "多腿回测腿级生效风控规则集与产物声明不一致: legs={leg_risk_versions:?} declared={risk_rule_set_version}"
        ));
    }
    let combined_return_bps = attribution.combined_return_bps;
    let groups = &attribution.groups;
    let totals = attribution.totals;
    let residual_fees_raw = attribution.residual_fees_raw;
    let residual_filled_qty_raw = attribution.residual_filled_qty_raw;
    let pending_reconcile = &attribution.pending_reconcile;
    let leg_integrity = &attribution.leg_integrity;
    let naked_filled_qty_raw = attribution.naked_filled_qty_raw;
    let net_cost_raw = attribution.net_cost_raw;
    let cost_bps = attribution.cost_bps;
    let margin_model = attribution.margin_model.as_str();
    println!(
        "[Multi-leg · Backtest] strategy={} primary={} fills={} return_bps={} reference={} fills={} return_bps={} combined_return_bps={} result_hashes={:016x}/{:016x}",
        kind.name(),
        primary_frame.instrument,
        primary_report.fills.len(),
        primary_report.return_bps,
        reference_frame.instrument,
        reference_report.fills.len(),
        reference_report.return_bps,
        combined_return_bps,
        primary_report.result_hash(),
        reference_report.result_hash()
    );
    let strategy_id = format!("builtin-{}-multi", kind.name());
    println!(
        "[Multi-leg · Integrity] {}",
        leg_integrity
            .iter()
            .map(multi_leg_integrity_summary)
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "[Multi-leg · Reconcile] pending={} naked_filled_qty_raw={} policy=mark-pending-reconcile-no-auto-close",
        pending_reconcile.len(),
        naked_filled_qty_raw
    );
    println!(
        "[Multi-leg · Attribution] strategy={} groups={} turnover_raw={} fees_raw={} funding_raw={} filled_qty_raw={} margin_peak_raw={} net_cost_raw={} cost_bps={} residual_filled_qty_raw={} residual_fees_raw={} funding_bps={} margin_model={} funding_model={}",
        strategy_id,
        groups.len(),
        totals.1,
        totals.0,
        totals.2,
        totals.3,
        totals.4,
        net_cost_raw,
        render_number(cost_bps.map(i128::from)),
        residual_filled_qty_raw,
        residual_fees_raw,
        funding_bps,
        margin_model,
        if funding_bps == 0 {
            "disabled"
        } else {
            "8h-pro-rata-by-holding-time"
        }
    );
    println!(
        "[Multi-leg · Risk] rule_set_version={} source={} kernel={}",
        risk_rule_set_version, risk_source, BAR_MATCHING_KERNEL
    );
    println!(
        "[Multi-leg · Execution] fill_model={} source={}",
        fill_model_name, fill_model_source
    );
    if let Some(root) = artifact_root {
        let config_fingerprint = runtime
            .as_ref()
            .map(|config| config.fingerprint())
            .transpose()?;
        let artifact_set = qx_app::artifact_store::publish_multi_builtin_backtest_artifacts(
            &app_run,
            qx_app::artifact_store::MultiBuiltinArtifactPublicationInput {
                root,
                code_commit: build_identity::BUILD_REVISION,
                strategy_id: &strategy_id,
                strategy_kind: kind.name(),
                quantity,
                funding_bps,
                runtime_config_path,
                config_fingerprint: config_fingerprint.as_deref(),
                primary_path,
                reference_path,
                primary_frame: &primary_frame,
                reference_frame: &reference_frame,
                primary_spec_path,
                reference_spec_path,
                spec_versions: [primary_spec_version, reference_spec_version],
                initial_cash: [primary_cash, reference_cash],
                risk_rule_set_version: &risk_rule_set_version,
                risk_source,
                cost_source: &cost_source,
                fill_model: (fill_model_name, fill_model_source),
                matching_kernel: BAR_MATCHING_KERNEL,
                account_base_source: BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
                dataset_quality: [
                    primary_dataset_quality.as_ref(),
                    reference_dataset_quality.as_ref(),
                ],
                build_profile: build_identity::BUILD_PROFILE,
                target_triple: build_identity::TARGET_TRIPLE,
                operator: &std::env::var("QX_OPERATOR").unwrap_or_else(|_| "local".to_string()),
            },
        )?;
        let evidence_path = artifact_set.evidence_bundle;
        let artifact_set = artifact_set.artifacts;
        println!(
            "[Artifacts] spread_attribution={}",
            artifact_set.attribution.display()
        );
        let summary_path = artifact_set.composition_summary;
        println!("[Artifacts] composition_summary={}", summary_path.display());
        println!("[Artifacts] evidence_bundle={}", evidence_path.display());
    }
    Ok(())
}
