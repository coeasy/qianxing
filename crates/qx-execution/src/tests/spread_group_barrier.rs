//! 多腿跨腿屏障的**网关层**反向验证（V10 §6.1 第 5 项，从 CLI 的
//! `crates/qx-cli/src/tests/paper_and_strategy_worker.rs` 搬下来）。
//!
//! 屏障判定已从 CLI 下沉到 `qx-execution`：唯一实现是
//! [`spread_group_barrier`](crate::spread_group_barrier)，由 [`ExecutionGateway`] 在
//! 写入任何事实之前调用。因此"摘掉屏障这一条腿就会变成 Filled 订单"的反向验证也
//! 必须钉在网关上——删掉 `PortExecutionService::submit_command` 里那一行
//! `spread_group_barrier(self.spread_store, command)?` 后，本文件用例会立刻变红。
//!
//! 断言口径是**错误分类**（`FAIL_CLOSED:` 前缀）而不只是"没有记下订单"：只有前缀
//! 能区分"网关按纪律拒绝"与"载荷恰好不合法"，也正是 CLI 与监控据以分类拒绝原因的
//! 契约。这里的行情与风控条件都齐备，唯一的拒绝理由就是屏障本身。

use super::*;

/// 一条腿结果未知、另一条腿仍待提交的订单组，外加那条待提交腿的命令。
///
/// `fills_venue` 故意返回 Accepted + Fill：一旦屏障被摘掉，这条腿就会真的成交，
/// 用例的 `FAIL_CLOSED:` 断言随即变红——这就是反向验证的机制。
fn blocked_group_and_pending_leg(
    root: &std::path::Path,
    dir: &str,
) -> (FileSpreadOrderGroupStore, ControlCommand) {
    let first = port_order(401);
    let second = port_order(402);
    let mut group = SpreadOrderGroup::new(
        "spread-gateway-barrier",
        "basis-arbitrage",
        vec![
            SpreadOrderLeg {
                leg_id: "spot".into(),
                venue_id: "binance".into(),
                order: first,
            },
            SpreadOrderLeg {
                leg_id: "future".into(),
                venue_id: "okx".into(),
                order: second.clone(),
            },
        ],
    )
    .unwrap();
    // 生产归约路径把腿 A 置为 Unknown 后落盘的就是这个状态，这里直接复现它。
    group.begin_submission().unwrap();
    group.record_unknown("future").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::ReconcileRequired);
    let mut store = FileSpreadOrderGroupStore::new(root.join(dir)).unwrap();
    store.save(&group).unwrap();

    let command = ControlCommand {
        command_id: 402,
        request_id: "spread-leg-402".into(),
        operator_id: "basis-arbitrage".into(),
        reason: "multi-leg second leg".into(),
        kind: CommandKind::SubmitOrder,
        target: "402".into(),
        payload: BTreeMap::from([
            ("order_json".into(), serde_json::to_string(&second).unwrap()),
            ("spread_group_id".into(), "spread-gateway-barrier".into()),
        ]),
        permission: Permission::Trading,
        dry_run: false,
    };
    (store, command)
}

/// 网关内部的那一行屏障：带 `spread_group_id` 的腿在组待对账时必须被拒绝，
/// 且拒绝原因按 `FAIL_CLOSED:` 分类，且不留任何事实。
#[test]
fn gateway_blocks_next_leg_of_reconcile_required_group_and_classifies_fail_closed() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-exec-spread-barrier-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (store, command) = blocked_group_and_pending_leg(&root, "groups");
    let mut state = PortState::default();
    // 行情侧完全配合：Venue 直接给出 Accepted + Fill，唯一能挡住它的就是屏障。
    let mut venue = PortVenue {
        result: Ok(vec![
            ExecutionEvent::Accepted {
                client_order_id: 402,
                venue_order_id: "remote-402".into(),
            },
            ExecutionEvent::Fill(Box::new(qx_core::Fill {
                order_id: 402,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ..qx_core::Fill::default()
            })),
        ]),
    };
    let mut source_seq = 0_u64;
    let error = ExecutionGateway::new(&mut venue, &mut state, "gate-worker", 10, &mut source_seq)
        .with_spread_group_store(&store)
        .submit_command(&command)
        .unwrap_err();
    assert!(
        error.starts_with("FAIL_CLOSED:"),
        "屏障拒绝必须按 FAIL_CLOSED 分类，供 CLI 与监控按前缀识别: {error}"
    );
    assert!(
        error.contains("spread-gateway-barrier")
            && error.contains("ReconcileRequired")
            && error.contains("future"),
        "拒绝理由必须点名组、组状态与未知腿: {error}"
    );
    // 反向验证的正面同伴：屏障在位时这条腿绝不能留下任何事实。
    assert!(state.orders.is_empty(), "被拒的腿不得登记订单");
    assert!(state.events.is_empty(), "被拒的腿不得写入执行事实");
    assert_eq!(source_seq, 0, "屏障必须先于任何 source_seq 推进");
    let _ = std::fs::remove_dir_all(root);
}

/// 带风控的提交入口共用同一道网关内屏障（`submit_command_with_risk` 那一行）。
#[test]
fn gateway_risk_submit_entry_shares_the_same_spread_barrier() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-exec-spread-barrier-risk-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let (store, command) = blocked_group_and_pending_leg(&root, "groups");
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 402,
            venue_order_id: "remote-402".into(),
        }]),
    };
    let mut source_seq = 0_u64;
    let risk = qx_risk::OrderRiskContext::default();
    let error = ExecutionGateway::new(&mut venue, &mut state, "gate-worker", 10, &mut source_seq)
        .with_spread_group_store(&store)
        .submit_command_with_risk(&command, &CanonicalRiskPort { context: &risk })
        .unwrap_err();
    assert!(
        error.starts_with("FAIL_CLOSED:"),
        "风控版本同样必须按 FAIL_CLOSED 分类: {error}"
    );
    assert!(state.orders.is_empty() && state.events.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// 第三条 fail-closed 纪律：带 `spread_group_id` 而提交路径**没注入**组存储时，
/// "跳过屏障"不再是一个可选项，只能拒绝提交。
#[test]
fn grouped_leg_without_injected_group_store_fails_closed_instead_of_skipping() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-exec-spread-barrier-nostore-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let order = port_order(403);
    let command = ControlCommand {
        command_id: 403,
        request_id: "spread-leg-403".into(),
        operator_id: "basis-arbitrage".into(),
        reason: "grouped leg on a path without group store".into(),
        kind: CommandKind::SubmitOrder,
        target: "403".into(),
        payload: BTreeMap::from([
            ("order_json".into(), serde_json::to_string(&order).unwrap()),
            ("spread_group_id".into(), "spread-without-store".into()),
        ]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![ExecutionEvent::Accepted {
            client_order_id: 403,
            venue_order_id: "remote-403".into(),
        }]),
    };
    let mut source_seq = 0_u64;
    let error = submit_order_via_gateway(
        &command,
        &mut venue,
        &mut state,
        "gate-worker",
        10,
        &mut source_seq,
        None,
    )
    .unwrap_err();
    assert!(
        error.starts_with("FAIL_CLOSED:"),
        "未注入组存储必须拒绝而不是放行: {error}"
    );
    assert!(state.orders.is_empty() && state.events.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// 正面同伴用例：组干净时同一份载荷照常成交。它保证上面三条拒绝来自屏障判定，
/// 而不是载荷本身不合法或 Venue 被配坏。
#[test]
fn clean_group_still_submits_its_next_leg_through_the_gateway() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-exec-spread-barrier-clean-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let first = port_order(401);
    let second = port_order(402);
    let mut group = SpreadOrderGroup::new(
        "spread-gateway-clean",
        "basis-arbitrage",
        vec![
            SpreadOrderLeg {
                leg_id: "spot".into(),
                venue_id: "binance".into(),
                order: first.clone(),
            },
            SpreadOrderLeg {
                leg_id: "future".into(),
                venue_id: "okx".into(),
                order: second.clone(),
            },
        ],
    )
    .unwrap();
    // 只登记一条腿被接受：组仍在 Submitting，不阻塞其余腿。
    group.begin_submission().unwrap();
    group.record_accepted("spot").unwrap();
    assert!(!group.blocks_new_leg_submission());
    let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
    store.save(&group).unwrap();

    let command = ControlCommand {
        command_id: 402,
        request_id: "spread-leg-402".into(),
        operator_id: "basis-arbitrage".into(),
        reason: "multi-leg second leg".into(),
        kind: CommandKind::SubmitOrder,
        target: "402".into(),
        payload: BTreeMap::from([
            ("order_json".into(), serde_json::to_string(&second).unwrap()),
            ("spread_group_id".into(), "spread-gateway-clean".into()),
        ]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let mut state = PortState::default();
    let mut venue = PortVenue {
        result: Ok(vec![
            ExecutionEvent::Accepted {
                client_order_id: 402,
                venue_order_id: "remote-402".into(),
            },
            ExecutionEvent::Fill(Box::new(qx_core::Fill {
                order_id: 402,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ..qx_core::Fill::default()
            })),
        ]),
    };
    let mut source_seq = 0_u64;
    submit_order_via_gateway(
        &command,
        &mut venue,
        &mut state,
        "gate-worker",
        10,
        &mut source_seq,
        Some(&store),
    )
    .unwrap();
    assert_eq!(state.orders.len(), 1);
    assert_eq!(state.orders[0].status, OrderStatus::Filled);
    let _ = std::fs::remove_dir_all(root);
}
