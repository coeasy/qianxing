//! `RunBacktest` 用例（T2-2，方案 §5 阶段 2、§15.3-A）。
//!
//! 先校验 spec 与输入，再用具名模型装配唯一的 `BacktestEngine`；运行后校验 replay hash，
//! 冲突检查通过后写四份核心产物和重放校验事件流 sidecar。数据不足返回 `FidelityInsufficient`，无效或缺失输入不落产物。
//!
//! ## 为什么冲突检查排在写之前
//!
//! 同一 `run_id` 仅当 `config_hash` 与 `data_fingerprint` 都相同才允许幂等重跑；否则
//! 返回 `Conflict`，避免两个不同运行共用一个身份。
use crate::cases::artifacts::{
    artifact_paths, ensure_output_dir, equity_csv, equity_csv_with_exact, event_trace_jsonl,
    existing_manifest, fills_csv, read_text, summary_json, write_text, ProtectiveExitSummary,
    RunReplaySummary, RunSummary, DATA_TIER, SUMMARY_SCHEMA_VERSION,
};
use crate::cases::attach;
use crate::cases::experiment_search::{DrawdownPruningWindow, SearchBacktestAttempt};
use crate::cases::guard::guard_panics;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::run_handle::CancellationToken;
use crate::spec::{BacktestOutcome, BacktestSpec, MIN_BACKTEST_BARS};
use qx_core::{ExactDecimal, Fnv1a, InstrumentId, Money, ReplayVerifier, RunManifest};
use qx_datastruct::BarFrame;
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, DataTier, PortfolioBarStrategy,
    ProtectiveExitPolicy, ProtectiveExitReason,
};
use std::collections::BTreeMap;
use std::path::Path;

mod bar_strategy;
mod cancellation;
mod canonical_spec;
mod custom_strategy;
mod execution_binding;
pub(in crate::cases) mod strategy_slot;
use crate::cases::risk_gate::backtest_risk_gate;
pub(in crate::cases) use bar_strategy::run_backtest_inner_with_bar_strategy;
pub(super) use bar_strategy::BacktestRunDetails;
use cancellation::run_candidate_engine;
pub(in crate::cases) use custom_strategy::run_backtest_inner_with_experiment_strategy;
pub use custom_strategy::StrategyArtifactIdentity;
use custom_strategy::{strategy_slots_config_hash, InjectedStrategy};
pub(crate) use execution_binding::{bind_execution, bind_margin};
use strategy_slot::build_single_native_strategy;
use strategy_slot::{build_strategy_slot, strategy_version, StrategySlotContext};

/// Historical default retained for callers whose spec predates explicit account ownership.
const DEFAULT_ACCOUNT_ID: &str = "app";

/// 只用于未声明 market spec 的历史兼容请求；带规格的运行绑定其规范化内容摘要。
const NO_INSTRUMENT_SPEC: &str = "instrument-spec:none";

/// 跑一次 Bar 回测并落四份产物。
///
/// 权限：需要 [`CallerCapability::Research`]（R 档，无外部副作用）。
/// 幂等：同一 `run_id` + 同一 `config_hash` + 同一 `data_fingerprint` 重跑是允许的
/// （产物内容逐字节相同）；三者任一不同则 [`AppErrorCategory::Conflict`]。
/// 取消：同步、无阻塞等待，没有取消点（长任务与 `RunHandle` 属 T2-5）。
pub fn run_backtest(
    spec: &BacktestSpec,
    context: &RunContext,
) -> Result<BacktestOutcome, AppError> {
    run_backtest_with_identity_extensions(spec, context, BTreeMap::new())
}

/// Run the shared Bar engine while binding extra source lineage into CanonicalRunSpec.
/// This is used by Provider workflows whose selected source is not represented by BacktestSpec.
pub(crate) fn run_backtest_with_identity_extensions(
    spec: &BacktestSpec,
    context: &RunContext,
    identity_extensions: BTreeMap<String, serde_json::Value>,
) -> Result<BacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    let outcome = guard_panics(&correlation_id, || {
        run_backtest_inner_with_identity_extensions(spec, context, None, identity_extensions)
    })
    .map_err(|error| attach(error, &correlation_id))?;
    outcome.ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步 Bar 回测未产生报告",
            ),
            &correlation_id,
        )
    })
}

impl BacktestSpec {
    /// Start a cooperative worker for this Bar backtest specification.
    pub fn start(self, context: RunContext) -> crate::RunHandle<BacktestOutcome> {
        let run_id = context.correlation_id().to_string();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_backtest_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

pub(super) fn run_backtest_inner(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<BacktestOutcome>, AppError> {
    run_backtest_inner_with_identity_extensions(spec, context, cancellation, BTreeMap::new())
}

fn run_backtest_inner_with_identity_extensions(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity_extensions: BTreeMap<String, serde_json::Value>,
) -> Result<Option<BacktestOutcome>, AppError> {
    bar_strategy::run_backtest_inner_unchecked_with_identity_extensions(
        spec,
        context,
        cancellation,
        Vec::new(),
        identity_extensions,
    )
}

fn run_backtest_inner_with_strategies(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Vec<InjectedStrategy>,
) -> Result<Option<BacktestOutcome>, AppError> {
    bar_strategy::run_backtest_inner_unchecked(spec, context, cancellation, injected)
}

fn run_backtest_inner_unchecked_with_details(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Vec<InjectedStrategy>,
    bar_strategy: Option<Box<dyn BarStrategy>>,
    drawdown_pruning: Option<DrawdownPruningWindow>,
    identity_extensions: BTreeMap<String, serde_json::Value>,
) -> Result<SearchBacktestAttempt<BacktestRunDetails>, AppError> {
    spec.validate()?;
    let identities = injected
        .iter()
        .map(|strategy| strategy.identity.clone())
        .collect::<Vec<_>>();
    let config_hash = if identities.is_empty() {
        spec_config_hash(spec)?
    } else {
        strategy_slots_config_hash(spec, &identities)?
    };
    let mut injected = injected
        .into_iter()
        .map(|strategy| (strategy.identity.strategy_id.clone(), strategy))
        .collect::<BTreeMap<_, _>>();
    let instrument = InstrumentId::parse(&spec.instrument).ok_or_else(|| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BacktestSpec.instrument 非法: {}", spec.instrument),
        )
    })?;
    let payload = read_text(&spec.bars_path)?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 校验失败 {}: {error:?}", spec.bars_path),
        )
    })?;
    if frame.instrument != instrument {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "BarFrame 标的 {} 与 BacktestSpec.instrument {} 不一致——用 A 的数据跑 B 的回测，\
                 产物里的身份会指向一个从没被用过的标的",
                frame.instrument, instrument
            ),
        ));
    }
    if spec.market_spec.as_ref().is_some_and(|market| {
        frame.ts.iter().any(|timestamp| {
            *timestamp < market.valid_from
                || market.valid_to.is_some_and(|until| *timestamp >= until)
        })
    }) {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "Bar 数据时间超出 BacktestSpec.market_spec 的有效期",
        ));
    }
    if frame.ts.len() < MIN_BACKTEST_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "样本过短：{} 根 Bar，Bar 回测下限 {} 根",
                frame.ts.len(),
                MIN_BACKTEST_BARS
            ),
        ));
    }
    let frame_path = Path::new(&spec.bars_path);
    let (bars, input_manifest) = crate::backtest_input_identity::barframe_dataset_identity(
        frame_path, &frame,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame Provider 输入身份复核失败：{error}"),
        )
    })?;
    let content_fingerprint = qx_data::fingerprint_bar_frame(&frame).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 规范内容指纹计算失败：{error}"),
        )
    })?;
    if content_fingerprint != input_manifest.fingerprint {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "BarFrame Provider 清单指纹与规范内容指纹不一致",
        ));
    }
    let input_provenance =
        crate::backtest_input_identity::barframe_input_provenance(frame_path, &input_manifest);
    let mut input_provenance_json = input_provenance.to_json();
    if let Some(provider_lineage) = identity_extensions.get("provider_lineage") {
        input_provenance_json["provider_lineage"] = provider_lineage.clone();
    }
    let data_fingerprint = format!("barframe:{}", input_manifest.fingerprint);
    let risk = backtest_risk_gate(spec);
    let risk_rule_set_version = risk.rule_set().version().to_string();
    let execution = bind_execution(spec)?;
    let fee_model = execution.fee_descriptor;
    let fill_model = execution.fill_name.clone();
    let margin = bind_margin(spec);
    let instrument_spec_version = market_spec_version(spec.market_spec.as_ref())?;
    let account_id = spec
        .account_id
        .as_deref()
        .unwrap_or(DEFAULT_ACCOUNT_ID)
        .to_string();
    let config = BacktestConfig {
        instrument: instrument.clone(),
        instrument_spec: spec.market_spec.clone(),
        account_id: account_id.clone(),
        currency: spec.settlement_currency.clone(),
        initial_cash: Money::from_raw(spec.initial_cash_raw),
        multiplier: 1,
        fill: execution.fill,
        fee: execution.fee,
        data_tier: DataTier::Bar,
        latency: execution.latency,
        margin,
        seed: spec.seed,
        risk,
        virtual_trading: execution.virtual_trading,
    };
    let currency = config.currency.clone();
    let as_of = bars.first().map(|bar| bar.ts).unwrap_or(1);
    let primary_allocation = spec
        .portfolio
        .as_ref()
        .map(|portfolio| portfolio.primary_reserved_capital_raw)
        .unwrap_or(spec.initial_cash_raw);
    let primary_allocation_exact = match &spec.portfolio {
        Some(portfolio) => portfolio
            .primary_reserved_capital()
            .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?,
        None => spec.initial_cash_exact.unwrap_or_else(|| {
            ExactDecimal::from_parts(spec.initial_cash_raw, 9)
                .expect("validated initial cash fits ExactDecimal")
        }),
    };
    let primary_priority = spec
        .portfolio
        .as_ref()
        .map(|portfolio| portfolio.primary_priority)
        .unwrap_or(0);
    let injected_version = injected
        .get(&spec.strategy.strategy_id)
        .map(|injected| injected.identity.strategy_version.clone());
    let strategy_id = if spec.portfolio.is_some() {
        "portfolio".to_string()
    } else {
        spec.strategy.strategy_id.clone()
    };
    let strategy_version = if spec.portfolio.is_some() {
        "portfolio-bar-v1".to_string()
    } else if let Some(version) = &injected_version {
        version.clone()
    } else {
        strategy_version(&spec.strategy)
    };
    let engine = match spec.protective_exit {
        Some(policy) => BacktestEngine::new(config)
            .with_protective_exits(ProtectiveExitPolicy {
                stop_loss_bps: policy.stop_loss_bps,
                take_profit_bps: policy.take_profit_bps,
                trailing_stop_bps: policy.trailing_stop_bps,
            })
            .map_err(|error| AppError::from_qx_error(&error))?,
        None => BacktestEngine::new(config),
    };
    let engine = if let Some(initial_cash_exact) = spec.initial_cash_exact {
        engine
            .with_initial_cash_exact(initial_cash_exact)
            .map_err(|error| AppError::from_qx_error(&error))?
    } else {
        engine
    };
    let (report, pruning) = if let Some(mut strategy) = bar_strategy {
        run_candidate_engine(
            engine,
            &bars,
            strategy.as_mut(),
            cancellation,
            drawdown_pruning,
        )?
    } else if spec.portfolio.is_none() && injected.is_empty() {
        let mut strategy = build_single_native_strategy(
            &spec.strategy,
            StrategySlotContext {
                priority: primary_priority,
                reserved_capital_raw: primary_allocation,
                reserved_capital_exact: primary_allocation_exact,
                instrument: &instrument,
                account_id: &account_id,
                currency: &currency,
                data_fingerprint: &data_fingerprint,
                as_of,
                strategy_version: injected_version.as_deref(),
            },
        )?;
        run_candidate_engine(engine, &bars, &mut strategy, cancellation, drawdown_pruning)?
    } else {
        let mut strategy_slots = vec![build_strategy_slot(
            &spec.strategy,
            StrategySlotContext {
                priority: primary_priority,
                reserved_capital_raw: primary_allocation,
                reserved_capital_exact: primary_allocation_exact,
                instrument: &instrument,
                account_id: &account_id,
                currency: &currency,
                data_fingerprint: &data_fingerprint,
                as_of,
                strategy_version: injected_version.as_deref(),
            },
            injected
                .remove(&spec.strategy.strategy_id)
                .map(|injected| injected.strategy),
        )?];
        if let Some(portfolio) = &spec.portfolio {
            for member in &portfolio.additional_strategies {
                let reserved_capital_exact = member
                    .reserved_capital()
                    .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
                strategy_slots.push(build_strategy_slot(
                    &member.strategy,
                    StrategySlotContext {
                        priority: member.priority,
                        reserved_capital_raw: member.reserved_capital_raw,
                        reserved_capital_exact,
                        instrument: &instrument,
                        account_id: &account_id,
                        currency: &currency,
                        data_fingerprint: &data_fingerprint,
                        as_of,
                        strategy_version: None,
                    },
                    injected
                        .remove(&member.strategy.strategy_id)
                        .map(|injected| injected.strategy),
                )?);
            }
        }
        let mut strategy = PortfolioBarStrategy::new(strategy_slots)
            .map_err(|error| AppError::from_qx_error(&error))?;
        run_candidate_engine(engine, &bars, &mut strategy, cancellation, drawdown_pruning)?
    };
    if let Some(pruning) = pruning {
        return Ok(SearchBacktestAttempt::Pruned(pruning));
    }
    let Some(report) = report else {
        return Ok(SearchBacktestAttempt::Completed(None));
    };

    // 重放自检：与 CLI 回测同一道闸——跑不过重放的那一轮没有资格往产物里写一个"看起来校验过"的哈希。
    let replay =
        ReplayVerifier::verify(report.event_log.events(), &report.ledger).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("回测事件日志未通过重放校验: {error}"),
            )
        })?;
    if replay.log_digest != report.result_hash() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            format!(
                "结果哈希与重放哈希不一致: result={:016x} replay={:016x}",
                report.result_hash(),
                replay.log_digest
            ),
        ));
    }
    let timestamps: Vec<u64> = bars.iter().map(|bar| bar.ts).collect();
    if timestamps.len() != report.equity.len()
        || timestamps.len() != report.equity_exact.len()
        || timestamps.len() != report.positions.len()
    {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            format!(
                "权益/持仓曲线点数 ({}/{}) 与样本根数 {} 不一致——研究曲线必须逐根对齐",
                report.equity.len(),
                report.positions.len(),
                timestamps.len()
            ),
        ));
    }

    let artifacts = artifact_paths(spec);
    let manifest = crate::build_backtest_run_manifest(
        &report,
        crate::BacktestRunManifestInput {
            run_id: &spec.run_id,
            code_commit: context.code_commit(),
            config_hash: &config_hash,
            strategy_version: &strategy_version,
            instrument_spec_version: &instrument_spec_version,
            runtime_version: &format!("qx-app-{}", env!("CARGO_PKG_VERSION")),
            data_fingerprint: &data_fingerprint,
            input_components: BTreeMap::new(),
        },
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("RunManifest 构建失败: {error}"),
        )
    })?;
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let canonical_run_spec = canonical_spec::build_bar_canonical_run_spec(
        spec,
        &identities,
        context.code_commit(),
        &data_fingerprint,
        &frame,
        &report,
        &runtime_version,
        &instrument_spec_version,
        &risk_rule_set_version,
        &account_id,
        identity_extensions.clone(),
    )?;
    let canonical_run_spec_json = canonical_run_spec.canonical_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("CanonicalRunSpec 序列化失败: {error}"),
        )
    })?;
    let canonical_run_spec_fingerprint = canonical_run_spec.fingerprint().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("CanonicalRunSpec 指纹计算失败: {error}"),
        )
    })?;
    reject_conflicting_run(
        spec,
        &artifacts.run_manifest,
        &config_hash,
        &data_fingerprint,
        &canonical_run_spec_fingerprint,
        !identity_extensions.is_empty(),
    )?;
    let legacy_manifest_without_canonical_spec = existing_manifest(&artifacts.run_manifest)?
        .map(|payload| RunManifest::from_json(&payload))
        .transpose()
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("已有 RunManifest 无法识别 CanonicalRunSpec 迁移状态: {error}"),
            )
        })?
        .is_some_and(|manifest| manifest.canonical_run_spec_json.is_none());
    let mut manifest = manifest;
    if !legacy_manifest_without_canonical_spec {
        manifest.canonical_run_spec_json = Some(canonical_run_spec_json);
        manifest.canonical_run_spec_fingerprint = Some(canonical_run_spec_fingerprint);
        manifest.validate().map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunManifest 的 CanonicalRunSpec 无效: {error}"),
            )
        })?;
    }
    ensure_output_dir(spec)?;
    let run_record_path = artifacts
        .summary
        .strip_suffix(".summary.json")
        .map(|stem| format!("{stem}.record.json"))
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!(
                    "回测摘要文件名无效，不能登记 RunRecord: {}",
                    artifacts.summary
                ),
            )
        })?;
    let summary = RunSummary {
        schema_version: SUMMARY_SCHEMA_VERSION,
        run_id: spec.run_id.clone(),
        instrument: spec.instrument.clone(),
        account_id: account_id.clone(),
        currency: spec.settlement_currency.clone(),
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        bars: timestamps.len() as u64,
        fills: report.fills.len() as u64,
        equity_points: report.equity.len() as u64,
        result_hash: format!("{:016x}", report.result_hash()),
        data_fingerprint: data_fingerprint.clone(),
        input: Some(input_provenance_json),
        input_data_hash: Some(manifest.input_event_hash.clone()),
        run_manifest: Some(artifacts.run_manifest.clone()),
        run_record: Some(run_record_path.clone()),
        artifact_locations: Some(serde_json::json!({
            "summary": artifacts.summary,
            "equity": artifacts.equity,
            "fills": artifacts.fills,
            "run_manifest": artifacts.run_manifest,
        })),
        config_hash: config_hash.clone(),
        matching_kernel: Some(crate::cases::kernels::BAR_MATCHING_KERNEL.to_string()),
        account: Some(serde_json::json!({
            "initial_cash_raw": spec.initial_cash_raw.to_string(),
            "initial_cash_exact": report.initial_cash_exact.to_string(),
            "source": "backtest-spec",
        })),
        risk_rules: Some(serde_json::json!({
            "rule_set_version": risk_rule_set_version.clone(),
            "source": if spec.risk_rules.is_some() { "runtime-config" } else { "conservative-default" },
        })),
        execution_costs: Some(serde_json::json!({
            "source": if spec.ashare_rules.is_some() {
                "ashare-rules"
            } else if spec.execution_cost_rules.is_some() {
                "execution-cost-rules"
            } else if spec.fee_schedule.is_some() {
                "fee-schedule"
            } else {
                "built-in-default"
            },
        })),
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        final_equity_raw: report.final_equity(),
        fees_raw: report.fees_raw,
        turnover_raw: report.turnover_raw,
        metrics_exact: Some(report.exact_metrics_json()),
        replay_events: replay.events as u64,
        replay: Some(RunReplaySummary {
            log_digest: format!("{:016x}", replay.log_digest),
            events: replay.events as u64,
            ledger_entries: replay.ledger_entries as u64,
            run_ledger_entries: report.ledger.entries().len() as u64,
        }),
        replay_ledger_entries: replay.ledger_entries as u64,
        ledger_entries: report.ledger.entries().len() as u64,
        clock_start: report.clock_start,
        clock_end: report.clock_end,
        seed: report.seed,
        risk_rule_set_version: risk_rule_set_version.clone(),
        fee_model,
        fill_model,
        fill_model_details: Some(report.fill_model_descriptor.clone()),
        data_tier: DATA_TIER.to_string(),
        rejection_facts: crate::trade_facts::rejection_facts_u64(&report.event_log),
        protective_exit_fills: report
            .protective_exit_fills
            .iter()
            .map(|fill| ProtectiveExitSummary {
                reason: match fill.reason {
                    ProtectiveExitReason::StopLoss => "stop_loss",
                    ProtectiveExitReason::TrailingStop => "trailing_stop",
                    ProtectiveExitReason::TakeProfit => "take_profit",
                }
                .into(),
                trigger_ts: fill.trigger_ts,
                fill_ts: fill.fill_ts,
                order_id: fill.order_id,
                qty_raw: fill.qty_raw,
                price_raw: fill.price_raw,
            })
            .collect(),
    };
    write_text(&artifacts.summary, &summary_json(&summary)?)?;
    let exact_curve_needed = report
        .equity
        .iter()
        .zip(&report.equity_exact)
        .any(|(raw, exact)| exact.to_raw(9).ok() != Some(*raw));
    let equity_payload = if exact_curve_needed {
        equity_csv_with_exact(
            &timestamps,
            &report.equity,
            &report.equity_exact,
            &report.positions,
        )?
    } else {
        equity_csv(&timestamps, &report.equity, &report.positions)
    };
    write_text(&artifacts.equity, &equity_payload)?;
    write_text(&artifacts.fills, &fills_csv(&report.fills)?)?;
    write_text(
        artifacts
            .event_trace
            .as_deref()
            .expect("Bar backtest trace path"),
        &event_trace_jsonl(report.event_log.events())?,
    )?;
    // 核心文件先写完，再发布 manifest 和 RunRecord；RunRecord 是这套身份文件的完成标记。
    write_text(
        &artifacts.run_manifest,
        &manifest.to_json().map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunManifest 序列化失败: {error}"),
            )
        })?,
    )?;
    let persisted_record_path = crate::artifact_store::persist_verified_run_record(
        Path::new(&artifacts.run_manifest),
        Path::new(&artifacts.summary),
        Path::new(&artifacts.equity),
        Path::new(&artifacts.fills),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("RunRecord 落盘失败: {error}"),
        )
    })?;
    if persisted_record_path.to_string_lossy() != run_record_path {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "RunRecord 实际落点与 summary 声明路径不一致",
        ));
    }

    let outcome = BacktestOutcome {
        run_id: spec.run_id.clone(),
        instrument: spec.instrument.clone(),
        result_hash: format!("{:016x}", report.result_hash()),
        data_fingerprint,
        fills: report.fills.len() as u64,
        equity_points: report.equity.len() as u64,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        artifacts,
        account_id: spec.account_id.clone(),
    };
    Ok(SearchBacktestAttempt::Completed(Some(BacktestRunDetails {
        outcome,
        report,
        risk_rule_set_version,
    })))
}

pub(super) fn run_backtest_inner_for_experiment(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Vec<InjectedStrategy>,
    pruning: Option<DrawdownPruningWindow>,
) -> Result<SearchBacktestAttempt<BacktestOutcome>, AppError> {
    Ok(
        match run_backtest_inner_unchecked_with_details(
            spec,
            context,
            cancellation,
            injected,
            None,
            pruning,
            BTreeMap::new(),
        )? {
            SearchBacktestAttempt::Completed(details) => {
                SearchBacktestAttempt::Completed(details.map(|details| details.outcome))
            }
            SearchBacktestAttempt::Pruned(pruning) => SearchBacktestAttempt::Pruned(pruning),
        },
    )
}

/// 规格摘要：`BacktestSpec` 的规范化 JSON 过一遍 FNV-1a。
///
/// 用**规范化 JSON** 而不是手抄一串字段：spec 加一格而这里忘了加，摘要就不再随那一格变化，
/// 于是"同一 run_id、不同规格"会退化成幂等重跑——正是 [`reject_conflicting_run`] 要拦的那件事。
fn spec_config_hash(spec: &BacktestSpec) -> Result<String, AppError> {
    let payload = spec.to_json()?;
    let mut hash = Fnv1a::new();
    hash.write_text(&payload);
    Ok(format!("{:016x}", hash.finish()))
}

fn market_spec_version(
    market: Option<&qx_core::TradingInstrumentSpec>,
) -> Result<String, AppError> {
    let Some(market) = market else {
        return Ok(NO_INSTRUMENT_SPEC.to_string());
    };
    let payload = serde_json::to_string(market).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("市场规格摘要序列化失败: {error}"),
        )
    })?;
    let mut hash = Fnv1a::new();
    hash.write_text(&payload);
    Ok(format!("inline-market-spec:{:016x}", hash.finish()))
}

/// 同一 `run_id` 已有产物时，只有**身份完全相同**才允许重写。
fn reject_conflicting_run(
    spec: &BacktestSpec,
    manifest_path: &str,
    config_hash: &str,
    data_fingerprint: &str,
    canonical_run_spec_fingerprint: &str,
    require_canonical_identity: bool,
) -> Result<(), AppError> {
    let Some(existing) = existing_manifest(manifest_path)? else {
        return Ok(());
    };
    let existing = RunManifest::from_json(&existing).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("既有 run manifest 无法解析 {manifest_path}: {error}"),
        )
    })?;
    let canonical_identity_matches = match existing.canonical_run_spec_fingerprint.as_deref() {
        Some(existing) => existing == canonical_run_spec_fingerprint,
        None => !require_canonical_identity,
    };
    if existing.config_hash == config_hash
        && existing.data_fingerprint == data_fingerprint
        && canonical_identity_matches
    {
        return Ok(());
    }
    Err(AppError::new(
        AppErrorCategory::Conflict,
        format!(
            "run_id {} 已存在且身份不同（config_hash {} -> {}、data_fingerprint {} -> {}、canonical_spec {} -> {}）；\
             换 run_id 或换产物目录——覆盖会让两次不同的运行共用一个身份，\
             而引用它的报告、对比与台账说的可能是前一次",
            spec.run_id,
            existing.config_hash,
            config_hash,
            existing.data_fingerprint,
            data_fingerprint,
            existing
                .canonical_run_spec_fingerprint
                .as_deref()
                .unwrap_or("legacy"),
            canonical_run_spec_fingerprint,
        ),
    ))
}

/// 内置策略参数被拒：那是**调用方的输入问题**，不是内部错误。
fn invalid_strategy(error: String) -> AppError {
    AppError::new(
        AppErrorCategory::InvalidInput,
        format!("内置策略参数非法: {error}"),
    )
}
