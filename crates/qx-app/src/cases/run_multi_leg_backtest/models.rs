//! Named shared fill-model construction for two-leg runs.

use super::{invalid, MultiLegFillModel};
use crate::AppError;
use qx_core::TradingInstrumentSpec;
use qx_xingban::{BestPriceFillModel, FillModel, NextBarOpenFillModel, OneTickSlippageFillModel};

pub(in crate::cases) fn fill_model_for(
    kind: MultiLegFillModel,
    market_spec: Option<&TradingInstrumentSpec>,
) -> Result<Box<dyn FillModel>, AppError> {
    Ok(match kind {
        MultiLegFillModel::NextBarOpen => Box::new(NextBarOpenFillModel),
        MultiLegFillModel::BestPrice => Box::new(BestPriceFillModel),
        MultiLegFillModel::OneTickSlippage => {
            let spec = market_spec.ok_or_else(|| {
                invalid("one_tick_slippage 要求两条腿都提供含真实 price_tick 的 market spec")
            })?;
            Box::new(OneTickSlippageFillModel {
                tick: spec.price_tick,
            })
        }
    })
}

pub(in crate::cases) fn fill_model_name(kind: MultiLegFillModel) -> &'static str {
    match kind {
        MultiLegFillModel::NextBarOpen => "next_bar_open",
        MultiLegFillModel::BestPrice => "best_price",
        MultiLegFillModel::OneTickSlippage => "one_tick_slippage",
    }
}
