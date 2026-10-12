//! P1a 概念单点化 · `TargetPosition` 单点定义（V10 §4.8）。
//!
//! `qx-zhenlu` 的 `SignalMerger` / `rebalance_intent` 必须消费 `qx-core` 里那份
//! 唯一定义。反向验证：`qx-zhenlu` 重新声明本地 `pub struct TargetPosition` →
//! `TypeId` 断言红；`rebalance_intent` 不再接受归并结果 → 编译失败。

use qx_core::{InstrumentId, TargetPosition};
use qx_zhenlu::{rebalance_intent, Signal, SignalMerger, TargetPosition as ZhenluTargetPosition};
use std::any::TypeId;

fn signal(instrument: &str, id: u64, qty: i128, priority: i32) -> Signal {
    Signal {
        strategy_id: format!("s{id}"),
        signal_id: id,
        instrument: InstrumentId::parse(instrument).expect("valid instrument"),
        target_qty: qty,
        confidence: 1_000,
        priority,
        expires_at: 0,
    }
}

#[test]
fn zhenlu_reuses_the_kernel_target_position_type() {
    assert_eq!(
        TypeId::of::<ZhenluTargetPosition>(),
        TypeId::of::<TargetPosition>(),
        "qx-zhenlu 不得再定义第二份 TargetPosition，必须引用 qx-core 的唯一定义"
    );
}

#[test]
fn merged_targets_feed_the_intent_builder_and_keep_provenance() {
    let instrument = InstrumentId::parse("BTCUSDT-PERP.BINANCE").unwrap();
    let merged = SignalMerger
        .merge(
            vec![
                signal("BTCUSDT-PERP.BINANCE", 11, 4, 5),
                signal("BTCUSDT-PERP.BINANCE", 12, -1, 5),
            ],
            100,
        )
        .expect("信号目标和应在整数范围内");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].target_qty, 3);
    assert_eq!(merged[0].source_signals, vec![11, 12]);
    assert_eq!(merged[0].instrument, instrument);

    // 归并结果直接喂给订单意图构造器：类型必须完全一致，否则这里编译不过。
    let intent = rebalance_intent(&merged[0], 1, "s11", "main", 1, 100).expect("delta 非零");
    assert_eq!(intent.instrument, instrument);
    assert_eq!(intent.signal_id, Some(11));
    assert_eq!(intent.rule_version, "default");
}

#[test]
fn opposing_signals_are_netted_deterministically() {
    let targets = SignalMerger
        .merge(vec![signal("T.V", 2, -3, 1), signal("T.V", 1, 10, 1)], 1)
        .unwrap();
    assert_eq!(targets[0].target_qty, 7);
    assert_eq!(targets[0].source_signals, vec![1, 2]);
}

#[test]
fn duplicate_signal_replay_does_not_change_target() {
    let value = signal("T.V", 7, 10, 1);
    let targets = SignalMerger.merge(vec![value.clone(), value], 1).unwrap();
    assert_eq!(targets[0].target_qty, 10);
    assert_eq!(targets[0].source_signals, vec![7]);
}

#[test]
fn signal_target_overflow_is_rejected_instead_of_saturated() {
    let error = SignalMerger
        .merge(
            vec![signal("T.V", 1, i128::MAX, 1), signal("T.V", 2, 1, 0)],
            1,
        )
        .unwrap_err();
    assert!(matches!(error, qx_core::QxError::BusinessViolation(_)));
    assert!(error.message().contains("目标仓位溢出"));
}
