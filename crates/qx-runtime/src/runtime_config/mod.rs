//! 运行时配置装配：schema 定义、确定性校验与配置指纹。
//!
//! 该模块把 API、存储、消息、worker、调度与策略配置校验成一个可审计的进程拓扑。
//! 子模块只按职责切分（类型定义 / 策略实例校验 / 拓扑校验），公开出口仍留在 crate 根。
//! 跨子模块共用的 serde 默认值函数保持私有并集中在本文件，子模块经 `use super::*` 使用。

use crate::*;

fn default_scheduler_state_path() -> String {
    "scheduler.json".into()
}

fn default_scheduler_jobs_path() -> String {
    "scheduler.jobs.json".into()
}

fn default_scheduler_job_queue_path() -> String {
    "job-queue".into()
}

fn default_scheduler_tick_interval_ms() -> u64 {
    1_000
}

fn default_strategy_version() -> String {
    "strategy-runtime-v1".into()
}

fn default_strategy_max_orders() -> u64 {
    1_000
}

fn default_strategy_python_timeout_ms() -> u64 {
    2_000
}

fn default_strategy_live_timeframe() -> String {
    "1m".into()
}

fn default_strategy_live_history_limit() -> usize {
    200
}

fn default_strategy_live_closed_only() -> bool {
    true
}

fn default_strategy_c_abi_max_library_bytes() -> u64 {
    64 * 1024 * 1024
}

fn default_postgres_pool_size() -> usize {
    4
}

fn default_strategy_shared_memory_capacity() -> u32 {
    qx_strategy::DEFAULT_RING_CAPACITY
}

fn default_strategy_shared_memory_slot_bytes() -> u32 {
    qx_strategy::DEFAULT_RING_SLOT_BYTES
}

fn default_risk_rules_version() -> String {
    "risk-rules-cfg-v1".into()
}

mod schema;
#[cfg(test)]
mod schema_tests;
mod strategy_schema;
#[cfg(test)]
mod strategy_tests;
mod strategy_validation;
#[cfg(test)]
mod test_support;
mod topology_validation;
mod validate;

pub use schema::*;
pub use strategy_schema::*;
#[cfg(test)]
pub(crate) use test_support::*;
