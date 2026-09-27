//! CCXT 子进程通道的重连预算：连续失败计数、递增退避与具名放弃。
//!
//! 口径与 Binance 用户流一致（V12 R4-c）：终态判定只看**连续**失败次数，拿到一次成功的
//! 应答就清零；退避公式一律走 `qx-core::retry`，不在连接器里就地重算。
//! 用户流（`watch_orders`）与行情 RPC（`fetch_ticker`/`fetch_ohlcv`）共用这一份预算，
//! 只按 `subject` 区分放弃时点名的那条通道。

use qx_core::retry::{Backoff, RetryPolicy};
use std::time::Duration;

/// 一条 CCXT 子进程通道的重连预算。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CcxtReconnectBudget {
    policy: RetryPolicy,
    consecutive_failures: u32,
    /// 放弃时报错点名的通道：两条通道共用上限与退避，混在一条文案里会让运维去用户流
    /// 日志里找一条行情链的错。
    subject: &'static str,
}

impl Default for CcxtReconnectBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl CcxtReconnectBudget {
    pub(crate) const MAX_RECONNECTS: u32 = 10;
    const BASE_DELAY: Duration = Duration::from_millis(500);
    const MAX_DELAY: Duration = Duration::from_secs(8);
    const USER_STREAM: &'static str = "CCXT Pro 用户流";
    const MARKET_RPC: &'static str = "CCXT 行情子进程";

    /// `Default` 走同一条路径：固定 500ms 起、8s 封顶、最多 10 次连续重连。
    pub(crate) const fn new() -> Self {
        Self::with_subject(Self::USER_STREAM)
    }

    /// 行情 worker 的 `fetch_ticker`/`fetch_ohlcv` 通道：同一份预算，换一条放弃文案。
    pub(crate) const fn market_rpc() -> Self {
        Self::with_subject(Self::MARKET_RPC)
    }

    const fn with_subject(subject: &'static str) -> Self {
        Self {
            policy: RetryPolicy::new(
                Self::MAX_RECONNECTS,
                Backoff::exponential(Self::BASE_DELAY, Self::MAX_DELAY),
            ),
            consecutive_failures: 0,
            subject,
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
                "{}连续 {} 次重连仍失败，超过上限 {}",
                self.subject,
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

/// 子进程对 `wait_ms` 窗口的回话是否表示"接得上话、这一窗没有订单事件"。
///
/// 单独成点是因为这个键跨语言（Python 侧在 `qianxing_ccxt/worker.py` 写
/// `event["idle"] = True`）。读错键名的两种失败方式在日志里看不出区别：把空闲
/// 当断链会吃掉重连预算，把空闲当交付会替坏链路复位预算——两者都会让当天没有
/// 成交的账户在几个窗口后被具名放弃。
pub(crate) fn ccxt_watch_reply_is_idle(event: &serde_json::Value) -> bool {
    event.get("idle").and_then(serde_json::Value::as_bool) == Some(true)
}
