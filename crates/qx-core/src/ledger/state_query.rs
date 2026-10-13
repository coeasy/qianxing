use super::{Ledger, LedgerEntry, PositionState};
use crate::identity::InstrumentId;
use crate::numeric::{Money, Quantity};
use crate::trading::PositionSide;
use std::collections::BTreeMap;

impl Ledger {
    /// 返回账户所有币种的现金余额，按币种排序，供跨币种抵押品估值使用。
    pub fn cash_balances_for(&self, account_id: &str) -> BTreeMap<String, i128> {
        self.cash
            .iter()
            .filter(|((account, _), _)| account == account_id)
            .map(|((_, currency), amount)| (currency.clone(), *amount))
            .collect()
    }

    pub fn position_for(&self, account_id: &str, instrument: &InstrumentId) -> PositionState {
        let mut out = self
            .positions
            .get(&(account_id.to_string(), instrument.clone()))
            .cloned()
            .unwrap_or_default();
        for ((account, id, _), state) in self
            .hedge_positions
            .iter()
            .filter(|((account, id, _), _)| account == account_id && id == instrument)
        {
            let _ = (account, id);
            out.quantity =
                Quantity::from_raw(out.quantity.raw().saturating_add(state.quantity.raw()));
            out.realized_pnl = Money::from_raw(
                out.realized_pnl
                    .raw()
                    .saturating_add(state.realized_pnl.raw()),
            );
            if out.quantity.raw() != 0 {
                out.average_entry = state.average_entry;
            }
        }
        out
    }

    pub fn position_for_side(
        &self,
        account_id: &str,
        instrument: &InstrumentId,
        side: PositionSide,
    ) -> PositionState {
        if side == PositionSide::Net {
            return self.position_for(account_id, instrument);
        }
        self.hedge_positions
            .get(&(account_id.to_string(), instrument.clone(), side))
            .cloned()
            .unwrap_or_default()
    }

    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }
}
