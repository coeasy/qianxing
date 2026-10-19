//! Shared deterministic defaults for product margin and short-selling policy.

use crate::{MarginMode, TradingProduct};

/// Resolve the runtime default margin mode for a product.
pub fn margin_mode_for_product(
    product: TradingProduct,
    configured: Option<MarginMode>,
) -> MarginMode {
    configured.unwrap_or(if product == TradingProduct::Spot {
        MarginMode::Cash
    } else {
        MarginMode::Cross
    })
}

/// Resolve short-selling permission after the caller has parsed its configuration.
/// Cash accounts always prohibit shorts; otherwise explicit policy wins and the
/// default only enables shorts for derivatives.
pub fn short_selling_enabled(
    product: TradingProduct,
    margin_mode: MarginMode,
    configured: Option<bool>,
) -> bool {
    margin_mode != MarginMode::Cash && configured.unwrap_or(product.is_derivative())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_defaults_and_cash_margin_are_conservative() {
        assert_eq!(
            margin_mode_for_product(TradingProduct::Spot, None),
            MarginMode::Cash
        );
        assert_eq!(
            margin_mode_for_product(TradingProduct::Perpetual, None),
            MarginMode::Cross
        );
        assert!(!short_selling_enabled(
            TradingProduct::Spot,
            MarginMode::Cash,
            Some(true)
        ));
        assert!(!short_selling_enabled(
            TradingProduct::Perpetual,
            MarginMode::Cash,
            None
        ));
    }

    #[test]
    fn explicit_permission_and_derivative_default_are_applied() {
        assert!(short_selling_enabled(
            TradingProduct::Perpetual,
            MarginMode::Cross,
            None
        ));
        assert!(!short_selling_enabled(
            TradingProduct::Margin,
            MarginMode::Cross,
            None
        ));
        assert!(short_selling_enabled(
            TradingProduct::Margin,
            MarginMode::Isolated,
            Some(true)
        ));
        assert!(!short_selling_enabled(
            TradingProduct::Perpetual,
            MarginMode::Cross,
            Some(false)
        ));
    }
}
