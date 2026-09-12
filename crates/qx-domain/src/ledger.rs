use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum LedgerEntryKind {
    Cash,
    Trade,
    Fee,
    Adjustment,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerEntry {
    pub id: String,
    pub timestamp: u64,
    pub account_id: String,
    pub currency: String,
    pub amount_raw: i128,
    pub kind: LedgerEntryKind,
    pub reference: String,
}

impl LedgerEntry {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.currency.trim().is_empty()
            || self.reference.trim().is_empty()
        {
            return Err("ledger entry identity is required".into());
        }
        if self.timestamp == 0 {
            return Err("ledger entry timestamp must be non-zero".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ledger {
    pub entries: Vec<LedgerEntry>,
}

impl Ledger {
    pub fn append(&mut self, entry: LedgerEntry) -> Result<(), String> {
        entry.validate()?;
        if self.entries.iter().any(|existing| existing.id == entry.id) {
            return Err(format!("duplicate ledger entry id: {}", entry.id));
        }
        if let Some(last) = self.entries.last() {
            if last.timestamp > entry.timestamp {
                return Err("ledger timestamps must be monotonic".into());
            }
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn balance_raw(&self, account_id: &str, currency: &str) -> i128 {
        self.entries
            .iter()
            .filter(|entry| entry.account_id == account_id && entry.currency == currency)
            .fold(0_i128, |balance, entry| balance.saturating_add(entry.amount_raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_is_append_only_and_deduplicated() {
        let entry = LedgerEntry {
            id: "l1".into(),
            timestamp: 1,
            account_id: "main".into(),
            currency: "USD".into(),
            amount_raw: 100,
            kind: LedgerEntryKind::Cash,
            reference: "deposit".into(),
        };
        let mut ledger = Ledger::default();
        ledger.append(entry.clone()).unwrap();
        assert!(ledger.append(entry).is_err());
        assert_eq!(ledger.balance_raw("main", "USD"), 100);
    }
}
