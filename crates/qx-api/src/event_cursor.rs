//! 事件游标的单一口径（V12 R4-g）。
//!
//! `after` 在这条边界上只有一种解释：**事件序号**。此前 `/events` 按投影日志的
//! **下标**切数组，`/events/live` 按序号过滤，同一个游标会让两条读链给出不同的
//! 事件批次；日志被裁剪过或序号不连续时，下标口径还会把"游标已失效"印成
//! "没有新事件"。两条链因此共用下面这一个实现。

use crate::{query_value, EventBusError};
use qx_core::Event;

/// 游标 → 事件批次的唯一实现：返回 `seq > after` 的那一段。游标超前于日志、或落在
/// 已被裁剪的区间时显式报错，让调用方回到快照重取，而不是悄悄切出一段。
pub(crate) fn events_after_cursor<'a>(
    events: impl Iterator<Item = &'a Event> + Clone,
    next_seq: u64,
    after: Option<u64>,
) -> Result<Vec<Event>, EventBusError> {
    let Some(after) = after else {
        return Ok(events.cloned().collect());
    };
    if after >= next_seq {
        return Err(EventBusError::CursorAhead {
            requested: after,
            next_seq,
        });
    }
    // 空日志没有"被裁掉的区间"，oldest 记 0 让下面的比较恒不成立。
    let oldest = events.clone().next().map_or(0, |event| event.seq);
    if after.saturating_add(1) < oldest {
        return Err(EventBusError::CursorTooOld {
            requested: after,
            oldest,
        });
    }
    Ok(events.filter(|event| event.seq > after).cloned().collect())
}

/// `after=` 的解析（`/events` 与 `/events/live` 共用）：缺省表示"从头给"，
/// 非无符号整数是客户端错误。
pub(crate) fn parse_after_cursor(query: &str) -> Result<Option<u64>, &'static str> {
    match query_value(query, "after") {
        None => Ok(None),
        Some(value) => match value.parse::<u64>() {
            Ok(value) => Ok(Some(value)),
            Err(_) => Err("after must be an unsigned integer"),
        },
    }
}
