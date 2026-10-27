//! A-share issuer and entitlement facts rendered from the completed backtest ledger.

use std::collections::BTreeMap;

use qx_core::{InstrumentId, Ledger, LedgerEntryKind};
use qx_xingban::AshareIssuerCapitalSnapshot;

/// Build the persisted A-share read model only from balances and issuer facts actually
/// produced by this run. Raw fixed-point amounts are strings to preserve all i128 digits.
pub(crate) fn ashare_analysis_json(
    ledger: &Ledger,
    issuer_capital: &[AshareIssuerCapitalSnapshot],
    issuer_capital_at_end: Option<&AshareIssuerCapitalSnapshot>,
) -> Option<serde_json::Value> {
    let mut keys: BTreeMap<(&str, &str, &InstrumentId), Vec<&'static str>> = BTreeMap::new();
    for entry in ledger.entries() {
        let Some(instrument) = entry.instrument.as_ref() else {
            continue;
        };
        let Some(kind) = (match &entry.kind {
            LedgerEntryKind::RightsEntitlement => Some("rights"),
            LedgerEntryKind::CashDividendEntitlement => Some("cash_dividend"),
            LedgerEntryKind::ConvertibleBondInterestEntitlement => {
                Some("convertible_bond_interest")
            }
            _ => None,
        }) else {
            continue;
        };
        let kinds = keys
            .entry((
                entry.account_id.as_str(),
                entry.currency.as_str(),
                instrument,
            ))
            .or_default();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if keys.is_empty() && issuer_capital.is_empty() && issuer_capital_at_end.is_none() {
        return None;
    }

    let mut keys = keys.into_iter().collect::<Vec<_>>();
    keys.sort_by(
        |((left_account, left_currency, left_instrument), _),
         ((right_account, right_currency, right_instrument), _)| {
            left_currency
                .cmp(right_currency)
                .then_with(|| {
                    left_instrument
                        .to_string()
                        .cmp(&right_instrument.to_string())
                })
                .then_with(|| left_account.cmp(right_account))
        },
    );
    let entitlements = keys
        .into_iter()
        .map(|((account_id, currency, instrument), kinds)| {
            let has_rights = kinds.contains(&"rights");
            let has_cash_dividend = kinds.contains(&"cash_dividend");
            let has_convertible_interest = kinds.contains(&"convertible_bond_interest");
            let mut row = serde_json::json!({
                "instrument": instrument.to_string(),
                "currency": currency,
                "action_kinds": kinds,
            });
            if has_rights {
                row["rights_entitlement_raw"] = serde_json::json!(ledger
                    .rights_entitlement_for(account_id, instrument)
                    .raw()
                    .to_string());
            }
            if has_cash_dividend {
                row["cash_dividend_entitlement_raw"] = serde_json::json!(ledger
                    .cash_dividend_entitlement_for(account_id, instrument, currency)
                    .raw()
                    .to_string());
            }
            if has_convertible_interest {
                row["convertible_bond_interest_entitlement_raw"] = serde_json::json!(ledger
                    .convertible_bond_interest_entitlement_for(account_id, instrument, currency)
                    .raw()
                    .to_string());
            }
            row
        })
        .collect::<Vec<_>>();
    let issuer_capital = issuer_capital
        .iter()
        .map(|snapshot| {
            serde_json::json!({
                "instrument": snapshot.instrument,
                "effective_ts": snapshot.effective_ts,
                "total_shares_raw": snapshot.total_shares_raw.to_string(),
                "free_float_shares_raw": snapshot.free_float_shares_raw.map(|value| value.to_string()),
                "source": snapshot.source,
            })
        })
        .collect::<Vec<_>>();
    Some(serde_json::json!({
        "schema_version": 1,
        "issuer_capital_snapshots": issuer_capital,
        "issuer_capital_at_end": issuer_capital_at_end.map(|snapshot| serde_json::json!({
            "instrument": snapshot.instrument,
            "effective_ts": snapshot.effective_ts,
            "total_shares_raw": snapshot.total_shares_raw.to_string(),
            "free_float_shares_raw": snapshot.free_float_shares_raw.map(|value| value.to_string()),
            "source": snapshot.source,
        })),
        "entitlements": entitlements,
    }))
}
