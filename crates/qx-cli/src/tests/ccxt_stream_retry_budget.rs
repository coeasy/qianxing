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

/// #167 接线判据：`run_ccxt_market_worker` 的每一处重生子进程都得先过预算，
/// 成功应答要能把计数复位。光测预算本身不会发现 worker 把 `note_failure()` 摘掉、
/// 退回"每轮清零 + 固定 500ms 重生"的旧形状，所以这里按调用点逐个数。
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
        source.contains("CcxtReconnectBudget::market_rpc()"),
        "行情 worker 不再持有重连预算"
    );
    // 1 处初次 spawn + ticker/OHLCV 两个失败分支各 1 处预算内重生。
    assert_eq!(
        source.matches("CcxtProcessClient::spawn(").count(),
        3,
        "spawn 点数目变了：新增的重生点必须带预算，否则会退回无限重启子进程"
    );
    assert_eq!(
        source.matches("reconnect_budget.note_failure()?").count(),
        2,
        "两个失败分支不是都过预算"
    );
    assert_eq!(
        source.matches("reconnect_budget.note_success()").count(),
        2,
        "成功应答不再复位连续失败计数，偶发失败会累计成误判"
    );
    assert!(
        !source.contains("thread::sleep(Duration::from_millis(500))"),
        "固定 500ms 重生死循环回来了"
    );
}
