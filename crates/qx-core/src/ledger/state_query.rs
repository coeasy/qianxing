use super::{Ledger, LedgerEntry, PositionState};
use crate::identity::InstrumentId;
use crate::numeric::{ExactDecimal, Money, Quantity};
use crate::trading::PositionSide;
use std::collections::BTreeMap;
use std::sync::OnceLock;

static EMPTY_CASH_BALANCES: OnceLock<BTreeMap<String, i128>> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExactPositionState {
    pub quantity: ExactDecimal,
    pub average_entry: ExactDecimal,
    pub realized_pnl: ExactDecimal,
}

impl Ledger {
    /// 返回账户所有币种的现金余额，按币种排序，供跨币种抵押品估值使用。
    pub fn cash_balances_for(&self, account_id: &str) -> BTreeMap<String, i128> {
        self.cash_balances_for_ref(account_id).clone()
    }

    /// Borrow an account's canonical currency-sorted cash balances without allocating a copy.
    ///
    /// Hot-path consumers that only inspect the snapshot should use this method. The owned
    /// [`Self::cash_balances_for`] remains available for callers that need to retain or mutate it.
    pub fn cash_balances_for_ref(&self, account_id: &str) -> &BTreeMap<String, i128> {
        self.cash
            .get(account_id)
            .unwrap_or_else(|| EMPTY_CASH_BALANCES.get_or_init(BTreeMap::new))
    }

    /// Exact cash by currency. Legacy entries are represented at their existing 1e9 scale;
    /// exact entries retain their original decimal scale.
    pub fn cash_balances_exact_for(&self, account_id: &str) -> BTreeMap<String, ExactDecimal> {
        let mut balances = self
            .cash_balances_for_ref(account_id)
            .iter()
            .map(|(currency, raw)| {
                (
                    currency.clone(),
                    ExactDecimal::from_parts(*raw, 9).expect("legacy scale is supported"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(currencies) = self.exact_cash.get(account_id) {
            balances.extend(
                currencies
                    .iter()
                    .map(|(currency, amount)| (currency.clone(), *amount)),
            );
        }
        balances
    }

    pub fn cash_for_exact(&self, account_id: &str, currency: &str) -> ExactDecimal {
        self.exact_cash
            .get(account_id)
            .and_then(|currencies| currencies.get(currency))
            .copied()
            .unwrap_or_else(|| {
                ExactDecimal::from_parts(self.cash_for(account_id, currency), 9)
                    .expect("legacy scale is supported")
            })
    }

    pub fn position_for(&self, account_id: &str, instrument: &InstrumentId) -> PositionState {
        let mut out = self
            .positions
            .get(account_id)
            .and_then(|positions| positions.get(instrument))
            .cloned()
            .unwrap_or_default();
        if let Some(sides) = self
            .hedge_positions
            .get(account_id)
            .and_then(|positions| positions.get(instrument))
        {
            for state in sides.values() {
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
            .get(account_id)
            .and_then(|positions| positions.get(instrument))
            .and_then(|sides| sides.get(&side))
            .cloned()
            .unwrap_or_default()
    }

    pub fn position_for_side_exact(
        &self,
        account_id: &str,
        instrument: &InstrumentId,
        side: PositionSide,
    ) -> ExactPositionState {
        let state = if side == PositionSide::Net {
            self.positions
                .get(account_id)
                .and_then(|positions| positions.get(instrument))
        } else {
            self.hedge_positions
                .get(account_id)
                .and_then(|positions| positions.get(instrument))
                .and_then(|sides| sides.get(&side))
        };
        state.map_or(
            ExactPositionState {
                quantity: ExactDecimal::ZERO,
                average_entry: ExactDecimal::ZERO,
                realized_pnl: ExactDecimal::ZERO,
            },
            |state| ExactPositionState {
                quantity: state.exact_quantity.unwrap_or_else(|| {
                    ExactDecimal::from_parts(state.quantity.raw(), 9)
                        .expect("legacy scale is supported")
                }),
                average_entry: state.exact_average_entry.unwrap_or_else(|| {
                    ExactDecimal::from_parts(state.average_entry.raw(), 9)
                        .expect("legacy scale is supported")
                }),
                realized_pnl: state.exact_realized_pnl.unwrap_or_else(|| {
                    ExactDecimal::from_parts(state.realized_pnl.raw(), 9)
                        .expect("legacy scale is supported")
                }),
            },
        )
    }

    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Look up an append-only ledger entry by its stable sequential ID.
    ///
    /// Entry IDs are assigned from zero without gaps, so the ID is also its
    /// position in `entries`. Keep the ID check to fail closed if a ledger was
    /// deserialized from malformed or legacy data.
    pub fn entry(&self, id: u64) -> Option<&LedgerEntry> {
        let index = usize::try_from(id).ok()?;
        self.entries.get(index).filter(|entry| entry.id == id)
    }
}
