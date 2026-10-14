use super::worker_runtime::load_worker_market_spec_for_instrument;
use crate::*;

mod pending_exposure;
use pending_exposure::pending_order_gross_notional;

pub(crate) fn paper_available_margin_with_market_specs(
    ledger: &Ledger,
    account_id: &str,
    marks: &BTreeMap<InstrumentId, Price>,
    settlement: &str,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
) -> Result<i128, String> {
    paper_available_margin_with_market_specs_and_fx(
        ledger,
        account_id,
        marks,
        settlement,
        market_specs,
        &BTreeMap::new(),
    )
}

pub(crate) fn paper_available_margin_with_market_specs_and_fx(
    ledger: &Ledger,
    account_id: &str,
    marks: &BTreeMap<InstrumentId, Price>,
    settlement: &str,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    fx_rates: &BTreeMap<String, Price>,
) -> Result<i128, String> {
    ledger
        .valuate(
            &ValuationContext::spot(account_id, marks, settlement)
                .with_market_specs(market_specs)
                .with_fx(fx_rates),
        )
        .map(|result| result.equity.raw())
        .map_err(|error| format!("Paper 账户多市场可用保证金计算失败: {error:?}"))
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
    worker_risk_context_inner(worker, order, pipeline, runtime_config_path, None)
}

pub(crate) fn worker_risk_context_with_config(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
) -> Result<Option<(RiskContext, OrderRiskPosition)>, String> {
    worker_risk_context_inner(
        worker,
        order,
        pipeline,
        runtime_config_path,
        Some(runtime_config),
    )
}

fn worker_risk_context_inner(
    worker: &WorkerConfig,
    order: &Order,
    pipeline: &LiveEventPipeline,
    runtime_config_path: Option<&Path>,
    runtime_config: Option<&RuntimeConfig>,
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
    if spec.product != qx_core::TradingProduct::Spot
        && !spec.settlement_currency.eq_ignore_ascii_case(&settlement)
    {
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
        let (market_specs, fx_rates) = match runtime_config {
            Some(config) => account_market_specs_and_fx(
                config,
                account_id,
                pipeline,
                &spec,
                &settlement,
                runtime_config_path,
            )?,
            None => (
                BTreeMap::from([(spec.instrument.clone(), spec.clone())]),
                BTreeMap::new(),
            ),
        };
        paper_available_margin_with_market_specs_and_fx(
            pipeline.ledger(),
            account_id,
            pipeline.marks(),
            &settlement,
            &market_specs,
            &fx_rates,
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
        pending_order_gross_notional(
            &pipeline.orders(),
            order,
            account_id,
            &spec,
            pipeline.marks(),
        )?
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

/// Resolve frozen specifications for every non-zero position in a Paper account.
/// A worker must not estimate account buying power from only its own market when
/// another enabled execution worker owns a position in the same account.
type AccountMarketValuationInputs = (
    BTreeMap<InstrumentId, TradingInstrumentSpec>,
    BTreeMap<String, Price>,
);

fn account_market_specs_and_fx(
    config: &RuntimeConfig,
    account_id: &str,
    pipeline: &LiveEventPipeline,
    current_spec: &TradingInstrumentSpec,
    reporting_currency: &str,
    runtime_config_path: Option<&Path>,
) -> Result<AccountMarketValuationInputs, String> {
    let mut specs = BTreeMap::from([(current_spec.instrument.clone(), current_spec.clone())]);
    let held_instruments = pipeline
        .ledger()
        .entries()
        .iter()
        .filter(|entry| entry.account_id == account_id)
        .filter_map(|entry| entry.instrument.clone())
        .collect::<BTreeSet<_>>();

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
        if one_way == 0 && long == 0 && short == 0 {
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
                    && candidate.symbols.iter().any(|symbol| {
                        symbol == &instrument_label
                            || InstrumentId::parse(symbol).as_ref() == Some(&instrument)
                    })
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
                    ))
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
                    ))
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
                ))
            }
            1 => {
                fx_rates.insert(currency, Price::from_raw(*observed_rates.first().unwrap()));
            }
            _ => {
                return Err(format!(
                    "FAIL_CLOSED: Paper 账户存在多个不一致的 {currency}->{reporting_currency} FX 报价"
                ))
            }
        }
    }
    Ok((specs, fx_rates))
}

fn load_worker_spec_if_market_matches(
    worker: &WorkerConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let Some(spec_path) = worker.instrument_spec_path.as_deref() else {
        return Ok(None);
    };
    let resolved_path = runtime_config_path
        .map(|path| resolve_runtime_relative_path(path, spec_path))
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
    load_worker_market_spec_for_instrument(worker, instrument, runtime_config_path)
}
