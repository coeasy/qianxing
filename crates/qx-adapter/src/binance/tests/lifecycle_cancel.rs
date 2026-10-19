use super::*;

#[test]
fn binance_terminal_cancel_is_idempotent_without_transport_io() {
    let (mut venue, transport) = venue("");
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
        venue.restore_orders([terminal]).unwrap();

        assert!(venue
            .cancel(client_id, 1_700_000_000_000)
            .unwrap()
            .is_empty());
        assert_eq!(venue.snapshot()[index].status, status);
    }
    assert!(transport.requests.lock().unwrap().is_empty());
}
