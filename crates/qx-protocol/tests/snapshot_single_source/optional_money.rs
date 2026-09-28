use super::*;

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

/// 持仓行的一份"两列钱都没算过"的形状，`stable_json_tables` 的跨语言夹具共用它。
pub(super) fn position_row(instrument: &str) -> PositionSnapshot {
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
