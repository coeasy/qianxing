//! Canonical account EventLog identity shared by service and worker hosts.

use qx_core::VenueFamily;

/// Build the sole log identity for an account and venue pair.
pub fn account_event_log_name(account_id: &str, venue_id: &str) -> Option<String> {
    let account = account_id.trim();
    let venue = venue_id.trim().to_ascii_lowercase();
    if account.is_empty() || venue.is_empty() {
        return None;
    }
    let prefix = match VenueFamily::parse(&venue) {
        VenueFamily::Paper => "paper",
        VenueFamily::Binance => "binance",
        VenueFamily::Other => "ccxt",
    };
    Some(format!("{prefix}-{account}-{venue}-events"))
}

#[cfg(test)]
mod tests {
    use super::account_event_log_name;

    #[test]
    fn account_event_log_identity_normalizes_family_and_rejects_missing_parts() {
        assert_eq!(
            account_event_log_name(" main ", "Paper"),
            Some("paper-main-paper-events".into())
        );
        assert_eq!(
            account_event_log_name("main", "binance-testnet"),
            Some("binance-main-binance-testnet-events".into())
        );
        assert_eq!(
            account_event_log_name("main", "OKX"),
            Some("ccxt-main-okx-events".into())
        );
        assert_eq!(account_event_log_name("", "paper"), None);
        assert_eq!(account_event_log_name("main", "  "), None);
    }
}
