use super::*;

#[test]
fn hedge_recovery_worker_is_idempotent_and_fails_closed_on_unknown_state() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-hedge-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut first = port_order(401);
    first.side = Side::Buy;
    let mut second = port_order(402);
    second.side = Side::Sell;
    let mut group = SpreadOrderGroup::new(
        "hedge-recovery-1",
        "basis-recovery",
        vec![
            SpreadOrderLeg {
                leg_id: "spot".into(),
                venue_id: "binance".into(),
                order: first,
            },
            SpreadOrderLeg {
                leg_id: "future".into(),
                venue_id: "okx".into(),
                order: second,
            },
        ],
    )
    .unwrap();
    group.begin_submission().unwrap();
    group.record_accepted("spot").unwrap();
    group
        .record_fill(
            "spot",
            &qx_core::Fill {
                order_id: 401,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ..qx_core::Fill::default()
            },
        )
        .unwrap();
    group.record_cancelled("future").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
    let mut risk_blocked_group = group.clone();
    risk_blocked_group.group_id = "hedge-recovery-risk-blocked".into();

    let mut state = PortState::default();
    let mut router = PortRouter::default();
    let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
    let other_token = store
        .try_claim("hedge-recovery-1", "other-worker", 500, 30_000)
        .unwrap()
        .expect("other worker should acquire claim");
    let mut blocked_state = PortState::default();
    let mut blocked_router = PortRouter::default();
    let mut blocked_seq = 0;
    let blocked = HedgeRecoveryWorker::new(
        group.clone(),
        &mut store,
        &mut blocked_router,
        &mut blocked_state,
        "hedge-worker",
        501,
        &mut blocked_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(!blocked.completed);
    assert!(blocked.errors[0].contains("其他恢复 owner"));
    store
        .release_claim("hedge-recovery-1", "other-worker", other_token)
        .unwrap();
    let mut source_seq = 0;
    let outcome = HedgeRecoveryWorker::new(
        group,
        &mut store,
        &mut router,
        &mut state,
        "hedge-worker",
        500,
        &mut source_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(outcome.completed);
    assert_eq!(outcome.attempted, 1);
    assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Hedged);
    assert_eq!(router.calls, vec!["binance"]);
    assert_eq!(state.orders.len(), 1);
    assert!(state.orders[0].policy.unwrap().reduce_only);
    let persisted = store.load("hedge-recovery-1").unwrap().unwrap();
    assert_eq!(persisted.status, SpreadOrderGroupStatus::Hedged);

    let mut guarded_state = PortState::default();
    let mut guarded_router = PortRouter::default();
    let mut guarded_store = FileSpreadOrderGroupStore::new(root.join("guarded-groups")).unwrap();
    let guarded_validator =
        |_order: &Order| -> Result<(), String> { Err("risk snapshot unavailable".into()) };
    let guarded = HedgeRecoveryWorker::new(
        risk_blocked_group,
        &mut guarded_store,
        &mut guarded_router,
        &mut guarded_state,
        "hedge-worker",
        500,
        &mut source_seq,
    )
    .unwrap()
    .execute_with_validator(Some(&guarded_validator))
    .unwrap();
    assert!(!guarded.completed);
    assert!(guarded.errors[0].contains("风控"));
    assert!(guarded_router.calls.is_empty());
    assert_eq!(guarded.attempted, 0);

    let mut retry_state = PortState::default();
    let mut retry_router = PortRouter::default();
    let mut retry_seq = source_seq;
    let retry = HedgeRecoveryWorker::new(
        outcome.group.clone(),
        &mut store,
        &mut retry_router,
        &mut retry_state,
        "hedge-worker",
        501,
        &mut retry_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(retry.completed);
    assert!(retry_router.calls.is_empty());

    let mut unknown = outcome.group;
    unknown.group_id = "hedge-recovery-unknown".into();
    unknown.status = SpreadOrderGroupStatus::ReconcileRequired;
    let mut unknown_state = PortState::default();
    let mut unknown_router = PortRouter::default();
    let mut unknown_seq = retry_seq;
    let blocked = HedgeRecoveryWorker::new(
        unknown,
        &mut store,
        &mut unknown_router,
        &mut unknown_state,
        "hedge-worker",
        502,
        &mut unknown_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(!blocked.completed);
    assert!(blocked.errors[0].contains("对账"));
    assert!(unknown_router.calls.is_empty());
    let _ = std::fs::remove_dir_all(root);
}
