//! P1a 概念单点化 · 快照类型不变量（V10 §4.7）。
//!
//! 这些用例在"重复定义回来"时必须变红：
//! 1. `qx-protocol` 再声明一份本地 `AccountPositionSnapshot`/`AccountBalance`
//!    → 与 crate 根的再导出直接冲突（E0252），整个 crate 编译失败。
//! 2. 删除再导出 / `From` 折算层，`qx-cli` 重新用两套别名导入同名快照
//!    → 下面的 TypeId、折算与别名扫描用例红。
//! 3. 任何 crate 里再次出现第二处 `pub struct TargetPosition` / `PositionSnapshot`
//!    / `AccountPositionSnapshot` / `AccountBalance`
//!    → `concept_definitions_are_single_sourced` 红。

use qx_core::{
    AccountBalance, AccountPositionSnapshot, Fill, InstrumentId, Money, Order, OrderStatus, Price,
    Quantity, Side, Ts,
};
use qx_protocol::{
    AccountSnapshot, FillSnapshot, OrderSnapshot, PositionSnapshot, TransferSnapshot,
};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod optional_money;
mod single_source;
mod stable_json_tables;
