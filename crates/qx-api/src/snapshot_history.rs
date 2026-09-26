//! `/account/snapshot/diff?base_hash=…` 的历史基线表。
//!
//! 读模型每 250 ms 重装一次账户快照，账户每变一次就是一个新的 `state_hash`。这张表
//! 过去只进不出：一个跑几十天的 API 进程会一路持有该账户自启动以来的每一份完整快照
//! （余额、持仓、订单与成交全在里面），而它本来只需要回答"最近这份与某份基线差在哪"。
//!
//! 上界按刷新节奏取整：1_024 份约合 4 分钟的基线可回看窗口。被退场的 base 不需要新
//! 错误——`deploy/README.md` 早已把 409 `snapshot_base_not_found` 写成这条端点的一等
//! 回答，最坏情况退化成一次全量重取，而不是把增量比对变成静默给出错误的差异。
//!
//! 退场顺序按**最近写入**排，不是首次写入：同一个摘要被重装时先挪回队尾，否则安静账户
//! 里唯一那一份基线会永远排在队首，下一轮波动就把客户端正在用的那一格退掉。

use qx_protocol::AccountSnapshot;

use std::collections::{BTreeMap, VecDeque};

/// 单份读模型常驻的快照条数上限。250 ms 的刷新节奏下约合 4 分钟的基线可回看窗口。
pub(super) const MAX_SNAPSHOT_HISTORY: usize = 1_024;

/// 按写入顺序封顶的快照表：`base_hash` 命中即返回，越限退最旧的一份。
#[derive(Default)]
pub(super) struct SnapshotHistory {
    by_hash: BTreeMap<u64, AccountSnapshot>,
    /// 写入顺序，队首最旧。同一个摘要被重装时把它挪回队尾——否则安静账户里唯一那一份
    /// 基线会排在队首，下一轮波动就把刚刚还在被读的那一格退掉。
    insertion: VecDeque<u64>,
}

impl SnapshotHistory {
    pub(super) fn insert(&mut self, hash: u64, snapshot: AccountSnapshot) {
        if let Some(position) = self.insertion.iter().position(|stored| *stored == hash) {
            self.insertion.remove(position);
        }
        self.insertion.push_back(hash);
        self.by_hash.insert(hash, snapshot);
        while self.by_hash.len() > MAX_SNAPSHOT_HISTORY {
            let Some(oldest) = self.insertion.pop_front() else {
                break;
            };
            self.by_hash.remove(&oldest);
        }
    }

    pub(super) fn get(&self, hash: &u64) -> Option<&AccountSnapshot> {
        self.by_hash.get(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: u64) -> AccountSnapshot {
        AccountSnapshot::new(id, "main", "default", "paper", id * 1_000)
    }

    /// 越限只退最旧的那一份，且容量是硬上界：重装同一份摘要不能把窗口撑大。
    #[test]
    fn republishing_the_same_snapshot_never_grows_the_window() {
        let mut history = SnapshotHistory::default();
        for id in 0..MAX_SNAPSHOT_HISTORY as u64 {
            history.insert(id, snapshot(id));
        }
        assert_eq!(history.by_hash.len(), MAX_SNAPSHOT_HISTORY);
        for _ in 0..MAX_SNAPSHOT_HISTORY {
            history.insert(7, snapshot(7));
        }
        assert_eq!(
            history.by_hash.len(),
            MAX_SNAPSHOT_HISTORY,
            "同一份摘要重装 1,024 次把窗口撑大，就是修前那个只进不出的形状"
        );
        assert_eq!(
            history.insertion.len(),
            MAX_SNAPSHOT_HISTORY,
            "只收 `by_hash` 不收写入顺序，窗口就会一边宣称有界一边留着全部历史摘要"
        );
    }

    /// 重装把摘要挪回队尾：安静账户里那份唯一基线正是客户端在用的 base，不能排在队首被退。
    #[test]
    fn republishing_keeps_the_live_baseline_at_the_newest_end() {
        let mut history = SnapshotHistory::default();
        for id in 0..MAX_SNAPSHOT_HISTORY as u64 {
            history.insert(id, snapshot(id));
        }
        assert_eq!(
            history.insertion.front(),
            Some(&0),
            "满窗时最早那份排在队首"
        );
        history.insert(0, snapshot(0));
        assert_eq!(
            history.insertion.back(),
            Some(&0),
            "重装之后它该挪到最新那一端"
        );
        for id in MAX_SNAPSHOT_HISTORY as u64..MAX_SNAPSHOT_HISTORY as u64 + 8 {
            history.insert(id, snapshot(id));
        }
        assert!(
            history.get(&0).is_some(),
            "刚被重装过的基线仍然被退场，就是那条把增量比对打成全量重取的路径"
        );
        assert!(
            history.get(&1).is_none(),
            "退的该是最久没被写过的那一份，而不是刚刚读过的那一份"
        );
    }

    /// 窗口内读得到、窗口外退得掉：上界是硬上界，不是软提醒。
    #[test]
    fn the_window_holds_the_newest_snapshots_and_forgets_the_oldest() {
        let mut history = SnapshotHistory::default();
        for id in 0..MAX_SNAPSHOT_HISTORY as u64 + 8 {
            history.insert(id, snapshot(id));
        }
        assert_eq!(history.by_hash.len(), MAX_SNAPSHOT_HISTORY);
        assert_eq!(history.insertion.len(), MAX_SNAPSHOT_HISTORY);
        for id in 0..8u64 {
            assert!(history.get(&id).is_none(), "最旧的 {id} 份该已经退场");
        }
        for id in 8..MAX_SNAPSHOT_HISTORY as u64 + 8 {
            assert!(history.get(&id).is_some(), "窗口内的 {id} 份该还读得到");
        }
    }
}
