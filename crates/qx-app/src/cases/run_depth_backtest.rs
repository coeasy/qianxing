//! L1 Tick and L2 order-book application workflow.
//!
//! This use case only validates/assembles the request and persists evidence. The
//! Rust Tick, order-book matching, risk, OMS, ledger, and replay implementations
//! remain the single source of trading semantics.

use super::artifacts::{equity_csv, existing_manifest, fills_csv, read_text, write_text};
use super::{attach, guard::guard_panics};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext,
};
use qx_core::{Fnv1a, Money, Quantity, ReplayVerifier, RunManifest};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, StrategyContext};
use qx_xingban::{
    depth_frame_to_ticks, DataTier, DepthBarStrategy, DepthFrame, OrderBookBacktestConfig,
    OrderBookBacktestEngine, OrderBookExecutionModel, RunManifestIdentity, TickBacktestConfig,
    TickBacktestEngine,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;
use std::path::PathBuf;

const ACCOUNT_ID: &str = "app";

/// Execute a versioned L1/L2 run through the existing Rust engines and persist
/// the same four evidence artifacts used by Bar backtests.
pub fn run_depth_backtest(
    spec: &DepthBacktestSpec,
    context: &RunContext,
) -> Result<DepthBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Tick/OrderBook 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步 Tick/OrderBook 回测未产生报告",
                ),
                &correlation_id,
            )
        })
}

impl DepthBacktestSpec {
    /// Start a cooperative worker for this Tick/OrderBook specification.
    pub fn start(self, context: RunContext) -> crate::RunHandle<DepthBacktestOutcome> {
        let run_id = context.correlation_id().to_string();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "Tick/OrderBook 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

fn run_inner(
    spec: &DepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<DepthBacktestOutcome>, AppError> {
    spec.validate()?;
    let payload = read_text(&spec.depth_path)?;
    let frame = DepthFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("DepthFrame 校验失败 {}: {error}", spec.depth_path),
        )
    })?;
    if frame.snapshots.len() < 2 {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "Tick/OrderBook 回测至少需要两份快照",
        ));
    }
    if spec.latency_snapshots > 0
        && spec.latency_snapshots >= (frame.snapshots.len() as u64).saturating_sub(1)
    {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "latency_snapshots={} 在 {} 份快照上无法成熟成交；最大有效延迟为 {}",
                spec.latency_snapshots,
                frame.snapshots.len(),
                (frame.snapshots.len() as u64).saturating_sub(2)
            ),
        ));
    }
    if spec.tier == "l1"
        && frame
            .snapshots
            .iter()
            .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
    {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "L1 Tick 回测每份快照必须恰有一个 bid 和一个 ask 档位",
        ));
    }

    let instrument = frame.instrument.clone();
    let data_fingerprint = format!("depth:{}:{:016x}", frame.source, frame.input_hash());
    let config_hash = {
        let mut hash = Fnv1a::new();
        hash.write_text(&spec.to_json()?);
        format!("{:016x}", hash.finish())
    };
    let strategy_id = spec.strategy.strategy_id.clone();
    let strategy_version = format!("{}-v1", strategy_id);
    let risk = RiskGate::conservative_default();
    let risk_rule_set_version = risk.rule_set().version().to_string();
    let mut model = OrderBookExecutionModel::new(spec.fee_bps).map_err(invalid_input)?;
    model.latency_snapshots = spec.latency_snapshots;
    model.queue_position_bps = spec.queue_position_bps;
    model.market_impact_bps = spec.market_impact_bps;
    model.validate().map_err(invalid_input)?;

    let mut strategy_config = BuiltinStrategyConfig::new(
        spec.strategy.kind,
        strategy_id.clone(),
        instrument.clone(),
        Quantity::from_raw(spec.strategy.quantity_raw),
    )
    .map_err(invalid_input)?;
    strategy_config.fast_window = spec.strategy.fast_window;
    strategy_config.slow_window = spec.strategy.slow_window;
    strategy_config.period = spec.strategy.period;
    strategy_config.threshold_bps = spec.strategy.threshold_bps;
    strategy_config.validate().map_err(invalid_input)?;
    let strategy = BuiltinStrategy::new(strategy_config).map_err(invalid_input)?;
    let strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: ACCOUNT_ID.into(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: frame.snapshots[0].ts,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]),
        available_margin_raw: Some(spec.initial_cash_raw),
        risk_state: "backtest".into(),
    };
    let mut strategy = DepthBarStrategy::new(strategy, strategy_context, instrument.clone());
    strategy.initialize().map_err(invalid_input)?;

    let report = match spec.tier.as_str() {
        "l1" => {
            let ticks = depth_frame_to_ticks(&frame).map_err(invalid_input)?;
            let engine = TickBacktestEngine::new(TickBacktestConfig {
                instrument: instrument.clone(),
                account_id: ACCOUNT_ID.into(),
                currency: spec.settlement_currency.clone(),
                initial_cash: Money::from_raw(spec.initial_cash_raw),
                fee_bps: spec.fee_bps,
                instrument_spec: None,
                risk,
            })
            .with_execution_model(model);
            match cancellation {
                Some(cancellation) => engine
                    .run_with_cancel(&ticks, &mut strategy, || cancellation.is_cancelled())
                    .map_err(|error| AppError::from_qx_error(&error))?,
                None => Some(
                    engine
                        .run(&ticks, &mut strategy)
                        .map_err(|error| AppError::from_qx_error(&error))?,
                ),
            }
        }
        "l2" => {
            let engine = OrderBookBacktestEngine::new(OrderBookBacktestConfig {
                instrument: instrument.clone(),
                account_id: ACCOUNT_ID.into(),
                currency: spec.settlement_currency.clone(),
                initial_cash: Money::from_raw(spec.initial_cash_raw),
                fee_bps: spec.fee_bps,
                instrument_spec: None,
                risk,
                data_tier: DataTier::L2L3,
            })
            .with_execution_model(model);
            match cancellation {
                Some(cancellation) => engine
                    .run_with_cancel(&frame.snapshots, &mut strategy, || {
                        cancellation.is_cancelled()
                    })
                    .map_err(|error| AppError::from_qx_error(&error))?,
                None => Some(
                    engine
                        .run(&frame.snapshots, &mut strategy)
                        .map_err(|error| AppError::from_qx_error(&error))?,
                ),
            }
        }
        _ => unreachable!("DepthBacktestSpec::validate constrains tier"),
    };
    let Some(report) = report else {
        return Ok(None);
    };

    let replay =
        ReplayVerifier::verify(report.event_log.events(), &report.ledger).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook 事件日志未通过重放校验: {error}"),
            )
        })?;
    if replay.log_digest != report.result_hash() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook 结果哈希与重放哈希不一致",
        ));
    }
    if report.snapshot_ts.len() != report.equity.len() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook 权益曲线与输入快照数量不一致",
        ));
    }

    let artifacts = artifact_paths(spec);
    if let Some(existing) = existing_manifest(&artifacts.run_manifest)? {
        let existing = RunManifest::from_json(&existing).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("既有 RunManifest 无法解析: {error}"),
            )
        })?;
        if existing.config_hash != config_hash || existing.data_fingerprint != data_fingerprint {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("run_id {} 已绑定到不同的 Tick/OrderBook 输入", spec.run_id),
            ));
        }
    }
    let manifest = report
        .run_manifest(
            RunManifestIdentity {
                run_id: &spec.run_id,
                code_commit: context.code_commit(),
                config_hash: &config_hash,
                strategy_version: &strategy_version,
                instrument_spec_version: "instrument-spec:none",
                runtime_version: &format!("qx-app-{}", env!("CARGO_PKG_VERSION")),
            },
            &data_fingerprint,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook RunManifest 构建失败: {error}"),
            )
        })?;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io(&format!("创建产物目录 {}", spec.output_dir), &error))?;

    let summary = serde_json::json!({
        "schema_version": 1,
        "run_id": spec.run_id,
        "instrument": instrument.to_string(),
        "tier": spec.tier,
        "strategy_id": strategy_id,
        "strategy_version": strategy_version,
        "snapshots": report.snapshot_ts.len(),
        "fills": report.fills.len(),
        "equity_points": report.equity.len(),
        "result_hash": format!("{:016x}", report.result_hash()),
        "data_fingerprint": data_fingerprint,
        "config_hash": config_hash,
        "return_bps": report.return_bps,
        "max_drawdown_bps": report.max_drawdown_bps,
        "final_equity_raw": report.final_equity(),
        "fees_raw": report.fees_raw,
        "turnover_raw": report.turnover_raw,
        "replay_events": replay.events,
        "replay_ledger_entries": replay.ledger_entries,
        "ledger_entries": report.ledger.entries().len(),
        "risk_rule_set_version": risk_rule_set_version,
        "model_descriptors": report.model_descriptors,
        "assumptions": report.assumptions,
    });
    let summary = serde_json::to_string(&summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Tick/OrderBook 摘要序列化失败: {error}"),
        )
    })?;
    publish(&artifacts.summary, &summary)?;
    publish(
        &artifacts.equity,
        &equity_csv(&report.snapshot_ts, &report.equity),
    )?;
    publish(&artifacts.fills, &fills_csv(&report.fills))?;
    let manifest_json = manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Tick/OrderBook RunManifest 序列化失败: {error}"),
        )
    })?;
    publish(&artifacts.run_manifest, &manifest_json)?;

    Ok(Some(DepthBacktestOutcome {
        run_id: spec.run_id.clone(),
        instrument: instrument.to_string(),
        tier: spec.tier.clone(),
        result_hash: format!("{:016x}", report.result_hash()),
        data_fingerprint,
        fills: report.fills.len() as u64,
        equity_points: report.equity.len() as u64,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        artifacts,
    }))
}

fn artifact_paths(spec: &DepthBacktestSpec) -> crate::BacktestArtifacts {
    let root = PathBuf::from(&spec.output_dir);
    let named = |suffix: &str| {
        root.join(format!("{}.{suffix}", spec.run_id))
            .to_string_lossy()
            .into_owned()
    };
    crate::BacktestArtifacts {
        run_manifest: named("run.json"),
        summary: named("summary.json"),
        equity: named("equity.csv"),
        fills: named("fills.csv"),
    }
}

fn publish(path: &str, body: &str) -> Result<(), AppError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == body => Ok(()),
        Ok(_) => Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("同一运行产物已存在不同内容: {path}"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match write_text(path, body) {
                Ok(()) => Ok(()),
                Err(write_error) => match std::fs::read_to_string(path) {
                    Ok(existing) if existing == body => Ok(()),
                    Ok(_) => Err(AppError::new(
                        AppErrorCategory::Conflict,
                        format!("并发运行已发布不同内容的产物: {path}"),
                    )),
                    Err(_) => Err(write_error),
                },
            }
        }
        Err(error) => Err(AppError::from_io(
            &format!("读取已有运行产物 {path}"),
            &error,
        )),
    }
}

fn invalid_input(message: impl std::fmt::Display) -> AppError {
    AppError::new(
        AppErrorCategory::InvalidInput,
        format!("Tick/OrderBook 输入或策略非法: {message}"),
    )
}
