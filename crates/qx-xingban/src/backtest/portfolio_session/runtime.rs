use super::*;
use crate::venue::BarMatchingEngine;
use qx_core::{Fnv1a, SCALE};

pub(super) struct RuntimeMarket<'a> {
    pub(super) priority: u32,
    pub(super) bars: &'a [Bar],
    pub(super) spec: TradingInstrumentSpec,
    pub(super) matcher: BarMatchingEngine,
    pub(super) margin: Box<dyn MarginRule>,
}

pub(super) fn build_runtime<'a>(
    markets: Vec<PortfolioBarMarket<'a>>,
) -> BTreeMap<InstrumentId, RuntimeMarket<'a>> {
    let mut runtime = BTreeMap::new();
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
        runtime.insert(
            market.instrument.clone(),
            RuntimeMarket {
                priority: market.priority,
                bars: market.bars,
                spec: market.spec,
                matcher,
                margin: market.margin,
            },
        );
    }
    runtime
}
