//! `RunBacktest` 用例（T2-2，方案 §5 阶段 2、§15.3-A）。
//!
//! 先校验 spec 与输入，再用具名模型装配唯一的 `BacktestEngine`；运行后校验 replay hash，
//! 冲突检查通过后写四份产物。数据不足返回 `FidelityInsufficient`，无效或缺失输入不落产物。
//!
//! ## 为什么冲突检查排在写之前
//!
//! 同一 `run_id` 仅当 `config_hash` 与 `data_fingerprint` 都相同才允许幂等重跑；否则
//! 返回 `Conflict`，避免两个不同运行共用一个身份。
use crate::cases::artifacts::{
    artifact_paths, ensure_output_dir, equity_csv, existing_manifest, fills_csv, read_text,
    summary_json, write_text, ProtectiveExitSummary, RunSummary, DATA_TIER, SUMMARY_SCHEMA_VERSION,
};
use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::run_handle::CancellationToken;
use crate::spec::{BacktestOutcome, BacktestSpec, MIN_BACKTEST_BARS};
use qx_core::{Fnv1a, InstrumentId, Money, ReplayVerifier, RunManifest};
use qx_datastruct::BarFrame;
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, DataTier, PortfolioBarStrategy,
    ProtectiveExitPolicy, ProtectiveExitReason,
};
use std::collections::BTreeMap;

mod bar_strategy;
mod cancellation;
mod custom_strategy;
mod execution_binding;
pub(in crate::cases) mod strategy_slot;
use crate::cases::risk_gate::backtest_risk_gate;
pub(in crate::cases) use bar_strategy::run_backtest_inner_with_bar_strategy;
pub(super) use bar_strategy::BacktestRunDetails;
use cancellation::run_engine;
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
    context.require(CallerCapability::Research, "Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    let outcome = guard_panics(&correlation_id, || run_backtest_inner(spec, context, None))
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
    run_backtest_inner_with_strategies(spec, context, cancellation, Vec::new())
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
) -> Result<Option<BacktestRunDetails>, AppError> {
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
    // `Bar` 的类型不在这里点名：转换目标就是引擎要吃的那个类型，写死等于给两处留一个漂移点。
    let bars: Vec<_> = (&frame).into();
    if spec.market_spec.as_ref().is_some_and(|market| {
        bars.iter().any(|bar| {
            bar.ts < market.valid_from || market.valid_to.is_some_and(|until| bar.ts >= until)
        })
    }) {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "Bar 数据时间超出 BacktestSpec.market_spec 的有效期",
        ));
    }
    if bars.len() < MIN_BACKTEST_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "样本过短：{} 根 Bar，Bar 回测下限 {} 根",
                bars.len(),
                MIN_BACKTEST_BARS
            ),
        ));
    }
    let data_fingerprint = format!(
        "barframe:{}",
        qx_data::fingerprint_bar_frame(&frame).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BarFrame 内容指纹计算失败：{error}"),
            )
        })?
    );
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
    let report = if let Some(mut strategy) = bar_strategy {
        run_engine(engine, &bars, strategy.as_mut(), cancellation)?
    } else if spec.portfolio.is_none() && injected.is_empty() {
        let mut strategy = build_single_native_strategy(
            &spec.strategy,
            StrategySlotContext {
                priority: primary_priority,
                reserved_capital_raw: primary_allocation,
                instrument: &instrument,
                account_id: &account_id,
                currency: &currency,
                data_fingerprint: &data_fingerprint,
                as_of,
                strategy_version: injected_version.as_deref(),
            },
        )?;
        run_engine(engine, &bars, &mut strategy, cancellation)?
    } else {
        let mut strategy_slots = vec![build_strategy_slot(
            &spec.strategy,
            StrategySlotContext {
                priority: primary_priority,
                reserved_capital_raw: primary_allocation,
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
                strategy_slots.push(build_strategy_slot(
                    &member.strategy,
                    StrategySlotContext {
                        priority: member.priority,
                        reserved_capital_raw: member.reserved_capital_raw,
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
        run_engine(engine, &bars, &mut strategy, cancellation)?
    };
    let Some(report) = report else {
        return Ok(None);
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
    if timestamps.len() != report.equity.len() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            format!(
                "权益曲线点数 {} 与样本根数 {} 不一致——权益曲线必须与样本逐根对应",
                report.equity.len(),
                timestamps.len()
            ),
        ));
    }

    let artifacts = artifact_paths(spec);
    reject_conflicting_run(
        spec,
        &artifacts.run_manifest,
        &config_hash,
        &data_fingerprint,
    )?;
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
    ensure_output_dir(spec)?;
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
        config_hash: config_hash.clone(),
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        final_equity_raw: report.final_equity(),
        fees_raw: report.fees_raw,
        turnover_raw: report.turnover_raw,
        replay_events: replay.events as u64,
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
    write_text(&artifacts.equity, &equity_csv(&timestamps, &report.equity))?;
    write_text(&artifacts.fills, &fills_csv(&report.fills))?;
    // 最后发布 manifest，避免 verify 把首次写入中途失败的半套产物当成已完成运行。
    write_text(
        &artifacts.run_manifest,
        &manifest.to_json().map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunManifest 序列化失败: {error}"),
            )
        })?,
    )?;

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
    Ok(Some(BacktestRunDetails {
        outcome,
        report,
        risk_rule_set_version,
    }))
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
    if existing.config_hash == config_hash && existing.data_fingerprint == data_fingerprint {
        return Ok(());
    }
    Err(AppError::new(
        AppErrorCategory::Conflict,
        format!(
            "run_id {} 已存在且身份不同（config_hash {} -> {}、data_fingerprint {} -> {}）；\
             换 run_id 或换产物目录——覆盖会让两次不同的运行共用一个身份，\
             而引用它的报告、对比与台账说的可能是前一次",
            spec.run_id,
            existing.config_hash,
            config_hash,
            existing.data_fingerprint,
            data_fingerprint
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
