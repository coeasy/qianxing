use super::*;

#[test]
fn hedge_recovery_submits_and_books_sub_nano_exact_compensation() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-exact-hedge-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let quantity = qx_core::ExactDecimal::parse("0.0000000003").unwrap();
    let source_order = Order {
        client_id: 791,
        instrument: InstrumentId::parse("TINY/USDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "tiny-binance".into(),
        trace: None,
        policy: None,
        exact_amounts: Some(qx_core::ExactOrderAmounts {
            quantity,
            limit_price: None,
            filled: qx_core::ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: qx_core::ExactDecimal::ZERO,
            market_reference_price: Some(qx_core::ExactDecimal::parse("100").unwrap()),
        }),
    };
    let other_order = port_order(792);
    let mut group = SpreadOrderGroup::new(
        "exact-small-coin-hedge",
        "tiny-pair",
        vec![
            SpreadOrderLeg {
                leg_id: "tiny-binance".into(),
                venue_id: "binance".into(),
                order: source_order.clone(),
            },
            SpreadOrderLeg {
                leg_id: "other-venue".into(),
                venue_id: "okx".into(),
                order: other_order.clone(),
            },
        ],
    )
    .unwrap();
    let mut filled_source = source_order;
    filled_source.status = OrderStatus::Cancelled;
    filled_source.exact_amounts.as_mut().unwrap().filled = quantity;
    group.apply_order_snapshot(&filled_source).unwrap();
    let mut cancelled_other = other_order;
    cancelled_other.status = OrderStatus::Cancelled;
    group.apply_order_snapshot(&cancelled_other).unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);

    let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
    store.save(&group).unwrap();
    let mut events = PortState::default();
    let mut router = PortRouter {
        exact_orders: true,
        ..PortRouter::default()
    };
    let mut source_seq = 0;
    let outcome = HedgeRecoveryWorker::new(
        group,
        &mut store,
        &mut router,
        &mut events,
        "tiny-recovery",
        900,
        &mut source_seq,
    )
    .unwrap()
    .execute()
    .unwrap();

    assert!(outcome.completed, "{:?}", outcome.errors);
    assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Hedged);
    assert_eq!(router.calls, vec!["binance"]);
    let compensation = &events.orders[0];
    assert_eq!(compensation.qty, Quantity::ZERO);
    assert_eq!(compensation.exact_amounts.unwrap().quantity, quantity);
    assert_eq!(compensation.filled_decimal(), quantity);
    assert!(compensation.policy.unwrap().reduce_only);
    assert_eq!(
        outcome.group.hedge_progress["tiny-binance"].hedged_qty_exact,
        Some(quantity)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cross_account_and_cross_venue_hedge_progress_converges_under_account_scoped_workers() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-cross-account-hedge-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut binance_leg = port_order(701);
    binance_leg.instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    binance_leg.account_id = "account-binance".into();
    binance_leg.qty = Quantity::from_i64(2);
    let mut okx_leg = port_order(702);
    okx_leg.instrument = InstrumentId::parse("ETH/USDT.OKX").unwrap();
    okx_leg.account_id = "account-okx".into();
    okx_leg.qty = Quantity::from_i64(2);
    okx_leg.side = Side::Sell;
    let mut group = SpreadOrderGroup::new(
        "cross-account-hedge",
        "basis-cross-account",
        vec![
            SpreadOrderLeg {
                leg_id: "binance-leg".into(),
                venue_id: "BINANCE".into(),
                order: binance_leg,
            },
            SpreadOrderLeg {
                leg_id: "okx-leg".into(),
                venue_id: "OKX".into(),
                order: okx_leg,
            },
        ],
    )
    .unwrap();
    group.begin_submission().unwrap();
    for (leg_id, order_id) in [("binance-leg", 701), ("okx-leg", 702)] {
        group.record_accepted(leg_id).unwrap();
        group
            .record_fill(
                leg_id,
                &qx_core::Fill {
                    order_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ..qx_core::Fill::default()
                },
            )
            .unwrap();
    }
    group.record_cancelled("binance-leg").unwrap();
    group.record_cancelled("okx-leg").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);

    let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
    store.save(&group).unwrap();

    let mut binance_state = PortState::default();
    let mut binance_router = PortRouter::default();
    let mut binance_seq = 0;
    let binance_outcome = HedgeRecoveryWorker::new(
        group.clone(),
        &mut store,
        &mut binance_router,
        &mut binance_state,
        "binance-recovery",
        500,
        &mut binance_seq,
    )
    .unwrap()
    .for_account_venue("account-binance", "BINANCE")
    .unwrap()
    .execute()
    .unwrap();

    assert!(!binance_outcome.completed);
    assert_eq!(
        binance_outcome.group.status,
        SpreadOrderGroupStatus::HedgeRequired
    );
    assert_eq!(binance_router.calls, vec!["BINANCE"]);
    assert_eq!(binance_state.orders.len(), 1);
    assert_eq!(binance_state.orders[0].account_id, "account-binance");
    assert_eq!(
        binance_outcome.group.hedge_progress["binance-leg"].hedged_qty,
        Quantity::from_i64(1)
    );
    assert!(!binance_outcome.group.hedge_progress.contains_key("okx-leg"));

    let mut okx_state = PortState::default();
    let mut okx_router = PortRouter::default();
    let mut okx_seq = 0;
    let okx_outcome = HedgeRecoveryWorker::new(
        binance_outcome.group,
        &mut store,
        &mut okx_router,
        &mut okx_state,
        "okx-recovery",
        501,
        &mut okx_seq,
    )
    .unwrap()
    .for_account_venue("account-okx", "OKX")
    .unwrap()
    .execute()
    .unwrap();

    assert!(okx_outcome.completed, "{:?}", okx_outcome.errors);
    assert_eq!(okx_outcome.group.status, SpreadOrderGroupStatus::Hedged);
    assert_eq!(okx_router.calls, vec!["OKX"]);
    assert_eq!(okx_state.orders.len(), 1);
    assert_eq!(okx_state.orders[0].account_id, "account-okx");
    assert!(okx_outcome.group.all_confirmed_exposure_hedged());
    assert_eq!(
        store.load("cross-account-hedge").unwrap().unwrap().status,
        SpreadOrderGroupStatus::Hedged
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn hedge_recovery_finishes_persisted_progress_after_restart_before_status_commit() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-hedge-status-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut first = port_order(711);
    first.account_id = "account-binance".into();
    first.qty = Quantity::from_i64(2);
    let mut second = port_order(712);
    second.account_id = "account-okx".into();
    second.qty = Quantity::from_i64(2);
    second.instrument = InstrumentId::parse("ETH/USDT.OKX").unwrap();
    let mut group = SpreadOrderGroup::new(
        "hedge-status-recovery",
        "basis-status-recovery",
        vec![
            SpreadOrderLeg {
                leg_id: "binance-leg".into(),
                venue_id: "BINANCE".into(),
                order: first,
            },
            SpreadOrderLeg {
                leg_id: "okx-leg".into(),
                venue_id: "OKX".into(),
                order: second,
            },
        ],
    )
    .unwrap();
    group.begin_submission().unwrap();
    for (leg_id, order_id) in [("binance-leg", 711), ("okx-leg", 712)] {
        group.record_accepted(leg_id).unwrap();
        group
            .record_fill(
                leg_id,
                &qx_core::Fill {
                    order_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ..qx_core::Fill::default()
                },
            )
            .unwrap();
    }
    group.record_cancelled("binance-leg").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
    // Model a restart after both fill-progress writes committed, but before the
    // process wrote the group-wide terminal status.
    group
        .record_compensation_fill("binance-leg", Quantity::from_i64(1))
        .unwrap();
    group
        .record_compensation_fill("okx-leg", Quantity::from_i64(1))
        .unwrap();
    assert!(group.compensation_targets().is_empty());

    let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
    store.save(&group).unwrap();
    let mut events = PortState::default();
    let mut router = PortRouter::default();
    let mut source_seq = 0;
    let mut outcome = HedgeRecoveryWorker::new(
        group,
        &mut store,
        &mut router,
        &mut events,
        "binance-recovery",
        800,
        &mut source_seq,
    )
    .unwrap()
    .for_account_venue("account-binance", "BINANCE")
    .unwrap()
    .execute()
    .unwrap();

    assert!(!outcome.completed, "an open source leg is not terminal");
    assert_eq!(outcome.group.status, SpreadOrderGroupStatus::HedgeRequired);
    assert!(router.calls.is_empty(), "recovery must not resubmit orders");
    outcome.group.record_cancelled("okx-leg").unwrap();
    store.save(&outcome.group).unwrap();

    let mut final_events = PortState::default();
    let mut final_router = PortRouter::default();
    let mut final_seq = 0;
    let outcome = HedgeRecoveryWorker::new(
        store.load("hedge-status-recovery").unwrap().unwrap(),
        &mut store,
        &mut final_router,
        &mut final_events,
        "okx-recovery",
        801,
        &mut final_seq,
    )
    .unwrap()
    .for_account_venue("account-okx", "OKX")
    .unwrap()
    .execute()
    .unwrap();

    assert!(outcome.completed, "{:?}", outcome.errors);
    assert_eq!(outcome.attempted, 0);
    assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Hedged);
    assert!(router.calls.is_empty(), "recovery must not resubmit orders");
    assert!(events.orders.is_empty());
    assert_eq!(
        store.load("hedge-status-recovery").unwrap().unwrap().status,
        SpreadOrderGroupStatus::Hedged
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn hedge_recovery_worker_is_idempotent_and_fails_closed_on_unknown_state() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
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

/// 对冲补偿提交也是提交：回包必须过 `PortExecutionService::submit` 同一道闸门，
/// 成交也必须带着那份冻结规格落库。
///
/// 修前的两处漏口：补偿路径既不做形状/精度校验（越界成交直接入账），也只追加裸
/// `Fill`，账簿拿不到 `contract_size` 就退回乘数 1，衍生腿方向对、数量差一个合约
/// 面值。规格解析失败时更要连提交都不发——没有规格就没有可核对的口径。
#[test]
fn hedge_compensation_submit_shares_the_submit_precision_gate_and_spec_bookkeeping() {
    const STEP: i128 = qx_core::SCALE / 10;
    const ON_TICK: i128 = 100 * qx_core::SCALE;
    const OFF_TICK: i128 = ON_TICK + qx_core::SCALE / 20;
    /// 故意不等于 1：乘数 1 记账正是本用例要排除的形态。
    const CONTRACT_SIZE: i128 = 10 * qx_core::SCALE;

    fn derivative_spec() -> qx_core::TradingInstrumentSpec {
        qx_core::TradingInstrumentSpec {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: CONTRACT_SIZE,
            linear: true,
            inverse: false,
            price_tick: qx_core::SCALE,
            qty_step: STEP,
            min_qty: STEP,
            min_notional_raw: 0,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 0,
            valid_to: None,
            exact_rules: None,
        }
    }

    /// 可设成交价的补偿路由：`PortRouter` 只会给 100，无法构造越界回包。
    struct PricedRouter {
        calls: Vec<String>,
        price: Price,
    }

    impl VenueRouterPort for PricedRouter {
        fn capabilities_for(
            &self,
            _venue_id: &str,
        ) -> Option<qx_zhenlu::venue_capabilities::VenueCapabilities> {
            Some(qx_zhenlu::venue_capabilities::PAPER_VENUE_CAPABILITIES)
        }

        fn execution_readiness_for(&self, _venue_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn submit_order(
            &mut self,
            venue_id: &str,
            order: Order,
            ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            self.calls.push(venue_id.to_string());
            Ok(vec![
                ExecutionEvent::Accepted {
                    client_order_id: order.client_id,
                    venue_order_id: format!("{venue_id}-{}", order.client_id),
                },
                ExecutionEvent::Fill(Box::new(qx_core::Fill {
                    order_id: order.client_id,
                    qty: order.qty,
                    price: self.price,
                    ts,
                    ..qx_core::Fill::default()
                })),
            ])
        }

        fn cancel_order(
            &mut self,
            venue_id: &str,
            client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            self.calls.push(format!("cancel:{venue_id}"));
            Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
        }
    }

    struct SpecGuard {
        spec: Option<qx_core::TradingInstrumentSpec>,
        resolve_error: Option<String>,
    }

    impl HedgeOrderValidator for SpecGuard {
        fn validate(&self, _order: &Order) -> Result<(), String> {
            Ok(())
        }

        fn instrument_spec(
            &self,
            _order: &Order,
        ) -> Result<Option<qx_core::TradingInstrumentSpec>, String> {
            match &self.resolve_error {
                Some(error) => Err(error.clone()),
                None => Ok(self.spec.clone()),
            }
        }
    }

    /// spot 腿确认成交、future 腿取消 → 净敞口落在 spot 腿上，组进入 HedgeRequired。
    fn hedge_required_group(group_id: &str) -> SpreadOrderGroup {
        let mut first = port_order(401);
        first.side = Side::Buy;
        let mut second = port_order(402);
        second.side = Side::Sell;
        let mut group = SpreadOrderGroup::new(
            group_id,
            "basis-recovery-gate",
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
        group
    }

    fn labels(events: &[ExecutionEventEnvelope]) -> Vec<&'static str> {
        events
            .iter()
            .map(|envelope| match &envelope.event {
                ExecutionEvent::Accepted { .. } => "accepted",
                ExecutionEvent::Fill(_) => "fill",
                ExecutionEvent::FillWithSpec { .. } => "fill-with-spec",
                ExecutionEvent::CancelPending { .. } => "cancel-pending",
                ExecutionEvent::Cancelled { .. } => "cancelled",
                ExecutionEvent::ReconcileRequired { .. } => "reconcile",
                ExecutionEvent::MarketQuote { .. } => "quote",
                ExecutionEvent::FeeAdjustment { .. } => "fee-adjustment",
            })
            .collect()
    }

    struct Scenario {
        label: &'static str,
        guard: SpecGuard,
        price_raw: i128,
        /// 期望的入账事实形状；`None` 表示本轮根本不该提交。
        expect_events: Option<Vec<&'static str>>,
        expect_completed: bool,
        expect_error: &'static str,
    }

    let scenarios = [
        Scenario {
            label: "off-tick-sync-fill-reconciles",
            guard: SpecGuard {
                spec: Some(derivative_spec()),
                resolve_error: None,
            },
            price_raw: OFF_TICK,
            expect_events: Some(vec!["reconcile"]),
            expect_completed: false,
            expect_error: "产品规格",
        },
        Scenario {
            label: "on-tick-sync-fill-books-with-spec",
            guard: SpecGuard {
                spec: Some(derivative_spec()),
                resolve_error: None,
            },
            price_raw: ON_TICK,
            expect_events: Some(vec!["accepted", "fill-with-spec"]),
            expect_completed: true,
            expect_error: "",
        },
        Scenario {
            label: "no-spec-configured-falls-back-to-plain-fill",
            guard: SpecGuard {
                spec: None,
                resolve_error: None,
            },
            price_raw: OFF_TICK,
            expect_events: Some(vec!["accepted", "fill"]),
            expect_completed: true,
            expect_error: "",
        },
        Scenario {
            label: "spec-resolution-failure-blocks-submit",
            guard: SpecGuard {
                spec: None,
                resolve_error: Some("规格文件缺失".into()),
            },
            price_raw: ON_TICK,
            expect_events: Some(vec![]),
            expect_completed: false,
            expect_error: "产品规格",
        },
    ];

    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "qianxing-hedge-gate-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    for scenario in scenarios {
        let group_id = format!("hedge-gate-{}", scenario.label);
        let mut store = FileSpreadOrderGroupStore::new(root.join(&group_id)).unwrap();
        let mut state = PortState::default();
        let mut router = PricedRouter {
            calls: Vec::new(),
            price: Price::from_raw(scenario.price_raw),
        };
        let mut source_seq = 0;
        let outcome = HedgeRecoveryWorker::new(
            hedge_required_group(&group_id),
            &mut store,
            &mut router,
            &mut state,
            "hedge-worker",
            500,
            &mut source_seq,
        )
        .unwrap()
        .execute_with_validator(Some(&scenario.guard))
        .unwrap();
        let seen = labels(&state.events);
        assert_eq!(
            seen,
            scenario.expect_events.unwrap(),
            "{}: 入账事实形状",
            scenario.label
        );
        assert_eq!(
            outcome.completed, scenario.expect_completed,
            "{}: {seen:?} {:?}",
            scenario.label, outcome.errors
        );
        if scenario.expect_error.is_empty() {
            assert!(
                outcome.errors.is_empty(),
                "{}: {:?}",
                scenario.label,
                outcome.errors
            );
        } else {
            assert!(
                outcome
                    .errors
                    .iter()
                    .any(|error| error.contains(scenario.expect_error)),
                "{}: 报错要点名 {:?}，实际 {:?}",
                scenario.label,
                scenario.expect_error,
                outcome.errors
            );
        }
        let persisted = store.load(&group_id).unwrap().unwrap();
        let expected_group_status = if scenario.expect_completed {
            SpreadOrderGroupStatus::Hedged
        } else if scenario.label == "off-tick-sync-fill-reconciles" {
            SpreadOrderGroupStatus::ReconcileRequired
        } else {
            SpreadOrderGroupStatus::HedgeRequired
        };
        assert_eq!(
            persisted.status, expected_group_status,
            "{}: 未知/未验证结果不得把组标记为已对冲",
            scenario.label
        );
        match seen.as_slice() {
            ["accepted", "fill-with-spec"] => {
                let spec = state
                    .events
                    .iter()
                    .find_map(|envelope| match &envelope.event {
                        ExecutionEvent::FillWithSpec { spec, .. } => Some(spec.clone()),
                        _ => None,
                    })
                    .expect("在 tick 上的补偿成交必须带着规格落库");
                assert_eq!(spec.contract_size, CONTRACT_SIZE);
            }
            ["reconcile"] => {
                let correlation = &state.events[0].correlation_id;
                assert!(
                    correlation.contains("submit-fill-out-of-spec"),
                    "{}: 待对账原因段要可检索，实际 {correlation}",
                    scenario.label
                );
            }
            [] => assert!(router.calls.is_empty(), "{}: 不该提交", scenario.label),
            _ => {}
        }
    }
    let _ = std::fs::remove_dir_all(root);
}
