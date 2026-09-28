use super::*;

/// V13 #166 读侧判据：只有显式的 `idle: true` 才算空闲。真事件、缺键、`false` 与字符串
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

/// V13 #166 跨语言判据：Rust 发的 `wait_ms` 与 Python 答的 `idle` 必须两侧都还在写。
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
    )
}

/// 三态读住进循环才算数：空闲臂必须既不吃重连预算、也不冒充交付。
/// 只看函数存在的话，把 `ccxt_watch_reply_is_idle` 挪进 `if let Err(..)` 里也照样绿。
#[test]
fn ccxt_user_stream_reads_shutdown_idle_and_failure_as_three_states() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let source = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("venue_runtime")
            .join("ccxt_execution.rs"),
    )
    .unwrap();
    // 停机态：循环头部读令牌。
    assert!(
        source.contains("if context.should_stop() {"),
        "用户流循环读不到停机令牌，只能靠进程被强杀"
    );
    // 空闲态：在交付解析之前分流，且带自己的 Degraded 文案。
    let idle_at = source
        .find("ccxt_watch_reply_is_idle(event)")
        .expect("用户流不再分流空闲回话");
    let stream_check_at = source
        .find("非 orders 事件")
        .expect("orders 事件校验不在源里");
    assert!(
        idle_at > stream_check_at,
        "空闲分流跑到了事件归属校验之前，别的 stream 会被当成空闲吞掉"
    );
    assert!(
        source.contains("user stream idle consecutive_windows="),
        "空闲窗没有自己的状态文案，运维只能看到重连计数"
    );
    // 故障态：连续失败仍然走预算，空闲不能替坏链路复位它。
    assert!(
        source.contains("reconnect_streak >= CCXT_DEAD_CYCLE_BUDGET"),
        "用户流的连续失败预算被摘掉了"
    );
    let failure_at = source
        .find("reconnect_streak = reconnect_streak.saturating_add(1)")
        .expect("用户流不再累计连续失败");
    assert!(
        failure_at < idle_at,
        "故障累计跑到了空闲分流之后，空闲窗会先把 streak 清零"
    );
}
