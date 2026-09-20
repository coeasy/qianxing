//! P1a 概念单点化 · `TargetPosition` 单点定义（V10 §4.8）。
//!
//! 唯一定义住在 `qx-core::target`；`qx-portfolio`（调仓计划）与 `qx-zhenlu`
//! （信号归并）都只引用它。反向验证：任一 crate 重新声明本地
//! `pub struct TargetPosition` → 下面的 `TypeId` 断言即红（并且 `qx-protocol`
//! 的 `concept_definitions_are_single_sourced` 也会红）。

use qx_core::{InstrumentId, TargetPosition};
use qx_portfolio::{
    rebalance, PortfolioConstraint, PortfolioState, TargetPosition as PortfolioTargetPosition,
};
use std::any::TypeId;
use std::collections::BTreeMap;

#[test]
fn portfolio_reuses_the_kernel_target_position_type() {
    assert_eq!(
        TypeId::of::<PortfolioTargetPosition>(),
        TypeId::of::<TargetPosition>(),
        "qx-portfolio 不得再定义第二份 TargetPosition，必须引用 qx-core 的唯一定义"
    );
}

#[test]
fn rebalance_plan_emits_the_shared_typed_target() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let plan = rebalance(
        &PortfolioState {
            portfolio_id: "p1".into(),
            timestamp: 1,
            cash: 1_000,
            positions: BTreeMap::from([(instrument.to_string(), 100_i128)]),
        },
        &[TargetPosition {
            instrument: instrument.clone(),
            target_qty: 110,
            source_signals: vec![7],
        }],
        &PortfolioConstraint {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        },
    )
    .unwrap();
    assert_eq!(plan.positions.len(), 1);
    // 调仓计划输出的是增量，且带类型化 instrument；机械推导不得伪造谱系。
    assert_eq!(plan.positions[0].target_qty, 10);
    assert_eq!(plan.positions[0].instrument, instrument);
    assert!(plan.positions[0].source_signals.is_empty());
}

#[test]
fn non_roundtrip_instrument_keys_fail_closed() {
    // PortfolioState 的裸字符串 key 无法解析成类型化 instrument 时必须报错，
    // 不允许悄悄产出一条对不上号的 TargetPosition。
    let error = rebalance(
        &PortfolioState {
            portfolio_id: "p1".into(),
            timestamp: 1,
            cash: 0,
            positions: BTreeMap::from([(String::from("BTCUSDT"), 5_i128)]),
        },
        &[],
        &PortfolioConstraint {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        },
    )
    .unwrap_err();
    assert!(error.contains("instrument"), "实际错误: {error}");
}
