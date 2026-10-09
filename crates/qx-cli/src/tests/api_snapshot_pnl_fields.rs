//! 账户级已实现/未实现盈亏的**生产者**（V13 R26，交易链路读模型侧）。
//!
//! 与 `api_snapshot_money_fields.rs` 的"未算即缺席"用例分居两处是刻意的：门禁
//! `account_money_field_registry_check` 按那条用例体派生"这一层算不出哪几格"，两种事实混进
//! 同一条里，名单就再没有对象可比。这里钉正向的一条——两个字段必须**算出来**（只开仓未平仓时
//! 是算出来的零），而不是继续躺在缺席里。
//!
//! 两个算点都取自 Ledger：已实现盈亏是逐条持仓累计的已实现盈亏之和（平仓到零不清零，故含
//! 已平掉的腿）；未实现盈亏是与 `equity_raw` 同源的现货乘数尺子 `(mark − 均价) × 数量`，
//! 缺标记价时与权益一样报缺席而不是 0。

use super::*;

fn pnl_snapshot(config: &RuntimeConfig) -> AccountSnapshot {
    load_api_account_snapshots(config)
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.header.account_id == "main")
        .expect("paper 拓扑必须投影出 main 账户快照")
}

/// 只开仓未平仓：累计已实现盈亏是"算出来的零"（`Some(0)`）而不是"没算过"（`None`）；
/// 未实现盈亏与持仓行同一条现货尺子。线格式与稳定 JSON 都要把它们印成数，而不是继续印 null。
#[test]
fn realized_and_unrealized_pnl_are_projected_from_the_ledger() {
    let root = temp_cli_case_dir("api-pnl-fields");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);

    let snapshot = pnl_snapshot(&config);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let row = snapshot
        .positions
        .get(&instrument)
        .expect("paper 成交必须投影出一行持仓");
    assert_ne!(row.quantity_raw, 0, "夹具必须真的留下一笔持仓");
    assert_eq!(
        snapshot.realized_pnl_raw,
        Some(0),
        "只开过仓没平过仓时，累计已实现盈亏必须是算出来的 0，而不是缺席"
    );
    // 未实现盈亏与持仓行取同一条标记价与同一套定点算子：`(mark − 均价) × 数量`。
    let expected = (row.mark_price_raw - row.average_price_raw) * row.quantity_raw / SCALE;
    assert_eq!(
        snapshot.unrealized_pnl_raw,
        Some(expected),
        "未实现盈亏必须由 Ledger 按持仓与标记价算出，而不是缺席"
    );
    // 逐字段读解析后的 JSON：持仓行里也有同名列（那里仍是 null，交易所没报），整串
    // `contains` 会被它命中而误判。
    let wire: serde_json::Value = serde_json::from_str(&snapshot.to_wire_json().unwrap()).unwrap();
    let stable: serde_json::Value = serde_json::from_str(&snapshot.to_json()).unwrap();
    for name in ["realized_pnl_raw", "unrealized_pnl_raw"] {
        assert!(
            wire[name].is_number(),
            "{name} 已接上生产者，线格式顶层必须是数而不是 null: {wire}"
        );
        assert!(
            stable[name].is_number(),
            "{name} 已接上生产者，稳定 JSON 顶层必须是数而不是 null: {stable}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}
