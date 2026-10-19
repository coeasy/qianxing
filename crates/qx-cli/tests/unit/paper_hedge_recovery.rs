//! Paper 多腿对冲恢复的行为用例：从 `HedgeRequired` 恢复到 `Hedged` 的完整链路，
//! 以及补偿成交必须带着该 worker 冻结的产品规格进归约入口（V11 Q57）。

use super::*;

/// 共用夹具：spot 腿确认成交、future 腿取消，账户里只有原始腿的事实，
/// 补偿单尚未登记——恢复扫描要在这份状态上把净敞口对冲掉。
fn paper_hedge_recovery_fixture(
    tag: &str,
    group_id: &str,
) -> (PathBuf, LiveEventPipeline, FileSpreadOrderGroupStore) {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-hedge-{}-{}-{}",
        tag,
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
        group_id,
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
                    external_id: format!("paper-hedge-{tag}-cash"),
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
    (root, pipeline, group_store)
}

#[test]
fn paper_hedge_recovery_replays_partial_fill_to_hedged() {
    let (root, mut pipeline, group_store) = paper_hedge_recovery_fixture("replay", "paper-hedge-1");
    let diagnostics = recover_paper_spread_groups(
        &root,
        &mut pipeline,
        "paper-hedge",
        6,
        None,
        &default_execution_cost_binding(),
    )
    .unwrap();
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

/// Paper 恢复扫描里由 `PaperVenue` 撮合出来的补偿成交，也必须带着该 worker 冻结的
/// 产品规格进归约入口。
///
/// 这是补偿链上最后一条回报入口：漏掉规格时，越界成交会照常入账、衍生品腿还会退回
/// 乘数 1 记账，而同一笔成交走用户流却会被转待对账。用例把两种口径放在同一个夹具下
/// 对比：tick 内照常 Hedged，tick 外只留待对账事实、组停在 HedgeRequired。
#[test]
fn paper_hedge_recovery_ingests_venue_fills_through_the_worker_frozen_spec() {
    fn spec_json(price_tick_raw: i128) -> String {
        format!(
            r#"{{
  "instrument": {{ "symbol": "BTCUSDT", "venue": "BINANCE" }},
  "product": "spot",
  "base_currency": "BTC",
  "quote_currency": "USDT",
  "settlement_currency": "USDT",
  "contract_size": 1000000000,
  "linear": true,
  "inverse": false,
  "price_tick": {price_tick_raw},
  "qty_step": 1000000,
  "min_qty": 1000000,
  "max_leverage": 1,
  "maintenance_margin_bps": 0,
  "valid_from": 0,
  "valid_to": null
}}"#
        )
    }

    // (标签, 价格 tick, 该成交是否必须被挡在账本之外)
    for (label, price_tick_raw, expect_refused) in
        [("on-tick", SCALE, false), ("off-tick", 7 * SCALE, true)]
    {
        let (root, mut pipeline, group_store) =
            paper_hedge_recovery_fixture(label, "paper-hedge-spec-gate");
        let spec_path = root.join(format!("spec-{label}.json"));
        std::fs::write(&spec_path, spec_json(price_tick_raw)).unwrap();
        let worker = mk_worker(
            "paper-hedge-spec-guard",
            WorkerRole::Execution,
            "paper",
            Some(&spec_path.to_string_lossy()),
        );
        let validator = recovery_order_validator(&worker, &pipeline, None);
        let result = recover_paper_spread_groups(
            &root,
            &mut pipeline,
            "paper-hedge",
            6,
            Some(&validator),
            &default_execution_cost_binding(),
        );
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
            .unwrap_or_else(|| panic!("{label}: 补偿订单应当已经登记"));
        let persisted = group_store
            .load("paper-hedge-spec-gate")
            .unwrap()
            .expect("组快照应仍在恢复存储里");
        if expect_refused {
            let error = match result {
                Err(error) => error,
                Ok(diagnostics) => {
                    panic!("{label}: 越界补偿成交不该入账，diagnostics={diagnostics:?}")
                }
            };
            assert!(error.contains("产品规格"), "{label} -> {error}");
            assert_eq!(
                persisted.status,
                SpreadOrderGroupStatus::HedgeRequired,
                "{label}: 未确认的补偿不能把组标记为已对冲"
            );
            assert_ne!(
                hedge.status,
                OrderStatus::Filled,
                "{label}: 越界成交只能转待对账，实际 {hedge:?}"
            );
        } else {
            let diagnostics =
                result.unwrap_or_else(|error| panic!("{label}: tick 内的补偿不该被拒: {error}"));
            assert!(
                diagnostics
                    .iter()
                    .any(|message| message.contains("hedge=completed")),
                "{label}: {diagnostics:?}"
            );
            assert_eq!(
                persisted.status,
                SpreadOrderGroupStatus::Hedged,
                "{label}: diagnostics={diagnostics:?}"
            );
            assert_eq!(hedge.status, OrderStatus::Filled, "{label}: {hedge:?}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
