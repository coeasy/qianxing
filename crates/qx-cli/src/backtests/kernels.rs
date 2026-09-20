//! 撮合内核名称常量：产物清单里声明"这条链实际用的是哪个内核"。

/// 产物里声明本次实际使用的撮合内核，避免把深度档说成与 Bar 链同一内核。
pub(crate) const BAR_MATCHING_KERNEL: &str = "qx-xingban::BacktestEngine(bar)";
pub(crate) const TICK_MATCHING_KERNEL: &str = "qx-xingban::TickBacktestEngine(l1-top-of-book)";
pub(crate) const ORDER_BOOK_MATCHING_KERNEL: &str =
    "qx-xingban::OrderBookBacktestEngine(l2-l3-book)";
