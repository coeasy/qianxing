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
use qx_protocol::{AccountSnapshot, FillSnapshot, OrderSnapshot, PositionSnapshot};
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
        unrealized_pnl: Money::from_i64(4),
        initial_margin: Money::from_i64(20),
        maintenance_margin: Money::from_i64(9),
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
    assert_eq!(wire.unrealized_pnl_raw, Money::from_i64(4).raw());
    // 两个保证金维度在线格式上收敛为 initial margin（唯一口径）。
    assert_eq!(wire.margin_raw, Money::from_i64(20).raw());

    let restored = AccountPositionSnapshot::from(&wire);
    assert_eq!(restored.instrument, base.instrument);
    assert_eq!(restored.quantity, base.quantity);
    assert_eq!(restored.average_price, base.average_price);
    assert_eq!(restored.mark_price, base.mark_price);
    assert_eq!(restored.unrealized_pnl, base.unrealized_pnl);
    assert_eq!(restored.initial_margin, base.initial_margin);
    // 线格式不携带的观察维度显式退回 None，而不是编造值。
    assert_eq!(restored.liquidation_price, None);
    assert_eq!(restored.leverage, None);
    assert_eq!(restored.margin_mode, None);
    assert_eq!(restored.position_side, None);
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
