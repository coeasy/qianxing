use qx_core::InstrumentId;
use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyHistoryRequirements,
    StrategyStopContextV1, STRATEGY_API_VERSION,
};
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
struct LifecycleFixture {
    warmup_bars: u64,
    instrument: String,
    bar_timestamps: Vec<u64>,
    warmup_observed_bars: u64,
    expected_normalized_callback_order: Vec<String>,
}

struct RecordingStrategy {
    callbacks: Vec<String>,
}

impl Strategy for RecordingStrategy {
    fn on_init(&mut self, _context: &StrategyContext) -> Result<(), String> {
        self.callbacks.push("on_init".into());
        Ok(())
    }

    fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
        Ok(StrategyHistoryRequirements { warmup_bars: 2 })
    }

    fn on_start(&mut self, _context: &StrategyContext) -> Result<(), String> {
        self.callbacks.push("on_start".into());
        Ok(())
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let MarketEvent::Bar { ts, .. } = event else {
            return Err("lifecycle golden expects Bar events".into());
        };
        self.callbacks.push(format!("market:Bar:{ts}"));
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("lifecycle-golden:{ts}"),
            strategy_id: context.strategy_id.clone(),
            signal_id: *ts,
            confidence: 0,
            priority: 0,
            expires_at: *ts,
            intents: Vec::new(),
        })
    }

    fn on_warmup_complete(
        &mut self,
        _context: &StrategyContext,
        instrument: Option<&InstrumentId>,
        observed_bars: u64,
    ) -> Result<(), String> {
        let instrument = instrument.ok_or("warmup fixture requires an instrument")?;
        self.callbacks
            .push(format!("warmup_complete:{instrument}:{observed_bars}"));
        Ok(())
    }

    fn on_stop_with_context(&mut self, stop: &StrategyStopContextV1) -> Result<(), String> {
        stop.validate()?;
        self.callbacks.push("on_stop".into());
        Ok(())
    }
}

#[test]
fn rust_strategy_trait_matches_shared_normalized_warmup_lifecycle() {
    let fixture: LifecycleFixture =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json"))
            .expect("shared cross-host lifecycle fixture parses");
    assert_eq!(fixture.warmup_bars, 2);
    assert_eq!(fixture.warmup_observed_bars, fixture.warmup_bars);
    assert_eq!(fixture.bar_timestamps, [9, 10]);

    let context = StrategyContext {
        strategy_id: "lifecycle-golden".into(),
        strategy_version: "1".into(),
        account_id: "main".into(),
        venue_id: "BINANCE".into(),
        data_fingerprint: "strategy-lifecycle-matrix-v1".into(),
        as_of: 10,
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: None,
        risk_state: "clear".into(),
    };
    let instrument = InstrumentId::parse(&fixture.instrument).expect("fixture instrument parses");
    let mut strategy = RecordingStrategy {
        callbacks: Vec::new(),
    };
    strategy.on_init(&context).unwrap();
    strategy.on_start(&context).unwrap();
    for (index, ts) in fixture.bar_timestamps.iter().copied().enumerate() {
        if index == 1 {
            strategy
                .on_warmup_complete(&context, Some(&instrument), fixture.warmup_observed_bars)
                .unwrap();
        }
        let event = MarketEvent::Bar {
            instrument: instrument.clone(),
            ts,
            open_raw: 100,
            high_raw: 101,
            low_raw: 99,
            close_raw: 100,
            volume_raw: 1,
            exact_ohlcv: None,
        };
        strategy.on_event(&context, &event).unwrap();
    }
    strategy
        .on_stop_with_context(&StrategyStopContextV1::host_stop(Some(context)))
        .unwrap();

    assert_eq!(
        strategy.callbacks,
        fixture.expected_normalized_callback_order
    );
}
