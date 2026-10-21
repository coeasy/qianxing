//! Shared construction of the strategy risk gate from runtime configuration.

use crate::worker_market_spec::load_worker_market_spec_for_instrument;
use qx_core::{
    InstrumentId, MarginMode, Order, Price, TradingInstrumentSpec, TradingProduct, VenueFamily,
    SCALE,
};
use qx_risk::{MaxNotionalRule, MaxQtyRule, NoShortRule, OrderRiskPosition, RuleSet};
use qx_runtime::{
    required_account_event_log, LiveEventPipeline, RuntimeConfig, WorkerConfig, WorkerRole,
};
use qx_zhenlu::{RiskContext, RiskGate};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The slowest reconciler (Binance) polls every 30s; allow one delayed cycle.
pub const LIVE_BALANCE_SNAPSHOT_MAX_AGE_MS: u64 = 60_000;

/// Fail closed when live order admission has no recent exchange balance fact.
pub fn require_fresh_live_balance_snapshot(
    worker_id: &str,
    account_id: Option<&str>,
    venue_id: Option<&str>,
    pipeline: &LiveEventPipeline,
    now: u64,
) -> Result<(), String> {
    if qx_core::VenueFamily::parse_option(venue_id) == Some(qx_core::VenueFamily::Paper) {
        return Ok(());
    }
    let account_id = account_id.ok_or_else(|| format!("worker {worker_id} 缺少 account_id"))?;
    let venue_id = venue_id.ok_or_else(|| format!("worker {worker_id} 缺少 venue_id"))?;
    let snapshot = pipeline.log().events().iter().rev().find(|event| {
        matches!(
            &event.kind,
            qx_core::EventKind::AccountBalanceSnapshot {
                account_id: snapshot_account,
                venue_id: snapshot_venue,
                ..
            } if snapshot_account == account_id && snapshot_venue.eq_ignore_ascii_case(venue_id)
        )
    });
    let Some(snapshot) = snapshot else {
        return Err(format!(
            "FAIL_CLOSED: worker {worker_id} 尚无账户余额快照，拒绝提交订单"
        ));
    };
    let age_ms = now
        .checked_sub(snapshot.ts)
        .ok_or_else(|| format!("FAIL_CLOSED: worker {worker_id} 的账户余额快照时间晚于下单时间"))?;
    if age_ms > LIVE_BALANCE_SNAPSHOT_MAX_AGE_MS {
        return Err(format!(
            "FAIL_CLOSED: worker {worker_id} 的账户余额快照已过期 {age_ms}ms（上限 {}ms），需先完成交易所对账",
            LIVE_BALANCE_SNAPSHOT_MAX_AGE_MS
        ));
    }
    Ok(())
}

/// Serialize account order admission across workers sharing one account event log.
pub fn worker_account_admission_lock(
    root: &Path,
    log_name: &str,
) -> Result<qx_core::FileLock, String> {
    qx_core::FileLock::acquire(
        root.join("account-admission")
            .join(format!("{log_name}.lock")),
    )
    .map_err(|error| format!("获取账户下单串行锁失败: {error}"))
}

/// Submit a live order only after the shared account lock, EventLog refresh, fresh-balance
/// requirement, and frozen-spec risk calculation have all succeeded.
#[allow(clippy::too_many_arguments)]
pub fn execute_submit_order_with_worker_risk<V: qx_zhenlu::Venue>(
    command: &qx_control::ControlCommand,
    worker: &WorkerConfig,
    root: &Path,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
    spread_store: Option<&dyn qx_zhenlu::SpreadOrderGroupStore>,
) -> Result<String, String> {
    let order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let log_name = required_account_event_log(worker)?;
    let _admission_lock = worker_account_admission_lock(root, &log_name)?;
    pipeline
        .refresh()
        .map_err(|error| format!("刷新账户风控 EventLog 失败: {error:?}"))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    require_fresh_live_balance_snapshot(
        &worker.id,
        worker.account_id.as_deref(),
        worker.venue_id.as_deref(),
        pipeline,
        timestamp,
    )?;
    let Some((risk, position)) =
        worker_risk_context(worker, &order, pipeline, None, runtime_config_path)?
    else {
        return Err(format!(
            "FAIL_CLOSED: worker {} 缺少风控配置（instrument_spec_path），拒绝提交订单",
            worker.id
        ));
    };
    let context = qx_execution::RiskExecutionContext {
        risk: &risk,
        position: &position,
    };
    qx_execution::submit_order_with_risk(qx_execution::SubmitOrderWithRiskRequest {
        command,
        venue,
        pipeline,
        worker_id: &worker.id,
        now,
        source_seq,
        context: &context,
        spread_store,
    })
}

/// Conservative default for a run without explicit risk rules.
pub const CONSERVATIVE_MAX_QTY_RAW: i128 = 1_000 * SCALE;
pub const CONSERVATIVE_DEFAULT_RULE_SET_VERSION: &str = "conservative-default-v1";

pub fn strategy_risk_gate(
    rules: Option<&qx_runtime::RiskRulesConfig>,
    allow_short: bool,
) -> RiskGate {
    let short_banned_by_config = rules.is_some_and(|rules| rules.no_short);
    let implicit_no_short = !allow_short && !short_banned_by_config;
    let conservative_max_qty = rules.is_none_or(|rules| {
        rules.max_qty_raw.is_none() && rules.max_notional_raw.is_none() && !rules.no_short
    });
    let mut version = match rules {
        None => CONSERVATIVE_DEFAULT_RULE_SET_VERSION.to_string(),
        Some(rules) if conservative_max_qty => {
            format!("{}+conservative-max-qty", rules.version)
        }
        Some(rules) => rules.version.clone(),
    };
    if implicit_no_short {
        version.push_str("+implicit-no-short");
    }
    let mut rule_set = RuleSet::with_version(version);
    if conservative_max_qty {
        rule_set.add(Box::new(MaxQtyRule {
            max_qty: CONSERVATIVE_MAX_QTY_RAW,
        }));
    }
    if let Some(rules) = rules {
        if let Some(max_qty) = rules.max_qty_raw {
            rule_set.add(Box::new(MaxQtyRule { max_qty }));
        }
        if let Some(max_notional) = rules.max_notional_raw {
            rule_set.add(Box::new(MaxNotionalRule { max_notional }));
        }
        if rules.no_short {
            rule_set.add(Box::new(NoShortRule));
        }
    }
    if implicit_no_short {
        rule_set.add(Box::new(NoShortRule));
    }
    RiskGate::from_rule_set(rule_set)
}

pub fn strategy_allows_short(config: &RuntimeConfig, margin_mode: MarginMode) -> bool {
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    qx_core::short_selling_enabled(product, margin_mode, config.strategy.allow_short)
}

pub fn config_margin_mode(config: &RuntimeConfig) -> MarginMode {
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    qx_core::margin_mode_for_product(product, config.strategy.margin_mode)
}

/// Compute Paper buying-power reservations for open account orders.
pub struct PaperPendingReserveInputs<'a> {
    pub candidate_order_id: u64,
    pub account_id: &'a str,
    pub market_specs: &'a BTreeMap<InstrumentId, TradingInstrumentSpec>,
    pub marks: &'a BTreeMap<InstrumentId, Price>,
    pub fx_rates: &'a BTreeMap<String, Price>,
    pub reporting_currency: &'a str,
    pub funding_currency: &'a str,
}

pub fn paper_pending_order_reserve_raw(
    orders: &[Order],
    inputs: PaperPendingReserveInputs<'_>,
) -> Result<i128, String> {
    let PaperPendingReserveInputs {
        candidate_order_id,
        account_id,
        market_specs,
        marks,
        fx_rates,
        reporting_currency,
        funding_currency,
    } = inputs;
    orders
        .iter()
        .filter(|order| {
            order.client_id != candidate_order_id
                && order.account_id == account_id
                && !order.status.is_terminal()
                && order.remaining().raw() > 0
        })
        .try_fold(0_i128, |total, order| {
            let spec = market_specs.get(&order.instrument).ok_or_else(|| {
                format!(
                    "FAIL_CLOSED: Paper 在途订单 {} 缺少冻结 market spec",
                    order.client_id
                )
            })?;
            let policy = order.policy.unwrap_or_default();
            policy.validate_for(spec).map_err(|error| {
                format!("FAIL_CLOSED: Paper 在途订单 {} policy 无效: {error}", order.client_id)
            })?;
            let reserve_amount = match spec.product {
                qx_core::TradingProduct::Spot if order.side == qx_core::Side::Buy => true,
                qx_core::TradingProduct::Spot => false,
                qx_core::TradingProduct::Margin
                | qx_core::TradingProduct::Perpetual
                | qx_core::TradingProduct::Future => !policy.reduce_only,
                qx_core::TradingProduct::Option => {
                    return Err(format!(
                        "FAIL_CLOSED: Paper 在途期权订单 {} 尚无可认证保证金估值",
                        order.client_id
                    ));
                }
            };
            if !reserve_amount {
                return Ok(total);
            }
            let mark = marks.get(&order.instrument).copied();
            let price = match (order.limit, mark) {
                (Some(limit), Some(mark)) => limit.max(mark),
                (Some(limit), None) => limit,
                (None, Some(mark)) => mark,
                (None, None) => {
                    return Err(format!(
                        "FAIL_CLOSED: Paper 在途市价订单 {} 缺少保证金参考价",
                        order.client_id
                    ));
                }
            };
            let (amount, currency) = match spec.product {
                qx_core::TradingProduct::Spot => (
                    spec.notional(order.remaining().raw(), price.raw()).map_err(|error| {
                        format!("FAIL_CLOSED: Paper 在途现货买单 {} 名义额计算失败: {error}", order.client_id)
                    })?,
                    &spec.quote_currency,
                ),
                qx_core::TradingProduct::Margin
                | qx_core::TradingProduct::Perpetual
                | qx_core::TradingProduct::Future => (
                    spec.initial_margin(order.remaining().raw(), price.raw(), policy.leverage)
                        .map_err(|error| {
                            format!("FAIL_CLOSED: Paper 在途订单 {} 保证金计算失败: {error}", order.client_id)
                        })?,
                    &spec.settlement_currency,
                ),
                qx_core::TradingProduct::Option => {
                    return Err(format!(
                        "FAIL_CLOSED: Paper 在途期权订单 {} 尚无可认证保证金估值",
                        order.client_id
                    ));
                }
            };
            let currency = currency.trim().to_ascii_uppercase();
            let reporting_currency = reporting_currency.trim().to_ascii_uppercase();
            let funding_currency = funding_currency.trim().to_ascii_uppercase();
            let rate_to_reporting = |currency: &str| -> Result<i128, String> {
                if currency.eq_ignore_ascii_case(&reporting_currency) {
                    return Ok(qx_core::SCALE);
                }
                let rate = fx_rates
                    .get(currency)
                    .ok_or_else(|| {
                        format!(
                            "FAIL_CLOSED: Paper 在途订单缺少 {currency}->{reporting_currency} 保证金汇率"
                        )
                    })?
                    .raw();
                if rate <= 0 {
                    return Err(format!(
                        "FAIL_CLOSED: Paper 在途保证金汇率 {currency}->{reporting_currency} 必须为正"
                    ));
                }
                Ok(rate)
            };
            let converted = if currency.eq_ignore_ascii_case(&funding_currency) {
                amount
            } else {
                let source_rate = rate_to_reporting(&currency)?;
                let funding_rate = rate_to_reporting(&funding_currency)?;
                amount
                    .checked_mul(source_rate)
                    .and_then(|value| value.checked_div(funding_rate))
                    .ok_or_else(|| "Paper 在途保证金跨币种换算溢出".to_string())?
            };
            total
                .checked_add(converted)
                .ok_or_else(|| "Paper 在途订单保证金合计溢出".to_string())
        })
}

/// Resolve frozen specifications for every non-zero position in a Paper account.
/// A worker must not estimate account buying power from only its own market when
/// another enabled execution worker owns a position in the same account.
pub type PaperAccountMarketValuationInputs = (
    BTreeMap<InstrumentId, TradingInstrumentSpec>,
    BTreeMap<String, Price>,
);

pub fn paper_account_market_specs_and_fx(
    config: &RuntimeConfig,
    account_id: &str,
    pipeline: &LiveEventPipeline,
    current_spec: &TradingInstrumentSpec,
    open_orders: &[Order],
    reporting_currency: &str,
    runtime_config_path: Option<&Path>,
) -> Result<PaperAccountMarketValuationInputs, String> {
    let mut specs = BTreeMap::from([(current_spec.instrument.clone(), current_spec.clone())]);
    let mut held_instruments = pipeline
        .ledger()
        .entries()
        .iter()
        .filter(|entry| entry.account_id == account_id)
        .filter_map(|entry| entry.instrument.clone())
        .collect::<BTreeSet<_>>();
    held_instruments.extend(
        open_orders
            .iter()
            .filter(|order| {
                order.account_id == account_id
                    && order.client_id != 0
                    && !order.status.is_terminal()
                    && order.remaining().raw() > 0
            })
            .map(|order| order.instrument.clone()),
    );

    for instrument in held_instruments {
        let aggregate = pipeline.ledger().position_for(account_id, &instrument);
        let long = pipeline
            .ledger()
            .position_for_side(account_id, &instrument, qx_core::PositionSide::Long)
            .quantity
            .raw();
        let short = pipeline
            .ledger()
            .position_for_side(account_id, &instrument, qx_core::PositionSide::Short)
            .quantity
            .raw();
        let one_way = aggregate
            .quantity
            .raw()
            .checked_sub(long)
            .and_then(|quantity| quantity.checked_sub(short))
            .ok_or_else(|| format!("{instrument} 持仓模式拆分溢出"))?;
        let has_open_order = open_orders.iter().any(|order| {
            order.account_id == account_id
                && order.instrument == instrument
                && !order.status.is_terminal()
                && order.remaining().raw() > 0
        });
        if one_way == 0 && long == 0 && short == 0 && !has_open_order {
            continue;
        }
        if specs.contains_key(&instrument) {
            continue;
        }

        let instrument_label = instrument.to_string();
        let candidates = config
            .workers
            .iter()
            .filter(|candidate| {
                candidate.enabled
                    && candidate.account_id.as_deref() == Some(account_id)
                    && candidate.instrument_spec_path.is_some()
                    && (candidate.symbols.is_empty()
                        || candidate.symbols.iter().any(|symbol| {
                            symbol == &instrument_label
                                || InstrumentId::parse(symbol).as_ref() == Some(&instrument)
                        }))
            })
            .collect::<Vec<_>>();
        let mut resolved: Option<TradingInstrumentSpec> = None;
        for candidate in candidates {
            let candidate_spec = load_worker_market_spec_for_instrument(
                candidate,
                &instrument,
                runtime_config_path,
            )?
            .ok_or_else(|| {
                format!(
                    "FAIL_CLOSED: worker {} 为持仓市场 {instrument} 声明了规格路径但未能解析规格",
                    candidate.id
                )
            })?;
            match &resolved {
                None => resolved = Some(candidate_spec),
                Some(existing) if existing == &candidate_spec => {}
                Some(_) => {
                    return Err(format!(
                        "FAIL_CLOSED: Paper 账户 {account_id} 对 {instrument} 配置了冲突的冻结规格"
                    ));
                }
            }
        }
        let resolved = resolved.ok_or_else(|| {
            format!(
                "FAIL_CLOSED: Paper 账户 {account_id} 的非零持仓 {instrument} 没有匹配的启用 worker market spec"
            )
        })?;
        specs.insert(instrument, resolved);
    }

    // Specs for declared account markets also provide explicit FX marks when the
    // market itself is not currently held as a position.
    for candidate in config.workers.iter().filter(|candidate| {
        candidate.enabled
            && candidate.account_id.as_deref() == Some(account_id)
            && candidate.instrument_spec_path.is_some()
    }) {
        for symbol in &candidate.symbols {
            let Some(instrument) = InstrumentId::parse(symbol) else {
                continue;
            };
            let Some(candidate_spec) =
                load_worker_spec_if_market_matches(candidate, &instrument, runtime_config_path)?
            else {
                continue;
            };
            match specs.get(&instrument) {
                None => {
                    specs.insert(instrument, candidate_spec);
                }
                Some(existing) if existing == &candidate_spec => {}
                Some(_) => {
                    return Err(format!(
                        "FAIL_CLOSED: Paper 账户 {account_id} 对 {instrument} 配置了冲突的冻结规格"
                    ));
                }
            }
        }
    }

    let mut required_fx = pipeline
        .ledger()
        .cash_balances_for(account_id)
        .into_iter()
        .filter(|(_, amount)| *amount != 0)
        .map(|(currency, _)| currency.trim().to_ascii_uppercase())
        .collect::<BTreeSet<_>>();
    if current_spec.product == qx_core::TradingProduct::Spot {
        required_fx.insert(current_spec.quote_currency.trim().to_ascii_uppercase());
    }
    for instrument in specs.keys() {
        let aggregate = pipeline.ledger().position_for(account_id, instrument);
        let long = pipeline
            .ledger()
            .position_for_side(account_id, instrument, qx_core::PositionSide::Long)
            .quantity
            .raw();
        let short = pipeline
            .ledger()
            .position_for_side(account_id, instrument, qx_core::PositionSide::Short)
            .quantity
            .raw();
        let one_way = aggregate
            .quantity
            .raw()
            .checked_sub(long)
            .and_then(|quantity| quantity.checked_sub(short))
            .ok_or_else(|| format!("{instrument} 持仓模式拆分溢出"))?;
        if one_way == 0 && long == 0 && short == 0 {
            continue;
        }
        let spec = &specs[instrument];
        required_fx.insert(if spec.product == qx_core::TradingProduct::Spot {
            spec.quote_currency.trim().to_ascii_uppercase()
        } else {
            spec.settlement_currency.trim().to_ascii_uppercase()
        });
    }
    for order in open_orders.iter().filter(|order| {
        order.account_id == account_id && !order.status.is_terminal() && order.remaining().raw() > 0
    }) {
        let spec = specs.get(&order.instrument).ok_or_else(|| {
            format!(
                "FAIL_CLOSED: Paper 在途订单 {} 缺少冻结 market spec",
                order.client_id
            )
        })?;
        let reserve_currency = match spec.product {
            qx_core::TradingProduct::Spot if order.side == qx_core::Side::Buy => {
                Some(&spec.quote_currency)
            }
            qx_core::TradingProduct::Margin
            | qx_core::TradingProduct::Perpetual
            | qx_core::TradingProduct::Future => Some(&spec.settlement_currency),
            qx_core::TradingProduct::Spot | qx_core::TradingProduct::Option => None,
        };
        if let Some(currency) = reserve_currency {
            required_fx.insert(currency.trim().to_ascii_uppercase());
        }
    }
    let reporting_currency = reporting_currency.trim().to_ascii_uppercase();
    required_fx.remove(&reporting_currency);

    let mut fx_rates = BTreeMap::new();
    for currency in required_fx {
        let mut observed_rates = BTreeSet::new();
        for spec in specs.values().filter(|spec| {
            spec.product == qx_core::TradingProduct::Spot
                && ((spec.base_currency.eq_ignore_ascii_case(&currency)
                    && spec
                        .quote_currency
                        .eq_ignore_ascii_case(&reporting_currency))
                    || (spec.base_currency.eq_ignore_ascii_case(&reporting_currency)
                        && spec.quote_currency.eq_ignore_ascii_case(&currency)))
        }) {
            let mark = pipeline.marks().get(&spec.instrument).ok_or_else(|| {
                format!(
                    "FAIL_CLOSED: {currency}->{reporting_currency} FX 市场 {} 缺少最新标记价",
                    spec.instrument
                )
            })?;
            if mark.raw() <= 0 {
                return Err(format!(
                    "FAIL_CLOSED: FX 市场 {} 标记价必须为正",
                    spec.instrument
                ));
            }
            let rate = if spec.base_currency.eq_ignore_ascii_case(&currency) {
                mark.raw()
            } else {
                qx_core::SCALE
                    .checked_mul(qx_core::SCALE)
                    .and_then(|value| value.checked_div(mark.raw()))
                    .ok_or_else(|| "FX 反向汇率计算溢出".to_string())?
            };
            observed_rates.insert(rate);
        }
        match observed_rates.len() {
            0 => {
                return Err(format!(
                    "FAIL_CLOSED: Paper 账户缺少 {currency}->{reporting_currency} 直接或反向 Spot FX 市场与标记价"
                ));
            }
            1 => {
                let Some(rate) = observed_rates.first().copied() else {
                    return Err(format!(
                        "FAIL_CLOSED: {currency}->{reporting_currency} FX 汇率集合为空"
                    ));
                };
                fx_rates.insert(currency, Price::from_raw(rate));
            }
            _ => {
                return Err(format!(
                    "FAIL_CLOSED: Paper 账户存在多个不一致的 {currency}->{reporting_currency} FX 报价"
                ));
            }
        }
    }
    Ok((specs, fx_rates))
}

pub fn load_worker_spec_if_market_matches(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return Ok(None);
    };
    let resolved_path = runtime_config_path
        .map(|path| qx_runtime::resolve_runtime_relative_path(path, spec_path))
        .unwrap_or_else(|| PathBuf::from(spec_path));
    let payload = std::fs::read_to_string(&resolved_path).map_err(|error| {
        format!(
            "读取 worker market spec 失败 {}: {error}",
            resolved_path.display()
        )
    })?;
    let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        format!(
            "worker market spec JSON 无效 {}: {error}",
            resolved_path.display()
        )
    })?;
    if market.get("base_currency").is_some() {
        let declared_instrument = market.get("instrument").and_then(|value| {
            value
                .as_str()
                .and_then(InstrumentId::parse)
                .or_else(|| serde_json::from_value::<InstrumentId>(value.clone()).ok())
        });
        let Some(declared_instrument) = declared_instrument else {
            return Err(format!(
                "FAIL_CLOSED: worker {} 的 market spec 缺少可解析的 instrument，无法验证 {instrument} 对应的规格",
                worker.id
            ));
        };
        if &declared_instrument != instrument {
            return Ok(None);
        }
    } else if let Some(symbol) = market.get("symbol").and_then(serde_json::Value::as_str) {
        if symbol != instrument.symbol {
            return Ok(None);
        }
    } else if worker.symbols.len() != 1 {
        return Err(format!(
            "FAIL_CLOSED: worker {} 的 CCXT market spec 没有 symbol 字段且绑定了多个标的，无法证明 {instrument} 对应的规格",
            worker.id
        ));
    }
    crate::worker_market_spec::load_worker_market_spec_for_instrument(
        worker,
        instrument,
        runtime_config_path,
    )
}
pub fn live_order_market_spec(
    config: &RuntimeConfig,
    account_id: &str,
    venue_id: &str,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<TradingInstrumentSpec, String> {
    let instrument_label = instrument.to_string();
    let candidates = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker.account_id.as_deref() == Some(account_id)
                && worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(venue_id))
                && worker.instrument_spec_path.is_some()
                && (worker.symbols.is_empty()
                    || worker.symbols.iter().any(|symbol| {
                        symbol == &instrument_label
                            || InstrumentId::parse(symbol).as_ref() == Some(instrument)
                    }))
        })
        .collect::<Vec<_>>();
    let mut resolved: Option<TradingInstrumentSpec> = None;
    for worker in candidates {
        let Some(spec) =
            load_worker_spec_if_market_matches(worker, instrument, runtime_config_path)?
        else {
            continue;
        };
        match &resolved {
            None => resolved = Some(spec),
            Some(existing) if existing == &spec => {}
            Some(_) => {
                return Err(format!(
                    "FAIL_CLOSED: Live 账户 {account_id} 对 {venue_id} 市场 {instrument} 配置了冲突的冻结规格"
                ));
            }
        }
    }
    resolved.ok_or_else(|| {
        format!(
            "FAIL_CLOSED: Live 账户 {account_id} 在 {venue_id} 没有为在途市场 {instrument} 配置冻结 market spec"
        )
    })
}
fn load_runtime_config_for_risk(path: &Path) -> Result<RuntimeConfig, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取运行时配置失败 {}: {error}", path.display()))?;
    RuntimeConfig::from_json(&payload)
}

pub fn worker_risk_context(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config: Option<&RuntimeConfig>,
    runtime_config_path: Option<&Path>,
) -> Result<Option<(RiskContext, OrderRiskPosition)>, String> {
    let Some(spec) = crate::worker_market_spec::load_worker_market_spec_for_instrument(
        worker,
        &order.instrument,
        runtime_config_path,
    )?
    else {
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
    if spec.product != qx_core::TradingProduct::Spot
        && !spec.settlement_currency.eq_ignore_ascii_case(&settlement)
    {
        return Err(format!(
            "FAIL_CLOSED: worker {} 的 market spec 结算币种 {} 与账户账簿币种 {settlement} 不一致；\
             可用保证金只能按账簿币种计，请统一 worker.settlement_currency 与规格文件后重启",
            worker.id, spec.settlement_currency
        ));
    }
    let funding_currency =
        if spec.product == qx_core::TradingProduct::Spot && order.side == qx_core::Side::Buy {
            spec.quote_currency.trim().to_ascii_uppercase()
        } else {
            settlement.clone()
        };
    let observed_available = worker.venue_id.as_deref().and_then(|venue_id| {
        pipeline
            .snapshot()
            .account_balances
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|balances| {
                balances
                    .iter()
                    .find(|balance| balance.asset.eq_ignore_ascii_case(&funding_currency))
            })
            .map(|balance| balance.free.raw().saturating_sub(balance.borrowed.raw()))
    });
    let open_orders = pipeline.orders();
    let available_margin_raw = if let Some(available) = observed_available {
        let pending_reserve =
            if VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper) {
                0
            } else {
                live_post_snapshot_order_reserve_raw(
                    &open_orders,
                    LiveOrderReserveInputs {
                        pipeline,
                        candidate_order_id: order.client_id,
                        account_id,
                        venue_id: worker.venue_id.as_deref().unwrap_or_default(),
                        funding_currency: &funding_currency,
                        current_spec: &spec,
                        runtime_config,
                        runtime_config_path,
                    },
                )?
            };
        available
            .checked_sub(pending_reserve)
            .ok_or_else(|| "Live 在途订单资金预留溢出".to_string())?
    } else if VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper) {
        let (market_specs, fx_rates) = match runtime_config {
            Some(config) => paper_account_market_specs_and_fx(
                config,
                account_id,
                pipeline,
                &spec,
                &open_orders,
                &settlement,
                runtime_config_path,
            )?,
            None => (
                BTreeMap::from([(spec.instrument.clone(), spec.clone())]),
                BTreeMap::new(),
            ),
        };
        let marked_margin = qx_app::paper_available_margin_raw(
            pipeline.ledger(),
            account_id,
            pipeline.marks(),
            &settlement,
            &market_specs,
            &fx_rates,
            &qx_app::RunContext::new(
                qx_app::CallerCapability::Paper,
                format!("paper-risk:{}", worker.id),
            ),
        )
        .map_err(|error| format!("Paper 账户多市场可用保证金计算失败: {error}"))?;
        // 现货买入只能消费报价币现金；已持有资产的市值属于权益，不代表可以重复花用的现金。
        // 衍生品则允许将同账户持仓 PnL/现金按组合估值口径计入保证金预算。
        let buying_power = if spec.product == qx_core::TradingProduct::Spot {
            pipeline
                .ledger()
                .cash_for(account_id, &spec.quote_currency.trim().to_ascii_uppercase())
        } else {
            marked_margin
        };
        buying_power
            .checked_sub(paper_pending_order_reserve_raw(
                &open_orders,
                PaperPendingReserveInputs {
                    candidate_order_id: order.client_id,
                    account_id,
                    market_specs: &market_specs,
                    marks: pipeline.marks(),
                    fx_rates: &fx_rates,
                    reporting_currency: &settlement,
                    funding_currency: if spec.product == qx_core::TradingProduct::Spot {
                        &spec.quote_currency
                    } else {
                        &settlement
                    },
                },
            )?)
            .ok_or_else(|| "Paper 在途订单资金预留溢出".to_string())?
    } else {
        return Err(format!(
            "worker {} 尚未收到 {} 账户余额快照，拒绝执行账户级风控订单",
            worker.id, funding_currency
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
    if spec.product == qx_core::TradingProduct::Spot && order.side == qx_core::Side::Sell {
        let pending_sell_qty = open_orders
            .iter()
            .filter(|pending| {
                pending.client_id != order.client_id
                    && pending.account_id == account_id
                    && pending.instrument == order.instrument
                    && pending.side == qx_core::Side::Sell
                    && !pending.status.is_terminal()
            })
            .try_fold(0_i128, |total, pending| {
                total
                    .checked_add(pending.remaining().raw())
                    .ok_or_else(|| "Paper 在途现货卖单数量合计溢出".to_string())
            })?;
        let available_inventory = one_way_qty
            .checked_sub(pending_sell_qty)
            .ok_or_else(|| "Paper 在途现货卖单导致可用持仓溢出".to_string())?;
        if order.qty.raw() > available_inventory {
            return Err(format!(
                "FAIL_CLOSED: 现货卖单数量超过扣除在途卖单后的可用持仓: requested={}, available={available_inventory}",
                order.qty.raw()
            ));
        }
    }
    let filled_gross_notional = reference_price
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
    // An accepted order is a potential position until it is filled or reaches a
    // terminal state. Include the rest of this account's open orders in the
    // projected gross exposure so separate workers cannot each pass the same
    // max-position limit against an unchanged Ledger snapshot.
    let pending_gross_notional = if worker.max_position_notional_raw.is_some() {
        qx_risk::pending_order_gross_notional(
            &pipeline.orders(),
            order.client_id,
            account_id,
            &order.instrument,
            &spec,
            pipeline.marks(),
        )
        .map_err(|error| format!("在途订单名义额风控计算失败: {error:?}"))?
    } else {
        0
    };
    let gross_notional = filled_gross_notional
        .checked_add(pending_gross_notional)
        .ok_or_else(|| "当前持仓与在途订单名义额合计溢出".to_string())?;
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

struct LiveOrderReserveInputs<'a> {
    pipeline: &'a LiveEventPipeline,
    candidate_order_id: u64,
    account_id: &'a str,
    venue_id: &'a str,
    funding_currency: &'a str,
    current_spec: &'a TradingInstrumentSpec,
    runtime_config: Option<&'a RuntimeConfig>,
    runtime_config_path: Option<&'a Path>,
}

fn live_post_snapshot_order_reserve_raw(
    orders: &[Order],
    inputs: LiveOrderReserveInputs<'_>,
) -> Result<i128, String> {
    let LiveOrderReserveInputs {
        pipeline,
        candidate_order_id,
        account_id,
        venue_id,
        funding_currency,
        current_spec,
        runtime_config,
        runtime_config_path,
    } = inputs;
    let Some(balance_seq) = pipeline.log().events().iter().rev().find_map(|event| {
        matches!(
            &event.kind,
            qx_core::EventKind::AccountBalanceSnapshot { account_id: snapshot_account, venue_id: snapshot_venue, .. }
                if snapshot_account == account_id && snapshot_venue.eq_ignore_ascii_case(venue_id)
        )
        .then_some(event.seq)
    }) else {
        return Err(format!(
            "FAIL_CLOSED: {venue_id} 有余额读数但找不到对应账户的余额快照事实"
        ));
    };

    let mut post_snapshot_order_ids = BTreeSet::new();
    for event in pipeline
        .log()
        .events()
        .iter()
        .filter(|event| event.seq > balance_seq)
    {
        match &event.kind {
            qx_core::EventKind::OrderSubmitted { order }
                if order.account_id == account_id && order.client_id != candidate_order_id =>
            {
                post_snapshot_order_ids.insert(order.client_id);
            }
            qx_core::EventKind::AccountCashflow { cashflow }
                if cashflow.account_id == account_id
                    && cashflow.venue_id.eq_ignore_ascii_case(venue_id) =>
            {
                return Err(format!(
                    "FAIL_CLOSED: 账户余额快照后发生现金流 {}，需先刷新交易所余额再继续下单",
                    cashflow.external_id
                ));
            }
            qx_core::EventKind::Filled { fill }
                if fill.account_id == account_id && fill.order_id != candidate_order_id =>
            {
                return Err(format!(
                    "FAIL_CLOSED: 账户余额快照后订单 {} 已成交，需先刷新交易所余额再继续下单",
                    fill.order_id
                ));
            }
            _ => {}
        }
    }

    let needs_other_market_spec = orders.iter().any(|order| {
        post_snapshot_order_ids.contains(&order.client_id)
            && order.client_id != candidate_order_id
            && order.account_id == account_id
            && order.instrument != current_spec.instrument
            && !order.status.is_terminal()
            && order.remaining().raw() > 0
    });
    let runtime_config_from_path = if needs_other_market_spec && runtime_config.is_none() {
        let path = runtime_config_path.ok_or_else(|| {
            "FAIL_CLOSED: Live 多市场在途订单需要运行时配置以解析冻结 market spec".to_string()
        })?;
        Some(load_runtime_config_for_risk(path)?)
    } else {
        None
    };
    let runtime_config = runtime_config.or(runtime_config_from_path.as_ref());

    orders
        .iter()
        .filter(|order| {
            post_snapshot_order_ids.contains(&order.client_id)
                && order.client_id != candidate_order_id
                && order.account_id == account_id
                && !order.status.is_terminal()
                && order.remaining().raw() > 0
        })
        .try_fold(0_i128, |total, order| {
            let order_spec = if order.instrument == current_spec.instrument {
                current_spec.clone()
            } else {
                let config = runtime_config.ok_or_else(|| {
                    format!(
                        "FAIL_CLOSED: Live 余额快照后存在其他市场 {} 的在途订单 {}，缺少账户运行时配置",
                        order.instrument, order.client_id
                    )
                })?;
                live_order_market_spec(
                    config,
                    account_id,
                    venue_id,
                    &order.instrument,
                    runtime_config_path,
                )?
            };
            let policy = order.policy.unwrap_or_default();
            policy.validate_for(&order_spec).map_err(|error| {
                format!("FAIL_CLOSED: 余额快照后订单 {} policy 无效: {error}", order.client_id)
            })?;
            let (should_reserve, currency) = match order_spec.product {
                qx_core::TradingProduct::Spot if order.side == qx_core::Side::Buy => {
                    (true, &order_spec.quote_currency)
                }
                qx_core::TradingProduct::Spot => (false, &order_spec.quote_currency),
                qx_core::TradingProduct::Margin
                | qx_core::TradingProduct::Perpetual
                | qx_core::TradingProduct::Future => {
                    (!policy.reduce_only, &order_spec.settlement_currency)
                }
                qx_core::TradingProduct::Option => {
                    return Err(format!(
                        "FAIL_CLOSED: Live 在途期权订单 {} 没有可认证的保证金预留模型",
                        order.client_id
                    ));
                }
            };
            if !should_reserve {
                return Ok(total);
            }
            if !currency.eq_ignore_ascii_case(funding_currency)
                && current_spec.product == qx_core::TradingProduct::Spot
                && order_spec.product == qx_core::TradingProduct::Spot
            {
                // Separate spot quote-asset balances are independent cash buckets.
                // A pending EUR spot buy cannot consume the candidate's USDT balance.
                return Ok(total);
            }
            let price = match (order.limit, pipeline.marks().get(&order.instrument).copied()) {
                (Some(limit), Some(mark)) => limit.max(mark),
                (Some(limit), None) => limit,
                (None, Some(mark)) => mark,
                (None, None) => {
                    return Err(format!(
                        "FAIL_CLOSED: Live 在途市价订单 {} 缺少资金预留参考价",
                        order.client_id
                    ));
                }
            };
            let amount = match order_spec.product {
                qx_core::TradingProduct::Spot => order_spec
                    .notional(order.remaining().raw(), price.raw())
                    .map_err(|error| format!("Live 在途现货买单名义额计算失败: {error}"))?,
                qx_core::TradingProduct::Margin
                | qx_core::TradingProduct::Perpetual
                | qx_core::TradingProduct::Future => order_spec
                    .initial_margin(order.remaining().raw(), price.raw(), policy.leverage)
                    .map_err(|error| format!("Live 在途订单保证金计算失败: {error}"))?,
                qx_core::TradingProduct::Option => {
                    return Err(format!(
                        "FAIL_CLOSED: Live 在途期权订单 {} 没有可认证的保证金预留模型",
                        order.client_id
                    ));
                }
            };
            if !currency.eq_ignore_ascii_case(funding_currency) {
                return Err(format!(
                    "FAIL_CLOSED: Live 在途订单资金币种 {currency} 与候选订单资金币种 {funding_currency} 不一致",
                ));
            }
            total
                .checked_add(amount)
                .ok_or_else(|| "Live 在途订单资金预留合计溢出".to_string())
        })
}
