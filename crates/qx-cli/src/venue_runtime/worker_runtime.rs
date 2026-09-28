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

/// worker **自己声明**的记账币种，只是身份判定的输入之一：账户日志的口径由
/// [`settlement_currency_for_log`] 在同一本日志的全部写入方声明里唯一化，风控和成交
/// 落账则回读管线自身的 [`LiveEventPipeline::settlement_currency`]。这里不要求账户身份
/// 完整，所以只用于 worker 级行情日志和声明集合。统一大写是因为 Venue 现金流水的币种
/// 按惯例大写，大小写错位会读到一个空账簿——成交扣减和风控读取各看一本账，谁都不报错。
pub(crate) fn worker_settlement_currency(worker: &WorkerConfig) -> String {
    worker
        .settlement_currency
        .as_deref()
        .unwrap_or(DEFAULT_SETTLEMENT_CURRENCY)
        .to_ascii_uppercase()
}

/// 会动账户现金账簿的角色。Strategy/Scheduler 只提交意图，所以它们声明的结算币种
/// 不能替账户日志定记账口径。这里不要求账户身份完整，doctor 的凭据检查按同一批角色筛。
pub(crate) fn writes_account_ledger(worker: &WorkerConfig) -> bool {
    worker.enabled
        && matches!(
            worker.role,
            WorkerRole::UserStream
                | WorkerRole::Execution
                | WorkerRole::SpreadRecovery
                | WorkerRole::Reconciler
        )
}

/// 拥有账户级 EventLog 写入权的 worker。
pub(crate) fn owns_account_event_log(worker: &WorkerConfig) -> bool {
    writes_account_ledger(worker) && worker.account_id.is_some() && worker.venue_id.is_some()
}

/// 只持有日志身份的读模型按同一规则找回记账币种。
///
/// 写入方是 worker，读取方只有日志名，而账户级日志名由 `(account_id, venue_id)`
/// 唯一确定，所以用日志身份反查 worker。同一账户/venue 上多个 worker 共享一本
/// 日志，因此它们的声明必须一致：币种会随 `LedgerApplied` 事实永久落盘，按配置
/// 顺序取第一个声明者等于让第二个写入方把自己的成交记到另一本账上，两边各自
/// "自洽"，事后对账看不出问题——所以这里是配置错误，不是回退。禁用 worker 不参与，
/// 否则一个下线了的 worker 能给在线账簿改记账币种。
pub(crate) fn settlement_currency_for_log(
    config: &RuntimeConfig,
    log_name: &str,
) -> Result<String, String> {
    settlement_currency_among_workers(&config.workers, log_name)
}

/// 记账币种判定的唯一实现：只吃 worker 列表，因为装配路径未必留得住整份配置。
pub(crate) fn settlement_currency_among_workers(
    workers: &[WorkerConfig],
    log_name: &str,
) -> Result<String, String> {
    let declared = account_log_currency_declarations(workers, log_name);
    let distinct = declared
        .iter()
        .map(|(_, currency)| currency.clone())
        .collect::<BTreeSet<_>>();
    if distinct.len() > 1 {
        return Err(account_log_currency_conflict_message(log_name, &declared));
    }
    Ok(distinct
        .into_iter()
        .next()
        .unwrap_or_else(|| DEFAULT_SETTLEMENT_CURRENCY.to_string()))
}

/// 写入方打开账户级 EventLog 前的记账币种。与读模型走同一条身份判定，所以配置里
/// 出现两个口径时在落账前就失败——`worker_settlement_currency` 只看自己声明的值，
/// 用它开册等于把冲突写进永久账本。worker 级行情日志没有账户身份，仍走前者。
pub(crate) fn account_worker_settlement_currency(
    config: &RuntimeConfig,
    worker: &WorkerConfig,
) -> Result<String, String> {
    account_worker_currency_among_workers(&config.workers, worker)
}

/// 同上，供只拿得到 worker 列表的装配路径使用；列表必须是全量，子集会漏掉冲突方。
pub(crate) fn account_worker_currency_among_workers(
    workers: &[WorkerConfig],
    worker: &WorkerConfig,
) -> Result<String, String> {
    settlement_currency_among_workers(workers, &required_account_event_log(worker)?)
}

/// 同上，供只拿得到配置路径的 worker 装配路径使用：在进程启动处解一次，
/// 循环内复用，避免每条命令重读配置，也不让某一段退回 `worker_settlement_currency`。
pub(crate) fn account_worker_currency_from_path(
    runtime_config_path: &Path,
    worker: &WorkerConfig,
) -> Result<String, String> {
    account_worker_settlement_currency(&read_runtime_config(runtime_config_path)?, worker)
}

/// 该账户身份上每个启用写入方各自会用的记账币种，按 worker id 排序。
fn account_log_currency_declarations(
    workers: &[WorkerConfig],
    log_name: &str,
) -> Vec<(String, String)> {
    workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
        .filter_map(|worker| {
            let name =
                account_event_log_name(worker.account_id.as_deref()?, worker.venue_id.as_deref()?)?;
            (name == log_name).then(|| (worker.id.clone(), worker_settlement_currency(worker)))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 冲突文案点名双方 worker 和各自币种：只报"币种不一致"无法定位该改哪一段配置。
fn account_log_currency_conflict_message(log_name: &str, declared: &[(String, String)]) -> String {
    let parties = declared
        .iter()
        .map(|(worker_id, currency)| format!("{worker_id}={currency}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "FAIL_CLOSED: 账户日志 {log_name} 的写入方声明了不一致的 settlement_currency（{parties}）；\
         同一本账只能有一个记账口径，请统一后重启"
    )
}

/// doctor 用：一次列全所有账户身份上的币种冲突，不在第一个错误处中断。
pub(crate) fn account_log_settlement_conflicts(config: &RuntimeConfig) -> Vec<String> {
    config
        .workers
        .iter()
        .filter(|worker| owns_account_event_log(worker))
        .filter_map(|worker| {
            account_event_log_name(worker.account_id.as_deref()?, worker.venue_id.as_deref()?)
        })
        .collect::<BTreeSet<_>>()
        .iter()
        .filter_map(|name| settlement_currency_for_log(config, name).err())
        .collect()
}

/// doctor 的账户日志记账口径检查。
///
/// 币种冲突不会让任何一段代码报错：写入方各自按 `worker_settlement_currency` 落现金腿，
/// 读取方按日志身份反查，两边各自"自洽"地读写半本账，事后对账也看不出问题。配置能加载
/// 却没人校验，所以 doctor 在这里拦一次。
pub(crate) fn check_account_log_settlement(
    config: &RuntimeConfig,
    checks: &mut Vec<serde_json::Value>,
    failures: &mut Vec<String>,
) {
    let conflicts = account_log_settlement_conflicts(config);
    checks.push(serde_json::json!({
        "name": "account_log_settlement",
        "status": if conflicts.is_empty() { "pass" } else { "fail" },
        "message": if conflicts.is_empty() {
            "账户事实日志的启用写入方共用同一记账币种".to_string()
        } else {
            conflicts.join("；")
        }
    }));
    failures.extend(conflicts);
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
    // 记账币种只有一个真相源：这本账户日志打开时用的币种，成交的现金腿就落在它上面。
    // market spec 的 settlement_currency 说的是标的用什么币结算，不是账簿口径；拿它兜底
    // 会从一本空账簿算出 0 可用保证金，账户明明有钱却被按保证金不足拒单。
    let settlement = pipeline.settlement_currency().to_string();
    if !spec.settlement_currency.eq_ignore_ascii_case(&settlement) {
        return Err(format!(
            "FAIL_CLOSED: worker {} 的 market spec 结算币种 {} 与账户账簿币种 {settlement} 不一致；\
             可用保证金只能按账簿币种计，请统一 worker.settlement_currency 与规格文件后重启",
            worker.id, spec.settlement_currency
        ));
    }
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
    } else if VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper) {
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
    let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "worker market spec JSON 无效 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
    // 形状判定、instrument 一致性与字段校验都在 [`market_spec_from_value`] 里，与三条
    // Bar 回测链吃同一份口径；这里只负责把报错落到哪个文件说清楚。
    let spec = market_spec_from_value(&order.instrument, &market).map_err(|error| {
        format!(
            "读取 worker market spec 失败 {}: {error}",
            resolved_spec_path.display()
        )
    })?;
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
