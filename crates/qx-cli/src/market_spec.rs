//! CLI adapter for market-spec snapshots; parsing and binding live in qx-app.

use qx_core::{InstrumentId, TradingInstrumentSpec};

pub(crate) fn market_spec_from_value(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    ccxt_market_to_spec(instrument, market)
}

/// The historical CLI symbol remains as a thin application-layer adapter so existing module
/// boundaries and diagnostics keep one stable call path.
fn ccxt_market_to_spec(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    qx_app::market_spec_from_value(instrument, market).map_err(|error| error.message().to_string())
}

/// Compatibility projection used by existing CLI tests and worker diagnostics.
#[cfg(test)]
pub(crate) fn app_margin_model_from_market(
    market: &serde_json::Value,
) -> qx_app::BacktestMarginConfig {
    qx_app::backtest_margin_model_from_market(market)
}

/// Runtime adapter: application code owns parsing; qx-cli only binds the serializable model to
/// the legacy execution trait expected by the worker path.
#[cfg(test)]
pub(crate) fn ccxt_margin_rule_from_market(
    market: &serde_json::Value,
) -> Box<dyn crate::MarginRule> {
    match app_margin_model_from_market(market) {
        qx_app::BacktestMarginConfig::NoMargin => Box::new(crate::NoMargin),
        qx_app::BacktestMarginConfig::Leverage {
            leverage,
            maintenance_bp,
        } => Box::new(qx_xingban::LeverageMargin {
            leverage,
            maintenance_bp,
        }),
        qx_app::BacktestMarginConfig::Tiers { tiers } => {
            let tiers = tiers
                .into_iter()
                .map(|tier| qx_xingban::MarginTier {
                    max_notional: tier.max_notional,
                    initial_bp: tier.initial_bp,
                    maintenance_bp: tier.maintenance_bp,
                    max_leverage: tier.max_leverage,
                })
                .collect();
            Box::new(qx_xingban::TieredMargin { tiers })
        }
    }
}
