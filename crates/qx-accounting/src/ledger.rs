//! Immutable accounting ledger.

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub id: String,
    pub amount: i64,
}

#[derive(Debug, Default)]
pub struct Ledger {
    entries: Vec<LedgerEntry>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, entry: LedgerEntry) {
        self.entries.push(entry);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
