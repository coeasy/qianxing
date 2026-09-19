//! 策略绑定：策略当前持仓读取与跨语言契约输入装配。

use super::*;

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
    } else if let Some(configured_venue) = config.strategy.venue_id.as_deref() {
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
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("恢复 Strategy 账户 EventLog 失败: {error}"))?;
    Ok(pipeline
        .ledger()
        .position_for(account_id, instrument)
        .quantity
        .raw())
}

pub(crate) fn build_strategy_contract_input(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
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
        validate_research_snapshot_binding(&config.strategy, &research)?;
        let research_as_of = research.as_of;
        let data_fingerprint = research.candidate.config.data_fingerprint.clone();
        let (positions, cash, available_margin_raw, risk_state) =
            strategy_account_context(root, config, instrument)?;
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
            .validate(now, config.environment.eq_ignore_ascii_case("production"))
            .map_err(|error| format!("Python StrategyContext 校验失败: {error}"))?;
        let bars = load_strategy_contract_bars(root, config, instrument, research_as_of)?
            .map(|(bars, _, _)| bars);
        return context.to_contract_input(request_id, instrument, bars);
    }

    let (positions, cash, available_margin_raw, risk_state) =
        strategy_account_context(root, config, instrument)?;
    let bars = load_strategy_contract_bars(root, config, instrument, now)?;
    let (bars, data_fingerprint, as_of) = if let Some((bars, data_fingerprint, as_of)) = bars {
        (Some(bars), data_fingerprint, as_of)
    } else {
        (None, "runtime-config-v1".into(), now.max(1))
    };
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: request_id.to_string(),
        strategy_id: config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| config.strategy.version.clone()),
        strategy_version: config.strategy.version.clone(),
        data_fingerprint,
        as_of,
        instrument: instrument.to_string(),
        positions,
        cash,
        available_margin_raw,
        risk_state,
        research_targets: BTreeMap::from([(instrument.to_string(), config.strategy.target_qty)]),
        bars,
    };
    input.validate()?;
    Ok(input)
}
