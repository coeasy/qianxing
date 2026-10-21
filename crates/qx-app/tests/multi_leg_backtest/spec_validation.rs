use super::*;

#[test]
fn multi_leg_spec_rejects_unrepresentable_combined_initial_cash() {
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("multi-leg-cash-overflow").into(),
            primary_bars_path: ("primary.json").into(),
            reference_bars_path: ("reference.json").into(),
        },
        "USDT",
        i128::MAX,
        1,
        7,
        "out",
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "cash-overflow"),
    );
    let error = spec
        .validate()
        .expect_err("combined cash must fit the signed ledger representation");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.to_string().contains("初始现金合计溢出"));
}
