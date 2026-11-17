use qx_core::SCALE;
use qx_guanxing::Bar;
use qx_xingban::{AshareCorporateActionEvent, AshareRuleConfig};

const DAY_MS: u64 = 86_400_000;
const SHANGHAI_OFFSET_MS: u64 = 8 * 3_600_000;
const MINUTE_MS: u64 = 60_000;

#[test]
fn adaptive_previous_close_series_matches_pointwise_rules_for_dense_actions() {
    let bars_per_day = 400;
    let mut bars = Vec::with_capacity(8_000);
    for day in 0..20_u64 {
        let day_start =
            (1_000 + day) * DAY_MS - SHANGHAI_OFFSET_MS + 9 * 3_600_000 + 30 * MINUTE_MS;
        for minute in 0..bars_per_day {
            let ts = day_start + minute as u64 * MINUTE_MS;
            let close = 100 * SCALE + (day as i128 * bars_per_day as i128 + minute as i128) * 1_000;
            bars.push(Bar::new(ts, close, close, close, close, SCALE));
        }
    }

    let actions = (0..1_024)
        .map(|index| AshareCorporateActionEvent {
            // Deliberately interleave ex-dates so the optimized path must sort and
            // advance its monotonic cursor across the whole date range.
            ts: (1_001 + index % 19) * DAY_MS - SHANGHAI_OFFSET_MS,
            cash_dividend_raw: SCALE / 1_000,
            ..AshareCorporateActionEvent::default()
        })
        .collect();
    let override_ts = bars[10 * bars_per_day + 7].ts;
    let rules = AshareRuleConfig {
        corporate_actions: actions,
        previous_close_raw: [(override_ts, 97 * SCALE)].into_iter().collect(),
        ..AshareRuleConfig::default()
    };

    // 8,000 Bars and 1,024 actions select the production adaptive path. Compare every
    // element against the established pointwise implementation, including day edges.
    let series = rules.previous_close_series(&bars).unwrap();
    assert_eq!(series.len(), bars.len());
    for (index, actual) in series.iter().enumerate() {
        assert_eq!(
            *actual,
            rules.previous_close(&bars, index).unwrap(),
            "bar index {index}"
        );
    }

    let ex_day_start = 10 * bars_per_day;
    let prior_close = bars[ex_day_start - 1].close;
    let actions_on_ex_day = 54;
    let adjusted = prior_close - actions_on_ex_day * (SCALE / 1_000);
    let tick = rules.price_tick;
    let expected_adjusted = (adjusted + tick / 2) / tick * tick;
    assert_eq!(series[ex_day_start], Some(expected_adjusted));
    assert_eq!(series[ex_day_start + 7], Some(97 * SCALE));
    assert_eq!(series[ex_day_start + 8], Some(expected_adjusted));
}
