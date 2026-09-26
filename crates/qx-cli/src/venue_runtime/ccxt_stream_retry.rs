//! CCXT Pro 用户流重连预算：连续失败计数、递增退避与具名放弃。
//!
//! 口径与 Binance 用户流一致（V12 R4-c）：终态判定只看**连续**失败次数，拿到一次成功的
//! `watch_orders` 应答就清零；退避公式一律走 `qx-core::retry`，不在连接器里就地重算。

use qx_core::retry::{Backoff, RetryPolicy};
use std::time::Duration;

/// 一个 CCXT Pro `watch_orders` 会话的重连预算。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CcxtStreamReconnectBudget {
    policy: RetryPolicy,
    consecutive_failures: u32,
}

impl Default for CcxtStreamReconnectBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl CcxtStreamReconnectBudget {
    pub(crate) const MAX_RECONNECTS: u32 = 10;
    const BASE_DELAY: Duration = Duration::from_millis(500);
    const MAX_DELAY: Duration = Duration::from_secs(8);

    /// `Default` 走同一条路径：固定 500ms 起、8s 封顶、最多 10 次连续重连。
    pub(crate) const fn new() -> Self {
        Self {
            policy: RetryPolicy::new(
                Self::MAX_RECONNECTS,
                Backoff::exponential(Self::BASE_DELAY, Self::MAX_DELAY),
            ),
            consecutive_failures: 0,
        }
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// 记下一次会话失败：返回重连前该等多久；超过上限时给出具名放弃原因。
    pub(crate) fn note_failure(&mut self) -> Result<Duration, String> {
        self.consecutive_failures = RetryPolicy::next_attempt_count(self.consecutive_failures);
        if !self.policy.should_retry(self.consecutive_failures - 1) {
            return Err(format!(
                "CCXT Pro 用户流连续 {} 次重连仍失败，超过上限 {}",
                self.consecutive_failures,
                Self::MAX_RECONNECTS
            ));
        }
        Ok(self.policy.delay_before_attempt(self.consecutive_failures))
    }

    /// 会话成功应答：连续失败计数清零。
    pub(crate) fn note_success(&mut self) {
        self.consecutive_failures = 0;
    }
}
