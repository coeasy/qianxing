use qx_app::{paper_available_margin_raw, AppErrorCategory, CallerCapability, RunContext};
use qx_core::{Ledger, Money, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

fn context(capability: CallerCapability, id: &str) -> RunContext {
    RunContext::new(capability, id)
}

#[test]
fn paper_capability_uses_the_shared_ledger_valuation_and_preserves_available_margin() {
    let mut ledger = Ledger::new();
    ledger
        .deposit("acct", "USDT", Money::from_raw(125_000), 1)
        .unwrap();

    let available = paper_available_margin_raw(
        &ledger,
        "acct",
        &BTreeMap::new(),
        "USDT",
        &BTreeMap::<_, TradingInstrumentSpec>::new(),
        &BTreeMap::<_, Price>::new(),
        &context(CallerCapability::Paper, "paper-valuation-ok"),
    )
    .expect("Paper account valuation");

    assert_eq!(available, 125_000);
}

#[test]
fn research_capability_cannot_valuate_a_paper_account() {
    let error = paper_available_margin_raw(
        &Ledger::new(),
        "acct",
        &BTreeMap::new(),
        "USDT",
        &BTreeMap::new(),
        &BTreeMap::new(),
        &context(CallerCapability::Research, "paper-valuation-denied"),
    )
    .expect_err("Research capability must be rejected");

    assert_eq!(error.category(), AppErrorCategory::PermissionDenied);
    assert_eq!(error.correlation_id(), Some("paper-valuation-denied"));
}

#[test]
fn paper_valuation_rejects_missing_account_identity() {
    let error = paper_available_margin_raw(
        &Ledger::new(),
        "  ",
        &BTreeMap::new(),
        "USDT",
        &BTreeMap::new(),
        &BTreeMap::new(),
        &context(CallerCapability::Paper, "paper-valuation-invalid"),
    )
    .expect_err("An empty account id must be rejected");

    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert_eq!(error.correlation_id(), Some("paper-valuation-invalid"));
}
