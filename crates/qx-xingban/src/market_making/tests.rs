use super::*;
use crate::BookLevel;
use qx_core::SCALE;

fn snapshot(sequence: u64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        ts: sequence * 1_000,
        sequence,
        bids: vec![BookLevel {
            price: Price::from_raw(100 * SCALE),
            qty: Quantity::from_raw(10 * SCALE),
        }],
        asks: vec![BookLevel {
            price: Price::from_raw(102 * SCALE),
            qty: Quantity::from_raw(10 * SCALE),
        }],
    }
}

fn strategy() -> MarketMakingStrategy {
    MarketMakingStrategy::new(
        MarketMakingConfig {
            spread_bps: 100,
            quote_qty_raw: SCALE,
            max_inventory_raw: 5 * SCALE,
            tick_size_raw: SCALE / 100,
            inventory_skew_bps: 100,
        },
        InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        "account",
        "mm",
        None,
    )
    .unwrap()
}

#[test]
fn emits_two_tick_aligned_quotes_and_inventory_skews_them_down() {
    let mut maker = strategy();
    let flat = maker.on_order_book_actions(&snapshot(1), 0, &[]).unwrap();
    assert_eq!(flat.orders.len(), 2);
    let buy = flat
        .orders
        .iter()
        .find(|order| order.side == Side::Buy)
        .unwrap();
    let sell = flat
        .orders
        .iter()
        .find(|order| order.side == Side::Sell)
        .unwrap();
    assert!(buy.limit.unwrap().raw() < sell.limit.unwrap().raw());
    assert_eq!(buy.limit.unwrap().raw() % (SCALE / 100), 0);
    assert_eq!(sell.limit.unwrap().raw() % (SCALE / 100), 0);

    let long = maker
        .on_order_book_actions(&snapshot(2), 3 * SCALE, &[])
        .unwrap();
    let long_buy = long
        .orders
        .iter()
        .find(|order| order.side == Side::Buy)
        .unwrap();
    let long_sell = long
        .orders
        .iter()
        .find(|order| order.side == Side::Sell)
        .unwrap();
    assert!(long_buy.limit.unwrap().raw() < buy.limit.unwrap().raw());
    assert!(long_sell.limit.unwrap().raw() < sell.limit.unwrap().raw());
    assert_eq!(long_buy.qty.raw(), SCALE);
    assert_eq!(long_sell.qty.raw(), SCALE);
}

#[test]
fn replaces_resting_quotes_and_fails_closed_on_bad_configuration_or_crossed_book() {
    let mut maker = strategy();
    let ids = [2, 3];
    let actions = maker.on_order_book_actions(&snapshot(2), 0, &ids).unwrap();
    assert_eq!(actions.cancel_client_ids, ids);
    assert_eq!(actions.orders.len(), 2);
    let mut crossed = snapshot(3);
    crossed.asks[0].price = crossed.bids[0].price;
    assert!(maker.on_order_book_actions(&crossed, 0, &[]).is_err());
    assert!(MarketMakingConfig {
        spread_bps: 0,
        ..strategy().config
    }
    .validate()
    .is_err());
}
