use super::*;

#[test]
fn fractional_kelly_uses_only_visible_returns_and_caps_to_available_margin() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Momentum,
        "momentum-kelly",
        instrument.clone(),
        Quantity::from_i64(999),
    )
    .unwrap();
    config.period = 2;
    config.threshold_bps = 0;
    config.target_kelly_fraction_bps = Some(5_000);
    config.kelly_lookback_bars = Some(60);
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    context.available_margin_raw = Some(1_000 * qx_core::SCALE);

    let mut close = 100 * qx_core::SCALE;
    let mut final_decision = None;
    for index in 0..=62_u64 {
        // Repeated gains/losses yield both sides of the return distribution; the
        // final two closes rise so the strategy emits a long signal after warmup.
        let next = match index % 3 {
            0 => close + close / 10,
            1 => close - close / 20,
            _ => close + close / 100,
        };
        close = if index >= 60 {
            close + close / 100
        } else {
            next
        };
        context.as_of = index + 1;
        final_decision = Some(
            strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts: index + 1,
                        open_raw: close,
                        high_raw: close,
                        low_raw: close,
                        close_raw: close,
                        volume_raw: qx_core::SCALE,
                    },
                )
                .unwrap(),
        );
    }

    let final_decision = final_decision.unwrap();
    let intent = final_decision.intents.first().expect("positive Kelly edge");
    assert_eq!(intent.side, Side::Buy);
    assert!(intent.qty.raw() > 0);
    let full_margin_quantity = 1_000 * qx_core::SCALE * qx_core::SCALE / close;
    assert!(intent.qty.raw() < full_margin_quantity);
    assert_ne!(intent.qty.raw(), Quantity::from_i64(999).raw());
}

#[test]
fn kelly_sizing_fails_closed_without_both_wins_and_losses_or_enough_history() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Momentum,
        "momentum-kelly-no-losses",
        instrument.clone(),
        Quantity::from_i64(999),
    )
    .unwrap();
    config.period = 2;
    config.threshold_bps = 0;
    config.target_kelly_fraction_bps = Some(5_000);
    config.kelly_lookback_bars = Some(30);
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    context.available_margin_raw = Some(1_000 * qx_core::SCALE);
    let mut decision = None;
    for ts in 1..=31_u64 {
        let close = i128::from(ts) * qx_core::SCALE;
        context.as_of = ts;
        decision = Some(
            strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close,
                        high_raw: close,
                        low_raw: close,
                        close_raw: close,
                        volume_raw: qx_core::SCALE,
                    },
                )
                .unwrap(),
        );
    }
    assert!(decision.unwrap().intents.is_empty());
}

#[test]
fn non_positive_directional_kelly_target_closes_existing_position_reduce_only() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Momentum,
        "momentum-kelly-close",
        instrument.clone(),
        Quantity::from_i64(999),
    )
    .unwrap();
    config.period = 2;
    config.threshold_bps = 0;
    config.target_kelly_fraction_bps = Some(5_000);
    config.kelly_lookback_bars = Some(30);
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    context.available_margin_raw = Some(1_000 * qx_core::SCALE);
    let mut close = 100 * qx_core::SCALE;
    let mut decision = None;
    for ts in 1..=33_u64 {
        close = if ts >= 32 {
            close - close / 10
        } else if ts % 10 == 0 {
            close - close / 1_000
        } else {
            close + close / 100
        };
        context.as_of = ts;
        if ts == 33 {
            context
                .positions
                .insert(instrument.to_string(), 100 * qx_core::SCALE);
        }
        decision = Some(
            strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close,
                        high_raw: close,
                        low_raw: close,
                        close_raw: close,
                        volume_raw: qx_core::SCALE,
                    },
                )
                .unwrap(),
        );
    }
    let intent = decision
        .unwrap()
        .intents
        .into_iter()
        .next()
        .expect("zero Kelly target must emit a close instead of orphaning inventory");
    assert_eq!(intent.side, Side::Sell);
    assert_eq!(intent.qty.raw(), 100 * qx_core::SCALE);
    assert!(intent.reduce_only);
}

#[test]
fn volatility_budget_never_exceeds_available_margin() {
    let qty_raw = volatility_budget_quantity(10_000);
    let expected_max_quantity = 1_000 * qx_core::SCALE * qx_core::SCALE / (102 * qx_core::SCALE);
    assert_eq!(qty_raw, expected_max_quantity);
}

pub(super) fn volatility_budget_quantity(risk_bps: u32) -> i128 {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut config = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::Momentum,
        "momentum-volatility-budget",
        instrument.clone(),
        Quantity::from_i64(999),
    )
    .unwrap();
    config.period = 2;
    config.threshold_bps = 10;
    config.target_volatility_bps = Some(risk_bps);
    let mut strategy = BuiltinStrategy::new(config).unwrap();
    let mut context = context(&instrument);
    context.available_margin_raw = Some(1_000 * qx_core::SCALE);

    let mut final_decision = None;
    for (ts, close) in [(1, 100), (2, 101), (3, 102)] {
        context.as_of = ts;
        final_decision = Some(
            strategy
                .on_event(
                    &context,
                    &MarketEvent::Bar {
                        instrument: instrument.clone(),
                        ts,
                        open_raw: close * qx_core::SCALE,
                        high_raw: (close + 2) * qx_core::SCALE,
                        low_raw: (close - 2) * qx_core::SCALE,
                        close_raw: close * qx_core::SCALE,
                        volume_raw: qx_core::SCALE,
                    },
                )
                .unwrap(),
        );
    }

    let intent = &final_decision.unwrap().intents[0];
    assert_eq!(intent.side, Side::Buy);
    intent.qty.raw()
}
