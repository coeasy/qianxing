//! 本仓写下的 NATS 等待预算。
//!
//! async-nats 0.50 对这几条等待各有默认值：握手 `connection_timeout` 5s、
//! 连接层 `request_timeout`（`$JS.API.*` 请求走这条）10s、`Context` 的发布确认
//! 超时 5s；有界拉取的 batch expiry 则必须由调用方给出。这些默认值不在本仓的配置
//! 文件、日志或校验里出现，运维改不动、判据也看不见，所以这里把三个数显式化：
//! 连接与拉取两项与被替换的依赖默认逐项相等，`request_timeout_ms` 一律取 5s，
//! 把原来的 10s 请求默认与 5s 确认默认并成一格（收紧，不放宽）。
//!
//! 下界一律取 100ms 而不是 1：拉取 expiry 或握手预算被打成毫秒级时，worker 循环会
//! 变成对 broker 的热循环（与 `messaging.consumer_handler_timeout_ms` 的下界同一条教训）。

use std::time::Duration;

/// 建立连接、JetStream 请求/发布确认、有界拉取三条等待的毫秒预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NatsWaitBudget {
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub pull_expires_ms: u64,
}

/// 三条预算共用的下界（防热循环）。
const MIN_MS: u64 = 100;
/// 握手预算上界：一次 connect 最多让 worker 停在启动阶段 1 分钟。
const MAX_CONNECT_MS: u64 = 60_000;
/// 请求/确认预算上界：relay 每个批次最多付出一批 publish 的这个价钱。
const MAX_REQUEST_MS: u64 = 300_000;
/// 拉取预算上界：必须显著小于 worker 的停机观察粒度。
const MAX_PULL_MS: u64 = 30_000;

impl Default for NatsWaitBudget {
    /// 连接与拉取沿用 async-nats 0.50 的 `ConnectOptions::default()`
    /// （connection_timeout 5s）与本仓此前写死的 1s 拉取 expiry；请求/确认取 5s，
    /// 同时替换依赖的 10s 请求默认与 5s 确认默认。
    fn default() -> Self {
        Self {
            connect_timeout_ms: 5_000,
            request_timeout_ms: 5_000,
            pull_expires_ms: 1_000,
        }
    }
}

impl NatsWaitBudget {
    /// 校验三条预算都落在本仓允许区间内，返回的错误串只含裸字段名，由配置校验方
    /// 加上它在配置文件里的路径前缀。
    pub fn validate(&self) -> Result<(), String> {
        for (field, value, max) in [
            (
                "connect_timeout_ms",
                self.connect_timeout_ms,
                MAX_CONNECT_MS,
            ),
            (
                "request_timeout_ms",
                self.request_timeout_ms,
                MAX_REQUEST_MS,
            ),
            ("pull_expires_ms", self.pull_expires_ms, MAX_PULL_MS),
        ] {
            if !(MIN_MS..=max).contains(&value) {
                return Err(format!("{field} 必须在 {MIN_MS}..={max} 内"));
            }
        }
        Ok(())
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }

    pub fn pull_expires(&self) -> Duration {
        Duration::from_millis(self.pull_expires_ms)
    }
}
