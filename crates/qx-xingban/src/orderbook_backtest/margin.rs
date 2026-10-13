/// Spot sizing uses settled cash. Derivative free margin awaits a certified
/// equity and maintenance-margin snapshot.
pub(super) fn settled_cash_margin(is_leveraged: bool, settled_cash_raw: i128) -> Option<i128> {
    (!is_leveraged).then_some(settled_cash_raw.max(0))
}
