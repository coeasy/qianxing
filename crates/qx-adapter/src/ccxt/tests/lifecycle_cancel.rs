use super::*;

struct CountingRpc(Arc<Mutex<Vec<Value>>>);
impl CcxtRpc for CountingRpc {
    fn call(&mut self, request: Value) -> Result<Value, String> {
        self.0.lock().unwrap().push(request);
        Ok(json!({}))
    }
}

#[test]
fn ccxt_unknown_cancel_state_fails_closed_before_worker_rpc() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut venue = CcxtProcessVenue::new("binance", Box::new(CountingRpc(Arc::clone(&calls))));
    let mut unknown = order();
    unknown.status = OrderStatus::Unknown;
    venue.restore_order(unknown, "remote-unknown").unwrap();

    assert!(matches!(
        venue.cancel(7, 11),
        Err(QxError::ReconcileRequired(_))
    ));
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Unknown);
}

#[test]
fn ccxt_terminal_cancel_is_idempotent_without_worker_rpc() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut venue = CcxtProcessVenue::new("binance", Box::new(CountingRpc(Arc::clone(&calls))));
    let terminal_statuses = [
        OrderStatus::Filled,
        OrderStatus::Cancelled,
        OrderStatus::Rejected,
        OrderStatus::Expired,
    ];
    for (index, status) in terminal_statuses.into_iter().enumerate() {
        let client_id = index as u64 + 1;
        let mut terminal = order();
        terminal.client_id = client_id;
        terminal.status = status;
        venue
            .restore_order(terminal, format!("remote-{client_id}"))
            .unwrap();

        assert!(venue.cancel(client_id, 11).unwrap().is_empty());
        assert_eq!(venue.snapshot()[index].status, status);
    }
    assert!(calls.lock().unwrap().is_empty());
}
