use super::*;

struct BudgetBuyer {
    submitted: bool,
}

impl BarStrategy for BudgetBuyer {
    fn on_bar_orders_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        if self.submitted {
            return Ok(Vec::new());
        }
        self.submitted = true;
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }])
    }
}

#[test]
fn fixed_quantity_order_cannot_exceed_its_strategy_capital_budget() {
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        50 * SCALE,
        BudgetBuyer { submitted: false },
    )])
    .unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(
                    1,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    2,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
            ],
            &mut strategy,
        )
        .unwrap();

    assert!(report.fills.is_empty());
    assert!(report.event_log.events().iter().any(|event| matches!(
        event.kind,
        qx_core::EventKind::Rejected { ref reason, .. }
            if reason.contains("reserved_capital_raw")
    )));
}

#[test]
fn fill_price_gap_that_breaks_strategy_budget_fails_closed_before_booking() {
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        150 * SCALE,
        BudgetBuyer { submitted: false },
    )])
    .unwrap();
    let result = BacktestEngine::new(simple_config()).run(
        &[
            Bar::new(
                1,
                100 * SCALE,
                101 * SCALE,
                99 * SCALE,
                100 * SCALE,
                10 * SCALE,
            ),
            Bar::new(
                2,
                200 * SCALE,
                201 * SCALE,
                199 * SCALE,
                200 * SCALE,
                10 * SCALE,
            ),
        ],
        &mut strategy,
    );

    assert!(result.is_err());
}

struct RestingBudgetBuyer;

impl BarStrategy for RestingBudgetBuyer {
    fn on_bar_orders_checked(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        _position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        Ok(vec![Order {
            client_id: 0,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(qx_core::Price::from_raw(50 * SCALE)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }])
    }
}

#[test]
fn unfilled_resting_orders_consume_their_strategy_budget() {
    let mut strategy = PortfolioBarStrategy::new(vec![PortfolioStrategySlot::new(
        0,
        "alpha",
        100 * SCALE,
        RestingBudgetBuyer,
    )])
    .unwrap();
    let report = BacktestEngine::new(simple_config())
        .run(
            &[
                Bar::new(
                    1,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    2,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    3,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
                Bar::new(
                    4,
                    100 * SCALE,
                    101 * SCALE,
                    99 * SCALE,
                    100 * SCALE,
                    10 * SCALE,
                ),
            ],
            &mut strategy,
        )
        .unwrap();
    let accepted = report
        .event_log
        .events()
        .iter()
        .filter(|event| matches!(event.kind, qx_core::EventKind::Accepted { .. }))
        .count();
    let rejected = report
        .event_log
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                qx_core::EventKind::Rejected { ref reason, .. }
                    if reason.contains("reserved_capital_raw")
            )
        })
        .count();

    assert_eq!(accepted, 2);
    assert_eq!(rejected, 1);
    assert!(report.fills.is_empty());
}
