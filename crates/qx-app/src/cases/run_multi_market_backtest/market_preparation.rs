//! Multi-market dataset validation, strategy slot construction, and run identity assembly.

use super::super::artifacts::read_text;
use super::super::run_backtest::strategy_slot::{
    build_strategy_slot, strategy_version, StrategySlotContext,
};
use super::super::run_multi_market_manifest::{
    dataset_manifest_fingerprint, invalid, verify_dataset_quality, DataIdentity, PreparedMarket,
};
use super::{InjectedMarketStrategy, MultiMarketExecutionWindow};
use crate::{
    AppError, AppErrorCategory, BacktestMarginConfig, MultiMarketBacktestSpec,
    StrategyArtifactIdentity,
};
use qx_core::{MakerTakerFeeModel, OrderPolicy};
use qx_datastruct::BarFrame;
use qx_xingban::{LeverageMargin, MarginTier, NoMargin, PortfolioBarStrategy, TieredMargin};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) struct PreparedMultiMarketRun {
    pub(super) prepared: Vec<PreparedMarket>,
    pub(super) routed_strategies: Vec<(
        qx_core::InstrumentId,
        Box<dyn qx_xingban::BarStrategy>,
        Option<OrderPolicy>,
    )>,
    pub(super) strategy_count: usize,
    pub(super) strategy_identities: Vec<StrategyArtifactIdentity>,
    pub(super) spec_fingerprint: String,
    pub(super) data_identities: Vec<DataIdentity>,
}

pub(super) fn prepare_multi_market_markets(
    spec: &MultiMarketBacktestSpec,
    cancellation: Option<&crate::run_handle::CancellationToken>,
    execution_window: Option<MultiMarketExecutionWindow>,
    spec_fingerprint_base: u64,
    mut injected: BTreeMap<String, InjectedMarketStrategy>,
) -> Result<Option<PreparedMultiMarketRun>, AppError> {
    let mut prepared = Vec::with_capacity(spec.markets.len());
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
        let source_bars = frame.bars().map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "市场 {} 的 BarFrame exact OHLCV 无效: {error:?}",
                    market.instrument
                ),
            )
        })?;
        if source_bars.len() < crate::MIN_BACKTEST_BARS {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 至少需要两根 Bar", market.instrument),
            ));
        }
        let source_row_count = source_bars.len();
        let bars = source_bars
            .into_iter()
            .take_while(|bar| {
                execution_window
                    .and_then(|window| window.data_end_ts_exclusive)
                    .is_none_or(|end| bar.ts < end)
            })
            .collect::<Vec<_>>();
        if bars.len() < crate::MIN_BACKTEST_BARS {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!("市场 {} 在实验数据截止点前不足两根 Bar", market.instrument),
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
            let reserved_capital_exact = member
                .reserved_capital()
                .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
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
                    reserved_capital_exact,
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
                row_count: source_row_count,
                start_ts_ms: frame.ts.first().copied().unwrap_or(0),
                end_ts_ms: frame.ts.last().copied().unwrap_or(0),
                quality_fingerprint: dataset_quality
                    .as_ref()
                    .map(dataset_manifest_fingerprint)
                    .transpose()?,
            },
            dataset_quality,
        });
    }
    if !injected.is_empty() {
        return Err(invalid("多市场自定义策略存在未消费的孤儿策略槽"));
    }
    strategy_identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    let spec_fingerprint = crate::multi_market_report::multi_market_strategy_set_fingerprint(
        spec_fingerprint_base,
        &strategy_identities,
    );
    let data_identities = prepared
        .iter()
        .map(|market| DataIdentity {
            instrument: market.identity.instrument.clone(),
            path: market.identity.path.clone(),
            content_hash: market.identity.content_hash.clone(),
            row_count: market.identity.row_count,
            start_ts_ms: market.identity.start_ts_ms,
            end_ts_ms: market.identity.end_ts_ms,
            quality_fingerprint: market
                .dataset_quality
                .as_ref()
                .and_then(|quality| quality.get("manifest_fingerprint"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
        .collect::<Vec<_>>();
    Ok(Some(PreparedMultiMarketRun {
        prepared,
        routed_strategies,
        strategy_count,
        strategy_identities,
        spec_fingerprint,
        data_identities,
    }))
}
