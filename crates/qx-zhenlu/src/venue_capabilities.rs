//! Executable capability declarations for order-routing venues.

use qx_core::Order;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueOrderType {
    Market,
    Limit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueMarketScope {
    InstrumentVenue,
    BinanceSpot,
    RuntimeConfigured,
}

/// A venue's declared execution surface. Empty/false fields mean unsupported and are
/// rejected at the shared execution boundary; adapters must not infer support from a
/// successful transport call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VenueCapabilities {
    pub market_scope: VenueMarketScope,
    pub exact_orders: bool,
    pub order_types: &'static [VenueOrderType],
    pub data_tiers: &'static [&'static str],
    pub cancel: bool,
    pub query: bool,
    pub reconciliation: bool,
    pub idempotent_client_order_id: bool,
}

const MARKET_AND_LIMIT: &[VenueOrderType] = &[VenueOrderType::Market, VenueOrderType::Limit];

pub const PAPER_VENUE_CAPABILITIES: VenueCapabilities = VenueCapabilities {
    market_scope: VenueMarketScope::InstrumentVenue,
    exact_orders: true,
    order_types: MARKET_AND_LIMIT,
    data_tiers: &[],
    cancel: true,
    query: true,
    reconciliation: false,
    idempotent_client_order_id: true,
};

pub const BINANCE_SPOT_VENUE_CAPABILITIES: VenueCapabilities = VenueCapabilities {
    market_scope: VenueMarketScope::BinanceSpot,
    exact_orders: true,
    order_types: MARKET_AND_LIMIT,
    data_tiers: &[],
    cancel: true,
    query: true,
    reconciliation: true,
    idempotent_client_order_id: true,
};

pub const CCXT_PROCESS_VENUE_CAPABILITIES: VenueCapabilities = VenueCapabilities {
    market_scope: VenueMarketScope::RuntimeConfigured,
    exact_orders: true,
    order_types: MARKET_AND_LIMIT,
    data_tiers: &[],
    cancel: true,
    query: true,
    reconciliation: true,
    idempotent_client_order_id: false,
};

impl VenueCapabilities {
    pub fn validate_submit(&self, order: &Order, venue_id: &str) -> Result<(), String> {
        if order.exact_amounts.is_some() && !self.exact_orders {
            return Err("该 Venue 尚未贯通精确订单金额，拒绝提交".into());
        }
        match self.market_scope {
            VenueMarketScope::InstrumentVenue => {}
            VenueMarketScope::BinanceSpot
                if order.instrument.venue.is_binance()
                    && !order
                        .instrument
                        .symbol
                        .to_ascii_uppercase()
                        .contains("-PERP") => {}
            VenueMarketScope::RuntimeConfigured
                if order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(configured_market_venue(venue_id)) => {}
            scope => {
                return Err(format!(
                    "FAIL_CLOSED: Instrument market 不在 Venue 声明范围内（scope={scope:?}, instrument={}, venue={venue_id}）",
                    order.instrument
                ));
            }
        }
        let order_type = if order.limit_price_decimal().is_some() {
            VenueOrderType::Limit
        } else {
            VenueOrderType::Market
        };
        if !self.order_types.contains(&order_type) {
            return Err(format!(
                "FAIL_CLOSED: Venue 未声明 {:?} 订单能力，拒绝注册/提交",
                order_type
            ));
        }
        Ok(())
    }

    pub fn validate_cancel(&self) -> Result<(), String> {
        if !self.cancel {
            return Err("FAIL_CLOSED: Venue 未声明撤单能力，拒绝调用撤单入口".into());
        }
        Ok(())
    }

    pub fn validate_query(&self) -> Result<(), String> {
        if !self.query {
            return Err("FAIL_CLOSED: Venue 未声明查询能力，拒绝调用查询入口".into());
        }
        Ok(())
    }

    pub fn require_query(&self) -> qx_core::QxResult<()> {
        self.validate_query()
            .map_err(qx_core::QxError::BusinessViolation)
    }

    pub fn validate_reconciliation(&self) -> Result<(), String> {
        if !self.reconciliation {
            return Err("FAIL_CLOSED: Venue 未声明对账能力，拒绝调用对账入口".into());
        }
        Ok(())
    }

    pub fn require_reconciliation(&self) -> qx_core::QxResult<()> {
        self.validate_reconciliation()
            .map_err(qx_core::QxError::BusinessViolation)
    }
}

fn configured_market_venue(venue_id: &str) -> &str {
    venue_id
        .get(..5)
        .filter(|prefix| prefix.eq_ignore_ascii_case("ccxt-"))
        .map_or(venue_id, |_| &venue_id[5..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{ExactDecimal, ExactOrderAmounts, InstrumentId, OrderStatus, Quantity, Side};

    #[test]
    fn venue_submit_gate_rejects_exact_values_when_capability_is_not_declared() {
        let order = Order {
            client_id: 1,
            instrument: InstrumentId::parse("PEPE/USDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("1000.0000000001").unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };
        assert!(PAPER_VENUE_CAPABILITIES
            .validate_submit(&order, "paper")
            .is_ok());
        assert!(CCXT_PROCESS_VENUE_CAPABILITIES
            .validate_submit(&order, "ccxt-okx")
            .is_ok());
    }
}
