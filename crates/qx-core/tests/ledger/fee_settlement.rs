//! 手续费币种折算的归约用例：异币种费用按面值入账是 D2 的原始缺陷。

use super::*;

/// 交易所把手续费算成另一种币（Binance 的 BNB 抵扣、CCXT 的 `fee_currency`）时，
/// 按面值记进结算币种等于凭空造钱：0.001 BNB 记成 0.001 USDT 低估两个数量级。
/// 账本必须拒收，而不是等事后对账发现现金对不上。
#[test]
fn fill_with_foreign_fee_currency_is_not_booked_at_face_value() {
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let before = ledger.entries().to_vec();
    let error = ledger
        .apply_fill(
            &order(Side::Buy),
            &Fill {
                order_id: 7,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                fee: Money::from_i64(1),
                fee_currency: Some("BNB".into()),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
        )
        .expect_err("异币种手续费不能按面值入账");
    let message = format!("{error:?}");
    assert!(
        message.contains("BNB") && message.contains("USDT"),
        "拒收文案要同时点名费用币种与账簿币种: {message}"
    );
    assert!(matches!(error, QxError::ReconcileRequired(_)));
    assert_eq!(ledger.entries(), before, "被拒的成交不能留下现金痕迹");
}

/// 衍生条款走的是另一条归约函数，同一判据必须也在那里生效。
#[test]
fn derivative_fill_with_foreign_fee_currency_is_rejected() {
    let instrument = InstrumentId::parse("BTC/USDT:USDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: crate::TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 100,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    };
    let mut buy = order(Side::Buy);
    buy.client_id = 11;
    buy.instrument = instrument;
    buy.qty = Quantity::from_i64(1);
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let before = ledger.entries().len();
    let error = ledger
        .apply_fill_with_spec(
            &buy,
            &Fill {
                order_id: 11,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                fee: Money::from_i64(1),
                fee_currency: Some("FDUSD".into()),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
            &spec,
        )
        .expect_err("衍生成交的异币种手续费同样不能入账");
    assert!(matches!(error, QxError::ReconcileRequired(_)), "{error:?}");
    assert_eq!(ledger.entries().len(), before);
}

/// 闸门只管"报了别的币种"这一种事实：同币种的任意大小写照常记，零费用不产生费用腿，
/// 交易所没报币种时沿用既有口径 —— 把这三种情形一起拒掉会打断现役链路。
#[test]
fn fee_currency_gate_only_bites_on_reported_foreign_currency() {
    let cases: [(&str, Option<&str>, i64, usize); 4] = [
        ("USDT", Some("usdt"), 1, 4),
        ("USDT", Some(" USDT "), 1, 4),
        ("USDT", None, 1, 4),
        ("USDT", Some("BNB"), 0, 3),
    ];
    for (book, fee_currency, fee_units, expected_entries) in cases {
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", book, Money::from_i64(1000), 1)
            .unwrap();
        let ids = ledger
            .apply_fill(
                &order(Side::Buy),
                &Fill {
                    order_id: 7,
                    qty: Quantity::from_i64(2),
                    price: Price::from_i64(100),
                    fee: Money::from_i64(fee_units),
                    fee_currency: fee_currency.map(str::to_string),
                    ts: 2,
                    ..Fill::default()
                },
                book,
            )
            .unwrap_or_else(|error| {
                panic!("book={book} fee_currency={fee_currency:?} fee={fee_units} 应当可记账: {error:?}")
            });
        assert_eq!(
            ledger.entries().len(),
            expected_entries,
            "book={book} fee_currency={fee_currency:?} fee={fee_units}"
        );
        assert_eq!(
            ids.iter().any(|id| ledger
                .entries()
                .iter()
                .any(|entry| entry.id == *id && entry.kind == LedgerEntryKind::Fee)),
            fee_units > 0,
            "零费用不产生费用腿，非零费用必须有费用腿"
        );
    }
}

/// 现货里最常见的异币种费用：Binance 从**收到的那份资产**里扣手续费，所以 BTCUSDT 买入
/// 回报的是 `commissionAsset=BTC`（CCXT 的 `fee_currency` 同义）。这一类既不能按面值记
/// （0.001 BTC 记成 0.001 USDT 差两个数量级），也不能拒 —— 拒掉等于把主力连接器上每天
/// 正常发生的成交全部挡在链外。唯一不需要外部汇率的正确算法是用**这笔成交自己的价格**折算：
/// 0.001 BTC × 100 USDT/BTC = 0.1 USDT。
#[test]
fn spot_base_asset_fee_converts_at_the_fill_price() {
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let ids = ledger
        .apply_fill(
            &order(Side::Buy),
            &Fill {
                order_id: 7,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                fee: Money::from_raw(1_000_000),
                fee_currency: Some("BTC".into()),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
        )
        .expect("基准资产费用必须能按成交价折算入账");
    let fee = ledger
        .entries()
        .iter()
        .filter(|entry| ids.contains(&entry.id) && entry.kind == LedgerEntryKind::Fee)
        .collect::<Vec<_>>();
    assert_eq!(fee.len(), 1, "折算后的费用仍要留下一条费用腿");
    assert_eq!(
        fee[0].currency, "USDT",
        "费用腿记在结算币种上，读模型才不会再跨币种相加"
    );
    assert_eq!(
        fee[0].amount.raw(),
        -100_000_000,
        "0.001 BTC × 100 USDT/BTC 必须是 0.1 USDT，而不是 0.001 面值"
    );
}

/// 折算只在符号真的点名了那个币种时发生：`BTC3L-USDT` 的基准资产是 BTC3L，
/// 回报里写着 BTC 时对不上，此时账簿上没有任何能把这份 BTC 通约成 USDT 的价格，只能拒。
#[test]
fn base_asset_fee_is_refused_when_the_symbol_does_not_name_it() {
    let mut buy = order(Side::Buy);
    buy.instrument = InstrumentId::parse("BTC3L-USDT.BINANCE").unwrap();
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let before = ledger.entries().to_vec();
    let error = ledger
        .apply_fill(
            &buy,
            &Fill {
                order_id: 7,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                fee: Money::from_raw(1_000_000),
                fee_currency: Some("BTC".into()),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
        )
        .expect_err("符号里没有这个币种时不能凭猜测折算");
    assert!(matches!(error, QxError::ReconcileRequired(_)), "{error:?}");
    assert_eq!(ledger.entries(), before, "被拒的成交不能留下现金痕迹");
}

/// 合约乘数路径上的基准资产费用不折算：那一层的数量是"手"，把每手的费用直接乘成交价
/// 会漏掉 contract_size，算错的代价比拒记更高。
#[test]
fn multiplier_path_does_not_convert_base_asset_fees() {
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let before = ledger.entries().len();
    let error = ledger
        .apply_fill_with_multiplier(
            &order(Side::Buy),
            &Fill {
                order_id: 7,
                qty: Quantity::from_i64(2),
                price: Price::from_i64(100),
                fee: Money::from_raw(1_000_000),
                fee_currency: Some("BTC".into()),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
            10,
        )
        .expect_err("乘数路径不做基准资产折算");
    assert!(matches!(error, QxError::ReconcileRequired(_)), "{error:?}");
    assert_eq!(ledger.entries().len(), before);
}

/// 判据用折完的数、不用回报里的原数：极小费用 × 极低价折算到 0 时不再凭空留一条 0 费用腿。
#[test]
fn base_asset_fee_that_converts_to_zero_leaves_no_fee_leg() {
    let mut ledger = Ledger::new();
    ledger
        .deposit("main", "USDT", Money::from_i64(1000), 1)
        .unwrap();
    let ids = ledger
        .apply_fill(
            &order(Side::Buy),
            &Fill {
                order_id: 7,
                qty: Quantity::from_raw(1),
                price: Price::from_raw(1),
                fee: Money::from_raw(1),
                fee_currency: Some("BTC".into()),
                ts: 2,
                ..Fill::default()
            },
            "USDT",
        )
        .expect("折算是 1e-9 × 1e-9 量级的正常结果，不能报错");
    assert_eq!(ledger.entries().len(), 3, "现金腿 + 持仓腿，不该再有费用腿");
    assert!(
        !ids.iter().any(|id| ledger
            .entries()
            .iter()
            .any(|entry| entry.id == *id && entry.kind == LedgerEntryKind::Fee)),
        "折算为 0 的费用不产生费用腿"
    );
}
