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

#[test]
fn binance_partial_fill_during_cancel_pending_keeps_cancel_until_ack() {
    let (mut venue, _transport) = venue(
        r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"qx-7","status":"NEW","transactTime":1700000000000,"fills":[]}"#,
    );
    venue.submit(order(), 1_700_000_000_000).unwrap();
    venue
        .orders
        .get_mut(&7)
        .unwrap()
        .status
        .transition(OrderStatus::CancelPending)
        .unwrap();

    let fill_events = venue
        .ingest_user_event(
            r#"{"e":"executionReport","E":1700000000100,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"TRADE","X":"PARTIALLY_FILLED","i":42,"l":"0.5","L":"100","n":"0.001","N":"BTC","T":1700000000100,"t":11}"#,
        )
        .unwrap();
    assert!(matches!(fill_events.as_slice(), [VenueEvent::Fill(_)]));
    assert_eq!(venue.snapshot()[0].status, OrderStatus::CancelPending);
    assert_eq!(
        venue.snapshot()[0].filled,
        Quantity::from_dec("0.5").unwrap()
    );

    let cancel_events = venue
        .ingest_user_event(
            r#"{"e":"executionReport","E":1700000000200,"s":"BTCUSDT","c":"qx-7","S":"BUY","x":"CANCELED","X":"CANCELED","i":42,"T":1700000000200}"#,
        )
        .unwrap();
    assert!(matches!(
        cancel_events.as_slice(),
        [VenueEvent::Cancelled {
            client_order_id: 7,
            ..
        }]
    ));
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Cancelled);
    assert_eq!(
        venue.snapshot()[0].filled,
        Quantity::from_dec("0.5").unwrap()
    );
}
