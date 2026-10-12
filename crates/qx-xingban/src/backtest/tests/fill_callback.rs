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
