//! N-market shared-account Bar backtest through the single PortfolioBarEngine.

use super::artifacts::{read_text, write_text};
use super::attach;
use super::guard::guard_panics;
use super::run_backtest::strategy_slot::{
    build_strategy_slot, strategy_version, StrategySlotContext,
};
use super::run_multi_market_manifest::{
    build_run_manifest, dataset_manifest_fingerprint, existing_identical, invalid,
    max_drawdown_bps, take_runtime_market, verify_dataset_quality, DataIdentity,
    MultiMarketManifestInput, PreparedMarket,
};
use crate::{
    AppError, AppErrorCategory, BacktestMarginConfig, CallerCapability, MultiMarketBacktestOutcome,
    MultiMarketBacktestSpec, RunContext, StrategyArtifactIdentity,
};
use qx_core::{Fnv1a, MakerTakerFeeModel, Money, OrderPolicy};
use qx_datastruct::BarFrame;
use qx_strategy::Strategy;
use qx_xingban::{
    LeverageMargin, MarginTier, MarketRoutedBarStrategy, NoMargin, PortfolioBarEngine,
    PortfolioBarSessionConfig, PortfolioBarStrategy, TieredMargin,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn run_multi_market_backtest(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
) -> Result<MultiMarketBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多市场共享账户 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner(spec, context, None, BTreeMap::new())
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            AppError::new(
                AppErrorCategory::InternalInvariant,
                "同步多市场回测意外返回取消状态",
            ),
            &correlation_id,
        )
    })
}

impl MultiMarketBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiMarketBacktestOutcome> {
        let run_id = self.run_id.clone();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场共享账户 Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), BTreeMap::new())
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Run selected market slots with caller-authored strategies on the same Rust engine.
    pub fn run_with_strategy_slots(
        &self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: &RunContext,
    ) -> Result<MultiMarketBacktestOutcome, AppError> {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多市场多语言策略 Bar 回测")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        let injected =
            validate_injected(self, strategies).map_err(|error| attach(error, &correlation_id))?;
        guard_panics(&correlation_id, || run_inner(self, context, None, injected))
            .map_err(|error| attach(error, &correlation_id))?
            .ok_or_else(|| {
                attach(
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "同步多市场自定义策略回测未产生报告",
                    ),
                    &correlation_id,
                )
            })
    }

    /// Start a cancellable multi-market run with caller-authored strategy slots.
    pub fn start_with_strategy_slots(
        self,
        strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
        context: RunContext,
    ) -> Result<crate::RunHandle<MultiMarketBacktestOutcome>, AppError> {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多市场多语言策略 Bar 回测")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        let injected =
            validate_injected(&self, strategies).map_err(|error| attach(error, &correlation_id))?;
        let run_id = self.run_id.clone();
        Ok(crate::RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), injected)
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

pub(super) struct InjectedMarketStrategy {
    pub(super) identity: StrategyArtifactIdentity,
    pub(super) strategy: Box<dyn Strategy>,
}

pub(super) fn validate_injected(
    spec: &MultiMarketBacktestSpec,
    strategies: Vec<(StrategyArtifactIdentity, Box<dyn Strategy>)>,
) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError> {
    if strategies.is_empty() {
        return Err(invalid("多市场自定义策略组合至少需要一个回调槽"));
    }
    let mut injected = BTreeMap::new();
    for (identity, strategy) in strategies {
        let slot = spec
            .markets
            .iter()
            .flat_map(|market| &market.strategies)
            .find(|member| member.strategy.strategy_id == identity.strategy_id)
            .ok_or_else(|| {
                invalid(format!(
                    "自定义策略身份不属于多市场组合: {}",
                    identity.strategy_id
                ))
            })?;
        identity.validate_for_slot(&slot.strategy.strategy_id, &slot.strategy)?;
        let id = identity.strategy_id.clone();
        if injected
            .insert(id.clone(), InjectedMarketStrategy { identity, strategy })
            .is_some()
        {
            return Err(invalid(format!("多市场自定义策略身份重复: {id}")));
        }
    }
    Ok(injected)
}

pub(super) fn run_inner(
    spec: &MultiMarketBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    mut injected: BTreeMap<String, InjectedMarketStrategy>,
) -> Result<Option<MultiMarketBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.bars_path.clear();
        market.dataset_manifest_path = None;
    }
    let spec_json = identity_spec.to_json()?;
    let mut spec_hasher = Fnv1a::new();
    spec_hasher.write_text(&spec_json);
    let spec_fingerprint_base = spec_hasher.finish();

    let mut prepared = Vec::with_capacity(spec.markets.len());
    let mut data_hasher = Fnv1a::new();
    let mut routed_strategies = Vec::with_capacity(spec.markets.len());
    let mut strategy_count = 0_usize;
    let mut strategy_identities = Vec::new();
    for market in &spec.markets {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let payload = read_text(&market.bars_path)?;
        let frame = BarFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} BarFrame 无效: {error:?}",
                market.instrument
            ))
        })?;
        if frame.instrument.to_string() != market.instrument {
            return Err(invalid(format!(
                "市场 {} 的 BarFrame 实际标的是 {}",
                market.instrument, frame.instrument
            )));
        }
        let bars: Vec<_> = (&frame).into();
        if bars.len() < crate::MIN_BACKTEST_BARS {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要两根 Bar", market.instrument),
            ));
        }
        let instrument = frame.instrument.clone();
        let input_path = PathBuf::from(&market.bars_path)
            .canonicalize()
            .map_err(|error| AppError::from_io("解析多市场 BarFrame 输入路径", &error))?;
        let data_hash = qx_data::fingerprint_bar_frame(&frame).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "市场 {} 的 BarFrame 内容指纹计算失败：{error}",
                    market.instrument
                ),
            )
        })?;
        let dataset_quality = market
            .dataset_manifest_path
            .as_deref()
            .map(|path| verify_dataset_quality(path, &market.bars_path, &frame, &data_hash))
            .transpose()?;
        data_hasher.write_text(&instrument.to_string());
        data_hasher.write_text(&data_hash);
        if let Some(quality) = &dataset_quality {
            data_hasher.write_text(dataset_manifest_fingerprint(quality)?);
        }
        let market_data_fingerprint = format!("barframe:{data_hash}");
        let contract = market
            .market_spec
            .as_ref()
            .map(|snapshot| crate::market_spec_from_value(&instrument, snapshot))
            .transpose()?
            .unwrap_or_else(|| {
                qx_core::TradingInstrumentSpec::spot_default(&instrument, &spec.settlement_currency)
            });
        let default_policy = OrderPolicy {
            margin_mode: contract.margin_mode_default(),
            ..OrderPolicy::default()
        };
        let order_policy = market.order_policy.unwrap_or(default_policy);
        let derived_margin_model = market
            .market_spec
            .as_ref()
            .filter(|snapshot| {
                snapshot
                    .get("leverage_tiers")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|tiers| !tiers.is_empty())
            })
            .map(crate::backtest_margin_model_from_market);
        let margin_model = market
            .margin_model
            .as_ref()
            .or(derived_margin_model.as_ref());
        let margin: Box<dyn qx_xingban::MarginRule> = if contract.product.supports_leverage() {
            match margin_model {
                Some(BacktestMarginConfig::Tiers { tiers }) => Box::new(TieredMargin {
                    tiers: tiers
                        .iter()
                        .map(|tier| MarginTier {
                            max_notional: tier.max_notional,
                            initial_bp: tier.initial_bp,
                            maintenance_bp: tier.maintenance_bp,
                            max_leverage: tier.max_leverage,
                        })
                        .collect(),
                }),
                Some(BacktestMarginConfig::Leverage {
                    leverage,
                    maintenance_bp,
                }) => Box::new(LeverageMargin {
                    leverage: *leverage,
                    maintenance_bp: *maintenance_bp,
                }),
                Some(BacktestMarginConfig::NoMargin) => Box::new(NoMargin),
                None => Box::new(LeverageMargin {
                    leverage: order_policy.leverage,
                    maintenance_bp: i64::from(contract.maintenance_margin_bps),
                }),
            }
        } else {
            Box::new(NoMargin)
        };
        let fee = market
            .fee_schedule
            .map(|schedule| schedule.model())
            .unwrap_or_else(MakerTakerFeeModel::default_maker_taker);
        let mut strategy_slots = Vec::with_capacity(market.strategies.len());
        for member in &market.strategies {
            let external = injected.remove(&member.strategy.strategy_id);
            let version = external
                .as_ref()
                .map(|item| item.identity.strategy_version.clone())
                .unwrap_or_else(|| strategy_version(&member.strategy));
            if let Some(item) = &external {
                strategy_identities.push(item.identity.clone());
            }
            let slot = build_strategy_slot(
                &member.strategy,
                StrategySlotContext {
                    priority: member.priority,
                    reserved_capital_raw: member.reserved_capital_raw,
                    instrument: &instrument,
                    account_id: &spec.account_id,
                    currency: &spec.settlement_currency,
                    data_fingerprint: &market_data_fingerprint,
                    as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
                    strategy_version: Some(&version),
                },
                external.map(|item| item.strategy),
            )?;
            strategy_slots.push(slot);
            strategy_count += 1;
        }
        let strategy_basket = PortfolioBarStrategy::new(strategy_slots)
            .map_err(|error| AppError::from_qx_error(&error))?;
        routed_strategies.push((
            instrument.clone(),
            Box::new(strategy_basket) as Box<dyn qx_xingban::BarStrategy>,
            Some(order_policy),
        ));
        let row_count = bars.len();
        prepared.push(PreparedMarket {
            instrument,
            bars,
            priority: market.priority,
            contract,
            fee: Some(fee),
            margin: Some(margin),
            seed: spec.seed,
            identity: DataIdentity {
                instrument: market.instrument.clone(),
                path: input_path.to_string_lossy().to_string(),
                content_hash: data_hash,
                row_count,
            },
            dataset_quality,
        });
    }
    if !injected.is_empty() {
        return Err(invalid("多市场自定义策略存在未消费的孤儿策略槽"));
    }
    strategy_identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let spec_fingerprint = if strategy_identities.is_empty() {
        format!("{spec_fingerprint_base:016x}")
    } else {
        let mut fingerprint = Fnv1a::new();
        fingerprint.write_u64(spec_fingerprint_base);
        for identity in &strategy_identities {
            fingerprint.write_text(&identity.strategy_id);
            fingerprint.write_text(&identity.strategy_version);
            fingerprint.write_text(&identity.source_sha256);
        }
        format!("{:016x}", fingerprint.finish())
    };
    let data_identities = prepared
        .iter()
        .map(|market| DataIdentity {
            instrument: market.identity.instrument.clone(),
            path: market.identity.path.clone(),
            content_hash: market.identity.content_hash.clone(),
            row_count: market.identity.row_count,
        })
        .collect::<Vec<_>>();
    let runtime_markets = prepared
        .iter_mut()
        .map(take_runtime_market)
        .collect::<Result<Vec<_>, AppError>>()?;
    let data_fingerprint = format!("{:016x}", data_hasher.finish());
    let mut strategy = MarketRoutedBarStrategy::new_with_order_policies(routed_strategies)
        .map_err(|error| AppError::from_qx_error(&error))?;
    let funding_bps = spec
        .markets
        .iter()
        .filter(|market| market.funding_bps != 0)
        .map(|market| {
            let instrument = qx_core::InstrumentId::parse(&market.instrument)
                .ok_or_else(|| invalid("通过校验的资金费标的无法再次解析"))?;
            Ok((instrument, market.funding_bps))
        })
        .collect::<Result<BTreeMap<_, _>, AppError>>()?;
    let funding_events = spec
        .markets
        .iter()
        .flat_map(|market| market.funding_events.iter().cloned())
        .collect::<Vec<_>>();
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: spec.account_id.clone(),
            currency: spec.settlement_currency.clone(),
            initial_cash: Money::from_raw(spec.initial_cash_raw),
            risk: RiskGate::conservative_default(),
            funding_bps,
            funding_events,
        },
        runtime_markets,
    )
    .map_err(|error| AppError::from_qx_error(&error))?;
    let report = match cancellation {
        Some(token) => engine.run_with_cancel(&mut strategy, || token.is_cancelled()),
        None => engine.run(&mut strategy).map(Some),
    }
    .map_err(|error| AppError::from_qx_error(&error))?;
    let Some(report) = report else {
        return Ok(None);
    };
    report
        .verify()
        .map_err(|error| AppError::from_qx_error(&error))?;
    let final_equity_raw = report
        .equity
        .last()
        .map(|(_, value)| *value)
        .unwrap_or(spec.initial_cash_raw);
    let return_bps = final_equity_raw
        .checked_sub(spec.initial_cash_raw)
        .and_then(|pnl| pnl.checked_mul(10_000))
        .and_then(|value| value.checked_div(spec.initial_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid("多市场组合收益无法表示"))?;
    let max_drawdown_bps = max_drawdown_bps(&report.equity)?;
    let successful_fills =
        u64::try_from(report.fills.len()).map_err(|_| invalid("多市场成交数超出 u64 范围"))?;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多市场回测产物目录", &error))?;
    let output_root = PathBuf::from(&spec.output_dir)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多市场回测产物目录", &error))?;
    let output_path = output_root.join(format!("{}.multi-market.json", spec.run_id));
    let artifact_path = output_path.to_string_lossy().into_owned();
    let manifest_path = output_root.join(format!("{}.multi-market.run.json", spec.run_id));
    let run_manifest_path = manifest_path.to_string_lossy().into_owned();
    let equity_path = output_root.join(format!("{}.multi-market.equity.json", spec.run_id));
    let equity_artifact_path = equity_path.to_string_lossy().into_owned();
    let fills_path = output_root.join(format!("{}.multi-market.fills.json", spec.run_id));
    let fills_artifact_path = fills_path.to_string_lossy().into_owned();
    let instrument_equity_path = output_root.join(format!(
        "{}.multi-market.instrument-equity.json",
        spec.run_id
    ));
    let instrument_equity_artifact_path = instrument_equity_path.to_string_lossy().into_owned();
    let mut input_components = prepared
        .iter()
        .map(|market| {
            (
                format!("barframe:{}", market.identity.instrument),
                market.identity.content_hash.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for market in &prepared {
        if let Some(quality) = &market.dataset_quality {
            input_components.insert(
                format!("dataset-quality:{}", market.identity.instrument),
                dataset_manifest_fingerprint(quality)?.to_string(),
            );
        }
    }
    let instrument_equity_json = serde_json::to_string(
        &report
            .instrument_equity_contributions
            .iter()
            .map(|(instrument, rows)| {
                (
                    instrument.to_string(),
                    rows.iter()
                        .map(|(equity_index, ts, value)| {
                            serde_json::json!({
                                "equity_index": equity_index,
                                "ts": ts,
                                "pnl_contribution_raw": value.to_string()
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    )
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    input_components.insert(
        "derived:instrument-equity-contributions".into(),
        qx_data::sha256_hex_bytes(instrument_equity_json.as_bytes()),
    );
    let result_hash = format!("{:016x}", report.result_hash());
    let event_hash = format!("{:016x}", report.event_log.digest());
    let run_manifest_json = build_run_manifest(MultiMarketManifestInput {
        spec,
        context,
        data_fingerprint: &data_fingerprint,
        input_components,
        clock_start: report.clock_start,
        clock_end: report.clock_end,
        spec_fingerprint: &spec_fingerprint,
        result_hash: &result_hash,
        event_hash: &event_hash,
    })?;
    let equity_json = serde_json::to_string(&report.equity)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let fills_json = serde_json::to_string(&report.fills)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let outcome = MultiMarketBacktestOutcome {
        run_id: spec.run_id.clone(),
        account_id: spec.account_id.clone(),
        spec_fingerprint,
        data_fingerprint,
        shared_result_hash: format!("{:016x}", report.result_hash()),
        markets_total: spec.markets.len(),
        strategies_total: strategy_count,
        successful_fills,
        equity_points: u64::try_from(report.equity.len())
            .map_err(|_| invalid("多市场权益点数超出 u64 范围"))?,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
        artifact_path: artifact_path.clone(),
    };
    let per_market_fills = report
        .fills_by_instrument
        .iter()
        .map(|(instrument, fills)| (instrument.to_string(), fills.len()))
        .collect::<BTreeMap<_, _>>();
    let artifact = serde_json::json!({
        "schema_version": 1,
        "spec": spec,
        "outcome": outcome,
        "market_data": data_identities,
        "dataset_quality": prepared.iter().filter_map(|market| market.dataset_quality.as_ref().map(|quality| (market.identity.instrument.clone(), quality.clone()))).collect::<BTreeMap<_, _>>(),
        "strategy_identities": strategy_identities,
        "per_market_fills": per_market_fills,
        "funding_by_instrument_raw": report.funding_by_instrument_raw,
        "funding_events_by_instrument": report.funding_events_by_instrument,
        "funding_rate_input": spec.markets.iter().map(|market| serde_json::json!({
            "instrument": market.instrument,
            "funding_bps": market.funding_bps,
            "funding_events": market.funding_events,
        })).collect::<Vec<_>>(),
        "equity": report.equity,
        "event_log": report.event_log.events(),
        "ledger_entries": report.ledger.entries(),
        "replay_verified": true,
        "run_manifest_path": run_manifest_path,
        "equity_path": equity_artifact_path,
        "fills_path": fills_artifact_path,
        "instrument_equity_contributions_path": instrument_equity_artifact_path,
    });
    let artifact_json = serde_json::to_string(&artifact)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let manifest_exists = match std::fs::read_to_string(&manifest_path) {
        Ok(existing) if existing == run_manifest_json => true,
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场运行 RunManifest 已存在且内容不同: {run_manifest_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(AppError::from_io("读取多市场 RunManifest", &error)),
    };
    let equity_exists = existing_identical(&equity_path, &equity_json, "权益曲线")?;
    let fills_exists = existing_identical(&fills_path, &fills_json, "成交明细")?;
    let instrument_equity_exists = existing_identical(
        &instrument_equity_path,
        &instrument_equity_json,
        "逐标的权益贡献曲线",
    )?;
    match std::fs::read_to_string(&output_path) {
        Ok(existing) if existing == artifact_json => {
            if !equity_exists {
                write_text(&equity_artifact_path, &equity_json)?;
            }
            if !fills_exists {
                write_text(&fills_artifact_path, &fills_json)?;
            }
            if !instrument_equity_exists {
                write_text(&instrument_equity_artifact_path, &instrument_equity_json)?;
            }
            if !manifest_exists {
                write_text(&run_manifest_path, &run_manifest_json)?;
            }
            return Ok(Some(outcome));
        }
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场运行产物已存在且内容不同: {artifact_path}"),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取多市场回测产物", &error)),
    }
    if !equity_exists {
        write_text(&equity_artifact_path, &equity_json)?;
    }
    if !fills_exists {
        write_text(&fills_artifact_path, &fills_json)?;
    }
    if !instrument_equity_exists {
        write_text(&instrument_equity_artifact_path, &instrument_equity_json)?;
    }
    if !manifest_exists {
        write_text(&run_manifest_path, &run_manifest_json)?;
    }
    write_text(&outcome.artifact_path, &artifact_json)?;
    Ok(Some(outcome))
}
