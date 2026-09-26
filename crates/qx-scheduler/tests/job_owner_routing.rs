//! 作业 owner 的路由判据：领取端（Strategy worker）与装配端（运行拓扑）共用的那一个函数。

use qx_scheduler::{claimable_by, JOB_OWNER_ANY};

#[test]
fn wildcard_owner_is_claimable_by_any_worker_id() {
    assert!(claimable_by(JOB_OWNER_ANY, "strategy-paper"));
    assert!(claimable_by(JOB_OWNER_ANY, "strategy-binance"));
}

#[test]
fn exact_owner_is_claimable_only_by_that_worker_id() {
    assert!(claimable_by("strategy-paper", "strategy-paper"));
    assert!(!claimable_by("strategy-paper", "strategy-other"));
    // id 逐字比较：大小写不同就是另一个 worker，不做忽略大小写的兜底。
    assert!(!claimable_by("strategy-PAPER", "strategy-paper"));
}

#[test]
fn owner_that_matches_nothing_is_claimable_by_no_one() {
    assert!(!claimable_by("strategy-nobody", "strategy-paper"));
    assert!(!claimable_by("", "strategy-paper"));
}
