//! 多内置策略（双腿套利）回测链：`run_multi_builtin_backtest`。CCXT OHLCV 入口交给 `single_strategy.rs::run_builtin_backtest`，与单标的链共用装配。
use super::{
    apply_configured_builtin_signal, backtest_risk_binding, bar_fill_model,
    barframe_dataset_identity, builtin_signal_note, configured_bar_volume_participation_bps,
    configured_fill_model, configured_instrument_product, execution_cost_binding,
    market_spec_with_margin, multi_leg_builtin_strategy_config, multi_leg_integrity_summary,
    multi_leg_leg_cash, multi_leg_spec_guard, persist_multi_composition_artifacts,
    persist_multi_leg_run_artifacts, read_bar_frame_for_multi_backtest,
    recompute_multi_barframe_input, reject_ashare_rules_config, reject_configured_initial_cash,
    render_number, verified_dataset_quality, Bar, BuiltinStrategyKind, MarketSpecLoad,
    MultiCompositionArtifactInput, MultiLegArtifactInput, Quantity,
    BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE, BAR_MATCHING_KERNEL, MULTI_LEG_KINDS,
};
use std::path::Path;
mod app_adapter;

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
    let primary_frame = read_bar_frame_for_multi_backtest(primary_path, "主腿")?;
    let reference_frame = read_bar_frame_for_multi_backtest(reference_path, "对冲腿")?;
    if primary_dataset_manifest_path.is_some() != reference_dataset_manifest_path.is_some() {
        return Err("多腿质量清单必须同时为主腿和对冲腿提供，或两者都不提供".into());
    }
    let primary_dataset_quality = primary_dataset_manifest_path
        .map(|path| {
            let (_, dataset) = barframe_dataset_identity(primary_path, &primary_frame)?;
            verified_dataset_quality(path, primary_path, &primary_frame, &dataset)
        })
        .transpose()?;
    let reference_dataset_quality = reference_dataset_manifest_path
        .map(|path| {
            let (_, dataset) = barframe_dataset_identity(reference_path, &reference_frame)?;
            verified_dataset_quality(path, reference_path, &reference_frame, &dataset)
        })
        .transpose()?;
    if primary_frame.ts != reference_frame.ts {
        return Err("多腿回测要求两条 BarFrame 的时间戳完全对齐".into());
    }
    let primary_bars: Vec<Bar> = (&primary_frame).into();
    let reference_bars: Vec<Bar> = (&reference_frame).into();
    if primary_bars.len() < 3 {
        return Err("多腿内置策略回测至少需要三根对齐 Bar".into());
    }
    let costs = execution_cost_binding(runtime_config_path)?;
    let fill_configured = configured_fill_model(runtime_config_path)?;
    let volume_participation_bps = configured_bar_volume_participation_bps(runtime_config_path)?;
    // 本金在这条链上没有落点：两条腿的账户各按本腿行情定资，因此拒绝而不是收下再丢掉。
    reject_configured_initial_cash(runtime_config_path, "backtest multi-builtin")?;
    // `multi_leg_leg_cash` 按本腿最高价和生效费率定资，计算失败即报错，不截断到 i64::MAX。
    let primary_cash = multi_leg_leg_cash(quantity, &primary_bars, costs.rules.taker_bp, "主")?;
    let reference_cash =
        multi_leg_leg_cash(quantity, &reference_bars, costs.rules.taker_bp, "对冲")?;
    let mut strategy_config = multi_leg_builtin_strategy_config(
        kind,
        primary_frame.instrument.clone(),
        reference_frame.instrument.clone(),
        Quantity::from_i64(quantity),
    );
    // 与单标的链同读 `--config`（风控、撮合、成本与信号参数；V11 Q64）。
    let signal_provenance =
        apply_configured_builtin_signal(&mut strategy_config, runtime_config_path)?;
    println!(
        "[Multi · Signal] {} quantity={}",
        builtin_signal_note(&strategy_config, &signal_provenance),
        quantity
    );
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
    // 规格闸门先于撮合：名义额、保证金与资金费取自 spec；缺 spec 按现货乘数 1，不能声称计提衍生品费用。
    multi_leg_spec_guard(
        [
            ("primary", primary_spec.as_ref()),
            ("reference", reference_spec.as_ref()),
        ],
        funding_bps,
        configured_instrument_product(runtime_config_path)?,
    )?;
    // `margin_peak_raw=0` 可能表示现货，也可能表示衍生品漏 spec；本行与 `market_specs` 一起区分两者（V11 Q58）。
    // Both legs use the same explicit app risk, cost and fill bindings; their ledgers remain independent.
    let risk_binding = backtest_risk_binding(runtime_config_path, true)?;
    reject_ashare_rules_config(
        runtime_config_path,
        "backtest multi-builtin",
        "一份 strategy 段无法同时描述两条腿所属标的的交易制度",
    )?;
    let risk_rule_set_version = risk_binding.gate().rule_set().version().to_string();
    let cost_source = costs.source();
    let primary_fill = bar_fill_model(fill_configured.as_deref(), primary_spec.as_ref())?;
    let reference_fill = bar_fill_model(fill_configured.as_deref(), reference_spec.as_ref())?;
    let leg_fill_models = vec![
        (primary_fill.name, primary_fill.source),
        (reference_fill.name, reference_fill.source),
    ];
    let Some((fill_model_name, fill_model_source)) = leg_fill_models.first().copied() else {
        return Err("多腿回测未记录任何腿级撮合模型".into());
    };
    if leg_fill_models
        .iter()
        .any(|leg| *leg != (fill_model_name, fill_model_source))
    {
        return Err(format!(
            "多腿回测腿级撮合模型口径不一致: legs={leg_fill_models:?} declared={fill_model_name}/{fill_model_source}"
        ));
    }

    let app_legs = app_adapter::run(app_adapter::AppLegInput {
        kind,
        funding_bps,
        primary_frame: &primary_frame,
        reference_frame: &reference_frame,
        primary_path,
        reference_path,
        primary_spec: primary_spec.as_ref(),
        reference_spec: reference_spec.as_ref(),
        primary_margin_model: primary_margin_model.as_ref(),
        reference_margin_model: reference_margin_model.as_ref(),
        primary_cash,
        reference_cash,
        strategy_config: &strategy_config,
        costs: &costs,
        risk: &risk_binding,
        fill_model: fill_configured.as_ref().map(|_| primary_fill.model),
        max_bar_volume_participation_bps: volume_participation_bps,
    })?;
    let app_run = app_legs.run;
    let primary_report = &app_run.primary.report;
    let reference_report = &app_run.reference.report;
    let attribution = &app_run.attribution;
    let leg_risk_versions = app_legs.risk_rule_versions;
    if leg_risk_versions
        .iter()
        .any(|version| version != &risk_rule_set_version)
    {
        return Err(format!(
            "多腿回测腿级生效风控规则集与产物声明不一致: legs={leg_risk_versions:?} declared={risk_rule_set_version}"
        ));
    }
    let primary_equity = primary_report.final_equity();
    let reference_equity = reference_report.final_equity();
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
        risk_rule_set_version,
        risk_binding.source(),
        BAR_MATCHING_KERNEL
    );
    println!(
        "[Multi-leg · Execution] fill_model={} source={}",
        fill_model_name, fill_model_source
    );
    if let Some(root) = artifact_root {
        let runs = root.join("runs");
        std::fs::create_dir_all(&runs)
            .map_err(|error| format!("创建多腿归因产物目录失败: {error}"))?;
        let leg_summary_paths = persist_multi_leg_run_artifacts(MultiLegArtifactInput {
            root,
            strategy_id: strategy_id.as_str(),
            runtime_config_path,
            primary_path,
            reference_path,
            dataset_quality: [
                primary_dataset_quality.as_ref(),
                reference_dataset_quality.as_ref(),
            ],
            primary_frame: &primary_frame,
            reference_frame: &reference_frame,
            primary_bars: &primary_bars,
            reference_bars: &reference_bars,
            primary_spec_version,
            reference_spec_version,
            primary_report: &primary_report,
            reference_report: &reference_report,
            primary_cash,
            reference_cash,
            risk_rule_set_version: &risk_rule_set_version,
            risk_rule_source: risk_binding.source(),
            cost_source: &cost_source,
            fill_model: (fill_model_name, fill_model_source),
        })?;
        let attribution = qx_app::multi_builtin_composition_summary_json(
            qx_app::MultiBuiltinCompositionSummary {
                strategy_id: &strategy_id,
                strategy_kind: kind.name(),
                quantity,
                primary_instrument: &primary_frame.instrument.to_string(),
                reference_instrument: &reference_frame.instrument.to_string(),
                funding_bps,
                risk_rule_set_version: &risk_rule_set_version,
                risk_source: risk_binding.source(),
                matching_kernel: BAR_MATCHING_KERNEL,
                cost_source: &cost_source,
                fill_model_name,
                fill_model_source,
                account_base_source: BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE,
                primary_initial_cash_raw: primary_cash.raw(),
                reference_initial_cash_raw: reference_cash.raw(),
                primary_final_equity_raw: primary_equity,
                reference_final_equity_raw: reference_equity,
                primary_spec_path,
                reference_spec_path,
                attribution,
            },
        );
        let attribution_path = qx_app::artifact_store::persist_multi_leg_attribution_artifact(
            root,
            kind.name(),
            primary_report.result_hash(),
            reference_report.result_hash(),
            &attribution,
        )?;
        println!(
            "[Artifacts] spread_attribution={}",
            attribution_path.display()
        );
        let summary_path = persist_multi_composition_artifacts(MultiCompositionArtifactInput {
            root,
            runtime_config_path,
            strategy_id: &strategy_id,
            primary_path,
            reference_path,
            dataset_quality: [
                primary_dataset_quality.as_ref(),
                reference_dataset_quality.as_ref(),
            ],
            primary_frame: &primary_frame,
            reference_frame: &reference_frame,
            primary_report: &primary_report,
            reference_report: &reference_report,
            leg_summary_paths: [&leg_summary_paths[0], &leg_summary_paths[1]],
            summary: attribution,
        })?;
        println!("[Artifacts] composition_summary={}", summary_path.display());
        let composition_summary: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&summary_path)
                .map_err(|error| format!("读取组合级摘要以生成 EvidenceBundle 失败: {error}"))?,
        )
        .map_err(|error| format!("组合级摘要 JSON 无效，无法生成 EvidenceBundle: {error}"))?;
        recompute_multi_barframe_input(&composition_summary)?;
        let evidence_path =
            crate::run_evidence::write_run_evidence(&summary_path, &composition_summary, None)?;
        println!("[Artifacts] evidence_bundle={}", evidence_path.display());
    }
    Ok(())
}
