//! 回测链的账户本金（V11 Q72）：没声明时按哪一个数、声明时原样落地、非正声明报错。
//!
//! 命令行侧的形状（哪条链印/拒/写摘要）在 `crates/qx-cli/tests/backtest_account_base.rs`，
//! 这里只钉"必答题"本身的三条读法。

use super::*;

/// 没人声明时给出**有名字的**默认，并把来源写成 `builtin-default`。
///
/// 常数本身也被钉住：它是所有未声明回测的收益率分母，改小一位就会让仓库里每一份
/// "没配本金"的历史产物失去可比性，这类改动必须让用例先红一次。
#[test]
fn undeclared_backtest_cash_answers_with_the_one_named_default() {
    let base = backtest_initial_cash(None).unwrap();
    assert_eq!(DEFAULT_BACKTEST_INITIAL_CASH, 100_000);
    assert_eq!(base.cash, Money::from_i64(DEFAULT_BACKTEST_INITIAL_CASH));
    assert_eq!(base.source, BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE);
    assert_eq!(
        backtest_account_base_note(base),
        "initial_cash_raw=100000000000000 account_base_source=builtin-default"
    );
}

/// 声明值原样进账户，来源改口为 `strategy-initial-cash`：与"没配但数值恰好相同"可区分。
#[test]
fn declared_backtest_cash_lands_verbatim_with_its_own_source() {
    let declared = Money::from_i64(DEFAULT_BACKTEST_INITIAL_CASH);
    let base = backtest_initial_cash(Some(declared.raw())).unwrap();
    assert_eq!(
        base.cash, declared,
        "声明 100,000 与没声明必须落到同一个数，但来源不能混"
    );
    assert_eq!(base.source, BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE);
    assert_eq!(
        backtest_account_base_note(base),
        "initial_cash_raw=100000000000000 account_base_source=strategy-initial-cash"
    );
    // 定点小数同样原样保留：折成整数单位等于把使用者给的尺度改窄，还看不出来。
    let odd = Money::from_raw(123_456_789);
    assert_eq!(
        backtest_initial_cash(Some(odd.raw())).unwrap().cash,
        odd,
        "本金是 1e-9 定点数，不得被任何整数单位截断"
    );
}

/// 非正的声明报错而不是回落默认：0 或负数账户上跑出来的收益率没有含义。
#[test]
fn non_positive_declared_cash_fails_rather_than_becoming_a_default() {
    for raw in [0, -1, -1_000_000_000] {
        let error = backtest_initial_cash(Some(raw)).unwrap_err();
        assert!(
            error.contains("strategy.initial_cash_raw") && error.contains(&format!("{raw}")),
            "报错要点出是哪一格、值是多少: {error}"
        );
        assert!(
            error.contains("不是正的本金"),
            "报错要说清为什么不能只是回落: {error}"
        );
    }
}
