//! 策略绑定：策略当前持仓读取与跨语言契约输入装配。

mod contract_input;
#[allow(unused_imports)]
pub(crate) use contract_input::{
    build_strategy_contract_input, build_strategy_contract_input_with_execution_updates,
    build_strategy_contract_input_with_market_events,
    build_strategy_contract_input_with_market_ticks, build_strategy_contract_input_with_updates,
};

use crate::{
    account_event_log_name, event_log_exists, invoke_builtin_strategy, invoke_python_strategy,
    invoke_python_strategy_with_client, native_strategy_context, open_account_pipeline,
    strategy_target_qty, PythonStrategyClient, StrategyProcessClient,
};
use qx_core::{ExactDecimal, InstrumentId, PositionSide};
use qx_runtime::{
    OutboxRecovery, RuntimeConfig, StrategyContractInput, StrategyContractOutput, WorkerConfig,
};
use qx_strategy::contract::{
    StrategyCheckpointV1, StrategyContractMarketOrderBookV1, StrategyContractMarketTickV1,
};
use qx_strategy::{DynamicCAbiStrategy, StrategyFillUpdateV2, StrategyOrderStatusUpdate};
use std::path::Path;

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
                            "state_mode": "bootstrap"
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
