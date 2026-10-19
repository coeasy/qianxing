use crate::spec::{BacktestMarginConfig, BacktestSpec};
use crate::{AppError, AppErrorCategory};
use qx_core::{AShareFeeModel, FeeModel};
use qx_xingban::{
    BarVolumeParticipationLimit, BestPriceFillModel, FillModel, LatencyModel, LeverageMargin,
    MarginRule, MarginTier, NextBarOpenFillModel, NoMargin, OneTickSlippageFillModel, TieredMargin,
    VirtualTradingConfig, ZeroLatency,
};

pub(crate) struct ExecutionBinding {
    pub(crate) fee: Box<dyn FeeModel>,
    pub(crate) fee_descriptor: String,
    pub(crate) latency: Box<dyn LatencyModel>,
    pub(crate) virtual_trading: VirtualTradingConfig,
    pub(crate) fill: Box<dyn FillModel>,
    pub(crate) fill_name: String,
}

pub(crate) fn bind_execution(spec: &BacktestSpec) -> Result<ExecutionBinding, AppError> {
    let virtual_trading = VirtualTradingConfig {
        ashare_rules: spec.ashare_rules.clone(),
        ..VirtualTradingConfig::default()
    };
    let fee: Box<dyn FeeModel> = if let Some(rules) = &spec.ashare_rules {
        Box::new(AShareFeeModel {
            commission_bp: rules.commission_bp,
            min_commission: rules.min_commission,
            stamp_duty_bp: rules.stamp_duty_bp,
            transfer_fee_bp: rules.transfer_fee_bp,
        })
    } else if let Some(schedule) = spec.fee_schedule {
        Box::new(schedule.model())
    } else if let Some(costs) = &spec.execution_cost_rules {
        costs.fee_model()
    } else {
        Box::new(qx_xingban::MakerTakerFeeModel::default_maker_taker())
    };
    let fee_descriptor = fee.descriptor();
    let latency = spec
        .execution_cost_rules
        .as_ref()
        .map(|costs| costs.latency_model())
        .unwrap_or_else(|| Box::new(ZeroLatency));
    let fill: Box<dyn FillModel> = match spec.fill_model {
        Some(crate::spec::BacktestFillModel::BestPrice) => Box::new(BestPriceFillModel),
        Some(crate::spec::BacktestFillModel::OneTickSlippage) => {
            let market = spec.market_spec.as_ref().ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "one_tick_slippage requires market_spec",
                )
            })?;
            Box::new(OneTickSlippageFillModel {
                tick: market.price_tick,
            })
        }
        Some(crate::spec::BacktestFillModel::NextBarOpen) | None => Box::new(NextBarOpenFillModel),
    };
    let fill: Box<dyn FillModel> = match spec.max_bar_volume_participation_bps {
        Some(participation_bps) => Box::new(
            BarVolumeParticipationLimit::new(fill, participation_bps)
                .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?,
        ),
        None => fill,
    };
    let fill_name = fill.name().to_string();
    Ok(ExecutionBinding {
        fee,
        fee_descriptor,
        latency,
        virtual_trading,
        fill,
        fill_name,
    })
}

pub(crate) fn bind_margin(spec: &BacktestSpec) -> Box<dyn MarginRule> {
    match (&spec.margin_model, &spec.market_spec) {
        (Some(BacktestMarginConfig::NoMargin), _) => Box::new(NoMargin),
        (
            Some(BacktestMarginConfig::Leverage {
                leverage,
                maintenance_bp,
            }),
            _,
        ) => Box::new(LeverageMargin {
            leverage: *leverage,
            maintenance_bp: *maintenance_bp,
        }),
        (Some(BacktestMarginConfig::Tiers { tiers }), _) => Box::new(TieredMargin {
            tiers: tiers
                .iter()
                .map(|tier| MarginTier {
                    max_notional: tier.max_notional,
                    initial_bp: tier.initial_bp,
                    maintenance_bp: tier.maintenance_bp,
                    max_leverage: tier.max_leverage,
                })
                .collect(),
        }),
        (None, Some(market)) if market.product.supports_leverage() => Box::new(LeverageMargin {
            leverage: 1,
            maintenance_bp: i64::from(market.maintenance_margin_bps),
        }),
        _ => Box::new(NoMargin),
    }
}
