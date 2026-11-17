use super::*;

fn order(qty: i64, side: Side) -> Order {
    Order {
        client_id: 1,
        instrument: InstrumentId::parse("T.V").unwrap(),
        side,
        qty: Quantity::from_i64(qty),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,

        exact_amounts: None,
    }
}
#[test]
fn restored_paper_venue_continues_the_durable_order_id_sequence() {
    let mut existing = order(1, Side::Buy);
    existing.client_id = 41;
    let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
    venue
        .restore_orders_with_next_venue_id([existing], 42)
        .unwrap();
    let mut next = order(1, Side::Sell);
    next.client_id = 42;
    assert!(matches!(
        venue.submit(next, 11).unwrap().as_slice(),
        [VenueEvent::Accepted { venue_order_id, .. }] if venue_order_id == "paper-42"
    ));
}

#[test]
fn paper_venue_sequence_exhaustion_fails_before_registering_order() {
    let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
    venue
        .restore_orders_with_next_venue_id([], u64::MAX)
        .unwrap();
    assert!(venue.submit(order(1, Side::Buy), 1).is_err());
    assert!(venue.snapshot().is_empty());
}

#[test]
fn paper_venue_rejects_invalid_fee_model_before_accepting_orders() {
    let mut venue = PaperVenue::new(
        "paper",
        Box::new(qx_core::MakerTakerFeeModel {
            maker_bp: -1,
            taker_bp: 5,
        }),
    );
    assert!(venue.submit(order(1, Side::Buy), 1).is_err());
    assert!(venue.snapshot().is_empty());
}

#[test]
fn paper_venue_consumes_quote_capacity_once() {
    let instrument = InstrumentId::parse("T.V").unwrap();
    let mut venue = PaperVenue::new("paper", Box::new(qx_core::ZeroFeeModel));
    let mut first = order(6, Side::Buy);
    first.client_id = 1;
    let mut second = order(6, Side::Buy);
    second.client_id = 2;
    venue.submit(first, 1).unwrap();
    venue.submit(second, 1).unwrap();
    let events = venue.on_quote(
        &instrument,
        QuoteTick::new(
            2,
            Price::from_i64(99),
            Quantity::from_i64(10),
            Price::from_i64(100),
            Quantity::from_i64(10),
            1,
        ),
    );
    let filled: i128 = events
        .iter()
        .map(|event| match event {
            VenueEvent::Fill(fill) => fill.qty.raw(),
            _ => 0,
        })
        .sum();
    assert_eq!(filled, Quantity::from_i64(10).raw());
    assert_eq!(
        venue.snapshot()[0].filled.raw(),
        Quantity::from_i64(6).raw()
    );
    assert_eq!(
        venue.snapshot()[1].filled.raw(),
        Quantity::from_i64(4).raw()
    );
    assert!(venue
        .on_quote(
            &instrument,
            QuoteTick::new(
                3,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                1,
            )
        )
        .is_empty());
}
