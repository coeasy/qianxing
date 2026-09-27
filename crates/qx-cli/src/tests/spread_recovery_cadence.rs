use super::*;

/// #168c 行为判据：多腿对冲恢复的节律只按"这一轮扫描有没有推进"退避，并且**永不放弃**。
///
/// 停在 `HedgeRequired` 的分组是一条腿已成交、另一条还没对冲的裸腿。照行情链那套"连续
/// 失败到上限就具名报错"处理，等于让几次网络抖动之后把敞口永久晾着 —— 那比原来的
/// 每 100ms 起灭一个 Python 子进程更危险。所以这条链要的判据是"退避有界、循环无界"。
#[test]
fn spread_recovery_backoff_is_capped_but_never_gives_up() {
    assert_eq!(
        spread_recovery_poll_delay(0),
        Duration::from_millis(100),
        "没有积压时仍是 100ms 忙轮询"
    );
    assert_eq!(
        spread_recovery_poll_delay(1),
        Duration::from_millis(500),
        "第一档退避与 `qx-core::retry` 的口径同源"
    );
    let mut previous = Duration::from_millis(100);
    for stalls in 1..=200 {
        let delay = spread_recovery_poll_delay(stalls);
        assert!(delay >= previous, "退避必须单调不减: stalls={stalls}");
        previous = delay;
    }
    assert_eq!(
        previous,
        Duration::from_secs(8),
        "原地不动 200 轮后仍要给出一个正的间隔，而不是判死这条恢复链"
    );
}

/// #168c 接线判据：三条恢复循环（Binance / CCXT / Paper）都走同一份节律函数，`100ms`
/// 只剩 `spread_recovery_poll_delay` 里那一处定义。只测上面那条纯函数不会发现某个循环被改
/// 回 `thread::sleep(Duration::from_millis(100))` —— 那正是原地不动时每秒起灭 10 个恢复尝试
/// 的形状，所以按调用点逐个数。
#[test]
fn every_recovery_loop_polls_through_the_shared_cadence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let source_at = |rel: &str| {
        std::fs::read_to_string(root.join("crates").join("qx-cli").join("src").join(rel)).unwrap()
    };
    let worker_entry = source_at("worker_entry.rs");
    for (file, loops, source) in [
        ("worker_entry.rs", 2, worker_entry.clone()),
        (
            "venue_runtime/paper_worker.rs",
            1,
            source_at("venue_runtime/paper_worker.rs"),
        ),
    ] {
        assert_eq!(
            source
                .matches("thread::sleep(spread_recovery_poll_delay(recovery_stalls))")
                .count(),
            loops,
            "{file} 的恢复循环不是都按共享节律睡"
        );
        assert_eq!(
            source.matches("pending_spread_recovery_groups(").count(),
            loops * 2,
            "扫描前后各次数一次才算得出\"这一轮有没有推进\""
        );
        assert_eq!(
            source.matches("if pending_before > 0 {").count(),
            loops,
            "{file} 的恢复扫描没有按积压开关：没有待对冲分组时也会每轮起子进程/开 EventLog"
        );
        assert_eq!(
            source
                .matches("if pending_after >= pending_before {")
                .count(),
            loops,
            "推进判定必须按\"分组没减少就算原地不动\"，每条循环各一处"
        );
    }
    // 两条 venue 循环所在的文件里不许再出现固定 100ms：那份节律只剩一处定义。
    // （paper_worker.rs 不并入这条，是因为它的执行循环本就按固定节拍消费队列。）
    assert!(
        !worker_entry.contains("thread::sleep(Duration::from_millis(100))"),
        "worker_entry.rs 里固定 100ms 忙轮询回来了：分组原地不动时会每秒起灭 10 个恢复尝试"
    );
}
