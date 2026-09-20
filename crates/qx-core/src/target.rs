//! 目标仓位：策略信号经组合层净额后的唯一目标表示（V10 §4.8 概念单点化）。
//!
//! 历史上 `qx-zhenlu` 与 `qx-portfolio` 各定义了一份 `TargetPosition`，字段与
//! 职责逐步漂移。本模块是唯一权威定义：`qx-zhenlu`（信号归并）与
//! `qx-portfolio`（调仓计划）都只引用这里的类型，不再各自持有一份。

use crate::InstrumentId;
use serde::{Deserialize, Serialize};

/// 组合层净额后的目标仓位。
///
/// `source_signals` 保留审计谱系：任何一条由目标仓位推导出的订单意图都必须
/// 能回溯到贡献它的信号；纯机械推导（如兼容路径）填空列表，不得伪造来源。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TargetPosition {
    pub instrument: InstrumentId,
    pub target_qty: i128,
    pub source_signals: Vec<u64>,
}

impl TargetPosition {
    /// 单目标便捷构造：无信号谱系（如策略契约兼容路径的直接目标）。
    pub fn single(instrument: InstrumentId, target_qty: i128) -> Self {
        Self {
            instrument,
            target_qty,
            source_signals: Vec::new(),
        }
    }
}
