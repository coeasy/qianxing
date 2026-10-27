//! Shared construction of the strategy risk gate from runtime configuration.

use crate::worker_market_spec::load_worker_market_spec_for_instrument;
use qx_core::{
    AccountBalance, DecimalRounding, ExactDecimal, InstrumentId, MarginMode, Order, PositionSide,
    Price, TradingInstrumentSpec, TradingProduct, VenueFamily, SCALE,
};
use qx_risk::{
    ExactOrderRiskFacts, ExactOrderRiskPosition, MaxNotionalRule, MaxQtyRule, NoShortRule,
    OrderRiskPosition, RuleSet,
};
use qx_runtime::{
    required_account_event_log, LiveEventPipeline, RuntimeConfig, WorkerConfig, WorkerRole,
};
use qx_zhenlu::{RiskContext, RiskGate};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

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
    execute_submit_order_with_worker_risk_and_market_reference(
        command,
        worker,
        root,
        venue,
        pipeline,
        now,
        source_seq,
        runtime_config_path,
        spread_store,
        None,
    )
}

/// Market orders with exact strategy quantities capture their fresh exact risk price in the
/// durable order fact. That price is risk metadata only and is not passed to the venue as a
/// limit price.
#[allow(clippy::too_many_arguments)]
pub fn execute_submit_order_with_worker_risk_and_market_reference<V: qx_zhenlu::Venue>(
    command: &qx_control::ControlCommand,
    worker: &WorkerConfig,
    root: &Path,
    venue: &mut V,
    pipeline: &mut LiveEventPipeline,
    now: u64,
    source_seq: &mut u64,
    runtime_config_path: Option<&Path>,
    spread_store: Option<&dyn qx_zhenlu::SpreadOrderGroupStore>,
    market_reference_price: Option<ExactDecimal>,
) -> Result<String, String> {
    let mut order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let mut enriched_command = command.clone();
    if let Some(reference_price) = market_reference_price {
        if !reference_price.is_positive() {
            return Err("FAIL_CLOSED: 市价单精确风险参考价必须为正".into());
        }
        let Some(mut amounts) = order.exact_amounts else {
            return Err("FAIL_CLOSED: legacy 市价单不能携带精确小数风险参考价".into());
        };
        if amounts.limit_price.is_some() {
            return Err("FAIL_CLOSED: 精确限价单不接受市价风险参考价".into());
        }
        amounts.market_reference_price = Some(reference_price);
        order.exact_amounts = Some(amounts);
        order
            .validate()
            .map_err(|error| format!("精确市价订单无效: {error}"))?;
        let order_json = serde_json::to_string(&order)
            .map_err(|error| format!("编码精确市价订单失败: {error}"))?;
        enriched_command
            .payload
            .insert("order_json".into(), order_json);
    }
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
    if let Some(spec) = risk.instrument_spec.as_ref() {
        venue.validate_instrument_spec(spec).map_err(|error| {
            format!("FAIL_CLOSED: Live 市场规格与交易所当前规格校验失败: {error:?}")
        })?;
        venue
            .validate_order_constraints(spec, &order)
            .map_err(|error| format!("FAIL_CLOSED: Live 订单不满足交易所当前过滤器: {error:?}"))?;
    }
    let context = qx_execution::RiskExecutionContext {
        risk: &risk,
        position: &position,
    };
    qx_execution::submit_order_with_risk(qx_execution::SubmitOrderWithRiskRequest {
        command: &enriched_command,
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
    for order in orders.iter().filter(|order| {
        order.client_id != candidate_order_id
            && order.account_id == account_id
            && !order.status.is_terminal()
    }) {
        order.require_legacy_amounts().map_err(|error| {
            format!(
                "FAIL_CLOSED: Paper 在途订单 {} 金额精度不受当前预留器支持: {error}",
                order.client_id
            )
        })?;
    }
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

/// Resolve each requested asset currency to the reporting currency using configured spot marks.
/// The shortest available conversion path is used; equally short paths must agree exactly or
/// valuation fails closed instead of selecting an arbitrary exchange quote.
pub fn spot_fx_rates_to_reporting(
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<InstrumentId, Price>,
    currencies: impl IntoIterator<Item = String>,
    reporting_currency: &str,
) -> Result<BTreeMap<String, Price>, String> {
    let reporting = reporting_currency.trim().to_ascii_uppercase();
    if reporting.is_empty() {
        return Err("FAIL_CLOSED: FX 报告币种不能为空".into());
    }

    // An edge rate is a SCALE-valued amount of `to` received for one unit of `from`.
    let mut edges = BTreeMap::<String, Vec<(String, i128, String)>>::new();
    for (instrument, spec) in specs {
        if spec.product != TradingProduct::Spot {
            continue;
        }
        let base = spec.base_currency.trim().to_ascii_uppercase();
        let quote = spec.quote_currency.trim().to_ascii_uppercase();
        if base.is_empty() || quote.is_empty() || base == quote {
            continue;
        }
        let Some(mark) = marks.get(instrument) else {
            continue;
        };
        if mark.raw() <= 0 {
            // An invalid mark on an unrelated market must not block a valid conversion path.
            // If this market is required, the missing route below still fails closed.
            continue;
        }
        let reverse = SCALE
            .checked_mul(SCALE)
            .and_then(|value| value.checked_div(mark.raw()))
            .filter(|value| *value > 0);
        let label = instrument.to_string();
        edges
            .entry(base.clone())
            .or_default()
            .push((quote.clone(), mark.raw(), label.clone()));
        if let Some(reverse) = reverse {
            edges.entry(quote).or_default().push((base, reverse, label));
        }
    }
    for neighbors in edges.values_mut() {
        neighbors
            .sort_by(|left, right| (&left.0, &left.2, left.1).cmp(&(&right.0, &right.2, right.1)));
    }

    let mut reverse_edges = BTreeMap::<String, Vec<(String, i128, String)>>::new();
    for (from, neighbors) in &edges {
        for (to, rate, instrument) in neighbors {
            reverse_edges.entry(to.clone()).or_default().push((
                from.clone(),
                *rate,
                instrument.clone(),
            ));
        }
    }

    let mut resolved = BTreeMap::new();
    for currency in currencies {
        let source = currency.trim().to_ascii_uppercase();
        if source.is_empty() {
            return Err("FAIL_CLOSED: FX 资产币种不能为空".into());
        }
        if source == reporting {
            continue;
        }

        let distance_from_source = fx_currency_distances(&edges, &source);
        let Some(shortest_distance) = distance_from_source.get(&reporting).copied() else {
            return Err(format!(
                "FAIL_CLOSED: 缺少 {source}->{reporting} 可用 Spot FX 路径与标记价"
            ));
        };
        let distance_to_reporting = fx_currency_distances(&reverse_edges, &reporting);

        // Only currencies that lie on a shortest source-to-reporting path participate in
        // rate consistency checks. Conflicting prices in an unrelated branch do not poison
        // the requested conversion.
        let mut path_nodes = distance_from_source
            .iter()
            .filter_map(|(currency, distance)| {
                let remaining = distance_to_reporting.get(currency)?;
                (distance.checked_add(*remaining) == Some(shortest_distance))
                    .then_some((currency.clone(), *distance))
            })
            .collect::<Vec<_>>();
        path_nodes.sort_by(|left, right| (left.1, &left.0).cmp(&(right.1, &right.0)));

        let mut rate_to_currency = BTreeMap::from([(source.clone(), SCALE)]);
        for (current, current_distance) in path_nodes {
            if current_distance >= shortest_distance {
                continue;
            }
            let current_rate = rate_to_currency
                .get(&current)
                .copied()
                .ok_or_else(|| "FAIL_CLOSED: FX 最短路径状态缺少前序汇率".to_string())?;
            for (next, edge_rate, instrument) in edges.get(&current).into_iter().flatten() {
                let next_distance = current_distance + 1;
                if distance_from_source.get(next) != Some(&next_distance)
                    || distance_to_reporting.get(next).is_none_or(|remaining| {
                        next_distance.checked_add(*remaining) != Some(shortest_distance)
                    })
                {
                    continue;
                }
                let next_rate = checked_fx_rate_mul(current_rate, *edge_rate)
                    .filter(|value| *value > 0)
                    .ok_or_else(|| {
                        format!(
                            "FAIL_CLOSED: FX 路径 {source}->{next} 经 {instrument} 换算溢出或精度不足"
                        )
                    })?;
                match rate_to_currency.get(next).copied() {
                    None => {
                        rate_to_currency.insert(next.clone(), next_rate);
                    }
                    Some(existing_rate) if existing_rate != next_rate => {
                        return Err(format!(
                            "FAIL_CLOSED: {source}->{next} 存在等长但不一致的 FX 路径（{} 与 {} raw）",
                            existing_rate, next_rate
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
        let rate = rate_to_currency.get(&reporting).copied().ok_or_else(|| {
            format!("FAIL_CLOSED: 缺少 {source}->{reporting} 可用 Spot FX 路径与标记价")
        })?;
        resolved.insert(source, Price::from_raw(rate));
    }
    Ok(resolved)
}

/// Scale-aware counterpart of [`spot_fx_rates_to_reporting`] for read-only portfolio
/// valuation. It preserves venue prices below the kernel `Price` scale and fails closed if
/// an inverse, multi-hop product, or equal-length route cannot be represented exactly enough.
pub fn spot_fx_exact_rates_to_reporting(
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<InstrumentId, ExactDecimal>,
    currencies: impl IntoIterator<Item = String>,
    reporting_currency: &str,
) -> Result<BTreeMap<String, ExactDecimal>, String> {
    let reporting = reporting_currency.trim().to_ascii_uppercase();
    if reporting.is_empty() {
        return Err("FAIL_CLOSED: FX 报告币种不能为空".into());
    }
    let one = ExactDecimal::ONE;
    let mut edges = BTreeMap::<String, Vec<(String, ExactDecimal, String)>>::new();
    for (instrument, spec) in specs {
        if spec.product != TradingProduct::Spot {
            continue;
        }
        let base = spec.base_currency.trim().to_ascii_uppercase();
        let quote = spec.quote_currency.trim().to_ascii_uppercase();
        if base.is_empty() || quote.is_empty() || base == quote {
            continue;
        }
        let Some(mark) = marks
            .get(instrument)
            .copied()
            .filter(|mark| mark.is_positive())
        else {
            continue;
        };
        let reverse = one
            .checked_div_at_scale(mark, ExactDecimal::MAX_SCALE, DecimalRounding::TowardZero)
            .filter(|rate| rate.is_positive());
        let label = instrument.to_string();
        edges
            .entry(base.clone())
            .or_default()
            .push((quote.clone(), mark, label.clone()));
        if let Some(reverse) = reverse {
            edges.entry(quote).or_default().push((base, reverse, label));
        }
    }
    for neighbors in edges.values_mut() {
        neighbors.sort_by(|left, right| (&left.0, &left.2).cmp(&(&right.0, &right.2)));
    }

    let mut reverse_edges = BTreeMap::<String, Vec<(String, ExactDecimal, String)>>::new();
    for (from, neighbors) in &edges {
        for (to, rate, instrument) in neighbors {
            reverse_edges.entry(to.clone()).or_default().push((
                from.clone(),
                *rate,
                instrument.clone(),
            ));
        }
    }
    for neighbors in reverse_edges.values_mut() {
        neighbors.sort_by(|left, right| (&left.0, &left.2).cmp(&(&right.0, &right.2)));
    }

    let mut resolved = BTreeMap::new();
    for currency in currencies {
        let source = currency.trim().to_ascii_uppercase();
        if source.is_empty() {
            return Err("FAIL_CLOSED: FX 资产币种不能为空".into());
        }
        if source == reporting {
            continue;
        }
        let distance_from_source = fx_exact_currency_distances(&edges, &source);
        let Some(shortest_distance) = distance_from_source.get(&reporting).copied() else {
            return Err(format!(
                "FAIL_CLOSED: 缺少 {source}->{reporting} 可用 Spot FX 路径与精确标记价"
            ));
        };
        let distance_to_reporting = fx_exact_currency_distances(&reverse_edges, &reporting);
        let mut path_nodes = distance_from_source
            .iter()
            .filter_map(|(currency, distance)| {
                let remaining = distance_to_reporting.get(currency)?;
                (distance.checked_add(*remaining) == Some(shortest_distance))
                    .then_some((currency.clone(), *distance))
            })
            .collect::<Vec<_>>();
        path_nodes.sort_by(|left, right| (left.1, &left.0).cmp(&(right.1, &right.0)));

        let mut rate_to_currency = BTreeMap::from([(source.clone(), one)]);
        for (current, current_distance) in path_nodes {
            if current_distance >= shortest_distance {
                continue;
            }
            let current_rate = rate_to_currency
                .get(&current)
                .copied()
                .ok_or_else(|| "FAIL_CLOSED: FX 最短路径状态缺少前序精确汇率".to_string())?;
            for (next, edge_rate, instrument) in edges.get(&current).into_iter().flatten() {
                let next_distance = current_distance + 1;
                if distance_from_source.get(next) != Some(&next_distance)
                    || distance_to_reporting.get(next).is_none_or(|remaining| {
                        next_distance.checked_add(*remaining) != Some(shortest_distance)
                    })
                {
                    continue;
                }
                let next_rate = current_rate.checked_mul(*edge_rate)
                    .filter(|rate| rate.is_positive())
                    .ok_or_else(|| format!(
                        "FAIL_CLOSED: FX 精确路径 {source}->{next} 经 {instrument} 换算溢出或精度不足"
                    ))?;
                match rate_to_currency.get(next).copied() {
                    None => {
                        rate_to_currency.insert(next.clone(), next_rate);
                    }
                    Some(existing) if existing != next_rate => {
                        return Err(format!(
                            "FAIL_CLOSED: {source}->{next} 存在等长但不一致的精确 FX 路径（{} 与 {}）",
                            existing, next_rate
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
        let rate = rate_to_currency.get(&reporting).copied().ok_or_else(|| {
            format!("FAIL_CLOSED: 缺少 {source}->{reporting} 可用 Spot FX 精确路径")
        })?;
        resolved.insert(source, rate);
    }
    Ok(resolved)
}

fn fx_exact_currency_distances(
    edges: &BTreeMap<String, Vec<(String, ExactDecimal, String)>>,
    source: &str,
) -> BTreeMap<String, usize> {
    let mut distances = BTreeMap::from([(source.to_string(), 0_usize)]);
    let mut queue = VecDeque::from([source.to_string()]);
    while let Some(current) = queue.pop_front() {
        let Some(current_distance) = distances.get(&current).copied() else {
            continue;
        };
        for (next, _, _) in edges.get(&current).into_iter().flatten() {
            if distances.contains_key(next) {
                continue;
            }
            distances.insert(next.clone(), current_distance + 1);
            queue.push_back(next.clone());
        }
    }
    distances
}

fn fx_currency_distances(
    edges: &BTreeMap<String, Vec<(String, i128, String)>>,
    source: &str,
) -> BTreeMap<String, usize> {
    let mut distances = BTreeMap::from([(source.to_string(), 0_usize)]);
    let mut queue = VecDeque::from([source.to_string()]);
    while let Some(current) = queue.pop_front() {
        let Some(current_distance) = distances.get(&current).copied() else {
            continue;
        };
        for (next, _, _) in edges.get(&current).into_iter().flatten() {
            if distances.contains_key(next) {
                continue;
            }
            distances.insert(next.clone(), current_distance + 1);
            queue.push_back(next.clone());
        }
    }
    distances
}

fn checked_fx_rate_mul(left: i128, right: i128) -> Option<i128> {
    if left < 0 || right < 0 {
        return None;
    }
    let scale = SCALE;
    let left_whole = left / scale;
    let left_fraction = left % scale;
    let right_whole = right / scale;
    let right_fraction = right % scale;
    let whole = left_whole.checked_mul(right_whole)?.checked_mul(scale)?;
    let mixed = left_whole
        .checked_mul(right_fraction)?
        .checked_add(right_whole.checked_mul(left_fraction)?)?;
    let fraction = left_fraction
        .checked_mul(right_fraction)?
        .checked_div(scale)?;
    whole.checked_add(mixed)?.checked_add(fraction)
}

pub fn paper_account_market_specs_and_fx(
    config: &RuntimeConfig,
    account_id: &str,
    pipeline: &LiveEventPipeline,
    current_spec: &TradingInstrumentSpec,
    open_orders: &[Order],
    reporting_currency: &str,
    runtime_config_path: Option<&Path>,
) -> Result<PaperAccountMarketValuationInputs, String> {
    for order in open_orders
        .iter()
        .filter(|order| order.account_id == account_id && !order.status.is_terminal())
    {
        order.require_legacy_amounts().map_err(|error| {
            format!(
                "FAIL_CLOSED: Paper 在途订单 {} 金额精度不受当前估值器支持: {error}",
                order.client_id
            )
        })?;
    }
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

    let fx_rates =
        spot_fx_rates_to_reporting(&specs, pipeline.marks(), required_fx, &reporting_currency)?;
    Ok((specs, fx_rates))
}

pub fn load_worker_spec_if_market_matches(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    if worker.instrument_spec_path.is_none() {
        return Ok(None);
    }
    if !crate::worker_market_spec::worker_market_spec_matches(
        worker,
        instrument,
        runtime_config_path,
    )? {
        return Ok(None);
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
    let is_paper =
        VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper);
    if is_paper && order.exact_amounts.is_some() {
        return Err("FAIL_CLOSED: 精确小数订单尚未接入 Paper 撮合与精确购买力预留".into());
    }
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
    let funding_currency = match (spec.product, order.side, is_paper) {
        (qx_core::TradingProduct::Spot, qx_core::Side::Buy, _) => {
            spec.quote_currency.trim().to_ascii_uppercase()
        }
        (qx_core::TradingProduct::Spot, qx_core::Side::Sell, false) => {
            spec.base_currency.trim().to_ascii_uppercase()
        }
        _ => settlement.clone(),
    };
    let pipeline_snapshot = pipeline.snapshot();
    let observed_balance = worker.venue_id.as_deref().and_then(|venue_id| {
        pipeline_snapshot
            .account_balances
            .get(&(account_id.to_string(), venue_id.to_string()))
            .and_then(|balances| {
                balances
                    .iter()
                    .find(|balance| balance.asset.eq_ignore_ascii_case(&funding_currency))
            })
    });
    let open_orders = pipeline.orders();
    let (available_margin_raw, live_pending_reserve_exact) = if let Some(balance) = observed_balance
    {
        let pending_reserve = if is_paper {
            ExactDecimal::ZERO
        } else {
            live_post_snapshot_order_reserve_exact(
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
        let available = account_balance_available_exact(balance)?
            .checked_sub(pending_reserve)
            .ok_or_else(|| "Live 精确在途订单资金预留溢出".to_string())?;
        (exact_to_raw_floor(available)?, pending_reserve)
    } else if is_paper {
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
        (
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
                .ok_or_else(|| "Paper 在途订单资金预留溢出".to_string())?,
            ExactDecimal::ZERO,
        )
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
        let one_way_exact = pipeline
            .ledger()
            .position_for_side_exact(account_id, &order.instrument, PositionSide::Net)
            .quantity;
        let all_pending_sells = pending_spot_sell_quantity_exact(
            &open_orders,
            order.client_id,
            account_id,
            &order.instrument,
            None,
        )?;
        let (exchange_available, post_snapshot_sells) = if is_paper {
            (None, ExactDecimal::ZERO)
        } else {
            let venue_id = worker.venue_id.as_deref().unwrap_or_default();
            let post_snapshot_order_ids =
                live_post_snapshot_order_ids(pipeline, order.client_id, account_id, venue_id)?;
            let post_snapshot_sells = pending_spot_sell_quantity_exact(
                &open_orders,
                order.client_id,
                account_id,
                &order.instrument,
                Some(&post_snapshot_order_ids),
            )?;
            let exchange_available =
                account_balance_available_exact(observed_balance.ok_or_else(|| {
                    "FAIL_CLOSED: 精确现货卖单缺少交易所可用余额快照".to_string()
                })?)?;
            (Some(exchange_available), post_snapshot_sells)
        };
        let available_inventory = exact_spot_sell_available_inventory(
            one_way_exact,
            all_pending_sells,
            exchange_available,
            post_snapshot_sells,
        )?;
        if order
            .quantity_decimal()
            .checked_cmp(available_inventory)
            .is_none_or(|ordering| ordering.is_gt())
        {
            return Err(format!(
                "FAIL_CLOSED: 现货卖单数量超过账簿持仓和交易所 base 可用余额扣除在途卖单后的可卖量: requested={}, available={}",
                order.quantity_decimal(), available_inventory
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
    let pending_gross_exact = if worker.max_position_notional_raw.is_some() {
        qx_risk::pending_order_gross_notional_exact(
            &pipeline.orders(),
            order.client_id,
            account_id,
            &order.instrument,
            &spec,
            pipeline.marks(),
        )
        .map_err(|error| format!("在途订单名义额风控计算失败: {error:?}"))?
    } else {
        ExactDecimal::ZERO
    };
    let pending_gross_notional = exact_to_raw_ceiling(pending_gross_exact)?;
    let gross_notional = filled_gross_notional
        .checked_add(pending_gross_notional)
        .ok_or_else(|| "当前持仓与在途订单名义额合计溢出".to_string())?;
    let position =
        OrderRiskPosition::new_with_multiplier(one_way_qty, gross_notional, spec.contract_size)
            .with_hedge_legs(long_qty, short_qty);
    let exact = if !is_paper {
        observed_balance
            .map(|balance| {
                let ledger = pipeline.ledger();
                let one_way_exact = ledger
                    .position_for_side_exact(account_id, &order.instrument, PositionSide::Net)
                    .quantity;
                let long_exact = ledger
                    .position_for_side_exact(account_id, &order.instrument, PositionSide::Long)
                    .quantity;
                let short_exact = ledger
                    .position_for_side_exact(account_id, &order.instrument, PositionSide::Short)
                    .quantity;
                let exact_reference_price = order
                    .market_reference_price_decimal()
                    .or_else(|| order.limit_price_decimal())
                    .or_else(|| {
                        reference_price
                            .and_then(|price| ExactDecimal::from_parts(price.raw(), 9).ok())
                    });
                exact_live_order_risk_facts(
                    &spec,
                    balance,
                    live_pending_reserve_exact,
                    available_margin_raw,
                    &position,
                    one_way_exact,
                    long_exact,
                    short_exact,
                    exact_reference_price,
                    pending_gross_exact,
                    worker.max_order_notional_raw,
                    worker.max_position_notional_raw,
                )
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    Ok(Some((
        RiskContext {
            available_margin_raw: Some(available_margin_raw),
            reference_price,
            instrument_spec: Some(spec),
            max_order_notional_raw: worker.max_order_notional_raw,
            max_position_notional_raw: worker.max_position_notional_raw,
            exact,
        },
        position,
    )))
}

fn exact_to_raw_floor(value: ExactDecimal) -> Result<i128, String> {
    let scale = ExactDecimal::from_parts(SCALE, 0)
        .map_err(|error| format!("精确风控投影尺度非法: {error}"))?;
    value
        .checked_mul_at_scale(scale, 0, DecimalRounding::Floor)
        .map(|projected| projected.coefficient())
        .ok_or_else(|| "精确风控快照投影到 1e9 溢出".to_string())
}

fn exact_to_raw_ceiling(value: ExactDecimal) -> Result<i128, String> {
    let scale = ExactDecimal::from_parts(SCALE, 0)
        .map_err(|error| format!("精确风控投影尺度非法: {error}"))?;
    value
        .checked_mul_at_scale(scale, 0, DecimalRounding::Ceiling)
        .map(|projected| projected.coefficient())
        .ok_or_else(|| "精确风险敞口投影到 1e9 溢出".to_string())
}

fn account_balance_available_exact(balance: &AccountBalance) -> Result<ExactDecimal, String> {
    let from_raw = |raw| {
        ExactDecimal::from_parts(raw, 9).map_err(|error| format!("raw 余额快照转换失败: {error}"))
    };
    let amounts = balance.exact_amounts;
    let free = amounts
        .map(|amounts| amounts.free)
        .unwrap_or(from_raw(balance.free.raw())?);
    let borrowed = amounts
        .map(|amounts| amounts.borrowed)
        .unwrap_or(from_raw(balance.borrowed.raw())?);
    free.checked_sub(borrowed)
        .ok_or_else(|| "精确可用余额计算溢出".to_string())
}

fn pending_spot_sell_quantity_exact(
    orders: &[Order],
    candidate_order_id: u64,
    account_id: &str,
    instrument: &InstrumentId,
    selected_order_ids: Option<&BTreeSet<u64>>,
) -> Result<ExactDecimal, String> {
    orders
        .iter()
        .filter(|pending| {
            pending.client_id != candidate_order_id
                && pending.account_id == account_id
                && pending.instrument == *instrument
                && pending.side == qx_core::Side::Sell
                && !pending.status.is_terminal()
                && selected_order_ids.is_none_or(|selected| selected.contains(&pending.client_id))
        })
        .try_fold(ExactDecimal::ZERO, |total, pending| {
            let remaining = pending
                .remaining_decimal()
                .ok_or_else(|| format!("现货在途卖单 {} 精确剩余数量溢出", pending.client_id))?;
            total
                .checked_add(remaining)
                .ok_or_else(|| "在途现货卖单精确数量合计溢出".to_string())
        })
}

fn exact_spot_sell_available_inventory(
    ledger_position: ExactDecimal,
    all_pending_sells: ExactDecimal,
    exchange_available: Option<ExactDecimal>,
    post_snapshot_sells: ExactDecimal,
) -> Result<ExactDecimal, String> {
    let ledger_available = ledger_position
        .checked_sub(all_pending_sells)
        .ok_or_else(|| "账簿现货可卖量计算溢出".to_string())?;
    let Some(exchange_available) = exchange_available else {
        return Ok(ledger_available);
    };
    let exchange_available = exchange_available
        .checked_sub(post_snapshot_sells)
        .ok_or_else(|| "交易所现货可用余额扣除在途卖单后溢出".to_string())?;
    match ledger_available.checked_cmp(exchange_available) {
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal) => Ok(ledger_available),
        Some(std::cmp::Ordering::Greater) => Ok(exchange_available),
        None => Err("精确现货可卖量比较失败".into()),
    }
}

fn exact_live_order_risk_facts(
    spec: &TradingInstrumentSpec,
    balance: &AccountBalance,
    pending_reserve: ExactDecimal,
    available_margin_raw: i128,
    raw_position: &OrderRiskPosition,
    one_way_qty: ExactDecimal,
    long_qty: ExactDecimal,
    short_qty: ExactDecimal,
    reference_price: Option<ExactDecimal>,
    pending_gross_notional: ExactDecimal,
    max_order_notional_raw: Option<i128>,
    max_position_notional_raw: Option<i128>,
) -> Result<Option<ExactOrderRiskFacts>, String> {
    let from_raw = |raw| {
        ExactDecimal::from_parts(raw, 9).map_err(|error| format!("raw 风控快照转换失败: {error}"))
    };
    let exact_to_raw = |value: ExactDecimal| {
        let scale = ExactDecimal::from_parts(SCALE, 0)
            .map_err(|error| format!("精确风控投影尺度非法: {error}"))?;
        value
            .checked_mul_at_scale(scale, 0, DecimalRounding::TowardZero)
            .map(|projected| projected.coefficient())
            .ok_or_else(|| "精确风控快照投影到 1e9 溢出".to_string())
    };
    let balance_exact = balance.exact_amounts;
    let free = balance_exact
        .map(|amounts| amounts.free)
        .unwrap_or(from_raw(balance.free.raw())?);
    let borrowed = balance_exact
        .map(|amounts| amounts.borrowed)
        .unwrap_or(from_raw(balance.borrowed.raw())?);
    let Some(available_margin) = free
        .checked_sub(borrowed)
        .and_then(|available| available.checked_sub(pending_reserve))
    else {
        return Ok(None);
    };
    if exact_to_raw(available_margin)? != available_margin_raw
        || exact_to_raw(one_way_qty)? != raw_position.net_qty
        || exact_to_raw(long_qty)? != raw_position.long_qty
        || exact_to_raw(short_qty)? != raw_position.short_qty
        || long_qty.is_negative()
        || short_qty.is_positive()
    {
        return Ok(None);
    }

    let mut gross_notional = ExactDecimal::ZERO;
    for quantity in [one_way_qty, long_qty, short_qty] {
        if quantity.is_zero() {
            continue;
        }
        let Some(price) = reference_price else {
            return Ok(None);
        };
        let absolute_quantity = if quantity.is_negative() {
            let Some(absolute) = quantity.checked_neg() else {
                return Ok(None);
            };
            absolute
        } else {
            quantity
        };
        gross_notional = gross_notional
            .checked_add(
                spec.notional_exact(absolute_quantity, price)
                    .map_err(|error| format!("精确持仓名义额计算失败: {error:?}"))?,
            )
            .ok_or_else(|| "精确持仓名义额合计溢出".to_string())?;
    }
    gross_notional = gross_notional
        .checked_add(pending_gross_notional)
        .ok_or_else(|| "精确持仓与在途名义额溢出".to_string())?;

    let max_order_notional = max_order_notional_raw.map(from_raw).transpose()?;
    let max_position_notional = max_position_notional_raw.map(from_raw).transpose()?;
    Ok(Some(ExactOrderRiskFacts {
        available_margin: Some(available_margin),
        projected_initial_margin: None,
        reference_price,
        max_order_notional,
        max_position_notional,
        position: ExactOrderRiskPosition {
            net_qty: one_way_qty,
            gross_notional,
            long_qty,
            short_qty,
        },
    }))
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

fn live_post_snapshot_order_ids(
    pipeline: &LiveEventPipeline,
    candidate_order_id: u64,
    account_id: &str,
    venue_id: &str,
) -> Result<BTreeSet<u64>, String> {
    let Some(balance_seq) = pipeline.log().events().iter().rev().find_map(|event| {
        matches!(
            &event.kind,
            qx_core::EventKind::AccountBalanceSnapshot {
                account_id: snapshot_account,
                venue_id: snapshot_venue,
                ..
            } if snapshot_account == account_id && snapshot_venue.eq_ignore_ascii_case(venue_id)
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
    Ok(post_snapshot_order_ids)
}

fn pending_order_reserve_exact(
    order: &Order,
    spec: &TradingInstrumentSpec,
    reference_mark: Option<ExactDecimal>,
    candidate_product: qx_core::TradingProduct,
    funding_currency: &str,
) -> Result<ExactDecimal, String> {
    if order.instrument != spec.instrument {
        return Err(format!(
            "在途订单 {} 与精确预留规格标的不一致",
            order.client_id
        ));
    }
    if !order.has_remaining_amount() {
        return Ok(ExactDecimal::ZERO);
    }
    let policy = order.policy.unwrap_or_default();
    policy
        .validate_for(spec)
        .map_err(|error| format!("在途订单 {} policy 无效: {error}", order.client_id))?;
    let (should_reserve, currency) = match spec.product {
        qx_core::TradingProduct::Spot if order.side == qx_core::Side::Buy => {
            (true, spec.quote_currency.as_str())
        }
        qx_core::TradingProduct::Spot => (false, spec.quote_currency.as_str()),
        qx_core::TradingProduct::Margin
        | qx_core::TradingProduct::Perpetual
        | qx_core::TradingProduct::Future => {
            (!policy.reduce_only, spec.settlement_currency.as_str())
        }
        qx_core::TradingProduct::Option => {
            return Err(format!(
                "在途期权订单 {} 没有可认证的保证金预留模型",
                order.client_id
            ));
        }
    };
    if !should_reserve {
        return Ok(ExactDecimal::ZERO);
    }
    if !currency.eq_ignore_ascii_case(funding_currency) {
        if candidate_product == qx_core::TradingProduct::Spot
            && spec.product == qx_core::TradingProduct::Spot
        {
            return Ok(ExactDecimal::ZERO);
        }
        return Err(format!(
            "在途订单资金币种 {currency} 与候选订单资金币种 {funding_currency} 不一致"
        ));
    }
    if let Some(remaining_budget) = order.remaining_budget_decimal() {
        if spec.product != qx_core::TradingProduct::Spot || order.side != qx_core::Side::Buy {
            return Err(format!("在途预算订单 {} 只支持现货买入", order.client_id));
        }
        return Ok(remaining_budget);
    }
    let remaining = order
        .remaining_decimal()
        .ok_or_else(|| format!("在途订单 {} 精确剩余数量溢出", order.client_id))?;
    if !remaining.is_positive() {
        return Ok(ExactDecimal::ZERO);
    }
    let limit = order.limit_price_decimal();
    let market_reference = order.market_reference_price_decimal().or(reference_mark);
    let price = match (limit, market_reference) {
        (Some(limit), Some(mark)) => {
            if limit
                .checked_cmp(mark)
                .is_none_or(|ordering| ordering.is_gt())
            {
                limit
            } else {
                mark
            }
        }
        (Some(limit), None) => limit,
        (None, Some(mark)) => mark,
        (None, None) => {
            return Err(format!(
                "在途市价订单 {} 缺少精确资金预留参考价",
                order.client_id
            ));
        }
    };
    match spec.product {
        qx_core::TradingProduct::Spot => spec.notional_exact(remaining, price).map_err(|error| {
            format!(
                "在途现货订单 {} 精确名义额计算失败: {error}",
                order.client_id
            )
        }),
        qx_core::TradingProduct::Margin
        | qx_core::TradingProduct::Perpetual
        | qx_core::TradingProduct::Future => spec
            .initial_margin_exact(remaining, price, policy.leverage)
            .map_err(|error| format!("在途订单 {} 精确保证金计算失败: {error}", order.client_id)),
        qx_core::TradingProduct::Option => Err(format!(
            "在途期权订单 {} 没有可认证的保证金预留模型",
            order.client_id
        )),
    }
}

fn live_post_snapshot_order_reserve_exact(
    orders: &[Order],
    inputs: LiveOrderReserveInputs<'_>,
) -> Result<ExactDecimal, String> {
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
    let post_snapshot_order_ids =
        live_post_snapshot_order_ids(pipeline, candidate_order_id, account_id, venue_id)?;

    let needs_other_market_spec = orders.iter().any(|order| {
        post_snapshot_order_ids.contains(&order.client_id)
            && order.client_id != candidate_order_id
            && order.account_id == account_id
            && order.instrument != current_spec.instrument
            && !order.status.is_terminal()
            && order.has_remaining_amount()
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
                && order.has_remaining_amount()
        })
        .try_fold(ExactDecimal::ZERO, |total, order| {
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
            let mark = pipeline
                .marks()
                .get(&order.instrument)
                .map(|price| ExactDecimal::from_parts(price.raw(), 9))
                .transpose()
                .map_err(|error| format!("Live 在途标记价转换失败: {error}"))?;
            let amount = pending_order_reserve_exact(
                order,
                &order_spec,
                mark,
                current_spec.product,
                funding_currency,
            )?;
            total
                .checked_add(amount)
                .ok_or_else(|| "Live 精确在途订单资金预留合计溢出".to_string())
        })
}

#[cfg(test)]
mod spot_fx_resolution_tests {
    use super::*;
    use qx_core::{ExactAccountBalanceAmounts, Money};

    fn add_spot(
        specs: &mut BTreeMap<InstrumentId, TradingInstrumentSpec>,
        marks: &mut BTreeMap<InstrumentId, Price>,
        base: &str,
        quote: &str,
        venue: &str,
        price_raw: i128,
    ) {
        let instrument = InstrumentId::parse(&format!("{base}/{quote}.{venue}")).unwrap();
        let mut spec = TradingInstrumentSpec::spot_default(&instrument, quote);
        spec.base_currency = base.to_string();
        spec.quote_currency = quote.to_string();
        specs.insert(instrument.clone(), spec);
        marks.insert(instrument, Price::from_raw(price_raw));
    }

    #[test]
    fn required_currency_without_a_spot_route_fails_closed() {
        let error = spot_fx_rates_to_reporting(
            &BTreeMap::new(),
            &BTreeMap::new(),
            ["BTC".to_string()],
            "USDT",
        )
        .unwrap_err();

        assert!(error.contains("BTC->USDT"));
    }

    #[test]
    fn multi_venue_spot_path_resolves_compounded_rate() {
        let mut specs = BTreeMap::new();
        let mut marks = BTreeMap::new();
        add_spot(&mut specs, &mut marks, "AAA", "BBB", "BINANCE", 2 * SCALE);
        add_spot(&mut specs, &mut marks, "BBB", "USDT", "OKX", 3 * SCALE);

        let rates =
            spot_fx_rates_to_reporting(&specs, &marks, ["AAA".to_string()], "USDT").unwrap();

        assert_eq!(rates["AAA"].raw(), 6 * SCALE);
    }

    #[test]
    fn exact_fx_path_keeps_sub_nano_market_prices() {
        let instrument = InstrumentId::parse("TINY/USDT.BITGET").unwrap();
        let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        spec.base_currency = "TINY".into();
        spec.quote_currency = "USDT".into();
        let specs = BTreeMap::from([(instrument.clone(), spec)]);
        let marks = BTreeMap::from([(instrument, ExactDecimal::parse("0.0000000007").unwrap())]);

        let rates =
            spot_fx_exact_rates_to_reporting(&specs, &marks, ["TINY".to_string()], "USDT").unwrap();

        assert_eq!(rates["TINY"], ExactDecimal::parse("0.0000000007").unwrap());
    }

    #[test]
    fn exact_fx_conflicting_equal_length_routes_fail_closed() {
        let mut specs = BTreeMap::new();
        let mut marks = BTreeMap::new();
        for (base, quote, venue, price) in [
            ("TINY", "AAA", "BINANCE", "2"),
            ("AAA", "USDT", "OKX", "3"),
            ("TINY", "BBB", "BITGET", "4"),
            ("BBB", "USDT", "BINANCE", "3"),
        ] {
            let instrument = InstrumentId::parse(&format!("{base}/{quote}.{venue}")).unwrap();
            let mut spec = TradingInstrumentSpec::spot_default(&instrument, quote);
            spec.base_currency = base.into();
            spec.quote_currency = quote.into();
            specs.insert(instrument.clone(), spec);
            marks.insert(instrument, ExactDecimal::parse(price).unwrap());
        }

        let error = spot_fx_exact_rates_to_reporting(&specs, &marks, ["TINY".to_string()], "USDT")
            .unwrap_err();

        assert!(error.contains("等长但不一致的精确 FX 路径"));
    }

    #[test]
    fn equally_short_spot_paths_with_different_rates_fail_closed() {
        let mut specs = BTreeMap::new();
        let mut marks = BTreeMap::new();
        add_spot(&mut specs, &mut marks, "AAA", "BBB", "BINANCE", 2 * SCALE);
        add_spot(&mut specs, &mut marks, "BBB", "USDT", "OKX", 3 * SCALE);
        add_spot(&mut specs, &mut marks, "AAA", "CCC", "BITGET", 4 * SCALE);
        add_spot(&mut specs, &mut marks, "CCC", "USDT", "BINANCE", 3 * SCALE);

        let error =
            spot_fx_rates_to_reporting(&specs, &marks, ["AAA".to_string()], "USDT").unwrap_err();

        assert!(error.contains("等长但不一致的 FX 路径"));
    }

    #[test]
    fn live_exact_risk_facts_keep_balance_position_and_pending_reserve_precision() {
        let instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        let free = ExactDecimal::parse("10.000000000123").unwrap();
        let balance = AccountBalance {
            asset: "USDT".into(),
            free: Money::from_i64(10),
            locked: Money::ZERO,
            borrowed: Money::ZERO,
            net_cash_balance: None,
            exact_amounts: Some(ExactAccountBalanceAmounts {
                free,
                locked: ExactDecimal::ZERO,
                borrowed: ExactDecimal::ZERO,
                net_cash_balance: None,
            }),
        };
        let raw_position = OrderRiskPosition::new(0, 0);
        let facts = exact_live_order_risk_facts(
            &spec,
            &balance,
            ExactDecimal::parse("2").unwrap(),
            8 * SCALE,
            &raw_position,
            ExactDecimal::parse("0.0000000004").unwrap(),
            ExactDecimal::ZERO,
            ExactDecimal::ZERO,
            Some(ExactDecimal::parse("0.00000001").unwrap()),
            ExactDecimal::parse("0.0000000002").unwrap(),
            Some(5 * SCALE),
            Some(20 * SCALE),
        )
        .unwrap()
        .expect("exact sidecars project consistently to the legacy view");

        assert_eq!(
            facts.available_margin,
            Some(ExactDecimal::parse("8.000000000123").unwrap())
        );
        assert_eq!(
            facts.position.net_qty,
            ExactDecimal::parse("0.0000000004").unwrap()
        );
        assert_eq!(
            facts.position.gross_notional,
            ExactDecimal::parse("0.000000000200000004").unwrap()
        );
        assert_eq!(
            facts.max_order_notional,
            Some(ExactDecimal::parse("5").unwrap())
        );
        assert_eq!(
            facts.max_position_notional,
            Some(ExactDecimal::parse("20").unwrap())
        );
    }

    #[test]
    fn live_exact_risk_facts_are_omitted_when_raw_projection_conflicts() {
        let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        let balance = AccountBalance {
            asset: "USDT".into(),
            free: Money::from_i64(10),
            locked: Money::ZERO,
            borrowed: Money::ZERO,
            net_cash_balance: None,
            exact_amounts: Some(ExactAccountBalanceAmounts {
                free: ExactDecimal::parse("10.0000000001").unwrap(),
                locked: ExactDecimal::ZERO,
                borrowed: ExactDecimal::ZERO,
                net_cash_balance: None,
            }),
        };
        let raw_position = OrderRiskPosition::new(SCALE, 0);

        let facts = exact_live_order_risk_facts(
            &spec,
            &balance,
            ExactDecimal::ZERO,
            10 * SCALE,
            &raw_position,
            ExactDecimal::parse("0.0000000001").unwrap(),
            ExactDecimal::ZERO,
            ExactDecimal::ZERO,
            Some(ExactDecimal::parse("1").unwrap()),
            ExactDecimal::ZERO,
            None,
            None,
        )
        .unwrap();

        assert!(facts.is_none());
    }

    #[test]
    fn exact_spot_pending_reserve_keeps_sub_nano_quantity_and_price() {
        use qx_core::{ExactOrderAmounts, OrderStatus, Side};

        let instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
        let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        spec.exact_rules = Some(qx_core::ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        let order = Order {
            client_id: 77,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: qx_core::Quantity::ZERO,
            limit: None,
            status: OrderStatus::Working,
            filled: qx_core::Quantity::ZERO,
            account_id: "acct".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.0000000003").unwrap(),
                limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
                filled: ExactDecimal::parse("0.0000000001").unwrap(),
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };

        let reserve =
            pending_order_reserve_exact(&order, &spec, None, qx_core::TradingProduct::Spot, "USDT")
                .unwrap();
        assert_eq!(
            reserve,
            ExactDecimal::parse("0.000000000000000002").unwrap()
        );
        assert_eq!(
            pending_order_reserve_exact(&order, &spec, None, qx_core::TradingProduct::Spot, "EUR",)
                .unwrap(),
            ExactDecimal::ZERO,
            "不同现货报价币不能占用候选订单的资金桶"
        );
    }

    #[test]
    fn bitget_quote_budget_pending_reserve_tracks_unspent_quote_amount() {
        use qx_core::{ExactOrderAmounts, OrderStatus, Side};

        let instrument = InstrumentId::parse("PEPE/USDT.BITGET").unwrap();
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        let order = Order {
            client_id: 78,
            instrument,
            side: Side::Buy,
            qty: qx_core::Quantity::ZERO,
            limit: None,
            status: OrderStatus::PartiallyFilled,
            filled: qx_core::Quantity::ZERO,
            account_id: "acct".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::ZERO,
                limit_price: None,
                filled: ExactDecimal::parse("1000000").unwrap(),
                quote_budget: Some(ExactDecimal::parse("10").unwrap()),
                quote_filled: ExactDecimal::parse("4.25").unwrap(),
                market_reference_price: Some(ExactDecimal::parse("0.000001").unwrap()),
            }),
        };
        assert_eq!(
            pending_order_reserve_exact(
                &order,
                &spec,
                None,
                qx_core::TradingProduct::Spot,
                "USDT",
            )
            .unwrap(),
            ExactDecimal::parse("5.75").unwrap()
        );
    }

    #[test]
    fn exact_contract_pending_reserve_uses_remaining_quantity_and_leverage() {
        use qx_core::{
            ExactOrderAmounts, MarginMode, OrderPolicy, OrderStatus, PositionMode, PositionSide,
            Side, TradingProduct,
        };

        let instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
        let mut spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        spec.product = TradingProduct::Perpetual;
        spec.linear = true;
        spec.max_leverage = 10;
        spec.exact_rules = Some(qx_core::ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.01").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        });
        let order = Order {
            client_id: 78,
            instrument,
            side: Side::Buy,
            qty: qx_core::Quantity::ZERO,
            limit: None,
            status: OrderStatus::Working,
            filled: qx_core::Quantity::ZERO,
            account_id: "acct".into(),
            trace: None,
            policy: Some(OrderPolicy {
                margin_mode: MarginMode::Cross,
                position_mode: PositionMode::OneWay,
                position_side: PositionSide::Net,
                leverage: 3,
                ..OrderPolicy::default()
            }),
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.0000000003").unwrap(),
                limit_price: Some(ExactDecimal::parse("100").unwrap()),
                filled: ExactDecimal::parse("0.0000000001").unwrap(),
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };

        let reserve =
            pending_order_reserve_exact(&order, &spec, None, TradingProduct::Perpetual, "USDT")
                .unwrap();
        assert_eq!(
            reserve,
            ExactDecimal::parse("0.000000006666666667").unwrap()
        );
    }

    #[test]
    fn exact_spot_sell_inventory_deducts_sub_nano_pending_orders() {
        use qx_core::{ExactOrderAmounts, OrderStatus, Side};

        let instrument = InstrumentId::parse("PEPE/USDT.BINANCE").unwrap();
        let pending = Order {
            client_id: 91,
            instrument: instrument.clone(),
            side: Side::Sell,
            qty: qx_core::Quantity::ZERO,
            limit: None,
            status: OrderStatus::PartiallyFilled,
            filled: qx_core::Quantity::ZERO,
            account_id: "acct".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("0.0000000002").unwrap(),
                limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
                filled: ExactDecimal::parse("0.0000000001").unwrap(),
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };
        let mut pre_snapshot_pending = pending.clone();
        pre_snapshot_pending.client_id = 92;
        pre_snapshot_pending.exact_amounts = Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.0000000004").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::parse("0.0000000001").unwrap(),
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        });
        let pending_orders = [pending, pre_snapshot_pending];
        let all_pending =
            pending_spot_sell_quantity_exact(&pending_orders, 93, "acct", &instrument, None)
                .unwrap();
        let post_snapshot = pending_spot_sell_quantity_exact(
            &pending_orders,
            93,
            "acct",
            &instrument,
            Some(&BTreeSet::from([91])),
        )
        .unwrap();
        assert_eq!(all_pending, ExactDecimal::parse("0.0000000004").unwrap());
        assert_eq!(post_snapshot, ExactDecimal::parse("0.0000000001").unwrap());

        let available = exact_spot_sell_available_inventory(
            ExactDecimal::parse("0.0000000006").unwrap(),
            all_pending,
            Some(ExactDecimal::parse("0.0000000003").unwrap()),
            post_snapshot,
        )
        .unwrap();
        assert_eq!(available, ExactDecimal::parse("0.0000000002").unwrap());
        assert!(ExactDecimal::parse("0.0000000003")
            .unwrap()
            .checked_cmp(available)
            .unwrap()
            .is_gt());
    }
}
