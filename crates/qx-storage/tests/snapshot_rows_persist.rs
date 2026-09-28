//! 带行的账户快照必须存得进、读得出（V12 审计第三遍，交易链路 TX5）。
//!
//! 缺陷形态：`to_json` 是手写的稳定编码器，而 orders/fills/transfers 三张表按 `u64` 建键。
//! 键原样拼进 `{}` 时产出的不是合法 JSON，订单行的 `side`/`status` 又是数字码而不是枚举名，
//! 于是**一有订单**的快照在两个存储后端与平面快照路由上都写得出、读不回 —— 而仓库里原有的
//! 落库用例只放了纯现金，这条形状从未被自己的读法读过。

use qx_protocol::{
    AccountSnapshot, FileSnapshotStore, FillSnapshot, OrderSnapshot, SnapshotStore,
    TransferSnapshot,
};

fn snapshot_with_rows() -> AccountSnapshot {
    let instrument = qx_core::InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
    let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    snapshot.cash_raw.insert("USDT".into(), 1000);
    snapshot.equity_raw = Some(1000);
    snapshot.orders.insert(
        7,
        OrderSnapshot {
            order_id: 7,
            client_order_id: 7,
            instrument,
            side: qx_core::Side::Buy,
            quantity_raw: 2,
            filled_raw: 0,
            status: qx_core::OrderStatus::Working,
        },
    );
    snapshot.fills.insert(
        9,
        FillSnapshot {
            fill_id: 9,
            order_id: 7,
            quantity_raw: 2,
            price_raw: 100,
            fee_raw: 1,
            ts: 10,
        },
    );
    snapshot.transfers.insert(
        11,
        TransferSnapshot {
            transfer_id: 11,
            currency: "USDT".into(),
            amount_raw: 500,
            ts: 10,
        },
    );
    snapshot.seal();
    snapshot
}

/// 断言的是"落盘正文自证"，而不是只断言读回相等：正文必须仍是合法 JSON，且三张表各自
/// 按 id 字符串建键，读者才能拿同一份文件对账。
fn assert_rows_survive(store: &dyn SnapshotStore, snapshot: &AccountSnapshot) {
    let saved = store.save(snapshot).expect("带行快照必须存进去");
    let json = store
        .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
        .expect("带行快照必须读得回来");
    let parsed: serde_json::Value = serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("落盘正文必须是合法 JSON: {error}\n{json}"));
    for (table, key) in [("orders", "7"), ("fills", "9"), ("transfers", "11")] {
        assert!(
            parsed[table].get(key).is_some(),
            "{table} 必须按 id 建键，实际正文来自 {saved:?}: {json}"
        );
    }
    assert_eq!(
        AccountSnapshot::from_json(&json).unwrap(),
        *snapshot,
        "订单/成交/划转必须逐字段读回"
    );
}

#[test]
fn file_snapshot_store_recovers_a_snapshot_that_has_rows() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-tx5-file-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileSnapshotStore::new(&root);
    let snapshot = snapshot_with_rows();
    assert_rows_survive(&store, &snapshot);
    // 幂等重写不得把行弄丢或改坏：第二次 save 走的是"已存在且逐字相同"的分支。
    store.save(&snapshot).expect("同一快照必须可幂等重写");
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_snapshot_store_recovers_a_snapshot_that_has_rows() {
    use qx_storage::SqliteSnapshotStore;
    let path = std::env::temp_dir().join(format!(
        "qianxing-tx5-sqlite-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = SqliteSnapshotStore::new(&path).unwrap();
    let snapshot = snapshot_with_rows();
    assert_rows_survive(&store, &snapshot);
    let _ = std::fs::remove_file(path);
}
