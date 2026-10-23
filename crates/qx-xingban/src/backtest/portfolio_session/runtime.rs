use super::*;
use crate::venue::BarMatchingEngine;
use qx_core::{Fnv1a, SCALE};

pub(super) struct RuntimeMarket<'a> {
    pub(super) instrument: InstrumentId,
    pub(super) priority: u32,
    pub(super) bars: &'a [Bar],
    pub(super) spec: TradingInstrumentSpec,
    pub(super) matcher: BarMatchingEngine,
    pub(super) margin: Box<dyn MarginRule>,
}

pub(super) struct RuntimeMarkets<'a> {
    pub(super) by_index: Vec<RuntimeMarket<'a>>,
    pub(super) index_by_instrument: BTreeMap<InstrumentId, usize>,
}

pub(super) fn invariant(reason: &'static str) -> qx_core::QxError {
    qx_core::QxError::Invariant(reason.into())
}

pub(super) fn sum_pending_funding_reservations(runtime: &RuntimeMarkets<'_>) -> Option<u128> {
    runtime.by_index.iter().try_fold(0_u128, |total, market| {
        total.checked_add(market.matcher.pending_funding_reservation_sum_raw_exact()?)
    })
}

pub(super) fn pending_funding_reservation_raw(
    runtime: &RuntimeMarkets<'_>,
    cached: Option<u128>,
) -> i128 {
    cached
        .map(|total| total.min(i128::MAX as u128) as i128)
        .unwrap_or_else(|| {
            runtime.by_index.iter().fold(0_i128, |total, market| {
                total.saturating_add(market.matcher.pending_funding_reservation_raw())
            })
        })
}

pub(super) fn bar_at_or_before(bars: &[Bar], timestamp: u64) -> Option<&Bar> {
    // Validated sorted timestamps make partition_point equivalent to the old reverse scan.
    let end = bars.partition_point(|bar| bar.ts <= timestamp);
    end.checked_sub(1).and_then(|index| bars.get(index))
}

pub(super) fn build_runtime<'a>(markets: Vec<PortfolioBarMarket<'a>>) -> RuntimeMarkets<'a> {
    let mut runtime = RuntimeMarkets {
        by_index: Vec::with_capacity(markets.len()),
        index_by_instrument: BTreeMap::new(),
    };
    for market in markets {
        let fee_multiplier = if market.spec.product.supports_leverage() {
            market.spec.contract_size
        } else {
            SCALE
        };
        let mut seed_hasher = Fnv1a::new();
        seed_hasher.write_u64(market.seed);
        seed_hasher.write_text(&market.instrument.to_string());
        let mut matcher = BarMatchingEngine::new_with_latency_and_fee_multiplier(
            market.fill,
            market.fee,
            market.latency,
            seed_hasher.finish(),
            fee_multiplier,
        );
        matcher.set_inverse_fee_basis(market.spec.product.is_derivative() && market.spec.inverse);
        let market_index = runtime.by_index.len();
        runtime
            .index_by_instrument
            .insert(market.instrument.clone(), market_index);
        runtime.by_index.push(RuntimeMarket {
            instrument: market.instrument,
            priority: market.priority,
            bars: market.bars,
            spec: market.spec,
            matcher,
            margin: market.margin,
        });
    }
    runtime
}

pub(super) fn initialize_ledger(
    account_id: &str,
    currency: &str,
    initial_cash: Money,
    start: u64,
    has_start_settlement: bool,
) -> Result<(Ledger, EventLog), qx_core::QxError> {
    let mut ledger = Ledger::new();
    let deposit_id = ledger.deposit(account_id, currency, initial_cash, start)?;
    let deposit_entry = ledger
        .entry(deposit_id)
        .cloned()
        .ok_or_else(|| qx_core::QxError::Invariant("共享 Bar session 缺少初始入金".into()))?;
    let mut log = EventLog::new();
    let seq = log.alloc_seq();
    log.append(Event::new(
        seq,
        start,
        if has_start_settlement {
            qx_core::Priority::TIMER
        } else {
            qx_core::Priority::APPLY
        },
        EventKind::LedgerApplied {
            entry: deposit_entry,
        },
    ));
    Ok((ledger, log))
}
