use super::targets::ScheduledTargets;
use super::{fill_model_for, CancellationToken, MultiLegFillModel};
use crate::AppError;
use qx_core::{InstrumentId, MakerTakerFeeModel, Money, TradingInstrumentSpec};
use qx_guanxing::Bar;
use qx_xingban::{
    BacktestConfig, BacktestEngine, DataTier, NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;

pub(super) struct LegRunSpec<'a> {
    pub(super) instrument: &'a InstrumentId,
    pub(super) bars: &'a [Bar],
    pub(super) targets: std::collections::BTreeMap<u64, i128>,
    pub(super) account_id: String,
    pub(super) initial_cash_raw: i128,
    pub(super) seed: u64,
    pub(super) currency: &'a str,
    pub(super) instrument_spec: Option<TradingInstrumentSpec>,
    pub(super) maker_fee_bps: i64,
    pub(super) taker_fee_bps: i64,
    pub(super) fill_model: MultiLegFillModel,
    pub(super) cancellation: Option<&'a CancellationToken>,
}

pub(super) fn run_leg(
    input: LegRunSpec<'_>,
) -> Result<Option<qx_xingban::BacktestReport>, AppError> {
    let mut strategy = ScheduledTargets {
        instrument: input.instrument.clone(),
        targets: input.targets,
        account_id: input.account_id,
    };
    let config = BacktestConfig {
        instrument: input.instrument.clone(),
        instrument_spec: input.instrument_spec.clone(),
        account_id: strategy.account_id.clone(),
        currency: input.currency.into(),
        initial_cash: Money::from_raw(input.initial_cash_raw),
        multiplier: 1,
        fill: fill_model_for(input.fill_model, input.instrument_spec.as_ref())?,
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: input.maker_fee_bps,
            taker_bp: input.taker_fee_bps,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: input.seed,
        risk: RiskGate::conservative_default(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let engine = BacktestEngine::new(config);
    match input.cancellation {
        Some(token) => engine
            .run_with_cancel(input.bars, &mut strategy, || token.is_cancelled())
            .map_err(|error| AppError::from_qx_error(&error)),
        None => engine
            .run(input.bars, &mut strategy)
            .map(Some)
            .map_err(|error| AppError::from_qx_error(&error)),
    }
}
