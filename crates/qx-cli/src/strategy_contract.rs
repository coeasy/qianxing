//! 策略契约装配：Bar 窗口加载、账户上下文、意图到订单的转换。
//!
//! Python / C ABI / 内置三种策略宿主共用这一份契约，使回测、Paper 与 Live
//! 走同一条 target→intent→order 路径。

use super::*;
/// 把持久化 Python/C++ JSONL 策略和受信任 C ABI 策略接入与 Rust
/// 原生策略相同的 Bar 回测循环。
/// 回测引擎传入的 history 已经按 `as_of` 截止，因此跨语言策略不会看到当前
/// 正在撮合的 Bar；输出仍必须经过统一的 OrderIntent、Risk 和 OMS 转换。
pub(crate) struct ContractBarStrategy {
    config: RuntimeConfig,
    client: ContractStrategyClient,
    instrument: InstrumentId,
    initial_cash: Money,
    currency: String,
    data_fingerprint: String,
    native_initialized: bool,
}

impl ContractBarStrategy {
    pub(crate) fn from_config(
        config: RuntimeConfig,
        frame: &BarFrame,
        initial_cash: Money,
        currency: impl Into<String>,
        dataset_bundle_fingerprint: Option<&str>,
    ) -> Result<Self, String> {
        let instrument = config
            .strategy
            .instrument
            .as_deref()
            .and_then(InstrumentId::parse)
            .ok_or_else(|| "跨语言回测策略缺少合法 strategy.instrument".to_string())?;
        if instrument != frame.instrument {
            return Err(format!(
                "跨语言回测 instrument 不一致: strategy={} frame={}",
                instrument, frame.instrument
            ));
        }
        let client = if let Some(module) = config.strategy.python_module.as_deref() {
            ContractStrategyClient::Process(Box::new(
                PythonStrategyClient::start_with_transport_config(
                    module,
                    config.strategy.python_timeout_ms,
                    config.strategy.transport,
                    SharedRingConfig {
                        capacity: config.strategy.shared_memory_capacity,
                        slot_bytes: config.strategy.shared_memory_slot_bytes,
                    },
                    config.strategy.strategy_artifact_sha256.as_deref(),
                )?,
            ))
        } else if let Some(executable) = config.strategy.external_executable.as_deref() {
            ContractStrategyClient::Process(Box::new(
                StrategyProcessClient::start_process_with_transport_config(
                    executable,
                    &config.strategy.external_args,
                    &config.strategy.external_env,
                    config.strategy.python_timeout_ms,
                    "外部 Strategy",
                    config.strategy.transport,
                    SharedRingConfig {
                        capacity: config.strategy.shared_memory_capacity,
                        slot_bytes: config.strategy.shared_memory_slot_bytes,
                    },
                    None,
                )?,
            ))
        } else if let Some(library) = config.strategy.c_abi_library.as_deref() {
            let _ = library;
            let native = load_c_abi_strategy(&config.strategy)?;
            ContractStrategyClient::Native(Box::new(native))
        } else {
            return Err(
                "跨语言回测必须配置 strategy.python_module、external_executable 或 c_abi_library"
                    .into(),
            );
        };
        Ok(Self {
            config,
            client,
            instrument,
            initial_cash,
            currency: currency.into(),
            data_fingerprint: dataset_bundle_fingerprint
                .map(|fingerprint| format!("dataset-bundle:{fingerprint}"))
                .unwrap_or_else(|| format!("{:016x}", frame.digest())),
            native_initialized: false,
        })
    }
}

impl BarStrategy for ContractBarStrategy {
    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let Some(visible) = history.last() else {
            return Ok(Vec::new());
        };
        if instrument != &self.instrument || visible.ts >= ts {
            return Err(qx_core::QxError::Invariant(
                "跨语言 Bar 策略收到不可见或未绑定的 Bar".into(),
            ));
        }
        let bars = StrategyContractBars {
            source: "backtest-bar-history-v1".into(),
            ts: history.iter().map(|bar| bar.ts).collect(),
            open_raw: history.iter().map(|bar| bar.open).collect(),
            high_raw: history.iter().map(|bar| bar.high).collect(),
            low_raw: history.iter().map(|bar| bar.low).collect(),
            close_raw: history.iter().map(|bar| bar.close).collect(),
            volume_raw: history.iter().map(|bar| bar.volume).collect(),
        };
        let strategy_id = self
            .config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| self.config.strategy.version.clone());
        let input = StrategyContractInput {
            schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: format!(
                "{strategy_id}:backtest:{visible_ts}",
                visible_ts = visible.ts
            ),
            strategy_id: strategy_id.clone(),
            strategy_version: self.config.strategy.version.clone(),
            data_fingerprint: self.data_fingerprint.clone(),
            as_of: visible.ts,
            instrument: instrument.to_string(),
            positions: BTreeMap::from([(instrument.to_string(), position)]),
            cash: BTreeMap::from([(self.currency.clone(), self.initial_cash.raw())]),
            available_margin_raw: Some(self.initial_cash.raw()),
            risk_state: "backtest-verified".into(),
            research_targets: BTreeMap::new(),
            bars: Some(bars),
        };
        let output = match &mut self.client {
            ContractStrategyClient::Process(client) => client
                .request(&input)
                .map_err(qx_core::QxError::BusinessViolation)?,
            ContractStrategyClient::Native(strategy) => {
                let context = native_strategy_context(&self.config.strategy, &input);
                let event = NativeMarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts: visible.ts,
                    open_raw: visible.open,
                    high_raw: visible.high,
                    low_raw: visible.low,
                    close_raw: visible.close,
                    volume_raw: visible.volume,
                };
                invoke_c_abi_strategy(
                    strategy,
                    &mut self.native_initialized,
                    &context,
                    &input,
                    &event,
                )
                .map_err(qx_core::QxError::BusinessViolation)?
            }
        };
        let mut orders = Vec::with_capacity(output.intents.len().max(1));
        if output.intents.is_empty() {
            let rebalance = output
                .build_rebalance_plan(&input, 10_000, 1)
                .map_err(qx_core::QxError::BusinessViolation)?;
            if rebalance.positions.is_empty() {
                return Ok(Vec::new());
            }
            if let Some(order) = build_strategy_order_with_signal(
                &self.config,
                &strategy_id,
                output.signal_id,
                visible.ts,
                position,
                output.target_qty,
                Some(&output),
            )
            .map_err(qx_core::QxError::BusinessViolation)?
            {
                orders.push(order);
            }
        } else {
            for intent in &output.intents {
                orders.push(
                    build_strategy_order_from_contract_intent(
                        &self.config,
                        &strategy_id,
                        output.signal_id,
                        intent,
                        visible.ts,
                        position,
                    )
                    .map_err(qx_core::QxError::BusinessViolation)?,
                );
            }
        }
        Ok(orders)
    }
}

pub(crate) fn strategy_target_qty(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    now: u64,
) -> Result<i128, String> {
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
                    "读取 Strategy research snapshot 失败 {}: {error}",
                    path.display()
                )
            })?,
        )
        .map_err(|error| format!("Strategy research snapshot JSON 无效: {error:?}"))?;
        validate_research_snapshot_binding(&config.strategy, &research)?;
        let account_id = config
            .strategy
            .account_id
            .clone()
            .ok_or_else(|| "research snapshot 策略必须配置 account_id".to_string())?;
        let venue_id = config
            .strategy
            .venue_id
            .clone()
            .ok_or_else(|| "research snapshot 策略必须配置 venue_id".to_string())?;
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
            .map_err(|error| format!("StrategyContext 校验失败: {error}"))?;
        return context.target_for(instrument).ok_or_else(|| {
            format!(
                "Strategy research snapshot 未提供 instrument={} 的目标仓位",
                instrument
            )
        });
    }
    let Some(configured) = config.strategy.target_snapshot_path.as_deref() else {
        return Ok(config.strategy.target_qty);
    };
    let candidate = runtime_path(root, configured);
    let path = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(configured)
    };
    let snapshot: StrategyTargetSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "读取 Strategy target snapshot 失败 {}: {error}",
                path.display()
            )
        })?)
        .map_err(|error| format!("Strategy target snapshot JSON 无效: {error}"))?;
    snapshot.validate_for(&config.strategy.version, now)?;
    snapshot.target_for(instrument).ok_or_else(|| {
        format!(
            "Strategy target snapshot 未提供 instrument={} 的目标仓位",
            instrument
        )
    })
}

pub(crate) type StrategyAccountContext = (
    BTreeMap<String, i128>,
    BTreeMap<String, i128>,
    Option<i128>,
    String,
);

pub(crate) fn load_strategy_contract_bars(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    as_of: u64,
) -> Result<Option<(StrategyContractBars, String, u64)>, String> {
    let Some(configured) = config.strategy.bars_snapshot_path.as_deref() else {
        return Ok(None);
    };
    let candidate = runtime_path(root, configured);
    let path = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(configured)
    };
    let payload = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "读取 Strategy bars_snapshot_path 失败 {}: {error}",
            path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "Strategy bars_snapshot_path BarFrame 无效 {}: {error:?}",
            path.display()
        )
    })?;
    if &frame.instrument != instrument {
        return Err(format!(
            "Strategy bars_snapshot_path instrument 不一致: strategy={} frame={}",
            instrument, frame.instrument
        ));
    }
    let visible: Vec<usize> = frame
        .ts
        .iter()
        .enumerate()
        .filter_map(|(index, ts)| (*ts <= as_of).then_some(index))
        .collect();
    let Some(last_index) = visible.last().copied() else {
        return Err(format!(
            "Strategy bars_snapshot_path 在 as_of={} 前没有可见 Bar",
            as_of
        ));
    };
    let bars = StrategyContractBars {
        source: frame.source.0.clone(),
        ts: visible.iter().map(|index| frame.ts[*index]).collect(),
        open_raw: visible.iter().map(|index| frame.open_raw[*index]).collect(),
        high_raw: visible.iter().map(|index| frame.high_raw[*index]).collect(),
        low_raw: visible.iter().map(|index| frame.low_raw[*index]).collect(),
        close_raw: visible
            .iter()
            .map(|index| frame.close_raw[*index])
            .collect(),
        volume_raw: visible
            .iter()
            .map(|index| frame.volume_raw[*index])
            .collect(),
    };
    bars.validate()?;
    Ok(Some((
        bars,
        format!("barframe:{:016x}", frame.digest()),
        frame.ts[last_index],
    )))
}

pub(crate) fn strategy_account_context(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<StrategyAccountContext, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 venue_id".to_string())?;
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Err(format!(
            "StrategyContext 当前不支持 venue_id={} 的账户事实归约",
            venue_id
        ));
    };
    if !event_log_exists(config, root, &log_name)? {
        return Ok((
            BTreeMap::new(),
            BTreeMap::new(),
            None,
            "account-snapshot-not-seen".into(),
        ));
    }
    let pipeline = open_account_pipeline(config, root, &log_name)
        .map_err(|error| format!("恢复 StrategyContext 账户 EventLog 失败: {error}"))?;
    let state = pipeline.ledger().position_for(account_id, instrument);
    let positions = BTreeMap::from([(instrument.to_string(), state.quantity.raw())]);
    let cash = pipeline.ledger().cash_balances_for(account_id);
    let available_margin_raw =
        pipeline
            .ledger()
            .equity_for(account_id, pipeline.marks(), pipeline.settlement_currency());
    Ok((
        positions,
        cash,
        available_margin_raw,
        "ledger-replayed-account-state".into(),
    ))
}

#[cfg(test)]
pub(crate) fn build_strategy_order(
    config: &RuntimeConfig,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
) -> Result<Option<Order>, String> {
    build_strategy_order_with_signal(
        config,
        strategy_id,
        run_id,
        now,
        current_qty,
        target_qty,
        None,
    )
}

/// 将跨语言 Strategy API v1 的单笔 intent 转为统一核心订单。
/// 该转换只负责语义翻译，订单仍必须经过 RiskExecutionContext、OMS 和 Venue。
pub(crate) fn build_strategy_order_from_contract_intent(
    config: &RuntimeConfig,
    strategy_id: &str,
    signal_id: u64,
    intent: &StrategyContractIntent,
    now: u64,
    current_qty: i128,
) -> Result<Order, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 account_id".to_string())?;
    let instrument = InstrumentId::parse(&intent.instrument)
        .ok_or_else(|| format!("Strategy intent instrument 非法: {}", intent.instrument))?;
    let side = match intent.side.to_ascii_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        other => return Err(format!("Strategy intent side 非法: {other}")),
    };
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    let position_mode = match intent.position_mode.as_deref() {
        None => config
            .strategy
            .position_mode
            .unwrap_or(PositionMode::OneWay),
        Some("one_way") => PositionMode::OneWay,
        Some("hedge") => PositionMode::Hedge,
        Some(other) => return Err(format!("Strategy intent position_mode 非法: {other}")),
    };
    let margin_mode = match intent.margin_mode.as_deref() {
        None => config
            .strategy
            .margin_mode
            .unwrap_or(if product == TradingProduct::Spot {
                MarginMode::Cash
            } else {
                MarginMode::Cross
            }),
        Some("cash") => MarginMode::Cash,
        Some("cross") => MarginMode::Cross,
        Some("isolated") => MarginMode::Isolated,
        Some(other) => return Err(format!("Strategy intent margin_mode 非法: {other}")),
    };
    let leverage = intent
        .leverage
        .unwrap_or(config.strategy.leverage.unwrap_or(1));
    let allow_short = strategy_allows_short(config, margin_mode);
    let position_side = match intent
        .position_side
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("long") => PositionSide::Long,
        Some("short") => PositionSide::Short,
        Some("net") | None if position_mode == PositionMode::OneWay => PositionSide::Net,
        Some("net") | None => {
            if side == Side::Buy {
                PositionSide::Long
            } else {
                PositionSide::Short
            }
        }
        Some(other) => return Err(format!("Strategy intent position_side 非法: {other}")),
    };
    let policy = OrderPolicy {
        reduce_only: intent.reduce_only,
        position_side,
        margin_mode,
        position_mode,
        leverage,
        post_only: intent.post_only,
    };
    let mut order = Order {
        client_id: intent.intent_id,
        instrument,
        side,
        qty: qx_core::Quantity::from_raw(intent.qty_raw),
        limit: intent.limit_price_raw.map(Price::from_raw),
        status: OrderStatus::PendingSubmit,
        filled: qx_core::Quantity::ZERO,
        account_id: account_id.into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some(strategy_id.into()),
            signal_id: Some(signal_id),
            intent_id: Some(intent.intent_id),
            rule_version: Some(config.strategy.version.clone()),
        }),
        policy: None,
    };
    if product != TradingProduct::Spot
        || config.strategy.product.is_some()
        || config.strategy.margin_mode.is_some()
        || config.strategy.position_mode.is_some()
        || config.strategy.leverage.is_some()
        || intent.margin_mode.is_some()
        || intent.position_mode.is_some()
        || intent.leverage.is_some()
        || intent.reduce_only
        || intent.post_only
        || intent.position_side.is_some()
    {
        order.policy = Some(policy);
    }
    order
        .validate()
        .map_err(|error| format!("Strategy API v1 OrderIntent 转订单失败: {error}"))?;
    let risk = strategy_risk_gate(config.strategy.risk_rules.as_ref(), allow_short);
    risk.check(&order, &OrderRiskPosition::new(current_qty, 0))
        .map_err(|error| format!("Strategy API v1 RiskGate 拒绝 OrderIntent: {error:?}"))?;
    let _ = now;
    Ok(order)
}

pub(crate) fn build_strategy_order_with_signal(
    config: &RuntimeConfig,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
    contract_output: Option<&StrategyContractOutput>,
) -> Result<Option<Order>, String> {
    let instrument_text = config
        .strategy
        .instrument
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 instrument".to_string())?;
    let current = qx_zhenlu::portfolio::PortfolioState {
        portfolio_id: strategy_id.into(),
        timestamp: now,
        cash: 0,
        positions: BTreeMap::from([(instrument_text.to_string(), current_qty)]),
    };
    let rebalance = qx_zhenlu::portfolio::rebalance(
        &current,
        &[qx_zhenlu::portfolio::TargetPosition::single(
            InstrumentId::parse(instrument_text)
                .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?,
            target_qty,
        )],
        &qx_zhenlu::portfolio::PortfolioConstraint {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        },
    )?;
    if rebalance.positions.is_empty() {
        return Ok(None);
    }
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 account_id".to_string())?;
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    let signal = Signal {
        strategy_id: strategy_id.into(),
        signal_id: contract_output
            .map(|output| output.signal_id)
            .unwrap_or_else(|| run_id.max(1)),
        instrument: instrument.clone(),
        target_qty,
        confidence: contract_output
            .map(|output| output.confidence)
            .unwrap_or(1_000),
        priority: contract_output.map(|output| output.priority).unwrap_or(0),
        expires_at: contract_output
            .map(|output| {
                if output.expires_at == 0 {
                    now
                } else {
                    output.expires_at
                }
            })
            .unwrap_or(now),
    };
    let targets = SignalMerger.merge(vec![signal], now);
    let target = targets
        .first()
        .ok_or_else(|| "Strategy Signal 已过期或为空".to_string())?;
    let Some(intent) = rebalance_intent(
        target,
        current_qty,
        strategy_id,
        account_id,
        run_id.max(1),
        now,
    ) else {
        return Ok(None);
    };
    let order = intent.into_order();
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    let position_mode = config
        .strategy
        .position_mode
        .unwrap_or(PositionMode::OneWay);
    let margin_mode = config_margin_mode(config);
    let allow_short = strategy_allows_short(config, margin_mode);
    let leverage = config.strategy.leverage.unwrap_or(1);
    let mut order = order;
    if product != TradingProduct::Spot
        || config.strategy.product.is_some()
        || config.strategy.margin_mode.is_some()
        || config.strategy.position_mode.is_some()
        || config.strategy.leverage.is_some()
    {
        order.policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: if position_mode == PositionMode::OneWay {
                PositionSide::Net
            } else if target_qty >= 0 {
                PositionSide::Long
            } else {
                PositionSide::Short
            },
            margin_mode,
            position_mode,
            leverage,
            post_only: false,
        });
    }
    order
        .validate()
        .map_err(|error| format!("Strategy OrderIntent 转订单失败: {error}"))?;
    let risk = strategy_risk_gate(config.strategy.risk_rules.as_ref(), allow_short);
    risk.check(&order, &OrderRiskPosition::new(current_qty, 0))
        .map_err(|error| format!("Strategy RiskGate 拒绝 OrderIntent: {error:?}"))?;
    Ok(Some(order))
}

pub(crate) fn strategy_submit_command(
    strategy_id: &str,
    order: &Order,
    dry_run: bool,
    spread_group_id: Option<&str>,
) -> Result<ControlCommand, String> {
    let mut payload = BTreeMap::from([(
        "order_json".into(),
        serde_json::to_string(order)
            .map_err(|error| format!("Strategy 订单序列化失败: {error}"))?,
    )]);
    if let Some(group_id) = spread_group_id {
        if group_id.trim().is_empty() {
            return Err("Strategy 多腿 spread_group_id 不能为空".into());
        }
        payload.insert("spread_group_id".into(), group_id.into());
    }
    Ok(ControlCommand {
        command_id: order.client_id,
        request_id: format!("strategy:{strategy_id}:{}", order.client_id),
        operator_id: strategy_id.into(),
        reason: "strategy signal -> portfolio -> risk -> order intent".into(),
        kind: CommandKind::SubmitOrder,
        target: order.client_id.to_string(),
        payload,
        permission: Permission::Trading,
        dry_run,
    })
}

pub(crate) fn run_runtime_api(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let service = build_configured_api_service(&config, path)?;
    let listener = TcpListener::bind(&config.api.bind)
        .map_err(|error| format!("绑定 API 地址失败 {}: {error}", config.api.bind))?;
    println!(
        "[运行时 · API] bind={} transport={:?}，按 Ctrl+C 停止",
        config.api.bind, config.api.transport
    );
    match config.api.transport {
        ApiTransport::Plaintext => {
            let projection_stop = Arc::new(AtomicBool::new(false));
            let mut projection_thread =
                spawn_api_projection_bridge(&config, service.clone(), Arc::clone(&projection_stop));
            let worker = match supervisor.spawn_worker("api", move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve(listener, runtime_timestamp_ms())
                    .map_err(|error| format!("API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
                    return Err(error);
                }
            };
            let result = worker.join().map_err(|_| "API worker panic".to_string())?;
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            result
        }
        ApiTransport::Mtls => {
            let tls = config
                .api
                .tls
                .as_ref()
                .ok_or_else(|| "mTLS API 缺少 tls 配置".to_string())?;
            let server_config = load_mtls_server_config_from_pem(
                &tls.certificate_chain,
                &tls.private_key,
                &tls.client_ca,
            )?;
            let operator_paths = config
                .api
                .operators
                .iter()
                .map(|(operator_id, operator)| {
                    (operator_id.clone(), PathBuf::from(&operator.certificate))
                })
                .collect::<BTreeMap<_, _>>();
            let identity_reloader = MtlsIdentityPemReloader::new(operator_paths)?;
            let identity_store = MtlsIdentityStore::new(identity_reloader.load()?);
            let reloader = TlsPemReloader::new(
                tls.certificate_chain.clone(),
                tls.private_key.clone(),
                tls.client_ca.clone(),
            );
            let store = TlsConfigStore::new(server_config);
            let projection_stop = Arc::new(AtomicBool::new(false));
            let mut projection_thread =
                spawn_api_projection_bridge(&config, service.clone(), Arc::clone(&projection_stop));
            let reload_stop = Arc::new(AtomicBool::new(false));
            let reload_stop_thread = Arc::clone(&reload_stop);
            let reload_store = store.clone();
            let reload_identity_store = identity_store.clone();
            let reload_thread = thread::spawn(move || {
                while !reload_stop_thread.load(Ordering::Acquire) {
                    if let Err(error) = reloader.reload_if_changed(&reload_store) {
                        eprintln!("[运行时 · TLS] 证书轮询重载失败，保留当前配置: {error}");
                    }
                    if let Err(error) = identity_reloader.reload_if_changed(&reload_identity_store)
                    {
                        eprintln!(
                            "[运行时 · TLS] Operator 证书轮询重载失败，保留当前映射: {error}"
                        );
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            });
            let worker = match supervisor.spawn_worker("api", move |context| {
                context.heartbeat(runtime_timestamp_ms())?;
                service
                    .serve_tls_mtls_with_stores(
                        listener,
                        &store,
                        &identity_store,
                        runtime_timestamp_ms(),
                    )
                    .map_err(|error| format!("mTLS API 服务停止: {error}"))
            }) {
                Ok(worker) => worker,
                Err(error) => {
                    reload_stop.store(true, Ordering::Release);
                    let _ = reload_thread.join();
                    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
                    return Err(error);
                }
            };
            let result = worker
                .join()
                .map_err(|_| "mTLS API worker panic".to_string());
            reload_stop.store(true, Ordering::Release);
            let _ = reload_thread.join();
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            result?
        }
    }
}

pub(crate) fn stop_api_projection_bridge(
    stop: &Arc<AtomicBool>,
    thread: &mut Option<std::thread::JoinHandle<()>>,
) {
    stop.store(true, Ordering::Release);
    if let Some(thread) = thread.take() {
        let _ = thread.join();
    }
}
