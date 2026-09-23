//! P1a 概念单点化 · 快照类型不变量（V10 §4.7）。
//!
//! 这些用例在"重复定义回来"时必须变红：
//! 1. `qx-protocol` 再声明一份本地 `AccountPositionSnapshot`/`AccountBalance`
//!    → 与 crate 根的再导出直接冲突（E0252），整个 crate 编译失败。
//! 2. 删除再导出 / `From` 折算层，`qx-cli` 重新用两套别名导入同名快照
//!    → 下面的 TypeId、折算与别名扫描用例红。
//! 3. 任何 crate 里再次出现第二处 `pub struct TargetPosition` / `PositionSnapshot`
//!    / `AccountPositionSnapshot` / `AccountBalance`
//!    → `concept_definitions_are_single_sourced` 红。

use qx_core::{
    AccountBalance, AccountPositionSnapshot, Fill, InstrumentId, Money, Order, OrderStatus, Price,
    Quantity, Side, Ts,
};
use qx_protocol::{
    AccountSnapshot, FillSnapshot, OrderSnapshot, PositionSnapshot, TransferSnapshot,
};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn observation(instrument: &str) -> AccountPositionSnapshot {
    AccountPositionSnapshot {
        instrument: InstrumentId::parse(instrument).expect("valid instrument"),
        quantity: Quantity::from_i64(3),
        average_price: Some(Price::from_i64(101)),
        mark_price: Some(Price::from_i64(105)),
        liquidation_price: Some(Price::from_i64(80)),
        unrealized_pnl: Some(Money::from_i64(4)),
        initial_margin: Some(Money::from_i64(20)),
        maintenance_margin: Some(Money::from_i64(9)),
        leverage: Some(5),
        margin_mode: Some("cross".into()),
        position_side: Some("LONG".into()),
    }
}

/// 快照概念的唯一定义在 `qx-core`；`qx-protocol` 只再导出，不重复定义。
#[test]
fn protocol_reexports_the_kernel_observation_types() {
    assert_eq!(
        TypeId::of::<qx_protocol::AccountPositionSnapshot>(),
        TypeId::of::<AccountPositionSnapshot>(),
        "qx-protocol 必须再导出 qx-core 的 AccountPositionSnapshot，而不是自带一份"
    );
    assert_eq!(
        TypeId::of::<qx_protocol::AccountBalance>(),
        TypeId::of::<AccountBalance>(),
        "qx-protocol 必须再导出 qx-core 的 AccountBalance，而不是自带一份"
    );
    assert_eq!(
        TypeId::of::<qx_protocol::Fill>(),
        TypeId::of::<Fill>(),
        "qx-protocol 必须再导出 qx-core 的 Fill，成交线格式只做折算"
    );
}

/// 内核观察 → 线格式 → 内核观察：折算只发生在唯一的一对 `From` 实现里。
#[test]
fn core_observation_converts_through_the_single_wire_layer() {
    let base = observation("BTCUSDT-PERP.BINANCE");
    let wire = PositionSnapshot::from(&base);
    assert_eq!(wire.quantity_raw, Quantity::from_i64(3).raw());
    assert_eq!(wire.today_quantity_raw, wire.quantity_raw);
    assert_eq!(wire.average_price_raw, Price::from_i64(101).raw());
    assert_eq!(wire.mark_price_raw, Price::from_i64(105).raw());
    assert_eq!(wire.unrealized_pnl_raw, Some(Money::from_i64(4).raw()));
    // 两个保证金维度在线格式上收敛为 initial margin（唯一口径）。
    assert_eq!(wire.margin_raw, Some(Money::from_i64(20).raw()));

    let restored = AccountPositionSnapshot::from(&wire);
    assert_eq!(restored.instrument, base.instrument);
    assert_eq!(restored.quantity, base.quantity);
    assert_eq!(restored.average_price, base.average_price);
    assert_eq!(restored.mark_price, base.mark_price);
    assert_eq!(restored.unrealized_pnl, base.unrealized_pnl);
    assert_eq!(restored.initial_margin, base.initial_margin);
    // 线格式不携带的观察维度显式退回 None，而不是编造值。维护保证金曾经读回 0，
    // 那等于把"线格式没这一列"说成"交易所说维护保证金为零"（V11 Q68）。
    assert_eq!(restored.liquidation_price, None);
    assert_eq!(restored.maintenance_margin, None);
    assert_eq!(restored.leverage, None);
    assert_eq!(restored.margin_mode, None);
    assert_eq!(restored.position_side, None);
}

/// 交易所没报的钱必须一路以"缺席"的形状走到读侧（V11 Q68）。
///
/// 折算层是唯一允许在"内核观察"和"线格式"之间搬运这三列的地方，所以缺席只能在
/// 这里被保留或被抹掉：`map` 写成 `unwrap_or_default()` 就是把没报变成零。
#[test]
fn absent_venue_money_stays_absent_through_the_wire_folding() {
    let reported = observation("BTCUSDT-PERP.BINANCE");
    let mut unreported = reported.clone();
    unreported.unrealized_pnl = None;
    unreported.initial_margin = None;
    unreported.maintenance_margin = None;

    let wire = PositionSnapshot::from(&unreported);
    assert_eq!(
        (wire.unrealized_pnl_raw, wire.margin_raw),
        (None, None),
        "交易所没报的两列折算后仍是缺席，不能长成 0"
    );
    // 有价无钱的持仓行是真实形状：数量与价格照旧过去，只有钱缺席。
    assert_eq!(wire.quantity_raw, reported.quantity.raw());
    assert_eq!(wire.mark_price_raw, Price::from_i64(105).raw());

    let restored = AccountPositionSnapshot::from(&wire);
    assert_eq!(restored.unrealized_pnl, None);
    assert_eq!(restored.initial_margin, None);
    assert_eq!(restored.mark_price, reported.mark_price);

    // 报了零与没报是两份观察：折算后必须在同两列上分开。
    let mut settled_zero = unreported.clone();
    settled_zero.unrealized_pnl = Some(Money::ZERO);
    settled_zero.initial_margin = Some(Money::ZERO);
    let zero_wire = PositionSnapshot::from(&settled_zero);
    assert_eq!(
        (zero_wire.unrealized_pnl_raw, zero_wire.margin_raw),
        (Some(0), Some(0))
    );
    assert_ne!(zero_wire, wire);
}

#[test]
fn order_and_fill_facts_use_the_same_wire_projection() {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let order = Order {
        client_id: 77,
        instrument: instrument.clone(),
        side: Side::Sell,
        qty: Quantity::from_i64(2),
        limit: Some(Price::from_i64(99)),
        status: OrderStatus::Accepted,
        filled: Quantity::from_i64(1),
        account_id: "main".into(),
        trace: None,
        policy: None,
    };
    assert_eq!(
        OrderSnapshot::from(&order),
        OrderSnapshot {
            order_id: 77,
            client_order_id: 77,
            instrument,
            side: Side::Sell,
            quantity_raw: Quantity::from_i64(2).raw(),
            filled_raw: Quantity::from_i64(1).raw(),
            status: OrderStatus::Accepted,
        }
    );

    let fill = Fill {
        order_id: 77,
        qty: Quantity::from_i64(1),
        price: Price::from_i64(99),
        fee: Money::from_i64(0),
        ts: 1234 as Ts,
        account_id: "main".into(),
        strategy_id: None,
        signal_id: None,
        intent_id: None,
        venue_id: None,
        venue_order_id: None,
        rule_version: None,
        fee_currency: None,
    };
    assert_eq!(
        FillSnapshot::from_fact(9, &fill),
        FillSnapshot {
            fill_id: 9,
            order_id: 77,
            quantity_raw: Quantity::from_i64(1).raw(),
            price_raw: Price::from_i64(99).raw(),
            fee_raw: 0,
            ts: 1234,
        }
    );
}

/// 净现金口径只有 `AccountBalance::net_cash_raw` 一份实现。
#[test]
fn net_cash_uses_the_single_kernel_formula() {
    let balance = AccountBalance {
        asset: "USDT".into(),
        free: Money::from_i64(100),
        locked: Money::from_i64(30),
        borrowed: Money::from_i64(-20),
    };
    assert_eq!(balance.net_cash_raw(), Some(Money::from_i64(150).raw()));

    let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    snapshot.upsert_balance(&balance).unwrap();
    assert_eq!(
        snapshot.cash_raw.get("USDT").copied(),
        balance.net_cash_raw()
    );

    let overflowed = AccountBalance {
        asset: "USDT".into(),
        free: Money::from_raw(i128::MAX),
        locked: Money::from_raw(1),
        borrowed: Money::ZERO,
    };
    assert_eq!(overflowed.net_cash_raw(), None);
    let mut batch = AccountSnapshot::new(2, "main", "default", "BINANCE", 11);
    batch.cash_raw.insert("USDT".into(), 7);
    let error = batch
        .upsert_balances(&[
            AccountBalance {
                asset: "USD".into(),
                free: Money::from_i64(5),
                locked: Money::ZERO,
                borrowed: Money::ZERO,
            },
            overflowed,
        ])
        .expect_err("溢出必须 fail-closed");
    assert!(matches!(error, qx_protocol::ProtocolError::Invalid(_)));
    // fail-closed 语义：批量失败时一条都不生效。
    assert_eq!(
        batch.cash_raw,
        BTreeMap::from([(String::from("USDT"), 7_i128)])
    );
}

fn workspace_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("cwd");
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && std::fs::read_to_string(&manifest)
                .unwrap_or_default()
                .contains("[workspace]")
        {
            return dir;
        }
        if !dir.pop() {
            panic!("未找到 workspace 根目录：概念单点化用例依赖 cargo 在 workspace 内运行");
        }
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 架构不变量：按符号定义点计数，重复即红。
#[test]
fn concept_definitions_are_single_sourced() {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let mut files = Vec::new();
    collect_rs_files(&crates_dir, &mut files);
    assert!(files.len() > 50, "扫描范围异常：{:#?}", crates_dir);

    for concept in [
        "TargetPosition",
        "PositionSnapshot",
        "AccountPositionSnapshot",
        "AccountBalance",
    ] {
        let prefix = format!("pub struct {concept}");
        let mut hits: Vec<String> = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            for (index, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if !trimmed.starts_with(&prefix) {
                    continue;
                }
                let rest = &trimmed[prefix.len()..];
                // 只统计这个概念本身的定义，排除 `PositionSnapshotStore` 这类前缀匹配。
                if rest.starts_with(|c: char| c == '{' || c.is_whitespace()) {
                    hits.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(&root).unwrap_or(file.as_path()).display(),
                        index + 1,
                        trimmed
                    ));
                }
            }
        }
        assert_eq!(
            hits.len(),
            1,
            "概念 `{concept}` 必须只有 1 处 struct 定义，实际 {} 处:\n{}",
            hits.len(),
            hits.join("\n")
        );
    }
}

/// 架构不变量：快照转换层存在 ⇒ 任何 crate 都不需要再把两套快照**互相别名**导入。
///
/// V10 §4.7 的原始症状是 `crates/qx-cli/src/main.rs` 同时写
/// `AccountPositionSnapshot as VenuePositionSnapshot` 与
/// `PositionSnapshot as AccountPositionSnapshot`（同名指向两种类型，读代码时
/// 无法分辨）。再导出/折算层被删掉后这种别名就会回来，本用例即红。
#[test]
fn no_crate_aliases_the_two_snapshot_names_over_each_other() {
    let root = workspace_root();
    let mut offenders = Vec::new();
    for crate_entry in std::fs::read_dir(root.join("crates")).expect("crates dir") {
        let src = crate_entry.expect("crate entry").path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_rs_files(&src, &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            for (index, line) in text.lines().enumerate() {
                if line.contains("as AccountPositionSnapshot") {
                    offenders.push(format!(
                        "{}:{}",
                        file.strip_prefix(&root).unwrap_or(file.as_path()).display(),
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "快照类型必须经由 qx-protocol 的再导出/转换层单点化，禁止将线格式别名成 \
         AccountPositionSnapshot；违规位置:\n{}",
        offenders.join("\n")
    );
}

/// 八个"读过才算得出"的钱字段，按 `set_optional_money_field` 的下标顺序。
/// 权益排在末位是因为它晚到（V11 Q70）：这一层的权益要现金与**每一条**持仓的标记价
/// 才拼得出来，缺一条就是算不出，与其余七个同属"没读过就不能印数"的那一族。
const OPTIONAL_MONEY_FIELDS: [&str; 8] = [
    "available_raw",
    "margin_raw",
    "frozen_raw",
    "realized_pnl_raw",
    "unrealized_pnl_raw",
    "fees_raw",
    "funding_raw",
    "equity_raw",
];

fn set_optional_money_field(snapshot: &mut AccountSnapshot, index: usize, value: Option<i128>) {
    match index {
        0 => snapshot.available_raw = value,
        1 => snapshot.margin_raw = value,
        2 => snapshot.frozen_raw = value,
        3 => snapshot.realized_pnl_raw = value,
        4 => snapshot.unrealized_pnl_raw = value,
        5 => snapshot.fees_raw = value,
        6 => snapshot.funding_raw = value,
        7 => snapshot.equity_raw = value,
        _ => panic!("未知的可选钱字段下标 {index}"),
    }
}

fn optional_money_field(snapshot: &AccountSnapshot, index: usize) -> Option<i128> {
    match index {
        0 => snapshot.available_raw,
        1 => snapshot.margin_raw,
        2 => snapshot.frozen_raw,
        3 => snapshot.realized_pnl_raw,
        4 => snapshot.unrealized_pnl_raw,
        5 => snapshot.fees_raw,
        6 => snapshot.funding_raw,
        7 => snapshot.equity_raw,
        _ => panic!("未知的可选钱字段下标 {index}"),
    }
}

/// "这一层没算"与"算过、结果为零"必须是两份不同的状态（V11 Q67，Q70 把权益并进来）。
///
/// 这两个量此前都是 `i128`，`Option` 化只在类型上区分了它们；真正咬合的地方有三处，
/// 少任何一处都会让"未算"退回伪装成 0：
/// 1. `state_hash` 必须带存在性标记——否则一份快照从"没算保证金"改成"算出保证金为 0"
///    不会改变哈希， sealed 快照的哈希就证明不了它说的是哪一份状态。
/// 2. 稳定 JSON 与无损线格式都必须印 `null`，不能印合法的整数 0。
/// 3. `diff` 必须把这条改动认成一次真实的标量替换，否则增量同步会把它吞掉。
#[test]
fn uncomputed_money_is_not_the_same_state_as_computed_zero() {
    for (index, name) in OPTIONAL_MONEY_FIELDS.iter().enumerate() {
        let mut uncomputed = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
        let mut computed_zero = uncomputed.clone();
        set_optional_money_field(&mut computed_zero, index, Some(0));
        assert_eq!(
            optional_money_field(&uncomputed, index),
            None,
            "默认构造必须把 {name} 留在未算状态，而不是先替账户算出一个数"
        );

        assert_ne!(
            uncomputed.state_hash(),
            computed_zero.state_hash(),
            "{name} 从「未算」变成「算出为 0」必须改变 state_hash，否则哈希区分不了两份状态"
        );
        uncomputed.seal();
        computed_zero.seal();
        uncomputed.validate().expect("未算快照封存后仍须自洽");
        computed_zero.validate().expect("已算快照封存后仍须自洽");

        let absent = uncomputed.to_json();
        let settled = computed_zero.to_json();
        assert!(
            absent.contains(&format!("\"{name}\":null")),
            "{name} 未算时稳定 JSON 必须印 null: {absent}"
        );
        assert!(
            settled.contains(&format!("\"{name}\":0")),
            "{name} 算出为零时稳定 JSON 必须印 0: {settled}"
        );

        for (source, expected, sealed) in [
            (&absent, None, uncomputed.state_hash()),
            (&settled, Some(0), computed_zero.state_hash()),
        ] {
            let parsed = AccountSnapshot::from_json(source).expect("稳定 JSON 必须可解析");
            assert_eq!(
                optional_money_field(&parsed, index),
                expected,
                "{name} 经稳定 JSON 往返后必须保住「未算/已算」的区分"
            );
            assert_eq!(
                parsed.state_hash(),
                sealed,
                "{name} 往返后的内容必须落在同一份状态哈希上"
            );
            assert_eq!(
                parsed.header.state_hash, sealed,
                "{name} 往返必须保住封存时写下的那个哈希"
            );
        }
        let wire = computed_zero
            .to_wire_json()
            .expect("已算快照必须能进无损线格式");
        assert!(
            wire.contains(&format!("\"{name}\":0")),
            "{name} 算出为零时线格式必须印 0: {wire}"
        );
        let restored = AccountSnapshot::from_wire_json(&wire).expect("线格式必须可回读");
        assert_eq!(restored, computed_zero, "{name} 的线格式往返不得丢失状态");
        let absent_wire = uncomputed.to_wire_json().expect("未算快照必须能进线格式");
        assert!(
            absent_wire.contains(&format!("\"{name}\":null")),
            "{name} 未算时线格式必须印 null: {absent_wire}"
        );

        // 增量同步路径：`diff` + `apply` 必须把这条改动带到对端，而不是当成"没变"吞掉。
        let applied = uncomputed
            .diff(&computed_zero)
            .expect("同身份快照必须可比对")
            .apply(&uncomputed)
            .unwrap_or_else(|error| {
                panic!("{name} 从「未算」到「算出为 0」的增量必须可应用: {error:?}")
            });
        assert_eq!(
            optional_money_field(&applied, index),
            Some(0),
            "{name} 经增量应用后必须落在算出来的那个 0 上"
        );
        // 反方向同样是一次真实改动：从「算出为零」退回「未算」也不能被吞掉。
        let reverted = computed_zero
            .diff(&uncomputed)
            .expect("同身份快照必须可比对")
            .apply(&computed_zero)
            .unwrap_or_else(|error| {
                panic!("{name} 从「算出为 0」退回「未算」的增量必须可应用: {error:?}")
            });
        assert_eq!(
            optional_money_field(&reverted, index),
            None,
            "{name} 退回未算时增量应用必须抹掉那个 0"
        );
    }
}

/// 持仓行的两列钱，按 `set_position_money_field` 的下标顺序。
const OPTIONAL_POSITION_MONEY_FIELDS: [&str; 2] = ["unrealized_pnl_raw", "margin_raw"];
const POSITION_ROW_INSTRUMENT: &str = "BTCUSDT-PERP.BINANCE";

fn position_row(instrument: &str) -> PositionSnapshot {
    PositionSnapshot {
        instrument: InstrumentId::parse(instrument).expect("valid instrument"),
        quantity_raw: Quantity::from_i64(3).raw(),
        today_quantity_raw: Quantity::from_i64(3).raw(),
        average_price_raw: Price::from_i64(101).raw(),
        mark_price_raw: Price::from_i64(105).raw(),
        unrealized_pnl_raw: None,
        margin_raw: None,
    }
}

fn set_position_money_field(row: &mut PositionSnapshot, index: usize, value: Option<i128>) {
    match index {
        0 => row.unrealized_pnl_raw = value,
        1 => row.margin_raw = value,
        _ => panic!("未知的持仓行钱字段下标 {index}"),
    }
}

fn position_money_field(row: &PositionSnapshot, index: usize) -> Option<i128> {
    match index {
        0 => row.unrealized_pnl_raw,
        1 => row.margin_raw,
        _ => panic!("未知的持仓行钱字段下标 {index}"),
    }
}

/// 持仓行的钱与账户标量是同一条纪律（V11 Q68）：没算过的浮亏/保证金不能以 0 的身份
/// 被 `GET /account/positions` 长期发布。逐列验三处咬合点——存在性哈希、稳定 JSON 的
/// `null`、增量的双向搬运；任何一处退回 `unwrap_or(0)` 都会让"没报"变成"没有浮亏"。
#[test]
fn uncomputed_position_money_is_not_the_same_state_as_computed_zero() {
    let instrument = InstrumentId::parse(POSITION_ROW_INSTRUMENT).expect("valid instrument");
    for (index, name) in OPTIONAL_POSITION_MONEY_FIELDS.iter().enumerate() {
        let mut uncomputed = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
        uncomputed.equity_raw = Some(100);
        // 账户标量刻意算成 7：稳定 JSON 里同名的两处印字必须能分清谁是持仓行。
        uncomputed.margin_raw = Some(7);
        uncomputed.unrealized_pnl_raw = Some(7);
        uncomputed
            .positions
            .insert(instrument.clone(), position_row(POSITION_ROW_INSTRUMENT));
        let mut computed_zero = uncomputed.clone();
        set_position_money_field(
            computed_zero.positions.get_mut(&instrument).unwrap(),
            index,
            Some(0),
        );
        assert_eq!(
            position_money_field(&uncomputed.positions[&instrument], index),
            None,
            "默认持仓行必须把 {name} 留在未算状态"
        );

        assert_ne!(
            uncomputed.state_hash(),
            computed_zero.state_hash(),
            "持仓行 {name} 从「未算」变成「算出为 0」必须改变 state_hash"
        );
        uncomputed.seal();
        computed_zero.seal();
        uncomputed.validate().expect("未算持仓行封存后仍须自洽");
        computed_zero.validate().expect("已算持仓行封存后仍须自洽");

        let absent = uncomputed.to_json();
        let settled = computed_zero.to_json();
        assert!(
            absent.contains(&format!("\"{name}\":null"))
                && absent.contains(&format!("\"{name}\":7")),
            "{name} 未算时持仓行印 null、账户标量仍印算出来的 7: {absent}"
        );
        assert!(
            settled.contains(&format!("\"{name}\":0"))
                && settled.contains(&format!("\"{name}\":7")),
            "{name} 算出为零时持仓行印 0、账户标量不受影响: {settled}"
        );

        for (source, expected, sealed) in [
            (&absent, None, uncomputed.state_hash()),
            (&settled, Some(0), computed_zero.state_hash()),
        ] {
            let parsed = AccountSnapshot::from_json(source).expect("稳定 JSON 必须可解析");
            assert_eq!(
                position_money_field(&parsed.positions[&instrument], index),
                expected,
                "持仓行 {name} 经稳定 JSON 往返后必须保住「未算/已算」的区分"
            );
            assert_eq!(
                parsed.state_hash(),
                sealed,
                "持仓行 {name} 往返后的内容必须落在同一份状态哈希上"
            );
        }
        let wire = computed_zero
            .to_wire_json()
            .expect("已算持仓行的快照必须能进无损线格式");
        assert!(
            wire.contains(&format!("\"{name}\":0")),
            "持仓行 {name} 算出为零时线格式必须印 0: {wire}"
        );
        assert_eq!(
            AccountSnapshot::from_wire_json(&wire).expect("线格式必须可回读"),
            computed_zero,
            "持仓行 {name} 的线格式往返不得丢失状态"
        );
        let absent_wire = uncomputed.to_wire_json().expect("未算持仓行必须能进线格式");
        assert!(
            absent_wire.contains(&format!("\"{name}\":null")),
            "持仓行 {name} 未算时线格式必须印 null: {absent_wire}"
        );

        // 增量同步：持仓行的改动走 map 差分，必须被当成一次真实的 Upsert 带到对端。
        let applied = uncomputed
            .diff(&computed_zero)
            .expect("同身份快照必须可比对")
            .apply(&uncomputed)
            .unwrap_or_else(|error| {
                panic!("持仓行 {name} 从「未算」到「算出为 0」的增量必须可应用: {error:?}")
            });
        assert_eq!(
            position_money_field(&applied.positions[&instrument], index),
            Some(0),
            "持仓行 {name} 经增量应用后必须落在算出来的那个 0 上"
        );
        let reverted = computed_zero
            .diff(&uncomputed)
            .expect("同身份快照必须可比对")
            .apply(&computed_zero)
            .unwrap_or_else(|error| {
                panic!("持仓行 {name} 从「算出为 0」退回「未算」的增量必须可应用: {error:?}")
            });
        assert_eq!(
            position_money_field(&reverted.positions[&instrument], index),
            None,
            "持仓行 {name} 退回未算时增量应用必须抹掉那个 0"
        );
    }
}

/// `PositionSnapshot::default()` 本身也是一次表态（V11 Q68）：生态冒烟与协议内部的快照
/// 夹具都用 `..PositionSnapshot::default()` 起步、只覆盖自己知道的那几列，默认值兜一个 0
/// 就等于"没折算过的行替交易所报了零"。上一条钉的是折算后的两态，这条钉的是构造入口。
#[test]
fn default_position_row_reports_no_money() {
    let blank_row = PositionSnapshot::default();
    assert_eq!(
        (blank_row.unrealized_pnl_raw, blank_row.margin_raw),
        (None, None),
        "默认持仓行的两列钱必须是「没报」，Default 里兜一个 0 就是凭空造数"
    );
    assert_eq!(
        (
            AccountPositionSnapshot::from(&blank_row).unrealized_pnl,
            AccountPositionSnapshot::from(&blank_row).initial_margin
        ),
        (None, None),
        "默认行折算回内核后仍未报，而不是 Some(0)"
    );

    let instrument = InstrumentId::parse(POSITION_ROW_INSTRUMENT).expect("valid instrument");
    let mut row = blank_row;
    row.instrument = instrument.clone();
    let mut untouched = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
    untouched.equity_raw = Some(100);
    untouched.positions.insert(instrument.clone(), row);
    let mut reported_zero = untouched.clone();
    set_position_money_field(
        reported_zero.positions.get_mut(&instrument).unwrap(),
        0,
        Some(0),
    );
    // 默认行与"交易所报了零"的行必须是两份状态：Default 自己带着 0 时，一份从没折算过
    // 的快照会与一份真收到零回报的快照落在同一份哈希上，读模型就再也分不开两者。
    assert_ne!(
        untouched.state_hash(),
        reported_zero.state_hash(),
        "默认持仓行不得与「报了零」的持仓行共用一份状态哈希"
    );
}

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
