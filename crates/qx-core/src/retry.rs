//! 统一的重试 / 退避策略（V10 §4.9）。
//!
//! 此前框架里“该不该重试、下一次什么时候重试”被反复就地推导：适配器各写一套
//! 指数退避公式，调度器各写一套固定间隔，存储侧各写一套尝试计数。本模块把三件事
//! （尝试计数、最大次数判定、下一次延迟计算）收敛为唯一一份实现；退避的“形状”
//! 是数据（[`Backoff`] 枚举），不再是各处的重复结构体：
//!
//! - 指数：`base × 2^(attempt-1)`，封顶 `cap`（原 `BinanceStreamRetryPolicy` 语义）；
//! - 固定：任意一次重试的延迟恒为 `interval`（原 `qx-scheduler::RetryPolicy` 语义）。
//!
//! 热路径纪律与本 crate 其余模块一致：不读系统时钟，时间一律由调用方注入；
//! 全部判定都是纯函数，回测/故障注入可以确定性复现。

use std::time::Duration;

/// 退避形状：唯一允许出现“延迟怎么算”公式的地方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backoff {
    /// 固定间隔：第几次尝试不影响下一次延迟。
    Fixed { interval: Duration },
    /// 指数退避：第 `n` 次（从 1 起）尝试前等待 `base × 2^(n-1)`，封顶 `cap`。
    Exponential { base: Duration, cap: Duration },
}

impl Backoff {
    /// 固定间隔退避（调度器形状）。
    pub const fn fixed(interval: Duration) -> Self {
        Self::Fixed { interval }
    }

    /// 指数退避（连接器重连形状）。
    pub const fn exponential(base: Duration, cap: Duration) -> Self {
        Self::Exponential { base, cap }
    }

    /// 第 `attempt` 次尝试（从 1 起）之前的延迟。参数按 1 起算：`attempt=1`
    /// 即首次重试等待 `base`。`attempt=0` 与 `attempt=1` 等价（饱和减法）。
    pub fn delay_before_attempt(&self, attempt: u32) -> Duration {
        match self {
            Self::Fixed { interval } => *interval,
            Self::Exponential { base, cap } => {
                let exponent = attempt.saturating_sub(1).min(31);
                let multiplier = 1_u128 << exponent;
                let millis = base
                    .as_millis()
                    .saturating_mul(multiplier)
                    .min(cap.as_millis());
                Duration::from_millis(millis as u64)
            }
        }
    }
}

/// 一次重试的完整判定：尝试计数 + 上限判定 + 下一次延迟。
///
/// `max_attempts` 语义为“允许的尝试总次数”（含首次）。已经失败
/// `failed_attempts` 次后是否还要再来一次，用 [`RetryPolicy::should_retry`]；
/// 第 `attempt` 次（从 1 起）什么时候开始，用 [`RetryPolicy::delay_before_attempt`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    backoff: Backoff,
}

impl RetryPolicy {
    pub const fn new(max_attempts: u32, backoff: Backoff) -> Self {
        Self {
            max_attempts,
            backoff,
        }
    }

    /// 不做退避、只判断是否还能再试的策略（存储 outbox/consumer 计数形状）。
    pub const fn attempts_only(max_attempts: u32) -> Self {
        Self::new(max_attempts, Backoff::fixed(Duration::from_secs(0)))
    }

    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub const fn backoff(&self) -> Backoff {
        self.backoff
    }

    /// 已经失败 `failed_attempts` 次后是否仍允许再发起一次尝试。
    pub const fn should_retry(&self, failed_attempts: u32) -> bool {
        failed_attempts < self.max_attempts
    }

    /// 第 `attempt` 次尝试（从 1 起）之前的延迟。
    pub fn delay_before_attempt(&self, attempt: u32) -> Duration {
        self.backoff.delay_before_attempt(attempt)
    }

    /// 尝试计数的唯一递增口径：记录一次已发生的尝试（饱和，不溢出）。
    pub fn next_attempt_count(attempts: u32) -> u32 {
        attempts.saturating_add(1)
    }

    /// 以秒级逻辑时钟（调度器/存储使用的时间戳）计算下一次重试时刻；
    /// 已到达终态时返回 `None`。延迟换算为整秒后饱和相加。
    pub fn next_retry_ts(&self, finished_ts: u64, failed_attempts: u32) -> Option<u64> {
        self.should_retry(failed_attempts).then(|| {
            let seconds = self
                .delay_before_attempt(failed_attempts.saturating_add(1))
                .as_secs();
            finished_ts.saturating_add(seconds)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_matches_legacy_binance_schedule() {
        // 原 BinanceStreamRetryPolicy::delay_for 的逐点语义：base×2^(n-1) 封顶 cap。
        let backoff = Backoff::exponential(Duration::from_secs(1), Duration::from_secs(30));
        assert_eq!(backoff.delay_before_attempt(1), Duration::from_secs(1));
        assert_eq!(backoff.delay_before_attempt(2), Duration::from_secs(2));
        assert_eq!(backoff.delay_before_attempt(5), Duration::from_secs(16));
        assert_eq!(backoff.delay_before_attempt(6), Duration::from_secs(30));
        assert_eq!(backoff.delay_before_attempt(64), Duration::from_secs(30));
        // exponent 饱和在 31、attempt=0 与 1 等价（saturating_sub 边界）。
        assert_eq!(backoff.delay_before_attempt(0), Duration::from_secs(1));
        let large = Backoff::exponential(Duration::from_millis(700), Duration::from_secs(10));
        assert_eq!(large.delay_before_attempt(33), Duration::from_secs(10));
    }

    #[test]
    fn fixed_matches_legacy_scheduler_schedule() {
        let backoff = Backoff::fixed(Duration::from_secs(10));
        for attempt in [0, 1, 2, 9, u32::MAX] {
            assert_eq!(
                backoff.delay_before_attempt(attempt),
                Duration::from_secs(10)
            );
        }
    }

    #[test]
    fn attempt_boundaries_and_terminal_match_legacy_sites() {
        // 调度器：max_attempts=3，attempt(1 起) >= 3 时终态 <=> should_retry(attempt)。
        let scheduler = RetryPolicy::new(3, Backoff::fixed(Duration::from_secs(10)));
        assert!(scheduler.should_retry(1));
        assert!(scheduler.should_retry(2));
        assert!(!scheduler.should_retry(3));
        assert_eq!(scheduler.next_retry_ts(100, 2), Some(110));
        assert_eq!(scheduler.next_retry_ts(100, 3), None);
        assert_eq!(scheduler.next_retry_ts(u64::MAX - 1, 1), Some(u64::MAX));
        // 适配器：第 k 次重连允许 <=> k <= max_reconnects <=> should_retry(k-1)。
        let reconnect = RetryPolicy::new(
            10,
            Backoff::exponential(Duration::from_secs(1), Duration::from_secs(30)),
        );
        assert!(reconnect.should_retry(0));
        assert!(reconnect.should_retry(9));
        assert!(!reconnect.should_retry(10));
        assert_eq!(reconnect.delay_before_attempt(10), Duration::from_secs(30));
        // 存储：尝试计数饱和递增。
        assert_eq!(RetryPolicy::next_attempt_count(u32::MAX - 1), u32::MAX);
        assert_eq!(RetryPolicy::next_attempt_count(u32::MAX), u32::MAX);
    }
}
