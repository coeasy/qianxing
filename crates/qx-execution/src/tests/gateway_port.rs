use super::*;

#[test]
fn port_execution_service_registers_and_appends_standard_facts() {
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 7,
            venue_order_id: "remote-7".into(),
        }]),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
            .submit(port_order(7))
            .unwrap();
    assert_eq!(result.event_count, 1);
    assert_eq!(state.orders.len(), 1);
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.events[0].source_seq, 1);
    assert!(matches!(
        state.events[0].event,
        ExecutionEvent::Accepted { .. }
    ));
}

#[test]
fn port_execution_service_fails_closed_on_empty_venue_response() {
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(Vec::new()),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
            .submit(port_order(8));
    assert!(result.is_err());
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::ReconcileRequired { client_order_id: 8 })
    ));
}

#[test]
fn port_execution_service_fails_closed_on_invalid_venue_fact() {
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 999,
            venue_order_id: "wrong-order".into(),
        }]),
    };
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "port-worker", 10, &mut source_seq)
            .submit(port_order(9));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("非法下单事实"));
    assert!(matches!(
        state.events.last().map(|event| &event.event),
        Some(ExecutionEvent::ReconcileRequired { client_order_id: 9 })
    ));
}

#[test]
fn port_execution_service_rejects_before_registration_or_venue_side_effect() {
    let mut state = PortState::default();
    let mut venue = NeverCalledVenue;
    let mut source_seq = 0;
    let result =
        PortExecutionService::new(&mut venue, &mut state, "risk-worker", 10, &mut source_seq)
            .submit_with_risk(port_order(12), &RejectingRisk);

    assert_eq!(
        result.unwrap_err().to_string(),
        "账户级 RiskPort 拒绝订单: max_notional"
    );
    assert!(state.orders.is_empty());
    assert!(state.events.is_empty());
    assert_eq!(source_seq, 0);
}

/// 风控端口的两条通道必须各说各话（V13 §9.12 #172）。
///
/// 改前：两个生产 `RiskPort` 实现只在通过时写 `accepted: true`，拒绝改走 `Err`，
/// 于是 `submit_with_risk` 里「账户级 RiskPort 拒绝订单」那条播报在生产不可达，
/// 一次正常的风控拒绝被上层报成「执行失败」——运维据此去查链路，而链路是好的。
#[test]
fn canonical_risk_port_separates_verdict_from_port_failure() {
    let accepted_context = qx_risk::OrderRiskContext::default();
    assert_eq!(
        CanonicalRiskPort {
            context: &accepted_context,
        }
        .evaluate_order(&port_order(10))
        .unwrap(),
        RiskVerdict::Allow
    );

    // 带账户限额却没有产品规格：fail-closed 的拒绝（P0a 口径），不是端口故障。
    let rejected_context = qx_risk::OrderRiskContext {
        max_order_notional_raw: Some(1),
        ..qx_risk::OrderRiskContext::default()
    };
    let verdict = CanonicalRiskPort {
        context: &rejected_context,
    }
    .evaluate_order(&port_order(11))
    .expect("风控拒绝是端口给出的裁决，不是端口故障");
    let RiskVerdict::Reject { reason } = verdict else {
        panic!("缺规格的超限订单必须给出拒绝裁决: {verdict:?}")
    };
    // 拒绝理由要能就地复核：规则集版本 + 具体违规，二者缺一不可。
    assert!(
        reason.contains(qx_risk::ORDER_RISK_RULE_SET_VERSION),
        "拒绝理由必须点名规则集版本: {reason}"
    );
    assert!(
        reason.contains("TradingInstrumentSpec"),
        "拒绝理由要带违规原文: {reason}"
    );

    // 同一个拒绝经统一提交入口播报时不得说成「执行失败」，且不得留下伪订单。
    let mut state = PortState::default();
    let mut venue = NeverCalledVenue;
    let mut source_seq = 0;
    let error =
        PortExecutionService::new(&mut venue, &mut state, "risk-worker", 10, &mut source_seq)
            .submit_with_risk(
                port_order(11),
                &CanonicalRiskPort {
                    context: &rejected_context,
                },
            )
            .unwrap_err();
    assert!(
        error.starts_with("账户级 RiskPort 拒绝订单: "),
        "风控拒绝不得报成端口故障: {error}"
    );
    assert!(!error.contains("执行失败"), "两条通道又混了: {error}");
    assert!(state.orders.is_empty() && state.events.is_empty());
    assert_eq!(source_seq, 0);
}
