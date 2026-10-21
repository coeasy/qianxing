use super::{BookLevel, OrderBookSnapshot};
use qx_core::{Price, Side};

/// Locate a passive order's displayed queue level in the validated side of a book.
/// Buy-side bids are descending; sell-side asks are ascending.
pub(super) fn displayed_qty_at_limit(
    snapshot: &OrderBookSnapshot,
    limit: Price,
    side: Side,
) -> Option<i128> {
    let levels: &[BookLevel] = match side {
        Side::Buy => &snapshot.bids,
        Side::Sell => &snapshot.asks,
    };
    let index = match side {
        Side::Buy => levels.binary_search_by(|level| limit.raw().cmp(&level.price.raw())),
        Side::Sell => levels.binary_search_by(|level| level.price.raw().cmp(&limit.raw())),
    };
    index.ok().map(|index| levels[index].qty.raw())
}
