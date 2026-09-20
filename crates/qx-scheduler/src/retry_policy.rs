//! 调度侧重试策略（V10 §4.9）。
//!
//! [`RetryPolicy`] 保持既有的持久化 JSON 形状不变（字段与 serde 表示逐字保留），
//! 但“该不该重试、下一次什么时候重试”不再在本文件就地推导：判定与延迟计算全部
//! 委托给 `qx-core::retry` 的统一策略。调度器的形状是固定间隔
//! （fixed-interval）：任意一次失败后的重试延迟恒为 `backoff_seconds`。

use qx_core::retry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Duration;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_seconds: u64,
    pub retryable_codes: BTreeSet<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_seconds: 0,
            retryable_codes: BTreeSet::new(),
        }
    }
}

impl RetryPolicy {
    /// 折算成 `qx-core` 统一策略：固定间隔形状，`max_attempts` 即允许的尝试总数。
    pub fn unified(&self) -> retry::RetryPolicy {
        retry::RetryPolicy::new(
            self.max_attempts,
            retry::Backoff::fixed(Duration::from_secs(self.backoff_seconds)),
        )
    }

    /// 已经失败 `attempts` 次后是否仍允许再次调度。
    pub fn should_retry(&self, attempts: u32) -> bool {
        self.unified().should_retry(attempts)
    }

    /// 一次失败结束于 `finished_ts` 后的下次重试时刻（等价于旧 `finished_ts + backoff_seconds`）。
    pub fn retry_deadline(&self, finished_ts: u64) -> u64 {
        finished_ts.saturating_add(self.unified().delay_before_attempt(1).as_secs())
    }
}
