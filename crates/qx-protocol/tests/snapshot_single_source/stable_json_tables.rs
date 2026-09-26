//! 稳定 JSON 的键表与跨语言夹具（V11 R14/R15/R18）。
//!
//! 写侧 `to_json` 与读侧 `from_json` 此前各有两份编码：键表被手抄成裸数字键
//! （`"orders":{77:{…}}` 连合法 JSON 都不是），持仓行少印字段。这两条用例把两侧钉在
//! 同一份产物上——一条钉"写出来的就是 serde 那份、且读得回来"，一条钉"Python 侧读到的
//! 夹具就是写侧原样产出的那一份"。

use super::optional_money::position_row;
use super::*;

/// 两侧共用的那一份快照：四张键表各有一格，钱槽位一半算过一半没算过。
/// 稳定 JSON 与夹具都必须由它产出——夹具是 `to_json` 的输出，不是手抄的形状。
fn cross_language_sample() -> AccountSnapshot {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let mut snapshot = AccountSnapshot::new(7, "main", "default", "BINANCE", 10);
    snapshot.cash_raw.insert("USDT".into(), 1000);
    snapshot.equity_raw = Some(1000);
    snapshot
        .positions
        .insert(instrument.clone(), position_row("BTCUSDT.BINANCE"));
    snapshot.orders.insert(
        77,
        OrderSnapshot {
            order_id: 77,
            client_order_id: 77,
            instrument: instrument.clone(),
            side: Side::Sell,
            quantity_raw: Quantity::from_i64(2).raw(),
            filled_raw: Quantity::from_i64(1).raw(),
            status: OrderStatus::Accepted,
        },
    );
    snapshot.fills.insert(
        9,
        FillSnapshot {
            fill_id: 9,
            order_id: 77,
            quantity_raw: Quantity::from_i64(1).raw(),
            price_raw: Price::from_i64(99).raw(),
            fee_raw: Money::from_i64(1).raw(),
            ts: 1234,
        },
    );
    snapshot.transfers.insert(
        3,
        TransferSnapshot {
            transfer_id: 3,
            currency: "USDT".into(),
            amount_raw: Money::from_i64(5).raw(),
            ts: 4321,
        },
    );
    snapshot.seal();
    snapshot
}

/// 稳定 JSON 里四张键表此前把裸数字直接插进对象（`"orders":{77:{...}}`），产出的不是一份
/// JSON：`AccountSnapshot::from_json`、`SqliteSnapshotStore::load_json` 与 Python 侧的
/// `load_account_snapshot` 会在同一份产物上全部失败（V11 R14）。这里钉三层：输出必须合法、
/// 必须被自己的读侧解回、键表必须与 serde 写出的那一份同源——自定义编码一旦和读侧分叉
/// （数字键、数字枚举码、少印一个字段），最后一条就会变红。
/// `positions` 同一条判据（V11 R15）：它的键口径与 `to_wire_json` 用的是同一个
/// `instrument_map`，读侧 `from_json` 认的是 serde 那份，写侧手抄的 6 个字段迟早漏掉新字段。
#[test]
fn stable_json_tables_round_trip_through_the_reader() {
    let snapshot = cross_language_sample();

    let json = snapshot.to_json();
    let parsed: serde_json::Value = serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("账户快照稳定 JSON 必须是合法 JSON: {error}"));
    let wire: serde_json::Value = serde_json::to_value(&snapshot).expect("快照可以序列化");
    for table in ["cash_raw", "positions", "orders", "fills", "transfers"] {
        assert_eq!(
            parsed[table], wire[table],
            "稳定 JSON 的 {table} 表必须与 serde 的那一份同源，两份编码迟早与读侧分叉"
        );
    }
    let restored = AccountSnapshot::from_json(&json)
        .unwrap_or_else(|error| panic!("稳定 JSON 必须能被 from_json 解回: {error:?}"));
    assert_eq!(
        restored, snapshot,
        "带持仓/订单/成交/划转的快照必须原样回到同一份状态"
    );
}

/// 跨语言夹具的路径：Python 侧的 `python/tests/test_bridge.py` 读同一个文件。
const ACCOUNT_SNAPSHOT_SAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../python/tests/fixtures/account-snapshot-v1.sample.json"
);

/// 对面那条读侧（`qianxing_bridge.load_account_snapshot`）吃到的必须是写侧真实产出的那一份
/// 文档（V11 R18）。此前 Python 用例喂进去的是手抄字典，四张键表全填 `{}`——写侧把键印成裸
/// 数字、枚举印数字码的那几周，它一条都不会红。夹具由 `to_json` 原样产出，两侧各钉一次：
/// 改编码时 Rust 这条先红，忘了重产夹具时 Python 那条红。
#[test]
fn the_cross_language_sample_is_what_the_writer_emits() {
    let expected = std::fs::read_to_string(ACCOUNT_SNAPSHOT_SAMPLE)
        .unwrap_or_else(|error| panic!("跨语言夹具必须存在: {error}"));
    assert_eq!(
        cross_language_sample().to_json() + "\n",
        expected.replace("\r\n", "\n"),
        "夹具必须由写侧原样产出：改了编码就重新产出夹具，不要手抄一份近似形状"
    );
}
