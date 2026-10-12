use qx_sdk::{
    CallerCapability, CapitalAllocationAccount, CapitalAllocationRequest, CapitalAllocationSpec,
    Client,
};

#[test]
fn independent_consumer_can_plan_portfolio_capital_through_the_facade_only() {
    let client = Client::new(CallerCapability::Research, "portfolio-plan");
    let spec = CapitalAllocationSpec {
        schema_version: 1,
        portfolio_id: "portfolio-sdk".into(),
        portfolio_budget_raw: 70,
        accounts: vec![CapitalAllocationAccount {
            account_id: "acct-sdk".into(),
            available_cash_raw: 100,
        }],
        requests: vec![
            CapitalAllocationRequest {
                strategy_id: "strategy-b".into(),
                account_id: "acct-sdk".into(),
                requested_cash_raw: 60,
                priority: 1,
            },
            CapitalAllocationRequest {
                strategy_id: "strategy-a".into(),
                account_id: "acct-sdk".into(),
                requested_cash_raw: 60,
                priority: 0,
            },
        ],
    };
    let result = client.plan_capital_allocation(&spec).expect("规划成功");
    assert_eq!(result.allocations[0].strategy_id, "strategy-a");
    assert_eq!(result.allocations[0].allocated_cash_raw, 60);
    assert_eq!(result.allocations[1].allocated_cash_raw, 10);
}
