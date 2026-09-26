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

/// 一条不带 `spread_group_id` 的控制面 `SubmitOrder`，载荷与 `port_order` 同源。
fn control_submit_command(client_id: u64) -> ControlCommand {
    let order = port_order(client_id);
    ControlCommand {
        command_id: client_id,
        request_id: format!("gate-port-{client_id}"),
        operator_id: "gate-port-test".into(),
        reason: "single leg".into(),
        kind: CommandKind::SubmitOrder,
        target: order.client_id.to_string(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    }
}

/// 控制面来源的订单登记必须点名 `control:{command_id}`，两条控制入口各自都要有；
/// 不走控制面的 `submit` 则退回 worker 自己的形状。这一格是"这笔订单由谁下单"的
/// 唯一落盘位置：网关把关联号交给 `register_order` 后就不再回头改它。
#[test]
fn control_submits_register_correlation_and_plain_submit_keeps_worker_shape() {
    let mut source_seq = 0_u64;

    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 13,
            venue_order_id: "remote-13".into(),
        }]),
    };
    ExecutionGateway::new(&mut venue, &mut state, "gate-worker", 10, &mut source_seq)
        .submit_command(&control_submit_command(13))
        .unwrap();

    let mut risk_state = PortState::default();
    let mut risk_venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 14,
            venue_order_id: "remote-14".into(),
        }]),
    };
    let risk = qx_risk::OrderRiskContext::default();
    ExecutionGateway::new(
        &mut risk_venue,
        &mut risk_state,
        "gate-worker",
        10,
        &mut source_seq,
    )
    .submit_command_with_risk(
        &control_submit_command(14),
        &CanonicalRiskPort { context: &risk },
    )
    .unwrap();

    let mut plain_state = PortState::default();
    let mut plain_venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 15,
            venue_order_id: "remote-15".into(),
        }]),
    };
    ExecutionGateway::new(
        &mut plain_venue,
        &mut plain_state,
        "gate-worker",
        10,
        &mut source_seq,
    )
    .submit(port_order(15))
    .unwrap();

    assert_eq!(
        state.registrations,
        vec![(13, Some("control:13".to_string()))],
        "submit_command 必须把订单登记到控制面命令上"
    );
    assert_eq!(
        risk_state.registrations,
        vec![(14, Some("control:14".to_string()))],
        "带风控的提交入口同样要点名控制面命令"
    );
    assert_eq!(
        plain_state.registrations,
        vec![(15, Some("gate-worker:submit:15".to_string()))],
        "非控制面提交不能被写成 control:，否则两种来源再也分不开"
    );
}

#[test]
fn canonical_risk_port_is_available_without_zhenlu_context_conversion() {
    let accepted_context = qx_risk::OrderRiskContext::default();
    let accepted = CanonicalRiskPort {
        context: &accepted_context,
    }
    .evaluate_order(&port_order(10))
    .unwrap();
    assert!(accepted.accepted);
    assert_eq!(accepted.reason_code, "accepted");

    let rejected_context = qx_risk::OrderRiskContext {
        max_order_notional_raw: Some(1),
        ..qx_risk::OrderRiskContext::default()
    };
    let rejected = CanonicalRiskPort {
        context: &rejected_context,
    }
    .evaluate_order(&port_order(11));
    assert!(rejected.is_err());
}
