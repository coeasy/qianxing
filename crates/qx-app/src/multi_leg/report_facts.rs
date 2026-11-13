//! Deterministic accounting facts used when verifying a persisted multi-leg report.

use qx_core::{
    ExactDecimal, InstrumentId, Ledger, LedgerEntryKind, Price, TradingInstrumentSpec,
    ValuationContext, SCALE,
};
use qx_datastruct::BarFrame;
use serde_json::Value;

/// Inputs required to recompute one leg's final return from the replayed ledger and market data.
pub struct MultiLegLegReturnFacts<'a> {
    pub ledger: &'a Ledger,
    pub account_id: &'a str,
    pub instrument: &'a str,
    pub market_spec: &'a TradingInstrumentSpec,
    pub frame: &'a BarFrame,
    pub fills: &'a [qx_core::Fill],
    pub funding_cost_raw: i128,
    pub initial_cash_raw: i128,
}

/// Rebuild the persisted shared-account summary exclusively from the replayed ledger.
pub fn multi_leg_shared_account_ledger_summary_v1(
    account_id: &str,
    initial_cash_raw: i128,
    instruments: &[InstrumentId],
    ledger: &Ledger,
) -> Result<Value, String> {
    if instruments.len() != 2 {
        return Err("多腿共享账户账簿必须包含两个标的".into());
    }
    let positions = instruments
        .iter()
        .map(|instrument| {
            let position = ledger.position_for(account_id, instrument);
            serde_json::json!({
                "instrument": instrument.to_string(),
                "quantity_raw": position.quantity.raw().to_string(),
                "average_entry_raw": position.average_entry.raw().to_string(),
                "realized_pnl_raw": position.realized_pnl.raw().to_string(),
            })
        })
        .collect::<Vec<_>>();
    let realized_pnl = ledger
        .realized_pnl_for(account_id)
        .ok_or_else(|| "多腿重放账户已实现盈亏溢出".to_string())?;
    Ok(serde_json::json!({
        "account_id": account_id,
        "allocation_policy": "shared_buying_power_deterministic_market_priority",
        "cross_leg_buying_power": true,
        "initial_cash_raw": initial_cash_raw.to_string(),
        "initial_cash": format_raw(initial_cash_raw),
        "cash_balances_raw": ledger.cash_balances_for(account_id).iter()
            .map(|(currency, value)| (currency.clone(), value.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "positions": positions,
        "realized_pnl_raw": realized_pnl.to_string(),
        "ledger_entry_count": ledger.entries().len(),
        "replay_verified": true,
    }))
}

/// Build the exact-v2 shared account summary with its authoritative principal alongside the raw projection.
pub fn multi_leg_shared_account_ledger_summary_exact_v2(
    account_id: &str,
    initial_cash_raw: i128,
    initial_cash_exact: ExactDecimal,
    instruments: &[InstrumentId],
    ledger: &Ledger,
) -> Result<Value, String> {
    if !initial_cash_exact.is_positive() {
        return Err("多腿 exact 共享账户初始资金必须为正".into());
    }
    let mut summary = multi_leg_shared_account_ledger_summary_v1(
        account_id,
        initial_cash_raw,
        instruments,
        ledger,
    )?;
    let object = summary
        .as_object_mut()
        .ok_or_else(|| "多腿共享账户账簿摘要根节点不是对象".to_string())?;
    object.insert(
        "initial_cash_exact".into(),
        Value::String(initial_cash_exact.to_string()),
    );
    object.insert(
        "initial_cash".into(),
        Value::String(initial_cash_exact.to_string()),
    );
    Ok(summary)
}

/// Verify a persisted summary and return the canonical projection for report consumers.
pub fn verify_multi_leg_shared_account_ledger_v1(
    account_id: &str,
    initial_cash_raw: i128,
    instruments: &[InstrumentId],
    ledger: &Ledger,
    declared_summary: &Value,
) -> Result<Value, String> {
    let expected = multi_leg_shared_account_ledger_summary_v1(
        account_id,
        initial_cash_raw,
        instruments,
        ledger,
    )?;
    if declared_summary != &expected {
        return Err("多腿共享账户账簿摘要与 EventLog 重放不一致".into());
    }
    Ok(expected)
}

/// Verify the exact-v2 account summary against the EventLog-replayed ledger.
pub fn verify_multi_leg_shared_account_ledger_exact_v2(
    account_id: &str,
    initial_cash_raw: i128,
    initial_cash_exact: ExactDecimal,
    instruments: &[InstrumentId],
    ledger: &Ledger,
    declared_summary: &Value,
) -> Result<Value, String> {
    let expected = multi_leg_shared_account_ledger_summary_exact_v2(
        account_id,
        initial_cash_raw,
        initial_cash_exact,
        instruments,
        ledger,
    )?;
    if declared_summary != &expected {
        return Err("多腿 exact 共享账户账簿摘要与 EventLog 重放不一致".into());
    }
    Ok(expected)
}

/// Inputs to recompute and reconcile aggregate costs for a persisted two-leg report.
#[derive(Clone, Copy)]
pub struct MultiLegCostVerificationFacts<'a> {
    pub primary_fills: &'a [qx_core::Fill],
    pub reference_fills: &'a [qx_core::Fill],
    pub primary_market_spec: &'a TradingInstrumentSpec,
    pub reference_market_spec: &'a TradingInstrumentSpec,
    pub primary_funding_raw: i128,
    pub reference_funding_raw: i128,
    pub declared_group_fees_raw: i128,
    pub declared_residual_fees_raw: i128,
    pub declared_funding_raw: i128,
    pub declared_group_turnover_raw: i128,
    pub declared_pending_turnover_raw: i128,
}

/// Values verified against the replayed fills and the persisted attribution declarations.
pub struct MultiLegCostFacts {
    pub total_fees_raw: i128,
    pub total_funding_raw: i128,
    pub total_turnover_raw: i128,
    pub cost_bps: Option<i64>,
}

/// Recompute aggregate fee, funding, turnover, and cost facts, then reconcile their declarations.
pub fn verify_multi_leg_cost_facts_v1(
    facts: MultiLegCostVerificationFacts<'_>,
) -> Result<MultiLegCostFacts, String> {
    let total_fees_raw = facts
        .primary_fills
        .iter()
        .chain(facts.reference_fills)
        .try_fold(0_i128, |sum, fill| sum.checked_add(fill.fee.raw()))
        .ok_or_else(|| "多腿成交费用总额溢出".to_string())?;
    let total_funding_raw = facts
        .primary_funding_raw
        .checked_add(facts.reference_funding_raw)
        .ok_or_else(|| "多腿资金费总额溢出".to_string())?;
    let total_turnover_raw = facts
        .primary_fills
        .iter()
        .map(|fill| {
            facts
                .primary_market_spec
                .notional(fill.qty.raw(), fill.price.raw())
        })
        .chain(facts.reference_fills.iter().map(|fill| {
            facts
                .reference_market_spec
                .notional(fill.qty.raw(), fill.price.raw())
        }))
        .try_fold(0_i128, |sum, amount| {
            amount.ok().and_then(|amount| sum.checked_add(amount))
        })
        .ok_or_else(|| "多腿成交额重算失败或溢出".to_string())?;
    if facts
        .declared_group_fees_raw
        .checked_add(facts.declared_residual_fees_raw)
        != Some(total_fees_raw)
        || facts.declared_funding_raw != total_funding_raw
        || facts
            .declared_group_turnover_raw
            .checked_add(facts.declared_pending_turnover_raw)
            != Some(total_turnover_raw)
    {
        return Err("多腿费用、资金费或成交额归因与重放事实不一致".into());
    }
    let net_cost_raw = total_fees_raw
        .checked_add(total_funding_raw)
        .ok_or_else(|| "多腿净成本溢出".to_string())?;
    let cost_bps = super::metrics::multi_leg_cost_bps(net_cost_raw, total_turnover_raw)?;
    Ok(MultiLegCostFacts {
        total_fees_raw,
        total_funding_raw,
        total_turnover_raw,
        cost_bps,
    })
}

/// Sum funding cashflows for one account and instrument, returning cost as a positive amount.
pub fn multi_leg_funding_cost(
    ledger: &Ledger,
    account_id: &str,
    instrument: &str,
) -> Result<i128, String> {
    let instrument_id = qx_core::InstrumentId::parse(instrument)
        .ok_or_else(|| format!("多腿标的 ID 无效: {instrument}"))?;
    let cashflow = ledger
        .entries()
        .iter()
        .filter(|entry| {
            entry.account_id == account_id
                && entry.kind == LedgerEntryKind::Funding
                && entry.instrument.as_ref() == Some(&instrument_id)
        })
        .try_fold(0_i128, |sum, entry| sum.checked_add(entry.amount.raw()))
        .ok_or_else(|| "多腿重放资金费累计溢出".to_string())?;
    cashflow
        .checked_neg()
        .ok_or_else(|| "多腿重放资金费符号溢出".to_string())
}

/// Recompute a leg's return from replayed realized PnL, final mark, fees, funding, and capital.
pub fn multi_leg_leg_return_bps(facts: MultiLegLegReturnFacts<'_>) -> Result<i32, String> {
    let instrument_id = qx_core::InstrumentId::parse(facts.instrument)
        .ok_or_else(|| format!("多腿标的 ID 无效: {}", facts.instrument))?;
    let position = facts.ledger.position_for(facts.account_id, &instrument_id);
    let unrealized = if position.quantity.raw() == 0 {
        0
    } else {
        let mark = facts
            .frame
            .close_raw
            .last()
            .copied()
            .ok_or_else(|| "多腿最终估值缺少 Bar".to_string())?;
        facts
            .market_spec
            .unrealized_pnl(position.quantity.raw(), position.average_entry.raw(), mark)
            .map_err(|error| format!("多腿未实现盈亏复算失败: {error:?}"))?
    };
    let fees = facts
        .fills
        .iter()
        .try_fold(0_i128, |sum, fill| sum.checked_add(fill.fee.raw()))
        .ok_or_else(|| "多腿逐腿费用累计溢出".to_string())?;
    if facts.initial_cash_raw <= 0 {
        return Err("多腿逐腿收益分母必须为正".into());
    }
    position
        .realized_pnl
        .raw()
        .checked_add(unrealized)
        .and_then(|value| value.checked_sub(fees))
        .and_then(|value| value.checked_sub(facts.funding_cost_raw))
        .and_then(|value| value.checked_mul(10_000))
        .and_then(|value| value.checked_div(facts.initial_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| "多腿逐腿收益无法复算".into())
}

/// Recompute a leg's scale-aware PnL contribution from replayed cashflows and positions.
pub fn multi_leg_leg_pnl_exact(facts: MultiLegLegReturnFacts<'_>) -> Result<ExactDecimal, String> {
    let instrument = InstrumentId::parse(facts.instrument)
        .filter(|instrument| instrument.to_string() == facts.instrument)
        .ok_or_else(|| format!("多腿标的 ID 无效: {}", facts.instrument))?;
    let mark_raw = facts
        .frame
        .close_raw
        .last()
        .copied()
        .ok_or_else(|| "多腿精确最终估值缺少 Bar".to_string())?;
    let marks = std::collections::BTreeMap::from([(instrument.clone(), Price::from_raw(mark_raw))]);
    let mut cash_by_currency = std::collections::BTreeMap::<String, ExactDecimal>::new();
    for entry in facts.ledger.entries().iter().filter(|entry| {
        entry.account_id == facts.account_id && entry.instrument.as_ref() == Some(&instrument)
    }) {
        let amount = match entry.exact_amounts {
            Some(amounts) => amounts.amount,
            None => ExactDecimal::from_parts(entry.amount.raw(), 9)
                .map_err(|error| format!("多腿 exact 账簿金额无效: {error}"))?,
        };
        let previous = cash_by_currency
            .get(&entry.currency)
            .copied()
            .unwrap_or(ExactDecimal::ZERO);
        let next = previous
            .checked_add(amount)
            .ok_or_else(|| "多腿 exact 标的现金流累计溢出".to_string())?;
        cash_by_currency.insert(entry.currency.clone(), next);
    }
    let valuation = ValuationContext::spot(
        facts.account_id,
        &marks,
        &facts.market_spec.settlement_currency,
    )
    .with_spec(Some(facts.market_spec));
    facts
        .ledger
        .instrument_equity_contribution_from_exact_cash(&valuation, &cash_by_currency)
        .map_err(|error| format!("多腿 exact 标的权益贡献复算失败: {error:?}"))
}

/// Recompute a leg's scale-aware return in basis points from its exact PnL contribution.
pub fn multi_leg_leg_return_bps_exact(
    exact_pnl: ExactDecimal,
    initial_cash_raw: i128,
) -> Result<i32, String> {
    if initial_cash_raw <= 0 {
        return Err("多腿精确腿级收益分母必须为正".into());
    }
    let initial_cash = ExactDecimal::from_parts(initial_cash_raw, 9)
        .map_err(|error| format!("多腿精确腿级初始资金无效: {error}"))?;
    crate::report_analysis::exact_ratio_bps_v1(exact_pnl, initial_cash)
}

/// Recompute a leg's exact-scale return using the exact v2 principal denominator.
pub fn multi_leg_leg_return_bps_exact_v2(
    exact_pnl: ExactDecimal,
    initial_cash_exact: ExactDecimal,
) -> Result<i32, String> {
    if !initial_cash_exact.is_positive() {
        return Err("多腿 exact 腿级收益分母必须为正".into());
    }
    crate::report_analysis::exact_ratio_bps_v1(exact_pnl, initial_cash_exact)
}

fn format_raw(raw: i128) -> String {
    let whole = raw / SCALE;
    let fraction = raw.unsigned_abs() % SCALE as u128;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:08}")
            .trim_end_matches('0')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        Fill, InstrumentFunding, InstrumentId, Ledger, Money, Order, OrderStatus, Price, Quantity,
        SCALE,
    };

    #[test]
    fn funding_cost_is_scoped_to_account_and_instrument() {
        let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let eth = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
        let mut ledger = Ledger::new();
        for (account, instrument, amount) in [
            ("account-a", &btc, -7 * SCALE),
            ("account-a", &eth, -13 * SCALE),
            ("account-b", &btc, -17 * SCALE),
        ] {
            ledger
                .apply_instrument_funding(
                    account,
                    "USDT",
                    InstrumentFunding {
                        instrument,
                        quantity: Quantity::from_raw(SCALE),
                        mark_price: Price::from_raw(100 * SCALE),
                        amount: Money::from_raw(amount),
                        ts: 1,
                    },
                )
                .unwrap();
        }
        ledger
            .apply_funding("account-a", "USDT", Money::from_raw(-99 * SCALE), 2)
            .unwrap();

        assert_eq!(
            multi_leg_funding_cost(&ledger, "account-a", "BTCUSDT.BINANCE").unwrap(),
            7 * SCALE
        );
    }

    #[test]
    fn final_leg_return_includes_unrealized_pnl_fees_and_funding() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let spec = TradingInstrumentSpec::spot_default(&instrument, "USDT");
        let order = Order {
            client_id: 1,
            instrument: instrument.clone(),
            side: qx_core::Side::Buy,
            qty: Quantity::from_raw(SCALE),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "account-a".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let fill = Fill {
            order_id: 1,
            qty: Quantity::from_raw(SCALE),
            price: Price::from_raw(100 * SCALE),
            fee: Money::from_raw(2 * SCALE),
            ts: 1,
            account_id: "account-a".into(),
            ..Fill::default()
        };
        let mut ledger = Ledger::new();
        ledger
            .apply_fill_with_spec(&order, &fill, "USDT", &spec)
            .unwrap();
        let frame = BarFrame::from_json(&format!(
            r#"{{"instrument":"BTCUSDT.BINANCE","source":"test","ts":[1],"open_raw":[{}],"high_raw":[{}],"low_raw":[{}],"close_raw":[{}],"volume_raw":[{}]}}"#,
            110 * SCALE,
            110 * SCALE,
            110 * SCALE,
            110 * SCALE,
            SCALE
        ))
        .unwrap();

        let bps = multi_leg_leg_return_bps(MultiLegLegReturnFacts {
            ledger: &ledger,
            account_id: "account-a",
            instrument: "BTCUSDT.BINANCE",
            market_spec: &spec,
            frame: &frame,
            fills: &[fill],
            funding_cost_raw: SCALE,
            initial_cash_raw: 1_000 * SCALE,
        })
        .unwrap();
        assert_eq!(bps, 70);
    }

    #[test]
    fn shared_account_ledger_summary_is_rebuilt_and_tampering_is_rejected() {
        let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let eth = InstrumentId::parse("ETHUSDT.OKX").unwrap();
        let ledger = Ledger::new();
        let summary = multi_leg_shared_account_ledger_summary_v1(
            "account-a",
            1_500 * SCALE,
            &[btc.clone(), eth.clone()],
            &ledger,
        )
        .unwrap();
        assert_eq!(summary["initial_cash_raw"], (1_500 * SCALE).to_string());
        assert_eq!(summary["initial_cash"], "1500");
        assert_eq!(summary["cash_balances_raw"], serde_json::json!({}));
        assert_eq!(summary["positions"].as_array().unwrap().len(), 2);

        assert_eq!(
            verify_multi_leg_shared_account_ledger_v1(
                "account-a",
                1_500 * SCALE,
                &[btc.clone(), eth.clone()],
                &ledger,
                &summary,
            )
            .unwrap(),
            summary
        );
        let mut tampered = summary.clone();
        tampered["positions"][0]["quantity_raw"] = "1".into();
        assert!(verify_multi_leg_shared_account_ledger_v1(
            "account-a",
            1_500 * SCALE,
            &[btc, eth],
            &ledger,
            &tampered,
        )
        .is_err());
    }

    #[test]
    fn aggregate_costs_reconcile_fills_funding_groups_and_pending_trades() {
        let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let eth = InstrumentId::parse("ETHUSDT.OKX").unwrap();
        let btc_spec = TradingInstrumentSpec::spot_default(&btc, "USDT");
        let eth_spec = TradingInstrumentSpec::spot_default(&eth, "USDT");
        let primary_fills = [Fill {
            qty: Quantity::from_raw(SCALE),
            price: Price::from_raw(100 * SCALE),
            fee: Money::from_raw(2 * SCALE),
            ..Fill::default()
        }];
        let reference_fills = [Fill {
            qty: Quantity::from_raw(SCALE),
            price: Price::from_raw(100 * SCALE),
            fee: Money::from_raw(3 * SCALE),
            ..Fill::default()
        }];
        let facts = MultiLegCostVerificationFacts {
            primary_fills: &primary_fills,
            reference_fills: &reference_fills,
            primary_market_spec: &btc_spec,
            reference_market_spec: &eth_spec,
            primary_funding_raw: SCALE,
            reference_funding_raw: 2 * SCALE,
            declared_group_fees_raw: 5 * SCALE,
            declared_residual_fees_raw: 0,
            declared_funding_raw: 3 * SCALE,
            declared_group_turnover_raw: 200 * SCALE,
            declared_pending_turnover_raw: 0,
        };

        let computed = verify_multi_leg_cost_facts_v1(facts).unwrap();
        assert_eq!(computed.total_fees_raw, 5 * SCALE);
        assert_eq!(computed.total_funding_raw, 3 * SCALE);
        assert_eq!(computed.total_turnover_raw, 200 * SCALE);
        assert_eq!(computed.cost_bps, Some(400));

        assert!(
            verify_multi_leg_cost_facts_v1(MultiLegCostVerificationFacts {
                declared_pending_turnover_raw: 1,
                ..facts
            })
            .is_err()
        );
    }
}
