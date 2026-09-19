use crate::*;

pub(crate) fn resolve_worker_runtime_paths(worker: &mut WorkerConfig, runtime_path: &Path) {
    if let Some(files) = worker.credential_files.as_mut() {
        files.api_key = resolve_runtime_relative_path(runtime_path, &files.api_key)
            .to_string_lossy()
            .into_owned();
        files.secret = resolve_runtime_relative_path(runtime_path, &files.secret)
            .to_string_lossy()
            .into_owned();
    }
}

/// 从执行 worker 的冻结 market spec 和同一 EventLog 构造账户级风险快照。
///
/// 没有配置 `instrument_spec_path` 时保持兼容的基础执行路径；一旦配置，
/// 订单必须同时通过规格、杠杆、名义额、可用保证金和当前持仓预检，之后才
/// 能进入 OMS/Venue。这样 Paper、Binance、CCXT 共用同一个风险边界。
pub(crate) fn worker_risk_context(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&Path>,
) -> Result<Option<(RiskContext, OrderRiskPosition)>, String> {
    let Some(spec) = load_worker_instrument_spec(worker, order, runtime_config_path)? else {
        return Ok(None);
    };
    let reference_price = order
        .limit
        .or_else(|| pipeline.marks().get(&order.instrument).copied());
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let settlement = worker
        .settlement_currency
        .as_deref()
        .unwrap_or(&spec.settlement_currency);
    let observed_available = worker.venue_id.as_deref().and_then(|venue_id| {
        pipeline
            .snapshot()
            .account_balances
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|balances| balances.iter().find(|balance| balance.asset == settlement))
            .map(|balance| balance.free.raw().saturating_sub(balance.borrowed.raw()))
    });
    let available_margin_raw = if let Some(available) = observed_available {
        available
    } else if worker
        .venue_id
        .as_deref()
        .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
    {
        pipeline
            .ledger()
            .equity_for(account_id, pipeline.marks(), settlement)
            .unwrap_or_else(|| pipeline.ledger().cash_for(account_id, settlement))
    } else {
        return Err(format!(
            "worker {} 尚未收到 {} 账户余额快照，拒绝执行账户级风控订单",
            worker.id, settlement
        ));
    };
    let aggregate = pipeline
        .ledger()
        .position_for(account_id, &order.instrument);
    let long_qty = pipeline
        .ledger()
        .position_for_side(account_id, &order.instrument, qx_core::PositionSide::Long)
        .quantity
        .raw();
    let short_qty = pipeline
        .ledger()
        .position_for_side(account_id, &order.instrument, qx_core::PositionSide::Short)
        .quantity
        .raw();
    let one_way_qty = aggregate
        .quantity
        .raw()
        .checked_sub(long_qty)
        .and_then(|value| value.checked_sub(short_qty))
        .ok_or_else(|| "拆分 one-way/hedge 持仓数量溢出".to_string())?;
    let gross_notional = reference_price
        .map(|price| {
            let one_way = spec.notional(one_way_qty.saturating_abs(), price.raw())?;
            let long = spec.notional(long_qty.saturating_abs(), price.raw())?;
            let short = spec.notional(short_qty.saturating_abs(), price.raw())?;
            one_way
                .checked_add(long)
                .and_then(|value| value.checked_add(short))
                .ok_or_else(|| qx_core::QxError::Invariant("当前 gross notional 溢出".into()))
        })
        .transpose()
        .map_err(|error| format!("计算当前持仓名义额失败: {error:?}"))?
        .unwrap_or(0);
    let position =
        OrderRiskPosition::new_with_multiplier(one_way_qty, gross_notional, spec.contract_size)
            .with_hedge_legs(long_qty, short_qty);
    Ok(Some((
        RiskContext {
            available_margin_raw: Some(available_margin_raw),
            reference_price,
            instrument_spec: Some(spec),
            max_order_notional_raw: worker.max_order_notional_raw,
            max_position_notional_raw: worker.max_position_notional_raw,
        },
        position,
    )))
}

pub(crate) fn load_worker_instrument_spec(
    worker: &WorkerConfig,
    order: &Order,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return Ok(None);
    };
    let resolved_spec_path = runtime_config_path
        .map(|path| resolve_runtime_relative_path(path, spec_path))
        .unwrap_or_else(|| PathBuf::from(spec_path));
    let payload = std::fs::read_to_string(&resolved_spec_path).map_err(|error| {
        format!(
            "读取 worker market spec 失败 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    let spec = match serde_json::from_str::<TradingInstrumentSpec>(&payload) {
        Ok(spec) => spec,
        Err(_) => {
            let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
                format!(
                    "worker market spec JSON 无效 {}: {error}",
                    resolved_spec_path.display()
                )
            })?;
            ccxt_market_to_spec(&order.instrument, &market)?
        }
    };
    spec.validate()
        .map_err(|error| format!("worker market spec 非法: {error:?}"))?;
    if spec.instrument != order.instrument {
        return Err("worker market spec instrument 与订单不一致".into());
    }
    Ok(Some(spec))
}

/// 从一批 Venue 回报解析其冻结产品规格，让回报侧精度闸门覆盖用户流链路。
///
/// 未配置 `instrument_spec_path` 时返回 `None`，保持基础执行路径；一批回报跨多份
/// 规格时拒绝归约，而不是随便挑一份套上去。
pub(crate) fn worker_report_spec(
    worker: &WorkerConfig,
    events: &[VenueEvent],
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    if worker.instrument_spec_path.is_none() {
        return Ok(None);
    }
    let orders = pipeline.orders();
    let mut resolved: Option<TradingInstrumentSpec> = None;
    for event in events {
        let client_order_id = match event {
            VenueEvent::Accepted {
                client_order_id, ..
            }
            | VenueEvent::Cancelled {
                client_order_id, ..
            } => *client_order_id,
            VenueEvent::Fill(fill) => fill.order_id,
        };
        let order = orders
            .iter()
            .find(|order| order.client_id == client_order_id)
            .ok_or_else(|| {
                format!(
                    "worker {} 回报订单 {client_order_id} 不在本地 EventLog，无法解析产品规格",
                    worker.id
                )
            })?;
        let Some(spec) = load_worker_instrument_spec(worker, order, runtime_config_path)? else {
            return Ok(None);
        };
        match &resolved {
            None => resolved = Some(spec),
            Some(existing) if existing == &spec => {}
            Some(_) => {
                return Err(format!(
                    "worker {} 的一批回报跨多份产品规格，拒绝归约",
                    worker.id
                ))
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn execute_submit_order_with_worker_risk<V: Venue>(
    command: &ControlCommand,
    worker: &WorkerConfig,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
) -> Result<String, String> {
    let order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if let Some((risk, position)) =
        worker_risk_context(worker, &order, pipeline, runtime_config_path)?
    {
        let context = RiskExecutionContext {
            risk: &risk,
            position: &position,
        };
        execute_submit_order_with_risk(
            command, venue, pipeline, &worker.id, now, source_seq, &context,
        )
    } else {
        execute_submit_order(command, venue, pipeline, &worker.id, now, source_seq)
    }
}
