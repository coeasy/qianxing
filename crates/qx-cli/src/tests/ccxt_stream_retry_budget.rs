use super::*;

#[test]
fn ccxt_stream_backoff_escalates_and_resets_on_a_healthy_session() {
    let mut budget = CcxtReconnectBudget::default();
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(500));
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(1_000));
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(2_000));
    assert_eq!(budget.consecutive_failures(), 3);
    budget.note_success();
    assert_eq!(budget.consecutive_failures(), 0);
    // 清零后退避从第一档重新爬，不会记住健康会话之前的失败。
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(500));
}

#[test]
fn ccxt_stream_reconnect_budget_gives_up_after_consecutive_failures() {
    let mut budget = CcxtReconnectBudget::default();
    let mut last_delay = Duration::ZERO;
    for attempt in 1..=CcxtReconnectBudget::MAX_RECONNECTS {
        last_delay = budget
            .note_failure()
            .unwrap_or_else(|error| unreachable!("第 {attempt} 次重连不该放弃: {error}"));
    }
    assert_eq!(
        last_delay,
        Duration::from_secs(8),
        "退避应在 max_delay 处封顶"
    );
    match budget.note_failure() {
        Ok(delay) => unreachable!("超过上限后不该再等待 {delay:?}"),
        Err(error) => assert!(
            error.contains(&format!(
                "连续 {} 次",
                CcxtReconnectBudget::MAX_RECONNECTS + 1
            )),
            "{error}"
        ),
    }
}

/// #166 读侧判据：只有显式的 `idle: true` 才算空闲。真事件、缺键、`false` 与字符串
/// `"true"` 都不能被当成空闲，否则一笔成交会被静默丢掉而不进 EventLog。
#[test]
fn ccxt_watch_reply_only_treats_explicit_idle_flag_as_idle() {
    assert!(ccxt_watch_reply_is_idle(&serde_json::json!({
        "stream": "orders", "idle": true, "events": []
    })));
    for reply in [
        serde_json::json!({"stream": "orders", "events": [{"id": "remote-1"}]}),
        serde_json::json!({"stream": "orders", "idle": false, "events": []}),
        serde_json::json!({"stream": "orders", "idle": "true", "events": []}),
        serde_json::json!({"stream": "orders"}),
    ] {
        assert!(
            !ccxt_watch_reply_is_idle(&reply),
            "把回话判成了空闲，成交会被丢掉: {reply}"
        );
    }
}

/// #166 跨语言判据：Rust 发的 `wait_ms` 与 Python 答的 `idle` 必须两侧都还在写。
/// 单独看任一侧都能自洽，键名漂移只会表现为"空闲账户永远不出事件回话"。
///
/// V13 R17 C5 把同一条边界上的另外三张面孔并进来：子进程缺省等待窗口取读窗的 4/5（与
/// Rust 的 `ccxt_idle_window_ms` 同比值，否则子进程和父进程的读窗同时到期，"空闲回话赶在
/// 读窗之前"这条设计就只剩文字）、退避封顶与 `CcxtReconnectBudget::MAX_DELAY` 同值（否则
/// 基准 60000 毫秒 × 2**attempts 就是小时级一觉，进程在睡满之前早被杀掉重开），以及
/// "没有缺省窗口就一直 await"那条臂不得回来。三处都只在父进程被强杀或柜台静默时才发作，
/// 别处没有用例先喊。
#[test]
fn ccxt_idle_heartbeat_literals_exist_on_both_sides_of_the_boundary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let rust = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("venue_runtime")
            .join("ccxt_execution.rs"),
    )
    .unwrap();
    let python =
        std::fs::read_to_string(root.join("python").join("qianxing_ccxt").join("worker.py"))
            .unwrap();
    assert!(
        rust.contains("\"wait_ms\": watch_idle_ms") && rust.contains("ccxt_watch_reply_is_idle"),
        "Rust 侧不再发 wait_ms 或不再读 idle，空闲回话链路断了"
    );
    assert!(
        python.contains("request.get(\"wait_ms\")") && python.contains("event[\"idle\"] = True"),
        "Python Worker 侧不再收 wait_ms 或不再回 idle，空闲会被读窗判成断链"
    );
    let adapter = std::fs::read_to_string(
        root.join("crates")
            .join("qx-adapter")
            .join("src")
            .join("ccxt.rs"),
    )
    .unwrap();
    assert!(
        adapter.contains("timeout_ms.saturating_mul(4) / 5")
            && python.contains("else self.client.config.timeout_ms * 4 // 5")
            && !python.contains("if wait_ms is None:"),
        "缺省等待窗口不再与驱动方读窗同比值，或者\"没交 wait_ms 就 await 到永远\"那条臂回来了"
    );
    let budget = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("venue_runtime")
            .join("ccxt_stream_retry.rs"),
    )
    .unwrap();
    assert!(
        budget.contains("MAX_DELAY: Duration = Duration::from_secs(8)")
            && python.contains("WS_RETRY_MAX_DELAY_MS = 8_000")
            && python.matches("WS_RETRY_MAX_DELAY_MS").count() == 2,
        "退避封顶两侧不再同为 8 秒，或者 Python 侧的常量只剩定义、没人用"
    );
    assert_eq!(
        python.matches("CcxtErrorClass.RETRYABLE,").count(),
        2,
        "无名窗口到期不再报具名可重试故障，父进程的重连预算就记不到这一笔"
    );
}

/// #167 行为判据：行情 RPC 通道与用户流共用同一份预算口径（同一上限、同一退避），
/// 但放弃时必须点出自己那条通道 —— 否则运维会拿着"CCXT Pro 用户流"的文案去查一条
/// 行情链的错。两条文案各只有一处定义，所以这里两侧都得上断言。
#[test]
fn ccxt_market_rpc_budget_gives_up_and_names_the_market_channel() {
    let mut market = CcxtReconnectBudget::market_rpc();
    let mut user_stream = CcxtReconnectBudget::default();
    for attempt in 1..=CcxtReconnectBudget::MAX_RECONNECTS {
        let delay = market
            .note_failure()
            .unwrap_or_else(|error| unreachable!("第 {attempt} 次行情重连不该放弃: {error}"));
        assert_eq!(
            delay,
            user_stream
                .note_failure()
                .unwrap_or_else(|error| unreachable!("第 {attempt} 次用户流重连不该放弃: {error}")),
            "两条通道的退避必须同源，不能各算一份"
        );
    }
    let market_error = match market.note_failure() {
        Ok(delay) => unreachable!("超过上限后不该再等待 {delay:?}"),
        Err(error) => error,
    };
    assert!(
        market_error.contains("CCXT 行情子进程") && !market_error.contains("用户流"),
        "行情链的放弃文案认错通道: {market_error}"
    );
    assert!(
        user_stream.note_failure().unwrap_err().contains("用户流"),
        "用户流的放弃文案被行情通道顶掉，两条链在日志里就分不开了"
    );
}

/// #202 行为判据：一条通道的成功应答只能销自己那条通道的账。
///
/// 共享预算下"A 正常、B 已下架"的形状永远触不到上限（B 每轮被 A 清零），坏标的于是
/// 每轮重生一个 Python 子进程且无上限 —— #167 收掉的死循环换了个方向回来。这里让健康
/// 通道每轮都成功，坏通道必须照样在第 11 次连续失败时具名放弃，且点名要点到它自己。
#[test]
fn ccxt_market_budget_is_per_channel_so_a_healthy_symbol_cannot_reset_a_dead_one() {
    let mut budgets = CcxtChannelBudgets::market_rpc();
    let dead = "fetch_ticker:BAD/USDT";
    let alive = "fetch_ohlcv:GOOD/USDT";
    let mut solo = CcxtReconnectBudget::market_rpc();
    // 健康通道先抖一次再恢复：这次复位不能顺手把别的通道的账也清了。
    budgets
        .note_failure(alive)
        .expect("单次抖动不该放弃健康通道");
    budgets.note_success(alive);
    assert_eq!(
        budgets.consecutive_failures(alive),
        0,
        "健康通道自己的成功应答没把它复位"
    );
    for cycle in 1..=CcxtReconnectBudget::MAX_RECONNECTS {
        assert_eq!(
            budgets.note_failure(dead).unwrap(),
            solo.note_failure().unwrap(),
            "第 {cycle} 次坏通道的退避必须与独占该预算时同源"
        );
        budgets.note_success(alive);
    }
    let error = match budgets.note_failure(dead) {
        Ok(delay) => unreachable!("健康 symbol 每轮成功不该救回坏 symbol: {delay:?}"),
        Err(error) => error,
    };
    assert!(
        error.contains("CCXT 行情子进程") && error.contains(dead),
        "放弃文案没点名真正死掉的那条通道: {error}"
    );
    assert_eq!(
        budgets.consecutive_failures(alive),
        0,
        "坏通道触顶时把健康通道也拖进了计数"
    );
}

/// #167 接线判据：`run_ccxt_market_worker` 的每一处重生子进程都得先过预算，
/// 成功应答要能把计数复位。光测预算本身不会发现 worker 把 `note_failure()` 摘掉、
/// 退回"每轮清零 + 固定 500ms 重生"的旧形状，所以这里按调用点逐个数。
///
/// #202 把这条判据收到通道粒度：不带通道键的 `note_failure()?` 就是退回共享预算，
/// 一个健康 symbol 的成功应答会把坏 symbol 的账一起销掉。
#[test]
fn ccxt_market_worker_routes_every_respawn_through_the_reconnect_budget() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let source = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("venue_runtime")
            .join("ccxt_market_worker.rs"),
    )
    .unwrap();
    assert!(
        source.contains("CcxtChannelBudgets::market_rpc()"),
        "行情 worker 不再持有重连预算"
    );
    // 1 处初次 spawn + ticker/OHLCV 两个失败分支各 1 处预算内重生。
    assert_eq!(
        source.matches("CcxtProcessClient::spawn(").count(),
        3,
        "spawn 点数目变了：新增的重生点必须带预算，否则会退回无限重启子进程"
    );
    assert_eq!(
        source
            .matches("reconnect_budget.note_failure(&channel)?")
            .count(),
        2,
        "两个失败分支不是都按通道过预算"
    );
    assert_eq!(
        source
            .matches("reconnect_budget.note_success(&channel)")
            .count(),
        2,
        "成功应答不再复位连续失败计数，偶发失败会累计成误判"
    );
    // 带 `&channel` 的两处各有 1 处 `let channel` 赋值，共 4 处含 "channel" 的行。
    assert_eq!(
        source.matches("let channel = format!(").count(),
        2,
        "两个循环各自命名自己的通道，缺一个就等于把那条链放回共享预算"
    );
    assert!(
        source.contains("\"fetch_ticker:{instrument}\"")
            && source.contains("\"fetch_ohlcv:{}\", spec.instrument"),
        "通道键必须带上 op 与 instrument，否则两类调用会互相销账"
    );
    assert!(
        !source.contains("reconnect_budget.note_failure()?")
            && !source.contains("reconnect_budget.note_success()"),
        "不带通道键的预算调用回来了：健康 symbol 会替死掉的 symbol 清零"
    );
    // 专职恢复之外，运维还要看得见"离放弃还有几次"：两个失败分支都得把本通道的
    // 连续次数写进 Degraded 明细，否则这个计数只有用例读得到。
    assert_eq!(
        source
            .matches("reconnect_budget.consecutive_failures(&channel)")
            .count(),
        2,
        "按通道记账的连续次数没上报，放弃前最后几次在健康快照里是隐形的"
    );
    assert!(
        !source.contains("thread::sleep(Duration::from_millis(500))"),
        "固定 500ms 重生死循环回来了"
    );
}
