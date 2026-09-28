//! # qx-genglu — 更路
//!
//! 对账判定。更路簿是明清航海者记录航线、针位、更数的手抄本——用它命名
//! "本地事实与柜台事实的逐条比对"是精准的：两条记录必须对上，对不上的
//! 地方交给人核，任何一方都不得单方面改写对方。
//!
//! 本 crate 只承载全仓唯一的一份**订单维度对账裁决**（V10 §4.8）。以下
//! 相邻概念刻意不放在这里，避免同一问题出现第二份答案：
//! - 绩效指标（收益、回撤、费用合计）由撮合引擎在同一处算出，见
//!   `qx-xingban` 回测报告的 `return_bps` / `max_drawdown_bps`；
//! - 成交归因由多腿归因链（`qx-cli` 的 `multi_leg_group_attributions`）与
//!   `Fill` 自带的 `strategy_id` / `signal_id` / `intent_id` 承担；
//! - 现金对账由运行时管线 `qx-runtime` 的 `settlement_balance_discrepancies`
//!   承担，它读的是账户账本与柜台余额事实，不在这里重复实现。

mod reconcile;
pub use reconcile::*;
