//! CCXT 账户事实解析：余额、持仓、资金费、现金流与未成单差异。
//!
//! 所有金额换算在这里完成，随后交给 Runtime Ledger 与对账 worker 复用同一份定点数口径。

use qx_core::{
    AccountBalance, AccountCashflow, AccountPositionSnapshot, CashflowKind,
    ExactAccountBalanceAmounts, ExactAccountPositionAmounts, ExactDecimal, FundingRateSnapshot,
    InstrumentId, Money, Order, OrderStatus, Price, Quantity, SCALE,
};
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
use std::collections::{BTreeMap, BTreeSet};

pub use crate::ccxt_market_host::raw_json_i128;

fn ccxt_exact_decimal(value: &serde_json::Value, field: &str) -> Result<ExactDecimal, String> {
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    ExactDecimal::parse(&text)
        .map_err(|error| format!("CCXT 精确十进制字段非法: {field}={text}: {error}"))
}

fn ccxt_exact_to_raw(value: ExactDecimal, field: &str) -> Result<i128, String> {
    let scale = ExactDecimal::from_parts(SCALE, 0).map_err(str::to_string)?;
    value
        .checked_mul_at_scale(scale, 0, qx_core::DecimalRounding::TowardZero)
        .map(|projected| projected.coefficient())
        .ok_or_else(|| format!("CCXT 精确金额投影到 1e9 raw 溢出: {field}"))
}

fn ccxt_exact_balance_amount(
    value: &serde_json::Value,
    field: &str,
    asset: &str,
) -> Result<Option<ExactDecimal>, String> {
    if let Some(exact) = value
        .get("exact")
        .and_then(|exact| exact.get(field))
        .and_then(|amounts| amounts.get(asset))
    {
        return ccxt_exact_decimal(exact, &format!("exact.{field}.{asset}")).map(Some);
    }
    let Some(amount) = value
        .get("balance")
        .and_then(|balance| balance.get(field))
        .and_then(|amounts| amounts.get(asset))
    else {
        return Ok(None);
    };
    ccxt_exact_decimal(amount, &format!("balance.{field}.{asset}")).map(Some)
}

fn validate_balance_amount_projection(
    value: &serde_json::Value,
    field: &str,
    asset: &str,
    exact: ExactDecimal,
) -> Result<(), String> {
    let Some(reported) = value
        .get("balance")
        .and_then(|balance| balance.get(field))
        .and_then(|amounts| amounts.get(asset))
    else {
        return Ok(());
    };
    let reported = ccxt_exact_decimal(reported, &format!("balance.{field}.{asset}"))?;
    if ccxt_exact_to_raw(reported, &format!("balance.{field}.{asset}"))?
        != ccxt_exact_to_raw(exact, &format!("exact.{field}.{asset}"))?
    {
        return Err(format!(
            "CCXT balance {field}.{asset} 与 exact sidecar 的 1e9 兼容投影不一致"
        ));
    }
    Ok(())
}

pub fn ccxt_balance_facts(value: &serde_json::Value) -> Result<Vec<AccountBalance>, String> {
    let balance = value
        .get("balance")
        .ok_or_else(|| "CCXT balance 响应缺少 balance".to_string())?;
    let free = balance
        .get("free")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "CCXT balance 缺少 free map".to_string())?;
    let used = balance
        .get("used")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let total = balance
        .get("total")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let debt = balance
        .get("debt")
        .or_else(|| balance.get("borrowed"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut assets = BTreeSet::new();
    assets.extend(free.keys().cloned());
    assets.extend(used.keys().cloned());
    assets.extend(total.keys().cloned());
    assets.extend(debt.keys().cloned());
    if let Some(exact) = value.get("exact").and_then(serde_json::Value::as_object) {
        for amounts in exact.values().filter_map(serde_json::Value::as_object) {
            assets.extend(amounts.keys().cloned());
        }
    }
    let mut facts = Vec::new();
    for asset in assets {
        let exact_free =
            ccxt_exact_balance_amount(value, "free", &asset)?.unwrap_or(ExactDecimal::ZERO);
        let exact_locked =
            ccxt_exact_balance_amount(value, "used", &asset)?.unwrap_or(ExactDecimal::ZERO);
        let exact_borrowed_field = if debt.contains_key(&asset) {
            if balance.get("debt").is_some() {
                "debt"
            } else {
                "borrowed"
            }
        } else {
            "debt"
        };
        let exact_borrowed = ccxt_exact_balance_amount(value, "debt", &asset)?
            .or(ccxt_exact_balance_amount(value, "borrowed", &asset)?)
            .unwrap_or(ExactDecimal::ZERO);
        validate_balance_amount_projection(value, "free", &asset, exact_free)?;
        validate_balance_amount_projection(value, "used", &asset, exact_locked)?;
        validate_balance_amount_projection(value, exact_borrowed_field, &asset, exact_borrowed)?;
        let net_cash_exact = ccxt_net_wallet_cash_exact(value, &asset)?;
        let free_amount = Money::from_raw(ccxt_exact_to_raw(exact_free, &format!("free.{asset}"))?);
        let locked_amount =
            Money::from_raw(ccxt_exact_to_raw(exact_locked, &format!("used.{asset}"))?);
        let borrowed_amount =
            Money::from_raw(ccxt_exact_to_raw(exact_borrowed, &format!("debt.{asset}"))?);
        let net_cash_balance = net_cash_exact
            .map(|amount| ccxt_exact_to_raw(amount, &format!("net_cash.{asset}")))
            .transpose()?
            .map(Money::from_raw);
        if exact_free.is_zero()
            && exact_locked.is_zero()
            && exact_borrowed.is_zero()
            && net_cash_exact.is_none_or(ExactDecimal::is_zero)
        {
            continue;
        }
        facts.push(AccountBalance {
            asset,
            free: free_amount,
            locked: locked_amount,
            borrowed: borrowed_amount,
            net_cash_balance,
            exact_amounts: Some(ExactAccountBalanceAmounts {
                free: exact_free,
                locked: exact_locked,
                borrowed: exact_borrowed,
                net_cash_balance: net_cash_exact,
            }),
        });
    }
    Ok(facts)
}

/// Read a venue's wallet-cash field when it is available. CCXT's `total` can be derivative
/// equity (which already includes unrealized PnL), while `free + used` can be only available
/// margin plus initial margin. Neither is a safe substitute for wallet cash.
fn ccxt_net_wallet_cash_exact(
    value: &serde_json::Value,
    asset: &str,
) -> Result<Option<ExactDecimal>, String> {
    let info = value.get("balance").and_then(|balance| balance.get("info"));
    let Some(info) = info else {
        return Ok(None);
    };
    let asset = asset.trim();

    let parse = |row: &serde_json::Value, key: &str| -> Result<Option<ExactDecimal>, String> {
        let Some(value) = row.get(key) else {
            return Ok(None);
        };
        if value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty()) {
            return Ok(None);
        }
        ccxt_exact_decimal(value, &format!("info.{key}.{asset}")).map(Some)
    };
    let subtract_fields = |base: ExactDecimal, row: &serde_json::Value, keys: &[&str]| {
        keys.iter().try_fold(base, |total, key| {
            parse(row, key).and_then(|value| {
                total
                    .checked_sub(value.unwrap_or(ExactDecimal::ZERO))
                    .ok_or_else(|| format!("CCXT info.{key}.{asset} 精确净现金计算溢出"))
            })
        })
    };
    let row_matches = |row: &serde_json::Value, keys: &[&str]| {
        keys.iter().any(|key| {
            row.get(*key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(asset))
        })
    };

    // Binance Spot/Margin exposes netAsset directly; USDⓈ-M balance rows expose wallet
    // `balance` (and may additionally expose crossWalletBalance). Use wallet fields and never
    // marginBalance, which includes unrealized PnL.
    let binance_rows = info
        .as_array()
        .or_else(|| info.get("assets").and_then(serde_json::Value::as_array))
        .or_else(|| info.get("balances").and_then(serde_json::Value::as_array))
        .or_else(|| info.get("userAssets").and_then(serde_json::Value::as_array));
    if let Some(rows) = binance_rows {
        for row in rows.iter().filter(|row| row_matches(row, &["asset"])) {
            if let Some(net) = parse(row, "netAsset")? {
                return Ok(Some(net));
            }
            if let Some(wallet) = parse(row, "walletBalance")?.or(parse(row, "balance")?) {
                return Ok(Some(wallet));
            }
            if row.get("availableBalance").is_some() || row.get("initialMargin").is_some() {
                return Err(format!(
                    "CCXT Binance {asset} derivative balance lacks an explicit wallet-balance field"
                ));
            }
        }
    }

    // OKX Trading account: cashBal is the wallet component; eq includes unrealized PnL.
    // Subtract reported liabilities and interest so a margin loan is not counted as owned cash.
    if let Some(outer_rows) = info.get("data").and_then(serde_json::Value::as_array) {
        for outer in outer_rows {
            if let Some(details) = outer.get("details").and_then(serde_json::Value::as_array) {
                for row in details.iter().filter(|row| row_matches(row, &["ccy"])) {
                    if let Some(cash) = parse(row, "cashBal")? {
                        let liability_key = if parse(row, "liab")?.is_some() {
                            "liab"
                        } else {
                            "crossLiab"
                        };
                        let net = subtract_fields(cash, row, &[liability_key, "interest"])?;
                        return Ok(Some(net));
                    }
                    if row.get("eq").is_some() {
                        return Err(format!(
                            "CCXT OKX {asset} balance reports equity but omits cashBal"
                        ));
                    }
                }
            }
        }
    }

    let bitget_rows = info
        .as_array()
        .or_else(|| info.get("data").and_then(serde_json::Value::as_array));
    if let Some(rows) = bitget_rows {
        // CCXT parses Bitget's `data` list into `balance.info` directly. Classic swap
        // accountEquity includes unrealizedPL, so separate it before treating the result as cash.
        for row in rows.iter().filter(|row| row_matches(row, &["marginCoin"])) {
            if let Some(equity) = parse(row, "accountEquity")? {
                let pnl = parse(row, "unrealizedPL")?.ok_or_else(|| {
                    format!(
                        "CCXT Bitget {asset} 有 accountEquity 但缺 unrealizedPL，无法剥离钱包现金"
                    )
                })?;
                return equity
                    .checked_sub(pnl)
                    .map(Some)
                    .ok_or_else(|| format!("CCXT Bitget {asset} 精确净钱包现金计算溢出"));
            }
        }
    }

    // Bitget UTA separates `balance` from `equity`; equity adds frozen margin and unrealized
    // PnL. `balance` is already the account's balance field, while `debt` is a separately
    // reported risk metric derived from balance + PnL, so subtracting debt again double counts it.
    let uta_rows = bitget_rows.or_else(|| {
        info.get("data")
            .and_then(|data| data.get("assets"))
            .and_then(serde_json::Value::as_array)
    });
    if let Some(rows) = uta_rows {
        for row in rows.iter().filter(|row| row_matches(row, &["coin"])) {
            if row.get("equity").is_some() && row.get("balance").is_none() {
                return Err(format!(
                    "CCXT Bitget {asset} UTA balance reports equity but omits wallet balance"
                ));
            }
            if let Some(balance) = parse(row, "balance")? {
                return Ok(Some(balance));
            }
        }
    }
    Ok(None)
}

pub fn ccxt_position_facts(
    value: &serde_json::Value,
    venue_id: &str,
) -> Result<Vec<AccountPositionSnapshot>, String> {
    let positions = value
        .get("positions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT positions 响应缺少 positions 数组".to_string())?;
    let mut facts = Vec::with_capacity(positions.len());
    for position in positions {
        let symbol = position
            .get("symbol")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "CCXT position 缺少 symbol".to_string())?;
        let instrument = InstrumentId::parse(&format!("{symbol}.{}", venue_id.to_uppercase()))
            .ok_or_else(|| format!("CCXT position symbol 不是合法 InstrumentId: {symbol}"))?;
        let contracts_raw = raw_json_i128(position, "contracts_raw")?;
        if contracts_raw < 0 {
            return Err(format!("CCXT position contracts_raw 不能为负: {symbol}"));
        }
        let contracts_exact = position
            .get("contracts_exact")
            .filter(|value| !value.is_null())
            .map(|value| ccxt_exact_decimal(value, &format!("{symbol}.contracts_exact")))
            .transpose()?
            .or_else(|| ExactDecimal::from_parts(contracts_raw, 9).ok());
        let contracts_exact = contracts_exact
            .ok_or_else(|| format!("CCXT position {symbol} contracts 无法形成精确快照"))?;
        if contracts_exact.is_negative() {
            return Err(format!("CCXT position contracts_exact 不能为负: {symbol}"));
        }
        if ccxt_exact_to_raw(contracts_exact, &format!("{symbol}.contracts"))? != contracts_raw {
            return Err(format!(
                "CCXT position {symbol} contracts_raw 与精确数量投影不一致，拒绝保存快照"
            ));
        }
        let contracts = contracts_raw;
        if contracts == 0 {
            if contracts_exact.is_zero() {
                continue;
            }
        }
        // `contracts_raw` 是无符号的张数，持仓的正负完全由 side 决定。连接器在交易所
        // 没给方向时会填 "unknown"（python/qianxing_ccxt 的 _normalize_position），
        // 把它连同缺失一起默认成多头，等于凭空替一笔可能是空头的持仓定了符号——
        // 数量符号会顺着权益、保证金、风控一路算下去。方向不明只能拒绝这份回报。
        let side = position
            .get("side")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase);
        let side = match side.as_deref() {
            Some(value @ ("long" | "short")) => value.to_string(),
            read => {
                return Err(format!(
                    "CCXT position {symbol} 的 side 不是 long/short（读到 {read:?}）；\
                     持仓方向决定数量符号，未知一律 fail-closed，请核对交易所回报的 hedged/one-way 形状"
                ));
            }
        };
        let quantity_raw = if side == "short" {
            contracts_raw
                .checked_neg()
                .ok_or_else(|| "CCXT short position 数量溢出".to_string())?
        } else {
            contracts_raw
        };
        let exact_quantity = if side == "short" {
            contracts_exact
                .checked_neg()
                .ok_or_else(|| format!("CCXT short position {symbol} 精确数量取反溢出"))?
        } else {
            contracts_exact
        };
        let optional_exact =
            |exact_key: &str, raw_key: &str| -> Result<Option<ExactDecimal>, String> {
                if let Some(value) = position.get(exact_key).filter(|value| !value.is_null()) {
                    return ccxt_exact_decimal(value, &format!("{symbol}.{exact_key}")).map(Some);
                }
                if position.get(raw_key).is_none_or(serde_json::Value::is_null) {
                    return Ok(None);
                }
                let raw = raw_json_i128(position, raw_key)?;
                ExactDecimal::from_parts(raw, 9)
                    .map(Some)
                    .map_err(|error| format!("CCXT position {symbol}.{raw_key}: {error}"))
            };
        let optional_price = |exact_key: &str, raw_key: &str| -> Result<Option<Price>, String> {
            let Some(exact) = optional_exact(exact_key, raw_key)? else {
                return Ok(None);
            };
            if !exact.is_positive() {
                return Err(format!("CCXT position {symbol}.{exact_key} 必须为正"));
            }
            let raw = ccxt_exact_to_raw(exact, &format!("{symbol}.{exact_key}"))?;
            if position.get(raw_key).is_some_and(|value| !value.is_null()) {
                if raw_json_i128(position, raw_key)? != raw {
                    return Err(format!(
                        "CCXT position {symbol}.{raw_key} 与精确价格投影不一致"
                    ));
                }
            }
            Ok((raw > 0).then_some(Price::from_raw(raw)))
        };
        // 钱字段与价格字段的"缺席"必须区分开：省略 unrealizedPnl 的连接器说的是
        // "这一项我没报"，读成 0 就变成了"这仓位没有浮亏、没占保证金"，并会作为
        // 账户持仓事实长期发布（V11 Q68）。
        let exact_money_values = std::cell::RefCell::new(Vec::with_capacity(3));
        let optional_money = |key: &str| -> Result<Option<Money>, String> {
            let exact_key = key
                .strip_suffix("_raw")
                .map(|prefix| format!("{prefix}_exact"))
                .ok_or_else(|| format!("CCXT position 钱字段名无效: {key}"))?;
            let exact = match position.get(&exact_key) {
                Some(value) => {
                    if value.is_null() {
                        None
                    } else {
                        Some(ccxt_exact_decimal(value, &format!("{symbol}.{exact_key}"))?)
                    }
                }
                None => None,
            };
            let raw = match position.get(key) {
                Some(value) => {
                    if value.is_null() {
                        None
                    } else {
                        Some(raw_json_i128(position, key)?)
                    }
                }
                None => None,
            };
            let exact = match (exact, raw) {
                (Some(exact), Some(raw)) => {
                    if ccxt_exact_to_raw(exact, &format!("{symbol}.{exact_key}"))? != raw {
                        return Err(format!("CCXT position {symbol}.{key} 与精确金额投影不一致"));
                    }
                    Some(exact)
                }
                (Some(exact), None) => Some(exact),
                (None, Some(raw)) => Some(
                    ExactDecimal::from_parts(raw, 9)
                        .map_err(|error| format!("CCXT position {symbol}.{key}: {error}"))?,
                ),
                (None, None) => None,
            };
            let money = exact
                .map(|value| ccxt_exact_to_raw(value, &format!("{symbol}.{exact_key}")))
                .transpose()?
                .map(Money::from_raw);
            exact_money_values.borrow_mut().push(exact);
            Ok(money)
        };
        let unrealized_pnl = optional_money("unrealized_pnl_raw")?;
        let initial_margin = optional_money("initial_margin_raw")?;
        let maintenance_margin = optional_money("maintenance_margin_raw")?;
        let mut exact_money_values = exact_money_values.into_inner().into_iter();
        let leverage = position
            .get("leverage")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        facts.push(AccountPositionSnapshot {
            instrument,
            settlement_currency: position
                .get("settlement_currency")
                .and_then(serde_json::Value::as_str)
                .map(|value| value.trim().to_ascii_uppercase())
                .filter(|value| !value.is_empty()),
            quantity: Quantity::from_raw(quantity_raw),
            average_price: optional_price("entry_price_exact", "entry_price_raw")?,
            mark_price: optional_price("mark_price_exact", "mark_price_raw")?,
            liquidation_price: optional_price("liquidation_price_exact", "liquidation_price_raw")?,
            unrealized_pnl,
            initial_margin,
            maintenance_margin,
            leverage,
            margin_mode: position
                .get("margin_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            position_side: Some(side),
            exact_amounts: Some(ExactAccountPositionAmounts {
                quantity: exact_quantity,
                average_price: optional_exact("entry_price_exact", "entry_price_raw")?,
                mark_price: optional_exact("mark_price_exact", "mark_price_raw")?,
                liquidation_price: optional_exact(
                    "liquidation_price_exact",
                    "liquidation_price_raw",
                )?,
                unrealized_pnl: exact_money_values.next().flatten(),
                initial_margin: exact_money_values.next().flatten(),
                maintenance_margin: exact_money_values.next().flatten(),
            }),
        });
    }
    Ok(facts)
}

pub fn ccxt_funding_fact(
    value: &serde_json::Value,
    venue_id: &str,
) -> Result<(FundingRateSnapshot, u64), String> {
    let funding = value
        .get("funding")
        .ok_or_else(|| "CCXT funding 响应缺少 funding".to_string())?;
    let symbol = funding
        .get("symbol")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT funding 缺少 symbol".to_string())?;
    let instrument = InstrumentId::parse(&format!("{symbol}.{}", venue_id.to_uppercase()))
        .ok_or_else(|| format!("CCXT funding symbol 不是合法 InstrumentId: {symbol}"))?;
    let rate = raw_json_i128(funding, "funding_rate_bps")?;
    let rate = i64::try_from(rate).map_err(|_| "CCXT funding rate 超出 i64".to_string())?;
    let timestamp = funding
        .get("timestamp_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok((
        FundingRateSnapshot {
            instrument,
            funding_rate_bps: rate,
            next_funding_timestamp_ms: funding
                .get("next_funding_timestamp_ms")
                .and_then(serde_json::Value::as_u64),
        },
        timestamp,
    ))
}

pub fn ccxt_cashflow_facts(
    value: &serde_json::Value,
    account_id: &str,
    venue_id: &str,
) -> Result<Vec<(AccountCashflow, u64)>, String> {
    let rows = value
        .get("cashflows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT cashflow 响应缺少 cashflows 数组".to_string())?;
    let mut facts = Vec::with_capacity(rows.len());
    for row in rows {
        let external_id = row
            .get("external_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "CCXT cashflow 缺少 external_id".to_string())?;
        let currency = row
            .get("currency")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("CCXT cashflow {external_id} 缺少 currency"))?
            .to_ascii_uppercase();
        let kind = match row
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
        {
            "funding" => CashflowKind::Funding,
            "interest" => CashflowKind::Interest,
            "settlement" => CashflowKind::Settlement,
            "transfer" => CashflowKind::Transfer,
            other => return Err(format!("CCXT cashflow {external_id} kind 非法: {other}")),
        };
        let amount_raw = raw_json_i128(row, "amount_raw")?;
        let timestamp = row
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        facts.push((
            AccountCashflow {
                account_id: account_id.into(),
                venue_id: venue_id.into(),
                currency,
                kind,
                amount: Money::from_raw(amount_raw),
                external_id: external_id.into(),
            },
            timestamp,
        ));
    }
    Ok(facts)
}

pub fn cashflow_kind_key(kind: CashflowKind) -> &'static str {
    match kind {
        CashflowKind::Funding => "funding",
        CashflowKind::Interest => "interest",
        CashflowKind::Settlement => "settlement",
        CashflowKind::Transfer => "transfer",
    }
}

pub fn ingest_ccxt_cashflows(
    pipeline: &mut LiveEventPipeline,
    value: &serde_json::Value,
    account_id: &str,
    venue_id: &str,
    worker_id: &str,
    received_ts: u64,
    source_seq: &mut u64,
) -> Result<usize, String> {
    let facts = ccxt_cashflow_facts(value, account_id, venue_id)?;
    let mut ingested = 0_usize;
    for (cashflow, event_ts) in facts {
        *source_seq = (*source_seq).saturating_add(1);
        let event_ts = if event_ts == 0 { received_ts } else { event_ts };
        let correlation_id = format!(
            "{}:cashflow:{}:{}:{}:{}",
            worker_id,
            cashflow_kind_key(cashflow.kind),
            cashflow.currency,
            cashflow.external_id,
            account_id
        );
        let receipt = pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow { cashflow },
                event_ts,
                received_ts,
                *source_seq,
                correlation_id,
            ))
            .map_err(|error| format!("CCXT 现金流水事实归约失败: {error:?}"))?;
        if !receipt.deduplicated {
            ingested = ingested.saturating_add(1);
        }
    }
    Ok(ingested)
}

pub fn ccxt_error_is_optional_derivatives_capability(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("unsupported")
        || error.contains("not supported")
        || error.contains("非合约市场")
        || error.contains("fetchpositions")
}

/// 将 CCXT `fetch_open_orders` 的结果与本地订单索引做只读核对。
///
/// 这里故意不把远端未知订单注册进 OMS，也不根据单次查询自动撤单或平仓。
/// 远端订单可能来自进程崩溃前尚未写入 Accepted 事实、人工操作或其他系统；
/// 正确的 fail-safe 行为是保留原始证据并让对账服务降级，交由人工确认归属。
pub fn ccxt_open_order_issues(
    value: &serde_json::Value,
    local_orders: &[Order],
    known_remote_orders: &BTreeMap<String, (u64, OrderStatus)>,
    venue_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let orders = value
        .get("orders")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "CCXT fetch_open_orders 返回缺少 orders 数组".to_string())?;
    let mut issues = Vec::new();
    for remote in orders {
        let object = remote
            .as_object()
            .ok_or_else(|| "CCXT open order 返回项必须是 object".to_string())?;
        let remote_order_id = object
            .get("order_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "CCXT open order 缺少 order_id".to_string())?;
        let symbol = object
            .get("symbol")
            .or_else(|| object.get("instrument"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let status = object
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let client_order_id = object
            .get("client_order_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let client_order_id_u64 = client_order_id.and_then(|value| value.parse::<u64>().ok());
        let local_by_client_id = client_order_id_u64.and_then(|client_id| {
            local_orders
                .iter()
                .find(|order| order.client_id == client_id)
        });
        let local_by_remote_id = known_remote_orders.get(remote_order_id);

        if let Some((local_client_id, local_status)) = local_by_remote_id {
            if local_status.is_terminal() {
                issues.push(serde_json::json!({
                    "kind": "remote_open_local_terminal",
                    "source": "ccxt.fetch_open_orders",
                    "venue_id": venue_id,
                    "remote_order_id": remote_order_id,
                    "client_order_id": local_client_id,
                    "instrument": symbol,
                    "status": status,
                    "local_status": format!("{local_status:?}"),
                    "observed": true,
                }));
            }
            continue;
        }

        let kind = if let Some(local) = local_by_client_id {
            serde_json::json!({
                "kind": "remote_open_unmapped_local_order",
                "source": "ccxt.fetch_open_orders",
                "venue_id": venue_id,
                "remote_order_id": remote_order_id,
                "client_order_id": local.client_id,
                "instrument": symbol,
                "status": status,
                "local_status": format!("{:?}", local.status),
                "observed": true,
            })
        } else {
            serde_json::json!({
                "kind": "unknown_remote_open_order",
                "source": "ccxt.fetch_open_orders",
                "venue_id": venue_id,
                "remote_order_id": remote_order_id,
                "client_order_id": client_order_id,
                "instrument": symbol,
                "status": status,
                "observed": true,
            })
        };
        issues.push(kind);
    }
    Ok(issues)
}

#[cfg(test)]
mod wallet_cash_tests {
    use super::*;

    #[test]
    fn derivatives_use_wallet_cash_instead_of_equity_or_available_margin() {
        let binance = serde_json::json!({
            "balance": {
                "free": {"USDT": "80"},
                "used": {"USDT": "20"},
                "total": {"USDT": "103"},
                "info": [{"asset":"USDT","balance":"100","marginBalance":"103","availableBalance":"80","initialMargin":"20"}]
            }
        });
        let okx = serde_json::json!({
            "balance": {
                "free": {"USDT": "95"}, "used": {"USDT": "5"}, "total": {"USDT": "103"},
                "info": {"data":[{"details":[{"ccy":"USDT","cashBal":"100","eq":"103","upl":"3","liab":"2","interest":"1"}]}]}
            }
        });
        let bitget_classic = serde_json::json!({
            "balance": {
                "free": {"USDT": "90"}, "used": {"USDT": "10"}, "total": {"USDT": "105"},
                "info": [{"marginCoin":"USDT","accountEquity":"105","unrealizedPL":"5"}]
            }
        });
        let bitget_uta = serde_json::json!({
            "balance": {
                "free": {"USDT": "95"}, "used": {"USDT": "5"}, "debt": {"USDT": "4"}, "total": {"USDT": "105"},
                "info": [{"coin":"USDT","balance":"100","equity":"105","available":"95","locked":"5","debt":"4"}]
            }
        });

        for (response, expected_wallet_cash) in [
            (binance, 100),
            (okx, 97),
            (bitget_classic, 100),
            (bitget_uta, 100),
        ] {
            let facts = ccxt_balance_facts(&response).expect("recognized venue balance schema");
            assert_eq!(facts.len(), 1);
            assert_eq!(facts[0].asset, "USDT");
            assert_eq!(
                facts[0].net_cash_raw(),
                Some(Money::from_i64(expected_wallet_cash).raw())
            );
        }
    }

    #[test]
    fn bitget_classic_equity_without_pnl_is_rejected_as_ambiguous_cash() {
        let response = serde_json::json!({
            "balance": {
                "free": {"USDT": "95"}, "used": {"USDT": "5"}, "total": {"USDT": "100"},
                "info": [{"marginCoin":"USDT","accountEquity":"100","unrealizedPL":""}]
            }
        });
        assert!(ccxt_balance_facts(&response)
            .expect_err("equity must not be misreported as wallet cash")
            .contains("缺 unrealizedPL"));
    }

    #[test]
    fn balance_snapshot_keeps_sub_nano_values_and_legacy_projection_is_conservative() {
        let response = serde_json::json!({
            "balance": {
                "free": {"PEPE": "0.000000000123"},
                "used": {"PEPE": "0"},
                "total": {"PEPE": "0.000000000123"}
            },
            "exact": {
                "free": {"PEPE": "0.000000000123"},
                "used": {"PEPE": "0"},
                "total": {"PEPE": "0.000000000123"}
            }
        });
        let facts = ccxt_balance_facts(&response).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].free, Money::ZERO);
        assert_eq!(facts[0].net_cash_raw(), Some(0));
        assert_eq!(
            facts[0].net_cash_exact(),
            Some(ExactDecimal::parse("0.000000000123").unwrap())
        );
        assert_eq!(
            facts[0].exact_amounts.unwrap().free,
            ExactDecimal::parse("0.000000000123").unwrap()
        );
    }

    #[test]
    fn balance_snapshot_rejects_a_conflicting_exact_sidecar_projection() {
        let response = serde_json::json!({
            "balance": {
                "free": {"PEPE": "1.000000002"},
                "used": {"PEPE": "0"}
            },
            "exact": {
                "free": {"PEPE": "1.0000000001"},
                "used": {"PEPE": "0"}
            }
        });
        assert!(ccxt_balance_facts(&response)
            .expect_err("conflicting raw and exact balance must not enter the event log")
            .contains("与 exact sidecar 的 1e9 兼容投影不一致"));
    }

    #[test]
    fn position_snapshot_keeps_sub_nano_quantity_price_and_negative_pnl() {
        let response = serde_json::json!({
            "positions": [{
                "symbol": "PEPE/USDT:USDT",
                "settlement_currency": "USDT",
                "side": "short",
                "contracts_raw": 0,
                "contracts_exact": "0.0000000001",
                "entry_price_raw": 0,
                "entry_price_exact": "0.0000000002",
                "mark_price_raw": 0,
                "mark_price_exact": "0.0000000003",
                "unrealized_pnl_raw": 0,
                "unrealized_pnl_exact": "-0.0000000004"
            }]
        });
        let facts = ccxt_position_facts(&response, "OKX").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].quantity, Quantity::ZERO);
        assert_eq!(facts[0].average_price, None);
        let exact = facts[0].exact_amounts.unwrap();
        assert_eq!(
            exact.quantity,
            ExactDecimal::parse("-0.0000000001").unwrap()
        );
        assert_eq!(
            exact.average_price,
            Some(ExactDecimal::parse("0.0000000002").unwrap())
        );
        assert_eq!(
            exact.unrealized_pnl,
            Some(ExactDecimal::parse("-0.0000000004").unwrap())
        );
    }

    #[test]
    fn sub_nano_position_with_zero_legacy_projection_still_requires_a_direction() {
        let response = serde_json::json!({
            "positions": [{
                "symbol": "PEPE/USDT:USDT",
                "contracts_raw": 0,
                "contracts_exact": "0.0000000001"
            }]
        });
        assert!(ccxt_position_facts(&response, "OKX")
            .expect_err("a nonzero exact position cannot be skipped as an empty raw row")
            .contains("side 不是 long/short"));
    }

    #[test]
    fn position_snapshot_rejects_conflicting_raw_and_exact_money() {
        let response = serde_json::json!({
            "positions": [{
                "symbol": "BTC/USDT:USDT",
                "side": "long",
                "contracts_raw": 1_000_000_000,
                "contracts_exact": "1",
                "unrealized_pnl_raw": 2_000_000_000,
                "unrealized_pnl_exact": "1"
            }]
        });
        assert!(ccxt_position_facts(&response, "OKX")
            .expect_err("conflicting money projection must not enter the event log")
            .contains("与精确金额投影不一致"));
    }
}
