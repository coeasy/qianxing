use super::*;
use qx_strategy::{
    Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent, STRATEGY_API_VERSION,
};

struct NativeBuyThenClose {
    submitted: bool,
    positions_at_update: Vec<i128>,
    cash_at_update: Vec<i128>,
}

impl Strategy for NativeBuyThenClose {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let instrument = event.instrument().cloned().ok_or("missing instrument")?;
        let intents = if self.submitted {
            Vec::new()
        } else {
            self.submitted = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument,
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
        };
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("book:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 1,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        let value: serde_json::Value =
            serde_json::from_str(update).map_err(|error| error.to_string())?;
        if value["schema_version"] != qx_strategy::ORDER_UPDATE_SCHEMA_VERSION
            || value["event"] != "fill"
        {
            return Err("unexpected order update envelope".into());
        }
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let position = context.positions[&instrument.to_string()];
        self.positions_at_update.push(position);
        self.cash_at_update.push(context.cash["USDT"]);
        if position == 0 {
            return Ok(None);
        }
        Ok(Some(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: "close-after-fill".into(),
            strategy_id: context.strategy_id.clone(),
            signal_id: 2,
            confidence: 1,
            priority: 0,
            expires_at: 0,
            intents: vec![StrategyOrderIntent {
                intent_id: 2,
                instrument,
                side: Side::Sell,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: true,
                post_only: false,
            }],
        }))
    }
}

#[test]
fn native_strategy_adapter_uses_the_same_order_book_causal_path() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 0,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let context = StrategyContext {
        strategy_id: "native-book".into(),
        strategy_version: "v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "book-1".into(),
        as_of: 1,
        positions: std::collections::BTreeMap::new(),
        cash: std::collections::BTreeMap::new(),
        available_margin_raw: Some(10_000),
        risk_state: "ready".into(),
    };
    let mut strategy = NativeOrderBookStrategy::new(
        NativeBuyThenClose {
            submitted: false,
            positions_at_update: Vec::new(),
            cash_at_update: Vec::new(),
        },
        context,
    );
    let report = OrderBookBacktestEngine::new(config)
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].order_id, 1);
}

#[test]
fn native_order_book_fill_update_queues_a_risk_checked_next_snapshot_order() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let config = OrderBookBacktestConfig {
        instrument: instrument.clone(),
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(10_000),
        fee_bps: 10,
        instrument_spec: None,
        risk: RiskGate::conservative_default(),
        data_tier: DataTier::L2L3,
    };
    let mut strategy = NativeOrderBookStrategy::new(
        NativeBuyThenClose {
            submitted: false,
            positions_at_update: Vec::new(),
            cash_at_update: Vec::new(),
        },
        StrategyContext {
            strategy_id: "native-book-fill".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "book-fill-1".into(),
            as_of: 1,
            positions: std::collections::BTreeMap::new(),
            cash: std::collections::BTreeMap::new(),
            available_margin_raw: Some(10_000 * SCALE),
            risk_state: "ready".into(),
        },
    );
    let report = OrderBookBacktestEngine::new(config)
        .run(
            &[snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)],
            &mut strategy,
        )
        .unwrap();
    assert_eq!(report.fills.len(), 2);
    assert_eq!(strategy.strategy.positions_at_update, vec![SCALE, 0]);
    assert!(strategy.strategy.cash_at_update[0] > 0);
    assert!(strategy.strategy.cash_at_update[0] < 10_000 * SCALE);
    assert_eq!(
        report
            .ledger
            .position_for("main", &instrument)
            .quantity
            .raw(),
        0
    );
    qx_core::ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
}
