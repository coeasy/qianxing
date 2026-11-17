//! Paper venue order lifecycle benchmark: submit, partial fill, and completion.

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use qx_core::{
    InstrumentFunding, InstrumentId, Ledger, MakerTakerFeeModel, Money, Order, OrderPolicy,
    OrderStatus, Price, Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_guanxing::QuoteTick;
use qx_zhenlu::{PaperVenue, RateLimiter, Venue};
use std::hint::black_box;

fn paper_order(instrument: &InstrumentId, client_id: u64, maker: bool) -> Order {
    Order {
        client_id,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(4),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "paper-bench".into(),
        trace: None,
        policy: Some(OrderPolicy {
            post_only: maker,
            ..OrderPolicy::default()
        }),
        exact_amounts: None,
    }
}

fn quote(ts: u64, ask_qty: i64, source_seq: u64) -> QuoteTick {
    QuoteTick::new(
        ts,
        Price::from_i64(99),
        Quantity::from_i64(10_000),
        Price::from_i64(100),
        Quantity::from_i64(ask_qty),
        source_seq,
    )
}

fn paper_venue_order_lifecycle(c: &mut Criterion) {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: SCALE,
        qty_step: SCALE,
        min_qty: SCALE,
        min_notional_raw: 0,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
    };
    let mut group = c.benchmark_group("qx_zhenlu_crypto_paper_lifecycle_v2");
    for count in [1_usize, 16, 256] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_batched(
                || {
                    let orders = (1..=count as u64)
                        .map(|id| paper_order(&instrument, id, id % 2 == 0))
                        .collect::<Vec<_>>();
                    (
                        PaperVenue::new(
                            "paper",
                            Box::new(MakerTakerFeeModel::default_maker_taker()),
                        ),
                        RateLimiter::new(8, 8, 0),
                        orders,
                    )
                },
                |(mut venue, mut limiter, orders)| {
                    for (index, order) in orders.iter().enumerate() {
                        spec.validate_order(order.qty.raw(), order.limit.map(Price::raw))
                            .expect("order precision matches the frozen market spec");
                        let now = (index as u64 / 8) * 1_000_000_000;
                        assert!(
                            limiter.try_acquire(1, now),
                            "weighted rate limit admits order"
                        );
                        black_box(
                            venue
                                .submit(order.clone(), 1)
                                .expect("submit succeeds after precision and rate checks"),
                        );
                    }
                    let first = venue.on_quote(&instrument, quote(2, 1, 1));
                    assert_eq!(first.len(), 1, "the first quote partially fills one order");
                    let rest = venue.on_quote(&instrument, quote(3, (count as i64) * 4, 2));
                    assert_eq!(rest.len(), count, "the second quote completes every order");
                    let mut ledger = Ledger::new();
                    for event in first.iter().chain(rest.iter()) {
                        if let qx_zhenlu::VenueEvent::Fill(fill) = event {
                            spec.validate_fill(fill.qty.raw(), fill.price.raw())
                                .expect("fill precision matches the frozen market spec");
                            let order = &orders[(fill.order_id - 1) as usize];
                            ledger
                                .apply_fill_with_spec(order, fill, "USDT", &spec)
                                .expect("fill applies to the perpetual ledger");
                        }
                    }
                    let position_qty = (count as i128) * 4 * SCALE;
                    let mark_price = Price::from_i64(100);
                    let funding_raw = spec
                        .funding_payment(position_qty, mark_price.raw(), 10)
                        .expect("funding calculation succeeds");
                    ledger
                        .apply_instrument_funding(
                            "paper-bench",
                            "USDT",
                            InstrumentFunding {
                                instrument: &instrument,
                                quantity: Quantity::from_raw(position_qty),
                                mark_price,
                                amount: Money::from_raw(funding_raw),
                                ts: 28_800_000,
                            },
                        )
                        .expect("funding posts to the instrument ledger");
                    assert_eq!(ledger.entries().len(), 2 * (count + 1) + 1);
                    black_box(ledger);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, paper_venue_order_lifecycle);
criterion_main!(benches);
