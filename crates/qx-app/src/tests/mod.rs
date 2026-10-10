//! `qx-app` 的用例（目录式 `src/tests/`）。
//!
//! 刻意用**目录**而不是单文件 `src/tests.rs`：门禁的 `TEST_PATH` 两种形态都认，但
//! `zero_reference_public_surface_check` 的"生产读者"计数在目录式下会把整个 `tests/`
//! 排除干净——于是"只有用例读过的 `pub fn`"照样被数成零读者。这正是我们想要的：
//! 公共面必须有**生产**读者，用例不算。放在单文件 `src/tests.rs` 里会把这个前提悄悄松开。

mod depth_backtest;
mod error_contract;
mod fixtures;
mod spec_contract;
mod use_cases;
