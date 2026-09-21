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

/// worker 账户日志的记账币种。EventLog 按记账币种选择现金账簿，因此打开日志、播种初始资金、
/// 归约成交和构造风控快照必须使用同一个值；未声明时回落 USDT。统一大写是因为 Venue 现金流水的
/// 币种按惯例大写，大小写错位会读到一个空账簿——成交扣减和风控读取各看一本账，谁都不报错。
pub(crate) fn worker_settlement_currency(worker: &WorkerConfig) -> String {
    worker
        .settlement_currency
        .as_deref()
        .unwrap_or("USDT")
        .to_ascii_uppercase()
}

/// Paper 账户可用保证金的估值口径，必须和 `RiskContext` 里的 `initial_margin` 同一把尺子：
/// 现货买入付出现金，持仓按全额名义额计入权益才是账户价值；保证金产品开仓不动现金，
/// 账上属于这笔持仓的只有已实现/未实现 PnL，再按名义额累加等于凭空多出整笔可用保证金
/// （10k USDT 开 1 张 50k 名义的永续会把权益算成 60k，10 倍杠杆规则形同虚设，且每成交
/// 一次就更宽松）。缺少标记价格时退回纯现金，宁可保守也不放松闸门。
pub(crate) fn paper_available_margin(
    ledger: &Ledger,
    account_id: &str,
    marks: &BTreeMap<InstrumentId, Price>,
    settlement: &str,
    spec: &TradingInstrumentSpec,
) -> Result<i128, String> {
    let cash_only = ledger.cash_for(account_id, settlement);
    if !spec.product.is_derivative() {
        return Ok(ledger
            .equity_for(account_id, marks, settlement)
            .unwrap_or(cash_only));
    }
    if marks.contains_key(&spec.instrument) {
        ledger
            .equity_for_with_spec(account_id, marks, settlement, spec)
            .map_err(|error| format!("Paper 衍生品可用保证金计算失败: {error:?}"))
    } else {
        Ok(cash_only)
    }
}

/// 从执行 worker 的冻结 market spec 和同一 EventLog 构造账户级风险快照。
///
/// 返回 `None` 只表示该 worker 没有配置 `instrument_spec_path`，**不是**"允许降级到无风控提交"：
/// 提交路径必须把它当作 fail-closed 处理（见 `execute_submit_order_with_worker_risk`），
/// 只有不产生新订单的回报归约侧可以忽略。一旦配置，订单必须同时通过规格、杠杆、名义额、
/// 可用保证金和当前持仓预检，之后才能进入 OMS/Venue；Paper、Binance、CCXT 共用同一个边界。
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
    let settlement = match worker.settlement_currency.as_deref() {
        Some(currency) => currency.to_ascii_uppercase(),
        None => spec.settlement_currency.clone(),
    };
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
        paper_available_margin(
            pipeline.ledger(),
            account_id,
            pipeline.marks(),
            &settlement,
            &spec,
        )?
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

/// 提交前置门槛：没有冻结的 market spec 就没有账户级风控，实盘链一律拒绝，
/// 不再降级为无风控提交；放在 venue/凭据装配之前，让未配置的风控边界在
/// 任何外部动作发生前就失败。
pub(crate) fn require_worker_risk_spec(
    worker: &WorkerConfig,
    command: &ControlCommand,
    runtime_config_path: Option<&Path>,
) -> Result<(), String> {
    let order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if load_worker_instrument_spec(worker, &order, runtime_config_path)?.is_some() {
        return Ok(());
    }
    Err(format!(
        "FAIL_CLOSED: worker {} 缺少风控配置（instrument_spec_path），拒绝提交订单",
        worker.id
    ))
}

/// 提交一条 Binance / CCXT 实盘腿。
///
/// `spread_store` 是**必填**的多腿订单组快照来源：屏障判定已下沉到 `qx-execution`
/// 网关（V10 §6.1），CLI 只负责把存储根解析出的组存储交给它。传 `None` 不再意味着
/// "跳过屏障"，而是让任何带 `spread_group_id` 的腿以 `FAIL_CLOSED` 被拒绝。
#[allow(clippy::too_many_arguments)] // V10 P0a/P1b：风控与组存储形参由编译器逐个点名，不合并成参数包。
pub(crate) fn execute_submit_order_with_worker_risk<V: Venue>(
    command: &ControlCommand,
    worker: &WorkerConfig,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
    spread_store: Option<&dyn SpreadOrderGroupStore>,
) -> Result<String, String> {
    let order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let Some((risk, position)) =
        worker_risk_context(worker, &order, pipeline, runtime_config_path)?
    else {
        return Err(format!(
            "FAIL_CLOSED: worker {} 缺少风控配置（instrument_spec_path），拒绝提交订单",
            worker.id
        ));
    };
    let context = RiskExecutionContext {
        risk: &risk,
        position: &position,
    };
    execute_submit_order_with_risk(
        command,
        venue,
        pipeline,
        &worker.id,
        now,
        source_seq,
        &context,
        spread_store,
    )
}
