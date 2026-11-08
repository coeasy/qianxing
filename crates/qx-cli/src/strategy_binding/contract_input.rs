use crate::{
    account_event_log_name, event_log_exists, execution_worker_supports_instrument,
    load_strategy_contract_bars, open_account_pipeline, runtime_path, strategy_account_context,
    strategy_execution_account_for_instrument, validate_research_snapshot_binding,
};
use qx_core::{InstrumentId, PositionSide};
use qx_factor::StrategyResearchSnapshot;
use qx_runtime::{
    OutboxRecovery, RuntimeConfig, StrategyContext, StrategyContractBars, StrategyContractInput,
};
use qx_strategy::contract::{StrategyContractMarketOrderBookV1, StrategyContractMarketTickV1};
use qx_strategy::{StrategyFillUpdateV2, StrategyOrderStatusUpdate};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
pub(crate) fn build_strategy_contract_input(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractInput, String> {
    build_strategy_contract_input_with_updates(root, config, instrument, request_id, now, &[])
}

pub(crate) fn build_strategy_contract_input_with_updates(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
    execution_updates: &[StrategyOrderStatusUpdate],
) -> Result<StrategyContractInput, String> {
    build_strategy_contract_input_with_execution_updates(
        root,
        config,
        instrument,
        request_id,
        now,
        execution_updates,
        &[],
    )
}

pub(crate) fn build_strategy_contract_input_with_execution_updates(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
    execution_updates: &[StrategyOrderStatusUpdate],
    fill_updates: &[StrategyFillUpdateV2],
) -> Result<StrategyContractInput, String> {
    build_strategy_contract_input_with_market_ticks(
        root,
        config,
        instrument,
        request_id,
        now,
        execution_updates,
        fill_updates,
        &[],
    )
}

pub(crate) fn build_strategy_contract_input_with_market_ticks(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
    execution_updates: &[StrategyOrderStatusUpdate],
    fill_updates: &[StrategyFillUpdateV2],
    market_ticks: &[StrategyContractMarketTickV1],
) -> Result<StrategyContractInput, String> {
    build_strategy_contract_input_with_market_events(
        root,
        config,
        instrument,
        request_id,
        now,
        execution_updates,
        fill_updates,
        market_ticks,
        &[],
    )
}

pub(crate) fn build_strategy_contract_input_with_market_events(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
    execution_updates: &[StrategyOrderStatusUpdate],
    fill_updates: &[StrategyFillUpdateV2],
    market_ticks: &[StrategyContractMarketTickV1],
    market_order_books: &[StrategyContractMarketOrderBookV1],
) -> Result<StrategyContractInput, String> {
    let account_id = config
        .strategy
        .account_id
        .clone()
        .ok_or_else(|| "Python Strategy 必须配置 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .clone()
        .ok_or_else(|| "Python Strategy 必须配置 venue_id".to_string())?;
    if let Some(configured) = config.strategy.research_snapshot_path.as_deref() {
        let candidate = runtime_path(root, configured);
        let path = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(configured)
        };
        let research = StrategyResearchSnapshot::from_json(
            &std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读取 Python Strategy research snapshot 失败 {}: {error}",
                    path.display()
                )
            })?,
        )
        .map_err(|error| format!("Python Strategy research snapshot JSON 无效: {error:?}"))?;
        validate_research_snapshot_binding(root, &config.strategy, &research)?;
        let research_as_of = research.as_of;
        let data_fingerprint = research.candidate.config.data_fingerprint.clone();
        let (
            mut positions,
            mut positions_exact,
            cash,
            cash_exact,
            available_margin_raw,
            available_margin_exact,
            risk_state,
        ) = strategy_account_context(root, config, instrument)?;
        populate_strategy_execution_positions(
            root,
            config,
            instrument,
            &mut positions,
            &mut positions_exact,
        )?;
        let context = StrategyContext {
            strategy_id: config
                .strategy
                .id
                .clone()
                .unwrap_or_else(|| config.strategy.version.clone()),
            strategy_version: config.strategy.version.clone(),
            data_fingerprint,
            as_of: research.as_of,
            research,
            account_id,
            venue_id,
            positions,
            cash,
            available_margin_raw,
            risk_state,
        };
        context
            .validate(now, config.is_production())
            .map_err(|error| format!("Python StrategyContext 校验失败: {error}"))?;
        let bars = load_strategy_contract_bars(root, config, instrument, research_as_of)?
            .map(|(bars, _, _)| bars);
        let mut input = context.to_contract_input(
            request_id,
            instrument,
            bars,
            positions_exact,
            cash_exact,
            available_margin_exact,
        )?;
        input.execution_updates = execution_updates.to_vec();
        input.fill_updates = fill_updates.to_vec();
        input.market_ticks = market_ticks.to_vec();
        input.market_order_books = market_order_books.to_vec();
        if let Some(update_as_of) = input
            .execution_updates
            .iter()
            .map(|update| update.ts)
            .chain(input.fill_updates.iter().map(|update| update.ts))
            .chain(input.market_ticks.iter().map(|tick| tick.ts))
            .chain(input.market_order_books.iter().map(|book| book.ts))
            .max()
        {
            input.as_of = input.as_of.max(update_as_of);
        }
        let (market_bars, market_fingerprints) =
            load_strategy_market_bars(root, config, instrument, input.as_of)?;
        input.market_bars = market_bars;
        input.data_fingerprint = combine_strategy_market_fingerprint(
            &input.instrument,
            &input.data_fingerprint,
            &market_fingerprints,
        )?;
        combine_market_data_fingerprint(&mut input)?;
        input.schema_version = input.schema_version_for_features();
        input.validate()?;
        return Ok(input);
    }

    let (
        mut positions,
        mut positions_exact,
        cash,
        cash_exact,
        available_margin_raw,
        available_margin_exact,
        risk_state,
    ) = strategy_account_context(root, config, instrument)?;
    populate_strategy_execution_positions(
        root,
        config,
        instrument,
        &mut positions,
        &mut positions_exact,
    )?;
    let bars = load_strategy_contract_bars(root, config, instrument, now)?;
    let (bars, data_fingerprint, as_of) = if let Some((bars, data_fingerprint, as_of)) = bars {
        (Some(bars), data_fingerprint, as_of)
    } else {
        (None, "runtime-config-v1".into(), now.max(1))
    };
    let (market_bars, market_fingerprints) =
        load_strategy_market_bars(root, config, instrument, as_of)?;
    let mut input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: request_id.to_string(),
        strategy_id: config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| config.strategy.version.clone()),
        strategy_version: config.strategy.version.clone(),
        data_fingerprint: combine_strategy_market_fingerprint(
            &instrument.to_string(),
            &data_fingerprint,
            &market_fingerprints,
        )?,
        as_of,
        instrument: instrument.to_string(),
        positions,
        positions_exact,
        cash,
        cash_exact,
        available_margin_raw,
        available_margin_exact,
        risk_state,
        research_targets: BTreeMap::from([(instrument.to_string(), config.strategy.target_qty)]),
        market_bars,
        bars,
        execution_updates: execution_updates.to_vec(),
        fill_updates: fill_updates.to_vec(),
        strategy_checkpoint: None,
        market_ticks: market_ticks.to_vec(),
        account_updates: Vec::new(),
        market_order_books: market_order_books.to_vec(),
    };
    if let Some(update_as_of) = input
        .execution_updates
        .iter()
        .map(|update| update.ts)
        .chain(input.fill_updates.iter().map(|update| update.ts))
        .chain(input.market_ticks.iter().map(|tick| tick.ts))
        .chain(input.market_order_books.iter().map(|book| book.ts))
        .max()
    {
        input.as_of = input.as_of.max(update_as_of);
    }
    combine_market_data_fingerprint(&mut input)?;
    input.schema_version = input.schema_version_for_features();
    input.validate()?;
    Ok(input)
}

fn combine_market_data_fingerprint(input: &mut StrategyContractInput) -> Result<(), String> {
    if input.market_ticks.is_empty() && input.market_order_books.is_empty() {
        return Ok(());
    }
    let payload = if input.market_order_books.is_empty() {
        serde_json::to_vec(&(&input.data_fingerprint, &input.market_ticks))
    } else {
        serde_json::to_vec(&(
            &input.data_fingerprint,
            &input.market_ticks,
            &input.market_order_books,
        ))
    }
    .map_err(|error| format!("编码 Strategy 行情事件数据指纹失败: {error}"))?;
    input.data_fingerprint = qx_strategy::sha256_hex(&payload);
    Ok(())
}

fn load_strategy_market_bars(
    root: &Path,
    config: &RuntimeConfig,
    primary_instrument: &InstrumentId,
    as_of: u64,
) -> Result<
    (
        BTreeMap<String, StrategyContractBars>,
        BTreeMap<String, String>,
    ),
    String,
> {
    let mut market_bars = BTreeMap::new();
    let mut fingerprints = BTreeMap::new();
    let Some(reference_text) = config.strategy.builtin_reference_instrument.as_deref() else {
        return Ok((market_bars, fingerprints));
    };
    let reference = InstrumentId::parse(reference_text)
        .ok_or_else(|| format!("Strategy reference market instrument 非法: {reference_text}"))?;
    if &reference == primary_instrument {
        return Err("Strategy reference market 不能重复主 instrument".into());
    }
    let reference_path = config
        .strategy
        .builtin_reference_bars_snapshot_path
        .as_deref()
        .ok_or_else(|| {
            "Strategy 配置了 builtin_reference_instrument 但缺少 builtin_reference_bars_snapshot_path".to_string()
        })?;
    let mut reference_config = config.clone();
    reference_config.strategy.instrument = Some(reference.to_string());
    reference_config.strategy.bars_snapshot_path = Some(reference_path.to_string());
    let Some((bars, fingerprint, market_as_of)) =
        load_strategy_contract_bars(root, &reference_config, &reference, as_of)?
    else {
        return Err(format!("Strategy reference market 缺少 Bar: {reference}"));
    };
    if market_as_of != as_of {
        return Err(format!(
            "Strategy 双市场 Bar 时间未对齐: primary_as_of={as_of} reference={} reference_as_of={market_as_of}",
            reference
        ));
    }
    market_bars.insert(reference.to_string(), bars);
    fingerprints.insert(reference.to_string(), fingerprint);
    Ok((market_bars, fingerprints))
}

fn combine_strategy_market_fingerprint(
    primary_instrument: &str,
    primary_fingerprint: &str,
    market_fingerprints: &BTreeMap<String, String>,
) -> Result<String, String> {
    if market_fingerprints.is_empty() {
        return Ok(primary_fingerprint.to_string());
    }
    let mut fingerprints = market_fingerprints.clone();
    fingerprints.insert(
        primary_instrument.to_string(),
        primary_fingerprint.to_string(),
    );
    let payload = serde_json::to_vec(&fingerprints)
        .map_err(|error| format!("Strategy multi-market fingerprint 编码失败: {error}"))?;
    Ok(format!(
        "multi-market:{}",
        qx_strategy::sha256_hex(&payload)
    ))
}

fn populate_strategy_execution_positions(
    root: &Path,
    config: &RuntimeConfig,
    primary_instrument: &InstrumentId,
    positions: &mut BTreeMap<String, i128>,
    positions_exact: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    // Cross-venue Strategy API intents need account-scoped position facts. Keep
    // the legacy bare InstrumentId keys for the strategy's home account, and add
    // `account_id|InstrumentId` aliases for every configured execution route so
    // Python, Rust and C ABI strategies can make idempotent multi-account choices.
    let mut routes: BTreeMap<(String, String), BTreeSet<InstrumentId>> = BTreeMap::new();
    let reference_route = if config.strategy.builtin_strategy.is_some() {
        config
            .strategy
            .builtin_reference_instrument
            .as_deref()
            .and_then(InstrumentId::parse)
            .map(|reference| {
                strategy_execution_account_for_instrument(
                    config,
                    config.strategy.account_id.as_deref(),
                    &reference,
                )
                .map(|account_id| (reference, account_id))
            })
            .transpose()?
    } else {
        None
    };
    for worker in config
        .workers
        .iter()
        .filter(|worker| worker.enabled && worker.role == qx_runtime::WorkerRole::Execution)
    {
        let (Some(account_id), Some(venue_id)) =
            (worker.account_id.as_deref(), worker.venue_id.as_deref())
        else {
            continue;
        };
        let mut instruments = worker
            .symbols
            .iter()
            .filter_map(|symbol| InstrumentId::parse(symbol))
            .filter(|instrument| execution_worker_supports_instrument(worker, instrument))
            .collect::<BTreeSet<_>>();
        if worker.symbols.is_empty() {
            let mut candidates = vec![primary_instrument.clone()];
            if let Some(reference) = config
                .strategy
                .builtin_reference_instrument
                .as_deref()
                .and_then(InstrumentId::parse)
            {
                candidates.push(reference);
            }
            for candidate in candidates {
                if execution_worker_supports_instrument(worker, &candidate) {
                    instruments.insert(candidate);
                }
            }
        }
        if !instruments.is_empty() {
            routes
                .entry((account_id.to_string(), venue_id.to_string()))
                .or_default()
                .extend(instruments);
        }
    }
    for ((account_id, venue_id), instruments) in routes {
        let Some(log_name) = account_event_log_name(&account_id, &venue_id) else {
            continue;
        };
        if !event_log_exists(config, root, &log_name)? {
            continue;
        }
        let pipeline = open_account_pipeline(config, root, &log_name, OutboxRecovery::ReadOnly)
            .map_err(|error| format!("恢复 Strategy 多账户 EventLog 失败: {error}"))?;
        for instrument in instruments {
            let qualified = format!("{account_id}|{instrument}");
            let state = pipeline.ledger().position_for(&account_id, &instrument);
            positions.insert(qualified.clone(), state.quantity.raw());
            positions_exact.insert(
                qualified,
                pipeline
                    .ledger()
                    .position_for_side_exact(&account_id, &instrument, PositionSide::Net)
                    .quantity
                    .to_string(),
            );
            if config.strategy.account_id.as_deref() == Some(account_id.as_str())
                && instrument == *primary_instrument
            {
                positions.insert(instrument.to_string(), state.quantity.raw());
                positions_exact.insert(
                    instrument.to_string(),
                    pipeline
                        .ledger()
                        .position_for_side_exact(&account_id, &instrument, PositionSide::Net)
                        .quantity
                        .to_string(),
                );
            }
            if reference_route
                .as_ref()
                .is_some_and(|(reference, reference_account)| {
                    reference == &instrument && reference_account == &account_id
                })
            {
                // Built-in spread strategies use bare instrument keys internally.
                positions.insert(instrument.to_string(), state.quantity.raw());
                positions_exact.insert(
                    instrument.to_string(),
                    pipeline
                        .ledger()
                        .position_for_side_exact(&account_id, &instrument, PositionSide::Net)
                        .quantity
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}
