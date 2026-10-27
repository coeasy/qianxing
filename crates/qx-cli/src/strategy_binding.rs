//! 策略绑定：策略当前持仓读取与跨语言契约输入装配。

use crate::{
    account_event_log_name, event_log_exists, execution_worker_supports_instrument,
    invoke_builtin_strategy, invoke_python_strategy, invoke_python_strategy_with_client,
    load_strategy_contract_bars, native_strategy_context, open_account_pipeline, runtime_path,
    strategy_account_context, strategy_execution_account_for_instrument, strategy_target_qty,
    validate_research_snapshot_binding, PythonStrategyClient, StrategyProcessClient,
};
use qx_core::{ExactDecimal, InstrumentId, PositionSide};
use qx_factor::StrategyResearchSnapshot;
use qx_runtime::{
    OutboxRecovery, RuntimeConfig, StrategyContext, StrategyContractBars, StrategyContractInput,
    StrategyContractOutput, WorkerConfig,
};
use qx_strategy::contract::{
    StrategyCheckpointV1, StrategyContractMarketOrderBookV1, StrategyContractMarketTickV1,
};
use qx_strategy::{DynamicCAbiStrategy, StrategyFillUpdateV2, StrategyOrderStatusUpdate};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(crate) fn validate_ccxt_worker_binding(
    worker: &WorkerConfig,
    ccxt_config_path: &Path,
) -> Result<(), String> {
    let expected = worker
        .venue_id
        .as_deref()
        .ok_or_else(|| format!("CCXT worker {} 缺少 venue_id", worker.id))?;
    let payload = std::fs::read_to_string(ccxt_config_path).map_err(|error| {
        format!(
            "读取 CCXT 配置失败 path={} error={error}",
            ccxt_config_path.display()
        )
    })?;
    let config: serde_json::Value =
        serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
            format!(
                "解析 CCXT 配置失败 path={} error={error}",
                ccxt_config_path.display()
            )
        })?;
    let configured = config
        .get("exchange_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "CCXT 配置缺少 exchange_id path={}",
                ccxt_config_path.display()
            )
        })?;
    if !configured.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "CCXT worker {} venue_id={} 与配置 exchange_id={} 不一致",
            worker.id, expected, configured
        ));
    }
    if let Some(credentials) = config
        .get("credential_env")
        .filter(|value| !value.is_null())
    {
        let credentials = credentials.as_object().ok_or_else(|| {
            format!(
                "CCXT 配置 credential_env 必须是对象或 null path={}",
                ccxt_config_path.display()
            )
        })?;
        for key in ["api_key", "secret", "password", "uid"] {
            if let Some(value) = credentials.get(key) {
                if !value.is_string() || value.as_str().is_some_and(|value| value.trim().is_empty())
                {
                    return Err(format!(
                        "CCXT 配置 credential_env.{key} 必须是非空环境变量名 path={}",
                        ccxt_config_path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn strategy_current_qty(root: &Path, config: &RuntimeConfig) -> Result<i128, String> {
    let Some(instrument_text) = config.strategy.instrument.as_deref() else {
        return Ok(0);
    };
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    strategy_current_qty_for(root, config, &instrument)
}

pub(crate) fn strategy_current_qty_for(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<i128, String> {
    let Some(account_id) = config.strategy.account_id.as_deref() else {
        return Ok(0);
    };
    strategy_current_qty_for_account(root, config, account_id, instrument)
}

pub(crate) fn strategy_current_qty_for_account(
    root: &Path,
    config: &RuntimeConfig,
    account_id: &str,
    instrument: &InstrumentId,
) -> Result<i128, String> {
    // 多交易所套利的每条 intent 可以属于不同 venue；优先按
    // InstrumentId 的 venue 读取，不能把所有腿都误读成策略主腿。
    // 兼容旧的 paper/测试账户：历史配置可能把事件写入 strategy.venue_id
    // 对应的 EventLog，而 instrument 本身仍使用交易所 venue。
    let venue_id = instrument.venue.to_string();
    let Some(instrument_log_name) = account_event_log_name(account_id, &venue_id) else {
        return Err(format!(
            "Strategy 当前持仓暂不支持 venue_id={}；请先接入该 Venue 的账户事件归约",
            venue_id
        ));
    };
    let log_name = if event_log_exists(config, root, &instrument_log_name)? {
        instrument_log_name
    } else if config.strategy.account_id.as_deref() == Some(account_id) {
        let Some(configured_venue) = config.strategy.venue_id.as_deref() else {
            return Ok(0);
        };
        let Some(configured_log_name) = account_event_log_name(account_id, configured_venue) else {
            return Ok(0);
        };
        if event_log_exists(config, root, &configured_log_name)? {
            configured_log_name
        } else {
            return Ok(0);
        }
    } else {
        return Ok(0);
    };
    let pipeline = open_account_pipeline(config, root, &log_name, OutboxRecovery::ReadOnly)
        .map_err(|error| format!("恢复 Strategy 账户 EventLog 失败: {error}"))?;
    Ok(pipeline
        .ledger()
        .position_for(account_id, instrument)
        .quantity
        .raw())
}

pub(crate) fn strategy_current_qty_exact_for(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<String, String> {
    let Some(account_id) = config.strategy.account_id.as_deref() else {
        return Ok(ExactDecimal::ZERO.to_string());
    };
    strategy_current_qty_exact_for_account(root, config, account_id, instrument)
}

pub(crate) fn strategy_current_qty_exact_for_account(
    root: &Path,
    config: &RuntimeConfig,
    account_id: &str,
    instrument: &InstrumentId,
) -> Result<String, String> {
    let venue_id = instrument.venue.to_string();
    let Some(instrument_log_name) = account_event_log_name(account_id, &venue_id) else {
        return Err(format!(
            "Strategy 当前精确持仓暂不支持 venue_id={}；请先接入该 Venue 的账户事件归约",
            venue_id
        ));
    };
    let log_name = if event_log_exists(config, root, &instrument_log_name)? {
        instrument_log_name
    } else if config.strategy.account_id.as_deref() == Some(account_id) {
        let Some(configured_venue) = config.strategy.venue_id.as_deref() else {
            return Ok(ExactDecimal::ZERO.to_string());
        };
        let Some(configured_log_name) = account_event_log_name(account_id, configured_venue) else {
            return Ok(ExactDecimal::ZERO.to_string());
        };
        if event_log_exists(config, root, &configured_log_name)? {
            configured_log_name
        } else {
            return Ok(ExactDecimal::ZERO.to_string());
        }
    } else {
        return Ok(ExactDecimal::ZERO.to_string());
    };
    let pipeline = open_account_pipeline(config, root, &log_name, OutboxRecovery::ReadOnly)
        .map_err(|error| format!("恢复 Strategy 精确持仓 EventLog 失败: {error}"))?;
    Ok(pipeline
        .ledger()
        .position_for_side_exact(account_id, instrument, PositionSide::Net)
        .quantity
        .to_string())
}

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

/// 五家策略后端的选择梯：按声明的后端算出目标仓位，走契约后端时连同契约输出一起带回。
///
/// 顺序就是生效顺序：`builtin_strategy` > `python_module`（常驻子进程优先，没有就冷启动一次）>
/// 外部可执行 > C ABI 动态库 > 配置直给目标仓位。worker 侧新增一档只能加在这里，
/// 不允许在调用点再写一份 if-else 链（V13 R2 第十二遍 #190 抽出，纯搬运不改判定）。
pub(crate) struct StrategyContractClients<'a> {
    pub(crate) python_client: &'a mut Option<PythonStrategyClient>,
    pub(crate) external_client: &'a mut Option<StrategyProcessClient>,
    pub(crate) native_client: &'a mut Option<DynamicCAbiStrategy>,
    pub(crate) native_initialized: &'a mut bool,
    pub(crate) runtime_config_path: &'a Path,
}

pub(crate) fn evaluate_strategy_contract(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    run_id: u64,
    now: u64,
    execution_updates: &[StrategyOrderStatusUpdate],
    fill_updates: &[StrategyFillUpdateV2],
    market_ticks: &[StrategyContractMarketTickV1],
    market_order_books: &[StrategyContractMarketOrderBookV1],
    account_updates: &[qx_strategy::contract::StrategyAccountUpdateV1],
    strategy_checkpoint: Option<&qx_strategy::contract::StrategyCheckpointV1>,
    clients: StrategyContractClients<'_>,
) -> Result<
    (
        i128,
        Option<StrategyContractOutput>,
        Option<qx_strategy::contract::StrategyCheckpointV1>,
    ),
    String,
> {
    let StrategyContractClients {
        python_client,
        external_client,
        native_client,
        native_initialized,
        runtime_config_path,
    } = clients;
    let request_id = run_id.to_string();
    let (target_qty, output, checkpoint) =
        if config.strategy.builtin_strategy.is_some() {
            let output = invoke_builtin_strategy(
                root,
                config,
                Some(runtime_config_path),
                instrument,
                &request_id,
                now,
            )?;
            (output.target_qty, Some(output), None)
        } else if let Some(module) = config.strategy.python_module.as_deref() {
            let mut input = build_strategy_contract_input_with_market_events(
                root,
                config,
                instrument,
                &request_id,
                now,
                execution_updates,
                fill_updates,
                market_ticks,
                market_order_books,
            )?;
            attach_strategy_account_updates(&mut input, account_updates)?;
            if let Some(checkpoint) = strategy_checkpoint {
                input.strategy_checkpoint = Some(checkpoint.clone());
            } else if config.strategy.python_strategy_spec.is_none()
                && python_client
                    .as_ref()
                    .is_some_and(PythonStrategyClient::expects_strategy_checkpoint)
            {
                input.strategy_checkpoint = Some(StrategyCheckpointV1 {
                    schema_version: qx_strategy::contract::STRATEGY_CHECKPOINT_SCHEMA_VERSION,
                    strategy_id: input.strategy_id.clone(),
                    strategy_version: input.strategy_version.clone(),
                    adapter_state: serde_json::json!({
                        "schema_version": 1,
                        "state_mode": "bootstrap"
                    }),
                    strategy_state: serde_json::json!({}),
                });
            }
            input.schema_version = input.schema_version_for_features();
            input.validate()?;
            let output = if let Some(client) = python_client.as_mut() {
                invoke_python_strategy_with_client(client, &input)?
            } else {
                invoke_python_strategy(module, &input, config.strategy.python_timeout_ms)?
            };
            let checkpoint = python_client
                .as_mut()
                .and_then(PythonStrategyClient::take_strategy_checkpoint);
            (output.target_qty, Some(output), checkpoint)
        } else if let Some(client) = external_client.as_mut() {
            let mut input = build_strategy_contract_input_with_execution_updates(
                root,
                config,
                instrument,
                &request_id,
                now,
                execution_updates,
                fill_updates,
            )?;
            attach_strategy_account_updates(&mut input, account_updates)?;
            if client.expects_strategy_checkpoint() {
                input.strategy_checkpoint = Some(strategy_checkpoint.cloned().unwrap_or_else(
                    || StrategyCheckpointV1 {
                        schema_version: qx_strategy::contract::STRATEGY_CHECKPOINT_SCHEMA_VERSION,
                        strategy_id: input.strategy_id.clone(),
                        strategy_version: input.strategy_version.clone(),
                        adapter_state: serde_json::json!({
                            "schema_version": 1,
                            "state_mode": "external-process"
                        }),
                        strategy_state: serde_json::json!({}),
                    },
                ));
                input.schema_version = input.schema_version_for_features();
                input.validate()?;
            }
            let output = client.request(&input)?;
            let checkpoint = client.take_strategy_checkpoint();
            if checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint
                    .adapter_state
                    .get("state_mode")
                    .and_then(serde_json::Value::as_str)
                    != Some("external-process")
            }) {
                return Err("external Strategy checkpoint adapter_state 不匹配".into());
            }
            (output.target_qty, Some(output), checkpoint)
        } else if let Some(client) = native_client.as_mut() {
            let mut input = build_strategy_contract_input_with_execution_updates(
                root,
                config,
                instrument,
                &request_id,
                now,
                execution_updates,
                fill_updates,
            )?;
            attach_strategy_account_updates(&mut input, account_updates)?;
            if client.supports_state_checkpoint() {
                input.strategy_checkpoint = Some(strategy_checkpoint.cloned().unwrap_or_else(
                    || StrategyCheckpointV1 {
                        schema_version: qx_strategy::contract::STRATEGY_CHECKPOINT_SCHEMA_VERSION,
                        strategy_id: input.strategy_id.clone(),
                        strategy_version: input.strategy_version.clone(),
                        adapter_state: serde_json::json!({
                            "schema_version": 1,
                            "state_mode": "c-abi"
                        }),
                        strategy_state: serde_json::json!({}),
                    },
                ));
                input.schema_version = input.schema_version_for_features();
                input.validate()?;
            }
            let context = native_strategy_context(&config.strategy, &input)?;
            let event = qx_strategy::MarketEvent::Timer {
                name: format!("job:{run_id}"),
                ts: now,
            };
            let output = crate::invoke_c_abi_strategy_with_execution_updates(
                client,
                native_initialized,
                &context,
                &input,
                &event,
                execution_updates,
                fill_updates,
                account_updates,
            )?;
            let checkpoint = if client.supports_state_checkpoint() {
                let strategy_state = client.snapshot_strategy_checkpoint()?;
                let checkpoint = StrategyCheckpointV1 {
                    schema_version: qx_strategy::contract::STRATEGY_CHECKPOINT_SCHEMA_VERSION,
                    strategy_id: input.strategy_id.clone(),
                    strategy_version: input.strategy_version.clone(),
                    adapter_state: serde_json::json!({
                        "schema_version": 1,
                        "state_mode": "c-abi"
                    }),
                    strategy_state,
                };
                checkpoint.validate_for_input(&input)?;
                Some(checkpoint)
            } else {
                None
            };
            (output.target_qty, Some(output), checkpoint)
        } else {
            (
                strategy_target_qty(root, config, instrument, now)?,
                None,
                None,
            )
        };
    Ok((target_qty, output, checkpoint))
}

fn attach_strategy_account_updates(
    input: &mut StrategyContractInput,
    account_updates: &[qx_strategy::contract::StrategyAccountUpdateV1],
) -> Result<(), String> {
    input.account_updates = account_updates.to_vec();
    if let Some(update_as_of) = input.account_updates.iter().map(|update| update.ts).max() {
        input.as_of = input.as_of.max(update_as_of);
    }
    input.schema_version = input.schema_version_for_features();
    input.validate()
}
