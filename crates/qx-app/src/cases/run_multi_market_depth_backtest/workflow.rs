use super::super::artifacts::{read_text, write_text};
use crate::{
    multi_market_depth_input_set_identity_v1, multi_market_depth_spec_fingerprint_v1, AppError,
    AppErrorCategory, MultiMarketDepthBacktestMarket, MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec, MultiMarketDepthInputIdentityV1, RunContext,
    StrategyArtifactIdentity,
};
use qx_core::{Money, Quantity, ReplayVerifier};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{
    DepthBarStrategy, DepthFrame, NativeOrderBookStrategy, OrderBookExecutionModel,
    OrderBookPortfolioBacktestEngine, OrderBookPortfolioMarket, OrderBookStrategy,
};
use qx_zhenlu::RiskGate;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

mod execution_models;
use super::canonical_spec::build_multi_market_depth_canonical_run_spec;
use execution_models::from_report as execution_model_descriptors;

pub(super) struct InjectedStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

#[derive(Serialize)]
struct DataIdentity {
    instrument: String,
    path: String,
    data_tier: String,
    source: String,
    content_hash: String,
    snapshot_count: usize,
    start_ts_ms: u64,
    end_ts_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    dataset_quality: Option<serde_json::Value>,
}

struct PreparedMarket {
    request: MultiMarketDepthBacktestMarket,
    frame: DepthFrame,
    data_identity: DataIdentity,
    quality_manifest_fingerprint: Option<String>,
    execution_model: OrderBookExecutionModel,
    strategy: Box<dyn OrderBookStrategy>,
}

enum MarketStrategy {
    Builtin(Box<BuiltinStrategy>),
    Callback(Box<dyn Strategy>),
}

pub(super) fn run_inner(
    spec: &MultiMarketDepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<MultiMarketDepthBacktestOutcome>, AppError> {
    run_inner_with_strategies(spec, context, cancellation, BTreeMap::new())
}

pub(super) fn run_inner_with_strategies(
    spec: &MultiMarketDepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    mut injected: BTreeMap<String, InjectedStrategy>,
) -> Result<Option<MultiMarketDepthBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;
    let initial_cash_exact = spec.initial_cash_exact_value()?;
    let mut identities = Vec::new();
    let mut prepared = Vec::with_capacity(spec.markets.len());

    for request in &spec.markets {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let payload = read_text(&request.depth_path)?;
        let frame = DepthFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} DepthFrame 无效: {error}",
                request.instrument
            ))
        })?;
        if frame.instrument.to_string() != request.instrument {
            return Err(invalid(format!(
                "市场 {} 的深度帧实际标的是 {}",
                request.instrument, frame.instrument
            )));
        }
        let data_tier = request.data_tier.as_deref().unwrap_or("l2");
        if data_tier == "l1"
            && frame
                .snapshots
                .iter()
                .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
        {
            return Err(invalid(format!(
                "市场 {} 声明 l1，但至少一份快照不是单档 bid/ask",
                request.instrument
            )));
        }
        if request.latency_snapshots > 0
            && request.latency_snapshots >= (frame.snapshots.len() as u64).saturating_sub(1)
        {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "市场 {} 的 latency_snapshots 无法在当前样本中成熟成交",
                    request.instrument
                ),
            ));
        }
        let (Some(first), Some(last)) = (frame.snapshots.first(), frame.snapshots.last()) else {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要两份盘口快照", request.instrument),
            ));
        };
        if first.ts < request.market_spec.valid_from
            || request
                .market_spec
                .valid_to
                .is_some_and(|valid_to| last.ts >= valid_to)
        {
            return Err(invalid(format!(
                "市场 {} 的产品规格有效区间未覆盖深度帧时间范围",
                request.instrument
            )));
        }
        let frame_hash = frame.input_hash();
        let data_fingerprint = format!("depth:{data_tier}:{}:{frame_hash:016x}", frame.source);
        let dataset_quality = request
            .dataset_manifest_path
            .as_deref()
            .map(|manifest_path| {
                verify_depth_dataset_quality(manifest_path, &request.depth_path, &frame, data_tier)
            })
            .transpose()?;
        let quality_manifest_fingerprint = dataset_quality
            .as_ref()
            .and_then(|quality| quality.get("manifest_fingerprint"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        let external = injected.remove(&request.strategy.strategy_id);
        let (strategy_id, strategy_version, strategy) = match external {
            Some(value) => {
                identities.push(value.identity.clone());
                (
                    value.identity.strategy_id.clone(),
                    value.identity.strategy_version.clone(),
                    MarketStrategy::Callback(value.strategy),
                )
            }
            None => {
                let config = BuiltinStrategyConfig {
                    fast_window: request.strategy.fast_window,
                    slow_window: request.strategy.slow_window,
                    period: request.strategy.period,
                    threshold_bps: request.strategy.threshold_bps,
                    target_notional_raw: request.strategy.target_notional_raw,
                    target_available_margin_bps: request.strategy.target_available_margin_bps,
                    target_volatility_bps: request.strategy.target_volatility_bps,
                    target_kelly_fraction_bps: request.strategy.target_kelly_fraction_bps,
                    kelly_lookback_bars: request.strategy.kelly_lookback_bars,
                    primary_policy: request.order_policy,
                    ..BuiltinStrategyConfig::new(
                        request.strategy.kind,
                        request.strategy.strategy_id.clone(),
                        frame.instrument.clone(),
                        Quantity::from_raw(request.strategy.quantity_raw),
                    )
                    .map_err(invalid)?
                };
                config.validate().map_err(invalid)?;
                (
                    request.strategy.strategy_id.clone(),
                    format!("{}-v1", request.strategy.strategy_id),
                    MarketStrategy::Builtin(Box::new(
                        BuiltinStrategy::new(config).map_err(invalid)?,
                    )),
                )
            }
        };
        let cash = BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]);
        let cash_exact = spec
            .initial_cash_exact
            .map(|amount| BTreeMap::from([(spec.settlement_currency.clone(), amount)]))
            .unwrap_or_default();
        let strategy_context = StrategyContext {
            strategy_id,
            strategy_version,
            account_id: spec.account_id.clone(),
            venue_id: frame.instrument.venue.to_string(),
            data_fingerprint,
            as_of: first.ts,
            positions_exact: std::collections::BTreeMap::new(),
            cash_exact,
            available_margin_exact: spec.initial_cash_exact,
            positions: BTreeMap::new(),
            available_margin_raw: crate::cases::strategy_available_margin_raw(
                &cash,
                &spec.settlement_currency,
            ),
            cash,
            risk_state: "backtest".into(),
        };
        let tier = if data_tier == "l1" {
            qx_xingban::DataTier::L1
        } else {
            qx_xingban::DataTier::L2L3
        };
        let strategy: Box<dyn OrderBookStrategy> = match strategy {
            MarketStrategy::Builtin(strategy) => {
                let mut adapter =
                    DepthBarStrategy::new(*strategy, strategy_context, frame.instrument.clone());
                adapter.initialize().map_err(invalid)?;
                Box::new(adapter)
            }
            MarketStrategy::Callback(strategy) => {
                let mut adapter =
                    NativeOrderBookStrategy::new(CallbackStrategy(strategy), strategy_context)
                        .with_data_tier(tier);
                adapter.initialize().map_err(invalid)?;
                Box::new(adapter)
            }
        };
        let model = request.execution_model()?;
        let data_identity = DataIdentity {
            instrument: request.instrument.clone(),
            path: std::path::Path::new(&request.depth_path)
                .canonicalize()
                .map_err(|error| AppError::from_io("解析多市场盘口输入路径", &error))?
                .to_string_lossy()
                .to_string(),
            data_tier: data_tier.into(),
            source: frame.source.clone(),
            content_hash: format!("{frame_hash:016x}"),
            snapshot_count: frame.snapshots.len(),
            start_ts_ms: first.ts,
            end_ts_ms: last.ts,
            dataset_quality,
        };
        prepared.push(PreparedMarket {
            request: request.clone(),
            frame,
            data_identity,
            quality_manifest_fingerprint,
            execution_model: model,
            strategy,
        });
    }
    if !injected.is_empty() {
        return Err(invalid("多市场盘口自定义策略包含未消费的孤儿槽"));
    }
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let spec_fingerprint = multi_market_depth_spec_fingerprint_v1(spec, &identities)?;
    let depth_input_identities = prepared
        .iter()
        .map(|market| MultiMarketDepthInputIdentityV1 {
            instrument: market.data_identity.instrument.clone(),
            data_tier: market.data_identity.data_tier.clone(),
            source: market.data_identity.source.clone(),
            input_hash: market.frame.input_hash(),
            start_ts_ms: market.data_identity.start_ts_ms,
            end_ts_ms: market.data_identity.end_ts_ms,
            quality_fingerprint: market.quality_manifest_fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    let depth_input_identity =
        multi_market_depth_input_set_identity_v1(spec, &depth_input_identities)?;
    let data_fingerprint = depth_input_identity.data_fingerprint.clone();
    let funding_bps = prepared
        .iter()
        .filter(|market| market.request.funding_bps != 0)
        .map(|market| {
            (
                market.request.market_spec.instrument.clone(),
                market.request.funding_bps,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let funding_events = prepared
        .iter()
        .flat_map(|market| market.request.funding_events.iter().cloned())
        .collect::<Vec<_>>();
    let mut markets = prepared
        .iter_mut()
        .map(|market| OrderBookPortfolioMarket {
            instrument_spec: market.request.market_spec.clone(),
            data_tier: if market.request.data_tier.as_deref() == Some("l1") {
                qx_xingban::DataTier::L1
            } else {
                qx_xingban::DataTier::L2L3
            },
            priority: market.request.priority,
            snapshots: &market.frame.snapshots,
            execution_model: market.execution_model,
            strategy: market.strategy.as_mut(),
        })
        .collect::<Vec<_>>();
    let engine = if spec.initial_cash_exact.is_some() {
        OrderBookPortfolioBacktestEngine::new_with_exact_initial_cash(
            spec.account_id.clone(),
            spec.settlement_currency.clone(),
            spec.initial_cash_raw,
            initial_cash_exact,
            RiskGate::conservative_default(),
            std::mem::take(&mut markets),
        )
    } else {
        OrderBookPortfolioBacktestEngine::new(
            spec.account_id.clone(),
            spec.settlement_currency.clone(),
            Money::from_raw(spec.initial_cash_raw),
            RiskGate::conservative_default(),
            std::mem::take(&mut markets),
        )
    }
    .map_err(|error| AppError::from_qx_error(&error))?
    .with_funding_bps(funding_bps)
    .with_funding_events(funding_events);
    let report = engine
        .run_with_cancel(|| cancellation.is_some_and(|token| token.is_cancelled()))
        .map_err(|error| AppError::from_qx_error(&error))?;
    let Some(report) = report else {
        return Ok(None);
    };
    let execution_models = execution_model_descriptors(&report);
    report
        .event_log
        .validate()
        .map_err(|error| AppError::from_qx_error(&error))?;
    let replay = ReplayVerifier::verify(report.event_log.events(), &report.ledger)
        .map_err(|error| AppError::from_qx_error(&error))?;
    if replay.log_digest != report.result_hash() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场盘口回测结果哈希与 ReplayVerifier 不一致",
        ));
    }
    if report.equity.len() != report.equity_exact.len()
        || report
            .equity
            .iter()
            .zip(&report.equity_exact)
            .any(|((raw_ts, _), (exact_ts, _))| raw_ts != exact_ts)
    {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场盘口 raw/exact 权益曲线坐标不一致",
        ));
    }
    let final_equity_raw = report
        .equity
        .last()
        .map(|(_, value)| *value)
        .unwrap_or(spec.initial_cash_raw);
    let return_bps = report.return_bps;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多市场盘口产物目录", &error))?;
    let output_root = PathBuf::from(&spec.output_dir)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场盘口产物目录", &error))?;
    let outcome_path = output_root.join(format!("{}.multi-market-depth.json", spec.run_id));
    let manifest_path = output_root.join(format!("{}.multi-market-depth.run.json", spec.run_id));
    let equity_path = output_root.join(format!("{}.multi-market-depth.equity.json", spec.run_id));
    let equity_exact_path = output_root.join(format!(
        "{}.multi-market-depth.equity-exact.json",
        spec.run_id
    ));
    let fills_path = output_root.join(format!("{}.multi-market-depth.fills.json", spec.run_id));
    let event_trace_path =
        output_root.join(format!("{}.multi-market-depth.events.jsonl", spec.run_id));
    let artifact_path = outcome_path.to_string_lossy().into_owned();
    let outcome = MultiMarketDepthBacktestOutcome {
        run_id: spec.run_id.clone(),
        account_id: spec.account_id.clone(),
        spec_fingerprint: spec_fingerprint.clone(),
        data_fingerprint: data_fingerprint.clone(),
        shared_result_hash: format!("{:016x}", report.result_hash()),
        markets_total: spec.markets.len(),
        successful_fills: u64::try_from(report.fills.len())
            .map_err(|_| invalid("多市场盘口成交数量超出 u64 范围"))?,
        pending_orders: report.pending_orders,
        equity_points: u64::try_from(report.equity.len())
            .map_err(|_| invalid("多市场盘口权益点数超出 u64 范围"))?,
        final_equity_raw,
        return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        execution_models,
        artifact_path: artifact_path.clone(),
    };
    let per_market_fills = report
        .fills_by_instrument
        .iter()
        .map(|(instrument, fills)| (instrument.to_string(), fills.len()))
        .collect::<BTreeMap<_, _>>();
    let data_identities = prepared
        .iter()
        .map(|market| &market.data_identity)
        .collect::<Vec<_>>();
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let config_bytes = serde_json::to_vec(&identity_spec)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let canonical_spec = build_multi_market_depth_canonical_run_spec(
        spec,
        &identities,
        &depth_input_identities,
        &outcome.execution_models,
        context.code_commit(),
        &runtime_version,
        report.clock_start,
        report.clock_end,
    )?;
    let canonical_spec_json = canonical_spec
        .canonical_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let canonical_spec_fingerprint = canonical_spec
        .fingerprint()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let input_components = depth_input_identity.input_components;
    let manifest = qx_core::RunManifest {
        run_id: spec.run_id.clone(),
        code_commit: context.code_commit().to_string(),
        config_hash: qx_strategy::sha256_hex(&config_bytes),
        data_fingerprint: data_fingerprint.clone(),
        input_components,
        clock_start: report.clock_start,
        clock_end: report.clock_end,
        global_seed: 0,
        determinism_mode: true,
        result_hash: format!("{:016x}", report.result_hash()),
        strategy_version: format!("multi-market-depth:{}", spec_fingerprint),
        instrument_spec_version: "multi-market-depth-spec-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(
            b"OrderBookPortfolioBacktestEngine/OrderBookExecutionModel/v1",
        ),
        input_event_hash: data_fingerprint.clone(),
        output_event_hash: format!("{:016x}", report.event_log.digest()),
        runtime_version,
        canonical_run_spec_json: Some(canonical_spec_json),
        canonical_run_spec_fingerprint: Some(canonical_spec_fingerprint),
    };
    let manifest_json = manifest
        .to_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let equity_json = serde_json::to_string(&report.equity)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let equity_exact_json = serde_json::to_string(
        &report
            .equity_exact
            .iter()
            .enumerate()
            .map(|(equity_index, (ts, equity_exact))| {
                serde_json::json!({
                    "equity_index": equity_index,
                    "ts": ts,
                    "equity_exact": equity_exact.to_string()
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let metrics_exact = report
        .exact_metrics_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let fills_json = serde_json::to_string(&report.fills)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let manifest_artifact_path = manifest_path.to_string_lossy().into_owned();
    let equity_artifact_path = equity_path.to_string_lossy().into_owned();
    let equity_exact_artifact_path = equity_exact_path.to_string_lossy().into_owned();
    let fills_artifact_path = fills_path.to_string_lossy().into_owned();
    let event_trace_artifact_path = event_trace_path.to_string_lossy().into_owned();
    let artifact_json = serde_json::to_string(&serde_json::json!({
        "schema_version": 4,
        "spec": spec,
        "outcome": outcome,
        "market_data": data_identities,
        "strategy_identities": identities,
        "per_market_fills": per_market_fills,
        "equity": report.equity,
        "equity_exact": report.equity_exact.iter().enumerate().map(|(equity_index, (ts, equity_exact))| serde_json::json!({
            "equity_index": equity_index,
            "ts": ts,
            "equity_exact": equity_exact.to_string()
        })).collect::<Vec<_>>(),
        "metrics_exact": metrics_exact,
        "event_count": report.event_log.events().len(),
        "clock_start": report.clock_start,
        "clock_end": report.clock_end,
        "event_trace_path": event_trace_artifact_path,
        "ledger_entries": report.ledger.entries(),
        "replay_verified": true,
        "run_manifest_path": manifest_artifact_path,
        "equity_path": equity_artifact_path,
        "equity_exact_path": equity_exact_artifact_path,
        "fills_path": fills_artifact_path,
    }))
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let manifest_exists = super::super::run_multi_market_manifest::existing_identical(
        &manifest_path,
        &manifest_json,
        "RunManifest",
    )?;
    let equity_exists = super::super::run_multi_market_manifest::existing_identical(
        &equity_path,
        &equity_json,
        "权益曲线",
    )?;
    let equity_exact_exists = super::super::run_multi_market_manifest::existing_identical(
        &equity_exact_path,
        &equity_exact_json,
        "精确权益曲线",
    )?;
    let fills_exists = super::super::run_multi_market_manifest::existing_identical(
        &fills_path,
        &fills_json,
        "成交明细",
    )?;
    match std::fs::read_to_string(&outcome_path) {
        Ok(existing) if existing == artifact_json => {
            super::super::run_multi_market_manifest::ensure_event_trace_jsonl(
                &event_trace_path,
                report.event_log.events(),
                "盘口事件轨迹",
            )?;
            if !manifest_exists {
                write_text(&manifest_artifact_path, &manifest_json)?;
            }
            if !equity_exists {
                write_text(&equity_artifact_path, &equity_json)?;
            }
            if !equity_exact_exists {
                write_text(&equity_exact_artifact_path, &equity_exact_json)?;
            }
            if !fills_exists {
                write_text(&fills_artifact_path, &fills_json)?;
            }
            return Ok(Some(outcome));
        }
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场盘口运行产物已存在且内容不同: {artifact_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取多市场盘口回测产物", &error)),
    }
    if !manifest_exists {
        write_text(&manifest_artifact_path, &manifest_json)?;
    }
    if !equity_exists {
        write_text(&equity_artifact_path, &equity_json)?;
    }
    if !equity_exact_exists {
        write_text(&equity_exact_artifact_path, &equity_exact_json)?;
    }
    if !fills_exists {
        write_text(&fills_artifact_path, &fills_json)?;
    }
    super::super::run_multi_market_manifest::ensure_event_trace_jsonl(
        &event_trace_path,
        report.event_log.events(),
        "盘口事件轨迹",
    )?;
    write_text(&artifact_path, &artifact_json)?;
    Ok(Some(outcome))
}

fn verify_depth_dataset_quality(
    manifest_path: &str,
    frame_path: &str,
    frame: &DepthFrame,
    data_tier: &str,
) -> Result<serde_json::Value, AppError> {
    let path = std::path::Path::new(manifest_path);
    let manifest_bytes = std::fs::read(path)
        .map_err(|error| AppError::from_io("读取多市场深度数据质量清单", &error))?;
    let payload = std::str::from_utf8(&manifest_bytes).map_err(|error| {
        invalid(format!(
            "多市场深度数据质量清单不是 UTF-8 {}: {error}",
            path.display()
        ))
    })?;
    let manifest = qx_data::DatasetManifestV2::from_json(payload).map_err(invalid)?;
    let frame_bytes = std::fs::read(frame_path)
        .map_err(|error| AppError::from_io("读取多市场 DepthFrame 内容摘要", &error))?;
    let frame_sha256 = qx_data::sha256_hex_bytes(&frame_bytes);
    let rows =
        u64::try_from(frame.snapshots.len()).map_err(|_| invalid("DepthFrame 快照数超出 u64"))?;
    let first_ts = frame
        .snapshots
        .first()
        .ok_or_else(|| invalid("DepthFrame 不能为空"))?
        .ts;
    let last_ts = frame
        .snapshots
        .last()
        .ok_or_else(|| invalid("DepthFrame 不能为空"))?
        .ts;
    let expected_tier = if data_tier == "l1" {
        qx_data::DatasetTier::L1
    } else {
        qx_data::DatasetTier::L2
    };
    let fingerprint = format!("{:016x}", frame.input_hash());
    if manifest.instrument != frame.instrument.to_string()
        || manifest.tier != expected_tier
        || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
        || manifest.dataset.dataset_id != format!("depth-bars:{}", frame.instrument)
        || manifest.dataset.version != qx_data::DEPTHFRAME_DATASET_VERSION
        || manifest.dataset.source != frame.source
        || manifest.dataset.schema_version != frame.schema_version
        || manifest.source_lineage.primary_source != frame.source
        || manifest.content_hash != frame_sha256
        || manifest.dataset.fingerprint != fingerprint
        || manifest.dataset.start_timestamp != first_ts
        || manifest.dataset.end_timestamp != last_ts
        || manifest.quality_report.row_count != rows
        || manifest.quality_report.usable_tiers != [data_tier]
    {
        return Err(invalid(format!(
            "多市场深度数据质量清单与输入不一致: {}",
            path.display()
        )));
    }
    let interval = manifest
        .quality_report
        .interval_ms
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            invalid(format!(
                "多市场深度数据质量清单缺少正 interval_ms: {}",
                path.display()
            ))
        })?;
    let (duplicates, out_of_order, missing) = frame
        .snapshots
        .windows(2)
        .try_fold(
            (0_u64, 0_u64, 0_u64),
            |(duplicates, out_of_order, missing), pair| {
                if pair[1].ts < pair[0].ts {
                    Some((duplicates, out_of_order.checked_add(1)?, missing))
                } else if pair[1].ts == pair[0].ts {
                    Some((duplicates.checked_add(1)?, out_of_order, missing))
                } else {
                    Some((
                        duplicates,
                        out_of_order,
                        missing.checked_add((pair[1].ts - pair[0].ts).div_ceil(interval) - 1)?,
                    ))
                }
            },
        )
        .ok_or_else(|| invalid("多市场深度质量报告复算溢出"))?;
    if manifest.quality_report.duplicate_rows != duplicates
        || manifest.quality_report.out_of_order_rows != out_of_order
        || manifest.quality_report.missing_intervals != missing
    {
        return Err(invalid(format!(
            "多市场深度质量报告与 DepthFrame 实况不一致: {}",
            path.display()
        )));
    }
    let manifest_fingerprint = manifest.fingerprint().map_err(invalid)?;
    Ok(serde_json::json!({
        "manifest_path": path.to_string_lossy(),
        "manifest_sha256": qx_data::sha256_hex_bytes(&manifest_bytes),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": manifest.content_hash,
        "quality_report": manifest.quality_report,
        "manifest": manifest,
    }))
}

struct CallbackStrategy(Box<dyn Strategy>);

impl Strategy for CallbackStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_init(context)
    }

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.0.on_start(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        self.0.on_event(context, event)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        self.0.on_order_update(context, update)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.0.on_stop()
    }

    fn on_stop_with_context(
        &mut self,
        stop: &qx_strategy::StrategyStopContextV1,
    ) -> Result<(), String> {
        self.0.on_stop_with_context(stop)
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
