//! 账户快照的七个汇总钱字段必须区分"算过"与"没算"（V11 Q67，交易链路读模型侧）。
//!
//! 此前 `available_raw` 被写成 `equity_raw` 的副本、`fees_raw` 等六个字段在全仓没有任何写入点，
//! 于是同一份 JSON 一边逐笔公布成交费用、一边宣布账户费用为 0，且"没算过"与"算出是零"在协议上
//! 长得一样。这里钉住三件结果各自不同的事：算得出的按自己的来源算、算不出的必须缺席而不是零、
//! 以及"缺席"与"零"在哈希与线格式上都不是同一份状态。
//!
//! V11 Q68 把同一条纪律推到持仓行：读模型用 Ledger 自己拼出来的那一行同样没有
//! 未实现盈亏与保证金来源，交易所报了零的那一行则必须把零留在原地。

use super::*;

fn paper_snapshot(config: &RuntimeConfig) -> AccountSnapshot {
    load_api_account_snapshots(config)
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.header.account_id == "main")
        .expect("paper 拓扑必须投影出 main 账户快照")
}

/// 成交费用合计与权益里的持仓价值是两回事：把 `available_raw` 抄成 `equity_raw`，
/// 等于宣布已经压在持仓上的那段钱还能自由花掉。
#[test]
fn available_is_the_settlement_cash_and_not_a_copy_of_equity() {
    let root = temp_cli_case_dir("api-money-fields-available");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);

    let snapshot = paper_snapshot(&config);
    let settlement = snapshot
        .cash_raw
        .get("USDT")
        .copied()
        .expect("paper 成交必须落在 USDT 结算账簿上");
    assert_eq!(
        snapshot.available_raw,
        Some(settlement),
        "可用资金是结算账簿现金，实际 {:?}",
        snapshot.available_raw
    );
    assert!(
        snapshot.equity_raw > settlement,
        "持仓未平时权益必须高于现金，否则这条用例没在区分两个量：equity={} cash={settlement}",
        snapshot.equity_raw
    );
    assert_ne!(
        snapshot.available_raw,
        Some(snapshot.equity_raw),
        "available 不得再是 equity 的副本"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 同一条快照的 `fills` 逐笔带着 `fee_raw`，账户级 `fees_raw` 却是 0 —— 一份 JSON 两个口径。
#[test]
fn published_fees_are_the_sum_of_the_fills_on_the_same_snapshot() {
    let root = temp_cli_case_dir("api-money-fields-fees");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);

    let snapshot = paper_snapshot(&config);
    let per_fill: i128 = snapshot.fills.values().map(|fill| fill.fee_raw).sum();
    assert!(per_fill > 0, "夹具必须真产生一笔带费用的成交，否则断言恒等");
    assert_eq!(
        snapshot.fees_raw,
        Some(per_fill),
        "账户费用合计必须等于逐笔成交费用之和"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 这一层没有保证金簿、没有已实现盈亏口径，也没有资金费流水来源：这些字段必须**缺席**。
/// 把它们填成 0 会让读侧把"没算"当成"这个账户没有保证金、没交过费"。
#[test]
fn uncomputed_money_is_absent_rather_than_zero() {
    let root = temp_cli_case_dir("api-money-fields-absent");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);

    let snapshot = paper_snapshot(&config);
    for (name, value) in [
        ("margin_raw", snapshot.margin_raw),
        ("frozen_raw", snapshot.frozen_raw),
        ("realized_pnl_raw", snapshot.realized_pnl_raw),
        ("unrealized_pnl_raw", snapshot.unrealized_pnl_raw),
        ("funding_raw", snapshot.funding_raw),
    ] {
        assert_eq!(value, None, "{name} 在本层没有来源，必须报未算而不是 0");
    }
    // 线格式侧：缺席要印成 null，不能印成一个合法的整数 0。
    let wire = snapshot.to_wire_json().unwrap();
    for name in [
        "margin_raw",
        "frozen_raw",
        "realized_pnl_raw",
        "unrealized_pnl_raw",
        "funding_raw",
    ] {
        assert!(
            wire.contains(&format!("\"{name}\":null")),
            "{name} 未算过时线格式必须印 null: {wire}"
        );
    }
    let stable = snapshot.to_json();
    assert!(
        stable.contains("\"margin_raw\":null"),
        "稳定 JSON 同样不得把未算填成 0: {stable}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 费用合计溢出必须是"读不出这份快照"，而不是印出一个回绕成负数的费用。
///
/// 这笔夹具是算出来的，不是随手挑的：两笔费用各自合法、合计正好越出 `i128::MAX`，
/// 而账户现金落在 `i128::MIN`（可表示）——所以账簿那边不会先炸，读模型必须自己拒绝。
/// 换个更"极端"的夹具（两笔都取 `MAX/2+1`）会让现金侧先溢出，测的就不是这条分支了。
#[test]
fn overflowing_fee_total_is_refused_instead_of_wrapping() {
    let root = temp_cli_case_dir("api-money-fields-overflow");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    // 一笔 1 单位 @100 的名义额是 100*SCALE，两笔正好等于播种的 200 USDT 现金。
    let fees = [i128::MAX / 2 + 2, i128::MAX / 2];
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDT").unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow {
                    cashflow: AccountCashflow {
                        account_id: "main".into(),
                        venue_id: "paper".into(),
                        currency: "USDT".into(),
                        kind: CashflowKind::Transfer,
                        amount: Money::from_i64(200),
                        external_id: "money-fields:overflow:seed".into(),
                    },
                },
                1,
                1,
                1,
                "money-fields:overflow:seed",
            ))
            .unwrap();
        for (index, client_id) in [9702_u64, 9703].iter().enumerate() {
            let base = 1 + (index as u64) * 3;
            pipeline
                .register_order(mk_order(*client_id, &instrument, Side::Buy, 1), base + 1)
                .unwrap();
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Accepted {
                        client_order_id: *client_id,
                        venue_order_id: Some(format!("money-fields-overflow-{client_id}")),
                    },
                    base + 2,
                    base + 2,
                    1,
                    format!("money-fields:accept:{client_id}"),
                ))
                .unwrap();
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::Fill {
                        fill: qx_core::Fill {
                            order_id: *client_id,
                            qty: Quantity::from_i64(1),
                            price: Price::from_i64(100),
                            fee: Money::from_raw(fees[index]),
                            ts: base + 3,
                            account_id: "main".into(),
                            ..qx_core::Fill::default()
                        },
                    },
                    base + 3,
                    base + 3,
                    1,
                    format!("money-fields:overflow:{client_id}"),
                ))
                .unwrap_or_else(|error| {
                    panic!("两笔费用各自合法、合计越界，账簿必须先收下它们: {error:?}")
                });
        }
        assert_eq!(pipeline.ledger().cash_for("main", "USDT"), i128::MIN);
    }

    let error =
        load_api_account_snapshots(&config).expect_err("费用合计回绕时不能发布一个口径错误的快照");
    assert!(
        error.contains("溢出") && error.contains("main"),
        "溢出必须点名账户和溢出本身: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 读模型自己用 Ledger 拼出来的那一行持仓同样没有"未实现盈亏"和"占用的保证金"：
/// 这一层只有成交与现金，那两个量要按冻结的 market spec 逐标的算。写死 0 会让一个
/// 上涨 5% 的仓位长期报着"没有浮亏"（V11 Q68）。
#[test]
fn ledger_fallback_position_row_leaves_uncomputed_money_absent() {
    let root = temp_cli_case_dir("api-money-fields-row");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);

    let snapshot = paper_snapshot(&config);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let row = snapshot
        .positions
        .get(&instrument)
        .expect("paper 成交必须投影出一行持仓");
    assert_ne!(row.quantity_raw, 0, "夹具必须真的留下一笔持仓");
    assert_eq!(
        (row.unrealized_pnl_raw, row.margin_raw),
        (None, None),
        "本层算不出这两个钱字段，必须报未算而不是 0"
    );
    // 逐行断言要读解析后的 JSON：账户级标量在本层同样是 null，整串 `contains` 会
    // 被它先命中而永远为真。
    let wire: serde_json::Value = serde_json::from_str(&snapshot.to_wire_json().unwrap()).unwrap();
    let json_row = &wire["positions"]["BTCUSDT.BINANCE"];
    for name in ["unrealized_pnl_raw", "margin_raw"] {
        assert!(
            json_row[name].is_null(),
            "{name} 未经交易所上报时线格式必须印 null: {json_row}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

/// 同一行持仓、同一处读模型：交易所明确报了零就必须留下零，没报的那一项仍是缺席。
/// 这条与上一条是一对——只有"零与缺席是两种状态"时两者才可能同时成立。
#[test]
fn venue_reported_row_keeps_reported_zero_and_unreported_absent() {
    let root = temp_cli_case_dir("api-money-fields-venue-row");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDT").unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountPositionSnapshot {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    positions: vec![AccountPositionSnapshot {
                        instrument: instrument.clone(),
                        quantity: Quantity::from_i64(1),
                        average_price: Some(Price::from_i64(100)),
                        mark_price: Some(Price::from_i64(105)),
                        liquidation_price: None,
                        // 交易所报了"未实现盈亏为零"，但没报保证金占用。
                        unrealized_pnl: Some(Money::ZERO),
                        initial_margin: None,
                        maintenance_margin: None,
                        leverage: None,
                        margin_mode: None,
                        position_side: Some("long".into()),
                    }],
                },
                1,
                1,
                1,
                "money-fields:venue-row",
            ))
            .unwrap();
    }

    let snapshot = paper_snapshot(&config);
    let row = snapshot
        .positions
        .get(&instrument)
        .expect("交易所持仓事实必须投影成同一行");
    assert_eq!(row.unrealized_pnl_raw, Some(0), "报出来的零不能被抹成缺席");
    assert_eq!(row.margin_raw, None, "没报的保证金不能被读成 0");
    assert_eq!(row.mark_price_raw, Price::from_i64(105).raw());
    let _ = std::fs::remove_dir_all(root);
}

/// 落一份对账报告到 `data_dir/reconcile/<worker-id>.json`，形状与对账 worker 的写出口一致。
fn seed_reconcile_report(
    data_dir: &Path,
    worker_id: &str,
    account_id: &str,
    venue_id: &str,
    observed_ts: u64,
    order_issues: usize,
    balance_discrepancies: usize,
) {
    let report = ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: worker_id.into(),
        account_id: account_id.into(),
        venue_id: venue_id.into(),
        observed_ts,
        order_issues: vec![serde_json::json!({"kind": "remote-terminal"}); order_issues],
        balances_count: 1,
        balance_discrepancies: vec![serde_json::json!({"asset": "USDT"}); balance_discrepancies],
        position_snapshots_count: Some(0),
        funding_rate_snapshots_count: Some(0),
        cashflow_count: Some(0),
    };
    let dir = data_dir.join("reconcile");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{worker_id}.json")),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
}

/// 快照的对账侧字段必须有来源（V11 R7），而"没有来源"必须写成缺席而不是合法的 0（V11 R10）：
/// `/account/snapshot` 起初把两格停在构造时的 0，同一份 data_dir 里却躺着对账报告；补上来源之后
/// 0 仍然同时表示"从未对账"与"对过且无差异"，看板会把没跑过对账的账户读成绿色。
#[test]
fn account_snapshot_reconcile_fields_come_from_the_reports_on_disk() {
    let root = temp_cli_case_dir("api-reconcile-fields");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);
    seed_reconcile_report(&data_dir, "paper-reconciler", "main", "paper", 4_711, 2, 1);
    // 另一本账的报告不能算到 main 头上：它连身份都对不上。
    seed_reconcile_report(&data_dir, "ccxt-reconciler", "other", "okx", 9_999, 5, 5);

    let snapshot = paper_snapshot(&config);
    assert_eq!(
        snapshot.reconcile.last_reconcile_ts,
        Some(4_711),
        "对账时刻要来自本账户的报告，而不是别家或 0"
    );
    assert_eq!(
        snapshot.reconcile.discrepancy_count,
        Some(3),
        "订单差异与余额差异都要计入，实际 {:?}",
        snapshot.reconcile.discrepancy_count
    );

    // 反面一：没有报告时两格一起缺席，而不是凭空造一个对账时刻或一个"干净"结论。
    let clean_root = temp_cli_case_dir("api-reconcile-fields-clean");
    let clean_dir = clean_root.join("data");
    std::fs::create_dir_all(&clean_dir).unwrap();
    let clean_config = paper_runtime_config(&clean_dir);
    seed_paper_fill_with_fee(&clean_dir, &clean_config);
    let clean = paper_snapshot(&clean_config);
    assert_eq!(clean.reconcile.last_reconcile_ts, None);
    assert_eq!(clean.reconcile.discrepancy_count, None);

    // 反面二：对过且真的没有差异，零要留在零的位置上，且与"没对过"不是同一份协议状态。
    let zero_root = temp_cli_case_dir("api-reconcile-fields-zero");
    let zero_dir = zero_root.join("data");
    std::fs::create_dir_all(&zero_dir).unwrap();
    let zero_config = paper_runtime_config(&zero_dir);
    seed_paper_fill_with_fee(&zero_dir, &zero_config);
    seed_reconcile_report(&zero_dir, "paper-reconciler", "main", "paper", 5_000, 0, 0);
    let reconciled = paper_snapshot(&zero_config);
    assert_eq!(reconciled.reconcile.last_reconcile_ts, Some(5_000));
    assert_eq!(
        reconciled.reconcile.discrepancy_count,
        Some(0),
        "「对过且无差异」是算出来的零，不能被降格成缺席"
    );
    let mut never = reconciled.clone();
    never.reconcile.discrepancy_count = None;
    assert_ne!(
        reconciled.state_hash(),
        never.state_hash(),
        "只差「对过且干净 / 没对过」的两份快照必须是两份哈希：\
         只靠时间戳分开的话，`Some(0)` 与 `None` 在协议上仍是同一个数"
    );
    assert!(
        reconciled.to_json().contains(r#""discrepancy_count":0"#),
        "线格式里算出的零要写成 0，实际 {}",
        reconciled.to_json()
    );
    assert!(
        clean.to_json().contains(r#""discrepancy_count":null"#),
        "线格式里没算过的要写成 null，而不是一个合法的 0"
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(clean_root);
    let _ = std::fs::remove_dir_all(zero_root);
}
