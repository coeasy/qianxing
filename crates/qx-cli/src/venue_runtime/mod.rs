//! Venue 运行时：CCXT、Binance Spot 与 Paper 三条 worker 链路的执行、行情、
//! 对账与恢复入口。模块只按 Venue 边界切分文件，跨模块可见性由下面的
//! `pub(crate) use` 保持与拆分前一致（其他 qx-cli 模块仍经 crate 根引用这些条目）。

mod binance_reconcile;
mod binance_stream_worker;
mod binance_submit;
mod binance_venue;
mod ccxt_execution;
mod ccxt_live_bars;
mod ccxt_market_worker;
mod ccxt_reconcile_worker;
mod ccxt_stream_retry;
mod paper_submit;
mod paper_worker;
mod worker_runtime;

pub(crate) use binance_reconcile::*;
pub(crate) use binance_stream_worker::*;
pub(crate) use binance_submit::*;
pub(crate) use binance_venue::*;
pub(crate) use ccxt_execution::*;
pub(crate) use ccxt_live_bars::*;
pub(crate) use ccxt_market_worker::*;
pub(crate) use ccxt_reconcile_worker::*;
pub(crate) use ccxt_stream_retry::*;
pub(crate) use paper_submit::*;
pub(crate) use paper_worker::*;
pub(crate) use worker_runtime::*;
