//! 撮合内核名称常量：产物清单里声明"这条链实际用的是哪个内核"。
//!
//! 三条回测链给三个名字，不做同义合并：Bar 链用 `BacktestEngine`，L1 用 `TickBacktestEngine`
//! （只吃首档），L2/L3 用 `OrderBookBacktestEngine`（逐档）。还有第四套撮合不在本清单里，
//! 因为它不产出回测产物：Paper 的成交由 `qx-zhenlu` 的 `PaperVenue::on_quote` 对每条 QuoteTick
//! 用首档一次性 touch 产生，与簿内核没有符号耦合（V11 §4.3；把它改接簿内核排在 Q1d）。

/// 产物里声明本次实际使用的撮合内核，避免把深度档说成与 Bar 链同一内核。
pub(crate) const BAR_MATCHING_KERNEL: &str = "qx-xingban::BacktestEngine(bar)";
pub(crate) const TICK_MATCHING_KERNEL: &str = "qx-xingban::TickBacktestEngine(l1-top-of-book)";
pub(crate) const ORDER_BOOK_MATCHING_KERNEL: &str =
    "qx-xingban::OrderBookBacktestEngine(l2-l3-book)";
