use super::*;

struct MarginProbe {
    observed: Arc<Mutex<Vec<Option<i128>>>>,
    emitted: bool,
}

impl OrderBookStrategy for MarginProbe {
    fn on_order_book(
        &mut self,
        _snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        Ok(Vec::new())
    }

    fn on_order_book_actions_with_margin(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
        _open_order_ids: &[u64],
        available_margin_raw: Option<i128>,
    ) -> Result<OrderBookActions, String> {
        self.observed.lock().unwrap().push(available_margin_raw);
        if self.emitted {
            return Ok(OrderBookActions::default());
        }
        self.emitted = true;
        Ok(OrderBookActions {
            orders: vec![Order {
                client_id: 1,
                instrument: snapshot.instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            }],
            cancel_client_ids: Vec::new(),
        })
    }
}

#[test]
fn spot_margin_snapshot_reaches_strategy_after_each_filled_order() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut strategy = MarginProbe {
        observed: observed.clone(),
        emitted: false,
    };
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(
            &[snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)],
            &mut strategy,
        )
        .expect("spot run with margin snapshots");
    assert_eq!(report.fills.len(), 1);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            Some(Money::from_i64(10_000).raw()),
            Some(Money::from_i64(9_899).raw()),
            Some(Money::from_i64(9_899).raw()),
        ]
    );
}
