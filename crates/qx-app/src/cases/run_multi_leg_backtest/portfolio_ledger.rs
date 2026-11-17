use crate::{AppError, AppErrorCategory};
use qx_core::{ExactDecimal, InstrumentId, Ledger};
use serde_json::Value;

pub(super) fn shared_account_ledger(
    account_id: &str,
    initial_cash_raw: i128,
    initial_cash_exact: Option<ExactDecimal>,
    instruments: &[InstrumentId],
    ledger: &Ledger,
) -> Result<Value, AppError> {
    match initial_cash_exact {
        Some(exact) => crate::multi_leg_shared_account_ledger_summary_exact_v2(
            account_id,
            initial_cash_raw,
            exact,
            instruments,
            ledger,
        ),
        None => crate::multi_leg_shared_account_ledger_summary_v1(
            account_id,
            initial_cash_raw,
            instruments,
            ledger,
        ),
    }
    .map_err(invariant)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
