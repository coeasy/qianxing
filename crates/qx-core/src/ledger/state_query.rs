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
            .map(|(currency, raw)| (currency.clone(), ExactDecimal::from_legacy_raw(*raw)))
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

    /// Visit canonical exact cash balances without constructing the owned map returned by
    /// [`Self::cash_balances_exact_for`]. Legacy and exact maps are merged in currency order;
    /// an exact balance replaces the legacy projection for the same currency.
    pub fn for_each_cash_balance_exact_for(
        &self,
        account_id: &str,
        mut visit: impl FnMut(&str, ExactDecimal) -> crate::error::QxResult<()>,
    ) -> crate::error::QxResult<()> {
        let mut legacy = self.cash_balances_for_ref(account_id).iter().peekable();
        let mut exact = self
            .exact_cash
            .get(account_id)
            .into_iter()
            .flat_map(|balances| balances.iter())
            .peekable();

        loop {
            let legacy_next = legacy.peek().map(|entry| (entry.0.as_str(), *entry.1));
            let exact_next = exact.peek().map(|entry| (entry.0.as_str(), *entry.1));
            match (legacy_next, exact_next) {
                (None, None) => return Ok(()),
                (Some((currency, raw)), None) => {
                    visit(currency, ExactDecimal::from_legacy_raw(raw))?;
                    legacy.next();
                }
                (None, Some((currency, amount))) => {
                    visit(currency, amount)?;
                    exact.next();
                }
                (Some((legacy_currency, raw)), Some((exact_currency, amount))) => {
                    match legacy_currency.cmp(exact_currency) {
                        std::cmp::Ordering::Less => {
                            visit(legacy_currency, ExactDecimal::from_legacy_raw(raw))?;
                            legacy.next();
                        }
                        std::cmp::Ordering::Equal => {
                            visit(exact_currency, amount)?;
                            legacy.next();
                            exact.next();
                        }
                        std::cmp::Ordering::Greater => {
                            visit(exact_currency, amount)?;
                            exact.next();
                        }
                    }
                }
            }
        }
    }

    pub fn cash_for_exact(&self, account_id: &str, currency: &str) -> ExactDecimal {
        self.exact_cash
            .get(account_id)
            .and_then(|currencies| currencies.get(currency))
            .copied()
            .unwrap_or_else(|| ExactDecimal::from_legacy_raw(self.cash_for(account_id, currency)))
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
                quantity: state
                    .exact_quantity
                    .unwrap_or_else(|| ExactDecimal::from_legacy_raw(state.quantity.raw())),
                average_entry: state
                    .exact_average_entry
                    .unwrap_or_else(|| ExactDecimal::from_legacy_raw(state.average_entry.raw())),
                realized_pnl: state
                    .exact_realized_pnl
                    .unwrap_or_else(|| ExactDecimal::from_legacy_raw(state.realized_pnl.raw())),
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

#[cfg(test)]
mod exact_cash_iteration_tests {
    use super::*;
    use crate::numeric::Money;

    #[test]
    fn streaming_exact_cash_matches_owned_snapshot_and_prefers_exact_currency_values() {
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USDT", Money::from_i64(2), 1)
            .unwrap();
        ledger
            .deposit("main", "USD", Money::from_i64(5), 2)
            .unwrap();
        ledger
            .deposit_exact("main", "USDT", ExactDecimal::parse("3.125").unwrap(), 3)
            .unwrap();
        ledger
            .deposit_exact("main", "BTC", ExactDecimal::parse("0.25").unwrap(), 4)
            .unwrap();

        let expected = ledger
            .cash_balances_exact_for("main")
            .into_iter()
            .collect::<Vec<_>>();
        let mut actual = Vec::new();
        ledger
            .for_each_cash_balance_exact_for("main", |currency, amount| {
                actual.push((currency.to_string(), amount));
                Ok(())
            })
            .unwrap();

        assert_eq!(actual, expected);
        assert_eq!(
            actual[2],
            ("USDT".into(), ExactDecimal::parse("5.125").unwrap())
        );
    }

    #[test]
    fn streaming_exact_cash_for_unknown_account_is_empty() {
        let ledger = Ledger::new();
        let mut count = 0;
        ledger
            .for_each_cash_balance_exact_for("unknown", |_, _| {
                count += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
