use super::*;
use qx_core::InstrumentId;

mod sizing_tests;
use std::collections::BTreeMap;

fn context(instrument: &InstrumentId) -> StrategyContext {
    StrategyContext {
        strategy_id: "builtin".into(),
        strategy_version: "builtin-v1".into(),
        account_id: "main".into(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: "bars-v1".into(),
        as_of: 1,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000),
        risk_state: "ready".into(),
    }
}

#[test]
fn registry_contains_seventeen_builtin_strategies() {
    assert_eq!(BuiltinStrategyKind::ALL.len(), 17);
    assert_eq!(
        BuiltinStrategyKind::parse("EMA-CROSS").unwrap(),
        BuiltinStrategyKind::EmaCross
    );
}

/// `ema` 的直流增益必须是 1，且快线必须落在趋势的同侧：这两条不成立时
/// `ema_cross` / `macd` 的快慢线相对位置会随窗口大小漂移，金叉死叉无从判定。
#[test]
fn ema_keeps_unit_dc_gain_and_lags_on_the_trend_side() {
    use qx_core::SCALE;
    for window in [1usize, 2, 5, 12, 20, 26] {
        let flat = vec![7 * SCALE; window + 40];
        assert_eq!(
            ema(&flat, window).unwrap(),
            7 * SCALE,
            "window={window} 的常数序列必须收敛到常数本身"
        );
    }
    let rising: Vec<i128> = (0..40)
        .map(|index| i128::from(100 + index) * SCALE)
        .collect();
    assert!(
        ema(&rising, 5).unwrap() > ema(&rising, 20).unwrap(),
        "上涨段的快线必须在慢线之上"
    );
    let falling: Vec<i128> = rising.iter().rev().copied().collect();
    assert!(
        ema(&falling, 5).unwrap() < ema(&falling, 20).unwrap(),
        "下跌段的快线必须在慢线之下"
    );
}

/// 历史窗口上限必须容得下每个策略自己的信号门槛：MACD 需要 35 根 Bar，
/// 而上限一度只按 `slow_window`/`period` 算出 32 根，于是这条策略在任意长度的
/// 序列上都永远不信号——示例回测的 `fills=0` 就是这么来的。
#[test]
fn macd_signals_within_the_rolling_history_cap() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Macd,
        "builtin-macd".to_string(),
        instrument.clone(),
        Quantity::from_i64(1),
    )
    .unwrap();
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    assert!(
        strategy.max_history() >= strategy.required_bars(),
        "历史上限 {} 裁掉了 MACD 门槛所需的 {} 根",
        strategy.max_history(),
        strategy.required_bars()
    );
    let mut context = context(&instrument);
    let mut intents = 0_usize;
    for ts in 1..=70_u64 {
        // 价格按定点刻度放大：`ema` 全程整数截断，未放大的百位数会让快慢线
        // 之差恒为 0，用例测的就是"永远不交叉"而不是历史上限。
        let close = if ts <= 40 {
            100 + i128::from(40 - ts)
        } else {
            100 + i128::from(ts - 40)
        } * qx_core::SCALE;
        context.as_of = ts;
        let decision = strategy
            .on_event(
                &context,
                &MarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts,
                    open_raw: close,
                    high_raw: close + 1,
                    low_raw: close - 1,
                    close_raw: close,
                    volume_raw: 1,
                },
            )
            .unwrap();
        intents += decision.intents.len();
    }
    assert!(intents > 0, "V 形序列下 MACD 必须至少发出一条委托意图");
}

#[test]
fn momentum_emits_a_target_order_after_warmup() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = BuiltinStrategyConfig {
        kind: BuiltinStrategyKind::Momentum,
        strategy_id: "momentum".into(),
        strategy_version: "momentum-v1".into(),
        instrument: instrument.clone(),
        quantity: Quantity::from_i64(1),
        target_notional_raw: None,
        primary_market_spec: None,
        reference_market_spec: None,
        target_available_margin_bps: None,
        target_volatility_bps: None,
        target_kelly_fraction_bps: None,
        kelly_lookback_bars: None,
        fast_window: 2,
        slow_window: 3,
        period: 2,
        threshold_bps: 10,
        reference_instrument: None,
        primary_policy: None,
        reference_policy: None,
    };
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    for (ts, close) in [(1, 100), (2, 100), (3, 101)] {
        context.as_of = ts;
        let decision = strategy
            .on_event(
                &context,
                &MarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts,
                    open_raw: close,
                    high_raw: close,
                    low_raw: close,
                    close_raw: close,
                    volume_raw: 1,
                },
            )
            .unwrap();
        if ts == 3 {
            assert_eq!(decision.intents.len(), 1);
            assert_eq!(decision.intents[0].side, Side::Buy);
        }
    }
}

#[test]
fn fixed_notional_sizing_uses_visible_price_and_emits_scaled_quantity() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let notional_raw = 100 * qx_core::SCALE;
    let mut config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Momentum,
        "momentum-notional",
        instrument.clone(),
        Quantity::from_i64(999),
    )
    .unwrap();
    config.fast_window = 2;
    config.slow_window = 3;
    config.period = 2;
    config.threshold_bps = 10;
    config.target_notional_raw = Some(notional_raw);
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    let mut final_decision = None;
    for (ts, close) in [(1, 100), (2, 100), (3, 101)] {
        context.as_of = ts;
        let decision = strategy
            .on_event(
                &context,
                &MarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts,
                    open_raw: close * qx_core::SCALE,
                    high_raw: close * qx_core::SCALE,
                    low_raw: close * qx_core::SCALE,
                    close_raw: close * qx_core::SCALE,
                    volume_raw: qx_core::SCALE,
                },
            )
            .unwrap();
        final_decision = Some(decision);
    }
    let intent = &final_decision.unwrap().intents[0];
    assert_eq!(intent.side, Side::Buy);
    assert_eq!(
        intent.qty.raw(),
        notional_raw * qx_core::SCALE / (101 * qx_core::SCALE)
    );
    assert_ne!(intent.qty.raw(), Quantity::from_i64(999).raw());
}

#[test]
fn available_margin_sizing_uses_a_fresh_context_snapshot() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Momentum,
        "momentum-margin-budget",
        instrument.clone(),
        Quantity::from_i64(999),
    )
    .unwrap();
    config.fast_window = 2;
    config.slow_window = 3;
    config.period = 2;
    config.threshold_bps = 10;
    config.target_available_margin_bps = Some(1_000);
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    context.available_margin_raw = Some(1_000 * qx_core::SCALE);
    let mut final_decision = None;
    for (ts, close) in [(1, 100), (2, 100), (3, 101)] {
        context.as_of = ts;
        final_decision = Some(
            strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close * qx_core::SCALE,
                        high_raw: close * qx_core::SCALE,
                        low_raw: close * qx_core::SCALE,
                        close_raw: close * qx_core::SCALE,
                        volume_raw: qx_core::SCALE,
                    },
                )
                .unwrap(),
        );
    }
    let intent = &final_decision.unwrap().intents[0];
    assert_eq!(intent.side, Side::Buy);
    assert_eq!(
        intent.qty.raw(),
        100 * qx_core::SCALE * qx_core::SCALE / (101 * qx_core::SCALE)
    );
    assert_ne!(intent.qty.raw(), Quantity::from_i64(999).raw());
}

#[test]
fn volatility_budget_sizing_scales_exposure_by_visible_atr() {
    let qty_raw = sizing_tests::volatility_budget_quantity(100);
    let atr_raw = 4 * qx_core::SCALE;
    let atr_ratio_bps = atr_raw * 10_000 / (102 * qx_core::SCALE);
    let expected_notional = 1_000 * qx_core::SCALE * 100 / atr_ratio_bps;
    let expected_quantity = expected_notional * qx_core::SCALE / (102 * qx_core::SCALE);
    assert_eq!(qty_raw, expected_quantity);
    assert_ne!(qty_raw, Quantity::from_i64(999).raw());
}

#[test]
fn every_builtin_strategy_survives_a_warmup_and_signal_cycle() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let reference = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    for kind in BuiltinStrategyKind::ALL {
        let config = if kind.needs_reference_leg() {
            BuiltinStrategyConfig {
                kind,
                strategy_id: format!("builtin-{}", kind.name()),
                strategy_version: format!("builtin-{}-v1", kind.name()),
                instrument: instrument.clone(),
                quantity: Quantity::from_i64(1),
                target_notional_raw: None,
                primary_market_spec: None,
                reference_market_spec: None,
                target_available_margin_bps: None,
                target_volatility_bps: None,
                target_kelly_fraction_bps: None,
                kelly_lookback_bars: None,
                fast_window: 5,
                slow_window: 20,
                period: 14,
                threshold_bps: 100,
                reference_instrument: Some(reference.clone()),
                primary_policy: None,
                reference_policy: None,
            }
        } else {
            BuiltinStrategyConfig::new(
                kind,
                format!("builtin-{}", kind.name()),
                instrument.clone(),
                Quantity::from_i64(1),
            )
            .unwrap()
        };
        let has_reference = config.reference_instrument.is_some();
        let mut strategy = BuiltinStrategy::new(config).unwrap();
        let mut context = context(&instrument);
        for ts in 1..=40 {
            if has_reference {
                let reference_close = 100 + i128::from(ts);
                context.as_of = ts;
                strategy
                    .on_event(
                        &context,
                        &MarketEvent::Bar {
                            instrument: reference.clone(),
                            ts,
                            open_raw: reference_close,
                            high_raw: reference_close + 1,
                            low_raw: reference_close - 1,
                            close_raw: reference_close,
                            volume_raw: 1,
                        },
                    )
                    .unwrap();
            }
            let close = 100 + i128::from((ts % 7) as i64) * 3 + i128::from(ts);
            context.as_of = ts;
            let decision = strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close,
                        high_raw: close + 1,
                        low_raw: close - 1,
                        close_raw: close,
                        volume_raw: 1,
                    },
                )
                .unwrap();
            assert_eq!(decision.schema_version, STRATEGY_API_VERSION);
        }
    }
}
