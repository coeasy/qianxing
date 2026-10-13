use crate::{AppError, AppErrorCategory, MultiLegBacktestSpec};
use qx_core::{InstrumentId, Ledger, SCALE};
use serde_json::{json, Value};

pub(super) fn for_spec(
    spec: &MultiLegBacktestSpec,
    primary_instrument: &InstrumentId,
    reference_instrument: &InstrumentId,
    primary: &Ledger,
    reference: &Ledger,
) -> Result<Option<Value>, AppError> {
    spec.account_id
        .as_deref()
        .map(|account_id| {
            merge_account_ledger(
                account_id,
                spec.primary_initial_cash_raw,
                spec.reference_initial_cash_raw,
                primary_instrument,
                reference_instrument,
                primary,
                reference,
            )
        })
        .transpose()
}

/// Consolidates the two independently simulated leg ledgers into one deterministic,
/// replayable account projection. Each leg still has a fixed cash reservation during
/// matching; this projection does not imply cross-leg buying power during execution.
pub(super) fn merge_account_ledger(
    account_id: &str,
    primary_initial_cash_raw: i128,
    reference_initial_cash_raw: i128,
    primary_instrument: &InstrumentId,
    reference_instrument: &InstrumentId,
    primary: &Ledger,
    reference: &Ledger,
) -> Result<Value, AppError> {
    let expected_initial_cash = primary_initial_cash_raw
        .checked_add(reference_initial_cash_raw)
        .ok_or_else(|| invariant("多腿账户初始资金合计溢出"))?;
    let mut entries = primary
        .entries()
        .iter()
        .cloned()
        .map(|entry| (entry.ts, entry.id, 0_u8, entry))
        .chain(
            reference
                .entries()
                .iter()
                .cloned()
                .map(|entry| (entry.ts, entry.id, 1_u8, entry)),
        )
        .collect::<Vec<_>>();
    entries.sort_by_key(|(ts, id, leg_order, _)| (*ts, *id, *leg_order));

    let mut merged = Ledger::new();
    for (id, (_, _, _, mut entry)) in entries.into_iter().enumerate() {
        entry.id = u64::try_from(id).map_err(|_| invariant("多腿账户账簿 entry 数量溢出"))?;
        entry.account_id = account_id.to_string();
        merged
            .apply_entry(entry)
            .map_err(|error| invariant(format!("多腿账户账簿合并重放失败: {error}")))?;
    }

    let expected_cash = checked_cash_sum(account_id, primary, reference)?;
    let actual_cash = merged.cash_balances_for(account_id);
    if actual_cash != expected_cash {
        return Err(invariant("多腿账户账簿合并后的现金与源账簿不一致"));
    }
    for instrument in [primary_instrument, reference_instrument] {
        let expected_primary = primary.position_for(account_id, instrument);
        let expected_reference = reference.position_for(account_id, instrument);
        let expected_qty = expected_primary
            .quantity
            .raw()
            .checked_add(expected_reference.quantity.raw())
            .ok_or_else(|| invariant("多腿账户持仓数量合计溢出"))?;
        let expected_realized = expected_primary
            .realized_pnl
            .raw()
            .checked_add(expected_reference.realized_pnl.raw())
            .ok_or_else(|| invariant("多腿账户已实现盈亏合计溢出"))?;
        let actual = merged.position_for(account_id, instrument);
        if actual.quantity.raw() != expected_qty || actual.realized_pnl.raw() != expected_realized {
            return Err(invariant(format!(
                "多腿账户合并账簿的 {} 持仓或已实现盈亏与源账簿不一致",
                instrument
            )));
        }
    }
    let realized_pnl = merged
        .realized_pnl_for(account_id)
        .ok_or_else(|| invariant("多腿账户已实现盈亏合计溢出"))?;
    let positions = [primary_instrument, reference_instrument]
        .into_iter()
        .map(|instrument| {
            let position = merged.position_for(account_id, instrument);
            json!({
                "instrument": instrument.to_string(),
                "quantity_raw": position.quantity.raw().to_string(),
                "average_entry_raw": position.average_entry.raw().to_string(),
                "realized_pnl_raw": position.realized_pnl.raw().to_string(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "account_id": account_id,
        "allocation_policy": "fixed_per_leg_reservation",
        "cross_leg_buying_power": false,
        "initial_cash_raw": expected_initial_cash.to_string(),
        "initial_cash": format_raw(expected_initial_cash),
        "leg_reservations": {
            "primary_raw": primary_initial_cash_raw.to_string(),
            "reference_raw": reference_initial_cash_raw.to_string(),
        },
        "cash_balances_raw": actual_cash.iter().map(|(currency, amount)| (currency.clone(), amount.to_string())).collect::<std::collections::BTreeMap<_, _>>(),
        "positions": positions,
        "realized_pnl_raw": realized_pnl.to_string(),
        "ledger_entry_count": merged.entries().len(),
        "replay_verified": true,
    }))
}

fn checked_cash_sum(
    account_id: &str,
    left: &Ledger,
    right: &Ledger,
) -> Result<std::collections::BTreeMap<String, i128>, AppError> {
    let mut balances = left.cash_balances_for(account_id);
    for (currency, amount) in right.cash_balances_for(account_id) {
        let total = balances.entry(currency).or_default();
        *total = total
            .checked_add(amount)
            .ok_or_else(|| invariant("多腿账户现金余额合计溢出"))?;
    }
    Ok(balances)
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

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
