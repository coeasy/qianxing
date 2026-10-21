//! Outbox 候选分页的共享排序口径。
//!
//! 可投递事件优先于达到重试上限的停摆事件；两组内部按创建时间、序号、事件 ID
//! 排序。SQL 方言仅在数值转换上不同，排序维度和顺序在这里集中声明。

use crate::{outbox_exhausted, OutboxEvent};
use std::cmp::Ordering;

#[cfg(feature = "sqlite")]
pub(crate) const SQLITE_OUTBOX_PAGE_ORDER_BY: &str =
    "ORDER BY CAST(e.attempts AS INTEGER) >= CAST(?2 AS INTEGER), CAST(e.created_ts AS INTEGER), CAST(e.sequence AS INTEGER), e.event_id";

#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_OUTBOX_PAGE_ORDER_BY: &str =
    "ORDER BY e.attempts::numeric >= $2::text::numeric, e.created_ts::numeric, e.sequence::numeric, e.event_id";

pub(crate) fn compare_outbox_page_order(left: &OutboxEvent, right: &OutboxEvent) -> Ordering {
    outbox_exhausted(left.attempts)
        .cmp(&outbox_exhausted(right.attempts))
        .then_with(|| left.created_ts.cmp(&right.created_ts))
        .then_with(|| left.sequence.cmp(&right.sequence))
        .then_with(|| left.event_id.cmp(&right.event_id))
}
