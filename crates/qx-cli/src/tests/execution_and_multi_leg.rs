use super::*;

#[test]
fn dry_run_submit_order_is_audited_without_credentials_or_network() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-submit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.config_fingerprint = None;
    config.environment = "test".into();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.event_log_segment_events = Some(2);
    config.storage.backend = StorageBackend::Files;
    config.storage.consistency = qx_runtime::StorageConsistency::LocalDurable;
    config.storage.sqlite_path = None;
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let order = mk_order(
        7001,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let mut payload = BTreeMap::new();
    payload.insert("order_json".into(), serde_json::to_string(&order).unwrap());
    let command = ControlCommand {
        command_id: 7001,
        request_id: "submit-7001".into(),
        operator_id: "ops".into(),
        reason: "dry run integration".into(),
        kind: CommandKind::SubmitOrder,
        target: "7001".into(),
        payload,
        permission: Permission::Trading,
        dry_run: true,
    };
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

    run_binance_submit_order(&config_path, "binance-user-main", &command_path).unwrap();
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 2);
    assert_eq!(state.audit()[0].status, CommandStatus::Accepted);
    assert_eq!(state.audit()[1].status, CommandStatus::Executed);
    assert!(state.pending().next().is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_multileg_group_snapshot_is_idempotent_and_reconciles_eventlog_state() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-spread-group-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let first = mk_order(
        9101,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let second = mk_order(
        9102,
        &InstrumentId::parse("ETHUSDT.OKX").unwrap(),
        Side::Sell,
        1,
    );
    let group_id = spread_group_id("arb", 7, 11);
    persist_strategy_spread_group(&root, &group_id, "arb", &[first.clone(), second.clone()])
        .unwrap();
    // 策略重试或 worker 重启不能重复创建不同快照。
    persist_strategy_spread_group(&root, &group_id, "arb", &[first.clone(), second.clone()])
        .unwrap();
    let store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
    let group = store.load(&group_id).unwrap().unwrap();
    assert_eq!(group.status, qx_zhenlu::SpreadOrderGroupStatus::Planned);
    assert_eq!(group.strategy_id, "arb");

    let command = strategy_submit_command("arb", &first, false, Some(&group_id)).unwrap();
    let mut pipeline = LiveEventPipeline::open(&root, "spread-events", "USDT").unwrap();
    pipeline.register_order(first.clone(), 100).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: first.client_id,
                venue_order_id: Some("binance-9101".into()),
            },
            101,
            101,
            1,
            "binance:accepted:9101",
        ))
        .unwrap();
    sync_spread_group_after_order(&root, &pipeline, &command, 101).unwrap();
    let group = store.load(&group_id).unwrap().unwrap();
    assert_eq!(
        group.leg("leg-9101").unwrap().order.status,
        OrderStatus::Accepted
    );
    assert_eq!(group.status, qx_zhenlu::SpreadOrderGroupStatus::Submitting);
    // 组仍在正常推进：其余腿照常放行。
    let second_command = strategy_submit_command("arb", &second, false, Some(&group_id)).unwrap();
    assert!(qx_execution::spread_group_barrier(Some(&store), &second_command).is_ok());
    assert!(qx_execution::spread_group_barrier(
        Some(&store),
        &strategy_submit_command("arb", &second, false, None).unwrap()
    )
    .is_ok());

    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::ReconcileRequired {
                client_order_id: first.client_id,
            },
            102,
            102,
            2,
            "binance:reconcile:9101",
        ))
        .unwrap();
    sync_spread_group_after_order(&root, &pipeline, &command, 102).unwrap();
    let group = store.load(&group_id).unwrap().unwrap();
    assert_eq!(
        group.leg("leg-9101").unwrap().order.status,
        OrderStatus::Unknown
    );
    assert_eq!(
        group.status,
        qx_zhenlu::SpreadOrderGroupStatus::ReconcileRequired
    );
    // 同一份快照进入未知结果后，屏障必须挡住同组其余腿，直到对账给出确定事实。
    let reason = qx_execution::spread_group_barrier(Some(&store), &second_command).unwrap_err();
    assert!(
        reason.contains("FAIL_CLOSED")
            && reason.contains(&group_id)
            && reason.contains("ReconcileRequired")
            && reason.contains("leg-9101"),
        "屏障拒绝理由必须点名组、状态与未知腿: {reason}"
    );
    assert!(qx_execution::spread_group_barrier(
        Some(&store),
        &strategy_submit_command("arb", &second, false, None).unwrap()
    )
    .is_ok());
    let _ = std::fs::remove_dir_all(root);
}

/// 多腿生命周期走生产单轨：两条腿各自经 `execute_paper_submit_effect`（即
/// `ExecutionGateway` 一条路径）提交，回报由 `sync_spread_group_after_order` 归约回组。
/// 它承接被删除的"只在单元测试中构造的第二编排入口"的同类断言（组状态、腿订单数、
/// 补偿目标口径），但走的是 worker 真正使用的那条链路。
#[test]
fn paper_multi_leg_spread_submits_each_leg_through_single_track_and_reduces_group() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-spread-single-track-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut long = mk_order(
        9501,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    long.limit = Some(Price::from_i64(100));
    let mut short = mk_order(
        9502,
        &InstrumentId::parse("ETHUSDT.OKX").unwrap(),
        Side::Sell,
        1,
    );
    short.limit = Some(Price::from_i64(100));
    let group_id = spread_group_id("basis-arbitrage", 3, 7);
    persist_strategy_spread_group(
        &root,
        &group_id,
        "basis-arbitrage",
        &[long.clone(), short.clone()],
    )
    .unwrap();
    let mut pipeline =
        LiveEventPipeline::open(&root, "spread-single-track-events", "USDT").unwrap();
    // 两条腿的命令都带 `spread_group_id`，因此必须把真实组存储交给提交入口：
    // 屏障判定已在 `qx-execution` 网关内执行，注入 `None` 会让两腿全部 fail-closed。
    let group_store = open_spread_group_store(&root).unwrap();
    for (index, order) in [long.clone(), short.clone()].iter().enumerate() {
        let mut risk = smoke_paper_risk_context();
        // 每腿按自己的冻结产品规格做账户级预检：第二腿换到 OKX 的 ETH 现货规格。
        if let Some(spec) = risk.instrument_spec.as_mut() {
            spec.instrument = order.instrument.clone();
            spec.base_currency = if index == 0 {
                "BTC".into()
            } else {
                "ETH".into()
            };
        }
        let (bid, ask) = if index == 0 { (99, 100) } else { (100, 101) };
        let ts = 20 + index as u64;
        let quote = QuoteTick::new(
            ts,
            Price::from_i64(bid),
            Quantity::from_i64(1_000),
            Price::from_i64(ask),
            Quantity::from_i64(1_000),
            ts,
        );
        let command =
            strategy_submit_command("basis-arbitrage", order, false, Some(&group_id)).unwrap();
        let submit_message = execute_paper_submit_effect(
            &command,
            &mut pipeline,
            ts,
            Some(risk),
            Some(OrderRiskPosition::new(0, 0)),
            Some(quote),
            false,
            Some(&group_store),
        )
        .unwrap();
        assert!(
            submit_message.starts_with("PAPER_EXECUTED fills=1"),
            "第 {index} 腿撮合结果异常: {submit_message}"
        );
        sync_spread_group_after_order(&root, &pipeline, &command, ts).unwrap();
    }
    // 事实完整性回归：每笔腿都要各自留下 Accepted、Fill 与成交双 Ledger 条目。历史上
    // source_seq 每命令归零且关联号只到 `(worker, venue)`，第二笔腿的全部事实会被
    // EventLog 当作重放静默丢弃，多腿链路只剩一条腿的事实。
    for client_order_id in 9501_u64..=9502 {
        let events = pipeline.log().events();
        let accepted = events
            .iter()
            .filter(|event| {
                matches!(event.kind, qx_core::EventKind::Accepted { client_order_id: id, .. } if id == client_order_id)
            })
            .count();
        let fills = events
            .iter()
            .filter(|event| {
                matches!(&event.kind, qx_core::EventKind::Filled { fill } if fill.order_id == client_order_id)
            })
            .count();
        let ledger = events
            .iter()
            .filter(|event| {
                matches!(&event.kind, qx_core::EventKind::LedgerApplied { entry } if entry.order_id == Some(client_order_id))
            })
            .count();
        assert_eq!(
            (accepted, fills, ledger),
            (1, 1, 2),
            "订单 {client_order_id} 的执行事实不完整"
        );
    }
    let group_store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
    let group = group_store.load(&group_id).unwrap().unwrap();
    let legs = pipeline
        .orders()
        .iter()
        .map(|order| (order.client_id, order.status, order.filled.raw()))
        .collect::<Vec<_>>();
    assert_eq!(
        group.status,
        qx_zhenlu::SpreadOrderGroupStatus::Filled,
        "legs: {legs:?}"
    );
    assert!(group.compensation_targets().is_empty());
    assert_eq!(pipeline.orders().len(), 2);
    assert!(pipeline
        .orders()
        .iter()
        .all(|order| order.status == OrderStatus::Filled));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_hedge_recovery_replays_partial_fill_to_hedged() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-hedge-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut first = mk_order(9201, &instrument, Side::Buy, 2);
    first.limit = Some(Price::from_i64(100));
    let mut second = mk_order(
        9202,
        &InstrumentId::parse("ETHUSDT.BINANCE").unwrap(),
        Side::Sell,
        2,
    );
    second.limit = Some(Price::from_i64(100));
    let mut group = SpreadOrderGroup::new(
        "paper-hedge-1",
        "basis-arbitrage",
        vec![
            SpreadOrderLeg {
                leg_id: "spot".into(),
                venue_id: "BINANCE".into(),
                order: first.clone(),
            },
            SpreadOrderLeg {
                leg_id: "future".into(),
                venue_id: "BINANCE".into(),
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
                order_id: first.client_id,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ..qx_core::Fill::default()
            },
        )
        .unwrap();
    group.record_cancelled("future").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
    let mut group_store = FileSpreadOrderGroupStore::new(root.join("spread-groups")).unwrap();
    group_store.save(&group).unwrap();

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "paper-hedge-cash".into(),
                },
            },
            1,
            1,
            1,
            "paper:cash",
        ))
        .unwrap();
    pipeline.register_order(first.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: first.client_id,
                venue_order_id: Some("paper-9201".into()),
            },
            3,
            3,
            2,
            "paper:accepted:9201",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                4,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                3,
            ),
            4,
            3,
            "paper:quote:btc",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Fill {
                fill: qx_core::Fill {
                    order_id: first.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    fee: Money::ZERO,
                    ts: 5,
                    account_id: "main".into(),
                    venue_id: Some("paper".into()),
                    venue_order_id: Some("paper-9201".into()),
                    ..qx_core::Fill::default()
                },
            },
            5,
            5,
            4,
            "paper:fill:9201",
        ))
        .unwrap();

    let diagnostics =
        recover_paper_spread_groups(&root, &mut pipeline, "paper-hedge", 6, None).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("hedge=completed")),
        "{diagnostics:?}"
    );
    let restored = group_store.load("paper-hedge-1").unwrap().unwrap();
    assert_eq!(restored.status, SpreadOrderGroupStatus::Hedged);
    let hedge = pipeline
        .orders()
        .into_iter()
        .find(|order| {
            order
                .trace
                .as_ref()
                .and_then(|trace| trace.rule_version.as_deref())
                == Some("spread-hedge-v1")
        })
        .unwrap();
    assert_eq!(hedge.status, OrderStatus::Filled);
    assert!(hedge.policy.unwrap().reduce_only);
    let _ = std::fs::remove_dir_all(root);
}
