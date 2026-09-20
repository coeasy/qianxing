//! worker 进程监督：确定性健康状态、停机信号与 worker 生命周期。
//!
//! 健康快照只由显式上报的状态推导，不读取任何账户事实；停机信号是单向的
//! 原子标志，保证重复调用 `request_shutdown` 仍然幂等。

use crate::*;

mod health;
mod supervisor;
#[cfg(test)]
mod tests;

pub use health::*;
pub use supervisor::*;
