use super::*;

struct FillObserver {
    submitted: bool,
    observed: Vec<(u64, i128, String)>,
}

impl BarStrategy for FillObserver {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if self.submitted || position != 0 {
            return None;
        }
        self.submitted = true;
        Some(Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some("fill-observer".into()),
                signal_id: Some(1),
                intent_id: Some(1),
                rule_version: Some("test-v1".into()),
            }),
            policy: None,
        })
    }

    fn on_fill(&mut self, order: &Order, fill: &qx_core::Fill) -> Result<(), qx_core::QxError> {
        self.observed.push((
            fill.order_id,
            fill.qty.raw(),
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.strategy_id.clone())
                .unwrap_or_default(),
        ));
        Ok(())
    }
}

#[test]
fn strategy_fill_callback_runs_after_authoritative_fill_and_preserves_attribution() {
    let mut strategy = FillObserver {
        submitted: false,
        observed: Vec::new(),
    };
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(1, 100, 101, 99, 100, 10),
                Bar::new(2, 102, 103, 101, 102, 10),
                Bar::new(3, 104, 105, 103, 104, 10),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(report.fills.len(), 1);
    assert_eq!(strategy.observed.len(), 1);
    assert_eq!(
        strategy.observed[0],
        (
            report.fills[0].order_id,
            report.fills[0].qty.raw(),
            "fill-observer".to_string()
        )
    );
    qx_core::ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
}

struct FillDrivenStrategy {
    submitted: bool,
    updates: Vec<serde_json::Value>,
    positions_at_update: Vec<i128>,
    cash_at_update: Vec<i128>,
}

impl qx_strategy::Strategy for FillDrivenStrategy {
    fn on_event(
        &mut self,
        context: &qx_strategy::StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<qx_strategy::StrategyDecision, String> {
        let qx_strategy::MarketEvent::Bar { instrument, ts, .. } = event else {
            return Err("test strategy expects Bar events".into());
        };
        let intents = if !self.submitted {
            self.submitted = true;
            vec![qx_strategy::StrategyOrderIntent {
                intent_id: 1,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
        } else {
            Vec::new()
        };
        Ok(qx_strategy::StrategyDecision {
            schema_version: qx_strategy::STRATEGY_API_VERSION,
            request_id: format!("{}-{ts}", context.strategy_id),
            strategy_id: context.strategy_id.clone(),
            signal_id: *ts,
            confidence: 1,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }

    fn on_order_update(
        &mut self,
        context: &qx_strategy::StrategyContext,
        update: &str,
    ) -> Result<Option<qx_strategy::StrategyDecision>, String> {
        let value: serde_json::Value =
            serde_json::from_str(update).map_err(|error| error.to_string())?;
        if value["schema_version"] != qx_strategy::ORDER_UPDATE_SCHEMA_VERSION
            || value["event"] != "fill"
        {
            return Err("unexpected order update envelope".into());
        }
        self.updates.push(value);
        self.positions_at_update
            .push(context.positions.get("T.SIM").copied().unwrap_or_default());
        self.cash_at_update
            .push(context.cash.get("USD").copied().unwrap_or_default());
        if self.updates.len() > 1 {
            return Ok(None);
        }
        let instrument = qx_core::InstrumentId::parse("T.SIM").unwrap();
        Ok(Some(qx_strategy::StrategyDecision {
            schema_version: qx_strategy::STRATEGY_API_VERSION,
            request_id: "close-after-fill".into(),
            strategy_id: context.strategy_id.clone(),
            signal_id: 2,
            confidence: 1,
            priority: 0,
            expires_at: 0,
            intents: vec![qx_strategy::StrategyOrderIntent {
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
fn native_strategy_order_update_decision_is_risk_checked_and_filled_on_next_bar() {
    let mut strategy = NativeBarStrategy::new(
        FillDrivenStrategy {
            submitted: false,
            updates: Vec::new(),
            positions_at_update: Vec::new(),
            cash_at_update: Vec::new(),
        },
        qx_strategy::StrategyContext {
            strategy_id: "fill-driven".into(),
            strategy_version: "test-v1".into(),
            account_id: "main".into(),
            venue_id: "SIM".into(),
            data_fingerprint: "fill-callback-bars".into(),
            as_of: 1,
            positions: std::collections::BTreeMap::new(),
            cash: std::collections::BTreeMap::from([("USD".into(), 1_000 * SCALE)]),
            available_margin_raw: Some(1_000 * SCALE),
            risk_state: "backtest".into(),
        },
    );
    strategy.initialize().unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(1, 100, 101, 99, 100, 10),
                Bar::new(2, 102, 103, 101, 102, 10),
                Bar::new(3, 104, 105, 103, 104, 10),
            ],
            &mut strategy,
        )
        .unwrap();

    assert_eq!(strategy.strategy.updates.len(), 2);
    assert_eq!(strategy.strategy.updates[0]["event"], "fill");
    assert_eq!(
        strategy.strategy.updates[0]["fill"]["order_id"],
        report.fills[0].order_id
    );
    assert_eq!(
        strategy.strategy.positions_at_update[0],
        Quantity::from_i64(1).raw(),
        "the order update must see the position after authoritative fill application"
    );
    assert!(strategy.strategy.cash_at_update[0] < 1_000 * SCALE);
    assert!(strategy.strategy.cash_at_update[0] > 0);
    assert_eq!(report.fills.len(), 2);
    assert_eq!(report.fills[0].qty.raw(), report.fills[1].qty.raw());
    qx_core::ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
}
