//! 跨语言策略进程契约：只读输入、Bar 列式编码、订单意图输出与决策上下文。
//!
//! 该模块只定义进程边界上的稳定数据契约与确定性校验，不触碰任何交易事实；
//! 子模块按“契约载荷 / 决策上下文 / 契约用例”切分，公开出口仍留在 crate 根。

use crate::*;

mod context;
mod contract;
#[cfg(test)]
mod tests;

pub use context::*;
pub use contract::*;
