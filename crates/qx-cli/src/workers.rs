//! CLI Worker 实现边界。
//!
//! 这里保留 CLI 的配置/进程入口兼容性，但把 Scheduler 和 Strategy 的业务循环
//! 从 `main.rs` 抽出。具体执行仍复用 Runtime、Storage、Control 和 Execution
//! 公共契约，不在模块内创建第二套队列或订单状态。

mod strategy_orders;
pub(crate) use strategy_orders::{
    build_strategy_contract_intent_order, build_strategy_target_order,
};

mod scheduler_worker;
mod strategy_worker;
pub(crate) use scheduler_worker::run_scheduler_worker;
pub(crate) use strategy_worker::run_strategy_worker;
