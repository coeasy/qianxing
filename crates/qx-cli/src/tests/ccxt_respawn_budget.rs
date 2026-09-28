//! 复活类循环的终止性判据（V11 K1、O5）：退避形状、死轮账、按标的的复活上界都是纯函数，
//! 现场（Python 子进程持续不可用、两份标的里固定坏一份）不必真的复现一次就能钉死。
//! 补散度恢复那一侧的轮间隔不在这里——V13 #168c 把两份间隔函数并成 `spread.rs` 里的一颗，
//! 它的行为与接线判据随那份收敛住在 `tests/spread_recovery_cadence.rs`。

use super::*;

/// 修前的形状是"任何一次失败都等 500 ms"：Python 子进程起不来时 worker 以每秒两次的
/// 频率复活它。等待必须随连续失败变长，并封顶在 10 秒（不是无上限，也不是不变）。
#[test]
fn ccxt_respawn_delay_backs_off_and_caps() {
    assert_eq!(ccxt_respawn_delay(1), Duration::from_millis(500));
    assert_eq!(ccxt_respawn_delay(2), Duration::from_millis(1_000));
    assert_eq!(ccxt_respawn_delay(3), Duration::from_millis(2_000));
    assert_eq!(ccxt_respawn_delay(4), Duration::from_millis(4_000));
    assert_eq!(ccxt_respawn_delay(5), Duration::from_millis(8_000));
    assert_eq!(ccxt_respawn_delay(6), Duration::from_secs(10));
    assert_eq!(ccxt_respawn_delay(64), Duration::from_secs(10));
    for failures in 1..5 {
        assert!(
            ccxt_respawn_delay(failures) < ccxt_respawn_delay(failures + 1),
            "第 {failures} 次连续失败的等待要长于上一次，否则退化成修前的固定 500 ms 自旋"
        );
    }
}

/// 死轮账只记"整轮全败"：轮内计数每轮归零曾是这条链的漏洞——复活后成功一次就忘了
/// 失败过，于是永远到不了 `Failed`。
#[test]
fn ccxt_dead_cycle_ledger_counts_only_wholly_failed_rounds() {
    let budget = CCXT_DEAD_CYCLE_BUDGET;
    assert!(budget > 1, "预算为 1 等于一次抖动就放弃复活公共 Worker");
    // 本轮有一次成功（3/4 失败）不构成死轮，账要清零而不是继续累加。
    assert_eq!(
        ccxt_dead_cycle_ledger(3, 4, budget - 1, budget),
        (0, None),
        "任何一次成功都要清零连续死轮"
    );
    // 空轮（没有标的可试）是配置形状，不是对端故障的证据。
    assert_eq!(ccxt_dead_cycle_ledger(0, 0, budget - 1, budget), (0, None));
    // 到预算前一格仍给重试机会。
    assert_eq!(
        ccxt_dead_cycle_ledger(4, 4, budget - 2, budget),
        (budget - 1, None)
    );
    let (dead, reason) = ccxt_dead_cycle_ledger(4, 4, budget - 1, budget);
    assert_eq!(dead, budget);
    let reason = reason.expect("到达预算必须给出终止原因，交给调用方标 Failed");
    assert!(
        reason.contains("全部失败"),
        "终止原因要念出判据，实际 {reason}"
    );
    assert!(
        reason.contains(&budget.to_string()),
        "终止原因要念出轮数，实际 {reason}"
    );
    assert!(
        ccxt_dead_cycle_ledger(4, 4, budget, budget).1.is_some(),
        "越过预算之后不能回到『还能再试』"
    );
}

/// 混合失败（两份标的里固定坏一份）是 K1 那两半**同时**失效的一格（V11 O5）：
/// `respawn_streak` 被同轮另一颗标的的成功复位回 1、死轮账又永远攒不到"整轮全败"，
/// 于是 kill+wait+spawn 以约 1 Hz 持续下去，状态永久停在 `Degraded`。换成按标的记连续
/// 失败之后，复活次数收在一个上界里，而"这颗标的好了没有"仍然问得到。
#[test]
fn a_symbol_that_never_recovers_stops_buying_resurrections() {
    let budget = CCXT_RESPAWN_STRIKE_BUDGET;
    // 正向对照：第一次失败必须去复活——上界不能把"真的只是进程没了"那种形状也挡掉。
    assert!(
        ccxt_respawn_allowed(1, budget),
        "首次失败必须尝试复活公共 Worker"
    );
    let mut strikes = 0_u32;
    let mut respawns = 0_u32;
    for _cycle in 0..64 {
        strikes = strikes.saturating_add(1);
        if ccxt_respawn_allowed(strikes, budget) {
            respawns += 1;
        }
        // 同一轮里另一颗标的成功：K1 的两半在这里同时复位，只有按标的的计数不受影响。
        assert_eq!(
            ccxt_dead_cycle_ledger(1, 2, budget, budget),
            (0, None),
            "这颗标的一直坏着而同轮另有成功，死轮账必须归零"
        );
    }
    assert_eq!(
        respawns, budget,
        "64 轮只坏同一颗标的时复活次数必须收在预算处，实际 {respawns}"
    );
    assert!(
        !ccxt_respawn_allowed(budget + 1, budget),
        "越过预算的连续失败不得继续复活"
    );
    // 那颗标的恢复之后计数归零，下一次失败重新买到一次复活。
    let recovered = 1_u32;
    assert!(
        ccxt_respawn_allowed(recovered, budget),
        "恢复之后再失败要重新给复活机会"
    );
    // 整进程不可用的形状没有被这条上界改掉：每颗标的一起失败时仍由死轮账收口。
    // 这里的 previous 是死轮账自己的"预算前一格"，不是上面那颗复活上界——两把尺子量的是
    // 两件事（同一颗标的连续失败几次 / 整轮连续全败几轮），混用就会把全败形状读成没到界。
    let (dead, reason) =
        ccxt_dead_cycle_ledger(2, 2, CCXT_DEAD_CYCLE_BUDGET - 1, CCXT_DEAD_CYCLE_BUDGET);
    assert_eq!(dead, CCXT_DEAD_CYCLE_BUDGET);
    assert!(reason.is_some(), "全败形状必须由死轮账给出终止原因");
}
