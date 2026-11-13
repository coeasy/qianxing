use super::*;

struct PassiveLimitBuyOnce {
    emitted: bool,
}

impl OrderBookStrategy for PassiveLimitBuyOnce {
    fn on_order_book(
        &mut self,
        snapshot: &OrderBookSnapshot,
        _position: i128,
    ) -> Result<Vec<Order>, String> {
        if self.emitted {
            return Ok(Vec::new());
        }
        self.emitted = true;
        Ok(vec![Order {
            client_id: 700,
            instrument: snapshot.instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(99)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }])
    }
}

#[test]
fn fifo_queue_report_exposes_same_data_limit_fill_rate_sensitivity() {
    let snapshots = [
        OrderBookSnapshot {
            ts: 1,
            sequence: 1,
            bids: vec![crate::BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(99),
                qty: Quantity::from_i64(8),
            }],
            ..snapshot(1, 100)
        },
        OrderBookSnapshot {
            ts: 2,
            sequence: 2,
            bids: vec![crate::BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(99),
                qty: Quantity::from_i64(8),
            }],
            ..snapshot(2, 100)
        },
        OrderBookSnapshot {
            ts: 3,
            sequence: 3,
            bids: vec![crate::BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(99),
                qty: Quantity::from_i64(2),
            }],
            ..snapshot(3, 100)
        },
        OrderBookSnapshot {
            ts: 4,
            sequence: 4,
            bids: vec![],
            ..snapshot(4, 100)
        },
    ];
    let baseline_model = OrderBookExecutionModel {
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 5_000,
        queue_position_model: crate::QueuePositionModel::Disabled,
        market_impact_bps: 0,
    };
    let baseline = OrderBookBacktestEngine::new(l2_config(0))
        .with_execution_model(baseline_model)
        .run(&snapshots, &mut PassiveLimitBuyOnce { emitted: false })
        .unwrap();
    let queue_model = OrderBookExecutionModel {
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 5_000,
        queue_position_model: crate::QueuePositionModel::FifoVolumeEstimate {
            volume_advance_bps: 10_000,
        },
        market_impact_bps: 0,
    };
    let estimated = OrderBookBacktestEngine::new(l2_config(0))
        .with_execution_model(queue_model)
        .run(&snapshots, &mut PassiveLimitBuyOnce { emitted: false })
        .unwrap();
    let sensitivity =
        crate::QueuePositionSensitivityReport::compare(&baseline, &estimated).unwrap();

    assert!(baseline
        .execution_attempts
        .iter()
        .any(|attempt| attempt.reason == crate::ExecutionAttemptReason::QueueAheadRemains));
    assert!(estimated.execution_attempts.iter().any(|attempt| {
        matches!(
            attempt.reason,
            crate::ExecutionAttemptReason::Filled | crate::ExecutionAttemptReason::PartiallyFilled
        )
    }));
    let first_queue_observation = estimated
        .execution_attempts
        .iter()
        .find(|attempt| attempt.order_id == 700)
        .unwrap();
    assert_eq!(
        first_queue_observation.reason,
        crate::ExecutionAttemptReason::QueueBaselineUnavailable
    );
    assert_eq!(
        first_queue_observation.displayed_qty_raw,
        Some(Quantity::from_i64(8).raw())
    );
    assert_eq!(first_queue_observation.displayed_reduction_raw, None);
    assert_eq!(first_queue_observation.estimated_volume_raw, None);
    let attempt_json = serde_json::to_string(&estimated.execution_attempts).unwrap();
    let attempts: Vec<crate::ExecutionAttempt> = serde_json::from_str(&attempt_json).unwrap();
    assert_eq!(attempts, estimated.execution_attempts);
    assert!(attempt_json.contains("queue_baseline_unavailable"));

    assert_eq!(baseline.limit_order_fill_rate_bps(), Some(0));
    assert_eq!(estimated.limit_order_fill_rate_bps(), Some(10_000));
    assert!(baseline
        .model_descriptors
        .iter()
        .any(|value| value == "limit_order_fill_rate_bps=0"));
    assert!(estimated
        .model_descriptors
        .iter()
        .any(|value| value == "limit_order_fill_rate_bps=10000"));
    assert_ne!(baseline.result_hash(), estimated.result_hash());
    assert_eq!(sensitivity.baseline_limit_order_fill_rate_bps, 0);
    assert_eq!(sensitivity.estimated_limit_order_fill_rate_bps, 10_000);
    assert_eq!(sensitivity.delta_bps, 10_000);
    assert!(sensitivity.descriptor.contains("delta_bps=10000"));
    let sensitivity_json: serde_json::Value =
        serde_json::from_str(&sensitivity.to_json().unwrap()).unwrap();
    assert_eq!(
        sensitivity_json["input_data_hash"],
        baseline.input_data_hash
    );
    assert_eq!(sensitivity_json["delta_bps"], 10_000);
    let mut mismatched_input = estimated;
    mismatched_input.model_descriptors[1] =
        mismatched_input.model_descriptors[1].replace("market_impact_bps=0", "market_impact_bps=1");
    assert!(
        crate::QueuePositionSensitivityReport::compare(&baseline, &mismatched_input)
            .unwrap_err()
            .contains("identical matching parameters")
    );
    mismatched_input.input_data_hash += 1;
    assert!(
        crate::QueuePositionSensitivityReport::compare(&baseline, &mismatched_input)
            .unwrap_err()
            .contains("identical input_data_hash")
    );
}

#[test]
fn fifo_queue_resets_missing_level_and_rebases_before_estimating_again() {
    let snapshot_with_bid = |ts, qty: Option<i64>| {
        let mut value = snapshot(ts, 100);
        value.bids = qty
            .map(|qty| crate::BookLevel {
                price_exact: None,
                qty_exact: None,
                price: Price::from_i64(99),
                qty: Quantity::from_i64(qty),
            })
            .into_iter()
            .collect();
        value
    };
    let snapshots = [
        snapshot_with_bid(1, Some(5)),
        snapshot_with_bid(2, Some(5)), // First visible observation establishes no baseline.
        snapshot_with_bid(3, Some(5)), // A valid zero reduction must remain zero.
        snapshot_with_bid(4, None),    // A missing finite-depth level invalidates the baseline.
        snapshot_with_bid(5, Some(5)), // Reappearance is a new baseline, not traded volume.
        snapshot_with_bid(6, Some(4)), // Only the new 5 -> 4 reduction is estimated.
        snapshot_with_bid(7, Some(2)),
    ];
    let model = OrderBookExecutionModel {
        fee_bps: 0,
        latency_snapshots: 0,
        queue_position_bps: 0,
        queue_position_model: crate::QueuePositionModel::FifoVolumeEstimate {
            volume_advance_bps: 10_000,
        },
        market_impact_bps: 0,
    };
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .with_execution_model(model)
        .run(&snapshots, &mut PassiveLimitBuyOnce { emitted: false })
        .unwrap();
    let attempt_at = |ts| {
        report
            .execution_attempts
            .iter()
            .find(|attempt| attempt.ts == ts && attempt.order_id == 700)
            .unwrap_or_else(|| panic!("missing queue attempt at ts={ts}"))
    };

    let first = attempt_at(2);
    assert_eq!(
        first.reason,
        crate::ExecutionAttemptReason::QueueBaselineUnavailable
    );
    assert_eq!(first.displayed_reduction_raw, None);
    assert_eq!(first.estimated_volume_raw, None);

    let unchanged = attempt_at(3);
    assert_eq!(
        unchanged.reason,
        crate::ExecutionAttemptReason::NoVisibleLiquidity
    );
    assert_eq!(unchanged.displayed_reduction_raw, Some(0));
    assert_eq!(unchanged.estimated_volume_raw, Some(0));
    assert_eq!(unchanged.filled_qty_raw, 0);

    for ts in [4, 5] {
        let attempt = attempt_at(ts);
        assert_eq!(
            attempt.reason,
            crate::ExecutionAttemptReason::QueueBaselineUnavailable,
            "ts={ts} must not infer volume across a missing-level boundary"
        );
        assert_eq!(attempt.displayed_reduction_raw, None);
        assert_eq!(attempt.estimated_volume_raw, None);
        assert_eq!(attempt.filled_qty_raw, 0);
    }

    let after_rebase = attempt_at(6);
    assert_eq!(
        after_rebase.reason,
        crate::ExecutionAttemptReason::PartiallyFilled
    );
    assert_eq!(
        after_rebase.displayed_reduction_raw,
        Some(Quantity::from_i64(1).raw())
    );
    assert_eq!(
        after_rebase.estimated_volume_raw,
        Some(Quantity::from_i64(1).raw())
    );
    assert_eq!(after_rebase.filled_qty_raw, Quantity::from_i64(1).raw());

    let next_reduction = attempt_at(7);
    assert_eq!(next_reduction.reason, crate::ExecutionAttemptReason::Filled);
    assert_eq!(
        next_reduction.displayed_reduction_raw,
        Some(Quantity::from_i64(2).raw())
    );
    assert_eq!(
        next_reduction.estimated_volume_raw,
        Some(Quantity::from_i64(2).raw())
    );
    assert_eq!(next_reduction.filled_qty_raw, Quantity::from_i64(1).raw());
    assert_eq!(report.fills.len(), 2);
}
