//! 成交事实到订单状态与账本的单一归约接缝。
//!
//! 回测引擎与运行时管线各自持有 [`Ledger`] 和 OMS，历史上两侧把"先推进订单状态
//! 还是先记账"写成了不同顺序，选择记账条款（产品规格 / 历史乘数）的分支也各自
//! 复制了一份，同一事实序列因此可能在两侧得到不同状态。本模块把这段归约收成
//! 唯一入口：调用方只提供事实与可用条款，顺序和校验由这里固定。

use crate::error::{QxError, QxResult};
use crate::ledger::Ledger;
use crate::order::{Fill, Order};
use crate::trading::TradingInstrumentSpec;

/// 一笔成交可用的记账条款。
#[derive(Clone, Copy, Debug)]
pub enum FillTerms<'a> {
    /// 按产品规格记账：`contract_size` 决定每手规模，现货规格由账本回落为现金语义。
    Instrument(&'a TradingInstrumentSpec),
    /// 无规格时的历史兼容路径：名义额按显式乘数折算。
    LegacyMultiplier(i128),
}

impl<'a> FillTerms<'a> {
    /// 条款选择的唯一规则：有产品规格时规格优先，历史乘数只在无规格时生效。
    ///
    /// 两侧曾因回测按"仅衍生品才用规格"、实盘按"有规格就用规格"选择分支，导致
    /// 现货规格订单在实盘多做一层 `OrderPolicy` 与 instrument 一致性校验。规格
    /// 优先让回测与实盘的校验集合对齐。
    pub fn resolve(spec: Option<&'a TradingInstrumentSpec>, legacy_multiplier: i128) -> Self {
        match spec {
            Some(spec) => Self::Instrument(spec),
            None => Self::LegacyMultiplier(legacy_multiplier),
        }
    }
}

/// 订单状态侧的最小读写接口。
///
/// 由 `qx-zhenlu` 的 `Oms` 实现，使内核不必反向依赖 OMS。写入被拆成"先在副本上归约、
/// 后提交"两步，是为了让更严格的一侧（状态机）能在校验阶段就拦下成交，而不会
/// 先把现金记进账本、再把订单留在半个状态上。
pub trait OrderFillBook {
    fn order_state(&self, client_order_id: u64) -> Option<Order>;
    /// 在订单副本上校验并推进成交，不改动任何持久状态。
    fn reduce_fill(&self, fill: &Fill) -> QxResult<Order>;
    /// 写回 [`OrderFillBook::reduce_fill`] 的产物。
    fn commit_fill(&mut self, order: Order);
}

/// 只记账、不推进订单状态机。
///
/// 用于交易所发起的成交（强平、交割等）：这类事实没有对应的本地委托，因此不能
/// 走 [`apply_fill_to_books`] 的查单步骤，但记账条款必须由同一规则选出。
pub fn apply_ledger_fill(
    ledger: &mut Ledger,
    order: &Order,
    currency: &str,
    fill: &Fill,
    terms: FillTerms<'_>,
) -> QxResult<Vec<u64>> {
    match terms {
        FillTerms::Instrument(spec) => ledger.apply_fill_with_spec(order, fill, currency, spec),
        FillTerms::LegacyMultiplier(multiplier) => {
            ledger.apply_fill_with_multiplier(order, fill, currency, multiplier)
        }
    }
}

/// 把一笔成交事实同时归约到账本和订单状态机，返回受影响的账本 entry id。
///
/// 归约顺序固定为：读订单 → 在副本上跑状态机校验 → 落账 → 提交订单状态。
/// 状态机是更严的一侧，放在落账之前，被拒的成交（例如尚未 `Submitted` 的订单收到
/// 回报）不会先动现金；[`Ledger`] 的成交入口内部以 clone-then-commit 落账，失败时
/// 两侧都不留痕迹。
pub fn apply_fill_to_books<B: OrderFillBook + ?Sized>(
    ledger: &mut Ledger,
    orders: &mut B,
    currency: &str,
    fill: &Fill,
    terms: FillTerms<'_>,
) -> QxResult<Vec<u64>> {
    let order = orders.order_state(fill.order_id).ok_or_else(|| {
        QxError::ReconcileRequired(format!("成交对应的本地订单不存在: {}", fill.order_id))
    })?;
    let next = orders.reduce_fill(fill)?;
    let entries = apply_ledger_fill(ledger, &order, currency, fill, terms)?;
    orders.commit_fill(next);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::InstrumentId;
    use crate::numeric::{Money, Price, Quantity, SCALE};
    use crate::order::OrderStatus;
    use crate::order::Side;
    use crate::trading::TradingProduct;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Book {
        orders: BTreeMap<u64, Order>,
    }

    /// 与 `qx-zhenlu` 的 `Oms` 同构的最小替身：迁移只走 [`OrderStatus::transition`]，
    /// 这样测试里被状态机拦下的成交与真实 OMS 的行为一致。
    impl OrderFillBook for Book {
        fn order_state(&self, client_order_id: u64) -> Option<Order> {
            self.orders.get(&client_order_id).cloned()
        }

        fn reduce_fill(&self, fill: &Fill) -> QxResult<Order> {
            let order = self
                .orders
                .get(&fill.order_id)
                .ok_or_else(|| QxError::Invariant("订单不存在".into()))?
                .clone();
            if fill.qty.raw() > order.remaining().raw() {
                return Err(QxError::Invariant("成交数量超过剩余数量".into()));
            }
            let mut next = order;
            if matches!(next.status, OrderStatus::Submitted | OrderStatus::Accepted) {
                next.status
                    .transition(OrderStatus::Working)
                    .map_err(QxError::Invariant)?;
            }
            next.filled = Quantity::from_raw(next.filled.raw() + fill.qty.raw());
            let status = if next.filled.raw() >= next.qty.raw() {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            next.status.transition(status).map_err(QxError::Invariant)?;
            Ok(next)
        }

        fn commit_fill(&mut self, order: Order) {
            self.orders.insert(order.client_id, order);
        }
    }

    fn spot_spec() -> TradingInstrumentSpec {
        TradingInstrumentSpec {
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            product: TradingProduct::Spot,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 1,
            valid_to: None,
        }
    }

    fn spot_order(client_id: u64) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::Accepted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }
    }

    fn fill(client_id: u64, qty: i128, fee: i128) -> Fill {
        Fill {
            order_id: client_id,
            qty: Quantity::from_raw(qty),
            price: Price::from_i64(100),
            fee: Money::from_raw(fee),
            ts: 10,
            account_id: "main".into(),
            ..Fill::default()
        }
    }

    #[test]
    fn missing_local_order_requires_reconcile_not_panic() {
        let mut ledger = Ledger::new();
        let mut book = Book::default();
        let error = apply_fill_to_books(
            &mut ledger,
            &mut book,
            "USDT",
            &fill(7, 1, 0),
            FillTerms::LegacyMultiplier(1),
        )
        .expect_err("缺失本地订单必须失败");
        assert!(
            matches!(error, QxError::ReconcileRequired(_)),
            "成交缺单应按需对账而非不变量崩溃: {error:?}"
        );
        assert!(ledger.entries().is_empty());
    }

    #[test]
    fn rejected_fill_leaves_both_books_untouched() {
        let mut ledger = Ledger::new();
        let mut book = Book::default();
        book.orders.insert(1, spot_order(1));
        ledger
            .deposit("main", "USDT", Money::from_i64(1000), 1)
            .unwrap();
        let entries_before = ledger.entries().len();

        let error = apply_fill_to_books(
            &mut ledger,
            &mut book,
            "USDT",
            &fill(1, 5 * SCALE, 0),
            FillTerms::LegacyMultiplier(1),
        )
        .expect_err("超额成交必须被拒");
        assert!(matches!(error, QxError::Invariant(_)), "{error:?}");
        assert_eq!(
            ledger.entries().len(),
            entries_before,
            "被拒的成交不得先动现金"
        );
        assert_eq!(book.orders[&1].filled, Quantity::ZERO);
    }

    /// 账本只挡终态订单，`PendingSubmit` 的成交在账本侧是合法的；若先落账再推进
    /// 状态机，被状态机拒掉的回报会留下已扣的现金。这条用例区分两种顺序。
    #[test]
    fn state_machine_rejection_precedes_any_ledger_write() {
        let mut ledger = Ledger::new();
        let mut book = Book::default();
        book.orders.insert(
            1,
            Order {
                status: OrderStatus::PendingSubmit,
                ..spot_order(1)
            },
        );
        ledger
            .deposit("main", "USDT", Money::from_i64(1000), 1)
            .unwrap();
        let entries_before = ledger.entries().to_vec();

        let error = apply_fill_to_books(
            &mut ledger,
            &mut book,
            "USDT",
            &fill(1, SCALE, 0),
            FillTerms::LegacyMultiplier(1),
        )
        .expect_err("未提交的订单不能收成交");
        assert!(matches!(error, QxError::Invariant(_)), "{error:?}");
        assert_eq!(
            ledger.entries(),
            entries_before,
            "状态机拒绝不得留下现金痕迹"
        );
        let order = &book.orders[&1];
        assert_eq!(order.status, OrderStatus::PendingSubmit);
        assert_eq!(order.filled, Quantity::ZERO);
    }

    #[test]
    fn accepted_fill_updates_both_books() {
        let mut ledger = Ledger::new();
        let mut book = Book::default();
        book.orders.insert(1, spot_order(1));
        ledger
            .deposit("main", "USDT", Money::from_i64(1000), 1)
            .unwrap();

        let entries = apply_fill_to_books(
            &mut ledger,
            &mut book,
            "USDT",
            &fill(1, SCALE, 0),
            FillTerms::LegacyMultiplier(1),
        )
        .expect("有效成交必须归约成功");
        assert_eq!(entries.len(), 2);
        assert_eq!(book.orders[&1].status, OrderStatus::PartiallyFilled);
        assert_eq!(book.orders[&1].filled, Quantity::from_i64(1));
        assert_eq!(ledger.entries().len(), 3);
        assert_eq!(ledger.cash("USDT"), Money::from_i64(900).raw());
    }

    #[test]
    fn terms_resolution_prefers_instrument_spec() {
        let spec = spot_spec();
        assert!(matches!(
            FillTerms::resolve(Some(&spec), 100),
            FillTerms::Instrument(resolved) if std::ptr::eq(resolved, &spec)
        ));
        assert!(matches!(
            FillTerms::resolve(None, 100),
            FillTerms::LegacyMultiplier(100)
        ));
    }

    #[test]
    fn spec_and_legacy_multiplier_paths_book_the_same_spot_cash() {
        let spec = spot_spec();
        let mut with_spec = Ledger::new();
        let mut with_multiplier = Ledger::new();
        for (ledger, terms) in [
            (&mut with_spec, FillTerms::resolve(Some(&spec), 100)),
            (&mut with_multiplier, FillTerms::resolve(None, 1)),
        ] {
            ledger
                .deposit("main", "USDT", Money::from_i64(1000), 1)
                .unwrap();
            apply_ledger_fill(ledger, &spot_order(1), "USDT", &fill(1, SCALE, 0), terms).unwrap();
        }
        assert_eq!(
            with_spec.entries(),
            with_multiplier.entries(),
            "规格条款必须与乘数 1 的历史条款记出同一份现货现金"
        );
    }

    #[test]
    fn ledger_only_path_shares_the_same_terms_rule() {
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USDT", Money::from_i64(1000), 1)
            .unwrap();
        let order = spot_order(1);
        let entries = apply_ledger_fill(
            &mut ledger,
            &order,
            "USDT",
            &fill(1, 1, 0),
            FillTerms::resolve(None, 1),
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
    }
}
