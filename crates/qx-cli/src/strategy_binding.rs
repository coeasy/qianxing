//! 策略绑定：策略当前持仓读取、跨语言契约输入装配，以及内置策略信号参数的唯一读点。

use super::*;

/// 这套信号口径从哪来：四项全缺 = 写死默认，任一项给了 = 配置。
///
/// 四条链共用这句判词，否则"来源"由各入口各自臆测 —— `[Builtin · Cost] source=` 一族
/// 撒过谎的地方（Q0b）就是这么来的。判词只回答"配置提过没有"，"提过的项上没上场"由
/// [`BuiltinSignalProvenance`] 一起给出。
///
/// 这份配置声明了哪几项信号旋钮（按 `BuiltinSignalKnob::ALL` 的稳定顺序）。
///
/// 声明与生效是两件事：`builtin_fast_window` 对 MACD 而言从没上过场（V12 #102），
/// 而"配了哪几项"只有配置本身知道。播报把两份都印出来，读者才分得清
/// "没人提这一项"、"提了但这一轮不上场"与"提了并且改了信号"。
pub(crate) fn builtin_signal_declared(
    strategy: &StrategyRuntimeConfig,
) -> Vec<qx_strategy::builtin_signal::BuiltinSignalKnob> {
    use qx_strategy::builtin_signal::BuiltinSignalKnob;
    BuiltinSignalKnob::ALL
        .into_iter()
        .filter(|knob| match knob {
            BuiltinSignalKnob::FastWindow => strategy.builtin_fast_window.is_some(),
            BuiltinSignalKnob::SlowWindow => strategy.builtin_slow_window.is_some(),
            BuiltinSignalKnob::Period => strategy.builtin_period.is_some(),
            BuiltinSignalKnob::ThresholdBps => strategy.builtin_threshold_bps.is_some(),
        })
        .collect()
}

/// 一次运行的信号声明面：来源判词，加上"这个 kind 上场的清单"与"配置提了却没上场的键"。
///
/// 三项一次算清，播报那行与摘要那块才不会各渲染一套（V12 #102）。此前 `source=config`
/// 只回答"配置提过这一项"，读者据此以为印出来的四项都在改动信号。
#[derive(Clone, Debug)]
pub(crate) struct BuiltinSignalProvenance {
    pub(crate) source: &'static str,
    /// 这个 kind 的信号真正读的几项，逗号分隔；内核常数策略（MACD）为 `none`。
    pub(crate) knobs: String,
    /// 配置里声明了、这一轮却没有上场的键名；`none` 表示声明的几项全在场。
    pub(crate) declared_unused: String,
}

impl BuiltinSignalProvenance {
    /// 没给 `--config` 的那一侧：声明面是空的，但"哪些项本来就该上场"仍然要说清。
    pub(crate) fn defaults(kind: qx_strategy::BuiltinStrategyKind) -> BuiltinSignalProvenance {
        BuiltinSignalProvenance {
            source: "builtin-default",
            knobs: kind.signal_knob_list(),
            declared_unused: "none".to_string(),
        }
    }

    pub(crate) fn render(
        kind: qx_strategy::BuiltinStrategyKind,
        strategy: &StrategyRuntimeConfig,
    ) -> Self {
        let declared = builtin_signal_declared(strategy);
        let unused = declared
            .iter()
            .filter(|knob| !kind.uses_signal_knob(**knob))
            .map(|knob| knob.config_key())
            .collect::<Vec<_>>();
        Self {
            source: if declared.is_empty() {
                "builtin-default"
            } else {
                "config"
            },
            knobs: kind.signal_knob_list(),
            declared_unused: if unused.is_empty() {
                "none".to_string()
            } else {
                unused.join(",")
            },
        }
    }
}

/// `strategy.builtin_*` 四个信号参数的唯一读点（V11 Q64）。
///
/// 逐项可省：给了哪项换哪项，缺项留 `BuiltinStrategyConfig` 的默认。四条 Bar 回测链都走这里，
/// 于是同一份 `--config` 不会在各入口得到两套窗口。这里不做体检：双腿 kind 的
/// `reference_instrument` 在调用方才补齐，先验证会误报。
pub(crate) fn apply_builtin_signal_overrides(
    config: &mut BuiltinStrategyConfig,
    strategy: &StrategyRuntimeConfig,
) {
    if let Some(window) = strategy.builtin_fast_window {
        config.fast_window = window;
    }
    if let Some(window) = strategy.builtin_slow_window {
        config.slow_window = window;
    }
    if let Some(period) = strategy.builtin_period {
        config.period = period;
    }
    if let Some(threshold) = strategy.builtin_threshold_bps {
        config.threshold_bps = threshold;
    }
}

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
    let pipeline = open_account_pipeline(config, root, &log_name)
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
        validate_research_snapshot_binding(root, &config.strategy, &research)?;
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
