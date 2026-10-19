use qx_app::{
    plan_algo_execution, AlgoExecutionMethod, AlgoExecutionSpec, AppErrorCategory,
    CallerCapability, RunContext, ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
};
use qx_zhenlu::VwapVolumeBucket;

#[test]
fn twap_and_vwap_use_the_shared_planner_and_preserve_parent_quantity() {
    let context = RunContext::new(CallerCapability::Research, "parent-1");
    let twap = AlgoExecutionSpec {
        schema_version: ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
        parent_id: "parent-1".into(),
        total_quantity_raw: 11,
        start_ts: 10,
        end_ts: 40,
        method: AlgoExecutionMethod::Twap,
        child_count: Some(4),
        volume_profile: vec![],
    };
    let result = plan_algo_execution(&twap, &context).unwrap();
    assert_eq!(
        result
            .children
            .iter()
            .map(|child| child.quantity_raw)
            .sum::<i128>(),
        11
    );
    assert_eq!(
        result
            .children
            .iter()
            .map(|child| child.scheduled_ts)
            .collect::<Vec<_>>(),
        [10, 20, 30, 40]
    );

    let vwap = AlgoExecutionSpec {
        schema_version: ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
        parent_id: "parent-2".into(),
        total_quantity_raw: 10,
        start_ts: 100,
        end_ts: 300,
        method: AlgoExecutionMethod::Vwap,
        child_count: None,
        volume_profile: vec![
            VwapVolumeBucket {
                ts: 100,
                market_volume_raw: 1,
            },
            VwapVolumeBucket {
                ts: 200,
                market_volume_raw: 2,
            },
            VwapVolumeBucket {
                ts: 300,
                market_volume_raw: 3,
            },
        ],
    };
    let result = plan_algo_execution(&vwap, &context).unwrap();
    assert_eq!(
        result
            .children
            .iter()
            .map(|child| child.quantity_raw)
            .collect::<Vec<_>>(),
        [1, 3, 6]
    );
}

#[test]
fn algo_planning_rejects_invalid_wire_and_does_not_claim_order_submission() {
    let context = RunContext::new(CallerCapability::Research, "bad-parent");
    let invalid = AlgoExecutionSpec {
        schema_version: ALGO_EXECUTION_SPEC_SCHEMA_VERSION,
        parent_id: "bad-parent".into(),
        total_quantity_raw: 1,
        start_ts: 10,
        end_ts: 20,
        method: AlgoExecutionMethod::Twap,
        child_count: Some(2),
        volume_profile: vec![],
    };
    let error = plan_algo_execution(&invalid, &context).unwrap_err();
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert_eq!(error.correlation_id(), Some("bad-parent"));
}
