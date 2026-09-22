//! CCXT 账户事实解析：余额、持仓、资金费、现金流与未成单差异。
//!
//! 所有金额换算在这里完成，随后交给 Runtime Ledger 与对账 worker 复用同一份定点数口径。

use super::*;
pub(crate) fn raw_json_i128(value: &serde_json::Value, key: &str) -> Result<i128, String> {
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_i64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_u64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(serde_json::Value::as_str) {
        return value
            .parse::<i128>()
            .map_err(|_| format!("CCXT 字段不是定点整数: {key}"));
    }
    Err(format!("CCXT 字段不是定点整数: {key}"))
}

pub(crate) fn ccxt_money(value: &serde_json::Value, field: &str) -> Result<Money, String> {
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    Money::from_dec(&text).ok_or_else(|| format!("CCXT 余额字段非法: {field}={text}"))
}

pub(crate) fn ccxt_balance_facts(value: &serde_json::Value) -> Result<Vec<AccountBalance>, String> {
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
    let debt = balance
        .get("debt")
        .or_else(|| balance.get("borrowed"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut assets = BTreeSet::new();
    assets.extend(free.keys().cloned());
    assets.extend(used.keys().cloned());
    assets.extend(debt.keys().cloned());
    let mut facts = Vec::new();
    for asset in assets {
        let free_amount = free
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("free.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        let locked_amount = used
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("used.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        let borrowed_amount = debt
            .get(&asset)
            .map(|value| ccxt_money(value, &format!("debt.{asset}")))
            .transpose()?
            .unwrap_or(Money::ZERO);
        if free_amount.is_zero() && locked_amount.is_zero() && borrowed_amount.is_zero() {
            continue;
        }
        facts.push(AccountBalance {
            asset,
            free: free_amount,
            locked: locked_amount,
            borrowed: borrowed_amount,
        });
    }
    Ok(facts)
}

pub(crate) fn ccxt_position_facts(
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
        let contracts = raw_json_i128(position, "contracts_raw")?;
        if contracts < 0 {
            return Err(format!("CCXT position contracts_raw 不能为负: {symbol}"));
        }
        if contracts == 0 {
            continue;
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
        let quantity = if side == "short" {
            contracts
                .checked_neg()
                .ok_or_else(|| "CCXT short position 数量溢出".to_string())?
        } else {
            contracts
        };
        let optional_price = |key: &str| -> Result<Option<Price>, String> {
            let Some(value) = position.get(key) else {
                return Ok(None);
            };
            if value.is_null() {
                return Ok(None);
            }
            let raw = raw_json_i128(position, key)?;
            if raw <= 0 {
                return Err(format!("CCXT position {key} 必须为正: {symbol}"));
            }
            Ok(Some(Price::from_raw(raw)))
        };
        // 钱字段与价格字段的"缺席"必须区分开：省略 unrealizedPnl 的连接器说的是
        // "这一项我没报"，读成 0 就变成了"这仓位没有浮亏、没占保证金"，并会作为
        // 账户持仓事实长期发布（V11 Q68）。
        let optional_money = |key: &str| -> Result<Option<Money>, String> {
            let Some(value) = position.get(key) else {
                return Ok(None);
            };
            if value.is_null() {
                return Ok(None);
            }
            Ok(Some(Money::from_raw(raw_json_i128(position, key)?)))
        };
        let leverage = position
            .get("leverage")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        facts.push(AccountPositionSnapshot {
            instrument,
            quantity: Quantity::from_raw(quantity),
            average_price: optional_price("entry_price_raw")?,
            mark_price: optional_price("mark_price_raw")?,
            liquidation_price: optional_price("liquidation_price_raw")?,
            unrealized_pnl: optional_money("unrealized_pnl_raw")?,
            initial_margin: optional_money("initial_margin_raw")?,
            maintenance_margin: optional_money("maintenance_margin_raw")?,
            leverage,
            margin_mode: position
                .get("margin_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            position_side: Some(side),
        });
    }
    Ok(facts)
}

pub(crate) fn ccxt_funding_fact(
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

pub(crate) fn ccxt_cashflow_facts(
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

pub(crate) fn cashflow_kind_key(kind: CashflowKind) -> &'static str {
    match kind {
        CashflowKind::Funding => "funding",
        CashflowKind::Interest => "interest",
        CashflowKind::Settlement => "settlement",
        CashflowKind::Transfer => "transfer",
    }
}

pub(crate) fn ingest_ccxt_cashflows(
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

pub(crate) fn ccxt_error_is_optional_derivatives_capability(error: &str) -> bool {
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
pub(crate) fn ccxt_open_order_issues(
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
