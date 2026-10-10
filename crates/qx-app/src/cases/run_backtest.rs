//! `RunBacktest` 用例（T2-2，方案 §5 阶段 2、§15.3-A）。
//!
//! 一次 Bar 回测在这里被拆成六步，顺序是刻意的：
//!
//! 1. **校验规格**（`spec.validate()`）——形状不对就不该碰磁盘。
//! 2. **读输入并复核身份**——标的、时间戳、样本量；读不到是 `DataUnavailable`，
//!    读到了但不够用是 `FidelityInsufficient`。
//! 3. **装配**——撮合/费率/保证金/风控门/延迟各取一个**具名**模型。这里没有"默认"这个
//!    选项：`BacktestConfig` 的每一格都由本文件点名，所以产物里的口径与代码里的口径是同一份。
//! 4. **跑引擎**——`BacktestEngine` 是唯一的撮合实现，应用层不复制它。
//! 5. **重放自检**——`ReplayVerifier::verify` 必须过，且重放摘要必须等于结果哈希。
//!    过不了就不落盘（与 CLI 回测同一道闸）。这一步是"可复算"从口号变成门的地方。
//! 6. **落四份产物**——先查冲突（同一 `run_id` 已有不同内容 → `Conflict`），再写。
//!
//! ## 为什么冲突检查排在写之前
//!
//! 覆盖既有运行等于让两次**不同**的运行共用一个身份：事后拿 `run_id` 去查，拿到的是后一次，
//! 而引用它的报告、对比、台账说的可能是前一次。所以同一 `run_id` 只有在
//! `config_hash` 与 `data_fingerprint` 都相同时才允许重写（那才是幂等重跑），
//! 否则当场 `Conflict`——让调用方换 `run_id` 或换产物目录，而不是悄悄改写历史。

use crate::cases::artifacts::{
    artifact_paths, ensure_output_dir, equity_csv, existing_manifest, fills_csv, read_text,
    summary_json, write_text, RunSummary, DATA_TIER, SUMMARY_SCHEMA_VERSION,
};
use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestOutcome, BacktestSpec, MIN_BACKTEST_BARS};
use qx_core::{Fnv1a, InstrumentId, Money, Quantity, ReplayVerifier, RunManifest};
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, StrategyContext};
use qx_xingban::{
    BacktestConfig, BacktestEngine, DataTier, FeeModel, FillModel, MakerTakerFeeModel,
    NativeBarStrategy, NextBarOpenFillModel, NoMargin, RunManifestIdentity, VirtualTradingConfig,
    ZeroLatency,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;

/// 回测账簿的账户名。应用层只有一本账，所以它是一个常量而不是 spec 的一格——
/// 多账户是另一件事（V13 R28），不该靠这一格顺手塞进来。
const ACCOUNT_ID: &str = "app";

/// 本用例**不读** market spec（T2-3 才接入规格与保证金），所以这一格写的是"没有规格"
/// 这件事本身。写一个像规格名的字符串（`"v1"`）会让读者以为查过规格了。
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
    let outcome = guard_panics(&correlation_id, || run_backtest_inner(spec, context));
    outcome.map_err(|error| attach(error, &correlation_id))
}

fn run_backtest_inner(
    spec: &BacktestSpec,
    context: &RunContext,
) -> Result<BacktestOutcome, AppError> {
    spec.validate()?;
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
    let data_fingerprint = format!("barframe:{:016x}", frame.digest());
    let config_hash = spec_config_hash(spec)?;

    let risk = RiskGate::conservative_default();
    let risk_rule_set_version = risk.rule_set().version().to_string();
    let fee = MakerTakerFeeModel::default_maker_taker();
    let fee_model = fee.name().to_string();
    let fill_model = NextBarOpenFillModel.name().to_string();
    let config = BacktestConfig {
        instrument: instrument.clone(),
        instrument_spec: None,
        account_id: ACCOUNT_ID.to_string(),
        currency: spec.settlement_currency.clone(),
        initial_cash: Money::from_raw(spec.initial_cash_raw),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(fee),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: spec.seed,
        risk,
        virtual_trading: VirtualTradingConfig::default(),
    };
    let mut strategy_config = BuiltinStrategyConfig::new(
        spec.strategy.kind,
        spec.strategy.strategy_id.clone(),
        instrument.clone(),
        Quantity::from_raw(spec.strategy.quantity_raw),
    )
    .map_err(invalid_strategy)?;
    strategy_config.fast_window = spec.strategy.fast_window;
    strategy_config.slow_window = spec.strategy.slow_window;
    strategy_config.period = spec.strategy.period;
    strategy_config.threshold_bps = spec.strategy.threshold_bps;
    strategy_config.validate().map_err(invalid_strategy)?;
    let strategy_id = strategy_config.strategy_id.clone();
    let strategy_version = strategy_config.strategy_version.clone();

    let as_of = bars.first().map(|bar| bar.ts).unwrap_or(1);
    let strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: config.account_id.clone(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(config.currency.clone(), config.initial_cash.raw())]),
        available_margin_raw: Some(config.initial_cash.raw()),
        risk_state: "backtest".into(),
    };
    let mut strategy = NativeBarStrategy::new(
        BuiltinStrategy::new(strategy_config).map_err(invalid_strategy)?,
        strategy_context,
    );
    strategy
        .initialize()
        .map_err(|error| AppError::from_qx_error(&error))?;
    let report = BacktestEngine::new(config)
        .run(&bars, &mut strategy)
        .map_err(|error| AppError::from_qx_error(&error))?;

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
    let manifest = report
        .run_manifest_with_input_components(
            RunManifestIdentity {
                run_id: &spec.run_id,
                code_commit: context.code_commit(),
                config_hash: &config_hash,
                strategy_version: &strategy_version,
                instrument_spec_version: NO_INSTRUMENT_SPEC,
                runtime_version: &format!("qx-app-{}", env!("CARGO_PKG_VERSION")),
            },
            &data_fingerprint,
            BTreeMap::new(),
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
        account_id: ACCOUNT_ID.to_string(),
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
        risk_rule_set_version,
        fee_model,
        fill_model,
        data_tier: DATA_TIER.to_string(),
    };
    write_text(&artifacts.summary, &summary_json(&summary)?)?;
    write_text(&artifacts.equity, &equity_csv(&timestamps, &report.equity))?;
    write_text(&artifacts.fills, &fills_csv(&report.fills))?;
    // manifest 是这一轮完整性的提交标记。先原子发布数据文件，最后发布 manifest，
    // 这样首次写入中途失败时 verify 不会看到一份指向半套产物的“已完成”运行。
    write_text(
        &artifacts.run_manifest,
        &manifest.to_json().map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunManifest 序列化失败: {error}"),
            )
        })?,
    )?;

    Ok(BacktestOutcome {
        run_id: spec.run_id.clone(),
        instrument: spec.instrument.clone(),
        result_hash: format!("{:016x}", report.result_hash()),
        data_fingerprint,
        fills: report.fills.len() as u64,
        equity_points: report.equity.len() as u64,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        artifacts,
    })
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
