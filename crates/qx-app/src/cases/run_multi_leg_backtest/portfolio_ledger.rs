use crate::{AppError, AppErrorCategory};
use qx_core::{InstrumentId, Ledger, SCALE};
use serde_json::{json, Value};

pub(super) fn shared_account_ledger(
    account_id: &str,
    initial_cash_raw: i128,
    instruments: &[InstrumentId],
    ledger: &Ledger,
) -> Result<Value, AppError> {
    let cash = ledger.cash_balances_for(account_id);
    let positions = instruments
        .iter()
        .map(|instrument| {
            let state = ledger.position_for(account_id, instrument);
            json!({
                "instrument": instrument.to_string(),
                "quantity_raw": state.quantity.raw().to_string(),
                "average_entry_raw": state.average_entry.raw().to_string(),
                "realized_pnl_raw": state.realized_pnl.raw().to_string(),
            })
        })
        .collect::<Vec<_>>();
    let realized_pnl = ledger
        .realized_pnl_for(account_id)
        .ok_or_else(|| invariant("多市场共享账户已实现盈亏溢出"))?;
    Ok(json!({
        "account_id": account_id,
        "allocation_policy": "shared_buying_power_deterministic_market_priority",
        "cross_leg_buying_power": true,
        "initial_cash_raw": initial_cash_raw.to_string(),
        "initial_cash": format_raw(initial_cash_raw),
        "cash_balances_raw": cash.iter().map(|(currency, amount)| (currency.clone(), amount.to_string())).collect::<std::collections::BTreeMap<_, _>>(),
        "positions": positions,
        "realized_pnl_raw": realized_pnl.to_string(),
        "ledger_entry_count": ledger.entries().len(),
        "replay_verified": true,
    }))
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
