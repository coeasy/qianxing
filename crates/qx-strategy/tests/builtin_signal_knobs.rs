//! 内置策略的"信号旋钮 × kind"清单（V12 #102）。
//!
//! 清单住在 `qx-strategy::builtin_signal`，而它凭什么可信只能靠行为证明：**不在清单里的旋钮
//! 挪到任何数值，这一轮的信号序列一个字都不能变**（变成交、变预热、变成败都不行）；
//! **在清单里的旋钮至少存在一个能变出不同结果的档位**。前者拆掉的是历史裁剪上限与参数体检里
//! 那两条暗通道——`max_history` 曾按 `slow_window.max(period)` 开窗口，于是 MACD 的周期明明
//! 不进信号，却能靠"多留几根前缀"改动 EMA 结果；后者防的是清单写得比 `signal()` 更保守时
//! 用例集体失声。两向都成对，播报里那句"这一轮生效的是这几项"才是真话。

use qx_core::{InstrumentId, Quantity, Side};
use qx_strategy::{
    builtin_signal::BuiltinSignalKnob, BuiltinStrategy, BuiltinStrategyConfig, BuiltinStrategyKind,
    MarketEvent, Strategy, StrategyContext,
};
use std::collections::BTreeMap;

const PRIMARY: &str = "BTCUSDT.BINANCE";
const REFERENCE: &str = "ETHUSDT.BINANCE";
const BAR_COUNT: u64 = 90;
const DEFAULTS: (usize, usize, usize, i128) = (5, 20, 14, 100);

/// 一条"什么指标都得出结果"的序列：趋势段与反转段交替、实体逐根变宽、每隔几根来一根
/// 大实体（喂波动突破）、上下影线会归零（让收盘价能真的贴到最高/最低点）。
/// ATR/Keltner/VWAP/波动突破这类要看高低与量的 kind 若拿常数序列，"换了值结果也换"
/// 就成了空断言。
fn bar(instrument: &InstrumentId, ts: u64, offset: i128) -> MarketEvent {
    let scale = qx_core::SCALE;
    let up = ((i128::from(ts) / 5) % 7) < 4;
    let spike = if ts.is_multiple_of(9) { 3 } else { 1 };
    let body = i128::from(spike) * (2 + i128::from(ts % 13)) * scale;
    let drift = (40 + i128::from(ts) / 3) * scale;
    let open = if up { drift } else { drift + body };
    let close = if up { drift + body } else { drift };
    let upper_wick = if ts.is_multiple_of(6) {
        0
    } else {
        (1 + i128::from(ts % 4)) * scale
    };
    let lower_wick = if ts.is_multiple_of(7) {
        0
    } else {
        (1 + i128::from(ts % 5)) * scale
    };
    MarketEvent::Bar {
        instrument: instrument.clone(),
        ts,
        open_raw: open + offset,
        high_raw: open.max(close) + upper_wick + offset,
        low_raw: open.min(close) - lower_wick + offset,
        close_raw: close + offset,
        volume_raw: (100 + i128::from(ts % 7) * 13) * scale,
    }
}

fn context(instrument: &InstrumentId) -> StrategyContext {
    StrategyContext {
        strategy_id: format!("builtin-{}", instrument.symbol),
        strategy_version: "builtin-v1".into(),
        account_id: "main".into(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: "knobs-v1".into(),
        as_of: 1,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000),
        risk_state: "ready".into(),
    }
}

/// 跑一遍序列，返回"每一步做了什么决定"的时间线，首格是意图总数。
/// 预热多等一根、多一道拒绝、装配当场失败都会落进这份时间线里。
fn run(
    kind: BuiltinStrategyKind,
    fast: usize,
    slow: usize,
    period: usize,
    threshold: i128,
) -> Vec<String> {
    let instrument = InstrumentId::parse(PRIMARY).unwrap();
    let reference = kind
        .needs_reference_leg()
        .then(|| InstrumentId::parse(REFERENCE).unwrap());
    let config = BuiltinStrategyConfig {
        kind,
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        instrument: instrument.clone(),
        quantity: Quantity::from_i64(1),
        fast_window: fast,
        slow_window: slow,
        period,
        threshold_bps: threshold,
        reference_instrument: reference.clone(),
        primary_policy: None,
        reference_policy: None,
    };
    // 装配失败本身就是一种"结果变了"：参数体检若还去管不上场的旋钮，这里就会与基线分叉。
    let Ok(mut strategy) = BuiltinStrategy::new(config) else {
        return vec![format!(
            "assemble-err:{}:fast={fast}:slow={slow}:period={period}:threshold={threshold}",
            kind.name()
        )];
    };
    let mut context = context(&instrument);
    let mut timeline = Vec::new();
    let mut intents = 0_usize;
    for ts in 1..=BAR_COUNT {
        if let Some(reference) = &reference {
            context.as_of = ts;
            let decision = strategy.on_event(
                &context,
                &bar(reference, ts, (i128::from(ts % 5) - 2) * qx_core::SCALE),
            );
            intents += record(&mut timeline, ts, "reference", decision);
        }
        context.as_of = ts;
        let decision = strategy.on_event(&context, &bar(&instrument, ts, 0));
        intents += record(&mut timeline, ts, "primary", decision);
    }
    timeline.insert(0, format!("intents={intents}"));
    timeline
}

fn record(
    timeline: &mut Vec<String>,
    ts: u64,
    leg: &str,
    decision: Result<qx_strategy::StrategyDecision, String>,
) -> usize {
    match decision {
        Ok(decision) => {
            let legs = decision
                .intents
                .iter()
                .map(|intent| {
                    format!(
                        "{}{}@{}",
                        match intent.side {
                            Side::Buy => "buy",
                            Side::Sell => "sell",
                        },
                        intent.qty.raw(),
                        intent.instrument
                    )
                })
                .collect::<Vec<_>>()
                .join("+");
            timeline.push(format!("{ts}:{leg}:{legs}"));
            decision.intents.len()
        }
        Err(error) => {
            timeline.push(format!("{ts}:{leg}:err"));
            timeline.push(format!("  detail: {error}"));
            0
        }
    }
}

fn baseline(kind: BuiltinStrategyKind) -> Vec<String> {
    let (fast, slow, period, threshold) = DEFAULTS;
    run(kind, fast, slow, period, threshold)
}

/// 基线必须真的出过方向，否则"换了值也没变"是在比两串空数据。
fn assert_baseline_actually_traded(kind: BuiltinStrategyKind, base: &[String]) {
    assert!(
        base[0] != "intents=0",
        "{} 的基线一次都没出方向，下面的对比是空的: {base:?}",
        kind.name()
    );
}

/// 清单外的旋钮：每一项都试到"数值离谱"的档位（含 `fast>=slow`、`period<2`、负阈值），
/// 时间线必须一字不变。
#[test]
fn knobs_outside_the_list_cannot_change_a_single_run() {
    for kind in BuiltinStrategyKind::ALL {
        let base = baseline(kind);
        assert_baseline_actually_traded(kind, &base);
        for knob in BuiltinSignalKnob::ALL {
            if kind.uses_signal_knob(knob) {
                continue;
            }
            for value in [-9_999_i128, 1, 3, 25, 61] {
                let (fast, slow, period, threshold) = match knob {
                    BuiltinSignalKnob::FastWindow => (value as usize, 20, 14, 100),
                    BuiltinSignalKnob::SlowWindow => (5, value as usize, 14, 100),
                    BuiltinSignalKnob::Period => (5, 20, value as usize, 100),
                    BuiltinSignalKnob::ThresholdBps => (5, 20, 14, value),
                };
                assert_eq!(
                    base,
                    run(kind, fast, slow, period, threshold),
                    "{} 的清单是 {:?}，挪动 {:?}={value} 却改动了这一轮",
                    kind.name(),
                    kind.signal_knobs(),
                    knob,
                );
            }
        }
    }
}

/// 清单内的旋钮：至少存在一个档位真的换出不同结果。清单若把上场的项漏掉，上一条
/// 会一路绿灯，这一条才会红——两向必须成对。
#[test]
fn every_knob_on_the_list_can_still_change_the_run() {
    for kind in BuiltinStrategyKind::ALL {
        let base = baseline(kind);
        assert_baseline_actually_traded(kind, &base);
        for knob in kind.signal_knobs() {
            let moved = match knob {
                BuiltinSignalKnob::FastWindow => [
                    run(kind, 2, 20, 14, 100),
                    run(kind, 12, 20, 14, 100),
                    run(kind, 4, 20, 14, 100),
                ],
                BuiltinSignalKnob::SlowWindow => [
                    run(kind, 5, 9, 14, 100),
                    run(kind, 5, 40, 14, 100),
                    run(kind, 5, 31, 14, 100),
                ],
                BuiltinSignalKnob::Period => [
                    run(kind, 5, 20, 3, 100),
                    run(kind, 5, 20, 33, 100),
                    run(kind, 5, 20, 2, 100),
                ],
                BuiltinSignalKnob::ThresholdBps => [
                    run(kind, 5, 20, 14, 0),
                    run(kind, 5, 20, 14, 9_000),
                    run(kind, 5, 20, 14, 25),
                ],
            };
            assert!(
                moved.iter().any(|probe| *probe != base),
                "{} 声明读 {:?}，可这三个档位里没有一个能改动这一轮结果",
                kind.name(),
                knob,
            );
        }
    }
}

/// MACD 是清单上唯一的空集：12/26/9 是内核常数。这条不成立时，播报里的 `knobs=none`
/// 就成了假话，示例配置里那行 `builtin_fast_window=5` 也重新变回暗配置。
#[test]
fn macd_has_no_tunable_signal_knob_and_still_signals() {
    assert!(BuiltinStrategyKind::Macd.signal_knobs().is_empty());
    assert_baseline_actually_traded(
        BuiltinStrategyKind::Macd,
        &baseline(BuiltinStrategyKind::Macd),
    );
}

/// 清单要覆盖 17 个 kind，配置键与播报字段各是一套一一映射：
/// 新增 kind 忘了登记时这里先红，而不是让 `builtin-strategies` 少印一列。
#[test]
fn the_list_covers_every_builtin_kind() {
    assert_eq!(BuiltinStrategyKind::ALL.len(), 17);
    let mut config_keys = BTreeMap::new();
    for knob in BuiltinSignalKnob::ALL {
        assert!(
            config_keys
                .insert(knob.config_key(), knob.field())
                .is_none(),
            "两个旋钮共用了同一个配置键名"
        );
    }
    for kind in BuiltinStrategyKind::ALL {
        let listed = kind.signal_knobs();
        assert!(
            listed
                .iter()
                .enumerate()
                .all(|(index, knob)| !listed[..index].contains(knob)),
            "{} 的清单里 {:?} 重复登记",
            kind.name(),
            listed
        );
        assert_eq!(
            kind.signal_knob_list(),
            if listed.is_empty() {
                "none".to_string()
            } else {
                listed
                    .iter()
                    .map(|knob| knob.field())
                    .collect::<Vec<_>>()
                    .join(",")
            },
            "{} 的播报清单与旋钮表不一致",
            kind.name()
        );
    }
}
