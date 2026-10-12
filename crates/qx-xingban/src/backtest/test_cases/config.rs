use super::*;

pub(super) fn simple_config() -> BacktestConfig {
    BacktestConfig {
        instrument: InstrumentId::parse("T.SIM").unwrap(),
        instrument_spec: None,
        account_id: "main".into(),
        currency: "USD".into(),
        initial_cash: Money::from_i64(1000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 1,
        risk: RiskGate::conservative_default(),
        virtual_trading: VirtualTradingConfig::default(),
    }
}
